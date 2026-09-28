//! Constrained scalars: untrusted JSON becomes one of these exactly once, at
//! deserialization, and an invalid value cannot be constructed at all. Each
//! type's `JsonSchema` impl carries the same constraint the `TryFrom` enforces
//! — `pattern` for these string newtypes — so `json_validate` and an agent
//! reading the schema see the identical rule.

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A value that failed a newtype's format constraint.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid {type_name}: {value:?} does not match {pattern}")]
pub struct PatternError {
    pub type_name: &'static str,
    pub pattern: &'static str,
    pub value: String,
}

/// Defines a `String` newtype that validates against a fixed regex at
/// deserialization and publishes the same pattern in its JSON Schema.
macro_rules! pattern_newtype {
    (
        $(#[$meta:meta])*
        $name:ident, $pattern:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            fn regex() -> &'static Regex {
                static RE: OnceLock<Regex> = OnceLock::new();
                RE.get_or_init(|| Regex::new($pattern).expect("valid built-in pattern"))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = PatternError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if Self::regex().is_match(&value) {
                    Ok(Self(value))
                } else {
                    Err(PatternError {
                        type_name: stringify!($name),
                        pattern: $pattern,
                        value,
                    })
                }
            }
        }

        impl TryFrom<&str> for $name {
            type Error = PatternError;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_string())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                $name::try_from(raw).map_err(D::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn inline_schema() -> bool {
                true
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": $pattern,
                })
            }
        }
    };
}

pattern_newtype!(
    /// JCR-safe node name for the form's page and DAM asset (AEM.md §2, §9).
    FormName,
    r"^[A-Za-z][A-Za-z0-9_-]{0,63}$"
);

pattern_newtype!(
    /// A component's logical `name` (AEM.md §6.1), unique within the form —
    /// enforced by [`crate::model::validate`], since uniqueness is a
    /// whole-tree property no single node's type can see.
    ComponentName,
    r"^[A-Za-z][A-Za-z0-9_]{0,63}$"
);

pattern_newtype!(
    /// A JCR path into the fragment library (AEM.md §6.12 `fragRef`).
    FragmentRef,
    r"^/content/dam/formsanddocuments/[A-Za-z0-9_./-]+$"
);

pattern_newtype!(
    /// An XML NCName, used as an XSD root element name (AEM.md §19.1).
    XmlName,
    r"^[A-Za-z_][A-Za-z0-9_.-]*$"
);

pattern_newtype!(
    /// A JCR element name (`Common.jcr_name`) or a generic node's own
    /// property key (`Node::Component`'s `properties` map). Slightly more
    /// permissive than [`ComponentName`] (colons for a namespace prefix,
    /// no length cap) since a real package's own element and property
    /// names are not bound by this model's own `name` convention the way
    /// an agent-authored `ComponentName` is.
    JcrName,
    r"^[A-Za-z_][A-Za-z0-9_.:-]*$"
);

pattern_newtype!(
    /// A JCR `sling:resourceType` value (AEM.md §6 component table): a
    /// project-relative or absolute path, e.g.
    /// `fd/af/components/controls/textbox` or
    /// `/apps/some-customer/components/pages/aftemplatedpage`. The mapper never
    /// chooses one -- see `u2s-mapper-aem`'s module doc on why every
    /// node states its own resource type rather than the encoder looking
    /// one up from a "kind".
    ResourceType,
    r"^/?[A-Za-z0-9_][A-Za-z0-9_./-]*$"
);

pattern_newtype!(
    /// A choice component's submitted value (AEM.md §6.6 `options`). Commas
    /// and backslashes are excluded because the encoder packs option lists
    /// into a single JCR attribute using them as separators (AEM.md §13).
    OptionValue,
    r"^[^,\\\r\n]+$"
);

pattern_newtype!(
    /// One CSS class in [`CssClasses`].
    CssClass,
    r"^-?[A-Za-z_][A-Za-z0-9_-]*$"
);

pattern_newtype!(
    /// A date picture-clause symbol string (AEM.md §18.1). This validates the
    /// symbol alphabet only, not full grammar correctness — semantic
    /// exotica is an encoder concern.
    DatePattern,
    r"^[DMYE .,/:-]+$"
);

pattern_newtype!(
    /// A number picture-clause symbol string (AEM.md §18.2), alphabet-only
    /// like [`DatePattern`].
    NumberPattern,
    r"^[9ZzE.,$%Ss()CRcr-]+$"
);

pattern_newtype!(
    /// A text picture-clause symbol string (AEM.md §18.3), alphabet-only
    /// like [`DatePattern`].
    TextPattern,
    r"^[AXO09 .,-]+$"
);

/// A 1..=12 column span in AEM's responsive grid (AEM.md §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColSpan(u8);

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("column span must be between 1 and 12, got {0}")]
pub struct ColSpanError(u8);

impl ColSpan {
    pub const FULL: ColSpan = ColSpan(12);

    pub fn value(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ColSpan {
    type Error = ColSpanError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if (1..=12).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ColSpanError(value))
        }
    }
}

impl Serialize for ColSpan {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for ColSpan {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = u8::deserialize(deserializer)?;
        ColSpan::try_from(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for ColSpan {
    fn schema_name() -> Cow<'static, str> {
        "ColSpan".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 1,
            "maximum": 12,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_name_rejects_leading_digit() {
        assert!(ComponentName::try_from("1Field").is_err());
        assert!(ComponentName::try_from("Field1").is_ok());
    }

    #[test]
    fn component_name_rejects_empty() {
        assert!(ComponentName::try_from("").is_err());
    }

    #[test]
    fn option_value_rejects_comma_and_backslash() {
        assert!(OptionValue::try_from("a,b").is_err());
        assert!(OptionValue::try_from("a\\b").is_err());
        assert!(OptionValue::try_from("a-b").is_ok());
    }

    #[test]
    fn colspan_rejects_out_of_range() {
        assert!(ColSpan::try_from(0).is_err());
        assert!(ColSpan::try_from(13).is_err());
        assert!(ColSpan::try_from(12).is_ok());
        assert!(ColSpan::try_from(1).is_ok());
    }

    #[test]
    fn fragment_ref_requires_dam_prefix() {
        assert!(FragmentRef::try_from("/content/dam/formsanddocuments/x").is_ok());
        assert!(FragmentRef::try_from("/content/other/x").is_err());
    }
}
