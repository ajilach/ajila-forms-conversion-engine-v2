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
//! tool take either `doc_path` (a one-shot, stateless address, with the
//! optional `state` shorthand for a specific point in the space) or
//! `session` plus `revision` (a frozen view of a live, interacted-with
//! form) — never both, never neither.
//!
//! Schemas are plain `serde_json::Value` rather than `schemars` derives, for
//! the same reason as the other render servers: it decouples the schema from
//! the rmcp/schemars versions, and keeps the tool descriptions — which are
//! prompt surface — visible in one file.

use serde_json::{Value, json};
use u2s_render_xfa::states::ControlKind;

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

fn tool(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "input_schema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

fn doc_path_prop() -> Value {
    json!({
        "type": "string",
        "description": "Absolute path to the PDF on the machine running this server, \
                        normally inside U2S_BLOB_DIR. Exactly one of doc_path and session \
                        is required."
    })
}

fn session_prop() -> Value {
    json!({
        "type": "string",
        "description": "A session handle from xfa_open. Exactly one of doc_path and session \
                        is required — pass this instead of doc_path to address a live, \
                        interacted-with form rather than the document on disk."
    })
}

fn revision_prop() -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "description": "Required alongside session: the revision you last saw. A stale or \
                        future revision is refused and the error names the current one."
    })
}

/// The `state` argument, shared by every read tool that produces output when
/// addressed by `doc_path`.
fn state_prop() -> Value {
    json!({
        "type": "object",
        "description": "Which form state to use, when addressing by doc_path. Omit for the \
                        state the form opens in. To choose another, list the form's controls \
                        with xfa_controls and say what to do to them, in order, as `steps`: \
                        `{\"set\": {\"field\", \"value\"}}` sets a control, \
                        `{\"click\": {\"field\"}}` presses a button. Everything not touched \
                        keeps its default. Presses happen in the order given, so two presses \
                        of an add button make two new rows, and a later step can set a field \
                        of a row an earlier press created (`Row[1].Amount`). `selections`, a \
                        list of sets only, is the older spelling and still accepted; give one \
                        of the two, not both. Ignored when addressing by session, since a \
                        session's own interactions already determine its state.",
        "properties": {
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "set": {
                            "type": "object",
                            "properties": {
                                "field": { "type": "string", "description": "A field path, from xfa_controls." },
                                "value": { "type": "string", "description": "What to set it to." }
                            },
                            "required": ["field", "value"]
                        },
                        "click": {
                            "type": "object",
                            "properties": {
                                "field": { "type": "string", "description": "A button's field path, from xfa_controls (kind `button`)." }
                            },
                            "required": ["field"]
                        }
                    }
                }
            },
            "selections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "field": { "type": "string", "description": "A control's field path, from xfa_controls." },
                        "value": { "type": "string", "description": "One of that control's options." }
                    },
                    "required": ["field", "value"]
                }
            }
        }
    })
}

pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "xfa_open",
            "Open a form for interaction and get a session to work in. Everything after this \
             addresses `session` plus a `revision` instead of `doc_path`: the form is live, \
             and each interaction produces a new revision. Returns the same shape as \
             xfa_info, plus `session` and `revision` (always 0 on open). Call xfa_controls \
             next to see what can be set.",
            json!({ "doc_path": doc_path_prop() }),
            json!(["doc_path"]),
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
             the scripts wrote.",
            json!({
                "session": session_prop(),
                "expected_revision": revision_prop(),
                "field": { "type": "string", "description": "A field path, from xfa_controls." },
                "value": { "type": "string", "description": "What to set it to — one of the control's options, or its own on-value for a radio button." }
            }),
            json!(["session", "expected_revision", "field", "value"]),
        ),
        tool(
            "xfa_click",
            "Press one button the way a person would. The button's own click script runs, \
             so on a form with a repeatable section this is how rows are added or removed: \
             pressing an add button creates the next instance, and every field in it is \
             listed in `appeared` under an indexed path such as `form1.Body.Row[1].Amount` \
             (the first instance has no index) and in the next xfa_controls. Pressing a \
             remove button lists the removed instance's fields in `disappeared`; the \
             instances after it move down one index. A press the form refuses, because the \
             section is already at its minimum or maximum number of instances, leaves \
             `instances_changed` false and says so in `warning`. Returns the same shape as \
             xfa_set. Pass the `revision` you last saw as `expected_revision`; a stale or \
             future one is refused and the error names the current one. A button whose \
             `access` is not `open` is refused, since a person cannot press it either.",
            json!({
                "session": session_prop(),
                "expected_revision": revision_prop(),
                "field": { "type": "string", "description": "A button's field path, from xfa_controls (kind `button`)." }
            }),
            json!(["session", "expected_revision", "field"]),
        ),
        tool(
            "xfa_reset",
            "Put a session's form back the way it opened, keeping the session and its \
             handle. Cheaper than closing and opening again. Returns the same shape as \
             xfa_set, with the new revision and what changed relative to how the session \
             stood just before the reset.",
            json!({
                "session": session_prop(),
                "expected_revision": revision_prop(),
            }),
            json!(["session", "expected_revision"]),
        ),
        tool(
            "xfa_close",
            "Release a session. Optional: an unused session is reclaimed on its own after \
             going idle for a while. Worth calling when done with a large form and the \
             memory should be freed sooner.",
            json!({ "session": session_prop() }),
            json!(["session"]),
        ),
        tool(
            "xfa_info",
            "Page count, per-page dimensions in points, detected language, and the XFA \
             packets present. Call this FIRST when addressing by doc_path: if it reports kind \
             'not_xfa' the document is an ordinary PDF and this server cannot render it — use \
             pdf_info and pdf_render_page instead.",
            json!({ "doc_path": doc_path_prop(), "session": session_prop(), "revision": revision_prop(), "state": state_prop() }),
            json!([]),
        ),
        tool(
            "xfa_render_page",
            "Render one page of the form to an image. Returns the image inline when it is \
             small enough, otherwise a blob handle the caller dereferences out of band. \
             dpi defaults to 144 and is clamped to 36..600; the long edge is clamped to \
             max_edge_px (default 2576) and the effective dpi is reported back.",
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "page": { "type": "integer", "minimum": 1, "description": "1-based page number." },
                "dpi": { "type": "number", "description": "Target resolution; default 144." },
                "format": { "type": "string", "enum": ["jpeg", "png"], "description": "Default jpeg." },
                "max_edge_px": { "type": "integer", "description": "Long-edge clamp; default 2576." },
                "state": state_prop()
            }),
            json!(["page"]),
        ),
        tool(
            "xfa_render_pages",
            "Render several pages with an explicit cursor. Give either `pages` (an exact \
             list) or `from` plus `limit` (a window). The response stops at whichever cap \
             binds first — an image count and a total byte budget — and reports which in \
             `budget_hit`, with `next_from` naming where to continue. Walk `next_from` \
             until it is null rather than asking for a whole document at once.",
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "pages": {
                    "type": "array", "items": { "type": "integer", "minimum": 1 },
                    "description": "Exact pages, in the order you want them."
                },
                "from": { "type": "integer", "minimum": 1, "description": "Window start; default 1." },
                "limit": { "type": "integer", "minimum": 1, "description": "Max images this call." },
                "dpi": { "type": "number" },
                "format": { "type": "string", "enum": ["jpeg", "png"] },
                "state": state_prop()
            }),
            json!([]),
        ),
        tool(
            "xfa_render_region",
            "Render a rectangular region of one page at high resolution — the tool for \
             reading small print. The rect is in points with the origin at the page's \
             TOP-LEFT. dpi defaults to 300.",
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "page": { "type": "integer", "minimum": 1 },
                "rect_pt": {
                    "type": "object",
                    "description": "Region in points, origin top-left of the page.",
                    "properties": {
                        "x": { "type": "number" }, "y": { "type": "number" },
                        "width": { "type": "number" }, "height": { "type": "number" }
                    },
                    "required": ["x", "y", "width", "height"]
                },
                "dpi": { "type": "number" },
                "format": { "type": "string", "enum": ["jpeg", "png"] },
                "state": state_prop()
            }),
            json!(["page", "rect_pt"]),
        ),
        tool(
            "xfa_page_text",
            "Text of one page, windowed by character offset. Always reports total_chars \
             and truncated, so a caller can tell a partial read from a whole one. Note the \
             text is logical, not visual: line breaks happen at render time, so this is \
             what the page says rather than how it wraps.",
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "page": { "type": "integer", "minimum": 1 },
                "offset": { "type": "integer", "minimum": 0, "description": "Character offset; default 0." },
                "limit": { "type": "integer", "minimum": 1, "description": "Characters to return; default 4000." },
                "state": state_prop()
            }),
            json!(["page"]),
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
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "query": { "type": "string", "description": "Substring, or regex pattern when regex is true." },
                "regex": { "type": "boolean", "description": "Treat query as a regex; default false. A literal is case-insensitive; a regex is not, so write (?i) if you want that." },
                "from": { "type": "integer", "minimum": 1, "description": "First page to scan; default 1." },
                "limit": { "type": "integer", "minimum": 1, "description": "Max matches returned; default 50." },
                "state": state_prop()
            }),
            json!(["query"]),
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
             not the raw XFA data server's xfa_node — is where to find them.",
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "state": state_prop(),
                "offset": { "type": "integer", "minimum": 0, "description": "First field to return; default 0. Pass the previous next_offset." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Fields to return; default 100." },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string", "enum": ControlKind::ALL.map(ControlKind::wire_name) },
                    "description": "Only fields of these kinds. Omit for every kind."
                }
            }),
            json!([]),
        ),
        tool(
            "xfa_field",
            "One field's current state, exactly as xfa_controls lists it: its kind, value, \
             options, whether it is visible, its `access` (`open`, `readOnly`, `protected` or \
             `nonInteractive`, and `access_from` when a locked container imposes it) and \
             where it is drawn. The cheap way to check a single field after an interaction, \
             for example whether a script locked or prefilled it, without listing the whole \
             form. Addressed like xfa_controls.",
            json!({
                "doc_path": doc_path_prop(),
                "session": session_prop(),
                "revision": revision_prop(),
                "state": state_prop(),
                "field": { "type": "string", "description": "A field path, from xfa_controls." }
            }),
            json!(["field"]),
        ),
    ]
}

