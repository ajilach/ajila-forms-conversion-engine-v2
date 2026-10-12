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
//! **Instances and sessions.** An *instance* is one agent's AEM and
//! Chromium containers (`crate::instance`), picked by the injected
//! `instance_id`. A *session* is one form open in its own browser tab on
//! that instance, picked by the handle `verify_open` returned -- the same
//! role an `xfa_*` session handle plays. An instance holds up to
//! [`MAX_SESSIONS_PER_INSTANCE`] sessions, and several may show the same
//! package (they share its install, `crate::packages`). Sessions live in
//! `crate::instance::InstanceState::sessions`, under the instance's one
//! lock, so opening, closing and idle-sweeping always happen under the
//! lock `crate::instance::InstancePool` already provides. Calls on
//! different sessions of one instance therefore run one at a time, which a
//! single AEM author needs anyway for package installs.
//!
//! **Revisions.** Every call after `verify_open` names either `revision`
//! (a read: `verify_controls`, `verify_screenshot`) or `expected_revision`
//! (a mutation), refused if it does not match the session's own current
//! revision -- [`check_revision`]'s wording matches
//! `u2s_mcp::session::SessionStore`'s own error text so an agent sees the
//! same phrasing regardless of which u2s server it is talking to. A
//! mutation always advances the revision by exactly one, even when nothing
//! observable changed, so a caller's optimistic-concurrency check can never
//! be fooled by a no-op repeat.

use std::time::Instant;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use u2s_blob::BlobStore;
use u2s_verify_core::browser;
use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::types::{
    Artefact, Dialog, ErrorKind, FailedRequest, Finding, Step, VerifyError,
};

use crate::driver::FormDriver;
use crate::flow::{self, LiveForm};
use crate::package_check;
use crate::profile::{Profile, SubmitArtefact};
use crate::instance::{Containers, InstancePool, InstanceState};

/// The most sessions one instance keeps open at once. Each is a browser
/// tab on the instance's one Chromium; the cap keeps a runaway agent from
/// exhausting that browser's memory.
pub const MAX_SESSIONS_PER_INSTANCE: usize = 8;

/// One form open in its own browser tab on an instance -- lives in
/// `InstanceState::sessions` (this module's doc). `pub(crate)` rather than
/// `pub`: it wraps `crate::flow::LiveForm`, itself `pub(crate)` because
/// nothing outside this crate ever needs to name a live page directly.
pub(crate) struct OpenSession {
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
        "session {handle:?} is not open; it may never have existed, may have already been \
         closed, or its instance may have gone idle and been swept -- call verify_open again"
    )]
    UnknownSession { handle: String },
    #[error(
        "session {handle:?} is at revision {current}; you asked for {asked}, which is behind it -- \
         re-read at revision {current} before retrying"
    )]
    RevisionStale { handle: String, current: u64, asked: u64 },
    #[error(
        "session {handle:?} is at revision {current}; you asked for {asked}, which was never \
         issued -- the current revision is {current}"
    )]
    RevisionAhead { handle: String, current: u64, asked: u64 },
    #[error(
        "this instance already has {MAX_SESSIONS_PER_INSTANCE} sessions open ({open:?}) -- \
         call verify_close on one first"
    )]
    TooManySessions { open: Vec<String> },
    #[error(
        "no control {field:?} on this form -- call verify_controls for the available ones and \
         pass a control's `field` (its SOM expression)"
    )]
    UnknownField { field: String },
    #[error(
        "{field:?} names {} controls -- pass one's `field` (its SOM expression) instead: \
         {matches:?}", matches.len()
    )]
    AmbiguousField { field: String, matches: Vec<String> },
    #[error(
        "this form has no root panel (guideBridge.resolveNode('guide[0]').rootPanel is unset), \
         so it has no controls to list"
    )]
    RootPanelNotFound,
    #[error(
        "no navigable panel {panel:?} on this form -- pass one of these SOM expressions \
         (verify_controls lists them under `panels`): {known:?}"
    )]
    UnknownPanel { panel: String, known: Vec<String> },
    #[error(
        "{panel:?} names {} panels -- pass one's SOM expression instead: {matches:?}",
        matches.len()
    )]
    AmbiguousPanel { panel: String, matches: Vec<String> },
    #[error("{panel:?} is not visible right now, so the form will not show it")]
    PanelNotVisible { panel: String },
    #[error("{field:?} cannot be set to {given:?} -- its own options are {options:?}")]
    ValueNotAnOption {
        field: String,
        given: String,
        options: Vec<String>,
    },
    #[error("{field:?} is a button; it has no value to set. Press it with verify_click")]
    ButtonCannotBeSet { field: String },
    #[error(
        "{field:?} is readOnly: the form disabled it, so a visitor cannot change it; only the \
         form's own scripts can, often in response to another field"
    )]
    ReadOnlyField { field: String },
    #[error("{field:?} is a signature; a visitor draws it, so it has no value to set")]
    SignatureCannotBeSet { field: String },
    #[error("{field:?} is not a button; set it to a value with verify_set")]
    NotAButton { field: String },
    #[error(
        "{field:?} is not visible and enabled right now, so a visitor could not press it -- \
         call verify_controls to see what the form currently shows"
    )]
    NotClickable { field: String },
    #[error("{field:?} submits the form; use verify_submit, which also captures the result")]
    SubmitThroughClick { field: String },
    #[error("could not press {field:?}: its widget was not found on the page")]
    ClickFailed { field: String },
    #[error("could not set field {field:?} -- it may not currently be visible on this panel")]
    SetFailed { field: String },
    #[error("{0}")]
    PanelDidNotAdvance(String),
    #[error(
        "this is not the wizard's terminal panel yet -- keep calling verify_next until \
         has_next is false and is_terminal is true, then call verify_submit"
    )]
    NotOnTerminalPanel,
    #[error(
        "the form does not validate, so it was not submitted -- {}. Fix those fields \
         (verify_prev reaches earlier panels) and call verify_submit again",
        flow::describe_validation_errors(.errors)
    )]
    SubmitBlocked { errors: Vec<flow::ValidationError> },
    #[error("the form was not submitted: its validation could not be read ({0})")]
    SubmitValidationUnreadable(String),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Text,
    Number,
    Date,
    Radio,
    Checkbox,
    Dropdown,
    Button,
    /// A drawn signature (`guideScribble`), XFA's `signature` kind.
    Signature,
    Other,
}

/// What pressing a button does, read from its authored click expression
/// and the classes its widget renders -- the AEM counterpart of
/// `u2s_xfa::states::ClickEffect`, so an agent knows which button adds a
/// row before pressing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickEffect {
    /// Adds or removes an instance of a repeated panel.
    Instances,
    /// Submits the form; `verify_submit` is the tool for that.
    Submit,
    /// Wizard navigation; `verify_next`/`verify_prev` confirm the move.
    Navigation,
    /// Runs some other script.
    Script,
}

/// Classifies a button from its click expression (`jsonModel.clickExp`)
/// and its widget's classes. Navigation is recognised by class first,
/// since a wizard's next button may also run bookkeeping scripts. Pure.
pub fn click_effect(source: Option<&str>, widget_classes: &str) -> Option<ClickEffect> {
    let navigation_class = widget_classes
        .split_whitespace()
        .any(|class| matches!(class, "moveNext" | "movePrev" | "movePrevious"));
    if navigation_class {
        return Some(ClickEffect::Navigation);
    }
    let source = source.filter(|source| !source.trim().is_empty())?;
    if ["addInstance", "removeInstance", "insertInstance", "instanceManager"]
        .iter()
        .any(|call| source.contains(call))
    {
        Some(ClickEffect::Instances)
    } else if source.contains("submit(") {
        Some(ClickEffect::Submit)
    } else {
        Some(ClickEffect::Script)
    }
}

/// Whether a control takes input, the AEM counterpart of XFA's `access`.
/// AEM's runtime has no separate read-only flag on a field node (confirmed
/// live: `readOnly` is undefined), so this is `enabled` under the name the
/// XFA tools use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Access {
    Open,
    ReadOnly,
}

impl Access {
    pub fn from_enabled(enabled: bool) -> Self {
        if enabled { Access::Open } else { Access::ReadOnly }
    }
}

/// A control whose access changed during an interaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessChange {
    pub field: String,
    pub from: Access,
    pub to: Access,
}

