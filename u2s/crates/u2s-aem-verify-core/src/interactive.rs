//! One-step-at-a-time control tools built on `crate::flow`'s primitives:
//! `verify_open` installs a package and opens its form (mirroring
//! `crate::flow::open_live_form`, which `verify_run` also uses), then
//! `verify_controls`/`verify_set`/`verify_next`/`verify_prev`/`verify_reset`/
//! `verify_screenshot`/`verify_submit`/`verify_close` drive it one call at a
//! time -- the same shape `u2s-render-xfa-mcp`'s `xfa_open`/`xfa_set`/
//! `xfa_controls` surface gives an agent over an XFA form, applied here to
//! a live AEM page instead of an in-process rendering engine.
//!
//! **Addressing.** A control is addressed by its `field` -- the guide
//! node's own `name`, exactly what [`crate::flow::wizard_js::fill_field`]
//! already matches on and what [`crate::flow::RunRequest::fill`]'s keys
//! already mean -- never a coordinate or an index. [`Control::position`]
//! exists only as an *output*, feeding `verify_screenshot`'s optional
//! `field` argument so an agent can zoom in on a control it just read
//! about.
//!
//! **At most one form per session.** `crate::session::SessionState` has
//! exactly one `installed_package_path` slot; [`OpenForm`] lives inside
//! that same session state (`crate::session::SessionState::open_form`)
//! rather than in a second store with its own TTL clock, so opening,
//! closing and idle-sweeping a form always happen under the one lock
//! `crate::session::SessionPool` already provides. `verify_run` on a
//! `session_id` with a form open is refused (`crate::flow::run`) rather
//! than risk uninstalling a package an interactive caller still has open.
//!
//! **Mutating calls take ownership of the session's [`OpenForm`] for the
//! duration of the call** (`take_form`, below) rather than holding two
//! overlapping mutable borrows of `crate::session::SessionState` across an
//! `.await` -- every code path, success or error, puts the form back
//! before returning.
//!
//! **Revisions.** Every call after `verify_open` names either `revision`
//! (a read: `verify_controls`, `verify_screenshot`) or `expected_revision`
//! (a mutation), refused if it does not match the form's own current
//! revision -- [`check_revision`]'s wording matches
//! `u2s_mcp::session::SessionStore`'s own error text so an agent sees the
//! same phrasing regardless of which u2s server it is talking to. A
//! mutation always advances the revision by exactly one, even when nothing
//! observable changed, so a caller's optimistic-concurrency check can never
//! be fooled by a no-op repeat.

use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use u2s_blob::BlobStore;
use u2s_verify_core::browser;
use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::types::{Artefact, ErrorKind, FailedRequest, Finding, Step, VerifyError};

use crate::driver::FormDriver;
use crate::flow::{self, LiveForm};
use crate::package_check;
use crate::profile::{Profile, SubmitArtefact};
use crate::session::{Instances, SessionPool, SessionState};

/// One form open for interaction on a session -- lives at
/// `SessionState::open_form`; see this module's own doc for why there is
/// at most one per session rather than a second handle table. `pub(crate)`
/// rather than `pub`: it wraps `crate::flow::LiveForm`, itself
/// `pub(crate)` because nothing outside this crate ever needs to name a
/// live page or browser session directly.
pub(crate) struct OpenForm {
    pub handle: String,
    pub revision: u64,
    pub live: LiveForm,
    #[allow(dead_code)]
    pub opened_at: Instant,
}

/// Every hard failure an interactive call can produce. Wording for the
/// handle/revision cases matches `u2s_mcp::session::SessionStore`'s own
/// error text (see this module's own doc) -- a deliberate, disclosed small
/// duplication of three sentences, not of logic, the same trade
/// `u2s-render-xfa::session`'s own doc already makes for the same reason.
#[derive(Debug, thiserror::Error)]
pub enum InteractionError {
    #[error(
        "form {handle:?} is not open; it may never have existed, may have already been closed, \
         or its session may have gone idle and been swept -- call verify_open again"
    )]
    UnknownForm { handle: String },
    #[error(
        "form {handle:?} is at revision {current}; you asked for {asked}, which is behind it -- \
         re-read at revision {current} before retrying"
    )]
    RevisionStale { handle: String, current: u64, asked: u64 },
    #[error(
        "form {handle:?} is at revision {current}; you asked for {asked}, which was never \
         issued -- the current revision is {current}"
    )]
    RevisionAhead { handle: String, current: u64, asked: u64 },
    #[error(
        "a form ({handle:?}) is already open on this session -- call verify_close first, or \
         pass a different session_id"
    )]
    FormAlreadyOpen { handle: String },
    #[error(
        "no control named {field:?} on this form -- call verify_controls for the fields \
         available: {known:?}"
    )]
    UnknownField { field: String, known: Vec<String> },
    #[error("{field:?} cannot be set to {given:?} -- its own options are {options:?}")]
    ValueNotAnOption {
        field: String,
        given: String,
        options: Vec<String>,
    },
    #[error("{field:?} is a button and has no value to set")]
    ButtonCannotBeSet { field: String },
    #[error("could not set field {field:?} -- it may not currently be visible on this panel")]
    SetFailed { field: String },
    #[error("{0}")]
    PanelDidNotAdvance(String),
    #[error(
        "this is not the wizard's terminal panel yet -- keep calling verify_next until \
         has_next is false and is_terminal is true, then call verify_submit"
    )]
    NotOnTerminalPanel,
    #[error("{field:?} is not currently visible, so it has no screen position to screenshot")]
    ControlNotVisible { field: String },
    #[error("a screenshot could not be taken: {0}")]
    Screenshot(String),
    #[error(transparent)]
    Verify(#[from] VerifyError),
}

/// Refuses a revision that is not exactly `current` -- the same read/
/// mutation contract `u2s_mcp::session::SessionStore::read`/
/// `begin_mutation` already enforce, reimplemented here rather than
/// depending on that crate (see this module's own doc).
pub fn check_revision(handle: &str, current: u64, asked: u64) -> Result<(), InteractionError> {
    if asked < current {
        Err(InteractionError::RevisionStale {
            handle: handle.to_owned(),
            current,
            asked,
        })
    } else if asked > current {
        Err(InteractionError::RevisionAhead {
            handle: handle.to_owned(),
            current,
            asked,
        })
    } else {
        Ok(())
    }
}

