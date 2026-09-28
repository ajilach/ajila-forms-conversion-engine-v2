//! The tool schemas and the manifest.
//!
//! Schemas are plain `serde_json::Value`, matching the convention the three
//! other u2s servers already follow: decoupled from any `schemars` or `rmcp`
//! derive version, and readable in one place because a tool description is
//! prompt surface.

use serde_json::{Value, json};

/// The output format's key. Not bare `aem`: the JSON Schema is
/// `u2s-aem`'s **generic** Adaptive Forms model, but the encoder is a UBS
/// profile, and a second AEM profile must be able to register beside this one
/// rather than collide with it.
pub const FORMAT_KEY: &str = "aem-ubs";

/// The MCP convention version this server speaks.
pub const CONTRACT_VERSION: &str = "1.0.0";

/// The format module's version. Bumped when the schema changes; because
/// `output_formats` is unique on `(key, version)` and snapshotting is
/// `ON CONFLICT DO NOTHING`, a re-registration of an unchanged server is
/// idempotent while a bumped version lands as a **new proposed** row under
/// existing runs rather than mutating one.
///
/// Bumped `0.1.0` -> `0.2.0` alongside `decode`'s addition: the model grew
/// `Common.resource_type`/`guide_node_class`/`jcr_name`/`bind_ref`/
/// `passthrough` and `FormMetadata.chrome`/`dam_chrome`/`folder_path`/
/// `root_panel_layout`, all `#[serde(default)]` so a `0.1.0` document still
/// deserializes -- but a real, decoded document now routinely carries
/// them, which a caller pinned to reading `0.1.0`'s own schema description
/// would not expect.
pub const FORMAT_VERSION: &str = "0.2.0";

pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "decode",
            "description":
                "Decode a UBS AEM FileVault content package back into an aem-ubs JSON document. \
                 Exactly one of `artifact_blob` (a u2s blob handle) or `artifact_path` (a \
                 filesystem path, for conformance test vectors) must be given. Decode is \
                 lossless-or-error: a package this decoder cannot fully represent returns a \
                 tool error naming what could not be represented, never a partial document. \
                 The decoded document is returned as a blob reference, never inlined, and is \
                 not yet validated against every possible source quirk a real, human-authored \
                 package may carry -- known accepted gaps (binary DOR renditions, most vault \
                 packaging metadata) are named in this crate's own design notes, not silently \
                 dropped from the decoded document.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "artifact_blob": {
                        "type": "string",
                        "description": "A u2s blob handle naming the FileVault package to decode."
                    },
                    "artifact_path": {
                        "type": "string",
                        "description": "A filesystem path to the package, for conformance test vectors."
                    },
                    "media_type": {
                        "type": "string",
                        "description": "A hint; ignored -- this decoder only ever reads a ZIP."
                    },
                    "filename": {
                        "type": "string",
                        "description": "A hint; ignored."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "encode",
            "description":
                "Encode a finished output-format JSON document into the UBS AEM FileVault \
                 content package. The document is validated against the aem-ubs semantic \
                 rules (name uniqueness, translation coverage, layout bounds) before \
                 encoding; a document that fails validation returns a tool error naming \
                 the JSON Pointer of each violation instead of a package. The package is \
                 returned as a blob reference, never inlined -- it is a ZIP archive, always \
                 well past the size a prompt should carry.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "output_json": {
                        "type": "object",
                        "description": "A document conforming to the aem-ubs JSON Schema."
                    }
                },
                "required": ["output_json"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "fragment_search",
            "description":
                "Search the UBS AEM fragment library by keyword. Returns each matching \
                 fragment's JCR path (the value to write as a Fragment node's frag_ref), \
                 title and a short preview -- never a verdict on whether it fits. This is a \
                 mechanical text search, not a match by field type or meaning: judge fit \
                 yourself from the title and preview before referencing a result. Returns no \
                 hits, not an error, when the library is empty or unconfigured for this \
                 deployment.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords to search for in fragment titles and previews."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        }),
    ]
}

