//! JCR value parsing: the inverse of [`super::multi_value`]/[`super::option_pair`].
//!
//! Ported from `ajila-forms-conversion-engine/core/src/aem/parser.rs`
//! (`split_jcr_list`, `jcr_unescape`, `parse_jcr_array`, `parse_bool_attr`,
//! `parse_visible`, `parse_options` -- see `PORTING.md`), adjusted to return
//! plain `(String, String)` pairs rather than the reference's own `AemOption`
//! type: this module knows nothing about `u2s_aem`, and lowering a raw pair
//! into a typed `OptionValue`/`PlainText` is `decode::form`'s job, not this
//! one's.
#![allow(
    dead_code,
    reason = "consumed once decode::form (Phase 4a) wires up the decode() entry point; \
              remove this line when it does"
)]

use super::tree::JcrNode;

/// `visible="{Boolean}false"` (or the untyped `"false"`) is hidden;
/// anything else, including absence, is visible. Matches
/// [`super::typed_bool`]'s own convention: the writer always emits the
/// `{Boolean}` prefix, but a real-world package may not.
pub fn parse_visible(node: &JcrNode) -> bool {
    match node.attr("visible") {
        Some(v) => !v.contains("false"),
        None => true,
    }
}

/// Parses a boolean attribute written as either `"true"`/`"false"` or
/// `"{Boolean}true"`/`"{Boolean}false"` -- this workspace's own encoder
/// writes both spellings depending on the attribute (see
/// [`super::typed_bool`] vs [`super::plain_bool`]), and a real package may
/// use either regardless of which this crate's writer would have chosen.
pub fn parse_bool_attr(node: &JcrNode, attr: &str) -> bool {
    match node.attr(attr) {
        Some(v) => v.contains("true"),
        None => false,
    }
}

