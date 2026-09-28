//! The nine interactive control tools' schemas, manifest entries and
//! offline test vectors -- shared verbatim by both `u2s-aem-verify-mcp`
//! and `u2s-aem-ubs-verify-mcp`, since neither their wording nor their
//! shape depends on which format a profile serves (unlike `verify_run`'s
//! own description, which each bin still writes for itself). Kept apart
//! from `crate::interactive` (behaviour) the same way `specs.rs` in every
//! other u2s server is kept apart from the code it describes: a tool
//! description is prompt surface, worth reading and reviewing on its own.

use serde_json::{Value, json};

fn session_id_prop() -> Value {
    json!({
        "type": "string",
        "description":
            "Any stable string identifying the calling agent/run -- the same value passed to \
             verify_run/verify_status. Two different values get two independent AEM+Chromium \
             sessions. Omit to use a single shared default session."
    })
}

fn form_prop() -> Value {
    json!({
        "type": "string",
        "description": "The handle verify_open returned."
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

/// The nine interactive tool specs, in the order they are meant to be
/// used: open, then whatever mix of controls/set/next/prev/reset/screenshot
/// an agent needs, then submit, then close.
pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "verify_open",
            "Installs a FileVault package on this profile's persistent session and opens its \
             form for interaction, one step at a time -- the alternative to verify_run's own \
             one-shot walk. Returns a `form` handle plus `revision: 0`; every later interactive \
             call names both, and `expected_revision`/`revision` is refused (naming the current \
             one) if it does not match, so a caller can never act on a view of the form that has \
             moved. Call verify_controls next to see what can be set. Refused if a form is \
             already open on this session_id -- call verify_close first. This tool is \
             side-effecting: it installs software on a session it does not tear down.",
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
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_controls",
            "Every control on the open form: every radio group, checkbox, dropdown and text \
             field, with its options (for a choice control), what it is currently set to, \
             whether it is visible, enabled and required, which panel it lives on, and -- when \
             visible -- where it is drawn (feed straight into verify_screenshot's own `field` \
             argument to zoom in on it). Addressed by `form` and `revision`, answering for the \
             form as it stands after whatever interactions already happened -- this is how a \
             control a script only reveals mid-fill is found: absent before the interaction that \
             reveals it, present after. Also reports the current panel and the driver's own \
             `has_next`/`is_terminal` navigation signals. A read: `revision` must match exactly, \
             but this call never advances it.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "revision": revision_prop("The form's current revision, from verify_open or the last interactive call."),
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_set",
            "Sets one control the way a person would: focus into it, change it, focus out. The \
             form's own scripts run in response, so this can reveal a hidden section, recompute \
             a total, or move content onto another page. `field` is a control's own name, from \
             verify_controls; an unknown one is refused naming the fields that do exist. `value` \
             is one of the control's own option values for a radio or dropdown, an array of \
             option values for a multi-select checkbox, or free text/a number for anything else \
             -- a value that is not one of a choice control's own options is refused naming the \
             real ones, never silently coerced. Returns the new `revision` plus what happened: \
             which other fields changed as a side effect, which controls appeared or disappeared, \
             and the driver's own navigation signals. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "field": {
                        "type": "string",
                        "description": "A control's own name, from verify_controls."
                    },
                    "value": {
                        "description":
                            "What to set it to -- one of the control's own option values (an \
                             array for a multi-select checkbox), or free text/a number."
                    },
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_next",
            "Advances to the next wizard panel, the same click verify_run's own wizard walk \
             performs, confirming the visible panel actually changed. Refused if no \"next\" \
             control could be clicked, or if one was clicked but the panel never changed (a \
             validation script may be blocking navigation). Returns the same shape verify_set \
             does, with `field`/`value` both null. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_prev",
            "Goes back to the previous wizard panel -- the same mechanics as verify_next, in \
             reverse. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_reset",
            "Puts the form back the way it opened, keeping the same `form` handle -- cheaper \
             than closing and opening again, since the package stays installed and only the \
             page itself is reloaded. Returns the same shape verify_next does, describing what \
             changed relative to how the form stood just before the reset. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "session_id": session_id_prop(),
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
                    "form": form_prop(),
                    "revision": revision_prop("The form's current revision."),
                    "format": {
                        "type": "string",
                        "enum": ["png", "jpeg"],
                        "description": "Image format; default png."
                    },
                    "field": {
                        "type": "string",
                        "description": "A control's own name, to crop the screenshot to just that control; omit for the whole page."
                    },
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_submit",
            "Submits the form from its current panel -- refused unless the driver's own \
             navigation signals already say this is the wizard's last panel (call verify_next \
             until `has_next` is false and `is_terminal` is true first). Captures whatever this \
             profile's submit artefact strategy produces (a browser download, a Document of \
             Record fetch, or nothing), the same way verify_run's own submit step does. Does not \
             close the form -- call verify_close afterward. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "expected_revision": revision_prop("The revision this call is based on."),
                    "session_id": session_id_prop(),
                }
            }),
        ),
        tool(
            "verify_close",
            "Closes the page and browser session, uninstalls the package, and releases the \
             `form` handle. Always worth calling once done with a form, so the session's one \
             installed-package slot is free for the next verify_open or verify_run -- until \
             then, verify_run on the same session_id is refused. Side-effecting.",
            json!({
                "type": "object",
                "properties": {
                    "form": form_prop(),
                    "session_id": session_id_prop(),
                }
            }),
        ),
    ]
}

