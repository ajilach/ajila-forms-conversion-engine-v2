//! [`outline`] — one line per node: path, type, excerpt, flags. The first
//! thing an agent calls on a large document, since it costs a bounded
//! number of entries regardless of how big the real document is.

use jsonptr::{Pointer, PointerBuf, Token};
use serde_json::{Map, Value};

use crate::error::{JsonDocError, parse_pointer};
use serde::Serialize;

/// How long an excerpt is allowed to get before [`Flag::ExcerptTruncated`]
/// kicks in. Chosen so a line stays a line: long enough to be useful, short
/// enough that a hundred of them still fit in a small slice of context.
const MAX_EXCERPT_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum NodeKind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl NodeKind {
    fn of(value: &Value) -> Self {
        match value {
            Value::Object(_) => Self::Object,
            Value::Array(_) => Self::Array,
            Value::String(_) => Self::String,
            Value::Number(_) => Self::Number,
            Value::Bool(_) => Self::Bool,
            Value::Null => Self::Null,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Array => "array",
            Self::String => "string",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Null => "null",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Flag {
    /// This container has children that exist but were not emitted as their
    /// own entries because `depth` was reached first — re-outline from this
    /// pointer with a fresh `depth` to see them.
    ChildrenElidedByDepth,
    /// The excerpt itself was cut short of the real content.
    ExcerptTruncated,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutlineEntry {
    pub pointer: String,
    pub kind: NodeKind,
    pub excerpt: String,
    pub flags: Vec<Flag>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outline {
    pub entries: Vec<OutlineEntry>,
    /// `true` when `limit` capped the entry count — more siblings/children
    /// exist than were emitted, independent of `ChildrenElidedByDepth`
    /// (which is about depth, not count).
    pub truncated: bool,
}

/// Outlines the subtree at `pointer`: the node itself, then its descendants
/// up to `depth` levels below it, pre-order, capped at `limit` entries total.
///
/// `depth = 0` emits only the root entry. `limit` bounds the whole call's
/// cost regardless of how large the real subtree is — this is the guarantee
/// that makes `outline` safe to call on the document root.
pub fn outline(
    root: &Value,
    pointer: &str,
    depth: usize,
    limit: usize,
) -> Result<Outline, JsonDocError> {
    let ptr = parse_pointer(pointer)?;
    let start = resolve(root, ptr, pointer)?;

    let mut entries = Vec::new();
    let mut truncated = false;
    walk(
        start,
        ptr.to_buf(),
        0,
        depth,
        limit,
        &mut entries,
        &mut truncated,
    );

    Ok(Outline { entries, truncated })
}

fn resolve<'v>(root: &'v Value, ptr: &Pointer, raw: &str) -> Result<&'v Value, JsonDocError> {
    if ptr.is_root() {
        return Ok(root);
    }
    ptr.resolve(root).map_err(|_| JsonDocError::NotFound {
        pointer: raw.to_owned(),
    })
}

fn walk(
    value: &Value,
    at: PointerBuf,
    level: usize,
    depth: usize,
    limit: usize,
    entries: &mut Vec<OutlineEntry>,
    truncated: &mut bool,
) {
    if entries.len() >= limit {
        *truncated = true;
        return;
    }

    let (excerpt, excerpt_truncated) = excerpt_of(value);
    let mut flags = Vec::new();
    if excerpt_truncated {
        flags.push(Flag::ExcerptTruncated);
    }

    let has_children = matches!(value, Value::Object(m) if !m.is_empty())
        || matches!(value, Value::Array(a) if !a.is_empty());
    if has_children && level >= depth {
        flags.push(Flag::ChildrenElidedByDepth);
    }

    entries.push(OutlineEntry {
        pointer: at.to_string(),
        kind: NodeKind::of(value),
        excerpt,
        flags,
    });

    if level >= depth {
        return;
    }

    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if entries.len() >= limit {
                    *truncated = true;
                    return;
                }
                let mut child_ptr = at.clone();
                child_ptr.push_back(Token::new(key.as_str()));
                walk(
                    child,
                    child_ptr,
                    level + 1,
                    depth,
                    limit,
                    entries,
                    truncated,
                );
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                if entries.len() >= limit {
                    *truncated = true;
                    return;
                }
                let mut child_ptr = at.clone();
                child_ptr.push_back(Token::from(i));
                walk(
                    child,
                    child_ptr,
                    level + 1,
                    depth,
                    limit,
                    entries,
                    truncated,
                );
            }
        }
        _ => {}
    }
}

/// A one-line preview, by kind: an object lists its key names, an array
/// reports its length, a string previews its content, and a scalar is its
/// own excerpt — nothing to preview past the value itself.
fn excerpt_of(value: &Value) -> (String, bool) {
    match value {
        Value::Object(map) => object_excerpt(map),
        Value::Array(items) => (
            format!("[{} item{}]", items.len(), plural(items.len())),
            false,
        ),
        Value::String(s) => truncate(s),
        Value::Number(n) => (n.to_string(), false),
        Value::Bool(b) => (b.to_string(), false),
        Value::Null => ("null".to_owned(), false),
    }
}

fn object_excerpt(map: &Map<String, Value>) -> (String, bool) {
    if map.is_empty() {
        return ("{}".to_owned(), false);
    }
    let mut joined = String::from("{");
    let mut truncated = false;
    for (i, key) in map.keys().enumerate() {
        if i > 0 {
            joined.push_str(", ");
        }
        if joined.chars().count() + key.chars().count() > MAX_EXCERPT_CHARS {
            joined.push('…');
            truncated = true;
            break;
        }
        joined.push_str(key);
    }
    joined.push('}');
    (joined, truncated)
}