/// The AEM guide-runtime widget classes [`ControlKind::from_class_name`]
/// recognises -- `Other` covers everything the inventory still lists
/// (never dropped, per this module's own doc on why the raw `class_name`
/// travels alongside the derived kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Text,
    Number,
    Date,
    Radio,
    Checkbox,
    Dropdown,
    Button,
    Other,
}

impl ControlKind {
    /// Classifies a guide node's own `className` (a space-separated list
    /// of AEM guide-runtime classes, e.g. `"guideFieldNode guideTextBox"`)
    /// -- a best guess from what this crate's own live testing has
    /// observed, the same "kept as one spot to correct" caveat every other
    /// selector in `crate::flow::wizard_js` carries. Checked most-specific
    /// first: none of these substrings collide with each other today, but
    /// order still matters if a future one ever does.
    pub fn from_class_name(class_name: &str) -> Self {
        if class_name.contains("guideRadioButton") {
            Self::Radio
        } else if class_name.contains("guideCheckBox") {
            Self::Checkbox
        } else if class_name.contains("guideDropDownList") {
            Self::Dropdown
        } else if class_name.contains("guideDatePicker") {
            Self::Date
        } else if class_name.contains("guideNumericBox") {
            Self::Number
        } else if class_name.contains("guideButton") {
            Self::Button
        } else if class_name.contains("guideTextBox") {
            Self::Text
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ControlOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ControlPosition {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One control on the form, as [`control_js::INVENTORY`] reports it.
/// `raw_value` and `class_name` travel alongside the derived `value`/`kind`
/// deliberately -- see this module's own doc and [`ControlKind::from_class_name`]'s.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Control {
    pub field: String,
    pub som: Option<String>,
    pub kind: ControlKind,
    pub class_name: String,
    pub label: Option<String>,
    pub options: Vec<ControlOption>,
    /// The save value(s) this control currently holds -- a plain string
    /// for everything except a multi-select checkbox, which is
    /// normalised from `raw_value`'s own `\n`-joined convention into a
    /// JSON array (see [`parse_controls`]'s own doc).
    pub value: Value,
    /// Exactly what the guide node's own `.value` held, unnormalised --
    /// kept so a live run that finds the `\n`-joined convention wrong for
    /// some form can be corrected from this field's value alone.
    pub raw_value: Value,
    pub multi_select: bool,
    /// Model-visible *and* currently on screen (`offsetParent !== null`).
    pub visible: bool,
    /// Model-visible regardless of whether the DOM element for it exists
    /// yet -- a control can be `model_visible` but not `visible` on a
    /// panel the walk has not reached.
    pub model_visible: bool,
    pub enabled: bool,
    pub required: bool,
    /// The nearest ancestor panel's own SOM expression, or `None` for a
    /// control directly on the root panel.
    pub panel: Option<String>,
    /// Where this control is drawn, in document pixels with the origin at
    /// the page's top-left -- exactly the shape `verify_screenshot`'s
    /// `field` argument needs, and `None` whenever [`Self::visible`] is
    /// `false` (nothing to report a position for).
    pub position: Option<ControlPosition>,
}

/// A field `verify_set`/`verify_next`/`verify_prev`/`verify_reset` changed
/// as a side effect of the interaction, excluding the field the caller
/// itself set (if any).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub field: String,
    pub from: Value,
    pub to: Value,
}

/// What one interaction did, beyond the value it explicitly set (if any).
/// The shared shape `verify_set`, `verify_next`, `verify_prev` and
/// `verify_reset` all return -- deliberately one type rather than four
/// near-identical ad hoc objects, the same reasoning
/// `u2s-render-xfa::session::Interaction` already applied to the XFA
/// render MCP's own session tools.
#[derive(Debug, Clone, Serialize)]
pub struct Interaction {
    pub revision: u64,
    pub field: Option<String>,
    pub value: Value,
    pub values_changed: bool,
    pub side_effects: Vec<FieldChange>,
    pub appeared: Vec<String>,
    pub disappeared: Vec<String>,
    pub panel: Value,
    pub has_next: bool,
    pub is_terminal: bool,
    pub console_errors: Vec<String>,
    pub failed_requests: Vec<FailedRequest>,
}

/// `verify_open`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct OpenedFormResult {
    pub form: String,
    pub revision: u64,
    pub form_jcr_path: String,
    pub form_name: String,
    pub panel: Value,
    pub has_next: bool,
    pub is_terminal: bool,
    pub findings: Vec<Finding>,
}

/// `verify_controls`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct ControlsResult {
    pub revision: u64,
    pub panel: Value,
    pub has_next: bool,
    pub is_terminal: bool,
    pub controls: Vec<Control>,
}

/// `verify_submit`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct SubmitResult {
    pub revision: u64,
    pub steps: Vec<Step>,
    pub artefacts: Vec<Artefact>,
    pub findings: Vec<Finding>,
}

/// `verify_screenshot`'s result -- the raw bytes plus enough to build the
/// inline-or-blob tool result; `crate::server` owns that decision (same
/// `max_inline_bytes` threshold every screenshot in this crate uses).
pub struct ScreenshotResult {
    pub bytes: Vec<u8>,
    pub width_px: u32,
    pub height_px: u32,
    pub mime: &'static str,
    pub ext: &'static str,
}

