//! Form states, addressed on demand rather than enumerated.
//!
//! An XFA form's radio groups, checkboxes and dropdowns define a combinatorial
//! space: forty controls is a trillion states, and enumerating it to reach one
//! of them is absurd. But *materializing* a single state is cheap — apply the
//! selections, re-run the form's calculate scripts, re-flatten.
//!
//! So this module offers two things:
//!
//! 1. [`controls`] — the *dimensions* of the space. No product, always cheap.
//! 2. [`materialize`] — any single point in it, addressed by explicit
//!    selections. Never enumerates.
//!
//! A live interaction session (see `u2s-render-xfa`'s `session` module) is
//! built on the same [`XfaForm::interact`] primitive `materialize` uses, one
//! control at a time, so an agent never has to enumerate anything either.

use std::collections::HashMap;

use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::exhaustive::{
    InteractiveField, InteractiveFieldKind, field_affects_layout,
    get_all_interactive_fields_ordered,
};
use crate::flattened::FieldAccess;
use crate::fidelity::Fidelity;
use crate::flattened::{Flattened, FlattenedNodeKind};
use crate::xfa::XfaNode;
use crate::xfa::scripting::{SomPath, XfaForm};
use crate::{Error, XfaError};

/// One interactive control, and what it can be set to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Control {
    /// The field's SOM path — the stable handle used in selections.
    pub field: String,
    pub kind: ControlKind,
    /// The exclusive group this control belongs to, when it has one. Radio
    /// buttons sharing a group are alternatives: selecting one deselects the
    /// others, so they are a single dimension of the state space rather than
    /// several independent ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The values this control accepts.
    pub options: Vec<ControlOption>,
    /// What it is set to in the default state.
    pub default: Option<String>,
    /// What it is currently set to. Equal to `default` unless a `state`
    /// (or, for an interactive session, a prior interaction) has changed it.
    pub value: Option<String>,
    /// False when the form does not currently show this control. A hidden
    /// control can become visible after another one is set, which is why it
    /// is listed at all rather than omitted.
    pub visible: bool,
    /// Whether the form's own scripts read this control. Kept as data rather
    /// than as a filter, so an agent triaging a large form still knows where
    /// setting a value is likely to change something else.
    pub affects_layout: bool,
    /// Where this control is drawn, in reading order. Empty when the form is
    /// not currently showing it — not a null, since "nowhere right now" is a
    /// real, common state, not the absence of an answer. One entry in
    /// practice: every field, each instance of a repeated section included,
    /// has its own path; several only when the layout draws one path twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<ControlPosition>,
    /// What the person filling the form may do with it, as one of the XFA
    /// keywords (XFA 3.3 §17): `open` can be set or pressed; `readOnly`,
    /// `protected` and `nonInteractive` cannot, though the form's own
    /// scripts can still change it -- and can change `access` itself, so a
    /// locked control can open up after another one is set. This is the
    /// effective level: the control's own `access` tightened by every
    /// enclosing subform's and exclusion group's (XFA 3.3 §2).
    pub access: FieldAccess,
    /// The enclosing subform or exclusion group `access` comes from, when it
    /// is stricter than the control's own. Absent when the control's own
    /// `access` (from the template or a script) is what counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_from: Option<String>,
    /// Buttons only: what pressing it does, read from its click script.
    /// Absent on every other control, and on a button with no click script
    /// (pressing it does nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<ClickEffect>,
}

/// What pressing a button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickEffect {
    /// Its script calls an instance manager (XFA 3.3 §9): pressing it adds,
    /// removes or moves instances of a repeatable section.
    Instances,
    /// Its script does something else.
    Script,
}

/// What a button's click script sources make it do: `None` when they are
/// all comments (there is nothing to run), `Instances` when any of them
/// calls an instance manager (XFA 3.3 §9), `Script` otherwise.
///
/// Read from the script text, not by running it. Pass the click scripts
/// together with the body of every script-object function they call (see
/// [`called_script_object_functions`]): the corpus's add and remove buttons
/// reach the instance manager through a shared helper such as
/// `soPlusMinus.insertNode(this.parent.parent)`.
pub fn click_effect_of_sources<'a>(
    sources: impl IntoIterator<Item = &'a str>,
) -> Option<ClickEffect> {
    const INSTANCE_CALLS: [&str; 6] = [
        "instanceManager",
        "addInstance",
        "insertInstance",
        "removeInstance",
        "moveInstance",
        "setInstances",
    ];
    let live: Vec<&str> = sources
        .into_iter()
        .filter(|s| !crate::xfa::scripting::registry::is_comment_only(s))
        .collect();
    if live.is_empty() {
        return None;
    }
    let manages_instances = live
        .iter()
        .any(|src| INSTANCE_CALLS.iter().any(|call| src.contains(call)));
    Some(if manages_instances {
        ClickEffect::Instances
    } else {
        ClickEffect::Script
    })
}