/// The manifest `tools[]` entries for the nine interactive tools, scoped
/// to `format` like every other tool this crate declares. None of them
/// carries a `verify` capability -- only `verify_run` does -- and
/// `verify_controls`/`verify_screenshot` are the only two that are not
/// `side_effecting` (they read the form, never mutate it).
pub fn manifest_tools(format: &str) -> Vec<Value> {
    const SIDE_EFFECTING: &[&str] = &[
        "verify_open",
        "verify_set",
        "verify_next",
        "verify_prev",
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

/// Offline conformance vectors for the nine interactive tools: every one
/// but `verify_open` addresses a handle that was never issued, refused
/// with `is not open` before anything touches Docker (`crate::interactive`'s
/// own `take_form` resolves the handle before any of them boots a
/// session); `verify_open` itself is given a file that is not a FileVault
/// package at all (`$FIXTURES/minimal.xfa.pdf`, a plain PDF, already a
/// fixture this repository ships), refused with `package_invalid` by
/// `crate::package_check::inspect` before Docker is even connected to --
/// exactly the same "dry_run-shaped" guarantee `verify_run`'s own vector
/// relies on.
pub fn test_vectors() -> Vec<Value> {
    let bogus_form = "form_00000000000000000000000000000000";
    vec![
        json!({
            "tool": "verify_open",
            "args": { "package_path": "$FIXTURES/minimal.xfa.pdf" },
            "expect": { "error": true, "error_contains": "package_invalid" }
        }),
        json!({
            "tool": "verify_controls",
            "args": { "form": bogus_form, "revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_set",
            "args": { "form": bogus_form, "expected_revision": 0, "field": "x", "value": "x" },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_next",
            "args": { "form": bogus_form, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_prev",
            "args": { "form": bogus_form, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_reset",
            "args": { "form": bogus_form, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_screenshot",
            "args": { "form": bogus_form, "revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_submit",
            "args": { "form": bogus_form, "expected_revision": 0 },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
        json!({
            "tool": "verify_close",
            "args": { "form": bogus_form },
            "expect": { "error": true, "error_contains": "is not open" }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_nine_interactive_tools() {
        assert_eq!(tool_specs().len(), 9);
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
            "verify_next",
            "verify_prev",
            "verify_reset",
            "verify_submit",
            "verify_close",
        ] {
            assert_eq!(by_name(name)["side_effecting"], json!(true), "{name}");
        }
        for name in ["verify_controls", "verify_screenshot"] {
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