/// The JS `crate::interactive` evaluates against the page -- kept apart
/// from `crate::flow::wizard_js` since this vocabulary (an inventory of
/// every control, a snapshot for diffing) is specific to the interactive
/// tools; `verify_run` never needs it.
mod control_js {
    /// Every leaf field on the form, walked the same way
    /// [`crate::flow::wizard_js::fill_field`] locates one by name --
    /// `guideBridge.resolveNode` on the root panel, then `.items`
    /// recursively -- except this collects every leaf instead of stopping
    /// at the first name match. A leaf with no `.name` (a static text or
    /// image component, not an input) is skipped: it is not a control an
    /// agent could set.
    ///
    /// `position` is `null` whenever the control's own DOM element is not
    /// currently on screen (`offsetParent === null`) -- there is nothing
    /// meaningful to report a position for, and `verify_screenshot`
    /// refuses a `field` with no position rather than crop a stale
    /// coordinate against whatever panel happens to be visible now.
    /// `x`/`y` add `window.scrollX`/`scrollY` to `getBoundingClientRect()`
    /// so the rect is in *document* coordinates, matching
    /// `u2s_verify_core::browser::ScreenshotArea::Clip`'s own contract
    /// (`capture_beyond_viewport`).
    pub const INVENTORY: &str = r#"JSON.stringify((function() {
        var root = guideBridge.resolveNode('guide[0].guide1[0].guideRootPanel[0]');
        var out = [];
        function domOf(node) {
            return node && node.id ? document.getElementById(node.id) : null;
        }
        function rectOf(el) {
            if (!el || el.offsetParent === null) { return null; }
            var field = el.closest ? el.closest('.guideFieldNode') : null;
            var target = field || el;
            var r = target.getBoundingClientRect();
            return {
                x: r.left + window.scrollX,
                y: r.top + window.scrollY,
                width: r.width,
                height: r.height
            };
        }
        function options(node) {
            var values = node.enums || [];
            var labels = node.enumNames || [];
            var out = [];
            for (var i = 0; i < values.length; i++) {
                out.push({
                    value: String(values[i]),
                    label: labels[i] != null ? String(labels[i]) : String(values[i])
                });
            }
            return out;
        }
        function walk(items, panelSom) {
            if (!items) { return; }
            for (var i = 0; i < items.length; i++) {
                var node = items[i];
                if (!node) { continue; }
                if (node.items && node.items.length) {
                    walk(node.items, node.somExpression || panelSom);
                    continue;
                }
                if (!node.name) { continue; }
                var el = domOf(node);
                out.push({
                    field: node.name,
                    som: node.somExpression || null,
                    class_name: node.className || '',
                    label: node.title || null,
                    options: options(node),
                    raw_value: node.value === undefined ? null : node.value,
                    multi_select: !!node.multiSelect,
                    model_visible: node.visible !== false,
                    visible: node.visible !== false && !!(el && el.offsetParent !== null),
                    enabled: node.enabled !== false,
                    required: !!node.mandatory,
                    panel: panelSom || null,
                    position: rectOf(el)
                });
            }
        }
        walk(root.items, root.somExpression || null);
        return out;
    })())"#;

    /// A compact `{field: {value, visible}}` map of every named leaf --
    /// cheaper than [`INVENTORY`] (no options, no position, no DOM
    /// `getBoundingClientRect`) since it exists only to be diffed
    /// before/after an interaction, by [`super::diff_snapshots`].
    pub const SNAPSHOT: &str = r#"JSON.stringify((function() {
        var root = guideBridge.resolveNode('guide[0].guide1[0].guideRootPanel[0]');
        var out = {};
        function domOf(node) {
            return node && node.id ? document.getElementById(node.id) : null;
        }
        function walk(items) {
            if (!items) { return; }
            for (var i = 0; i < items.length; i++) {
                var node = items[i];
                if (!node) { continue; }
                if (node.items && node.items.length) { walk(node.items); continue; }
                if (!node.name) { continue; }
                var el = domOf(node);
                out[node.name] = {
                    value: node.value === undefined ? null : node.value,
                    visible: node.visible !== false && !!(el && el.offsetParent !== null)
                };
            }
        }
        walk(root.items);
        return out;
    })())"#;

    /// Which of the root panel's own direct children is currently on
    /// screen, plus the latest navigation-event SOM
    /// (`crate::flow::wizard_js::PANEL_FINGERPRINT` reads the same global).
    /// `null` fields when no root child is visible yet (a form still
    /// settling right after open).
    pub const CURRENT_PANEL: &str = r#"JSON.stringify((function() {
        var root = guideBridge.resolveNode('guide[0].guide1[0].guideRootPanel[0]');
        var items = (root && root.items) || [];
        for (var i = 0; i < items.length; i++) {
            var node = items[i];
            var el = node && node.id ? document.getElementById(node.id) : null;
            if (el && el.offsetParent !== null) {
                return {
                    som: node.somExpression || null,
                    name: node.name || null,
                    title: node.title || null,
                    index: i,
                    nav_som: window.__u2sPanelNav || ''
                };
            }
        }
        return { som: null, name: null, title: null, index: null, nav_som: window.__u2sPanelNav || '' };
    })())"#;
}

/// Parses [`control_js::INVENTORY`]'s JSON into [`Control`]s.
pub fn parse_controls(raw: &str) -> Result<Vec<Control>, InteractionError> {
    let entries: Vec<Value> = serde_json::from_str(raw).map_err(|err| {
        InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            format!("could not parse the control inventory: {err}"),
        ))
    })?;
    Ok(entries.iter().filter_map(parse_one_control).collect())
}