/// The `u2s.manifest` resource: what u2s reads at registration to learn each
/// tool's role, format scope, session contract and conformance vectors.
pub fn manifest() -> Value {
    // Declares no ingest capability, for now: opting in would make this
    // renderer win the normalizer's `info`/`render` probes for every XFA
    // form (it reports `applicable` honestly where `u2s-render-pdf-mcp`'s
    // shim page does not), which changes `page_count`, `input_pages` and
    // the review prompt for every existing XFA input. A real improvement,
    // but a separate, visible change from this one.
    let scope = json!({ "output_formats": [] });
    let names = [
        "xfa_open",
        "xfa_set",
        "xfa_click",
        "xfa_reset",
        "xfa_close",
        "xfa_info",
        "xfa_render_page",
        "xfa_render_pages",
        "xfa_render_region",
        "xfa_page_text",
        "xfa_search_text",
        "xfa_controls",
        "xfa_field",
    ];
    json!({
        "contract": CONTRACT_VERSION,
        "server": { "name": "u2s-render-xfa-mcp", "version": env!("CARGO_PKG_VERSION") },
        "tools": names
            .iter()
            .map(|n| json!({ "tool": n, "role": "query", "scope": scope }))
            .collect::<Vec<_>>(),
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
                "expect": { "structured": { "kind": "xfa", "page_count": 1 } }
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
