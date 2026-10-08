//! JCR attribute-value formatting (AEM.md §13: type hints) and parsing, plus
//! the small constants every emitted file shares.
//!
//! Nothing in this module decides *whether* an attribute is written -- that
//! is each node writer's job, working from data `ValidForm` already
//! guarantees is present and correct. This module only knows how to spell a
//! value once the caller has decided to write it (and how to read one back),
//! so the same boolean or option list is never formatted two different ways
//! in two different writers, and the reader inverts exactly what the writer
//! wrote.
//!
//! - [`tree`] -- the untyped JCR node tree ([`tree::JcrNode`]) both
//!   directions serialize through: `xml_writer` builds one and serializes
//!   it, `decode` parses XML into one. Ported from
//!   `ajila-forms-conversion-engine/core/src/aem/parser.rs` -- see
//!   `PORTING.md`.
//! - [`value`] -- JCR list/escape handling (`\,`-aware), ported from the
//!   same source.

/// `{Boolean}true` / `{Boolean}false` (AEM.md §13). Used for `visible`,
/// `enabled`, and the `Presence` flags -- but deliberately **not** for
/// `mandatory`, which AEM.md §6.1 documents as a plain `"true"`/`"false"`
/// string with no type-hint prefix.
pub fn typed_bool(value: bool) -> &'static str {
    if value { "{Boolean}true" } else { "{Boolean}false" }
}

/// Plain `"true"`/`"false"`, for the handful of AEM attributes (`mandatory`
/// chief among them) that are documented as untyped strings rather than
/// `{Boolean}`-prefixed values.
pub fn plain_bool(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// AEM.md §13: a JCR multi-value property, `"[a,b,c]"`. Each element is
/// escaped (`\` → `\\`, `,` → `\,`) before joining, so an element
/// containing a literal comma -- an option label, say -- round-trips
/// through [`value::split_jcr_list`] rather than being silently split into
/// two elements. This was a latent bug until the real corpus (which itself
/// relies on the `\,` convention, see `value.rs`) settled the escaping this
/// function had never applied.
pub fn multi_value<I: IntoIterator<Item = S>, S: AsRef<str>>(items: I) -> String {
    let joined = items
        .into_iter()
        .map(|item| escape_jcr_list_item(item.as_ref()))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{joined}]")
}

fn escape_jcr_list_item(item: &str) -> String {
    item.replace('\\', "\\\\").replace(',', "\\,")
}

/// AEM.md §6.6/§6.7/§6.8 `options="[value=label,...]"`. The corpus resolves
/// the ambiguity a bare `=` join has by convention, and [`value::parse_options`]
/// is written to match: the decoder splits on the *first* `=`, so a label
/// may itself contain `=` but a value may not -- structural, not just a
/// convention, since [`u2s_aem::model::newtypes::OptionValue`]'s own pattern
/// excludes `=`.
pub fn option_pair(value: &str, label: &str) -> String {
    format!("{value}={label}")
}

pub mod tree;
pub mod value;

pub mod ns {
    pub const JCR: &str = "http://www.jcp.org/jcr/1.0";
    pub const SLING: &str = "http://sling.apache.org/jcr/sling/1.0";
    pub const CQ: &str = "http://www.day.com/jcr/cq/1.0";
    pub const NT: &str = "http://www.jcp.org/jcr/nt/1.0";
    pub const FD: &str = "http://www.adobe.com/aemfd/fd/1.0";
    pub const DAM: &str = "http://www.day.com/dam/1.0";
    pub const MIX: &str = "http://www.jcp.org/jcr/mix/1.0";
    // Jackrabbit-internal, not a real URI -- kept for completeness with
    // AEM.md §3's own namespace table even though nothing here emits
    // `rep:`-prefixed attributes yet.
    #[allow(dead_code)]
    pub const REP: &str = "internal";
    pub const VLT: &str = "http://www.day.com/jcr/vault/1.0";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_bool_uses_the_boolean_prefix() {
        assert_eq!(typed_bool(true), "{Boolean}true");
        assert_eq!(typed_bool(false), "{Boolean}false");
    }

    #[test]
    fn plain_bool_has_no_prefix() {
        assert_eq!(plain_bool(true), "true");
        assert_eq!(plain_bool(false), "false");
    }

    #[test]
    fn multi_value_joins_and_brackets() {
        assert_eq!(multi_value(["a", "b", "c"]), "[a,b,c]");
        assert_eq!(multi_value(Vec::<&str>::new()), "[]");
    }

    /// The bug this crate carried until the real corpus (see `value.rs`)
    /// proved the `\,` escaping convention: an element containing a comma
    /// or backslash must not be split by [`value::split_jcr_list`] on read.
    #[test]
    fn multi_value_escapes_commas_and_backslashes_in_elements() {
        assert_eq!(multi_value(["a,b", "c"]), "[a\\,b,c]");
        assert_eq!(multi_value(["a\\b"]), "[a\\\\b]");

        let joined = multi_value(["a,b", "c\\d"]);
        let inner = &joined[1..joined.len() - 1];
        assert_eq!(
            value::split_jcr_list(inner),
            vec!["a,b".to_owned(), "c\\d".to_owned()],
            "escape then split must round-trip the original elements"
        );
    }

    #[test]
    fn option_pair_joins_with_equals() {
        assert_eq!(option_pair("CH", "Switzerland"), "CH=Switzerland");
    }
}