fn parse_one_control(entry: &Value) -> Option<Control> {
    let field = entry.get("field")?.as_str()?.to_owned();
    let class_name = entry
        .get("class_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let kind = ControlKind::from_class_name(&class_name);
    let som = entry.get("som").and_then(Value::as_str).map(str::to_owned);
    let label = entry
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let options = entry
        .get("options")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(ControlOption {
                        value: item.get("value")?.as_str()?.to_owned(),
                        label: item
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let raw_value = entry.get("raw_value").cloned().unwrap_or(Value::Null);
    let multi_select = entry
        .get("multi_select")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let value = normalize_value(&raw_value, multi_select);
    let visible = entry
        .get("visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model_visible = entry
        .get("model_visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let enabled = entry
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let required = entry
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let panel = entry
        .get("panel")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let position = entry
        .get("position")
        .filter(|value| !value.is_null())
        .and_then(|value| {
            Some(ControlPosition {
                x: value.get("x")?.as_f64()?,
                y: value.get("y")?.as_f64()?,
                width: value.get("width")?.as_f64()?,
                height: value.get("height")?.as_f64()?,
            })
        });

    Some(Control {
        field,
        som,
        kind,
        class_name,
        label,
        options,
        value,
        raw_value,
        multi_select,
        visible,
        model_visible,
        enabled,
        required,
        panel,
        position,
    })
}

/// A multi-select checkbox's `raw_value` is a single string with every
/// selected option's value joined by `\n` (the AEM Foundation runtime's
/// own observed convention -- not documented in `specs/AEM.md`, so this
/// is a best guess the same way every selector in `crate::flow::wizard_js`
/// is; a live run that finds it wrong corrects it here, in the one place
/// that assumes it). Normalised into a JSON array so a caller never has to
/// know the `\n` convention exists; [`validate_value`] reverses this
/// exact transform when setting a checkbox back.
fn normalize_value(raw_value: &Value, multi_select: bool) -> Value {
    if !multi_select {
        return raw_value.clone();
    }
    match raw_value.as_str() {
        Some("") => Value::Array(Vec::new()),
        Some(s) => Value::Array(
            s.split('\n')
                .filter(|part| !part.is_empty())
                .map(|part| Value::String(part.to_owned()))
                .collect(),
        ),
        None => Value::Array(Vec::new()),
    }
}

fn scalar_to_option_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Checks `requested` against `control`'s own options (for a radio,
/// dropdown or checkbox) and returns the value [`crate::flow::set_control`]
/// should actually be given -- a checkbox's array is rejoined with `\n`
/// (the reverse of [`normalize_value`]) since that is what the widget/model
/// itself expects. A radio or dropdown value that is not one of the
/// control's own options is refused naming the real ones, never silently
/// coerced -- the same "a refusal is only useful if it names what would
/// have worked" rule `crate::flow`'s own errors follow.
pub fn validate_value(control: &Control, requested: &Value) -> Result<Value, InteractionError> {
    match control.kind {
        ControlKind::Button => Err(InteractionError::ButtonCannotBeSet {
            field: control.field.clone(),
        }),
        ControlKind::Radio | ControlKind::Dropdown => {
            let given = scalar_to_option_string(requested);
            if control.options.iter().any(|option| option.value == given) {
                Ok(Value::String(given))
            } else {
                Err(InteractionError::ValueNotAnOption {
                    field: control.field.clone(),
                    given,
                    options: control.options.iter().map(|o| o.value.clone()).collect(),
                })
            }
        }
        ControlKind::Checkbox => {
            let requested_values: Vec<String> = match requested {
                Value::Array(items) => items.iter().map(scalar_to_option_string).collect(),
                other => vec![scalar_to_option_string(other)],
            };
            for given in &requested_values {
                if !control.options.iter().any(|option| &option.value == given) {
                    return Err(InteractionError::ValueNotAnOption {
                        field: control.field.clone(),
                        given: given.clone(),
                        options: control.options.iter().map(|o| o.value.clone()).collect(),
                    });
                }
            }
            if control.multi_select {
                Ok(Value::String(requested_values.join("\n")))
            } else {
                Ok(Value::String(
                    requested_values.into_iter().next().unwrap_or_default(),
                ))
            }
        }
        ControlKind::Text | ControlKind::Number | ControlKind::Date | ControlKind::Other => {
            Ok(requested.clone())
        }
    }
}

/// Diffs two [`control_js::SNAPSHOT`] results, excluding `set_field`
/// (already reported as the interaction's own `value`/`values_changed`)
/// from `side_effects`. A field present in one snapshot but not the other
/// (a repeated-section instance created or destroyed, say) is treated as
/// appearing/disappearing by its `visible` flag alone in the snapshot it
/// *is* present in.
pub fn diff_snapshots(
    before: &Value,
    after: &Value,
    set_field: Option<&str>,
) -> (Vec<FieldChange>, Vec<String>, Vec<String>) {
    let empty = serde_json::Map::new();
    let before_map = before.as_object().unwrap_or(&empty);
    let after_map = after.as_object().unwrap_or(&empty);

    let mut side_effects = Vec::new();
    let mut appeared = Vec::new();
    let mut disappeared = Vec::new();

    for (field, after_entry) in after_map {
        let after_value = after_entry.get("value").cloned().unwrap_or(Value::Null);
        let after_visible = after_entry
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match before_map.get(field) {
            Some(before_entry) => {
                let before_value = before_entry.get("value").cloned().unwrap_or(Value::Null);
                let before_visible = before_entry
                    .get("visible")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if Some(field.as_str()) != set_field && before_value != after_value {
                    side_effects.push(FieldChange {
                        field: field.clone(),
                        from: before_value,
                        to: after_value,
                    });
                }
                if !before_visible && after_visible {
                    appeared.push(field.clone());
                } else if before_visible && !after_visible {
                    disappeared.push(field.clone());
                }
            }
            None if after_visible => appeared.push(field.clone()),
            None => {}
        }
    }
    for (field, before_entry) in before_map {
        if after_map.contains_key(field) {
            continue;
        }
        if before_entry
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            disappeared.push(field.clone());
        }
    }

    side_effects.sort_by(|a, b| a.field.cmp(&b.field));
    appeared.sort();
    disappeared.sort();
    (side_effects, appeared, disappeared)
}

async fn snapshot(page: &browser::PageHandle) -> Value {
    let raw = page.evaluate_string(control_js::SNAPSHOT).await.unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

async fn current_panel(page: &browser::PageHandle) -> Value {
    let raw = page
        .evaluate_string(control_js::CURRENT_PANEL)
        .await
        .unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}

/// Removes `form`'s entry from `state` if it is the one open there,
/// leaving `state.open_form` untouched (whatever it held: `None`, or a
/// different handle) on a mismatch -- this module's own doc explains why
/// every mutating call takes ownership this way rather than holding a
/// borrow of `state` across an `.await`.
fn take_form(state: &mut SessionState, form: &str) -> Result<OpenForm, InteractionError> {
    match &state.open_form {
        Some(existing) if existing.handle == form => {
            Ok(state.open_form.take().expect("just matched Some above"))
        }
        _ => Err(InteractionError::UnknownForm {
            handle: form.to_owned(),
        }),
    }
}

/// Installs `package_bytes` on `session_id`'s AEM session and opens its
/// form, refusing if a form is already open there
/// ([`InteractionError::FormAlreadyOpen`]) or if `guideBridge` never
/// appears (the same signal `verify_run`'s own `guide_bridge_not_detected`
/// finding reports, raised as a hard error here since there is nothing an
/// interactive caller could do with an unusable page).
pub async fn open(
    profile: &Profile,
    driver: &dyn FormDriver,
    pool: &SessionPool,
    session_id: &str,
    package_bytes: Vec<u8>,
) -> Result<OpenedFormResult, InteractionError> {
    let package = package_check::inspect(&package_bytes).map_err(|err| {
        InteractionError::Verify(VerifyError::new(ErrorKind::PackageInvalid, err.to_string()))
    })?;

    let docker = DockerLifecycle::connect().await.map_err(|err| {
        InteractionError::Verify(VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))
    })?;

    let (mut guard, _boot_findings) = crate::session::ensure(pool, session_id, &docker, profile).await?;
    let state = guard
        .as_mut()
        .expect("ensure always leaves Some on success");

    if let Some(existing) = &state.open_form {
        return Err(InteractionError::FormAlreadyOpen {
            handle: existing.handle.clone(),
        });
    }

    let opened = flow::open_live_form(
        profile,
        driver,
        &state.instances,
        &mut state.installed_package_path,
        package,
        package_bytes,
    )
    .await?;

    if !opened.bridge_ready {
        opened.form.close().await;
        flow::uninstall_if_installed(
            profile,
            &state.instances,
            &mut state.installed_package_path,
            "a form that never became interactive",
        )
        .await;
        return Err(InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            "the global guideBridge object was not observed on this form -- see verify_run's \
             own guide_bridge_not_detected finding for the same signal"
                .to_owned(),
        )));
    }

    let findings = opened.findings;
    let form_jcr_path = opened.form.package.summary.form_jcr_path.clone();
    let form_name = opened.form.package.summary.form_name.clone();
    let panel = current_panel(&opened.form.page).await;
    let nav = flow::wait_for_navigation_controls(
        &opened.form.page,
        &driver.has_next_js(),
        &driver.terminal_panel_js(),
    )
    .await;

    let handle = format!("form_{}", uuid::Uuid::new_v4().simple());
    state.open_form = Some(OpenForm {
        handle: handle.clone(),
        revision: 0,
        live: opened.form,
        opened_at: Instant::now(),
    });
    state.touch();

    Ok(OpenedFormResult {
        form: handle,
        revision: 0,
        form_jcr_path,
        form_name,
        panel,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        findings,
    })
}

/// The inventory of every control, the current panel and the driver's own
/// navigation signals -- reads the form, never mutates it, so `revision`
/// must match exactly (like a read anywhere else in this workspace) but
/// this call never advances it.
pub async fn controls(
    pool: &SessionPool,
    driver: &dyn FormDriver,
    session_id: &str,
    form: &str,
    revision: u64,
) -> Result<ControlsResult, InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let open_form = take_form(state, form)?;

    let outcome = read_controls(&open_form, driver, revision).await;

    state.open_form = Some(open_form);
    let result = outcome?;
    state.touch();
    Ok(result)
}

async fn read_controls(
    open_form: &OpenForm,
    driver: &dyn FormDriver,
    revision: u64,
) -> Result<ControlsResult, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, revision)?;
    let page = &open_form.live.page;
    let raw = page.evaluate_string(control_js::INVENTORY).await.unwrap_or_default();
    let controls = parse_controls(&raw)?;
    let panel = current_panel(page).await;
    let nav =
        flow::wait_for_navigation_controls(page, &driver.has_next_js(), &driver.terminal_panel_js())
            .await;
    Ok(ControlsResult {
        revision: open_form.revision,
        panel,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        controls,
    })
}

