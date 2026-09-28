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
    SelectableFieldKind, field_affects_layout, get_all_selectable_fields_ordered,
};
use crate::fidelity::Fidelity;
use crate::flattened::{Flattened, FlattenedNodeKind};
use crate::xfa::XfaNode;
use crate::xfa::scripting::XfaForm;
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
    /// real, common state, not the absence of an answer. More than one entry
    /// when the control lives inside a repeated subform instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<ControlPosition>,
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
}

impl From<&SelectableFieldKind> for ControlKind {
    fn from(k: &SelectableFieldKind) -> Self {
        match k {
            SelectableFieldKind::Radio => ControlKind::Radio,
            SelectableFieldKind::Checkbox => ControlKind::Checkbox,
            SelectableFieldKind::Dropdown => ControlKind::Dropdown,
        }
    }
}

/// One control set to one value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSpec {
    pub field: String,
    pub value: String,
}

/// A requested state: the controls to change, everything else left at default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSpec {
    pub selections: Vec<SelectionSpec>,
}

impl StateSpec {
    /// The canonical form: selections sorted by field, so `[a,b]` and `[b,a]`
    /// are the same state. This is the cache key, which means state identity
    /// never depends on the order a caller happened to write them in.
    pub fn canonical(&self) -> Vec<SelectionSpec> {
        let mut s = self.selections.clone();
        s.sort_by(|a, b| a.field.cmp(&b.field).then(a.value.cmp(&b.value)));
        s.dedup();
        s
    }

    /// A stable identifier, usable as a label or a cache key.
    pub fn key(&self) -> String {
        let c = self.canonical();
        if c.is_empty() {
            return "default".to_string();
        }
        c.iter()
            .map(|s| format!("{}={}", s.field, s.value))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn is_default(&self) -> bool {
        self.selections.is_empty()
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

/// A radio button's SOM path is `…Group.Member`, so its group is the path with
/// the final segment removed. Non-radio controls stand alone.
fn group_of(path: &str, kind: &SelectableFieldKind) -> Option<String> {
    if !matches!(kind, SelectableFieldKind::Radio) {
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
/// `Vec<ControlPosition>` rather than one, and `Vec<String>` rather than one
/// path, because a repeated subform instantiates the same names more than
/// once: this index has to carry every occurrence, not just the first.
fn position_index(
    flat: &Flattened,
) -> (
    HashMap<String, Vec<ControlPosition>>,
    HashMap<String, Vec<String>>,
) {
    let mut by_path: HashMap<String, Vec<ControlPosition>> = HashMap::new();
    let mut by_leaf: HashMap<String, Vec<String>> = HashMap::new();

    for node in flat.iter_nodes() {
        if !matches!(node.kind, FlattenedNodeKind::Field { .. }) {
            continue;
        }
        let Some(som) = node.som_path() else {
            continue;
        };
        let full = som.as_str().to_string();
        let leaf = by_leaf.entry(som.name().to_string()).or_default();
        if !leaf.contains(&full) {
            leaf.push(full.clone());
        }
        if let Some((page, bounds)) = flat.locate(node) {
            by_path.entry(full).or_default().push(ControlPosition {
                page,
                x: to_f32(bounds.x),
                y: to_f32(bounds.y),
                width: to_f32(bounds.width),
                height: to_f32(bounds.height),
            });
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
pub fn controls_of_form(form: &mut XfaForm) -> Result<Controls, XfaError> {
    let fields = get_all_selectable_fields_ordered(form);
    let defaults = form.current_field_values();
    let (by_path, by_leaf) = position_index(form.flattened());

    let mut out = Vec::with_capacity(fields.len());
    let mut space: u64 = 1;
    let mut saturated = false;

    for f in &fields {
        let resolved = form.resolve(f.path.as_str());
        let (on, off) = resolved
            .as_ref()
            .map(|n| n.xfa_node().extract_item_values())
            .unwrap_or((None, None));

        let options: Vec<ControlOption> = match f.kind {
            SelectableFieldKind::Checkbox => vec![
                ControlOption::same(on.clone().unwrap_or_else(|| "1".into())),
                ControlOption::same(off.clone().unwrap_or_else(|| "0".into())),
            ],
            SelectableFieldKind::Radio => {
                vec![ControlOption::same(
                    on.clone().unwrap_or_else(|| f.path.name().to_string()),
                )]
            }
            // A dropdown's options can come from merged data or scripts, so
            // they are read from the live form rather than the template.
            // `dropdown_options` pairs every <items> entry rather than
            // reading only the first two, and keeps the display value
            // separate from the save value a selection must actually use.
            SelectableFieldKind::Dropdown => resolved
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
        };

        let value = defaults.get(&f.path).cloned();
        let visible = resolved.as_ref().map(|n| n.is_visible()).unwrap_or(false);
        let affects_layout = field_affects_layout(form, f);
        let positions = positions_for(&by_path, &by_leaf, f.path.as_str());

        out.push(Control {
            field: f.path.as_str().to_string(),
            kind: ControlKind::from(&f.kind),
            group: group_of(f.path.as_str(), &f.kind),
            options,
            default: value.clone(),
            value,
            visible,
            affects_layout,
            positions,
        });
    }

    // Radio buttons in one exclGroup are alternatives, not independent
    // dimensions: `RB_1` and `RB_2` together contribute a factor of two, not
    // one each. Multiplying every listed control would understate the space on
    // a form with radios and overstate the independence of its controls.
    let mut counted: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for c in &out {
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
    pub warning: Option<String>,
}

/// Materialize one state directly, without enumerating anything.
///
/// This is the operation that makes a large state space usable: any point in it
/// is one form refresh away, so a caller reads [`controls`], picks what it
/// wants, and asks for exactly that.
pub fn materialize(nodes: &[XfaNode], spec: &StateSpec) -> Result<MaterializedState, XfaError> {
    let (mut form, mut layout) =
        XfaForm::new_with_layout(nodes.to_vec()).map_err(Error::FormCreation)?;

    if !spec.is_default() {
        let known: std::collections::HashSet<String> = get_all_selectable_fields_ordered(&form)
            .into_iter()
            .map(|f| f.path.as_str().to_string())
            .collect();

        for sel in spec.canonical() {
            if !known.contains(&sel.field) {
                // Naming the field is not enough — a caller that guessed wrong
                // needs to know where the real names come from.
                return Err(XfaError::Layout(format!(
                    "no control '{}' on this form — call controls() for the {} available",
                    sel.field,
                    known.len()
                )));
            }
            // `interact`, not `set_value_as_user`: a radio must deselect its
            // siblings and set the exclGroup's value to render as selected,
            // and firing enter/exit is what makes this an interaction rather
            // than a raw value replay.
            form.interact(&sel.field, &sel.value)
                .map_err(XfaError::Layout)?;
        }
    }

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

    Ok(MaterializedState {
        flattened: form.flattened().clone(),
        key: spec.key(),
        fidelity,
        warning,
    })
}
