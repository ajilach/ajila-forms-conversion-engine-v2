//! Human-authored text: plain labels/titles and rich (HTML) content, each
//! keyed by language. A missing translation is an absent key, never an empty
//! string, so [`I18nText`] and [`I18nRichText`] are maps rather than fixed
//! per-language fields — `AemForm::validate` checks the key set against
//! `metadata.languages`, since that is a whole-form property no single map
//! can see on its own.

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
/// AEM.md §10 delivers one translation dictionary per locale; this is the key
/// that selects one.
///
/// The region subtag's case is accepted as given, not normalised to BCP-47's
/// own canonical uppercase: a real customer package's own dictionary file is
/// named `de-ch.xml`, lowercase, and that filename is exactly this value
/// round-tripped -- normalising the case on decode would make `encode`
/// write a ZIP entry name the source never had, which the canonical
/// round-trip comparison (`u2s-mapper-aem::canonical`) treats as a
/// real mismatch (a missing/extra entry), not a cosmetic one.
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

/// Plain, non-empty text with no markup: `jcr:title`, field labels
/// (AEM.md §6.1 `jcr:title`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainText(String);

#[derive(Debug, Clone, thiserror::Error)]
pub enum PlainTextError {
    #[error("plain text must not be empty or whitespace-only")]
    Empty,
    #[error("plain text must not contain markup ('<' or '>'); use rich text instead")]
    ContainsMarkup,
}

impl PlainText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlainText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PlainText {
    type Error = PlainTextError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err(PlainTextError::Empty);
        }
        if value.contains('<') || value.contains('>') {
            return Err(PlainTextError::ContainsMarkup);
        }
        Ok(Self(value))
    }
}

impl Serialize for PlainText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PlainText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        PlainText::try_from(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for PlainText {
    fn schema_name() -> Cow<'static, str> {
        "PlainText".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "pattern": "^[^<>]*$",
        })
    }
}

/// Rich (HTML) text: `_value` content on static text (AEM.md §6.9). The
/// encoder is responsible for HTML safety at emission; this type guarantees
/// only that content is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichText(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("rich text must not be empty or whitespace-only")]
pub struct RichTextError;

impl RichText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RichText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RichText {
    type Error = RichTextError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err(RichTextError);
        }
        Ok(Self(value))
    }
}

impl Serialize for RichText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RichText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        RichText::try_from(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for RichText {
    fn schema_name() -> Cow<'static, str> {
        "RichText".into()
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

/// A plain-text value per language. `Language`'s `JsonSchema` carries a
/// `pattern`, so schemars' `BTreeMap` impl renders this as
/// `patternProperties` with `additionalProperties: false` — a closed map
/// keyed by language code, not a free-form object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct I18nText(BTreeMap<Language, PlainText>);

impl I18nText {
    pub fn single(language: Language, text: PlainText) -> Self {
        Self(BTreeMap::from([(language, text)]))
    }

    /// Builds a value from every language a decoder found -- the inline
    /// master-language value plus whatever a Sling dictionary supplied for
    /// every other configured language. `single`/`FromIterator` both exist
    /// because a decoder builds this incrementally (one language found at
    /// a time) while a hand-built fixture usually wants the one-language
    /// case directly.
    pub fn from_entries(entries: impl IntoIterator<Item = (Language, PlainText)>) -> Self {
        Self(entries.into_iter().collect())
    }

    pub fn languages(&self) -> impl Iterator<Item = &Language> {
        self.0.keys()
    }

    pub fn get(&self, language: &Language) -> Option<&PlainText> {
        self.0.get(language)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_accepts_bare_and_regional_codes_in_either_case() {
        assert!(Language::try_from("en").is_ok());
        assert!(Language::try_from("de-CH").is_ok());
        // A real customer package's own dictionary uses the lowercase spelling --
        // see this type's own doc on why the case is preserved, not
        // normalised.
        assert!(Language::try_from("de-ch").is_ok());
        assert!(Language::try_from("english").is_err());
    }

    #[test]
    fn plain_text_rejects_empty_and_markup() {
        assert!(PlainText::try_from(String::from("   ")).is_err());
        assert!(PlainText::try_from(String::from("<b>x</b>")).is_err());
        assert!(PlainText::try_from(String::from("plain")).is_ok());
    }

    #[test]
    fn rich_text_allows_markup_but_not_empty() {
        assert!(RichText::try_from(String::from("<p>x</p>")).is_ok());
        assert!(RichText::try_from(String::from("  ")).is_err());
    }
}

/// A rich-text value per language. See [`I18nText`] for the map's schema
/// shape.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct I18nRichText(BTreeMap<Language, RichText>);

impl I18nRichText {
    pub fn single(language: Language, text: RichText) -> Self {
        Self(BTreeMap::from([(language, text)]))
    }

    /// See [`I18nText::from_entries`] -- the same constructor for rich text.
    pub fn from_entries(entries: impl IntoIterator<Item = (Language, RichText)>) -> Self {
        Self(entries.into_iter().collect())
    }

    pub fn languages(&self) -> impl Iterator<Item = &Language> {
        self.0.keys()
    }

    pub fn get(&self, language: &Language) -> Option<&RichText> {
        self.0.get(language)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