/// Sets one control by `field` the way a person would
/// ([`crate::flow::set_control`], the same setter `verify_run`'s own
/// `fill` uses), refusing a `field` this form does not have or a value
/// that is not one of a choice control's own options before ever touching
/// the page.
pub async fn set(
    pool: &SessionPool,
    driver: &dyn FormDriver,
    session_id: &str,
    form: &str,
    expected_revision: u64,
    field: &str,
    value: &Value,
) -> Result<Interaction, InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let mut open_form = take_form(state, form)?;

    let outcome = set_inner(&open_form, driver, expected_revision, field, value).await;
    let result = outcome.map(|mut interaction| {
        open_form.revision += 1;
        interaction.revision = open_form.revision;
        interaction
    });

    state.open_form = Some(open_form);
    let interaction = result?;
    state.touch();
    Ok(interaction)
}

async fn set_inner(
    open_form: &OpenForm,
    driver: &dyn FormDriver,
    expected_revision: u64,
    field: &str,
    value: &Value,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;

    let raw = page.evaluate_string(control_js::INVENTORY).await.unwrap_or_default();
    let controls = parse_controls(&raw)?;
    let control = controls
        .iter()
        .find(|control| control.field == field)
        .ok_or_else(|| InteractionError::UnknownField {
            field: field.to_owned(),
            known: controls.iter().map(|c| c.field.clone()).collect(),
        })?;
    let normalized = validate_value(control, value)?;

    let before = snapshot(page).await;
    let report = flow::set_control(page, field, &normalized).await;
    if !report.set {
        return Err(InteractionError::SetFailed {
            field: field.to_owned(),
        });
    }
    let after = snapshot(page).await;
    let (side_effects, appeared, disappeared) = diff_snapshots(&before, &after, Some(field));
    let target_before = before
        .get(field)
        .and_then(|entry| entry.get("value"))
        .cloned()
        .unwrap_or(Value::Null);
    let target_after = after
        .get(field)
        .and_then(|entry| entry.get("value"))
        .cloned()
        .unwrap_or(Value::Null);
    let panel = current_panel(page).await;
    let nav =
        flow::wait_for_navigation_controls(page, &driver.has_next_js(), &driver.terminal_panel_js())
            .await;

    Ok(Interaction {
        revision: 0, // overwritten by the caller once the mutation commits
        field: Some(field.to_owned()),
        value: normalized,
        values_changed: target_before != target_after,
        side_effects,
        appeared,
        disappeared,
        panel,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        console_errors: page.console_errors(),
        failed_requests: page.failed_requests(),
    })
}

