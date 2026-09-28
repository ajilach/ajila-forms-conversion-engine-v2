//! [`DatasetSlug`]: the one place untrusted slug text (a path segment, a
//! Keycloak group-path component, a form field) becomes a value that cannot
//! hold anything but a valid slug. `u2s-auth` keys grants on this type and
//! `u2s-store` stores it in the immutable `datasets.slug` column — one
//! definition, shared, so the two can never validate a slug differently.
//!
//! The pattern matches the database's `CHECK` constraint on `datasets.slug`
//! exactly (see `migrations/`): lowercase alphanumeric segments joined by
//! single hyphens, 1 to 63 characters, RFC 1123 label shape. Keep the two in
//! sync if either changes. The shape itself lives in [`crate::label`],
//! shared with [`crate::FormatId`].

use std::borrow::Cow;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::label;

/// A slug that failed the shape constraint.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid dataset slug {value:?}: must match {}", label::PATTERN)]
pub struct SlugError {
    pub value: String,
}

/// An immutable, URL- and group-path-safe dataset identifier, distinct from
/// [`crate::ids::DatasetId`] (the store's opaque primary key). Two datasets
/// never share a slug (`datasets.slug` is `UNIQUE`), and a slug is never
/// reused after a dataset using it is deleted — reuse would let a stale
/// Keycloak group grant silently start meaning something else.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DatasetSlug(String);

impl DatasetSlug {
    /// The one conversion point: untrusted text in, a valid slug or an
    /// error out. Nothing else in the codebase should construct one.
    pub fn parse(value: impl Into<String>) -> Result<Self, SlugError> {
        let value = value.into();
        if label::matches(&value) {
            Ok(Self(value))
        } else {
            Err(SlugError { value })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DatasetSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for DatasetSlug {
    type Err = SlugError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for DatasetSlug {
    type Error = SlugError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for DatasetSlug {
    type Error = SlugError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl AsRef<str> for DatasetSlug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for DatasetSlug {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DatasetSlug {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Cow::<str>::deserialize(deserializer)?;
        DatasetSlug::parse(raw.into_owned()).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_normal_slug() {
        assert!(DatasetSlug::parse("demo").is_ok());
        assert!(DatasetSlug::parse("customer-alpha").is_ok());
        assert!(DatasetSlug::parse("a1-b2-c3").is_ok());
    }

    #[test]
    fn accepts_single_character() {
        assert!(DatasetSlug::parse("a").is_ok());
        assert!(DatasetSlug::parse("9").is_ok());
    }

    #[test]
    fn rejects_uppercase() {
        assert!(DatasetSlug::parse("Demo").is_err());
        assert!(DatasetSlug::parse("DEMO").is_err());
    }

    #[test]
    fn rejects_leading_or_trailing_hyphen() {
        assert!(DatasetSlug::parse("-demo").is_err());
        assert!(DatasetSlug::parse("demo-").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(DatasetSlug::parse("").is_err());
    }

    #[test]
    fn rejects_whitespace_and_slashes() {
        assert!(DatasetSlug::parse(" demo").is_err());
        assert!(DatasetSlug::parse("demo ").is_err());
        assert!(DatasetSlug::parse("de mo").is_err());
        assert!(DatasetSlug::parse("de/mo").is_err());
        assert!(DatasetSlug::parse("../demo").is_err());
    }

    #[test]
    fn rejects_underscore() {
        // Hyphen only, to match Keycloak group-path segments and DNS labels.
        assert!(DatasetSlug::parse("de_mo").is_err());
    }

    #[test]
    fn allows_internal_consecutive_hyphens() {
        // Matches the DB CHECK constraint exactly: only the first and last
        // character are restricted to alphanumeric.
        assert!(DatasetSlug::parse("a--b").is_ok());
    }

    #[test]
    fn rejects_over_63_chars() {
        let too_long = "a".repeat(64);
        assert!(DatasetSlug::parse(too_long).is_err());
        let max_len = "a".repeat(63);
        assert!(DatasetSlug::parse(max_len).is_ok());
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let slug = DatasetSlug::parse("demo").unwrap();
        assert_eq!(serde_json::to_string(&slug).unwrap(), "\"demo\"");
    }

    #[test]
    fn deserialize_rejects_invalid_value() {
        let result: Result<DatasetSlug, _> = serde_json::from_str("\"Not Valid\"");
        assert!(result.is_err());
    }

    #[test]
    fn ordering_is_lexicographic_for_btreemap_use() {
        let a = DatasetSlug::parse("alpha").unwrap();
        let b = DatasetSlug::parse("beta").unwrap();
        assert!(a < b);
    }
}
