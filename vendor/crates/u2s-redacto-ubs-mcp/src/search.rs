//! The one mechanical text-match predicate `style_search` exposes --
//! case-insensitive substring, never a ranking, so the agent still has to
//! judge fit from what a hit actually says.
//!
//! A deliberate copy of `u2s-mapper-aem::search`'s own predicate, not a
//! shared dependency: this crate must not link an AEM crate for a
//! three-line function, the same genericity discipline that keeps
//! `u2s-server` off `u2s-aem`.

/// Whether `query` matches `haystacks` -- case-insensitive substring, true
/// the moment any one haystack contains it. An empty or whitespace-only
/// query never matches anything.
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
        assert!(matches("footnote", &["Footnote Panel", "something else"]));
        assert!(matches("FOOTNOTE", &["footnote panel"]));
        assert!(!matches("missing", &["footnote panel", "other text"]));
    }

    #[test]
    fn an_empty_or_blank_query_never_matches() {
        assert!(!matches("", &["anything"]));
        assert!(!matches("   ", &["anything"]));
    }
}