/// The body of every function of a named script object (XFA 3.3 §10) that
/// `source` calls as `object.function(`. `script_objects` is each script
/// object's name and source.
pub fn called_script_object_functions<'a>(
    source: &str,
    script_objects: &'a [(String, String)],
) -> Vec<&'a str> {
    let mut bodies = Vec::new();
    for (object, content) in script_objects {
        let prefix = format!("{object}.");
        let mut rest = source;
        while let Some(at) = rest.find(&prefix) {
            let after = &rest[at + prefix.len()..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if !name.is_empty()
                && after[name.len()..].trim_start().starts_with('(')
                && let Some(body) = function_body(content, &name)
            {
                bodies.push(body);
            }
            rest = &rest[at + prefix.len()..];
        }
    }
    bodies
}

/// The text of `function name(...) { ... }` in `source`, braces matched.
fn function_body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let header = format!("function {name}(");
    let start = source.find(&header)?;
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, c) in source[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[start..=open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Where a control is drawn: page-local points, top-left origin — the exact
/// convention `xfa_render_region`'s `rect_pt` input already uses (see
/// `u2s_render_xfa::types::rect_of` for the one-line conversion).
///
/// Resolved by matching this control's full SOM path against the laid-out
/// field nodes; when that fails, by its leaf name, but only when exactly one
/// field in the document carries that name. An ambiguous match reports no
/// position at all rather than a plausible-looking wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ControlPosition {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One value a control accepts, in the two vocabularies a form has for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlOption {
    /// What the form stores and what its scripts compare against. This is
    /// what a selection's `value` must be.
    pub value: String,
    /// What the form shows for it, only present when that differs from
    /// `value` (a display value distinct from the save value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ControlOption {
    fn same(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Radio,
    Checkbox,
    Dropdown,
    /// Pressed, not set: no options, no value. See [`Control::click`].
    Button,
    /// Free-value fields, set to any text the widget accepts: no `options`.
    Text,
    TextArea,
    Date,
    Time,
    DateTime,
    Numeric,
    Password,
    Signature,
    Barcode,
    Image,
}

impl ControlKind {
    /// Every kind, in declaration order. Kept beside the enum, and checked
    /// against it by `every_kind_is_listed_once`, so a schema or an error
    /// message listing the kinds is generated rather than written by hand.
    pub const ALL: [ControlKind; 14] = [
        ControlKind::Radio,
        ControlKind::Checkbox,
        ControlKind::Dropdown,
        ControlKind::Button,
        ControlKind::Text,
        ControlKind::TextArea,
        ControlKind::Date,
        ControlKind::Time,
        ControlKind::DateTime,
        ControlKind::Numeric,
        ControlKind::Password,
        ControlKind::Signature,
        ControlKind::Barcode,
        ControlKind::Image,
    ];

    /// Whether a control of this kind is set to one of a fixed list of
    /// `options` -- a radio button, checkbox or dropdown -- and so is a
    /// dimension of the state space. A button is pressed instead, and a
    /// free-value field takes any text.
    pub fn is_choice(self) -> bool {
        matches!(
            self,
            ControlKind::Radio | ControlKind::Checkbox | ControlKind::Dropdown
        )
    }

    /// The name a kind has on the wire (`text_area`, ...), as serde writes
    /// it; `every_kind_is_listed_once` holds the two together.
    pub fn wire_name(self) -> &'static str {
        match self {
            ControlKind::Radio => "radio",
            ControlKind::Checkbox => "checkbox",
            ControlKind::Dropdown => "dropdown",
            ControlKind::Button => "button",
            ControlKind::Text => "text",
            ControlKind::TextArea => "text_area",
            ControlKind::Date => "date",
            ControlKind::Time => "time",
            ControlKind::DateTime => "date_time",
            ControlKind::Numeric => "numeric",
            ControlKind::Password => "password",
            ControlKind::Signature => "signature",
            ControlKind::Barcode => "barcode",
            ControlKind::Image => "image",
        }
    }
}

impl From<&InteractiveFieldKind> for ControlKind {
    fn from(k: &InteractiveFieldKind) -> Self {
        match k {
            InteractiveFieldKind::Radio => ControlKind::Radio,
            InteractiveFieldKind::Checkbox => ControlKind::Checkbox,
            InteractiveFieldKind::Dropdown => ControlKind::Dropdown,
            InteractiveFieldKind::Button => ControlKind::Button,
            InteractiveFieldKind::Text => ControlKind::Text,
            InteractiveFieldKind::TextArea => ControlKind::TextArea,
            InteractiveFieldKind::Date => ControlKind::Date,
            InteractiveFieldKind::Time => ControlKind::Time,
            InteractiveFieldKind::DateTime => ControlKind::DateTime,
            InteractiveFieldKind::Numeric => ControlKind::Numeric,
            InteractiveFieldKind::Password => ControlKind::Password,
            InteractiveFieldKind::Signature => ControlKind::Signature,
            InteractiveFieldKind::Barcode => ControlKind::Barcode,
            InteractiveFieldKind::Image => ControlKind::Image,
        }
    }
}

/// One control set to one value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SelectionSpec {
    pub field: String,
    pub value: String,
}