/// Splits a JCR comma-separated string on unescaped commas, unescaping
/// `\,` → `,` and `\\` → `\` in the same pass.
///
/// Ported verbatim (`parser.rs:1678-1704` at the pinned commit). This is
/// the exact inverse of [`super::multi_value`]'s escaping.
// The nested `if let`/`if` below is clippy's `collapsible_if`, left exactly
// as upstream wrote it: this function is ported verbatim (see `PORTING.md`)
// specifically so a future re-sync has a small diff to reason about, and
// restructuring it for a style lint would defeat that.
#[allow(clippy::collapsible_if)]
pub fn split_jcr_list(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if next == ',' || next == '\\' {
                    current.push(next);
                    chars.next();
                    continue;
                }
            }
            current.push(ch);
        } else if ch == ',' {
            items.push(current);
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    items.push(current);
    // Filter out empty entries (matches the reference's own behaviour for
    // empty brackets, e.g. `"[]"`).
    items.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// Unescapes JCR backslash sequences (`\,` → `,`, `\\` → `\`) in a string
/// that is not itself a comma-list -- e.g. one half of an already-split
/// `options` entry. Ported verbatim (`parser.rs:1705-1725`).
// Same reasoning as `split_jcr_list`'s own `#[allow]`: ported verbatim.
#[allow(clippy::collapsible_if)]
pub fn jcr_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if next == ',' || next == '\\' {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parses a JCR multi-value array like `"[val1,val2,val3]"` into its
/// elements, unescaped. A bare (non-bracketed) value is treated as a
/// single-element list, matching the reference's own leniency. Ported
/// verbatim (`parser.rs:1726-1739`).
pub fn parse_jcr_array(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        split_jcr_list(&trimmed[1..trimmed.len() - 1])
            .into_iter()
            .map(|s| jcr_unescape(s.trim()))
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![trimmed.to_string()]
    }
}

/// Parses radio/checkbox/dropdown options from a JCR node into
/// `(value, label)` pairs, in source order.
///
/// AEM stores options three ways, tried in this order (ported from
/// `parser.rs:1615-1677`, the reference's own precedence):
///
/// 1. `enum`/`enumNames` — parallel arrays: `enum="[1,2]"
///    enumNames="[Individual,Legal Entity]"`.
/// 2. `options="[1=Individual,2=Legal Entity]"` — a single array of
///    `value=label` pairs. Split on the **first** `=` (a label may itself
///    contain `=`, a value may not -- [`super::option_pair`]'s own doc
///    explains why this is structural, not just convention).
/// 3. Child `<items>` elements, each an option of its own
///    (`value`/`jcr:title` or `value`/`text`).
///
/// Returns an empty vector, not an error, when none of the three shapes are
/// present -- a component with no options (most of them) is not malformed.
pub fn parse_options(node: &JcrNode) -> Vec<(String, String)> {
    let mut options = Vec::new();

    let enum_values = node.attr("enum").map(parse_jcr_array).unwrap_or_default();
    let enum_names = node
        .attr("enumNames")
        .map(parse_jcr_array)
        .unwrap_or_default();
    if !enum_values.is_empty() {
        for (i, value) in enum_values.iter().enumerate() {
            let label = enum_names.get(i).cloned().unwrap_or_else(|| value.clone());
            options.push((value.clone(), label));
        }
        return options;
    }

    if let Some(opts_str) = node.attr("options") {
        let trimmed = opts_str.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let inner = &trimmed[1..trimmed.len() - 1];
            for entry in split_jcr_list(inner) {
                let entry = entry.trim();
                if let Some((value, label)) = entry.split_once('=') {
                    options.push((jcr_unescape(value), jcr_unescape(label)));
                }
            }
            if !options.is_empty() {
                return options;
            }
        }
    }

    if let Some(items) = node.child("items") {
        for item in &items.children {
            let value = item.attr("value").unwrap_or("").to_string();
            let label = item
                .attr("jcr:title")
                .or_else(|| item.attr("text"))
                .unwrap_or(&value)
                .to_string();
            options.push((value, label));
        }
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_jcr_list_splits_on_unescaped_commas() {
        assert_eq!(
            split_jcr_list("a,b,c"),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn split_jcr_list_keeps_an_escaped_comma_inside_one_element() {
        assert_eq!(
            split_jcr_list("a\\,b,c"),
            vec!["a,b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn split_jcr_list_unescapes_a_literal_backslash() {
        assert_eq!(split_jcr_list("a\\\\b"), vec!["a\\b".to_owned()]);
    }

    #[test]
    fn split_jcr_list_drops_empty_entries() {
        assert_eq!(split_jcr_list(""), Vec::<String>::new());
        assert_eq!(split_jcr_list(",,"), Vec::<String>::new());
    }

    #[test]
    fn jcr_unescape_reverses_escaped_commas_and_backslashes() {
        assert_eq!(jcr_unescape("a\\,b"), "a,b");
        assert_eq!(jcr_unescape("a\\\\b"), "a\\b");
        assert_eq!(jcr_unescape("plain"), "plain");
    }

    #[test]
    fn parse_jcr_array_reads_a_bracketed_list() {
        assert_eq!(
            parse_jcr_array("[a,b,c]"),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn parse_jcr_array_treats_a_bare_value_as_one_element() {
        assert_eq!(parse_jcr_array("solo"), vec!["solo".to_owned()]);
    }

    #[test]
    fn parse_options_prefers_enum_over_options_over_items() {
        let node = JcrNode::leaf(
            "field",
            [
                ("enum", "[1,2]"),
                ("enumNames", "[One,Two]"),
                ("options", "[9=Nine]"),
            ],
        );
        assert_eq!(
            parse_options(&node),
            vec![
                ("1".to_owned(), "One".to_owned()),
                ("2".to_owned(), "Two".to_owned())
            ]
        );
    }

    #[test]
    fn parse_options_splits_the_options_attribute_on_the_first_equals() {
        let node = JcrNode::leaf("field", [("options", "[a=b=c,1=Individual]")]);
        assert_eq!(
            parse_options(&node),
            vec![
                ("a".to_owned(), "b=c".to_owned()),
                ("1".to_owned(), "Individual".to_owned())
            ]
        );
    }

    #[test]
    fn parse_options_reads_a_comma_escaped_label() {
        let node = JcrNode::leaf("field", [("options", "[1=Individual\\, Legal Entity]")]);
        assert_eq!(
            parse_options(&node),
            vec![("1".to_owned(), "Individual, Legal Entity".to_owned())]
        );
    }

    #[test]
    fn parse_options_falls_back_to_items_children() {
        let mut node = JcrNode::leaf("field", []);
        let mut items = JcrNode::leaf("items", []);
        items.children.push(JcrNode::leaf(
            "item_0",
            [("value", "1"), ("jcr:title", "One")],
        ));
        node.children.push(items);
        assert_eq!(parse_options(&node), vec![("1".to_owned(), "One".to_owned())]);
    }

    #[test]
    fn parse_options_is_empty_for_a_plain_field() {
        let node = JcrNode::leaf("field", [("name", "TF_Name")]);
        assert!(parse_options(&node).is_empty());
    }
}
