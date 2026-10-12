//! The twelve interactive control tools' schemas, manifest entries and
//! offline test vectors -- shared verbatim by both `u2s-aem-verify-mcp`
//! and `u2s-aem-ubs-verify-mcp`, since neither their wording nor their
//! shape depends on which format a profile serves (unlike `verify_run`'s
//! own description, which each bin still writes for itself). Kept apart
//! from `crate::interactive` (behaviour) the same way `specs.rs` in every
//! other u2s server is kept apart from the code it describes: a tool
//! description is prompt surface, worth reading and reviewing on its own.

use serde_json::{Value, json};

use crate::interactive::{ControlKind, ControlsQuery, MAX_SESSIONS_PER_INSTANCE};

fn instance_id_prop() -> Value {
    json!({
        "type": "string",
        "description":
            "Any stable string identifying the calling agent/run -- the same value passed to \
             verify_run/verify_status. Each value gets its own AEM+Chromium instance, which \
             holds that caller's sessions. Usually injected by the server; omit to use one \
             shared default instance."
    })
}

fn session_prop() -> Value {
    json!({
        "type": "string",
        "description": "The session handle verify_open returned: one open form, in its own browser tab."
    })
}

fn revision_prop(description: &str) -> Value {
    json!({ "type": "integer", "minimum": 0, "description": description })
}

/// Builds one tool spec. `schema` is the whole `{"type": "object", "properties": {...}}`
/// object; `additionalProperties: false` is added here so every spec gets
/// it without repeating it. No `required` array: like every other tool
/// this crate declares (`verify_run`, `verify_status`), a missing argument
/// is enforced by `crate::server`'s own parsing (`require_str`/`require_u64`),
/// not advertised at the JSON Schema level.
fn tool(name: &str, description: &str, mut schema: Value) -> Value {
    schema
        .as_object_mut()
        .expect("schema is always built as a JSON object")
        .insert("additionalProperties".to_owned(), json!(false));
    json!({
        "name": name,
        "description": description,
        "input_schema": schema,
    })
}