/// Clicks `click_js` (the driver's own next- or previous-panel control)
/// and confirms the panel actually changed
/// ([`crate::flow::advance_panel`], the same poll `verify_run`'s own
/// wizard walk uses for "next"), then reports the same [`Interaction`]
/// shape [`set`] does with `field`/`value` both `None`.
async fn advance(
    pool: &SessionPool,
    driver: &dyn FormDriver,
    session_id: &str,
    form: &str,
    expected_revision: u64,
    click_js: &str,
) -> Result<Interaction, InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let mut open_form = take_form(state, form)?;

    let outcome = advance_inner(&open_form, driver, expected_revision, click_js).await;
    let result = outcome.map(|mut interaction| {
        open_form.revision += 1;
        interaction.revision = open_form.revision;
        interaction
    });

    state.open_form = Some(open_form);
    let interaction = result?;
    state.touch();
    Ok(interaction)
}

async fn advance_inner(
    open_form: &OpenForm,
    driver: &dyn FormDriver,
    expected_revision: u64,
    click_js: &str,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;

    let before = snapshot(page).await;
    if let Some(message) = flow::advance_panel(page, click_js)
        .await
        .failure_message("this panel")
    {
        return Err(InteractionError::PanelDidNotAdvance(message));
    }
    let after = snapshot(page).await;
    let (side_effects, appeared, disappeared) = diff_snapshots(&before, &after, None);
    let panel = current_panel(page).await;
    let nav =
        flow::wait_for_navigation_controls(page, &driver.has_next_js(), &driver.terminal_panel_js())
            .await;

    Ok(Interaction {
        revision: 0,
        field: None,
        value: Value::Null,
        values_changed: false,
        side_effects,
        appeared,
        disappeared,
        panel,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        console_errors: page.console_errors(),
        failed_requests: page.failed_requests(),
    })
}

/// Advances to the next panel via the driver's own `click_next_js`.
pub async fn next(
    pool: &SessionPool,
    driver: &dyn FormDriver,
    session_id: &str,
    form: &str,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    let click_js = driver.click_next_js();
    advance(pool, driver, session_id, form, expected_revision, &click_js).await
}

/// Goes back to the previous panel via the driver's own `click_prev_js`.
pub async fn prev(
    pool: &SessionPool,
    driver: &dyn FormDriver,
    session_id: &str,
    form: &str,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    let click_js = driver.click_prev_js();
    advance(pool, driver, session_id, form, expected_revision, &click_js).await
}

/// Reopens the same form URL fresh -- cheaper than closing and opening
/// again (the browser session and installed package are both kept), and
/// the whole point of "reset" is to get back to how the form looked when
/// it was first opened.
pub async fn reset(
    profile: &Profile,
    driver: &dyn FormDriver,
    pool: &SessionPool,
    session_id: &str,
    form: &str,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let mut open_form = take_form(state, form)?;

    let outcome = reset_inner(profile, driver, &mut open_form, expected_revision).await;
    let result = outcome.map(|mut interaction| {
        open_form.revision += 1;
        interaction.revision = open_form.revision;
        interaction
    });

    state.open_form = Some(open_form);
    let interaction = result?;
    state.touch();
    Ok(interaction)
}

async fn reset_inner(
    profile: &Profile,
    driver: &dyn FormDriver,
    open_form: &mut OpenForm,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;

    let before = snapshot(&open_form.live.page).await;

    let url = open_form.live.form_url_for_browser.clone();
    let new_page = open_form
        .live
        .browser_session
        .open(&url, Some((&profile.aem_user, &profile.aem_password)))
        .await
        .map_err(|err| {
            InteractionError::Verify(VerifyError::new(ErrorKind::RenderTimeout, err.to_string()))
        })?;
    let old_page = std::mem::replace(&mut open_form.live.page, new_page);
    old_page.close().await;

    if !flow::wait_for_guide_bridge(&open_form.live.page).await {
        return Err(InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            "guideBridge was not observed again after reset".to_owned(),
        )));
    }
    let _ = open_form
        .live
        .page
        .evaluate_bool(flow::wizard_js::OBSERVE_PANEL_NAVIGATION)
        .await;

    let after = snapshot(&open_form.live.page).await;
    let (side_effects, appeared, disappeared) = diff_snapshots(&before, &after, None);
    let panel = current_panel(&open_form.live.page).await;
    let nav = flow::wait_for_navigation_controls(
        &open_form.live.page,
        &driver.has_next_js(),
        &driver.terminal_panel_js(),
    )
    .await;

    Ok(Interaction {
        revision: 0,
        field: None,
        value: Value::Null,
        values_changed: false,
        side_effects,
        appeared,
        disappeared,
        panel,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        console_errors: open_form.live.page.console_errors(),
        failed_requests: open_form.live.page.failed_requests(),
    })
}

/// Screenshots the whole page, or -- when `field` is given -- just that
/// control's own on-screen rectangle (from the inventory's own
/// `position`, refused when the control has none: nothing is visible to
/// crop). A read, like [`controls`]: `revision` must match exactly but
/// this call never advances it.
pub async fn screenshot(
    pool: &SessionPool,
    session_id: &str,
    form: &str,
    revision: u64,
    format: browser::ImageFormat,
    field: Option<&str>,
) -> Result<ScreenshotResult, InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let open_form = take_form(state, form)?;

    let outcome = screenshot_inner(&open_form, revision, format, field).await;

    state.open_form = Some(open_form);
    let result = outcome?;
    state.touch();
    Ok(result)
}

