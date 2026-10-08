//! [`get`] — a windowed read of one subtree. Depth-capped structurally
//! (deep containers become a placeholder, not full text) and then
//! character-windowed as text (`u2s_core::text::window_chars`) — the two
//! caps compose, so a call can never return the whole document by accident
//! regardless of how it is misused.

use jsonptr::Pointer;
use serde::Serialize;
use serde_json::{Value, json};
use u2s_core::text::{Windowed, window_chars};

use crate::error::{JsonDocError, parse_pointer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GetResult {
    pub pointer: String,
    /// The depth-capped subtree, serialized to pretty JSON and then
    /// character-windowed.
    pub json: Windowed,
}

/// Reads the subtree at `pointer`, capped `depth` levels deep, windowed by
/// character `offset`/`limit` over its pretty-printed JSON text.
pub fn get(
    root: &Value,
    pointer: &str,
    depth: usize,
    offset: usize,
    limit: usize,
) -> Result<GetResult, JsonDocError> {
    let ptr = parse_pointer(pointer)?;
    let value = resolve(root, ptr, pointer)?;
    let capped = cap_depth(value, 0, depth);
    let text = serde_json::to_string_pretty(&capped).expect("a Value always serializes");
    Ok(GetResult {
        pointer: pointer.to_owned(),
        json: window_chars(&text, offset, limit),
    })
}

fn resolve<'v>(root: &'v Value, ptr: &Pointer, raw: &str) -> Result<&'v Value, JsonDocError> {
    if ptr.is_root() {
        return Ok(root);
    }
    ptr.resolve(root).map_err(|_| JsonDocError::NotFound {
        pointer: raw.to_owned(),
    })
}

/// Replaces a container beyond `depth` with a small marker object naming
/// what was elided — structurally distinguishable from real document
/// content (a real object never has an `"$elided"` key by construction of
/// this function alone, but see the module docs: this is a *reading* tool,
/// so a document that happens to contain that key elsewhere is not a
/// correctness problem, only a display quirk).
fn cap_depth(value: &Value, level: usize, depth: usize) -> Value {
    if level >= depth {
        return match value {
            Value::Object(map) if !map.is_empty() => elided_marker("object", map.len()),
            Value::Array(items) if !items.is_empty() => elided_marker("array", items.len()),
            other => other.clone(),
        };
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_depth(v, level + 1, depth)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| cap_depth(v, level + 1, depth))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn elided_marker(kind: &str, children: usize) -> Value {
    json!({ "$elided": true, "kind": kind, "children": children })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_shallow_document_reads_back_whole() {
        let doc = json!({ "a": 1, "b": "hi" });
        let out = get(&doc, "", 5, 0, 10_000).unwrap();
        assert!(!out.json.truncated);
        assert!(out.json.text.contains("\"a\": 1"));
    }

    #[test]
    fn a_container_beyond_depth_becomes_an_elided_marker() {
        let doc = json!({ "a": { "b": { "c": 1, "d": 2 } } });
        let out = get(&doc, "", 1, 0, 10_000).unwrap();
        assert!(out.json.text.contains("$elided"));
        assert!(!out.json.text.contains("\"c\""), "{}", out.json.text);
    }

    #[test]
    fn depth_zero_at_a_leaf_returns_the_leaf_unchanged() {
        let doc = json!({ "a": 1 });
        let out = get(&doc, "/a", 0, 0, 10_000).unwrap();
        assert_eq!(out.json.text.trim(), "1");
    }

    #[test]
    fn an_empty_container_is_not_elided_even_at_depth_zero() {
        let doc = json!({ "a": {} });
        let out = get(&doc, "/a", 0, 0, 10_000).unwrap();
        assert_eq!(out.json.text.trim(), "{}");
    }

    #[test]
    fn the_character_window_applies_to_the_serialized_text() {
        let doc = json!({ "a": "hello world" });
        let full = get(&doc, "", 5, 0, 10_000).unwrap();
        let windowed = get(&doc, "", 5, 0, 5).unwrap();
        assert!(windowed.json.truncated);
        assert_eq!(windowed.json.text.chars().count(), 5);
        assert!(full.json.text.starts_with(&windowed.json.text));
    }

    #[test]
    fn get_of_a_missing_pointer_is_not_found() {
        let doc = json!({});
        assert!(matches!(
            get(&doc, "/nope", 1, 0, 100),
            Err(JsonDocError::NotFound { .. })
        ));
    }
}
