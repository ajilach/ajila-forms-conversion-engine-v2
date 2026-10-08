//! [`search`] — pointers and counts, **never bulk values**. A large document
//! can have thousands of matches; this returns where they are and a short
//! context snippet for each, never the matched value in full (that is what
//! [`crate::get`] is for, once the caller knows where to look).

use jsonptr::{PointerBuf, Token};
use serde::Serialize;
use serde_json::{Map, Value};
use u2s_core::text::window_chars;

use crate::error::{JsonDocError, parse_pointer};

/// How much of a matching string's surroundings to show — enough to tell
/// matches apart, short enough that a hundred results is still a small read.
const CONTEXT_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedIn {
    /// The match was in an object key at this pointer.
    Key,
    /// The match was in a string leaf's value.
    Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchMatch {
    /// Always a pointer to a **value** — RFC 6901 has no way to address a
    /// key on its own. For a [`MatchedIn::Key`] match, this is the pointer
    /// to the value stored *under* that key (the only addressable location
    /// co-located with it), not a pointer "to the key" — `get`ting this
    /// pointer immediately shows the matching key's own value.
    pub pointer: String,
    pub matched_in: MatchedIn,
    /// A short window around the match — never the whole value.
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    /// The true number of matches, even when `matches` was capped by
    /// `limit` — so a caller can tell "12 shown" from "12 total".
    pub total_count: usize,
    pub truncated: bool,
}

/// Case-insensitive substring search over object keys and string leaf
/// values in the subtree at `pointer`. Numbers, bools and null are not
/// searched — they have no meaningful "substring" — a caller comparing
/// against a scalar should read it directly via [`crate::get`].
pub fn search(
    root: &Value,
    pointer: &str,
    query: &str,
    limit: usize,
) -> Result<SearchResult, JsonDocError> {
    let ptr = parse_pointer(pointer)?;
    let start = if ptr.is_root() {
        root
    } else {
        ptr.resolve(root).map_err(|_| JsonDocError::NotFound {
            pointer: pointer.to_owned(),
        })?
    };

    let needle = query.to_lowercase();
    let mut matches = Vec::new();
    let mut total_count = 0;
    walk(
        start,
        ptr.to_buf(),
        &needle,
        limit,
        &mut matches,
        &mut total_count,
    );

    Ok(SearchResult {
        truncated: total_count > matches.len(),
        matches,
        total_count,
    })
}

fn walk(
    value: &Value,
    at: PointerBuf,
    needle: &str,
    limit: usize,
    out: &mut Vec<SearchMatch>,
    total: &mut usize,
) {
    match value {
        Value::Object(map) => walk_object(map, at, needle, limit, out, total),
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                let mut child_ptr = at.clone();
                child_ptr.push_back(Token::from(i));
                walk(child, child_ptr, needle, limit, out, total);
            }
        }
        Value::String(s) => {
            if let Some(pos) = s.to_lowercase().find(needle) {
                record(
                    &at,
                    MatchedIn::Value,
                    s,
                    pos,
                    needle.len(),
                    limit,
                    out,
                    total,
                );
            }
        }
        _ => {}
    }
}

fn walk_object(
    map: &Map<String, Value>,
    at: PointerBuf,
    needle: &str,
    limit: usize,
    out: &mut Vec<SearchMatch>,
    total: &mut usize,
) {
    for (key, child) in map {
        let mut child_ptr = at.clone();
        child_ptr.push_back(Token::new(key.as_str()));

        if let Some(pos) = key.to_lowercase().find(needle) {
            record(
                &child_ptr,
                MatchedIn::Key,
                key,
                pos,
                needle.len(),
                limit,
                out,
                total,
            );
        }
        walk(child, child_ptr, needle, limit, out, total);
    }
}

#[allow(clippy::too_many_arguments)]
fn record(
    at: &PointerBuf,
    matched_in: MatchedIn,
    text: &str,
    byte_pos: usize,
    needle_len: usize,
    limit: usize,
    out: &mut Vec<SearchMatch>,
    total: &mut usize,
) {
    *total += 1;
    if out.len() >= limit {
        return;
    }
    let char_pos = text[..byte_pos].chars().count();
    let context_start = char_pos.saturating_sub(CONTEXT_CHARS / 2);
    let windowed = window_chars(text, context_start, CONTEXT_CHARS.max(needle_len));
    out.push(SearchMatch {
        pointer: at.to_string(),
        matched_in,
        context: windowed.text,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_a_match_in_a_string_value() {
        let doc = json!({ "name": "Alpha Form" });
        let result = search(&doc, "", "form", 10).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pointer, "/name");
        assert_eq!(result.matches[0].matched_in, MatchedIn::Value);
        assert_eq!(result.total_count, 1);
        assert!(!result.truncated);
    }

    #[test]
    fn finds_a_match_in_an_object_key() {
        let doc = json!({ "customerName": "x" });
        let result = search(&doc, "", "customer", 10).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].matched_in, MatchedIn::Key);
        assert_eq!(result.matches[0].pointer, "/customerName");
    }

    #[test]
    fn search_is_case_insensitive() {
        let doc = json!({ "name": "ALPHA" });
        let result = search(&doc, "", "alpha", 10).unwrap();
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn numbers_bools_and_null_are_not_searched() {
        let doc = json!({ "n": 123, "b": true, "z": null });
        let result = search(&doc, "", "123", 10).unwrap();
        assert_eq!(result.matches.len(), 0);
    }

    #[test]
    fn total_count_exceeds_the_capped_matches_when_limited() {
        let doc = json!({ "a": "match", "b": "match", "c": "match" });
        let result = search(&doc, "", "match", 2).unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.total_count, 3);
        assert!(result.truncated);
    }

    #[test]
    fn context_never_returns_the_whole_value_for_a_long_string() {
        let long = "prefix ".repeat(50) + "needle" + &" suffix".repeat(50);
        let doc = json!({ "s": long.clone() });
        let result = search(&doc, "", "needle", 10).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].context.len() < long.len());
        assert!(result.matches[0].context.to_lowercase().contains("needle"));
    }

    #[test]
    fn search_can_be_scoped_to_a_subtree() {
        let doc = json!({ "a": { "match": 1 }, "b": { "other": "match" } });
        let result = search(&doc, "/a", "match", 10).unwrap();
        // Only "/a"'s own key matches; "/b/other"'s value is out of scope.
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pointer, "/a/match");
    }

    #[test]
    fn searching_a_missing_pointer_is_not_found() {
        let doc = json!({});
        assert!(matches!(
            search(&doc, "/nope", "x", 10),
            Err(JsonDocError::NotFound { .. })
        ));
    }
}
