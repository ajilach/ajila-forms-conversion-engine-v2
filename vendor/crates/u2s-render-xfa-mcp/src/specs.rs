//! Tool specifications and the `u2s.manifest` resource.
//!
//! The five core read tools mirror the `pdf_*` surface argument for
//! argument, so an agent that can drive one renderer can drive the other.
//! They are prefixed because both servers are registered at once — the PDF
//! server's `form_type` routes XFA documents here — and identical names
//! would collide in the tool list.
//!
//! `xfa_open`, `xfa_set`, `xfa_click`, `xfa_reset` and `xfa_close` are the interaction
//! surface: a session, not a state address. `xfa_controls` and every read
//! tool take either `doc` or `doc_path` (a one-shot, stateless address, with the
//! optional `state` shorthand for a specific point in the space) or
//! `session` plus `revision` (a frozen view of a live, interacted-with
//! form) — never both, never neither.
//!
//! Each tool's input schema is generated from its argument type, which is
//! also what its arguments parse into: the shared render arguments in
//! [`u2s_render_core::args`] next to [`XfaTarget`], or the session types
//! below. Tool descriptions stay here; argument descriptions are the doc
//! comments on those fields. Both are prompt surface.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use u2s_render_core::args::{
    InfoArgs, PageTextArgs, RenderPageArgs, RenderPagesArgs, RenderRegionArgs, SearchTextArgs,
    ServerArgs,
};
use u2s_render_xfa::states::{ControlKind, SelectionSpec, StateSpec, Step};
use u2s_schema::tool_input_schema;

// This is the shared MCP *convention*'s contract version (roles, ingest
// capabilities, blob-handle discipline, and now the additive `sessions`
// declaration) -- checked against `u2s_mcp::manifest::SUPPORTED_CONTRACT_MAJOR`
// at registration -- not a version for this server's own tool surface. A
// tool being removed and response shapes changing is a real breaking change
// to THIS server's API, but not to the convention itself: an old client that
// has never heard of `xfa_states` or the new response fields still parses
// this manifest and calls the tools that still exist exactly as before, so
// the major stays 1.
pub const CONTRACT_VERSION: &str = "1.0.0";

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "input_schema": input_schema })
}

/// A stored form, by blob handle or path: what `xfa_open` takes.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct XfaDocument {
    /// The form as the handle of a stored blob. Exactly one of doc, doc_path and session is required (xfa_open takes doc or doc_path).
    #[schemars(length(min = 1))]
    pub doc: Option<String>,
    /// Absolute path to the PDF on the machine running this server. Exactly one of doc, doc_path and session is required (xfa_open takes doc or doc_path).
    #[schemars(length(min = 1))]
    pub doc_path: Option<String>,
}

/// What a read tool addresses: a stored form in a requested state, or one
/// revision of a session. Which of the two is decided once, in
/// `XfaRenderServer::target_of`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct XfaTarget {
    #[serde(flatten)]
    pub document: XfaDocument,
    /// A session handle from xfa_open. Exactly one of doc, doc_path and session is required: pass this instead of doc or doc_path to address a live, interacted-with form rather than the stored document.
    #[schemars(length(min = 1))]
    pub session: Option<String>,
    /// Required alongside session: the revision you last saw. A stale or future revision is refused and the error names the current one.
    pub revision: Option<u64>,
    /// Which form state to use, when addressing by doc or doc_path. Leave out
    /// for the state the form opens in. To choose another, list the form's
    /// controls with xfa_controls and say what to do to them, in order, as
    /// `steps`: `{"set": {"field", "value"}}` sets a control, `{"click":
    /// {"field"}}` presses a button; each step is one or the other.
    /// Everything not touched keeps its default. Presses happen in the order
    /// given, so two presses of an add button make two new rows, and a later
    /// step can set a field of a row an earlier press created
    /// (`Row[1].Amount`). `selections`, a list of sets only, is the older
    /// spelling and still accepted; give exactly one of the two. Ignored when
    /// addressing by session, since a session's own interactions already
    /// determine its state.
    #[serde(default, deserialize_with = "state_arg")]
    pub state: Option<StateArg>,
}

