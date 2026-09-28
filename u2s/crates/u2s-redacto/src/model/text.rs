//! Language-keyed asset content. Redacto stores one `asset_version` row per
//! asset *per language*, so a document JSON's own map of language code to
//! HTML fragment is exactly that table's shape, one asset at a time.
//!
//! `Language` here is a deliberate copy of `u2s-aem::model::text::Language`,
//! not a shared dependency: this crate must not link `u2s-aem` (a Redacto
//! document is not an AEM one), and the two formats' language codes happen
//! to follow the same convention only because both ultimately come from the
//! same XFA-sourced locale strings upstream -- a coincidence, not a shared
//! contract worth coupling two format crates over.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::newtypes::PatternError;

/// ISO 639-1 language code, optionally with a region subtag (`de`, `de-CH`).
/// `asset_version.language`/`document_version.language` is the Java
/// `Locale.toString()` form, `varchar(20)` -- generous enough that this
/// pattern's own bound is never the binding constraint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Language(String);

const LANGUAGE_PATTERN: &str = r"^[a-z]{2}(-[A-Za-z]{2})?$";

impl Language {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(LANGUAGE_PATTERN).expect("valid built-in pattern"))
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Language {
    type Error = PatternError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if Self::regex().is_match(&value) {
            Ok(Self(value))
        } else {
            Err(PatternError {
                type_name: "Language",
                pattern: LANGUAGE_PATTERN,
                value,
            })
        }
    }
}

impl TryFrom<&str> for Language {
    type Error = PatternError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_string())
    }
}

impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Language::try_from(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for Language {
    fn schema_name() -> Cow<'static, str> {
        "Language".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": LANGUAGE_PATTERN,
        })
    }
}

/// One asset's rendered body for one language: Quill-flavoured HTML
/// (`<strong>`/`<em>`/`<sup>`, `<p>`, headings, lists, tables, `<a>`,
/// `<span>`, `<br>`, `<img>`, `<div>`) injected verbatim by the platform via
/// `th:utext` -- this type guarantees only that content is present and
/// non-blank. Well-formedness over the allowed tag set, heading placement
/// and image `alt` text are whole-document concerns checked by
/// [`super::validate`], not by this type, because whether a heading is
/// allowed depends on which slot the asset is referenced from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlFragment(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("asset content must not be empty or whitespace-only")]
pub struct HtmlFragmentError;

impl HtmlFragment {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HtmlFragment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for HtmlFragment {
    type Error = HtmlFragmentError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err(HtmlFragmentError);
        }
        Ok(Self(value))
    }
}

impl Serialize for HtmlFragment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for HtmlFragment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        HtmlFragment::try_from(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for HtmlFragment {
    fn schema_name() -> Cow<'static, str> {
        "HtmlFragment".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
        })
    }
}

/// An [`Asset`](super::Asset)'s content, one HTML fragment per language.
/// `Language`'s `JsonSchema` carries a `pattern`, so schemars' `BTreeMap`
/// impl renders this as `patternProperties` with
/// `additionalProperties: false` -- a closed map keyed by language code, not
/// a free-form object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct I18nHtml(BTreeMap<Language, HtmlFragment>);

impl I18nHtml {
    pub fn single(language: Language, content: HtmlFragment) -> Self {
        Self(BTreeMap::from([(language, content)]))
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (Language, HtmlFragment)>) -> Self {
        Self(entries.into_iter().collect())
    }

    pub fn languages(&self) -> impl Iterator<Item = &Language> {
        self.0.keys()
    }

    pub fn get(&self, language: &Language) -> Option<&HtmlFragment> {
        self.0.get(language)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Language, &HtmlFragment)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_accepts_bare_and_regional_codes() {
        assert!(Language::try_from("en").is_ok());
        assert!(Language::try_from("de-CH").is_ok());
        assert!(Language::try_from("english").is_err());
    }

    #[test]
    fn html_fragment_rejects_blank_content() {
        assert!(HtmlFragment::try_from(String::from("   ")).is_err());
        assert!(HtmlFragment::try_from(String::from("<p>x</p>")).is_ok());
    }
}