/// The `u2s://manifest` resource.
pub fn manifest() -> Value {
    json!({
        "contract": CONTRACT_VERSION,
        "server": {
            "name": "u2s-aem-ubs-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "tools": [
            {
                "tool": "decode",
                "role": "decode",
                "scope": { "output_formats": [FORMAT_KEY] }
            },
            {
                "tool": "encode",
                "role": "encode",
                // Output-side only: an encoder does not care what the source
                // was. PLAN.md's `FormatScope`, with an empty side meaning
                // "any".
                "scope": { "output_formats": [FORMAT_KEY] }
            },
            {
                "tool": "fragment_search",
                "role": "query",
                "scope": { "output_formats": [FORMAT_KEY] }
            }
        ],
        "format": {
            "key": FORMAT_KEY,
            "version": FORMAT_VERSION,
            "description_md":
                "UBS AEM Adaptive Forms. The delivered artefact is a FileVault content \
                 package (a ZIP of JCR XML); this JSON is the working tree the encoder \
                 lowers into it. The schema is generated from the typed model in `u2s-aem`, \
                 which is the only place that model is visible.",
            "json_schema": u2s_aem::schema(),
        },
        "test_vectors": [
            {
                "tool": "decode",
                "args": { "artifact_path": "$FIXTURES/AF_AABF.zip" },
                "expect": { "structured": { "format_version": FORMAT_VERSION } }
            },
            {
                "tool": "encode",
                "args": { "output_json": minimal_form() },
                "expect": { "structured": { "blob": { "media_type": "application/zip" } } }
            },
            {
                "tool": "fragment_search",
                "args": { "query": "no fragment library is configured for this conformance run" },
                "expect": { "structured": { "hits": [] } }
            }
        ],
    })
}

/// A minimal but real `aem-ubs` document -- small enough to embed inline in
/// a test vector's own `args` (unlike `decode`'s own vector, which needs a
/// real package and so reaches for `$FIXTURES` instead).
fn minimal_form() -> Value {
    json!({
        "metadata": {
            "form_name": "ConformanceForm",
            "master_language": "en",
            "languages": ["en"],
            "dor": "none",
            "data_model": { "kind": "unbound" }
        },
        "pages": [
            {
                "name": "Page1",
                "properties": {},
                "children": [
                    {
                        "type": "TextField",
                        "common": {
                            "name": "Name",
                            "resource_type": "fd/af/components/controls/textbox"
                        },
                        "field": { "label": { "en": "Name" } },
                        "layout": { "width": 12 },
                        "input": "single_line"
                    }
                ]
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_version_is_valid_semver() {
        // `u2s_mcp::manifest::FormatModule::version` is a `semver::Version`,
        // so a non-semver string here would be a manifest this workspace's
        // own client refuses to parse.
        semver::Version::parse(FORMAT_VERSION).expect("the format version must be semver");
        semver::Version::parse(CONTRACT_VERSION).expect("the contract version must be semver");
    }

    #[test]
    fn the_manifest_carries_the_schema_from_u2s_aem_verbatim() {
        let manifest = manifest();
        assert_eq!(
            manifest["format"]["json_schema"],
            u2s_aem::schema(),
            "the schema must be u2s-aem's own, not a copy that can drift"
        );
        assert_eq!(manifest["format"]["key"], FORMAT_KEY);
    }

    #[test]
    fn there_are_three_tools_claiming_the_output_side() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0]["name"], "decode");
        assert_eq!(specs[1]["name"], "encode");
        assert_eq!(specs[2]["name"], "fragment_search");

        let tools = manifest()["tools"].clone();
        assert_eq!(tools[0]["role"], "decode");
        assert_eq!(tools[1]["role"], "encode");
        assert_eq!(tools[2]["role"], "query");
        for tool in tools.as_array().expect("array") {
            assert_eq!(tool["scope"]["output_formats"][0], FORMAT_KEY);
        }
    }

    /// Every tool now ships a test vector, per this crate's own design
    /// plan (C5's "test_vectors unblocking") -- unlike the earlier
    /// manifest-only state, all three tools can now become `tested` and be
    /// enabled for a dataset.
    #[test]
    fn every_tool_has_a_test_vector() {
        let vectors = manifest()["test_vectors"].clone();
        let vectors = vectors.as_array().expect("an array");
        let vector_tools: std::collections::BTreeSet<&str> =
            vectors.iter().map(|v| v["tool"].as_str().expect("a tool name")).collect();
        assert_eq!(
            vector_tools,
            std::collections::BTreeSet::from(["decode", "encode", "fragment_search"]),
            "every declared tool should have at least one test vector"
        );
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
}
