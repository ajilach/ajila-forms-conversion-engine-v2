//! The one mechanical text-match predicate shared by every browsable
//! catalogue this crate exposes to the Conversion Agent -- the stock
//! Foundation-component table ([`crate::catalog`]) and a deployment's own
//! fragment library ([`crate::fragment_library`]). Neither browses by
//! algorithm: this is a plain case-insensitive substring test, never a
//! ranking, so the agent still has to judge fit from what a hit actually
//! says.
//!
//! Both callers apply this the same way -- filter the haystacks a query
//! is checked against, then keep the entry if any haystack matches -- so
//! this module is the one place that logic exists rather than a copy per
//! catalogue.

/// Whether `query` matches `haystacks` -- case-insensitive substring, true
/// the moment any one haystack contains it. An empty or whitespace-only
/// query never matches anything here: callers that want "no query means
/// everything" (see [`crate::catalog::search`]) check for that case
/// themselves before ever calling this function, since a closed catalogue
/// and an open-ended library disagree on what "no query" should mean.
pub fn matches(query: &str, haystacks: &[&str]) -> bool {
    let needle = query.to_lowercase();
    if needle.trim().is_empty() {
        return false;
    }
    haystacks.iter().any(|h| h.to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_case_insensitively_across_any_haystack() {
        assert!(matches("panel", &["Panel Title", "something else"]));
        assert!(matches("PANEL", &["panel title"]));
        assert!(!matches("missing", &["panel title", "other text"]));
    }

    #[test]
    fn an_empty_or_blank_query_never_matches() {
        assert!(!matches("", &["anything"]));
        assert!(!matches("   ", &["anything"]));
    }
}