impl ControlKind {
    /// Every kind, in the order `verify_controls`' `kinds` filter lists
    /// them in its refusal of an unknown one.
    pub const ALL: [ControlKind; 9] = [
        ControlKind::Text,
        ControlKind::Number,
        ControlKind::Date,
        ControlKind::Radio,
        ControlKind::Checkbox,
        ControlKind::Dropdown,
        ControlKind::Button,
        ControlKind::Signature,
        ControlKind::Other,
    ];

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
        } else if class_name.contains("guideScribble") {
            Self::Signature
        } else if class_name.contains("guideTextBox")
            || class_name.contains("guideTelephone")
            || class_name.contains("guideEmail")
        {
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
    /// The control's SOM expression -- what every tool takes as `field`.
    pub field: String,
    /// The guide node's own name. Not unique: every instance of a repeated
    /// panel repeats it.
    pub name: String,
    /// Which instance of its nearest enclosing repeatable panel this
    /// control belongs to; absent outside any repeatable panel.
    pub instance_index: Option<u32>,
    /// [`Access::Open`] when the control takes input, from `enabled`.
    pub access: Access,
    /// The format the form validates this control's value against, as
    /// authored: a regular expression (`^([^0-9]*)$`) or an AEM picture
    /// clause (`date{YYYY-MM-DD}`). Read from `validatePictureClause`, or
    /// from `displayPictureClause` when the form marks its display pattern
    /// as the validation pattern too (confirmed on `BAPJ_019`, whose name
    /// fields refuse any digit). Absent when the form checks no format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// The form's verdict on this control from its last validation
    /// (`isValid`, confirmed live): `true` until the form validates it --
    /// on a blocked `verify_next`, a refused `verify_submit`, or its own
    /// scripts -- so `true` does not mean a required field is filled.
    pub valid: Option<bool>,
    /// The message the form shows for a failed validation (`_errorText`,
    /// the runtime's own field; `errorMessage` from the spec does not exist
    /// at runtime, confirmed live). Absent while the control is valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// For a button: what pressing it does ([`click_effect`]); absent for
    /// any other control and for a button with no click expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click: Option<ClickEffect>,
    pub kind: ControlKind,
    pub class_name: String,
    pub label: Option<String>,
    pub options: Vec<ControlOption>,
    /// The save value(s) this control currently holds -- a plain string
    /// for everything except a multi-select checkbox, which is normalised
    /// from `raw_value`'s comma-joined form into a JSON array
    /// ([`MULTI_SELECT_SEPARATOR`]).
    pub value: Value,
    /// Exactly what the guide node's own `.value` held, unnormalised.
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

/// Which tool produced an [`Interaction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Set,
    Click,
    Next,
    Prev,
    Goto,
    Reset,
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
    pub kind: InteractionKind,
    pub field: Option<String>,
    pub value: Value,
    pub values_changed: bool,
    pub side_effects: Vec<FieldChange>,
    pub appeared: Vec<String>,
    pub disappeared: Vec<String>,
    /// Controls whose access changed, e.g. a script disabling a field.
    pub access_changes: Vec<AccessChange>,
    /// Whether any repeated panel gained or lost an instance.
    pub instances_changed: bool,
    /// Something worth knowing that is not an error, e.g. an Add button
    /// that changed nothing because its group is at `maxOccur`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub panel: Value,
    pub has_next: bool,
    pub is_terminal: bool,
    pub console_errors: Vec<String>,
    pub failed_requests: Vec<FailedRequest>,
    /// Native dialogs (`alert`, `confirm`, ...) the form raised since it
    /// opened, each already accepted the way a visitor clicking OK would.
    pub dialogs: Vec<Dialog>,
}

/// One step of a navigable container -- a wizard step, a tab, an
/// accordion section -- as `verify_open`/`verify_controls` list it and
/// `verify_goto` accepts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelInfo {
    /// The panel's SOM expression, what `verify_goto` takes.
    pub som: String,
    pub name: String,
    pub title: Option<String>,
    /// The container whose steps this panel is one of.
    pub parent: String,
    /// How many navigable containers enclose this one (0 for the root's
    /// own steps).
    pub depth: u32,
    /// The container's layout, the last segment of its resource type
    /// (`wizard`, a tabs layout, an accordion).
    pub layout: String,
    pub visible: bool,
    /// Whether this is the step its container currently shows.
    pub active: bool,
}

/// Parses [`control_js::PANELS`]' JSON.
pub fn parse_panels(raw: &str) -> Result<Vec<PanelInfo>, InteractionError> {
    let parsed: Value = serde_json::from_str(raw).map_err(|err| {
        InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            format!("could not parse the panel list: {err}"),
        ))
    })?;
    if parsed.get("error").and_then(Value::as_str) == Some("root_panel_not_found") {
        return Err(InteractionError::RootPanelNotFound);
    }
    serde_json::from_value(parsed).map_err(|err| {
        InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            format!("could not read the panel list: {err}"),
        ))
    })
}

/// Finds the panel `panel` names: its SOM expression, or a name exactly
/// one listed panel carries. The [`resolve_field`] rules, for panels. Pure.
pub fn resolve_panel<'a>(panels: &'a [PanelInfo], panel: &str) -> Result<&'a PanelInfo, InteractionError> {
    if let Some(found) = panels.iter().find(|info| info.som == panel) {
        return Ok(found);
    }
    let named: Vec<&PanelInfo> = panels.iter().filter(|info| info.name == panel).collect();
    match named.as_slice() {
        [only] => Ok(only),
        [] => Err(InteractionError::UnknownPanel {
            panel: panel.to_owned(),
            known: panels.iter().map(|info| info.som.clone()).collect(),
        }),
        many => Err(InteractionError::AmbiguousPanel {
            panel: panel.to_owned(),
            matches: many.iter().map(|info| info.som.clone()).collect(),
        }),
    }
}

/// `verify_controls`' paging and filter, validated once when the call
/// arrives so nothing later handles an out-of-range limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlsQuery {
    offset: usize,
    limit: usize,
    kinds: Vec<ControlKind>,
}

impl ControlsQuery {
    pub const DEFAULT_LIMIT: usize = 100;
    pub const MAX_LIMIT: usize = 500;

    /// `limit` defaults to [`Self::DEFAULT_LIMIT`] and must lie in
    /// `1..=MAX_LIMIT`; an empty `kinds` means every kind. The bounds match
    /// `xfa_controls`.
    pub fn new(offset: Option<u64>, limit: Option<u64>, kinds: Vec<ControlKind>) -> Result<Self, String> {
        let limit = match limit {
            None => Self::DEFAULT_LIMIT,
            Some(limit) => usize::try_from(limit)
                .ok()
                .filter(|limit| (1..=Self::MAX_LIMIT).contains(limit))
                .ok_or_else(|| format!("limit must be between 1 and {}, got {limit}", Self::MAX_LIMIT))?,
        };
        let offset = usize::try_from(offset.unwrap_or(0))
            .map_err(|_| "offset is too large".to_owned())?;
        Ok(Self { offset, limit, kinds })
    }

    /// The page of `controls` this query asks for, with the total that
    /// match its kinds and where the next page starts (`None` at the end).
    /// Mirrors `u2s_xfa::states::ControlsWindow` (a disclosed copy of about
    /// fifteen lines: this crate does not depend on the XFA engine). Pure.
    pub fn window(&self, controls: Vec<Control>) -> (Vec<Control>, usize, Option<usize>) {
        let matching: Vec<Control> = controls
            .into_iter()
            .filter(|control| self.kinds.is_empty() || self.kinds.contains(&control.kind))
            .collect();
        let total = matching.len();
        let page: Vec<Control> = matching.into_iter().skip(self.offset).take(self.limit).collect();
        let end = self.offset.saturating_add(page.len());
        let next_offset = (end < total).then_some(end);
        (page, total, next_offset)
    }
}

/// `verify_open`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct OpenedSessionResult {
    pub session: String,
    pub revision: u64,
    pub form_jcr_path: String,
    pub form_name: String,
    pub panel: Value,
    pub panels: Vec<PanelInfo>,
    pub has_next: bool,
    pub is_terminal: bool,
    pub findings: Vec<Finding>,
}

/// `verify_controls`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct ControlsResult {
    pub revision: u64,
    pub panel: Value,
    pub panels: Vec<PanelInfo>,
    pub has_next: bool,
    pub is_terminal: bool,
    pub controls: Vec<Control>,
    /// How many controls match the query's kinds, across every page.
    pub total: usize,
    pub offset: usize,
    /// Where the next page starts; absent on the last page.
    pub next_offset: Option<usize>,
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

/// The JS function `isContainer(node)`: whether a walk descends into
/// `node.items` instead of listing `node` as a control. Decided by class,
/// not by "has `.items`": a checkbox or dropdown can expose its option
/// entries as `.items` (confirmed live on `BBDZ_019`: `guideCheckBox` and
/// `guideDropDownList` nodes with `.items`), and treating those as
/// containers dropped the field itself from the inventory.
macro_rules! is_container_js {
    () => {
        "function isContainer(node) { return !!node && !!node.items && node.items.length > 0 \
         && /panel|toolbar|table|row/i.test(node.className || ''); }"
    };
}

/// The JS function `isInput(node)`: whether a walk lists `node` as a
/// control. Static text (`guideTextDraw`), footnote placeholders, images
/// and separators are not inputs, and a panel is never one even when it
/// has no children (confirmed live: `BBEU_019` listed 587 static texts as
/// controls, `AADD_019` eleven empty panels).
macro_rules! is_input_js {
    () => {
        "function isInput(node) { return !!node && !!node.name && !!node.somExpression \
         && !/panel|toolbar|table|row|textdraw|footnoteplaceholder|image|separator/i\
         .test(node.className || ''); }"
    };
}