/// Reads `state` with an error that says what is wrong with it. The derived
/// reading of an untagged enum only says that no variant matched, which
/// names neither the argument nor the keys it expects.
fn state_arg<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<StateArg>, D::Error> {
    use serde::de::Error as _;
    let Some(value) = Option::<Value>::deserialize(d)? else {
        return Ok(None);
    };
    StateArg::deserialize(&value)
        .map(Some)
        .map_err(|_| D::Error::custom(explain_state(&value)))
}

/// Why `value` is not a `state`.
fn explain_state(value: &Value) -> String {
    let Some(obj) = value.as_object() else {
        return "state must be an object of the shape { \"steps\": [...] }".to_owned();
    };
    match (obj.get("steps"), obj.get("selections")) {
        (Some(_), Some(_)) => {
            "state has both \"steps\" and \"selections\"; give one (selections are set steps)"
                .to_owned()
        }
        (None, None) => {
            let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            format!(
                "state must have a \"steps\" array (or the older \"selections\"); this object \
                 has keys {keys:?}"
            )
        }
        (Some(steps), None) => match Vec::<StepArg>::deserialize(steps) {
            Ok(_) => "state must have only \"steps\", with at least one step".to_owned(),
            Err(err) => format!(
                "state: each step is {{\"set\": {{field, value}}}} or {{\"click\": {{field}}}}: {err}"
            ),
        },
        (None, Some(selections)) => match Vec::<SelectionArg>::deserialize(selections) {
            Ok(_) => "state must have only \"selections\", with at least one".to_owned(),
            Err(err) => format!("state: each selection is {{field, value}}: {err}"),
        },
    }
}

/// An xfa read tool's arguments: [`XfaTarget`] next to the tool's own.
pub type XfaArgs<T> = ServerArgs<XfaTarget, T>;

/// The `state` argument as it arrives: exactly one of `steps` and
/// `selections`, each with at least one entry. The variants close
/// themselves, so an object with both, or with neither, matches neither
/// variant and is refused. [`StateArg::into_spec`] is its one reading.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum StateArg {
    Steps {
        #[schemars(length(min = 1))]
        steps: Vec<StepArg>,
    },
    Selections {
        #[schemars(length(min = 1))]
        selections: Vec<SelectionArg>,
    },
}

impl StateArg {
    pub fn into_spec(self) -> StateSpec {
        match self {
            StateArg::Steps { steps } => StateSpec {
                steps: steps.into_iter().map(StepArg::into_step).collect(),
            },
            StateArg::Selections { selections } => StateSpec::selections(
                selections
                    .into_iter()
                    .map(|s| SelectionSpec {
                        field: s.field,
                        value: s.value,
                    })
                    .collect(),
            ),
        }
    }
}

/// One step of a `state`: exactly one of `set` and `click`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StepArg {
    Set(SetStep),
    Click(ClickStep),
}

impl StepArg {
    fn into_step(self) -> Step {
        match self {
            StepArg::Set(set) => Step::Set(SelectionSpec {
                field: set.field,
                value: set.value,
            }),
            StepArg::Click(click) => Step::Click { field: click.field },
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStep {
    /// A field path, from xfa_controls.
    #[schemars(length(min = 1))]
    pub field: String,
    /// What to set it to.
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClickStep {
    /// A field path from xfa_controls; usually one of kind `button`, `radio` or `checkbox`.
    #[schemars(length(min = 1))]
    pub field: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectionArg {
    /// A control's field path, from xfa_controls.
    #[schemars(length(min = 1))]
    pub field: String,
    /// One of that control's options.
    pub value: String,
}

/// `xfa_controls`' own arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ControlsArgs {
    /// First field to return; default 0. Pass the previous next_offset.
    pub offset: Option<usize>,
    /// Fields to return; default 100.
    #[schemars(range(min = 1, max = 500))]
    pub limit: Option<usize>,
    /// Only fields of these kinds. Leave out for every kind.
    #[schemars(length(min = 1))]
    pub kinds: Option<Vec<ControlKind>>,
}

/// `xfa_field`'s own arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FieldArgs {
    /// A field path, from xfa_controls.
    pub field: String,
}

/// The session an interaction acts on, at the revision the caller last saw.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SessionRevision {
    /// A session handle from xfa_open. Exactly one of doc, doc_path and session is required: pass this instead of doc or doc_path to address a live, interacted-with form rather than the stored document.
    #[schemars(length(min = 1))]
    pub session: String,
    /// Required alongside session: the revision you last saw. A stale or future revision is refused and the error names the current one.
    pub expected_revision: u64,
}

/// `xfa_reset`'s arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ResetArgs {
    #[serde(flatten)]
    pub at: SessionRevision,
}

/// `xfa_set`'s arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SetArgs {
    #[serde(flatten)]
    pub at: SessionRevision,
    /// A field path, from xfa_controls.
    pub field: String,
    /// What to set it to — one of the control's options, or its own on-value for a radio button. A field with a `format` takes its canonical form (a date `YYYY-MM-DD`, a number `1234.5`) or the value as typed in that format (`05.01.2024` for `date{DD.MM.YYYY}`); either is stored canonically.
    pub value: String,
}

