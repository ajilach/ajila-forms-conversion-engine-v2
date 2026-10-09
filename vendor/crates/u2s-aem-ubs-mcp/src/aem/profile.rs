//! AEM profile configuration loaded from a TOML file.
//!
//! Plain data: the profile's constants. What follows from a form's own XFA
//! variables (its code, folders and DoR templates) is
//! [`super::identity::FormIdentity`], in Rust.

use serde::Deserialize;
use std::collections::HashMap;

/// An AEM output profile loaded from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct AemProfile {
    /// Master / primary language code (e.g. `"en"`, `"de"`).
    /// Default: `"en"`.
    pub master_language: Option<String>,

    /// The DAM folder the form's schema lives in: the schema file is
    /// `<form code>.xsd` there. Required when `bind_to_xsd = true`.
    pub xsd_dir: Option<String>,

    /// The profile's constants the components write (resource types, CSS
    /// classes, validation clauses, ...), each as it is written between an
    /// attribute's quotes.
    #[serde(default)]
    pub variables: HashMap<String, String>,

    /// Language synonym mappings (e.g. `de = ["de-ch"]`).
    #[serde(default)]
    pub language_synonyms: HashMap<String, Vec<String>>,

    /// Per-language wording for a repeatable's Add button, as a pattern holding
    /// `{subject}` (e.g. `en = "Add {subject}"`, `de = "{subject} hinzufügen"`).
    ///
    /// A repeatable's Add button has to name what it adds, and the word order
    /// differs per language, so the phrasing is profile data rather than
    /// something the writer can compose. Absent, or absent for the master
    /// language, the button keeps its bare label.
    #[serde(default)]
    pub add_label_patterns: HashMap<String, String>,

    /// Language code -> the locale string the HTML component's `localeContent`
    /// items are keyed by (e.g. `en = "en"`, or `en = "en-us"`).
    ///
    /// Every other component emits its text once and lets the Sling dictionary
    /// translate it, so no such mapping was ever needed. The HTML component
    /// resolves the reader's locale against its own `localeContent` children
    /// instead, and which spelling it matches on is a property of the deployed
    /// component, not of this engine -- hence a profile table rather than a
    /// constant. A language with no entry here keeps its own code as the
    /// locale, which is also the shipped default.
    #[serde(default)]
    pub html_locales: HashMap<String, String>,

    /// When `true`, the generated AEM package will include the XSD schema and
    /// all form fields / panels will receive a `bindRef` attribute pointing to
    /// their corresponding XSD element path.
    ///
    /// Requires an XSD profile (`xsd/config.toml`) to be present alongside the
    /// AEM profile so that name resolution is consistent.  Default: `false`.
    pub bind_to_xsd: Option<bool>,

    /// When `true`, recursively scan the `fragments/` subdirectory of the AEM
    /// profile for fragment `.content.xml` files. Matched XSD types in the
    /// generated AEM node tree are replaced by fragment references.
    /// Default: `false`.
    pub use_fragments: Option<bool>,

    /// The DAM path prefix used in fragment `xsdRef` attributes
    /// (e.g. `"/content/dam/formsanddocuments/afforms_xsd/"`).
    /// This prefix is replaced with `xsd/types/` when resolving fragment
    /// XSD files locally.
    pub fragment_xsd_ref: Option<String>,

    /// JCR path prefix for constructing fragment `fragRef` values
    /// (e.g. `"/content/forms/af/"`).
    /// The `fragRef` is built as `{prefix}{relative_fragment_dir_path}`.
    pub fragment_ref_prefix: Option<String>,

    /// A comma-separated list of fragment paths relative to `fragments/`.
    /// Each path can be a fragment library directory (scanned recursively,
    /// `"afforms_ubs_fragmentlib"`) or a specific fragment
    /// (`"afforms_ubs_fragmentlib/affrg_Address1"`).
    ///
    /// When set, only the listed paths are scanned. When absent, every
    /// subdirectory is.
    pub fragment_paths: Option<String>,

    /// Default translations for predefined UI elements (toolbar buttons,
    /// message boxes, etc.) that are not part of the form content tree.
    ///
    /// Loaded from per-language TOML files in the `translations/` profile directory.
    /// Structure: `{ "master_text": { "lang": "translated_text", ... }, ... }`.
    ///
    /// These are merged into the Sling i18n dictionaries at package generation
    /// time. Form-content translations take precedence over defaults.
    #[serde(default)]
    pub default_translations: HashMap<String, HashMap<String, String>>,
}