/// One thing done to a form: a control set to a value, or a button pressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Set(SelectionSpec),
    Click { field: String },
}

/// A requested state: what to do to the form, in order, starting from its
/// default. Everything not touched stays at default.
///
/// Ordered because presses are: two presses of an add button are two new
/// instances, and a value can only be set in an instance a press created.
/// Consecutive `Set` steps, which do not depend on each other's order, are
/// still put in canonical order (see [`StateSpec::canonical`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSpec {
    pub steps: Vec<Step>,
}

impl StateSpec {
    /// A state of selections only.
    pub fn selections(selections: Vec<SelectionSpec>) -> Self {
        StateSpec {
            steps: selections.into_iter().map(Step::Set).collect(),
        }
    }

    /// The canonical form: each run of consecutive `Set` steps sorted by
    /// field and deduplicated, `Click` steps left where they are. So `[a,b]`
    /// and `[b,a]` are the same state, and a pure selection state keeps the
    /// identity it always had, while presses keep their order.
    pub fn canonical(&self) -> Vec<Step> {
        let mut out: Vec<Step> = Vec::with_capacity(self.steps.len());
        let mut run: Vec<SelectionSpec> = Vec::new();
        let flush = |run: &mut Vec<SelectionSpec>, out: &mut Vec<Step>| {
            run.sort();
            run.dedup();
            out.extend(run.drain(..).map(Step::Set));
        };
        for step in &self.steps {
            match step {
                Step::Set(sel) => run.push(sel.clone()),
                Step::Click { .. } => {
                    flush(&mut run, &mut out);
                    out.push(step.clone());
                }
            }
        }
        flush(&mut run, &mut out);
        out
    }

    /// A stable identifier, usable as a label or a cache key.
    pub fn key(&self) -> String {
        let c = self.canonical();
        if c.is_empty() {
            return "default".to_string();
        }
        c.iter()
            .map(|step| match step {
                Step::Set(s) => format!("{}={}", s.field, s.value),
                Step::Click { field } => format!("click:{field}"),
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn is_default(&self) -> bool {
        self.steps.is_empty()
    }
}

/// The controls of a form, and how large its state space is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Controls {
    pub controls: Vec<Control>,
    /// The product of every control's option count — how many states exist.
    /// Saturating, because a form with enough controls overflows any integer,
    /// which is itself the point: this is a number to route around, not to
    /// enumerate.
    pub space_size: u64,
    /// True when the product overflowed and `space_size` is `u64::MAX` rather
    /// than a real count. A saturated value must not be read as one.
    pub saturated: bool,
}

/// One window of a form's controls: what a listing returns, since a form can
/// have hundreds of fields and a caller reads them a window at a time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlsWindow {
    /// The controls from `offset`, at most `limit` of them, in SOM path order.
    pub controls: Vec<Control>,
    /// How many controls match in all, across every window.
    pub total: usize,
    pub offset: usize,
    /// Where the next window starts; `None` once this one reaches the end.
    pub next_offset: Option<usize>,
    /// As [`Controls::space_size`], over the whole form whatever the window.
    pub space_size: u64,
    pub saturated: bool,
}

impl Controls {
    /// The controls of the given `kinds` (every kind when `None`) from
    /// `offset`, at most `limit` of them. `limit` must be at least one: a
    /// window that can never advance is a caller's bug.
    pub fn window(
        self,
        kinds: Option<&[ControlKind]>,
        offset: usize,
        limit: usize,
    ) -> ControlsWindow {
        assert!(limit >= 1, "a controls window needs a limit of at least one");
        let matching: Vec<Control> = self
            .controls
            .into_iter()
            .filter(|c| kinds.is_none_or(|k| k.contains(&c.kind)))
            .collect();
        let total = matching.len();
        let controls: Vec<Control> = matching.into_iter().skip(offset).take(limit).collect();
        let end = offset.saturating_add(controls.len());
        ControlsWindow {
            controls,
            total,
            offset,
            next_offset: (end < total).then_some(end),
            space_size: self.space_size,
            saturated: self.saturated,
        }
    }
}

/// A radio button's SOM path is `…Group.Member`, so its group is the path with
/// the final segment removed. Non-radio controls stand alone.
fn group_of(path: &str, kind: &InteractiveFieldKind) -> Option<String> {
    if !matches!(kind, InteractiveFieldKind::Radio) {
        return None;
    }
    path.rfind('.').map(|i| path[..i].to_string())
}

fn build_form(nodes: &[XfaNode]) -> Result<XfaForm, XfaError> {
    XfaForm::new(nodes.to_vec()).map_err(Error::FormCreation)
}

fn to_f32(d: crate::xfa::Num) -> f32 {
    d.to_f32().unwrap_or(0.0)
}

/// Every laid-out field, indexed two ways: by its full SOM path (the exact
/// match `positions_for` prefers) and by leaf name (the fallback, used only
/// when a leaf name is unambiguous — see `positions_for`).
///
/// `Vec<ControlPosition>` rather than one, because the layout can draw one
/// path more than once (a field split across a page break); `Vec<String>`
/// because a leaf name is shared by every instance of a repeated section.
/// Every laid-out field, as its SOM path and where it is drawn (`None`
/// when it has no page position).
pub fn drawn_fields(
    flat: &Flattened,
) -> impl Iterator<Item = (SomPath, Option<ControlPosition>)> + '_ {
    flat.iter_nodes().filter_map(move |node| {
        if !matches!(node.kind, FlattenedNodeKind::Field { .. }) {
            return None;
        }
        let som = node.som_path()?.clone();
        let position = flat.locate(node).map(|(page, bounds)| ControlPosition {
            page,
            x: to_f32(bounds.x),
            y: to_f32(bounds.y),
            width: to_f32(bounds.width),
            height: to_f32(bounds.height),
        });
        Some((som, position))
    })
}