/// The twelve interactive tool specs, in the order they are meant to be
/// used: open, then whatever mix of controls/set/click/next/prev/goto/reset/screenshot
/// an agent needs, then submit, then close.
pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "verify_open",
            &format!(
                "Opens a FileVault package's form for interaction, one step at a time, in a new \
                 session: its own browser tab on this caller's persistent AEM instance -- the \
                 alternative to verify_run's one-shot walk. Returns a `session` handle plus \
                 `revision: 0`; every later interactive call names both, and \
                 `expected_revision`/`revision` is refused (naming the current one) if it does \
                 not match, so a caller can never act on a view of the form that has moved. \
                 Several sessions can be open at once, including several of the same package \
                 (they share its install), up to {MAX_SESSIONS_PER_INSTANCE} per instance. \
                 Refused when another open session shows a different package for the same form. \
                 Call verify_controls next to see what can be set. Side-effecting: it installs \
                 the package on the instance."
            ),
            json!({
                "type": "object",
                "properties": {
                    "package": {
                        "type": "string",
                        "description": "A u2s blob handle naming the FileVault package to open."
                    },
                    "package_path": {
                        "type": "string",
                        "description": "A filesystem path to the package, for conformance test vectors."
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_controls",
            "Every control on the open form: every radio group, checkbox, dropdown and text \
             field, each identified by `field`, its SOM expression (unique, and the reference \
             every other tool takes; a field inside a repeated panel's second instance reads \
             like `...Row[1].Amount[0]`), plus its `name`, the repeated panel instance it \
             belongs to (`instance_index`), its options (for a choice control), what it is \
             currently set to, \
             whether it is visible, enabled and required, the format it must match (`pattern`, \
             a regular expression or an AEM picture clause such as date{YYYY-MM-DD}, when the \
             form checks one), its validation state (`valid` and \
             `error_message` from the form's last validation, which runs when verify_next is \
             blocked or verify_submit refuses; until then `valid` is true even for an empty \
             required field), which panel it lives on, and -- when \
             visible -- where it is drawn (feed straight into verify_screenshot's own `field` \
             argument to zoom in on it). Addressed by `session` and `revision`, answering for the \
             form as it stands after whatever interactions already happened -- this is how a \
             control a script only reveals mid-fill is found: absent before the interaction that \
             reveals it, present after. Also reports the current panel and the driver's own \
             `has_next`/`is_terminal` navigation signals. Paged like xfa_controls: at most \
             `limit` controls (default 100, at most 500) from `offset`, optionally only the \
             `kinds` asked for; `total` counts every match and `next_offset` is where the next \
             page starts (absent on the last page). A read: `revision` must match exactly, \
             but this call never advances it.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "revision": revision_prop("The form's current revision, from verify_open or the last interactive call."),
                    "offset": { "type": "integer", "minimum": 0, "description": "Where the page starts; default 0." },
                    "limit": {
                        "type": "integer", "minimum": 1, "maximum": ControlsQuery::MAX_LIMIT,
                        "description": "How many controls at most; default 100."
                    },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string", "enum": ControlKind::ALL },
                        "description": "Only controls of these kinds; omit for every kind."
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_field",
            "One control by `field` (its SOM expression, or a name only one control carries), \
             in the same shape verify_controls lists it -- the cheap way to re-read a control \
             after an interaction. A read: `revision` must match exactly, but this call never \
             advances it.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "revision": revision_prop("The form's current revision."),
                    "field": {
                        "type": "string",
                        "description": "A control's `field` (its SOM expression), from verify_controls."
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_set",
            "Sets one control's value on the form's model, the same place the form's own data \
             binding writes it. The form's own scripts run in response, so this can reveal a \
             hidden section, recompute a total, or move content onto another page. `field` is a \
             control's SOM expression from verify_controls (refused for a readOnly control, which \
             only the form's own scripts may change, and for a signature, which is drawn); a \
             name is accepted only while exactly \
             one control carries it (instances of a repeated panel share names, and an \
             ambiguous name is refused listing their SOM expressions). `value` \
             is one of the control's own option values for a radio or dropdown, an array of \
             option values for a multi-select checkbox, or free text/a number for anything else \
             -- a value that is not one of a choice control's own options is refused naming the \
             real ones, never silently coerced. Returns the new `revision` plus what happened: \
             which other fields changed as a side effect, which controls appeared or disappeared, \
             the driver's own navigation signals, and any native dialogs (alert, confirm) the \
             form raised, each already accepted as if OK was clicked. To fill several controls \
             at once, pass `fields` (up to 25 `{field, value}` pairs) instead of `field`/`value`: \
             they are set in order, each one's scripts running before the next. The batch stops \
             at the first refused set: the first one refused fails the call as a single set \
             would, a later one is reported under `failed` while the sets before it stay applied \
             and `revision` is the revision after the last of them. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "field": {
                        "type": "string",
                        "description": "A control's `field` (its SOM expression), from verify_controls."
                    },
                    "value": {
                        "description":
                            "What to set it to -- one of the control's own option values (an \
                             array for a multi-select checkbox), or free text/a number."
                    },
                    "fields": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 25,
                        "description": "Several controls to set in order, instead of `field`/`value`.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "field": {
                                    "type": "string",
                                    "description": "A control's `field` (its SOM expression), from verify_controls."
                                },
                                "value": {
                                    "description": "What to set it to, as `value` above."
                                }
                            },
                            "required": ["field", "value"]
                        }
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_click",
            "Presses one button the way a visitor would, running whatever the form's own click \
             script does. This is how a repeated panel gains or loses an instance: press its Add \
             or Remove button; the new instance's controls are listed in `appeared` and in \
             verify_controls under their own SOM expressions (`...Row[1].Amount[0]` for the \
             second instance), and `instances_changed` is true. When a button that changes \
             instances changed nothing, `warning` says why, usually that the group is already at \
             its maxOccur or minOccur. Each button in verify_controls says what it does in \
             `click`: `instances`, `navigation`, `submit` or `script`. Refused for a control that \
             is not a button (set it with verify_set), for a button that is hidden or disabled \
             right now, and for a submit button (use verify_submit). For wizard steps prefer \
             verify_next/verify_prev, which confirm the panel actually moved. Returns the same \
             shape verify_set does. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "field": {
                        "type": "string",
                        "description": "The button's `field` (its SOM expression), from verify_controls."
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_next",
            "Advances to the next wizard panel, the same click verify_run's own wizard walk \
             performs, confirming the visible panel actually changed. Refused if no \"next\" \
             control could be clicked, or if one was clicked but the panel never changed; the \
             refusal then lists the fields the form's validation reports on this panel (usually \
             required fields still empty), so fill those and call verify_next again. Returns the \
             same shape verify_set does, with `field`/`value` both null. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_prev",
            "Goes back to the previous wizard panel, confirming the visible panel actually \
             changed, and reports the same shape verify_next does. Refused on the first panel, \
             where there is nothing to go back to. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_goto",
            "Jumps straight to one panel of a wizard, tab set or accordion, confirming the \
             visible panel actually changed. `panel` is a SOM expression from the `panels` list \
             verify_open and verify_controls return (each entry says which container it belongs \
             to, its layout, whether it is visible and whether it is the one shown now); a name \
             only one panel carries is accepted too. Going to the panel already shown changes \
             nothing. Refused for a panel that is not visible. A jump forward skips the step \
             validation verify_next runs, and skips whatever the form's own next button does on \
             the way (a summary panel reached by a jump may be empty), so use verify_next to \
             walk a wizard as a visitor would and verify_goto to inspect or revisit a panel; \
             verify_submit still validates the whole form. Returns the same shape verify_next \
             does. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "panel": {
                        "type": "string",
                        "description": "A panel's `som` from `panels`."
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_reset",
            "Puts the form back the way it opened, keeping the same `session` handle -- cheaper \
             than closing and opening again, since the package stays installed and only the \
             page itself is reloaded. Returns the same shape verify_next does, describing what \
             changed relative to how the form stood just before the reset. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_screenshot",
            "Renders the open form as an image: the whole page, or -- when `field` is given -- \
             just that control's own on-screen rectangle (from verify_controls' own `position`; \
             refused if the control is not currently visible, since there is nothing to crop). \
             Returns the image inline when it is small enough, otherwise a blob handle the \
             caller dereferences out of band. A read: `revision` must match exactly, but this \
             call never advances it.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "revision": revision_prop("The form's current revision."),
                    "format": {
                        "type": "string",
                        "enum": ["png", "jpeg"],
                        "description": "Image format; default png."
                    },
                    "field": {
                        "type": "string",
                        "description": "A control's `field` (its SOM expression), to crop the screenshot to just that control; omit for the whole page."
                    },
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_submit",
            "Submits the form from its current panel -- refused unless the driver's own \
             navigation signals already say this is the wizard's last panel (call verify_next \
             until `has_next` is false and `is_terminal` is true first), and refused, listing \
             each failing field with its message, when the form's own validation rejects \
             anything. Captures whatever this \
             profile's submit artefact strategy produces (a browser download, a Document of \
             Record fetch, or nothing), the same way verify_run's own submit step does. Does not \
             close the form -- call verify_close afterward. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
        tool(
            "verify_close",
            &format!(
                "Closes the session's browser tab and releases the `session` handle; the package \
                 is uninstalled once no other open session shows it. Always worth calling once \
                 done with a form: an instance holds at most {MAX_SESSIONS_PER_INSTANCE} \
                 sessions, and verify_run on an instance with any session open is refused. \
                 Side-effecting."
            ),
            json!({
                "type": "object",
                "properties": {
                    "session": session_prop(),
                    "instance_id": instance_id_prop(),
                }
            }),
        ),
    ]
}

