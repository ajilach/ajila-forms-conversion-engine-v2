//! [`TagName`], the one place untrusted tag text becomes a value that
//! cannot hold an invalid name, and [`RuleSelection`], which rules a tag
//! leaves enabled for the inputs it is on.
//!
//! The name constraints match the `tags` table's CHECK constraints (see
//! `migrations/20261016090000_input_tags.sql`); keep the two in sync.
//! Uniqueness (per dataset, ignoring case) is the database's alone: it
//! needs the other rows.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::RuleId;

/// The longest tag name, in characters.
pub const MAX_TAG_NAME_CHARS: usize = 64;

/// A tag name that failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagNameError {
    #[error("a tag name must not be empty")]
    Empty,
    #[error("a tag name must be at most {MAX_TAG_NAME_CHARS} characters, not {0}")]
    TooLong(usize),
    #[error("a tag name must not contain control characters")]
    ControlCharacter,
}

/// A tag's display name: trimmed, non-empty, at most
/// [`MAX_TAG_NAME_CHARS`] characters, no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagName(String);

impl TagName {
    /// Surrounding whitespace is dropped rather than refused: a form field
    /// with a trailing space means the same name.
    pub fn parse(value: &str) -> Result<Self, TagNameError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(TagNameError::Empty);
        }
        let chars = trimmed.chars().count();
        if chars > MAX_TAG_NAME_CHARS {
            return Err(TagNameError::TooLong(chars));
        }
        if trimmed.chars().any(char::is_control) {
            return Err(TagNameError::ControlCharacter);
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TagName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for TagName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TagName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(D::Error::custom)
    }
}

/// Which rules a tag leaves enabled for the inputs it is on.
///
/// An input's enabled rules are the intersection over its `Only` tags; an
/// input with no `Only` tag has every rule enabled. The database function
/// `rule_enabled_for_input` is the one implementation of that rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RuleSelection {
    /// Every rule, including ones created later. The default.
    All,
    /// Only these rules; every other rule, including ones created later, is
    /// disabled. An empty set disables every rule.
    Only { rule_ids: BTreeSet<RuleId> },
}

impl RuleSelection {
    /// The `tags.rule_mode` value.
    pub fn mode_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Only { .. } => "only",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_accepts_a_plain_name() {
        assert_eq!(TagName::parse("  Batch 7 ").unwrap().as_str(), "Batch 7");
    }

    #[test]
    fn refuses_blank_long_and_control() {
        assert_eq!(TagName::parse("   "), Err(TagNameError::Empty));
        assert_eq!(
            TagName::parse(&"x".repeat(65)),
            Err(TagNameError::TooLong(65))
        );
        assert!(TagName::parse(&"ä".repeat(64)).is_ok());
        assert_eq!(TagName::parse("a\tb"), Err(TagNameError::ControlCharacter));
    }

    #[test]
    fn rule_selection_round_trips_as_tagged_json() {
        let all: RuleSelection = serde_json::from_str(r#"{"mode":"all"}"#).unwrap();
        assert_eq!(all, RuleSelection::All);
        let id = RuleId::new(uuid::Uuid::nil());
        let only: RuleSelection =
            serde_json::from_value(serde_json::json!({"mode": "only", "rule_ids": [id]})).unwrap();
        assert_eq!(
            only,
            RuleSelection::Only {
                rule_ids: BTreeSet::from([id])
            }
        );
        assert!(serde_json::from_str::<RuleSelection>(r#"{"mode":"only"}"#).is_err());
        assert!(serde_json::from_str::<RuleSelection>(r#"{"mode":"some"}"#).is_err());
    }
}