fn truncate(s: &str) -> (String, bool) {
    let char_count = s.chars().count();
    if char_count <= MAX_EXCERPT_CHARS {
        return (format!("{s:?}"), false);
    }
    let head: String = s.chars().take(MAX_EXCERPT_CHARS).collect();
    (format!("{head:?}…"), true)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outlines_the_root_object_one_level_deep() {
        let doc = json!({ "a": 1, "b": { "c": 2 } });
        let out = outline(&doc, "", 1, 100).unwrap();
        let pointers: Vec<&str> = out.entries.iter().map(|e| e.pointer.as_str()).collect();
        assert_eq!(pointers, vec!["", "/a", "/b"]);
        assert!(!out.truncated);
    }

    #[test]
    fn depth_zero_emits_only_the_root() {
        let doc = json!({ "a": 1 });
        let out = outline(&doc, "", 0, 100).unwrap();
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].pointer, "");
        assert!(out.entries[0].flags.contains(&Flag::ChildrenElidedByDepth));
    }

    #[test]
    fn a_container_beyond_depth_is_flagged_elided() {
        let doc = json!({ "a": { "b": { "c": 1 } } });
        let out = outline(&doc, "", 1, 100).unwrap();
        let a = out.entries.iter().find(|e| e.pointer == "/a").unwrap();
        assert!(a.flags.contains(&Flag::ChildrenElidedByDepth));
        // /a/b/c is two levels below root, past depth=1 — never emitted.
        assert!(!out.entries.iter().any(|e| e.pointer == "/a/b/c"));
    }

    #[test]
    fn a_leaf_beyond_depth_is_not_flagged_elided() {
        // depth=0 stops recursion, but a scalar has no children to elide —
        // outline the leaf itself directly, not its containing object (which
        // does have an elided child, covered by the test above).
        let doc = json!({ "a": 1 });
        let out = outline(&doc, "/a", 0, 100).unwrap();
        assert_eq!(out.entries[0].flags, vec![]);
    }

    #[test]
    fn the_limit_caps_entries_and_sets_truncated() {
        let doc = json!({ "a": 1, "b": 2, "c": 3, "d": 4 });
        let out = outline(&doc, "", 1, 2).unwrap();
        assert_eq!(out.entries.len(), 2);
        assert!(out.truncated);
    }

    #[test]
    fn array_indices_use_numeric_pointers() {
        let doc = json!({ "items": [10, 20] });
        let out = outline(&doc, "/items", 1, 100).unwrap();
        let pointers: Vec<&str> = out.entries.iter().map(|e| e.pointer.as_str()).collect();
        assert_eq!(pointers, vec!["/items", "/items/0", "/items/1"]);
    }

    #[test]
    fn a_key_containing_slash_and_tilde_round_trips_through_the_pointer() {
        let doc = json!({ "a/b~c": 1 });
        let out = outline(&doc, "", 1, 100).unwrap();
        let child = &out.entries[1];
        assert_eq!(child.pointer, "/a~1b~0c");
        // And it must resolve back to the same value.
        let resolved = jsonptr::Pointer::parse(&child.pointer)
            .unwrap()
            .resolve(&doc)
            .unwrap();
        assert_eq!(resolved, &json!(1));
    }

    #[test]
    fn outlining_a_missing_pointer_is_not_found() {
        let doc = json!({});
        assert!(matches!(
            outline(&doc, "/nope", 1, 100),
            Err(JsonDocError::NotFound { .. })
        ));
    }

    #[test]
    fn outlining_a_malformed_pointer_is_invalid_pointer() {
        let doc = json!({});
        assert!(matches!(
            outline(&doc, "no-leading-slash", 1, 100),
            Err(JsonDocError::InvalidPointer { .. })
        ));
    }

    #[test]
    fn object_excerpt_lists_key_names() {
        let doc = json!({ "obj": { "name": "x", "age": 1 } });
        let out = outline(&doc, "", 1, 100).unwrap();
        let obj = out.entries.iter().find(|e| e.pointer == "/obj").unwrap();
        // Key order is `Map`'s own iteration order (a `BTreeMap` in this
        // workspace — the `preserve_order` feature is off — so sorted, not
        // insertion order).
        assert_eq!(obj.excerpt, "{age, name}");
        assert_eq!(obj.kind, NodeKind::Object);
    }

    #[test]
    fn array_excerpt_reports_length_not_contents() {
        let doc = json!({ "items": [1, 2, 3] });
        let out = outline(&doc, "", 1, 100).unwrap();
        let items = out.entries.iter().find(|e| e.pointer == "/items").unwrap();
        assert_eq!(items.excerpt, "[3 items]");
    }

    #[test]
    fn a_long_string_excerpt_is_truncated_and_flagged() {
        let long = "x".repeat(200);
        let doc = json!({ "s": long });
        let out = outline(&doc, "", 1, 100).unwrap();
        let s = out.entries.iter().find(|e| e.pointer == "/s").unwrap();
        assert!(s.flags.contains(&Flag::ExcerptTruncated));
        assert!(s.excerpt.chars().count() < 200);
    }

    #[test]
    fn empty_object_and_array_have_no_children_to_elide() {
        // depth=0 stops recursion right at these two entries, so this only
        // tests something if they are actually reached — outline from one
        // level up so /obj and /arr are themselves emitted at depth 0.
        let doc = json!({ "obj": {}, "arr": [] });
        let out = outline(&doc, "", 1, 100).unwrap();
        let mut checked = 0;
        for e in &out.entries {
            if e.pointer == "/obj" || e.pointer == "/arr" {
                checked += 1;
                assert!(!e.flags.contains(&Flag::ChildrenElidedByDepth), "{e:?}");
            }
        }
        assert_eq!(checked, 2, "both entries must actually have been reached");
    }
}