/// `xfa_click`'s arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ClickArgs {
    #[serde(flatten)]
    pub at: SessionRevision,
    /// A field path from xfa_controls; usually one of kind `button`, `radio` or `checkbox`.
    pub field: String,
}

/// `xfa_close`'s arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CloseArgs {
    /// A session handle from xfa_open. Exactly one of doc, doc_path and session is required: pass this instead of doc or doc_path to address a live, interacted-with form rather than the stored document.
    #[schemars(length(min = 1))]
    pub session: String,
}

fn read_schema<T: JsonSchema>() -> Value {
    tool_input_schema::<XfaArgs<T>>()
}

pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "xfa_open",
            "Open a form for interaction and get a session to work in. Everything after this \
             addresses `session` plus a `revision` instead of `doc`: the form is live, \
             and each interaction produces a new revision. Returns the same shape as \
             xfa_info, plus `session` and `revision` (always 0 on open). Call xfa_controls \
             next to see what can be set.",
            tool_input_schema::<ServerArgs<XfaDocument, InfoArgs>>(),
        ),
        tool(
            "xfa_set",
            "Set one control the way a person would: focus into it, change it, focus out. \
             The form's own scripts run in response, so this can reveal a hidden section, \
             recompute a total, or move content onto another page. Returns the new \
             `revision` plus what happened: which other fields changed as a side effect, \
             which controls appeared or disappeared, the new page count, and the fidelity \
             this interaction was reached at. Pass the `revision` you last saw as \
             `expected_revision`; a stale or future one is refused and the error names the \
             current one, so an agent can never act on a view that has moved. Field paths \
             are indexed inside repeated sections: `Row.Amount` is the first row, \
             `Row[1].Amount` the second, exactly as xfa_controls lists them. A button has \
             no value to set; press it with xfa_click instead. A field whose `access` is \
             not `open` (see xfa_controls) is refused, since a person filling the form \
             cannot change it either. The form's scripts can lock or unlock fields in \
             response to a change, and prefill them: `access_changes` lists every field \
             whose access changed, as `{field, from, to}`, and `side_effects` every value \
             the scripts wrote. `messages` lists every dialog the scripts raised (an \
             `app.alert` saying a field is mandatory, say), each as `{source, text, title, \
             answer}`: nobody can press its buttons, so it takes its default, OK (answer 1) \
             or Yes (answer 4). A script that fails is named in `warning`, since whatever \
             the rest of its handler would have done is missing, and so is a value the \
             field's own validation rejects (empty though mandatory, or refused by its \
             validate script); the value is kept, as the form keeps it.",
            tool_input_schema::<SetArgs>(),
        ),
        tool(
            "xfa_click",
            "Press one field the way a person would: the mouse goes down and up on it (its \
             mouseDown and mouseUp scripts run), then its click. Pressing a checkbox toggles \
             it; pressing a radio selects it, and pressing a selected radio again keeps it \
             selected unless the form's own mouseDown or mouseUp scripts deselect it. Pressing \
             any other open field runs its mouse and click scripts and changes no value. Use \
             xfa_set to set a radio or checkbox to a value without pressing it. The button's own click script runs, \
             so on a form with a repeatable section this is how rows are added or removed: \
             pressing an add button creates the next instance, and every field in it is \
             listed in `appeared` under an indexed path such as `form1.Body.Row[1].Amount` \
             (the first instance has no index) and in the next xfa_controls. Pressing a \
             remove button lists the removed instance's fields in `disappeared`; the \
             instances after it move down one index. A press the form refuses, because the \
             section is already at its minimum or maximum number of instances, leaves \
             `instances_changed` false and says so in `warning`. Returns the same shape as \
             xfa_set, `messages` and failing scripts included. Pass the `revision` you last saw as `expected_revision`; a stale or \
             future one is refused and the error names the current one. A button whose \
             `access` is not `open` is refused, since a person cannot press it either.",
            tool_input_schema::<ClickArgs>(),
        ),
        tool(
            "xfa_reset",
            "Put a session's form back the way it opened, keeping the session and its \
             handle. Cheaper than closing and opening again. Returns the same shape as \
             xfa_set, with the new revision and what changed relative to how the session \
             stood just before the reset.",
            tool_input_schema::<ResetArgs>(),
        ),
        tool(
            "xfa_close",
            "Release a session. Optional: an unused session is reclaimed on its own after \
             going idle for a while. Worth calling when done with a large form and the \
             memory should be freed sooner.",
            tool_input_schema::<CloseArgs>(),
        ),
        tool(
            "xfa_info",
            "Page count, per-page dimensions in points, detected language, and the XFA \
             packets present. Call this FIRST when addressing by doc: if it reports kind \
             'not_xfa' the document is an ordinary PDF and this server cannot render it — use \
             pdf_info and pdf_render_page instead. `warning` names any of the form's scripts \
             that failed while it loaded. Also serves the normalizer's ingest `info` \
             step, reporting `applicable: true` for every XFA form it can read.",
            read_schema::<InfoArgs>(),
        ),
        tool(
            "xfa_render_page",
            "Render one page of the form to an image. Returns the image inline when it is \
             small enough, otherwise a blob handle the caller dereferences out of band. \
             dpi defaults to 144 and is clamped to 36..600; the long edge is clamped to \
             max_edge_px (default 2576) and the effective dpi is reported back. Open text \
             and choice fields are underlined along their bottom edge as viewer chrome: the \
             line marks where a value can be typed and is not part of the form.",
            read_schema::<RenderPageArgs>(),
        ),
        tool(
            "xfa_render_pages",
            "Render several pages with an explicit cursor. Give either `pages` (an exact \
             list) or `from` plus `limit` (a window). The response stops at whichever cap \
             binds first — an image count and a total byte budget — and reports which in \
             `budget_hit`, with `next_from` naming where to continue. Walk `next_from` \
             until it is null rather than asking for a whole document at once.",
            read_schema::<RenderPagesArgs>(),
        ),
        tool(
            "xfa_render_region",
            "Render a rectangular region of one page at high resolution — the tool for \
             reading small print. The rect is in points with the origin at the page's \
             TOP-LEFT. dpi defaults to 300.",
            read_schema::<RenderRegionArgs>(),
        ),
        tool(
            "xfa_page_text",
            "Text of one page, windowed by character offset. Always reports total_chars \
             and truncated, so a caller can tell a partial read from a whole one. Note the \
             text is logical, not visual: line breaks happen at render time, so this is \
             what the page says rather than how it wraps.",
            read_schema::<PageTextArgs>(),
        ),
        tool(
            "xfa_search_text",
            "Find where something is said, across pages, without reading each one. Greps the \
             form's page text for a literal substring (case-insensitive) or a regex, and tags \
             every match with its page plus the offset and length of the match in that page's \
             text — pass those three straight to xfa_page_text to read the hit in context. \
             Scans from `from` onwards and stops at whichever cap binds first, a match count \
             and a page count, reporting which in `budget_hit` with `next_from` naming where \
             to continue; walk `next_from` until it is null. A page is always scanned whole, \
             so a walk never repeats a match. total_matches counts the pages THIS call \
             scanned, and truncated says the match limit dropped entries — narrow the query \
             rather than raising limit. A query that could match the empty string is refused, \
             since it would report one hit per character. This runs the full layout path, so \
             it is not free; the first call on a document pays for it and later ones reuse \
             it. To grep the raw XFA XML instead of the laid-out text, use the xfa_search \
             tool of the XFA data server.",
            read_schema::<SearchTextArgs>(),
        ),
        tool(
            "xfa_controls",
            "The fields on the form, a window at a time: radio buttons, checkboxes, dropdowns and \
             buttons, and the \
             free-value fields (`text`, `text_area`, `date`, `time`, `date_time`, `numeric`, \
             `password`, `signature`, `barcode`, `image`), each with its options (empty for a \
             free-value field, which takes any text), what it is set to, whether it is \
             currently visible, and whether the form's own scripts react to it \
             (`affects_layout`) — a form with many controls is worth triaging by that flag \
             first.\n\n\
             Each field also reports `access`, the XFA keyword for what a person filling the \
             form may do with it: `open` can be set or pressed; `readOnly`, `protected` and \
             `nonInteractive` cannot, and xfa_set and xfa_click refuse them. A locked field \
             is still listed, because the form's scripts can still write its value and can \
             unlock it in response to another field; watch `access_changes` in the xfa_set \
             result. `access` is effective: a field inside a locked subform or exclusion \
             group is locked too, and `access_from` then names that container.\n\n\
             A large form has hundreds of fields, so the listing is windowed: it returns at \
             most `limit` (default 100, at most 500) from `offset` (default 0), in field path \
             order, with `total` and `next_offset`; walk `next_offset` until it is null. \
             `kinds` narrows the listing to the kinds given, before windowing — \
             `[\"radio\", \"checkbox\", \"dropdown\", \"button\"]` for the choices that \
             shape the form. `space_size` always describes the whole form.\n\n\
             A button carries `click`: \
             `instances` when pressing it adds or removes rows of a repeatable section, \
             `script` when it does something else, absent when pressing it does nothing. \
             Buttons have no options and do not count towards `space_size`; press them with \
             xfa_click in a session, or with a `click` step in `state`. Addressed by `session` and `revision`, \
             this answers for the form AS IT STANDS after those interactions, which is how a \
             control a script only reveals mid-fill is found: it is invisible before the \
             interaction that reveals it and listed after. Cheap: it reads the form, it does \
             not explore it.\n\n\
             Each control also reports `positions`: where it is drawn, as \
             `{page, x, y, width, height}` in points with the origin at the page's top-left — \
             the exact shape xfa_render_region's rect_pt takes, so an entry here can be passed \
             straight to that tool to zoom in on it. An empty list means the form is not \
             currently showing this control. Each instance of a repeated section is its own \
             control with its own indexed path (`Row.Amount`, `Row[1].Amount`, ...) and its \
             own position. Positions come from the laid-out form, which is why this tool — \
             not the raw XFA data server's xfa_node — is where to find them.\n\n\
             What the template says about a field's value is reported too, each only when \
             present: `format`, the picture clause the form shows the value through (such as \
             `date{DD.MM.YYYY}`); `shown`, the value as the form shows it when that differs \
             from `value`, which stays canonical (a date `YYYY-MM-DD`, a number `1234.5`); \
             `max_chars`, the most characters it takes; `multiline`; `caption`, the label \
             drawn beside the field; `tooltip`, the field's `<assist><toolTip>`; and \
             `mandatory`, when the form requires a value.",
            read_schema::<ControlsArgs>(),
        ),
        tool(
            "xfa_field",
            "One field's current state, exactly as xfa_controls lists it: its kind, value, \
             options, whether it is visible, its `access` (`open`, `readOnly`, `protected` or \
             `nonInteractive`, and `access_from` when a locked container imposes it), where it \
             is drawn, and its `format`, `shown` value, `max_chars`, `multiline`, `caption`, \
             `tooltip` and `mandatory` when the template gives them. The cheap way to check a single field after an interaction, \
             for example whether a script locked or prefilled it, without listing the whole \
             form. Addressed like xfa_controls.",
            read_schema::<FieldArgs>(),
        ),
    ]
}