/// The JS `crate::interactive` evaluates against the page -- kept apart
/// from `crate::flow::wizard_js` since this vocabulary (an inventory of
/// every control, a snapshot for diffing) is specific to the interactive
/// tools; `verify_run` never needs it.
mod control_js {
    /// Every leaf field on the form: the root panel ([`crate::root_panel`]),
    /// then `.items` recursively. A repeated panel's instances are siblings
    /// in their parent's `.items` (confirmed live on `AF_AABF`: after an
    /// add, `PN_CPGRP[0]` and `PN_CPGRP[1]` sit side by side), so the walk
    /// reaches every instance without asking the instance manager. A leaf
    /// with no `.name` (a static text or image component, not an input) is
    /// skipped: it is not a control an agent could set.
    ///
    /// `field` is the leaf's SOM expression, the one reference every tool
    /// uses; `instance_index` is the index of the nearest enclosing
    /// repeatable panel (an instance manager whose `maxOccur` is not 1 --
    /// every panel has one, a plain panel with `minOccur = maxOccur = 1`),
    /// absent outside any. Evaluates to `{"error": "root_panel_not_found"}`
    /// when the form has no root panel, which [`super::parse_controls`]
    /// turns into a hard error rather than an empty inventory.
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
    pub const INVENTORY: &str = concat!(
        r#"JSON.stringify((function() {
        var root = "#,
        crate::root_panel!(),
        r#";
        if (!root) { return { error: 'root_panel_not_found' }; }
        var out = [];
        "#,
        is_container_js!(),
        is_input_js!(),
        r#"
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
        // A choice control's options: the runtime's `enums`/`enumNames`, or
        // else the authored `jsonModel.options` list of "value=label"
        // strings, which is all a single confirmation checkbox carries
        // (confirmed live on BBDZ_019: `enums` is null, `options` is
        // ["0=<div>I confirm ...</div>"], and checking it sets the value "0").
        // A label is authored rich text, so its tags are stripped.
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
            if (out.length) { return out; }
            var authored = (node.jsonModel && node.jsonModel.options) || [];
            for (var j = 0; j < authored.length; j++) {
                var entry = String(authored[j]);
                var cut = entry.indexOf('=');
                var value = cut < 0 ? entry : entry.slice(0, cut);
                var label = cut < 0 ? entry : entry.slice(cut + 1);
                out.push({ value: value, label: label.replace(/<[^>]*>/g, '').replace(/&nbsp;/g, ' ').trim() });
            }
            return out;
        }
        function repeatable(node) {
            var manager = node.instanceManager;
            return !!manager && Number(manager.maxOccur) !== 1;
        }
        function walk(items, panelSom, instanceIndex) {
            if (!items) { return; }
            for (var i = 0; i < items.length; i++) {
                var node = items[i];
                if (!node) { continue; }
                if (isContainer(node)) {
                    walk(
                        node.items,
                        node.somExpression || panelSom,
                        repeatable(node) ? node.instanceIndex : instanceIndex
                    );
                    continue;
                }
                if (!isInput(node)) { continue; }
                var el = domOf(node);
                out.push({
                    field: node.somExpression,
                    name: node.name,
                    instance_index: instanceIndex === undefined ? null : instanceIndex,
                    class_name: node.className || '',
                    label: node.title || null,
                    options: options(node),
                    raw_value: node.value === undefined ? null : node.value,
                    multi_select: node.multiSelect === true
                        || String((node.jsonModel || {}).multiSelect) === 'true',
                    model_visible: node.visible !== false,
                    visible: node.visible !== false && !!(el && el.offsetParent !== null),
                    enabled: node.enabled !== false,
                    required: !!node.mandatory,
                    pattern: (function(model) {
                        model = model || {};
                        if (model.validatePictureClause) { return String(model.validatePictureClause); }
                        if (String(model.displayIsSameAsValidate) === 'true' && model.displayPictureClause) {
                            return String(model.displayPictureClause);
                        }
                        return null;
                    })(node.jsonModel),
                    valid: typeof node.isValid === 'boolean' ? node.isValid : null,
                    error_message: node._errorText ? String(node._errorText).trim() || null : null,
                    click_source: (node.jsonModel && node.jsonModel.clickExp) || null,
                    widget_classes: ((el && el.querySelector && el.querySelector('button')) || {}).className || '',
                    panel: panelSom || null,
                    position: rectOf(el)
                });
            }
        }
        walk(root.items, root.somExpression || null, undefined);
        return out;
    })())"#
    );

    /// Every named leaf as `fields: {som: {value, visible}}`, keyed by SOM so
    /// two instances of a repeated panel never overwrite each other, plus
    /// every repeatable group as `instances: {manager som: {count, min,
    /// max}}` (`max` is -1 when unlimited; `minOccur`/`maxOccur` arrive as
    /// strings, confirmed live, hence `Number`) --
    /// cheaper than [`INVENTORY`] (no options, no position, no DOM
    /// `getBoundingClientRect`) since it exists only to be diffed
    /// before/after an interaction, by [`super::diff_snapshots`].
    pub const SNAPSHOT: &str = concat!(
        r#"JSON.stringify((function() {
        var root = "#,
        crate::root_panel!(),
        r#";
        var out = { fields: {}, instances: {} };
        if (!root) { return out; }
        "#,
        is_container_js!(),
        is_input_js!(),
        r#"
        function domOf(node) {
            return node && node.id ? document.getElementById(node.id) : null;
        }
        function group(node) {
            var manager = node.instanceManager;
            if (!manager || Number(manager.maxOccur) === 1 || !manager.somExpression) { return; }
            out.instances[manager.somExpression] = {
                count: Number(manager.instanceCount),
                min: Number(manager.minOccur),
                max: Number(manager.maxOccur)
            };
        }
        function walk(items) {
            if (!items) { return; }
            for (var i = 0; i < items.length; i++) {
                var node = items[i];
                if (!node) { continue; }
                group(node);
                if (isContainer(node)) { walk(node.items); continue; }
                if (!isInput(node)) { continue; }
                var el = domOf(node);
                out.fields[node.somExpression] = {
                    value: node.value === undefined ? null : node.value,
                    visible: node.visible !== false && !!(el && el.offsetParent !== null),
                    enabled: node.enabled !== false
                };
            }
        }
        walk(root.items);
        return out;
    })())"#
    );

    /// Every step of every navigable container on the form: the panel
    /// children of a panel whose layout is a wizard, tabs or accordion and
    /// not marked `nonNavigable` (confirmed live on `AF_AABF`: the root's
    /// layout is `.../layouts/panel/wizard`, and each of its steps lays its
    /// own children out `nonNavigable`). `active` comes from the
    /// container's own `navigationContext.currentItem`.
    pub const PANELS: &str = concat!(
        r#"JSON.stringify((function() {
        var root = "#,
        crate::root_panel!(),
        r#";
        if (!root) { return { error: 'root_panel_not_found' }; }
        var out = [];
        function layoutOf(node) {
            return (node.jsonModel && node.jsonModel.layout) || {};
        }
        function navigable(node) {
            var layout = layoutOf(node);
            return layout.nonNavigable !== true
                && /wizard|tab|accordion/i.test(layout['sling:resourceType'] || '');
        }
        function walk(node, depth) {
            var steps = navigable(node);
            var context = steps ? node.navigationContext : null;
            var current = context && context.currentItem;
            var layout = (layoutOf(node)['sling:resourceType'] || '').split('/').pop();
            var items = node.items || [];
            for (var i = 0; i < items.length; i++) {
                var child = items[i];
                if (!child || child.className !== 'guidePanel' || !child.somExpression) { continue; }
                if (steps) {
                    out.push({
                        som: child.somExpression,
                        name: child.name || '',
                        title: child.title || null,
                        parent: node.somExpression,
                        depth: depth,
                        layout: layout,
                        visible: child.visible !== false,
                        active: !!current && current.somExpression === child.somExpression
                    });
                }
                walk(child, steps ? depth + 1 : depth);
            }
        }
        walk(root, 0);
        return out;
    })())"#
    );

    /// Clicks the button whose model node is `som`, through the `<button>`
    /// inside its widget, which runs the authored click expression the way
    /// a visitor's click does (confirmed live on `AF_AABF`: an Add button
    /// clicked this way adds an instance of its repeated panel). Evaluates
    /// to whether a button was found and clicked.
    pub fn click(som: &str) -> String {
        format!(
            r#"(function(som) {{
        var node = guideBridge.resolveNode(som);
        var el = node && node.id ? document.getElementById(node.id) : null;
        if (!el) {{ return false; }}
        (el.querySelector('button') || el).click();
        return true;
    }})({som})"#,
            som = serde_json::Value::String(som.to_owned())
        )
    }

    /// The root panel's current step: the wizard's own
    /// `navigationContext.currentItem` when it has one (the state
    /// `crate::flow::wizard_js::PANEL_FINGERPRINT` reads), otherwise the
    /// first root child on screen. `null` fields when neither is known yet
    /// (a form still settling right after open).
    pub const CURRENT_PANEL: &str = concat!(
        r#"JSON.stringify((function() {
        var root = "#,
        crate::root_panel!(),
        r#";
        var items = (root && root.items) || [];
        var current = root && root.navigationContext && root.navigationContext.currentItem;
        for (var i = 0; i < items.length; i++) {
            var node = items[i];
            var el = node && node.id ? document.getElementById(node.id) : null;
            var shown = current ? node === current : !!(el && el.offsetParent !== null);
            if (node && shown) {
                return {
                    som: node.somExpression || null,
                    name: node.name || null,
                    title: node.title || null,
                    index: i
                };
            }
        }
        return { som: null, name: null, title: null, index: null };
    })())"#
    );
}

/// Parses [`control_js::INVENTORY`]'s JSON into [`Control`]s.
pub fn parse_controls(raw: &str) -> Result<Vec<Control>, InteractionError> {
    let parsed: Value = serde_json::from_str(raw).map_err(|err| {
        InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            format!("could not parse the control inventory: {err}"),
        ))
    })?;
    if parsed.get("error").and_then(Value::as_str) == Some("root_panel_not_found") {
        return Err(InteractionError::RootPanelNotFound);
    }
    let entries = parsed.as_array().ok_or_else(|| {
        InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            format!("the control inventory is not a list: {raw}"),
        ))
    })?;
    Ok(entries.iter().filter_map(parse_one_control).collect())
}

/// Finds the control `field` names: its SOM expression, or else a guide
/// node name that exactly one control carries. A name several controls
/// share (every instance of a repeated panel repeats its children's names)
/// is refused with their SOM expressions rather than guessed at. The one
/// lookup every tool that takes a `field` goes through. Pure.
pub fn resolve_field<'a>(controls: &'a [Control], field: &str) -> Result<&'a Control, InteractionError> {
    if let Some(control) = controls.iter().find(|control| control.field == field) {
        return Ok(control);
    }
    let named: Vec<&Control> = controls.iter().filter(|control| control.name == field).collect();
    match named.as_slice() {
        [only] => Ok(only),
        [] => Err(InteractionError::UnknownField {
            field: field.to_owned(),
        }),
        many => Err(InteractionError::AmbiguousField {
            field: field.to_owned(),
            matches: many.iter().map(|control| control.field.clone()).collect(),
        }),
    }
}