/// The SOM path of every field the layout draws, in sorted order: what a
/// field "appearing" or "disappearing" is measured against -- each new
/// instance of a repeated section brings its own.
pub fn drawn_field_set(flat: &Flattened) -> std::collections::BTreeSet<String> {
    drawn_fields(flat)
        .map(|(som, _)| som.as_str().to_string())
        .collect()
}

fn position_index(
    flat: &Flattened,
) -> (
    HashMap<String, Vec<ControlPosition>>,
    HashMap<String, Vec<String>>,
) {
    let mut by_path: HashMap<String, Vec<ControlPosition>> = HashMap::new();
    let mut by_leaf: HashMap<String, Vec<String>> = HashMap::new();

    for (som, position) in drawn_fields(flat) {
        let full = som.as_str().to_string();
        let leaf = by_leaf.entry(som.name().to_string()).or_default();
        if !leaf.contains(&full) {
            leaf.push(full.clone());
        }
        if let Some(position) = position {
            by_path.entry(full).or_default().push(position);
        }
    }

    for positions in by_path.values_mut() {
        positions.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then_with(|| a.y.total_cmp(&b.y))
                .then_with(|| a.x.total_cmp(&b.x))
        });
    }
    (by_path, by_leaf)
}

/// Resolve one control's positions: exact SOM path match first; otherwise the
/// leaf name, but only when exactly one field in the whole document carries
/// it — a wrong rectangle is worse than a missing one, so an ambiguous leaf
/// yields nothing rather than a guess.
fn positions_for(
    by_path: &HashMap<String, Vec<ControlPosition>>,
    by_leaf: &HashMap<String, Vec<String>>,
    field: &str,
) -> Vec<ControlPosition> {
    if let Some(hit) = by_path.get(field) {
        return hit.clone();
    }
    let leaf = field.rsplit('.').next().unwrap_or(field);
    match by_leaf.get(leaf).map(Vec::as_slice) {
        Some([only]) => by_path.get(only).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// List a form's interactive controls without exploring anything.
pub fn controls(nodes: &[XfaNode]) -> Result<Controls, XfaError> {
    controls_of_form(&mut build_form(nodes)?)
}

/// As [`controls`], for a form that already exists -- a live session's, in
/// particular, which has been mutated since it was built and so must be
/// asked directly rather than rebuilt from its original nodes.
/// What describing a control reads from the form as a whole, gathered once
/// per listing rather than once per control.
struct ControlContext {
    defaults: HashMap<SomPath, String>,
    by_path: HashMap<String, Vec<ControlPosition>>,
    by_leaf: HashMap<String, Vec<String>>,
    script_objects: Vec<(String, String)>,
}

impl ControlContext {
    fn of(form: &mut XfaForm) -> Self {
        let defaults = form.current_field_values();
        let (by_path, by_leaf) = position_index(form.flattened());
        let script_objects = crate::xfa::collect_variable_scripts(form.xfa_nodes());
        ControlContext {
            defaults,
            by_path,
            by_leaf,
            script_objects,
        }
    }
}

/// One field, described as a [`Control`]: the single place a control's
/// options, value, visibility, access and positions are read, shared by the
/// whole listing and the one-field lookup.
fn control_of(
    form: &XfaForm,
    f: &InteractiveField,
    context: &ControlContext,
) -> Result<Control, XfaError> {
    let resolved = form.resolve(f.path.as_str());
    let (on, off) = resolved
        .as_ref()
        .map(|n| n.xfa_node().extract_item_values())
        .unwrap_or((None, None));

    let options: Vec<ControlOption> = match f.kind {
        InteractiveFieldKind::Checkbox => vec![
            ControlOption::same(on.clone().unwrap_or_else(|| "1".into())),
            ControlOption::same(off.clone().unwrap_or_else(|| "0".into())),
        ],
        InteractiveFieldKind::Radio => {
            vec![ControlOption::same(
                on.clone().unwrap_or_else(|| f.path.name().to_string()),
            )]
        }
        // A dropdown's options can come from merged data or scripts, so
        // they are read from the live form rather than the template.
        // `dropdown_options` pairs every <items> entry rather than
        // reading only the first two, and keeps the display value
        // separate from the save value a selection must actually use.
        InteractiveFieldKind::Dropdown => resolved
            .as_ref()
            .map(|n| {
                n.dropdown_options()
                    .into_iter()
                    .map(|(display, save)| ControlOption {
                        label: (display != save).then_some(display),
                        value: save,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        // Pressed, not set.
        InteractiveFieldKind::Button => Vec::new(),
        // Free-value fields: any text the widget accepts, no fixed list.
        InteractiveFieldKind::Text
        | InteractiveFieldKind::TextArea
        | InteractiveFieldKind::Date
        | InteractiveFieldKind::Time
        | InteractiveFieldKind::DateTime
        | InteractiveFieldKind::Numeric
        | InteractiveFieldKind::Password
        | InteractiveFieldKind::Signature
        | InteractiveFieldKind::Barcode
        | InteractiveFieldKind::Image => Vec::new(),
    };

    let is_button = matches!(f.kind, InteractiveFieldKind::Button);
    let value = if is_button {
        None
    } else {
        context.defaults.get(&f.path).cloned()
    };
    let click = if is_button {
        let sources: Vec<String> = form
            .script_registry()
            .get_event_scripts(
                &f.path,
                &crate::xfa::scripting::events::EventActivity::Click,
            )
            .iter()
            .map(|s| s.script.source.clone())
            .collect();
        let called: Vec<&str> = sources
            .iter()
            .flat_map(|src| called_script_object_functions(src, &context.script_objects))
            .collect();
        click_effect_of_sources(sources.iter().map(String::as_str).chain(called))
    } else {
        None
    };
    let visible = resolved.as_ref().map(|n| n.is_visible()).unwrap_or(false);
    let affects_layout = field_affects_layout(form, f);
    let positions = positions_for(&context.by_path, &context.by_leaf, f.path.as_str());
    // Every listed field was just found in this form's tree, so failing to
    // walk back to it is an engine bug; it is reported, not guessed around.
    let access = form.effective_access(f.path.as_str()).ok_or_else(|| {
        XfaError::Layout(format!(
            "field {} was listed but its access could not be read; this is an engine bug",
            f.path
        ))
    })?;

    Ok(Control {
        field: f.path.as_str().to_string(),
        kind: ControlKind::from(&f.kind),
        group: group_of(f.path.as_str(), &f.kind),
        options,
        default: value.clone(),
        value,
        visible,
        affects_layout,
        positions,
        access: access.access,
        access_from: access.inherited_from.map(|p| p.as_str().to_string()),
        click,
    })
}

/// Every field's effective access (see [`XfaForm::effective_access`]), by
/// SOM path: what an interaction's before/after comparison reads to tell
/// which fields a script locked or unlocked.
pub fn access_by_field(form: &XfaForm) -> std::collections::BTreeMap<String, FieldAccess> {
    get_all_interactive_fields_ordered(form)
        .into_iter()
        .filter_map(|f| {
            let access = form.effective_access(f.path.as_str())?;
            Some((f.path.as_str().to_string(), access.access))
        })
        .collect()
}

/// One field of a form as it stands, described exactly as [`controls_of_form`]
/// lists it, or `Ok(None)` when the form has no field at `field`. `field` is a
/// SOM path as the listing gives it.
pub fn field_of_form(form: &mut XfaForm, field: &str) -> Result<Option<Control>, XfaError> {
    let Some(found) = get_all_interactive_fields_ordered(form)
        .into_iter()
        .find(|f| f.path.as_str() == field)
    else {
        return Ok(None);
    };
    let context = ControlContext::of(form);
    control_of(form, &found, &context).map(Some)
}

pub fn controls_of_form(form: &mut XfaForm) -> Result<Controls, XfaError> {
    let fields = get_all_interactive_fields_ordered(form);
    let context = ControlContext::of(form);

    let mut out = Vec::with_capacity(fields.len());
    let mut space: u64 = 1;
    let mut saturated = false;

    for f in &fields {
        out.push(control_of(form, f, &context)?);
    }

    // Radio buttons in one exclGroup are alternatives, not independent
    // dimensions: `RB_1` and `RB_2` together contribute a factor of two, not
    // one each. Multiplying every listed control would understate the space on
    // a form with radios and overstate the independence of its controls.
    let mut counted: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    // Only choices are dimensions of the state space: a button is pressed,
    // not set to one of several values, and a free-value field takes any
    // text, so neither can be counted.
    for c in out.iter().filter(|c| c.kind.is_choice()) {
        let dimension = c.group.clone().unwrap_or_else(|| c.field.clone());
        let options = c.options.len().max(1) as u64;
        counted
            .entry(dimension)
            .and_modify(|n| *n += options)
            .or_insert(options);
    }
    for n in counted.values() {
        let next = space.saturating_mul(*n);
        if next == u64::MAX && space != 0 && *n != 0 {
            saturated = true;
        }
        space = next;
    }

    Ok(Controls {
        controls: out,
        space_size: space,
        saturated,
    })
}

/// A state, materialized.
pub struct MaterializedState {
    pub flattened: Flattened,
    /// The canonical key of the state that was produced.
    pub key: String,
    /// How faithfully this state was reached, mirroring [`prepare_default`]'s
    /// ladder rather than asserting the top of it unconditionally.
    pub fidelity: Fidelity,
    /// Set when the render is incomplete, or when a press did nothing
    /// because a repeatable section was already at its limit.
    pub warning: Option<String>,
}

/// Run a state's steps on `form`, in canonical order, refreshing after each
/// so the next one sees the form as this one left it. Returns why a press
/// did nothing, when one did.
fn apply_steps(form: &mut XfaForm, spec: &StateSpec) -> Result<Option<String>, XfaError> {
    let mut limit_hit: Option<String> = None;
    for step in spec.canonical() {
        let field = match &step {
            Step::Set(sel) => sel.field.as_str(),
            Step::Click { field } => field.as_str(),
        };
        // Looked up in the form as the previous step left it: a press can
        // add or remove fields.
        let Some(is_button) = form.resolve(field).map(|n| n.is_button()) else {
            // Naming the field is not enough — a caller that guessed wrong
            // needs to know where the real names come from.
            return Err(XfaError::Layout(format!(
                "no field '{field}' on this form — call controls() for the available ones"
            )));
        };
        let result = match (&step, is_button) {
            (Step::Set(sel), false) => {
                // `interact`, not `set_value_as_user`: a radio must deselect its
                // siblings and set the exclGroup's value to render as selected,
                // and firing enter/exit is what makes this an interaction rather
                // than a raw value replay.
                form.interact(&sel.field, &sel.value)
                    .map_err(XfaError::Layout)?
            }
            (Step::Set(_), true) => {
                return Err(XfaError::Layout(format!(
                    "'{field}' is a button; it has no value to set — press it with a click step"
                )));
            }
            (Step::Click { .. }, true) => form.click(field).map_err(XfaError::Layout)?,
            (Step::Click { .. }, false) => {
                return Err(XfaError::Layout(format!(
                    "'{field}' is not a button; set it to a value with a set step instead"
                )));
            }
        };
        if limit_hit.is_none() {
            limit_hit = result.instance_limit_hit;
        }
        // Each step must see the form as the previous one left it.
        form.refresh().map_err(Error::FormCreation)?;
    }
    Ok(limit_hit)
}

/// The live form in `spec`'s state, laid out.
fn form_in_state(nodes: &[XfaNode], spec: &StateSpec) -> Result<XfaForm, XfaError> {
    let (mut form, mut layout) =
        XfaForm::new_with_layout(nodes.to_vec()).map_err(Error::FormCreation)?;
    apply_steps(&mut form, spec)?;
    match layout.as_mut() {
        Some(l) => form.refresh_paged(l).map_err(Error::FormCreation)?,
        None => form.refresh().map_err(Error::FormCreation)?,
    }
    Ok(form)
}

/// As [`controls`], for the form in `spec`'s state: after its presses, a
/// repeated section's new instances are listed with their own controls.
pub fn controls_in_state(nodes: &[XfaNode], spec: &StateSpec) -> Result<Controls, XfaError> {
    controls_of_form(&mut form_in_state(nodes, spec)?)
}

/// As [`field_of_form`], for the form in `spec`'s state.
pub fn field_in_state(
    nodes: &[XfaNode],
    spec: &StateSpec,
    field: &str,
) -> Result<Option<Control>, XfaError> {
    field_of_form(&mut form_in_state(nodes, spec)?, field)
}

/// Materialize one state directly, without enumerating anything.
///
/// This is the operation that makes a large state space usable: any point in it
/// is one form refresh away, so a caller reads [`controls`], picks what it
/// wants, and asks for exactly that. Steps run in their canonical order (see
/// [`StateSpec::canonical`]); a field a press created can be set by a later
/// step under its instance path (`Row[1].Amount`).
pub fn materialize(nodes: &[XfaNode], spec: &StateSpec) -> Result<MaterializedState, XfaError> {
    let (mut form, mut layout) =
        XfaForm::new_with_layout(nodes.to_vec()).map_err(Error::FormCreation)?;
    let limit_hit = apply_steps(&mut form, spec)?;

    // Re-flatten through the master-page engine from this same document-wide
    // pass, rather than asserting `Fidelity::Scripts` unconditionally: a form
    // whose scripts could not run at all still degrades to level 2 instead of
    // failing outright, matching `prepare_default`'s own ladder.
    let (fidelity, warning) = match layout.as_mut() {
        Some(l) => {
            form.refresh_paged(l).map_err(Error::FormCreation)?;
            (Fidelity::Scripts, None)
        }
        None => {
            form.refresh().map_err(Error::FormCreation)?;
            (
                Fidelity::FormDom,
                Some(
                    "this form's scripts could not be executed; script-computed values are \
                     missing and script-hidden sections may still be visible"
                        .to_string(),
                ),
            )
        }
    };
    let warning = match (warning, limit_hit) {
        (Some(w), Some(l)) => Some(format!("{w}; {l}")),
        (w, l) => w.or(l),
    };

    Ok(MaterializedState {
        flattened: form.flattened().clone(),
        key: spec.key(),
        fidelity,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(field: &str, kind: ControlKind) -> Control {
        Control {
            field: field.to_string(),
            kind,
            group: None,
            options: Vec::new(),
            default: None,
            value: None,
            visible: true,
            affects_layout: false,
            positions: Vec::new(),
            access: FieldAccess::Open,
            access_from: None,
            click: None,
        }
    }

    fn listing() -> Controls {
        Controls {
            controls: vec![
                control("f.A", ControlKind::Text),
                control("f.B", ControlKind::Radio),
                control("f.C", ControlKind::Text),
                control("f.D", ControlKind::Checkbox),
                control("f.E", ControlKind::Text),
            ],
            space_size: 4,
            saturated: false,
        }
    }

    fn fields(w: &ControlsWindow) -> Vec<&str> {
        w.controls.iter().map(|c| c.field.as_str()).collect()
    }

    #[test]
    fn every_kind_is_listed_once() {
        // The match fails to compile when a variant is added, pointing here
        // to add it to `ALL` as well.
        fn position(k: ControlKind) -> usize {
            match k {
                ControlKind::Radio => 0,
                ControlKind::Checkbox => 1,
                ControlKind::Dropdown => 2,
                ControlKind::Button => 3,
                ControlKind::Text => 4,
                ControlKind::TextArea => 5,
                ControlKind::Date => 6,
                ControlKind::Time => 7,
                ControlKind::DateTime => 8,
                ControlKind::Numeric => 9,
                ControlKind::Password => 10,
                ControlKind::Signature => 11,
                ControlKind::Barcode => 12,
                ControlKind::Image => 13,
            }
        }
        for (i, k) in ControlKind::ALL.iter().enumerate() {
            assert_eq!(position(*k), i, "{k:?}");
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::Value::from(k.wire_name()),
                "{k:?}"
            );
        }
    }

    #[test]
    fn a_window_walks_the_listing_and_stops_at_its_end() {
        let first = listing().window(None, 0, 2);
        assert_eq!(fields(&first), ["f.A", "f.B"]);
        assert_eq!((first.total, first.next_offset), (5, Some(2)));
        assert_eq!(first.space_size, 4, "the space is the whole form's");

        let last = listing().window(None, 4, 2);
        assert_eq!(fields(&last), ["f.E"]);
        assert_eq!(last.next_offset, None);

        let past = listing().window(None, 9, 2);
        assert!(past.controls.is_empty());
        assert_eq!((past.total, past.next_offset), (5, None));
    }

    #[test]
    fn a_kinds_filter_applies_before_the_window() {
        let kinds = [ControlKind::Radio, ControlKind::Checkbox];
        let w = listing().window(Some(&kinds), 0, 1);
        assert_eq!(fields(&w), ["f.B"]);
        assert_eq!((w.total, w.next_offset), (2, Some(1)));
        let w = listing().window(Some(&kinds), 1, 1);
        assert_eq!(fields(&w), ["f.D"]);
        assert_eq!(w.next_offset, None);
    }

    #[test]
    #[should_panic(expected = "limit of at least one")]
    fn a_zero_limit_is_a_bug() {
        listing().window(None, 0, 0);
    }

    #[test]
    fn a_click_script_is_classified_by_whether_it_reaches_an_instance_manager() {
        assert_eq!(
            click_effect_of_sources(["_Row.addInstance(1);"]),
            Some(ClickEffect::Instances)
        );
        assert_eq!(
            click_effect_of_sources([
                "this.parent.instanceManager.removeInstance(this.parent.index);"
            ]),
            Some(ClickEffect::Instances)
        );
        assert_eq!(
            click_effect_of_sources(["xfa.host.messageBox('hi');"]),
            Some(ClickEffect::Script)
        );
        assert_eq!(
            click_effect_of_sources(["// addInstance, commented out"]),
            None
        );
        assert_eq!(click_effect_of_sources(std::iter::empty::<&str>()), None);
    }

    #[test]
    fn a_click_through_a_script_object_reads_the_called_function_body() {
        let objects = vec![(
            "soPlusMinus".to_string(),
            "function insertNode(first) {\n  if (first) { first.instanceManager.addInstance(); }\n}\nfunction applyIndex(list) { list.item(0).rawValue = 1; }".to_string(),
        )];
        let click = "soPlusMinus.insertNode(this.parent.parent);";
        let called = called_script_object_functions(click, &objects);
        assert_eq!(called.len(), 1);
        assert!(called[0].starts_with("function insertNode(") && called[0].ends_with('}'));
        assert_eq!(
            click_effect_of_sources(std::iter::once(click).chain(called)),
            Some(ClickEffect::Instances)
        );

        let numbering_only = "soPlusMinus.applyIndex(x);";
        let called = called_script_object_functions(numbering_only, &objects);
        assert_eq!(
            click_effect_of_sources(std::iter::once(numbering_only).chain(called)),
            Some(ClickEffect::Script)
        );
    }

    fn set(field: &str, value: &str) -> Step {
        Step::Set(SelectionSpec {
            field: field.to_string(),
            value: value.to_string(),
        })
    }

    fn click(field: &str) -> Step {
        Step::Click {
            field: field.to_string(),
        }
    }

    #[test]
    fn canonical_steps_sort_runs_of_sets_and_keep_presses_in_order() {
        let a = StateSpec {
            steps: vec![
                set("b", "1"),
                set("a", "2"),
                click("Add"),
                set("z", "3"),
                set("y", "4"),
            ],
        };
        let b = StateSpec {
            steps: vec![
                set("a", "2"),
                set("b", "1"),
                click("Add"),
                set("y", "4"),
                set("z", "3"),
            ],
        };
        assert_eq!(a.key(), b.key());
        assert_eq!(a.key(), "a=2,b=1,click:Add,y=4,z=3");

        // Moving a set across a press is a different state.
        let across = StateSpec {
            steps: vec![
                set("a", "2"),
                set("b", "1"),
                set("y", "4"),
                click("Add"),
                set("z", "3"),
            ],
        };
        assert_ne!(a.key(), across.key());

        // Two presses are two presses.
        let once = StateSpec {
            steps: vec![click("Add")],
        };
        let twice = StateSpec {
            steps: vec![click("Add"), click("Add")],
        };
        assert_ne!(once.key(), twice.key());

        // A selections-only state keeps the key it always had.
        assert_eq!(
            StateSpec::selections(vec![
                SelectionSpec {
                    field: "b".into(),
                    value: "1".into()
                },
                SelectionSpec {
                    field: "a".into(),
                    value: "2".into()
                },
            ])
            .key(),
            "a=2,b=1"
        );
        assert_eq!(StateSpec::default().key(), "default");
    }

    #[test]
    fn steps_serialize_as_set_and_click_objects() {
        let spec = StateSpec {
            steps: vec![set("F", "v"), click("Add")],
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(
            json,
            r#"{"steps":[{"set":{"field":"F","value":"v"}},{"click":{"field":"Add"}}]}"#
        );
    }
}
