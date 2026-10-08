//! [`FormatId`] and [`FormatScope`] — PLAN.md's "one scoping mechanism for
//! rules and tools". A rule and an MCP tool both declare a `FormatScope` at
//! registration, stored identically (`output_formats text[]` on both
//! `rules` and `mcp_tools`, GIN-indexed) and matched by exactly one
//! function: [`matches`].
//!
//! There is deliberately no input side any more. A dataset fixes only its
//! output format; which MCP tool serves a given conversion is resolved on
//! demand from the files actually presented to it, not narrowed in advance
//! by a format a dataset or a rule declared it would accept. `FormatScope`
//! is therefore a set of output formats a rule or tool applies to, nothing
//! about source material.
//!
//! The single matcher is still the whole point. `u2s-store`'s pull query —
//! "which rules and tools apply to this run's output format?" — has to
//! implement the identical semantics in SQL, and `u2s-store`'s test suite
//! asserts the two agree over a table of cases rather than trusting them to
//! stay in sync by inspection. Anything that reads a manifest's `scope`
//! block (the three MCP servers already serve one) or a rule's declared
//! scope constructs a `FormatScope` through this type, never a raw string
//! vector.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::label;

/// A registered output-format key — `"aem"`, `"aem-ubs"`. Shares its shape
/// with [`crate::DatasetSlug`] (see [`crate::label`]) because both are short
/// lowercase-hyphenated tokens used as a lookup key, though the two are
/// never interchangeable: a `FormatId` names a format, never a dataset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FormatId(String);

/// A format key that failed the shape constraint.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid format id {value:?}: must match {}", label::PATTERN)]
pub struct FormatIdError {
    pub value: String,
}

impl FormatId {
    /// The one conversion point: untrusted text — a manifest's `scope`
    /// block, a rule-creation request — becomes a valid key or an error.
    pub fn parse(value: impl Into<String>) -> Result<Self, FormatIdError> {
        let value = value.into();
        if label::matches(&value) {
            Ok(Self(value))
        } else {
            Err(FormatIdError { value })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FormatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for FormatId {
    type Err = FormatIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for FormatId {
    type Error = FormatIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for FormatId {
    type Error = FormatIdError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl AsRef<str> for FormatId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Which output formats a rule or tool applies to. **Empty means "any"**: a
/// rule scoped `{ outputs: [] }` holds regardless of the output format in
/// play; one scoped `{ outputs: ["aem"] }` holds only for AEM. It is a set
/// because one rule or tool can legitimately serve several formats.
///
/// Kept as a named struct with one field, not a bare `Vec<FormatId>`: the
/// manifest's `scope` block is a JSON object (`{"output_formats": [...]}`),
/// so this type is what lets that stay an object rather than becoming a
/// bare array, and [`matches`] stays a named function rather than an
/// inlined `is_empty() || contains(...)` that a caller could drift from the
/// SQL mirror without anyone noticing.
///
/// Not deduplicated or sorted on construction: two rules with the same
/// logical scope but a different literal ordering are still logically
/// identical, and [`matches`] does not care about order. Serialized as a
/// plain array (a manifest's `scope.output_formats`), so preserving the
/// caller's order is also what keeps a round trip lossless for display
/// purposes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FormatScope {
    pub output_formats: Vec<FormatId>,
}

impl FormatScope {
    /// Matches every format — the scope every native JSON tool (outline,
    /// get, patch) declares, since editing the working document has nothing
    /// to do with which output format is in play.
    pub fn any() -> Self {
        Self::default()
    }

    /// Convenience for the common single-format case.
    pub fn only(output: FormatId) -> Self {
        Self {
            output_formats: vec![output],
        }
    }
}

/// **The one matcher.** A rule or tool is pulled for a run iff the declared
/// set is empty or contains the run's output format — see the module docs
/// for why this exact function, and only this function, may answer that
/// question.
pub fn matches(scope: &FormatScope, output: &FormatId) -> bool {
    side_matches(&scope.output_formats, output)
}

fn side_matches(side: &[FormatId], format: &FormatId) -> bool {
    side.is_empty() || side.contains(format)
}

/// Two scopes conflict — in the sense PLAN.md's rule-conflict diagnostic
/// needs — only when they could ever both apply to the same run, i.e. their
/// output coverage overlaps. Used to narrow "which other rules could this
/// one ever be compared against" before the expensive reference-run query
/// runs; the query itself is still the authority on whether they *actually*
/// disagree.
pub fn scopes_could_overlap(a: &FormatScope, b: &FormatScope) -> bool {
    sides_could_overlap(&a.output_formats, &b.output_formats)
}

fn sides_could_overlap(a: &[FormatId], b: &[FormatId]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    let a: BTreeSet<&FormatId> = a.iter().collect();
    b.iter().any(|f| a.contains(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> FormatId {
        FormatId::parse(s).expect("valid in tests")
    }

    #[test]
    fn empty_scope_matches_any_format() {
        let scope = FormatScope {
            output_formats: vec![],
        };
        assert!(matches(&scope, &fmt("aem")));
        assert!(matches(&scope, &fmt("other")));
    }

    #[test]
    fn a_declared_scope_matches_only_its_own_formats() {
        let scope = FormatScope::only(fmt("aem"));
        assert!(matches(&scope, &fmt("aem")));
        assert!(!matches(&scope, &fmt("other")));
    }

    #[test]
    fn any_matches_every_format() {
        assert!(matches(&FormatScope::any(), &fmt("aem")));
        assert!(matches(&FormatScope::any(), &fmt("y")));
    }

    #[test]
    fn a_multi_format_scope_matches_any_member() {
        let scope = FormatScope {
            output_formats: vec![fmt("aem"), fmt("aem-ubs")],
        };
        assert!(matches(&scope, &fmt("aem")));
        assert!(matches(&scope, &fmt("aem-ubs")));
        assert!(!matches(&scope, &fmt("other")));
    }

    #[test]
    fn overlap_is_symmetric_and_respects_any() {
        let aem_only = FormatScope {
            output_formats: vec![fmt("aem")],
        };
        let other_only = FormatScope {
            output_formats: vec![fmt("other")],
        };
        assert!(!scopes_could_overlap(&aem_only, &other_only));
        assert!(!scopes_could_overlap(&other_only, &aem_only));
        assert!(scopes_could_overlap(&aem_only, &FormatScope::any()));
        assert!(scopes_could_overlap(&FormatScope::any(), &aem_only));
    }

    #[test]
    fn format_id_shares_the_slug_shape_and_rejects_the_same_inputs() {
        assert!(FormatId::parse("aem-ubs").is_ok());
        assert!(FormatId::parse("PDF").is_err());
        assert!(FormatId::parse("").is_err());
    }

    #[test]
    fn format_scope_round_trips_through_json() {
        let scope = FormatScope::only(fmt("aem"));
        let json = serde_json::to_string(&scope).expect("serializes");
        assert_eq!(json, r#"{"output_formats":["aem"]}"#);
        let back: FormatScope = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, scope);
    }
}