fn parse_one_control(entry: &Value) -> Option<Control> {
    let field = entry.get("field")?.as_str()?.to_owned();
    let class_name = entry
        .get("class_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let kind = ControlKind::from_class_name(&class_name);
    let name = entry.get("name").and_then(Value::as_str)?.to_owned();
    let instance_index = entry
        .get("instance_index")
        .and_then(Value::as_u64)
        .and_then(|index| u32::try_from(index).ok());
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

    let click = if kind == ControlKind::Button {
        click_effect(
            entry.get("click_source").and_then(Value::as_str),
            entry
                .get("widget_classes")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    } else {
        None
    };

    let valid = entry.get("valid").and_then(Value::as_bool);
    let pattern = entry
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned);
    let error_message = entry
        .get("error_message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_owned);

    Some(Control {
        field,
        name,
        instance_index,
        access: Access::from_enabled(enabled),
        pattern,
        valid,
        error_message,
        click,
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

/// How a multi-select checkbox joins its selected option values into one
/// model value. Confirmed live on `BBPT_019`'s `CB_Daily`: checking Monday
/// and Wednesday gives `"Monday,Wednesday"`, and assigning that string
/// checks exactly those boxes, while a `\n`-joined value (this crate's
/// earlier guess) checks none.
pub const MULTI_SELECT_SEPARATOR: char = ',';

/// A multi-select checkbox's `raw_value`, split on
/// [`MULTI_SELECT_SEPARATOR`] into a JSON array so a caller never has to
/// know the convention; [`validate_value`] reverses it when setting.
fn normalize_value(raw_value: &Value, multi_select: bool) -> Value {
    if !multi_select {
        return raw_value.clone();
    }
    match raw_value.as_str() {
        Some("") => Value::Array(Vec::new()),
        Some(s) => Value::Array(
            s.split(MULTI_SELECT_SEPARATOR)
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
/// should actually be given -- a checkbox's array is rejoined with
/// [`MULTI_SELECT_SEPARATOR`]
/// (the reverse of [`normalize_value`]) since that is what the widget/model
/// itself expects. A radio or dropdown value that is not one of the
/// control's own options is refused naming the real ones, never silently
/// coerced -- the same "a refusal is only useful if it names what would
/// have worked" rule `crate::flow`'s own errors follow.
pub fn validate_value(control: &Control, requested: &Value) -> Result<Value, InteractionError> {
    if control.access == Access::ReadOnly && control.kind != ControlKind::Button {
        return Err(InteractionError::ReadOnlyField {
            field: control.field.clone(),
        });
    }
    match control.kind {
        ControlKind::Button => Err(InteractionError::ButtonCannotBeSet {
            field: control.field.clone(),
        }),
        ControlKind::Signature => Err(InteractionError::SignatureCannotBeSet {
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
                Ok(Value::String(
                    requested_values.join(&MULTI_SELECT_SEPARATOR.to_string()),
                ))
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

/// One field's state in a [`Snapshot`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FieldState {
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub visible: bool,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// One repeated panel's instance count and limits in a [`Snapshot`].
/// `max` is -1 when unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct InstanceGroup {
    pub count: i64,
    pub min: i64,
    pub max: i64,
}

/// [`control_js::SNAPSHOT`]'s result: every named field by SOM, and every
/// repeatable group by its instance manager's SOM.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub fields: BTreeMap<String, FieldState>,
    #[serde(default)]
    pub instances: BTreeMap<String, InstanceGroup>,
}

/// Diffs two snapshots, excluding `set_field` (already reported as the
/// interaction's own `value`/`values_changed`) from `side_effects`. A
/// field present in one snapshot but not the other (a repeated-section
/// instance created or destroyed, say) is treated as appearing/
/// disappearing by its `visible` flag alone in the snapshot it *is*
/// present in. Pure.
pub fn diff_snapshots(
    before: &Snapshot,
    after: &Snapshot,
    set_field: Option<&str>,
) -> (Vec<FieldChange>, Vec<String>, Vec<String>) {
    let mut side_effects = Vec::new();
    let mut appeared = Vec::new();
    let mut disappeared = Vec::new();

    for (field, after_state) in &after.fields {
        match before.fields.get(field) {
            Some(before_state) => {
                if Some(field.as_str()) != set_field && before_state.value != after_state.value {
                    side_effects.push(FieldChange {
                        field: field.clone(),
                        from: before_state.value.clone(),
                        to: after_state.value.clone(),
                    });
                }
                if !before_state.visible && after_state.visible {
                    appeared.push(field.clone());
                } else if before_state.visible && !after_state.visible {
                    disappeared.push(field.clone());
                }
            }
            None if after_state.visible => appeared.push(field.clone()),
            None => {}
        }
    }
    for (field, before_state) in &before.fields {
        if !after.fields.contains_key(field) && before_state.visible {
            disappeared.push(field.clone());
        }
    }
    // Already ordered by field: both maps are `BTreeMap`s.
    (side_effects, appeared, disappeared)
}

/// Every control present in both snapshots whose access changed, the
/// counterpart of `xfa_set`'s `access_changes`. Pure.
pub fn access_changes(before: &Snapshot, after: &Snapshot) -> Vec<AccessChange> {
    after
        .fields
        .iter()
        .filter_map(|(field, after_state)| {
            let before_state = before.fields.get(field)?;
            (before_state.enabled != after_state.enabled).then(|| AccessChange {
                field: field.clone(),
                from: Access::from_enabled(before_state.enabled),
                to: Access::from_enabled(after_state.enabled),
            })
        })
        .collect()
}

/// Whether any repeated panel gained or lost an instance between the two
/// snapshots, including a group that appeared or vanished entirely. Pure.
pub fn instances_changed(before: &Snapshot, after: &Snapshot) -> bool {
    before.instances != after.instances
}

/// When an instance button changed nothing, which repeated panels around
/// it sit at an `minOccur`/`maxOccur` limit -- the likely reason, worded
/// for the caller. A group counts as around the button when the panel
/// holding its instance manager encloses the button (an Add button is the
/// group's sibling, a Remove button sits inside an instance). Pure.
pub fn instance_limit_warning(button: &str, after: &Snapshot) -> Option<String> {
    let at_limit: Vec<String> = after
        .instances
        .iter()
        .filter(|(manager, _)| {
            manager
                .rsplit_once('.')
                .is_some_and(|(container, _)| button.starts_with(&format!("{container}.")))
        })
        .filter_map(|(manager, group)| {
            if group.max >= 0 && group.count >= group.max {
                Some(format!(
                    "{manager} already has {} instance(s), the most its maxOccur allows",
                    group.count
                ))
            } else if group.count <= group.min {
                Some(format!(
                    "{manager} has {} instance(s), the fewest its minOccur allows",
                    group.count
                ))
            } else {
                None
            }
        })
        .collect();
    let reason = if at_limit.is_empty() {
        "no enclosing repeated panel is at a limit, so its script did something else"
            .to_owned()
    } else {
        at_limit.join("; ")
    };
    Some(format!(
        "{button} runs an instance script but no repeated panel changed its instance count: \
         {reason}"
    ))
}

async fn snapshot(page: &browser::PageHandle) -> Result<Snapshot, InteractionError> {
    let raw = page
        .evaluate_string(control_js::SNAPSHOT)
        .await
        .map_err(|err| {
            InteractionError::Verify(VerifyError::new(ErrorKind::FormNotFound, err.to_string()))
        })?;
    serde_json::from_str(&raw).map_err(|err| {
        InteractionError::Verify(VerifyError::new(
            ErrorKind::FormNotFound,
            format!("could not parse the form snapshot: {err}"),
        ))
    })
}

/// Polls until two consecutive snapshots agree, bounded by
/// [`CLICK_SETTLE_POLLS`]: a click's scripts (adding an instance, then
/// relabelling its buttons) finish on their own schedule, and nothing
/// signals when they are done.
async fn settled_snapshot(page: &browser::PageHandle) -> Result<Snapshot, InteractionError> {
    let mut previous = snapshot(page).await?;
    for _ in 0..CLICK_SETTLE_POLLS {
        page.wait(CLICK_SETTLE_INTERVAL).await;
        let current = snapshot(page).await?;
        if current == previous {
            return Ok(current);
        }
        previous = current;
    }
    Ok(previous)
}

const CLICK_SETTLE_POLLS: u32 = 10;
const CLICK_SETTLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// The [`Interaction`] every mutation reports, from the snapshots taken
/// around it: what changed, where the form now is, and what the browser
/// recorded. `set` is the field and value a `verify_set` wrote, which is
/// excluded from `side_effects` and reported as `values_changed` instead.
/// The one place an `Interaction` is assembled, so every tool reports the
/// same fields the same way.
async fn interaction_from(
    page: &browser::PageHandle,
    driver: &dyn FormDriver,
    kind: InteractionKind,
    before: &Snapshot,
    after: &Snapshot,
    set: Option<(&str, Value)>,
) -> Interaction {
    let set_field = set.as_ref().map(|(field, _)| *field);
    let (side_effects, appeared, disappeared) = diff_snapshots(before, after, set_field);
    let values_changed = set_field.is_some_and(|field| {
        before.fields.get(field).map(|state| &state.value)
            != after.fields.get(field).map(|state| &state.value)
    });
    let panel = current_panel(page).await;
    let nav =
        flow::wait_for_navigation_controls(page, &driver.has_next_js(), &driver.terminal_panel_js())
            .await;
    let (field, value) = match set {
        Some((field, value)) => (Some(field.to_owned()), value),
        None => (None, Value::Null),
    };
    Interaction {
        revision: 0, // stamped by `mutate_session` once the mutation commits
        kind,
        field,
        value,
        values_changed,
        side_effects,
        appeared,
        disappeared,
        access_changes: access_changes(before, after),
        instances_changed: instances_changed(before, after),
        warning: None,
        panel,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        console_errors: page.console_errors(),
        failed_requests: page.failed_requests(),
        dialogs: page.dialogs(),
    }
}

async fn read_panels(page: &browser::PageHandle) -> Result<Vec<PanelInfo>, InteractionError> {
    let raw = page.evaluate_string(control_js::PANELS).await.map_err(|err| {
        InteractionError::Verify(VerifyError::new(ErrorKind::FormNotFound, err.to_string()))
    })?;
    parse_panels(&raw)
}

async fn current_panel(page: &browser::PageHandle) -> Value {
    let raw = page
        .evaluate_string(control_js::CURRENT_PANEL)
        .await
        .unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}

fn unknown_session(handle: &str) -> InteractionError {
    InteractionError::UnknownSession {
        handle: handle.to_owned(),
    }
}

/// Runs `op` on `handle`'s session of `instance_id`'s instance, with the
/// instance locked for the whole call (this module's doc), and marks the
/// instance used when `op` succeeds. The one place every per-session call
/// finds its session, so none repeats the lookup.
async fn on_session<T>(
    pool: &InstancePool,
    instance_id: &str,
    handle: &str,
    op: impl AsyncFnOnce(&mut OpenSession, &Containers) -> Result<T, InteractionError>,
) -> Result<T, InteractionError> {
    let mut guard = pool.lock(instance_id).await;
    let state = guard.as_mut().ok_or_else(|| unknown_session(handle))?;
    let session = state
        .sessions
        .get_mut(handle)
        .ok_or_else(|| unknown_session(handle))?;
    let result = op(session, &state.containers).await;
    if result.is_ok() {
        state.touch();
    }
    result
}

/// A mutation's result, stamped with the revision the mutation produced.
trait Revised {
    fn set_revision(&mut self, revision: u64);
}

impl Revised for Interaction {
    fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }
}

impl Revised for SubmitResult {
    fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }
}

/// [`on_session`] for a mutation: a successful `op` advances the session's
/// revision by exactly one and stamps it on the result; a failed one leaves
/// the revision where it was.
async fn mutate_session<T: Revised>(
    pool: &InstancePool,
    instance_id: &str,
    handle: &str,
    op: impl AsyncFnOnce(&mut OpenSession, &Containers) -> Result<T, InteractionError>,
) -> Result<T, InteractionError> {
    on_session(pool, instance_id, handle, async |session, containers| {
        let mut result = op(&mut *session, containers).await?;
        session.revision += 1;
        result.set_revision(session.revision);
        Ok(result)
    })
    .await
}

/// Lets go of this caller's hold on `form_path`'s package and uninstalls
/// it if nobody else holds it any more (`crate::packages`).
async fn release_package(
    profile: &Profile,
    state: &mut InstanceState,
    form_path: &str,
    context: &str,
) {
    state.packages.release(form_path);
    flow::uninstall_unreferenced(profile, &state.containers, &mut state.packages, context).await;
}

/// Opens `package_bytes`' form in a new session (browser tab) on
/// `instance_id`'s instance, installing the package unless the identical
/// one is already open in another session there. Refused when the instance
/// is at [`MAX_SESSIONS_PER_INSTANCE`], when another session holds a
/// different package for the same form path, or when `guideBridge` never
/// appears (the same signal `verify_run`'s own `guide_bridge_not_detected`
/// finding reports, raised as a hard error here since there is nothing an
/// interactive caller could do with an unusable page).
pub async fn open(
    profile: &Profile,
    driver: &dyn FormDriver,
    pool: &InstancePool,
    instance_id: &str,
    package_bytes: Vec<u8>,
) -> Result<OpenedSessionResult, InteractionError> {
    let package = package_check::inspect(&package_bytes).map_err(|err| {
        InteractionError::Verify(VerifyError::new(ErrorKind::PackageInvalid, err.to_string()))
    })?;

    let docker = DockerLifecycle::connect().await.map_err(|err| {
        InteractionError::Verify(VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))
    })?;

    let (mut guard, _boot_findings) =
        crate::instance::ensure(pool, instance_id, &docker, profile).await?;
    let state = guard
        .as_mut()
        .expect("ensure always leaves Some on success");

    if state.sessions.len() >= MAX_SESSIONS_PER_INSTANCE {
        return Err(InteractionError::TooManySessions {
            open: state.sessions.keys().cloned().collect(),
        });
    }

    flow::uninstall_unreferenced(
        profile,
        &state.containers,
        &mut state.packages,
        "a previous call's leftover package",
    )
    .await;
    let form_path = package.summary.form_jcr_path.clone();
    flow::install_package(
        profile,
        &state.containers,
        &mut state.packages,
        &package,
        package_bytes,
    )
    .await?;

    let opened = match flow::open_live_form(profile, driver, &state.containers, package).await {
        Ok(opened) => opened,
        Err(err) => {
            release_package(profile, state, &form_path, "a form that failed to open").await;
            return Err(err.into());
        }
    };

    if !opened.bridge_ready {
        opened.form.close().await;
        release_package(
            profile,
            state,
            &form_path,
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
    let form_name = opened.form.package.summary.form_name.clone();
    let panel = current_panel(&opened.form.page).await;
    let panels = match read_panels(&opened.form.page).await {
        Ok(panels) => panels,
        Err(err) => {
            opened.form.close().await;
            release_package(profile, state, &form_path, "a form whose panels could not be read")
                .await;
            return Err(err);
        }
    };
    let nav = flow::wait_for_navigation_controls(
        &opened.form.page,
        &driver.has_next_js(),
        &driver.terminal_panel_js(),
    )
    .await;

    let handle = format!("sess_{}", uuid::Uuid::new_v4().simple());
    state.sessions.insert(
        handle.clone(),
        OpenSession {
            handle: handle.clone(),
            revision: 0,
            live: opened.form,
            opened_at: Instant::now(),
        },
    );
    state.touch();

    Ok(OpenedSessionResult {
        session: handle,
        revision: 0,
        form_jcr_path: form_path,
        form_name,
        panel,
        panels,
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
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    revision: u64,
    query: &ControlsQuery,
) -> Result<ControlsResult, InteractionError> {
    on_session(pool, instance_id, session, async |open, _| {
        read_controls(open, driver, revision, query).await
    })
    .await
}

/// One control, by `field` ([`resolve_field`]) -- the counterpart of
/// `xfa_field`. A read: `revision` must match, and it never advances.
pub async fn field(
    pool: &InstancePool,
    instance_id: &str,
    session: &str,
    revision: u64,
    field: &str,
) -> Result<Control, InteractionError> {
    on_session(pool, instance_id, session, async |open, _| {
        check_revision(&open.handle, open.revision, revision)?;
        let raw = open
            .live
            .page
            .evaluate_string(control_js::INVENTORY)
            .await
            .unwrap_or_default();
        let controls = parse_controls(&raw)?;
        resolve_field(&controls, field).cloned()
    })
    .await
}

async fn read_controls(
    open_form: &OpenSession,
    driver: &dyn FormDriver,
    revision: u64,
    query: &ControlsQuery,
) -> Result<ControlsResult, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, revision)?;
    let page = &open_form.live.page;
    let raw = page.evaluate_string(control_js::INVENTORY).await.unwrap_or_default();
    let (page_of_controls, total, next_offset) = query.window(parse_controls(&raw)?);
    let panel = current_panel(page).await;
    let panels = read_panels(page).await?;
    let nav =
        flow::wait_for_navigation_controls(page, &driver.has_next_js(), &driver.terminal_panel_js())
            .await;
    Ok(ControlsResult {
        revision: open_form.revision,
        panel,
        panels,
        has_next: nav.has_next,
        is_terminal: nav.is_terminal,
        controls: page_of_controls,
        total,
        offset: query.offset,
        next_offset,
    })
}

/// Sets one control by `field` the way a person would
/// ([`crate::flow::set_control`], the same setter `verify_run`'s own
/// `fill` uses), refusing a `field` this form does not have or a value
/// that is not one of a choice control's own options before ever touching
/// the page.
pub async fn set(
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
    field: &str,
    value: &Value,
) -> Result<Interaction, InteractionError> {
    mutate_session(pool, instance_id, session, async |open, _| {
        set_inner(open, driver, expected_revision, field, value).await
    })
    .await
}

async fn set_inner(
    open_form: &OpenSession,
    driver: &dyn FormDriver,
    expected_revision: u64,
    field: &str,
    value: &Value,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;

    let raw = page.evaluate_string(control_js::INVENTORY).await.unwrap_or_default();
    let controls = parse_controls(&raw)?;
    let control = resolve_field(&controls, field)?;
    let normalized = validate_value(control, value)?;
    let field = control.field.as_str();

    let before = snapshot(page).await?;
    let report = flow::set_control(page, flow::FieldRef::Som(field), &normalized).await;
    if !report.set {
        return Err(InteractionError::SetFailed {
            field: field.to_owned(),
        });
    }
    let after = snapshot(page).await?;
    let mut interaction = interaction_from(
        page,
        driver,
        InteractionKind::Set,
        &before,
        &after,
        Some((field, normalized)),
    )
    .await;
    interaction.warning = report.view_error.map(|error| {
        format!(
            "{field} holds the value in the form's model, which validation and submit read, but \
             the form's runtime raised `{error}` while updating its widget, so the page may still \
             show the old value"
        )
    });
    Ok(interaction)
}

/// Clicks `click_js` (the driver's own next- or previous-panel control)
/// and confirms the panel actually changed
/// ([`crate::flow::advance_panel`], the same poll `verify_run`'s own
/// wizard walk uses for "next"), then reports the same [`Interaction`]
/// shape [`set`] does with `field`/`value` both `None`.
async fn advance(
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
    click_js: &str,
    direction: flow::NavDirection,
) -> Result<Interaction, InteractionError> {
    mutate_session(pool, instance_id, session, async |open, _| {
        advance_inner(
            open,
            driver,
            expected_revision,
            click_js,
            flow::wizard_js::PANEL_FINGERPRINT,
            direction,
        )
        .await
    })
    .await
}

async fn advance_inner(
    open_form: &OpenSession,
    driver: &dyn FormDriver,
    expected_revision: u64,
    click_js: &str,
    fingerprint_js: &str,
    direction: flow::NavDirection,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;

    let before = snapshot(page).await?;
    if let Some(message) = flow::advance_panel(page, click_js, fingerprint_js)
        .await
        .failure_message("this panel", direction)
    {
        return Err(InteractionError::PanelDidNotAdvance(message));
    }
    let after = snapshot(page).await?;
    let kind = match direction {
        flow::NavDirection::Next => InteractionKind::Next,
        flow::NavDirection::Prev => InteractionKind::Prev,
        flow::NavDirection::Goto => InteractionKind::Goto,
    };
    Ok(interaction_from(page, driver, kind, &before, &after, None).await)
}

/// Advances to the next panel via the driver's own `click_next_js`.
pub async fn next(
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    let click_js = driver.click_next_js();
    advance(
        pool,
        driver,
        instance_id,
        session,
        expected_revision,
        &click_js,
        flow::NavDirection::Next,
    )
    .await
}

/// Goes back to the previous panel via the driver's own `click_prev_js`.
pub async fn prev(
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    let click_js = driver.click_prev_js();
    advance(
        pool,
        driver,
        instance_id,
        session,
        expected_revision,
        &click_js,
        flow::NavDirection::Prev,
    )
    .await
}

/// Presses one button the way a visitor would ([`control_js::click`]),
/// runs whatever its click expression does -- adding or removing an
/// instance of a repeated panel, revealing a section, recomputing a value
/// -- and reports the result like [`set`] does. Refused for anything that
/// is not a button, for a button a visitor could not press right now
/// (hidden or disabled), and for a submit button, which belongs to
/// `verify_submit` so the result is captured.
pub async fn click(
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
    field: &str,
) -> Result<Interaction, InteractionError> {
    mutate_session(pool, instance_id, session, async |open, _| {
        click_inner(open, driver, expected_revision, field).await
    })
    .await
}

async fn click_inner(
    open_form: &OpenSession,
    driver: &dyn FormDriver,
    expected_revision: u64,
    field: &str,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;

    let raw = page.evaluate_string(control_js::INVENTORY).await.unwrap_or_default();
    let controls = parse_controls(&raw)?;
    let control = resolve_field(&controls, field)?;
    let field = control.field.as_str();
    if control.kind != ControlKind::Button {
        return Err(InteractionError::NotAButton {
            field: field.to_owned(),
        });
    }
    if control.click == Some(ClickEffect::Submit) {
        return Err(InteractionError::SubmitThroughClick {
            field: field.to_owned(),
        });
    }
    if !(control.visible && control.enabled) {
        return Err(InteractionError::NotClickable {
            field: field.to_owned(),
        });
    }

    let before = snapshot(page).await?;
    if !page.evaluate_bool(&control_js::click(field)).await.unwrap_or(false) {
        return Err(InteractionError::ClickFailed {
            field: field.to_owned(),
        });
    }
    let after = settled_snapshot(page).await?;
    let mut interaction =
        interaction_from(page, driver, InteractionKind::Click, &before, &after, None).await;
    interaction.field = Some(field.to_owned());
    if control.click == Some(ClickEffect::Instances) && !interaction.instances_changed {
        interaction.warning = instance_limit_warning(field, &after);
    }
    Ok(interaction)
}

/// Jumps straight to one navigable panel (`panel` is its SOM from
/// `panels`, or a name only one panel carries), confirming the visible
/// panel changed the same way [`next`] does. Going to the panel already
/// shown changes nothing and still reports, so a caller can use it to
/// make sure where the form stands. Refused for a panel that is not
/// visible. A forward jump does not run the step validation
/// `verify_next` does; `verify_submit` still validates everything.
pub async fn goto(
    pool: &InstancePool,
    driver: &dyn FormDriver,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
    panel: &str,
) -> Result<Interaction, InteractionError> {
    mutate_session(pool, instance_id, session, async |open, _| {
        goto_inner(open, driver, expected_revision, panel).await
    })
    .await
}

async fn goto_inner(
    open_form: &OpenSession,
    driver: &dyn FormDriver,
    expected_revision: u64,
    panel: &str,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;
    let page = &open_form.live.page;
    let panels = read_panels(page).await?;
    let target = resolve_panel(&panels, panel)?;
    if !target.visible {
        return Err(InteractionError::PanelNotVisible {
            panel: target.som.clone(),
        });
    }
    if target.active {
        let now = snapshot(page).await?;
        return Ok(interaction_from(page, driver, InteractionKind::Goto, &now, &now, None).await);
    }
    advance_inner(
        open_form,
        driver,
        expected_revision,
        &flow::wizard_js::goto_panel(&target.som),
        &flow::wizard_js::container_fingerprint(&target.parent),
        flow::NavDirection::Goto,
    )
    .await
}

/// Reopens the same form URL fresh -- cheaper than closing and opening
/// again (the browser session and installed package are both kept), and
/// the whole point of "reset" is to get back to how the form looked when
/// it was first opened.
pub async fn reset(
    profile: &Profile,
    driver: &dyn FormDriver,
    pool: &InstancePool,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    mutate_session(pool, instance_id, session, async |open, _| {
        reset_inner(profile, driver, open, expected_revision).await
    })
    .await
}

async fn reset_inner(
    profile: &Profile,
    driver: &dyn FormDriver,
    open_form: &mut OpenSession,
    expected_revision: u64,
) -> Result<Interaction, InteractionError> {
    check_revision(&open_form.handle, open_form.revision, expected_revision)?;

    let before = snapshot(&open_form.live.page).await?;

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
    let page = &open_form.live.page;
    let after = snapshot(page).await?;
    Ok(interaction_from(page, driver, InteractionKind::Reset, &before, &after, None).await)
}

/// Screenshots the whole page, or -- when `field` is given -- just that
/// control's own on-screen rectangle (from the inventory's own
/// `position`, refused when the control has none: nothing is visible to
/// crop). A read, like [`controls`]: `revision` must match exactly but
/// this call never advances it.
pub async fn screenshot(
    pool: &InstancePool,
    instance_id: &str,
    session: &str,
    revision: u64,
    format: browser::ImageFormat,
    field: Option<&str>,
) -> Result<ScreenshotResult, InteractionError> {
    on_session(pool, instance_id, session, async |open, _| {
        screenshot_inner(open, revision, format, field).await
    })
    .await
}

async fn screenshot_inner(
    open_form: &OpenSession,
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
            let control = resolve_field(&controls, name)?;
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
    pool: &InstancePool,
    driver: &dyn FormDriver,
    profile: &Profile,
    blobs: &BlobStore,
    instance_id: &str,
    session: &str,
    expected_revision: u64,
) -> Result<SubmitResult, InteractionError> {
    mutate_session(pool, instance_id, session, async |open, containers| {
        submit_inner(open, driver, profile, blobs, expected_revision, containers).await
    })
    .await
}

async fn submit_inner(
    open_form: &OpenSession,
    driver: &dyn FormDriver,
    profile: &Profile,
    blobs: &BlobStore,
    expected_revision: u64,
    containers: &Containers,
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
    let errors = flow::validate(page, None)
        .await
        .map_err(InteractionError::SubmitValidationUnreadable)?;
    if !errors.is_empty() {
        return Err(InteractionError::SubmitBlocked { errors });
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
        containers,
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

/// Closes `session`'s browser tab and lets go of its package, which is
/// uninstalled once no other session on the instance shows it.
pub async fn close(
    pool: &InstancePool,
    profile: &Profile,
    instance_id: &str,
    session: &str,
) -> Result<(), InteractionError> {
    let mut guard = pool.lock(instance_id).await;
    let state = guard.as_mut().ok_or_else(|| unknown_session(session))?;
    let closed = state
        .sessions
        .remove(session)
        .ok_or_else(|| unknown_session(session))?;
    let form_path = closed.live.package.summary.form_jcr_path.clone();
    closed.live.close().await;
    release_package(profile, state, &form_path, "a closed session's package").await;
    state.touch();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn control(kind_class: &str, options: &[(&str, &str)], multi_select: bool) -> Control {
        Control {
            field: "guide[0].guide1[0].F1[0]".to_owned(),
            name: "F1".to_owned(),
            instance_index: None,
            access: Access::Open,
            pattern: None,
            valid: None,
            error_message: None,
            click: None,
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
                "field": "guide[0].guide1[0].RB1[0]", "name": "RB1", "instance_index": null,
                "class_name": "guideRadioButton",
                "label": "Choice", "options": [{"value":"1","label":"One"},{"value":"2","label":"Two"}],
                "raw_value": "1", "multi_select": false, "model_visible": true, "visible": true,
                "enabled": true, "required": true, "panel": "guide[0].guide1[0].panel1",
                "position": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}
            },
            {
                "field": "guide[0].guide1[0].Row[1].CB1[0]", "name": "CB1", "instance_index": 1,
                "class_name": "guideCheckBox", "label": null,
                "options": [{"value":"a","label":"A"},{"value":"b","label":"B"}],
                "raw_value": "a,b", "multi_select": true, "model_visible": true, "visible": false,
                "enabled": true, "required": false, "panel": null, "position": null
            },
            { "class_name": "guideStaticText" }
        ]"#;
        let controls = parse_controls(raw).expect("parses");
        assert_eq!(controls.len(), 2);

        let rb1 = controls.iter().find(|c| c.name == "RB1").unwrap();
        assert_eq!(rb1.field, "guide[0].guide1[0].RB1[0]");
        assert_eq!(rb1.instance_index, None);
        assert_eq!(rb1.kind, ControlKind::Radio);
        assert_eq!(rb1.value, Value::String("1".to_owned()));
        assert_eq!(rb1.position, Some(ControlPosition { x: 1.0, y: 2.0, width: 3.0, height: 4.0 }));
        assert!(rb1.required);

        let cb1 = controls.iter().find(|c| c.name == "CB1").unwrap();
        assert_eq!(cb1.instance_index, Some(1));
        assert_eq!(cb1.kind, ControlKind::Checkbox);
        assert_eq!(
            cb1.value,
            Value::Array(vec![Value::String("a".to_owned()), Value::String("b".to_owned())])
        );
        assert_eq!(cb1.raw_value, Value::String("a,b".to_owned()));
        assert!(!cb1.visible);
        assert_eq!(cb1.position, None);

        // A leaf with no "field" or "name" key at all (the JS side never emits one
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
    fn validate_value_rejoins_a_multi_select_checkbox_array_with_commas() {
        let checkbox = control("guideCheckBox", &[("a", "A"), ("b", "B")], true);
        let value = validate_value(
            &checkbox,
            &Value::Array(vec![Value::String("a".to_owned()), Value::String("b".to_owned())]),
        )
        .unwrap();
        assert_eq!(value, Value::String("a,b".to_owned()));
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

    fn snap(pairs: &[(&str, Value, bool)]) -> Snapshot {
        let mut map = serde_json::Map::new();
        for (field, value, visible) in pairs {
            map.insert(
                (*field).to_owned(),
                json!({ "value": value, "visible": visible }),
            );
        }
        from_fields(&Value::Object(map))
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

    fn named(field: &str, name: &str) -> Control {
        Control {
            field: field.to_owned(),
            name: name.to_owned(),
            ..control("guideTextBox", &[], false)
        }
    }

    /// Every tool resolves `field` the same way: an exact SOM wins, a name
    /// only one control carries is accepted, and a name several instances
    /// share is refused with their SOMs instead of picking the first.
    #[test]
    fn resolve_field_prefers_a_som_and_refuses_an_ambiguous_name() {
        let controls = vec![
            named("guide[0].p[0].Row[0].Amount[0]", "Amount"),
            named("guide[0].p[0].Row[1].Amount[0]", "Amount"),
            named("guide[0].p[0].Total[0]", "Total"),
        ];
        assert_eq!(
            resolve_field(&controls, "guide[0].p[0].Row[1].Amount[0]").unwrap().field,
            "guide[0].p[0].Row[1].Amount[0]"
        );
        assert_eq!(resolve_field(&controls, "Total").unwrap().field, "guide[0].p[0].Total[0]");
        match resolve_field(&controls, "Amount") {
            Err(InteractionError::AmbiguousField { matches, .. }) => assert_eq!(
                matches,
                ["guide[0].p[0].Row[0].Amount[0]", "guide[0].p[0].Row[1].Amount[0]"]
            ),
            other => panic!("expected AmbiguousField, got {other:?}"),
        }
        assert!(matches!(
            resolve_field(&controls, "Nope"),
            Err(InteractionError::UnknownField { .. })
        ));
    }

    #[test]
    fn a_form_without_a_root_panel_is_an_error_not_an_empty_inventory() {
        assert!(matches!(
            parse_controls(r#"{"error":"root_panel_not_found"}"#),
            Err(InteractionError::RootPanelNotFound)
        ));
    }

    /// Two instances of a repeated panel carry the same field names; keyed
    /// by SOM, a change in one is reported against that instance only.
    #[test]
    fn diff_snapshots_keeps_same_named_instances_apart() {
        let before = from_fields(&json!({
            "p[0].Row[0].Amount[0]": {"value": "1", "visible": true},
            "p[0].Row[1].Amount[0]": {"value": "2", "visible": true}
        }));
        let after = from_fields(&json!({
            "p[0].Row[0].Amount[0]": {"value": "1", "visible": true},
            "p[0].Row[1].Amount[0]": {"value": "5", "visible": true}
        }));
        let (side_effects, appeared, disappeared) = diff_snapshots(&before, &after, None);
        assert_eq!(side_effects.len(), 1);
        assert_eq!(side_effects[0].field, "p[0].Row[1].Amount[0]");
        assert!(appeared.is_empty() && disappeared.is_empty());
    }

    #[test]
    fn every_inventory_script_finds_the_root_through_one_expression() {
        for js in [control_js::INVENTORY, control_js::SNAPSHOT, control_js::CURRENT_PANEL] {
            assert!(js.contains(crate::root_panel!()), "{js}");
            assert!(!js.contains("guideRootPanel[0]"), "no hard-coded root SOM: {js}");
        }
    }

    /// A test snapshot from a plain `{som: {value, visible}}` map.
    fn from_fields(fields: &Value) -> Snapshot {
        Snapshot {
            fields: serde_json::from_value(fields.clone()).expect("a field map"),
            instances: BTreeMap::new(),
        }
    }

    fn groups(entries: &[(&str, i64, i64, i64)]) -> Snapshot {
        Snapshot {
            fields: BTreeMap::new(),
            instances: entries
                .iter()
                .map(|(manager, count, min, max)| {
                    (
                        (*manager).to_owned(),
                        InstanceGroup {
                            count: *count,
                            min: *min,
                            max: *max,
                        },
                    )
                })
                .collect(),
        }
    }

    /// Navigation is recognised by the widget's class even when its script
    /// does bookkeeping too; instance scripts are recognised by their calls,
    /// including a platform helper such as `app.addInstance(...)`.
    #[test]
    fn click_effect_classifies_buttons_by_class_then_script() {
        assert_eq!(
            click_effect(Some("summary.setSummaryData(); nav.nextStep(this);"), "button-default moveNext"),
            Some(ClickEffect::Navigation)
        );
        assert_eq!(
            click_effect(Some("window.app.addInstance(this.parent.Row);"), "Button"),
            Some(ClickEffect::Instances)
        );
        assert_eq!(
            click_effect(Some("Row.instanceManager.removeInstance(0)"), ""),
            Some(ClickEffect::Instances)
        );
        assert_eq!(
            click_effect(Some("window.app.navigation.submit(msg);"), "Button"),
            Some(ClickEffect::Submit)
        );
        assert_eq!(click_effect(Some("Total.value = 1;"), ""), Some(ClickEffect::Script));
        assert_eq!(click_effect(Some("   "), ""), None);
        assert_eq!(click_effect(None, ""), None);
    }

    #[test]
    fn only_buttons_carry_a_click_effect() {
        let raw = r#"[
            {"field": "g[0].Add[0]", "name": "Add", "class_name": "guideButton",
             "click_source": "this.parent.Row.instanceManager.addInstance()", "widget_classes": "Button"},
            {"field": "g[0].T[0]", "name": "T", "class_name": "guideTextBox",
             "click_source": "addInstance()", "widget_classes": ""}
        ]"#;
        let controls = parse_controls(raw).unwrap();
        assert_eq!(controls[0].click, Some(ClickEffect::Instances));
        assert_eq!(controls[1].click, None);
    }

    #[test]
    fn instances_change_when_any_group_count_moves() {
        let before = groups(&[("p[0]._Row[0]", 1, 1, 4)]);
        assert!(!instances_changed(&before, &before.clone()));
        assert!(instances_changed(&before, &groups(&[("p[0]._Row[0]", 2, 1, 4)])));
    }

    /// An Add button beside a full group, and a Remove button inside an
    /// instance of a group at its minimum, both name the limit; a group
    /// elsewhere on the form is not blamed.
    #[test]
    fn the_limit_warning_names_the_enclosing_group_at_its_limit() {
        let after = groups(&[("g[0].p[0]._Row[0]", 4, 1, 4), ("g[0].q[0]._Other[0]", 4, 1, 4)]);
        let add = instance_limit_warning("g[0].p[0].Add[0]", &after).unwrap();
        assert!(add.contains("g[0].p[0]._Row[0] already has 4 instance(s), the most its maxOccur allows"), "{add}");
        assert!(!add.contains("_Other"), "{add}");

        let at_min = groups(&[("g[0].p[0]._Row[0]", 1, 1, 4)]);
        let remove = instance_limit_warning("g[0].p[0].Row[0].Remove[0]", &at_min).unwrap();
        assert!(remove.contains("the fewest its minOccur allows"), "{remove}");

        let unlimited = groups(&[("g[0].p[0]._Row[0]", 9, 0, -1)]);
        let neither = instance_limit_warning("g[0].p[0].Add[0]", &unlimited).unwrap();
        assert!(neither.contains("no enclosing repeated panel is at a limit"), "{neither}");
    }

    #[test]
    fn the_click_js_presses_the_button_inside_the_widget_and_escapes_the_som() {
        let js = control_js::click("g[0].x\"); alert(1); //[0]");
        assert!(js.contains("el.querySelector('button') || el).click()"), "{js}");
        assert!(js.ends_with(r#"})("g[0].x\"); alert(1); //[0]")"#), "{js}");
    }

    #[test]
    fn the_inventory_reads_each_controls_validation_state() {
        let raw = r#"[
            {"field": "g[0].A[0]", "name": "A", "class_name": "guideTextBox",
             "valid": false, "error_message": "   This field is required.\r\n"},
            {"field": "g[0].B[0]", "name": "B", "class_name": "guideTextBox",
             "valid": true, "error_message": null, "pattern": "^([^0-9]*)$"},
            {"field": "g[0].C[0]", "name": "C", "class_name": "guideTextBox"}
        ]"#;
        let controls = parse_controls(raw).unwrap();
        assert_eq!(controls[0].valid, Some(false));
        assert_eq!(controls[0].error_message.as_deref(), Some("This field is required."));
        assert_eq!(controls[1].valid, Some(true));
        assert_eq!(controls[1].error_message, None);
        assert_eq!(controls[1].pattern.as_deref(), Some("^([^0-9]*)$"));
        assert_eq!(controls[0].pattern, None);
        assert_eq!(controls[2].valid, None);
    }

    #[test]
    fn a_blocked_submit_names_every_failing_field() {
        let message = InteractionError::SubmitBlocked {
            errors: vec![
                flow::ValidationError {
                    field: "g[0].A[0]".to_owned(),
                    message: "This field is required.".to_owned(),
                },
                flow::ValidationError {
                    field: "g[0].B[0]".to_owned(),
                    message: "Invalid".to_owned(),
                },
            ],
        }
        .to_string();
        assert!(message.contains("2 error(s): g[0].A[0]: This field is required.; g[0].B[0]: Invalid"), "{message}");
        assert!(message.contains("was not submitted"), "{message}");
    }

    fn panel(som: &str, name: &str, visible: bool, active: bool) -> PanelInfo {
        PanelInfo {
            som: som.to_owned(),
            name: name.to_owned(),
            title: None,
            parent: "g[0].root[0]".to_owned(),
            depth: 0,
            layout: "wizard".to_owned(),
            visible,
            active,
        }
    }

    #[test]
    fn the_panel_list_parses_and_a_missing_root_is_an_error() {
        let raw = r#"[{"som": "g[0].root[0].Step1[0]", "name": "Step1", "title": null,
            "parent": "g[0].root[0]", "depth": 0, "layout": "wizard", "visible": true, "active": true}]"#;
        assert_eq!(
            parse_panels(raw).unwrap(),
            [panel("g[0].root[0].Step1[0]", "Step1", true, true)]
        );
        assert!(matches!(
            parse_panels(r#"{"error":"root_panel_not_found"}"#),
            Err(InteractionError::RootPanelNotFound)
        ));
    }

    #[test]
    fn resolve_panel_follows_the_field_rules() {
        let panels = vec![
            panel("g[0].a[0].Tab[0]", "Tab", true, false),
            panel("g[0].b[0].Tab[0]", "Tab", true, false),
            panel("g[0].root[0].Costs[0]", "Costs", true, false),
        ];
        assert_eq!(resolve_panel(&panels, "g[0].b[0].Tab[0]").unwrap().som, "g[0].b[0].Tab[0]");
        assert_eq!(resolve_panel(&panels, "Costs").unwrap().som, "g[0].root[0].Costs[0]");
        assert!(matches!(
            resolve_panel(&panels, "Tab"),
            Err(InteractionError::AmbiguousPanel { .. })
        ));
        match resolve_panel(&panels, "Nope") {
            Err(InteractionError::UnknownPanel { known, .. }) => assert_eq!(known.len(), 3),
            other => panic!("expected UnknownPanel, got {other:?}"),
        }
    }

    #[test]
    fn the_panel_script_finds_the_root_through_the_shared_expression() {
        assert!(control_js::PANELS.contains(crate::root_panel!()));
        assert!(control_js::PANELS.contains("nonNavigable !== true"));
    }

    fn kinds(list: &[(&str, &str)]) -> Vec<Control> {
        list.iter()
            .map(|(field, class)| Control {
                field: (*field).to_owned(),
                kind: ControlKind::from_class_name(class),
                ..control(class, &[], false)
            })
            .collect()
    }

    #[test]
    fn a_controls_query_refuses_a_limit_outside_one_to_five_hundred() {
        assert!(ControlsQuery::new(None, Some(0), vec![]).is_err());
        assert!(ControlsQuery::new(None, Some(501), vec![]).is_err());
        assert!(ControlsQuery::new(None, Some(500), vec![]).is_ok());
        assert_eq!(
            ControlsQuery::new(None, None, vec![]).unwrap(),
            ControlsQuery::new(Some(0), Some(100), vec![]).unwrap()
        );
    }

    /// Paging walks the matches in order and says where the next page
    /// starts; a kinds filter narrows what `total` counts.
    #[test]
    fn a_controls_query_pages_and_filters_like_xfa_controls() {
        let all = kinds(&[("a", "guideTextBox"), ("b", "guideButton"), ("c", "guideTextBox"), ("d", "guideTextBox")]);
        let first = ControlsQuery::new(Some(0), Some(2), vec![]).unwrap().window(all.clone());
        assert_eq!(first.0.iter().map(|c| c.field.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((first.1, first.2), (4, Some(2)));
        let last = ControlsQuery::new(Some(2), Some(2), vec![]).unwrap().window(all.clone());
        assert_eq!((last.1, last.2), (4, None));

        let texts = ControlsQuery::new(Some(1), Some(5), vec![ControlKind::Text]).unwrap().window(all.clone());
        assert_eq!(texts.0.iter().map(|c| c.field.as_str()).collect::<Vec<_>>(), ["c", "d"]);
        assert_eq!((texts.1, texts.2), (3, None));

        let beyond = ControlsQuery::new(Some(10), None, vec![]).unwrap().window(all);
        assert!(beyond.0.is_empty());
        assert_eq!((beyond.1, beyond.2), (4, None));
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_name() {
        for kind in ControlKind::ALL {
            let wire = serde_json::to_value(kind).unwrap();
            assert_eq!(serde_json::from_value::<ControlKind>(wire).unwrap(), kind);
        }
        assert!(serde_json::from_value::<Vec<ControlKind>>(json!(["text", "slider"])).is_err());
    }

    #[test]
    fn access_changes_report_a_control_that_was_enabled_or_disabled() {
        let state = |enabled: bool| FieldState {
            value: Value::Null,
            visible: true,
            enabled,
        };
        let before = Snapshot {
            fields: [("a".to_owned(), state(true)), ("b".to_owned(), state(true))].into(),
            instances: BTreeMap::new(),
        };
        let after = Snapshot {
            fields: [("a".to_owned(), state(false)), ("b".to_owned(), state(true)), ("c".to_owned(), state(false))].into(),
            instances: BTreeMap::new(),
        };
        assert_eq!(
            access_changes(&before, &after),
            [AccessChange {
                field: "a".to_owned(),
                from: Access::Open,
                to: Access::ReadOnly
            }]
        );
        assert_eq!(serde_json::to_value(Access::ReadOnly).unwrap(), json!("readOnly"));
    }

    /// A single confirmation checkbox carries its option only in the
    /// authored `jsonModel.options`, so the inventory must read it there
    /// when the runtime's `enums` is empty.
    #[test]
    fn the_inventory_falls_back_to_authored_options() {
        let js = control_js::INVENTORY;
        let enums = js.find("node.enums").expect("runtime enums first");
        let authored = js.find("node.jsonModel.options").expect("authored options fallback");
        assert!(enums < authored, "{js}");
        assert!(js.contains("indexOf('=')"), "{js}");
    }

    /// A field that exposes its options as `.items` is still a field: both
    /// walks decide containers by class through one shared helper.
    #[test]
    fn walks_descend_only_into_containers_by_class() {
        for js in [control_js::INVENTORY, control_js::SNAPSHOT] {
            assert!(js.contains(is_container_js!()), "{js}");
            assert!(js.contains("if (isContainer(node))"), "{js}");
            assert!(!js.contains("if (node.items && node.items.length)"), "{js}");
        }
    }

    #[test]
    fn a_read_only_control_and_a_signature_cannot_be_set() {
        let mut locked = control("guideTextBox", &[], false);
        locked.access = Access::ReadOnly;
        assert!(matches!(
            validate_value(&locked, &json!("x")),
            Err(InteractionError::ReadOnlyField { .. })
        ));
        let signature = control("guideFieldNode guideScribble", &[], false);
        assert_eq!(signature.kind, ControlKind::Signature);
        assert!(matches!(
            validate_value(&signature, &json!("x")),
            Err(InteractionError::SignatureCannotBeSet { .. })
        ));
    }

    #[test]
    fn telephone_and_email_fields_are_text() {
        assert_eq!(ControlKind::from_class_name("guideFieldNode guideTelephone"), ControlKind::Text);
        assert_eq!(ControlKind::from_class_name("guideFieldNode guideEmail"), ControlKind::Text);
    }

    #[test]
    fn a_multi_select_value_round_trips_through_its_comma_form() {
        let checkbox = control("guideCheckBox", &[("Monday", "Monday"), ("Wednesday", "Wednesday")], true);
        let joined = validate_value(&checkbox, &json!(["Monday", "Wednesday"])).unwrap();
        assert_eq!(joined, json!("Monday,Wednesday"));
        assert_eq!(normalize_value(&joined, true), json!(["Monday", "Wednesday"]));
    }

    /// Static text and empty panels are not controls, in the inventory and
    /// in the snapshot alike.
    #[test]
    fn both_walks_list_only_inputs() {
        for js in [control_js::INVENTORY, control_js::SNAPSHOT] {
            assert!(js.contains(is_input_js!()), "{js}");
            assert!(js.contains("if (!isInput(node)) { continue; }"), "{js}");
        }
        assert!(control_js::INVENTORY.contains("(node.jsonModel || {}).multiSelect"));
    }
}
