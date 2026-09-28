//! [`FactKey`]: the name a script reads a fact by (`ctx.facts.<key>`) and
//! declares it under (`const requires = ["<key>"]`).

use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Kept in sync with the `facts_key_shape` CHECK constraint in
/// `migrations/`. Lowercase snake case starting with a letter, so every key
/// is also a plain JavaScript identifier: `ctx.facts.source_fields` works
/// without bracket notation.
pub const PATTERN: &str = r"^[a-z][a-z0-9_]{0,62}$";

/// Keys that match [`PATTERN`] but name a property every JavaScript object
/// inherits. The other inherited names (`toString`, `__proto__`, ...) are
/// camel case or start with an underscore, so the pattern already refuses
/// them. `ctx.facts` is a `Proxy` that throws on an undeclared key; a fact
/// named `constructor` would make that check depend on how the proxy treats
/// inherited properties, so the name is refused outright. Also enforced by
/// the `facts_key_shape` CHECK constraint.
const RESERVED: [&str; 1] = ["constructor"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactKeyError {
    #[error("invalid fact key {value:?}: must match {PATTERN}")]
    Shape { value: String },
    #[error("invalid fact key {value:?}: reserved JavaScript property name")]
    Reserved { value: String },
}

/// A validated fact name. Constructed only by [`FactKey::parse`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKey(String);

impl FactKey {
    /// The one conversion point: untrusted text in, a valid key or an error.
    pub fn parse(value: impl Into<String>) -> Result<Self, FactKeyError> {
        let value = value.into();
        if !regex().is_match(&value) {
            return Err(FactKeyError::Shape { value });
        }
        if RESERVED.contains(&value.as_str()) {
            return Err(FactKeyError::Reserved { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(PATTERN).expect("valid built-in pattern"))
}

impl fmt::Display for FactKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for FactKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for FactKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FactKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_snake_case_identifiers() {
        for ok in ["a", "source_fields", "field_count_2", &format!("a{}", "b".repeat(62))] {
            assert!(FactKey::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "Source",
            "1field",
            "_field",
            "field-name",
            "field name",
            "feld_ä",
            &format!("a{}", "b".repeat(63)),
        ] {
            assert!(
                matches!(FactKey::parse(bad), Err(FactKeyError::Shape { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn rejects_inherited_object_property_names() {
        assert!(matches!(
            FactKey::parse("constructor"),
            Err(FactKeyError::Reserved { .. })
        ));
    }

    #[test]
    fn deserializing_validates() {
        assert!(serde_json::from_str::<FactKey>("\"ok_key\"").is_ok());
        assert!(serde_json::from_str::<FactKey>("\"Bad\"").is_err());
    }
}