async fn screenshot_inner(
    open_form: &OpenForm,
    revision: u64,
    format: browser::ImageFormat,
    field: Option<&str>,
) -> Result<ScreenshotResult, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, revision)?;
    let page = &open_form.live.page;

    let area = match field {
        None => browser::ScreenshotArea::FullPage,
        Some(name) => {
            let raw = page.evaluate_string(control_js::INVENTORY).await.unwrap_or_default();
            let controls = parse_controls(&raw)?;
            let control = controls
                .iter()
                .find(|control| control.field == name)
                .ok_or_else(|| InteractionError::UnknownField {
                    field: name.to_owned(),
                    known: controls.iter().map(|c| c.field.clone()).collect(),
                })?;
            let position = control
                .position
                .ok_or_else(|| InteractionError::ControlNotVisible {
                    field: name.to_owned(),
                })?;
            browser::ScreenshotArea::Clip(browser::ClipRect {
                x: position.x,
                y: position.y,
                width: position.width,
                height: position.height,
            })
        }
    };

    let (width_px, height_px) = match &area {
        browser::ScreenshotArea::FullPage => {
            let raw = page
                .evaluate_string(
                    "JSON.stringify({w: document.documentElement.scrollWidth, \
                     h: document.documentElement.scrollHeight})",
                )
                .await
                .unwrap_or_default();
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            (
                parsed.get("w").and_then(Value::as_u64).unwrap_or(0) as u32,
                parsed.get("h").and_then(Value::as_u64).unwrap_or(0) as u32,
            )
        }
        browser::ScreenshotArea::Clip(rect) => {
            (rect.width.round() as u32, rect.height.round() as u32)
        }
    };

    let bytes = page
        .screenshot(area, format)
        .await
        .map_err(|err| InteractionError::Screenshot(err.to_string()))?;

    Ok(ScreenshotResult {
        bytes,
        width_px,
        height_px,
        mime: format.mime(),
        ext: format.ext(),
    })
}

/// Submits from the current panel -- refused
/// ([`InteractionError::NotOnTerminalPanel`]) unless the driver's own
/// navigation signals already say this is the wizard's last panel, the
/// same "no next control, driver's own terminal-panel check fires" test
/// `verify_run`'s own wizard walk uses. Shares
/// [`crate::flow::submit_and_capture_artefact`] with `verify_run`, so a
/// download or Document of Record is captured exactly the same way either
/// path reaches it. Does not close the form -- call `verify_close`
/// afterward.
pub async fn submit(
    pool: &SessionPool,
    driver: &dyn FormDriver,
    profile: &Profile,
    blobs: &BlobStore,
    session_id: &str,
    form: &str,
    expected_revision: u64,
) -> Result<SubmitResult, InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let mut open_form = take_form(state, form)?;

    let outcome = submit_inner(
        &open_form,
        driver,
        profile,
        blobs,
        expected_revision,
        &state.instances,
    )
    .await;
    let result = outcome.map(|mut submitted| {
        open_form.revision += 1;
        submitted.revision = open_form.revision;
        submitted
    });

    state.open_form = Some(open_form);
    let submitted = result?;
    state.touch();
    Ok(submitted)
}

async fn submit_inner(
    open_form: &OpenForm,
    driver: &dyn FormDriver,
    profile: &Profile,
    blobs: &BlobStore,
    expected_revision: u64,
    instances: &Instances,
) -> Result<SubmitResult, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;
    let package = &open_form.live.package;

    let nav =
        flow::wait_for_navigation_controls(page, &driver.has_next_js(), &driver.terminal_panel_js())
            .await;
    if !nav.is_terminal {
        return Err(InteractionError::NotOnTerminalPanel);
    }

    if matches!(profile.submit, SubmitArtefact::Download) {
        flow::check_redacto_reachable(profile).await?;
    }

    let mut steps = Vec::new();
    let mut artefacts = Vec::new();
    let mut findings = Vec::new();
    flow::submit_and_capture_artefact(
        page,
        &open_form.live.browser_session,
        profile,
        driver,
        package,
        instances,
        blobs,
        &mut steps,
        &mut artefacts,
        &mut findings,
    )
    .await;

    Ok(SubmitResult {
        revision: 0,
        steps,
        artefacts,
        findings,
    })
}