/// The `u2s.manifest` resource: what u2s reads at registration to learn each
/// tool's role, format scope, session contract and conformance vectors.
pub fn manifest() -> Value {
    let scope = json!({ "output_formats": [] });
    json!({
        "contract": CONTRACT_VERSION,
        "server": { "name": "u2s-render-xfa-mcp", "version": env!("CARGO_PKG_VERSION") },
        // `xfa_info`/`xfa_page_text`/`xfa_render_pages` opt into the
        // normalizer's ingest capabilities (`u2s_mcp::manifest::IngestCapability`),
        // so an XFA input is normalized from the real form rather than from
        // pdfium's "please update your reader" shim page. The election stays
        // correct for every document: pdfium answers `applicable: false` for
        // an XFA form, and `xfa_info` fails with `not_xfa` for an ordinary
        // PDF. No `outline`: `u2s-xfa-mcp` serves the XFA outline.
        "tools": [
            { "tool": "xfa_open",          "role": "query", "scope": scope },
            { "tool": "xfa_set",           "role": "query", "scope": scope },
            { "tool": "xfa_click",         "role": "query", "scope": scope },
            { "tool": "xfa_reset",         "role": "query", "scope": scope },
            { "tool": "xfa_close",         "role": "query", "scope": scope },
            { "tool": "xfa_info",          "role": "query", "ingest": "info",   "scope": scope },
            { "tool": "xfa_render_page",   "role": "query", "scope": scope },
            { "tool": "xfa_render_pages",  "role": "query", "ingest": "render", "scope": scope },
            { "tool": "xfa_render_region", "role": "query", "scope": scope },
            { "tool": "xfa_page_text",     "role": "query", "ingest": "text",   "scope": scope },
            { "tool": "xfa_search_text",   "role": "query", "scope": scope },
            { "tool": "xfa_controls",      "role": "query", "scope": scope },
            { "tool": "xfa_field",         "role": "query", "scope": scope },
        ],
        "sessions": {
            "open": "xfa_open",
            "close": "xfa_close",
            "probe": "xfa_info",
            "mutators": ["xfa_set", "xfa_click", "xfa_reset"],
        },
        "test_vectors": [
            {
                "tool": "xfa_info",
                "args": { "doc_path": "$FIXTURES/minimal.xfa.pdf" },
                "expect": { "structured": { "kind": "xfa", "page_count": 1, "applicable": true } }
            },
            {
                "tool": "xfa_render_page",
                "args": { "doc_path": "$FIXTURES/minimal.xfa.pdf", "page": 1, "dpi": 72, "format": "png" },
                "expect": { "structured": { "page": 1 } }
            },
            {
                "tool": "xfa_search_text",
                "args": { "doc_path": "$FIXTURES/minimal.xfa.pdf", "query": "Ada" },
                "expect": { "structured": { "total_matches": 1, "truncated": false } }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_of(name: &str) -> Value {
        tool_specs()
            .into_iter()
            .find(|spec| spec["name"] == name)
            .unwrap_or_else(|| panic!("{name} is declared"))["input_schema"]
            .clone()
    }

    fn violations(name: &str, args: Value) -> Vec<String> {
        u2s_schema::validate(&schema_of(name), &args)
            .expect("the schema compiles")
            .into_iter()
            .map(|v| format!("{}: {}", v.pointer, v.message))
            .collect()
    }

    /// The arguments a model sent in a reproduced run, every optional field
    /// filled in. The schema itself must refuse them, so the agent gate
    /// tells the model to leave the empty ones out.
    #[test]
    fn the_schema_refuses_every_optional_field_sent_empty() {
        let filled_in = json!({
            "query": "AAOV",
            "session": "sess_1",
            "revision": 0,
            "state": { "steps": [], "selections": [] }
        });
        assert!(!violations("xfa_search_text", filled_in).is_empty());

        for args in [
            json!({ "doc_path": "", "query": "AAOV" }),
            json!({ "doc_path": "/f.pdf", "query": "AAOV", "state": {} }),
            json!({ "doc_path": "/f.pdf", "query": "AAOV", "state": { "steps": [ { "set": { "field": "a", "value": "b" }, "click": {} } ] } }),
        ] {
            assert!(
                !violations("xfa_search_text", args.clone()).is_empty(),
                "{args}"
            );
        }
        assert!(
            !violations("xfa_controls", json!({ "doc_path": "/f.pdf", "kinds": [] })).is_empty()
        );
        assert!(
            !violations(
                "xfa_render_pages",
                json!({ "doc_path": "/f.pdf", "pages": [] })
            )
            .is_empty()
        );
        assert!(
            !violations(
                "xfa_render_page",
                json!({ "doc_path": "/f.pdf", "page": 1, "max_edge_px": 0 })
            )
            .is_empty()
        );
        assert!(!violations("xfa_render_region", json!({ "doc_path": "/f.pdf", "page": 1, "dpi": 0, "rect_pt": { "x": 0, "y": 0, "width": 1, "height": 1 } })).is_empty());
    }

    fn state_of(state: Value) -> Result<StateSpec, String> {
        u2s_render_core::args::parse_args::<XfaArgs<InfoArgs>>(
            &json!({ "doc_path": "/f.pdf", "state": state }),
        )
        .map(|a| a.document.state.map(StateArg::into_spec).unwrap_or_default())
        .map_err(|err| err.to_string())
    }

    #[test]
    fn a_state_reads_into_its_steps_in_order() {
        let spec = state_of(json!({ "steps": [
            { "click": { "field": "Add" } },
            { "set": { "field": "Row[1].Amount", "value": "5" } }
        ] }))
        .unwrap();
        assert_eq!(
            spec.steps,
            vec![
                Step::Click { field: "Add".to_owned() },
                Step::Set(SelectionSpec { field: "Row[1].Amount".to_owned(), value: "5".to_owned() }),
            ]
        );
        let spec = state_of(json!({ "selections": [{ "field": "a", "value": "x" }] })).unwrap();
        assert_eq!(spec, StateSpec::selections(vec![SelectionSpec { field: "a".to_owned(), value: "x".to_owned() }]));
    }

    #[test]
    fn a_malformed_state_is_refused_saying_what_is_wrong() {
        for (state, needles) in [
            (json!({ "steps": [], "selections": [] }), &["state", "both"][..]),
            (json!({ "selction": [] }), &["state", "selections", "selction"][..]),
            (json!("steps"), &["state", "object"][..]),
            (json!({ "steps": [{ "set": { "field": "a" } }] }), &["state", "value"][..]),
            (json!({ "steps": [{ "set": { "field": "a", "value": "b" }, "click": {} }] }), &["state"][..]),
        ] {
            let err = state_of(state.clone()).expect_err("refused");
            for needle in needles {
                assert!(err.contains(needle), "{state}: {err}");
            }
        }
    }

    #[test]
    fn every_tool_schema_can_be_offered_as_function_parameters() {
        for spec in tool_specs() {
            assert_eq!(
                u2s_schema::tool_schema_violations(&spec["input_schema"]),
                Vec::<String>::new(),
                "{}",
                spec["name"]
            );
        }
    }

    /// The well-formed calls the schemas exist for still pass.
    #[test]
    fn the_schema_accepts_a_state_of_steps_or_of_selections() {
        for state in [
            json!({ "steps": [ { "set": { "field": "a", "value": "" } }, { "click": { "field": "b" } } ] }),
            json!({ "selections": [ { "field": "a", "value": "x" } ] }),
        ] {
            let args = json!({ "doc_path": "/f.pdf", "query": "AAOV", "state": state });
            assert_eq!(violations("xfa_search_text", args), Vec::<String>::new());
        }
        assert_eq!(
            violations(
                "xfa_search_text",
                json!({ "session": "sess_1", "revision": 0, "query": "AAOV" })
            ),
            Vec::<String>::new()
        );
    }
}