/// The manifest `tools[]` entries for the twelve interactive tools, scoped
/// to `format` like every other tool this crate declares. None of them
/// carries a `verify` capability -- only `verify_run` does -- and
/// `verify_controls`/`verify_field`/`verify_screenshot` are the only three that are not
/// `side_effecting` (they read the form, never mutate it).
pub fn manifest_tools(format: &str) -> Vec<Value> {
    const SIDE_EFFECTING: &[&str] = &[
        "verify_open",
        "verify_set",
        "verify_click",
        "verify_next",
        "verify_prev",
        "verify_goto",
        "verify_reset",
        "verify_submit",
        "verify_close",
    ];
    tool_specs()
        .iter()
        .map(|spec| {
            let name = spec["name"].as_str().expect("every spec has a name");
            let mut entry = json!({
                "tool": name,
                "role": "query",
                "scope": { "output_formats": [format] }
            });
            if SIDE_EFFECTING.contains(&name) {
                entry["side_effecting"] = json!(true);
            }
            entry
        })
        .collect()
}

/// Offline conformance vectors for the twelve interactive tools: every one
/// but `verify_open` addresses a handle that was never issued, refused
/// with `is not open` before anything touches Docker (`crate::interactive`'s
/// `on_session` resolves the handle before any of them boots an
/// instance); `verify_open` itself is given a file that is not a FileVault
/// package at all (`$FIXTURES/minimal.xfa.pdf`, a plain PDF, already a
/// fixture this repository ships), refused with `package_invalid` by
/// `crate::package_check::inspect` before Docker is even connected to --
/// exactly the same "dry_run-shaped" guarantee `verify_run`'s own vector
/// relies on.
pub fn test_vectors() -> Vec<Value> {
    let bogus_session = "sess_00000000000000000000000000000000";
    vec![
        json!({
            "tool": "verify_open",
            "args": { "package_path": "$FIXTURES/minimal.xfa.pdf" },
            "expect": { "error": true, "error_contains": "package_invalid" }
        }),
        json!({
            "tool": "verify_controls",
            "args": { "session": bogus_session, "revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_field",
            "args": { "session": bogus_session, "revision": 0, "field": "x" },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_set",
            "args": { "session": bogus_session, "expected_revision": 0, "field": "x", "value": "x" },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_click",
            "args": { "session": bogus_session, "expected_revision": 0, "field": "x" },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_goto",
            "args": { "session": bogus_session, "expected_revision": 0, "panel": "x" },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_next",
            "args": { "session": bogus_session, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_prev",
            "args": { "session": bogus_session, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_reset",
            "args": { "session": bogus_session, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_screenshot",
            "args": { "session": bogus_session, "revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_submit",
            "args": { "session": bogus_session, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_close",
            "args": { "session": bogus_session },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_twelve_interactive_tools() {
        assert_eq!(tool_specs().len(), 12);
    }

    #[test]
    fn every_tool_description_is_real_prompt_surface() {
        for spec in tool_specs() {
            let description = spec["description"].as_str().expect("a description");
            assert!(
                description.len() > 60,
                "u2s-mcp's conformance suite fails an empty description: {description}"
            );
        }
    }

    #[test]
    fn only_the_mutators_are_side_effecting() {
        let tools = manifest_tools("aem");
        let by_name = |name: &str| tools.iter().find(|t| t["tool"] == name).unwrap();
        for name in [
            "verify_open",
            "verify_set",
            "verify_click",
            "verify_next",
            "verify_prev",
            "verify_goto",
            "verify_reset",
            "verify_submit",
            "verify_close",
        ] {
            assert_eq!(by_name(name)["side_effecting"], json!(true), "{name}");
        }
        for name in ["verify_controls", "verify_field", "verify_screenshot"] {
            assert_eq!(by_name(name)["side_effecting"], Value::Null, "{name}");
        }
    }

    #[test]
    fn no_interactive_tool_declares_the_verify_run_capability() {
        for tool in manifest_tools("aem") {
            assert_eq!(tool.get("verify"), None, "{tool}");
        }
    }

    #[test]
    fn every_tool_has_at_least_one_test_vector() {
        let vectors = test_vectors();
        let vector_tools: std::collections::BTreeSet<&str> = vectors
            .iter()
            .map(|v| v["tool"].as_str().expect("a tool name"))
            .collect();
        let specs = tool_specs();
        let tool_names: std::collections::BTreeSet<&str> = specs
            .iter()
            .map(|spec| spec["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(vector_tools, tool_names);
    }

    #[test]
    fn every_test_vector_expects_an_error_and_is_answerable_without_docker() {
        // Every interactive vector's whole point is a refusal that never
        // touches Docker (an unresolvable handle, or a package that fails
        // offline inspection) -- see this module's own doc.
        for vector in test_vectors() {
            assert_eq!(vector["expect"]["error"], json!(true), "{vector}");
        }
    }
}
