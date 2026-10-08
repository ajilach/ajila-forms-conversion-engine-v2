//! The tool schemas and the manifest for the `redacto-ubs` verifier.
//!
//! **No `format` module.** The output format this profile verifies is
//! already owned by its own encoder server, `u2s-redacto-ubs-mcp`.
//! Registering this server never registers a format -- the same
//! distinction `u2s-aem-verify-mcp`'s own manifest draws for the same
//! reason.

use serde_json::{Value, json};

pub const CONTRACT_VERSION: &str = "1.0.0";

/// Scoped to the format this profile verifies, the same way every tool a
/// verify server declares is -- see `u2s-aem-verify-mcp`'s own precedent.
pub const FORMAT_KEY: &str = "redacto-ubs";

pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "verify_status",
            "description":
                "Reports this profile's own configuration (the platform images, which of them \
                 this host's Docker daemon is missing, or which setting is missing), whether \
                 Docker is reachable, and whether `session_id` has a booted platform. Touches \
                 nothing side-effecting -- safe to call at any time, and is how a caller \
                 distinguishes \"this server is misconfigured\" from \"Docker is down right \
                 now\".",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description":
                            "Which caller's platform to report on; omitted means the shared default session."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "verify_dump_check",
            "description":
                "Validates a dump offline: does it decode (well-formed SQL, every asset \
                 reference resolving, a non-empty body, every declared language covered). \
                 Exactly one of `artifact_blob` (a u2s blob handle) or `artifact_path` (a \
                 filesystem path, for conformance test vectors) must be given. Never touches \
                 Docker or a network -- this is the check `verify_run`'s own `dry_run: true` \
                 performs internally before anything with a side effect runs.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "artifact_blob": {
                        "type": "string",
                        "description": "A u2s blob handle naming the dump to check."
                    },
                    "artifact_path": {
                        "type": "string",
                        "description": "A filesystem path to the dump, for conformance test vectors."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "verify_run",
            "description":
                "Imports the dump into the Redacto platform of `session_id` (booted on that \
                 session's first call, which takes some seconds, then reused), replacing any \
                 earlier import of the same document (so an edited document can be \
                 re-verified), reports that document's row counts, then renders it once per \
                 declared language and returns each rendered PDF as an artefact. An import or \
                 render failure is an error finding naming the cause. `dry_run: true` performs \
                 only the offline dump check and touches neither Docker nor a network. Every \
                 artefact travels as a blob, never inline. This tool is side-effecting: it \
                 writes to the platform database and is never offered to the Conversion Agent.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "artifact_blob": {
                        "type": "string",
                        "description": "A u2s blob handle naming the dump to verify."
                    },
                    "artifact_path": {
                        "type": "string",
                        "description": "A filesystem path to the dump, for conformance test vectors."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Which caller's platform to verify on; omitted means the shared default session. Calls on one session serialize."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "Validate the dump offline only; default false."
                    }
                },
                "additionalProperties": false
            }
        }),
    ]
}

pub fn manifest() -> Value {
    json!({
        "contract": CONTRACT_VERSION,
        "server": {
            "name": "u2s-redacto-ubs-verify-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "tools": [
            {
                "tool": "verify_status",
                "role": "query",
                "scope": { "output_formats": [FORMAT_KEY] }
            },
            {
                "tool": "verify_dump_check",
                "role": "query",
                "scope": { "output_formats": [FORMAT_KEY] }
            },
            {
                "tool": "verify_run",
                "role": "query",
                "side_effecting": true,
                "verify": "run",
                "scope": { "output_formats": [FORMAT_KEY] }
            }
        ],
        // No `format` module: see this module's own doc.
        "format": null,
        "test_vectors": [
            {
                "tool": "verify_status",
                "args": {},
                "expect": { "structured": { "profile": { "name": "redacto-ubs" } } }
            },
            {
                "tool": "verify_dump_check",
                "args": { "artifact_path": "$FIXTURES/redacto-AAEV_019.sql" },
                "expect": { "structured": { "ok": true, "document_id": "aaev_019" } }
            },
            {
                "tool": "verify_run",
                "args": { "artifact_path": "$FIXTURES/redacto-AAEV_019.sql", "dry_run": true },
                "expect": { "structured": { "dry_run": true } }
            }
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_version_is_valid_semver() {
        semver::Version::parse(CONTRACT_VERSION).expect("the contract version must be semver");
    }

    #[test]
    fn there_are_three_tools_all_scoped_to_redacto_ubs() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0]["name"], "verify_status");
        assert_eq!(specs[1]["name"], "verify_dump_check");
        assert_eq!(specs[2]["name"], "verify_run");

        let tools = manifest()["tools"].clone();
        for tool in tools.as_array().expect("array") {
            assert_eq!(tool["scope"]["output_formats"][0], FORMAT_KEY);
        }
        assert_eq!(tools[2]["side_effecting"], true);
        assert_eq!(tools[2]["verify"], "run");
    }

    #[test]
    fn the_manifest_declares_no_format_module() {
        assert_eq!(manifest()["format"], Value::Null);
    }

    #[test]
    fn every_tool_has_a_test_vector() {
        let vectors = manifest()["test_vectors"].clone();
        let vectors = vectors.as_array().expect("an array");
        let vector_tools: std::collections::BTreeSet<&str> =
            vectors.iter().map(|v| v["tool"].as_str().expect("a tool name")).collect();
        assert_eq!(
            vector_tools,
            std::collections::BTreeSet::from(["verify_status", "verify_dump_check", "verify_run"]),
        );
    }

    #[test]
    fn every_tool_description_is_real_prompt_surface() {
        for spec in tool_specs() {
            let description = spec["description"].as_str().expect("a description");
            assert!(description.len() > 60, "{description}");
        }
    }
}
