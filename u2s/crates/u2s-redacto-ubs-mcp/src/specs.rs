//! The tool schemas and the manifest.
//!
//! Schemas are plain `serde_json::Value`, matching the convention
//! `u2s-aem-ubs-mcp`/`u2s-aem-mcp` already follow: decoupled from any
//! `schemars`/`rmcp` derive version, and readable in one place because a
//! tool description is prompt surface.

use serde_json::{Value, json};

/// The output format's key. Not bare `redacto`: the format server registers
/// a UBS profile, and a second Redacto profile must be able to register
/// beside this one rather than collide with it -- the same rationale
/// `u2s-aem-ubs-mcp`'s own `aem-ubs` key gives.
pub const FORMAT_KEY: &str = "redacto-ubs";

/// The MCP convention version this server speaks.
pub const CONTRACT_VERSION: &str = "1.0.0";

/// The format module's version. Bumped when the schema changes; because
/// `output_formats` is unique on `(key, version)` and snapshotting is
/// `ON CONFLICT DO NOTHING`, a re-registration of an unchanged server is
/// idempotent while a bumped version lands as a **new proposed** row under
/// existing runs rather than mutating one.
pub const FORMAT_VERSION: &str = "0.1.0";

pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "decode",
            "description":
                "Decode a real, delivered Redacto INSERT script back into a redacto-ubs JSON \
                 document. Exactly one of `artifact_blob` (a u2s blob handle) or `artifact_path` \
                 (a filesystem path, for conformance test vectors) must be given. Decode is \
                 lossless-or-error for content and structure: a dump this decoder cannot fully \
                 represent returns a tool error naming what could not be represented, never a \
                 partial document. Two things are deliberately NOT reproduced from the source -- \
                 named so they are not mistaken for data loss: `master_language` is not persisted \
                 anywhere in the platform's own row model (it is recomputed, preferring `en`), \
                 and every asset's own key is an invented label (the platform only persists a \
                 generated UUID, never an authored name), stable within this one decode but not \
                 across a further encode/decode round trip. The decoded document is returned as \
                 a blob reference, never inlined.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "artifact_blob": {
                        "type": "string",
                        "description": "A u2s blob handle naming the INSERT script to decode."
                    },
                    "artifact_path": {
                        "type": "string",
                        "description": "A filesystem path to the dump, for conformance test vectors."
                    },
                    "media_type": {
                        "type": "string",
                        "description": "A hint; ignored -- this decoder only ever reads UTF-8 SQL text."
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
                "Encode a finished redacto-ubs JSON document into the platform's own \
                 transactional INSERT script over the app_redacto schema. The document is \
                 validated first (a non-empty body, every declared language covered by every \
                 asset, every asset reference resolving, no heading in a furniture slot, every \
                 <img> carrying alt text) before encoding; a document that fails validation \
                 returns a tool error naming the JSON Pointer of each violation instead of a \
                 script. The script is returned as a blob reference (media type \
                 application/sql), never inlined.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "output_json": {
                        "type": "object",
                        "description": "A document conforming to the redacto-ubs JSON Schema."
                    }
                },
                "required": ["output_json"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "style_search",
            "description":
                "Search the published Redacto CSS class vocabulary and this deployment's own \
                 scanned stylesheets by keyword. Returns each matching class name, whether it \
                 comes from the platform's own fixed contract or a scanned tenant stylesheet, \
                 and a short preview of its declaration -- never a verdict on whether it fits. \
                 This is a mechanical text search, not a match by layout intent: judge fit \
                 yourself from the name and preview before referencing a result on a styledPanel \
                 or an asset. The published vocabulary (page-number, page-count, right, logo, \
                 preserve-spaces, redacto-reading-order, layout-split, layout-split-block, \
                 footnote, new-page) is always searchable, scanned directory configured or not.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords to search for in class names and declaration previews."
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
            "name": "u2s-redacto-ubs-mcp",
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
                "scope": { "output_formats": [FORMAT_KEY] }
            },
            {
                "tool": "style_search",
                "role": "query",
                "scope": { "output_formats": [FORMAT_KEY] }
            }
        ],
        "format": {
            "key": FORMAT_KEY,
            "version": FORMAT_VERSION,
            "description_md": description_md(),
            "json_schema": u2s_redacto::schema(),
        },
        "test_vectors": [
            {
                "tool": "decode",
                "args": { "artifact_path": "$FIXTURES/redacto-AAEV_019.sql" },
                "expect": { "structured": { "format_version": FORMAT_VERSION } }
            },
            {
                "tool": "encode",
                "args": { "output_json": minimal_document() },
                "expect": { "structured": { "blob": { "media_type": "application/sql" } } }
            },
            {
                "tool": "style_search",
                "args": { "query": "no class in this conformance run is named this" },
                "expect": { "structured": { "hits": [] } }
            }
        ],
    })
}