/// Closes the page and browser session, uninstalls the package, and
/// releases this session's `open_form` slot.
pub async fn close(
    pool: &SessionPool,
    profile: &Profile,
    session_id: &str,
    form: &str,
) -> Result<(), InteractionError> {
    let mut guard = pool.lock(session_id).await;
    let state = guard.as_mut().ok_or_else(|| InteractionError::UnknownForm {
        handle: form.to_owned(),
    })?;
    let open_form = take_form(state, form)?;

    open_form.live.close().await;
    flow::uninstall_if_installed(
        profile,
        &state.instances,
        &mut state.installed_package_path,
        "a closed interactive form",
    )
    .await;
    state.touch();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn control(kind_class: &str, options: &[(&str, &str)], multi_select: bool) -> Control {
        Control {
            field: "F1".to_owned(),
            som: Some("guide[0].guide1[0].F1".to_owned()),
            kind: ControlKind::from_class_name(kind_class),
            class_name: kind_class.to_owned(),
            label: Some("Field 1".to_owned()),
            options: options
                .iter()
                .map(|(v, l)| ControlOption {
                    value: (*v).to_owned(),
                    label: (*l).to_owned(),
                })
                .collect(),
            value: Value::Null,
            raw_value: Value::Null,
            multi_select,
            visible: true,
            model_visible: true,
            enabled: true,
            required: false,
            panel: None,
            position: None,
        }
    }

    #[test]
    fn check_revision_ok_stale_and_ahead_are_distinct() {
        assert!(check_revision("h", 3, 3).is_ok());
        assert!(matches!(
            check_revision("h", 3, 2),
            Err(InteractionError::RevisionStale { current: 3, asked: 2, .. })
        ));
        assert!(matches!(
            check_revision("h", 3, 4),
            Err(InteractionError::RevisionAhead { current: 3, asked: 4, .. })
        ));
    }

    #[test]
    fn control_kind_recognises_every_guide_runtime_class_this_module_knows() {
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideRadioButton"),
            ControlKind::Radio
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideCheckBox"),
            ControlKind::Checkbox
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideDropDownList"),
            ControlKind::Dropdown
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideDatePicker"),
            ControlKind::Date
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideNumericBox"),
            ControlKind::Number
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideTextBox"),
            ControlKind::Text
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode guideButton"),
            ControlKind::Button
        );
        assert_eq!(
            ControlKind::from_class_name("guideFieldNode somethingElse"),
            ControlKind::Other
        );
    }

    #[test]
    fn parse_controls_reads_every_field_the_inventory_js_can_produce() {
        let raw = r#"[
            {
                "field": "RB1", "som": "guide[0].guide1[0].RB1", "class_name": "guideRadioButton",
                "label": "Choice", "options": [{"value":"1","label":"One"},{"value":"2","label":"Two"}],
                "raw_value": "1", "multi_select": false, "model_visible": true, "visible": true,
                "enabled": true, "required": true, "panel": "guide[0].guide1[0].panel1",
                "position": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}
            },
            {
                "field": "CB1", "som": null, "class_name": "guideCheckBox", "label": null,
                "options": [{"value":"a","label":"A"},{"value":"b","label":"B"}],
                "raw_value": "a\nb", "multi_select": true, "model_visible": true, "visible": false,
                "enabled": true, "required": false, "panel": null, "position": null
            },
            { "class_name": "guideStaticText" }
        ]"#;
        let controls = parse_controls(raw).expect("parses");
        assert_eq!(controls.len(), 2);

        let rb1 = controls.iter().find(|c| c.field == "RB1").unwrap();
        assert_eq!(rb1.kind, ControlKind::Radio);
        assert_eq!(rb1.value, Value::String("1".to_owned()));
        assert_eq!(rb1.position, Some(ControlPosition { x: 1.0, y: 2.0, width: 3.0, height: 4.0 }));
        assert!(rb1.required);

        let cb1 = controls.iter().find(|c| c.field == "CB1").unwrap();
        assert_eq!(cb1.kind, ControlKind::Checkbox);
        assert_eq!(
            cb1.value,
            Value::Array(vec![Value::String("a".to_owned()), Value::String("b".to_owned())])
        );
        assert_eq!(cb1.raw_value, Value::String("a\nb".to_owned()));
        assert!(!cb1.visible);
        assert_eq!(cb1.position, None);

        // A leaf with no "field" key at all (the JS side never emits one
        // for a non-input component, but a malformed entry is skipped
        // defensively here too rather than panicking) contributes nothing.
        assert!(controls.iter().all(|c| !c.class_name.contains("guideStaticText")));
    }

    #[test]
    fn validate_value_accepts_a_radios_own_option_and_refuses_anything_else() {
        let radio = control("guideRadioButton", &[("1", "One"), ("2", "Two")], false);
        assert_eq!(
            validate_value(&radio, &Value::String("2".to_owned())).unwrap(),
            Value::String("2".to_owned())
        );
        let err = validate_value(&radio, &Value::String("9".to_owned())).unwrap_err();
        assert!(matches!(err, InteractionError::ValueNotAnOption { given, .. } if given == "9"));
    }

    #[test]
    fn validate_value_rejoins_a_multi_select_checkbox_array_with_newlines() {
        let checkbox = control("guideCheckBox", &[("a", "A"), ("b", "B")], true);
        let value = validate_value(
            &checkbox,
            &Value::Array(vec![Value::String("a".to_owned()), Value::String("b".to_owned())]),
        )
        .unwrap();
        assert_eq!(value, Value::String("a\nb".to_owned()));
    }

    #[test]
    fn validate_value_refuses_a_checkbox_array_entry_that_is_not_an_option() {
        let checkbox = control("guideCheckBox", &[("a", "A")], true);
        let err = validate_value(&checkbox, &Value::Array(vec![Value::String("z".to_owned())]))
            .unwrap_err();
        assert!(matches!(err, InteractionError::ValueNotAnOption { given, .. } if given == "z"));
    }

    #[test]
    fn validate_value_passes_free_text_through_untouched() {
        let text = control("guideTextBox", &[], false);
        assert_eq!(
            validate_value(&text, &Value::String("hello".to_owned())).unwrap(),
            Value::String("hello".to_owned())
        );
        assert_eq!(
            validate_value(&text, &Value::from(42)).unwrap(),
            Value::from(42)
        );
    }

    #[test]
    fn validate_value_refuses_to_set_a_button() {
        let button = control("guideButton", &[], false);
        let err = validate_value(&button, &Value::String("click".to_owned())).unwrap_err();
        assert!(matches!(err, InteractionError::ButtonCannotBeSet { .. }));
    }

    fn snap(pairs: &[(&str, Value, bool)]) -> Value {
        let mut map = serde_json::Map::new();
        for (field, value, visible) in pairs {
            map.insert(
                (*field).to_owned(),
                json!({ "value": value, "visible": visible }),
            );
        }
        Value::Object(map)
    }

    #[test]
    fn diff_snapshots_excludes_the_set_field_from_side_effects() {
        let before = snap(&[("A", Value::String("x".into()), true), ("B", Value::String("y".into()), true)]);
        let after = snap(&[("A", Value::String("z".into()), true), ("B", Value::String("y".into()), true)]);
        let (side_effects, appeared, disappeared) = diff_snapshots(&before, &after, Some("A"));
        assert!(side_effects.is_empty(), "the field the caller itself set must not show up as a side effect: {side_effects:?}");
        assert!(appeared.is_empty());
        assert!(disappeared.is_empty());
    }

    #[test]
    fn diff_snapshots_reports_a_value_side_effect_on_a_different_field() {
        let before = snap(&[("A", Value::String("x".into()), true), ("B", Value::String("y".into()), true)]);
        let after = snap(&[("A", Value::String("z".into()), true), ("B", Value::String("y2".into()), true)]);
        let (side_effects, _, _) = diff_snapshots(&before, &after, Some("A"));
        assert_eq!(side_effects.len(), 1);
        assert_eq!(side_effects[0].field, "B");
        assert_eq!(side_effects[0].from, Value::String("y".into()));
        assert_eq!(side_effects[0].to, Value::String("y2".into()));
    }

    #[test]
    fn diff_snapshots_reports_appeared_and_disappeared_by_visibility_flips() {
        let before = snap(&[("A", Value::Null, true), ("B", Value::Null, false)]);
        let after = snap(&[("A", Value::Null, false), ("B", Value::Null, true)]);
        let (side_effects, appeared, disappeared) = diff_snapshots(&before, &after, None);
        assert!(side_effects.is_empty(), "values did not change, only visibility: {side_effects:?}");
        assert_eq!(appeared, vec!["B".to_owned()]);
        assert_eq!(disappeared, vec!["A".to_owned()]);
    }
}
