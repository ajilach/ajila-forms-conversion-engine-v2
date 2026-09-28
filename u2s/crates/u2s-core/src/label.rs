//! One shape, shared by every lowercase-hyphenated identifier this crate
//! validates at its edge: [`crate::DatasetSlug`] and [`crate::FormatId`].
//!
//! Defined once so the two can never validate differently (CLAUDE.md: do not
//! duplicate logic) — a `DatasetSlug` is a Keycloak group-path segment, a
//! `FormatId` is a registry key used identically on `rules` and `mcp_tools`
//! (PLAN.md's `FormatScope`), and both need the same RFC 1123 label shape:
//! lowercase alphanumeric segments joined by single hyphens, 1 to 63
//! characters. Private to the crate — callers reach it only through the two
//! public newtypes, which is what keeps the pattern from being copy-pasted a
//! third time.

use std::sync::OnceLock;

use regex::Regex;

pub(crate) const PATTERN: &str = r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$";

pub(crate) fn matches(value: &str) -> bool {
    regex().is_match(value)
}

fn regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(PATTERN).expect("valid built-in pattern"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_rejects_the_documented_shape() {
        assert!(matches("demo"));
        assert!(matches("a--b"));
        assert!(matches("a"));
        assert!(!matches(""));
        assert!(!matches("Demo"));
        assert!(!matches("-demo"));
        assert!(!matches("demo-"));
        assert!(!matches("de_mo"));
        assert!(!matches("de mo"));
        assert!(!matches(&"a".repeat(64)));
    }
}
