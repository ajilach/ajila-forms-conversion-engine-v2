//! Constrained scalars: untrusted JSON becomes one of these exactly once, at
//! deserialization, and an invalid value cannot be constructed at all. Each
//! type's `JsonSchema` impl carries the same constraint the `TryFrom`
//! enforces — `pattern` for these string newtypes — so `json_validate` and
//! an agent reading the schema see the identical rule.
//!
//! Every length bound here mirrors a real column width in the platform's
//! own DDL (`ajila-redacto-migration/sql/V1__baseline_schema.sql`), pushed
//! into the type so an over-long value fails at deserialization rather than
//! at `psql` time: `documents.document_id`/`assets.asset_id` are
//! `varchar(50)`, `documents.form_path` is `varchar(200)`,
//! `ownerships.owner_id` is `varchar(50)`, `asset_version.language` is
//! `varchar(20)`.

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
    /// The document's business identifier, e.g. `aaev_019`. Mirrors
    /// `documents.document_id varchar(50)` -- a naming convention such as
    /// `<form code lowercase>_<entity>` is a profile choice, not something
    /// this generic model enforces. Written into `configuration.document.id`
    /// verbatim.
    DocumentId,
    r"^[a-z0-9][a-z0-9_-]{0,49}$"
);

pattern_newtype!(
    /// An agent-chosen, form-local stable name for an [`super::Asset`],
    /// e.g. `intro` or `footer`. This is the load-bearing difference from
    /// the platform's own `assets.asset_id`: the encoder mints that UUID
    /// (and the technical `assets.id` primary key) from this key, so the
    /// document JSON never carries either UUID and a key/PK mix-up --
    /// the single easiest mistake to make against this format -- cannot be
    /// authored in the first place.
    AssetKey,
    r"^[A-Za-z][A-Za-z0-9_-]{0,49}$"
);

pattern_newtype!(
    /// The AEM authoring path of the document (`documents.form_path
    /// varchar(200)`), e.g. `/content/forms/af/redacto-documents/aaev_019`.
    FormPath,
    r"^/[A-Za-z0-9_./-]{0,199}$"
);

pattern_newtype!(
    /// The authoring user recorded as the document owner
    /// (`ownerships.owner_id varchar(50)`), e.g. `admin`. Redacto rejects
    /// every authoring write against a document with no
    /// `(owner_id, USER, OWNER, document_id, DOCUMENT)` ownership row --
    /// see `u2s-mapper-redacto`, which derives that row mechanically from
    /// this value.
    OwnerId,
    r"^[A-Za-z0-9_-]{1,50}$"
);

pattern_newtype!(
    /// One or more space-separated CSS classes applied to a `styledPanel`
    /// (never an `assetContainer` -- the platform silently drops a `style`
    /// there), e.g. `layout-split-block` or `tab-box border-box`.
    PanelStyle,
    r"^[A-Za-z][A-Za-z0-9-]*(?: [A-Za-z][A-Za-z0-9-]*)*$"
);

pattern_newtype!(
    /// A stylesheet file name resolved from the Redacto bundle
    /// (`styles/<name>`), e.g. `default.css`.
    StyleName,
    r"^[A-Za-z0-9_-]+\.css$"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_id_rejects_uppercase_and_leading_hyphen() {
        assert!(DocumentId::try_from("aaev_019").is_ok());
        assert!(DocumentId::try_from("AAEV_019").is_err());
        assert!(DocumentId::try_from("-aaev").is_err());
    }

    #[test]
    fn document_id_rejects_over_fifty_chars() {
        let too_long = "a".repeat(51);
        assert!(DocumentId::try_from(too_long.as_str()).is_err());
        let exactly_fifty = "a".repeat(50);
        assert!(DocumentId::try_from(exactly_fifty.as_str()).is_ok());
    }

    #[test]
    fn asset_key_requires_leading_letter() {
        assert!(AssetKey::try_from("intro").is_ok());
        assert!(AssetKey::try_from("1intro").is_err());
        assert!(AssetKey::try_from("").is_err());
    }

    #[test]
    fn form_path_rejects_over_two_hundred_chars() {
        let path = format!("/{}", "a".repeat(199));
        assert!(FormPath::try_from(path.as_str()).is_ok());
        let too_long = format!("/{}", "a".repeat(200));
        assert!(FormPath::try_from(too_long.as_str()).is_err());
    }

    #[test]
    fn panel_style_accepts_multiple_classes() {
        assert!(PanelStyle::try_from("layout-split").is_ok());
        assert!(PanelStyle::try_from("tab-box border-box margin-bottom").is_ok());
        assert!(PanelStyle::try_from("").is_err());
        assert!(PanelStyle::try_from(" leading-space").is_err());
    }

    #[test]
    fn style_name_requires_css_extension() {
        assert!(StyleName::try_from("default.css").is_ok());
        assert!(StyleName::try_from("default").is_err());
    }
}