/// Real prompt surface: what a conversion targeting this format must
/// produce, and the UBS conventions that used to be a Tera-templated
/// profile in the reference converter and are now the agent's own call
/// (see this crate's own module doc).
fn description_md() -> String {
    "UBS Redacto documents. The delivered artefact is a transactional \
     Postgres INSERT script over the `app_redacto` schema (six tables: \
     assets, asset_version, documents, document_version, ownerships, \
     relations); this JSON is the working tree the encoder lowers into it. \
     The schema is generated from the typed model in `u2s-redacto`, which \
     is the only place that model is visible.\n\n\
     **Four page slots**: `firstHeader` (page one only), `header` (every \
     page, page one only when firstHeader is set), `body` (the content, \
     once, flowing across pages), `footer` (every page). Each is a list of \
     `assetContainer` (references named assets) and `styledPanel` \
     (a CSS-classed group of nested components).\n\n\
     **Asset content is Quill-flavoured HTML**: `<strong>`/`<em>`/`<sup>`, \
     `<p>`, headings, lists, tables, `<a>`, `<span>`, `<br>`, `<img>`, \
     `<div>`. A furniture slot (`firstHeader`/`header`/`footer`) must not \
     carry `h1`-`h3` headings. Every `<img>` needs a non-blank `alt`.\n\n\
     **UBS convention, not a platform rule -- author it yourself**: \
     `document_id` as `<form code lowercase>_<entity>` (e.g. `aaev_019`); \
     the header wrapped in `<div class=\"right preserve-spaces\">` so it \
     does not overlap the page logo; a footer built from seven \
     `<span class=\"footer-...\">` fields in source order (`footer-form-id`, \
     `footer-language`, `footer-version`, `footer-man-code`, \
     `footer-form-code`, `footer-release-date`, `footer-j-version`), ending \
     with `<span class=\"right\">Page <span class=\"page-number\"></span>/\
     <span class=\"page-count\"></span></span>`. `style_search` browses the \
     full published class vocabulary and any tenant stylesheet."
        .to_owned()
}

/// A minimal but real `redacto-ubs` document -- small enough to embed
/// inline in a test vector's own `args` (unlike `decode`'s own vector,
/// which needs a real dump and so reaches for `$FIXTURES` instead).
fn minimal_document() -> Value {
    json!({
        "metadata": {
            "document_id": "conformance_1",
            "title": "Conformance",
            "master_language": "en",
            "languages": ["en"],
            "owner_id": "admin"
        },
        "assets": [
            {
                "key": "intro",
                "kind": "text",
                "content": { "en": "<p>Hello</p>" }
            }
        ],
        "body": [
            { "type": "assetContainer", "assets": ["intro"] }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_version_is_valid_semver() {
        semver::Version::parse(FORMAT_VERSION).expect("the format version must be semver");
        semver::Version::parse(CONTRACT_VERSION).expect("the contract version must be semver");
    }

    #[test]
    fn the_manifest_carries_the_schema_from_u2s_redacto_verbatim() {
        let manifest = manifest();
        assert_eq!(
            manifest["format"]["json_schema"],
            u2s_redacto::schema(),
            "the schema must be u2s-redacto's own, not a copy that can drift"
        );
        assert_eq!(manifest["format"]["key"], FORMAT_KEY);
    }

    #[test]
    fn there_are_three_tools_claiming_the_output_side() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0]["name"], "decode");
        assert_eq!(specs[1]["name"], "encode");
        assert_eq!(specs[2]["name"], "style_search");

        let tools = manifest()["tools"].clone();
        assert_eq!(tools[0]["role"], "decode");
        assert_eq!(tools[1]["role"], "encode");
        assert_eq!(tools[2]["role"], "query");
        for tool in tools.as_array().expect("array") {
            assert_eq!(tool["scope"]["output_formats"][0], FORMAT_KEY);
        }
    }

    #[test]
    fn every_tool_has_a_test_vector() {
        let vectors = manifest()["test_vectors"].clone();
        let vectors = vectors.as_array().expect("an array");
        let vector_tools: std::collections::BTreeSet<&str> =
            vectors.iter().map(|v| v["tool"].as_str().expect("a tool name")).collect();
        assert_eq!(
            vector_tools,
            std::collections::BTreeSet::from(["decode", "encode", "style_search"]),
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

    #[test]
    fn the_minimal_document_is_a_real_valid_document() {
        let json = minimal_document();
        let doc = u2s_redacto::RedactoDocument::from_json(&json).expect("parses");
        doc.validate().expect("the conformance vector must itself be valid");
    }
}
