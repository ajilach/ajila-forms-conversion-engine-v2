//! The reference tools' specifications. Schemas are plain `serde_json::Value`,
//! as in the vendored u2s servers, so the tool descriptions (which are prompt
//! surface) stay visible in one file. Each tool's arguments are typed in
//! [`crate::args`]; a test holds the two together.
//!
//! The profile the references are scoped to is supplied by the host
//! ([`crate::ReferencesServer::new`]), never by the model, so it appears in no
//! schema.

use serde_json::{Value, json};

fn tool(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "input_schema": { "type": "object", "properties": properties, "required": required }
    })
}

pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "list_reference_forms",
            "List the profile's reference forms (hand-built, known-good worked examples). \
             Consult references BEFORE building: they show the expected JCR structure, \
             dictionary setup and DoR conventions for this profile's forms.",
            json!({}),
            json!([]),
        ),
        tool(
            "search_references",
            "Semantic search for precedent forms by MEANING, not by name. The query must be a \
             natural-language DESCRIPTION of the input you are building — the form's (or the \
             current section's) purpose, the kinds of fields it contains and how they are \
             grouped — NOT a form name or a single keyword. References are matched by embedding \
             this description against each reference's stored description (a literal substring \
             fallback over descriptions + package XML is folded in). Run this first (before \
             building), section by section; each hit carries a ref_id to pass to \
             get_reference_package / read_reference_file. Optional top_k caps hits per signal \
             (default 3).",
            json!({"query": {"type":"string"}, "top_k": {"type":"integer"}}),
            json!(["query"]),
        ),
        tool(
            "grep_references",
            "Literal/regex substring search over reference descriptions + AEM package XML — the \
             grep counterpart to search_references. Use it to find a specific string (a field \
             name, label, or AEM resource type) verbatim; use search_references when looking \
             for a form that resembles your input by meaning.",
            json!({"query": {"type":"string"}, "regex": {"type":"boolean"}}),
            json!(["query"]),
        ),
        tool(
            "read_reference_file",
            "Read a reference's description ('description') or a package file by path (get the \
             path from get_reference_package). Use it to study how a known-good form was built \
             and mirror its structure.",
            json!({"ref_id": {"type":"string"}, "path": {"type":"string"}, "offset": {"type":"integer"}, "limit": {"type":"integer"}}),
            json!(["ref_id", "path"]),
        ),
        tool(
            "get_reference_package",
            "List the package files (known-good output) of a reference by its ref_id (from \
             list_reference_forms / search_references), then read individual files with \
             read_reference_file.",
            json!({"ref_id": {"type":"string"}}),
            json!(["ref_id"]),
        ),
        tool(
            "list_reference_docs",
            "List the profile's reference documentation (.md/.txt).",
            json!({}),
            json!([]),
        ),
        tool(
            "read_reference_doc",
            "Read a reference documentation doc by id.",
            json!({"doc_id": {"type":"string"}, "offset": {"type":"integer"}, "limit": {"type":"integer"}}),
            json!(["doc_id"]),
        ),
        tool(
            "grep_reference_docs",
            "Regex/substring search over reference documentation.",
            json!({"query": {"type":"string"}, "regex": {"type":"boolean"}}),
            json!(["query"]),
        ),
    ]
}

/// Whether `name` is one of the reference tools.
pub fn is_reference_tool(name: &str) -> bool {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            tool_specs()
                .iter()
                .filter_map(|spec| spec["name"].as_str().map(str::to_string))
                .collect()
        })
        .iter()
        .any(|n| n == name)
}
