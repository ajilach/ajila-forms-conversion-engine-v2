//! AEM profile configuration loaded from a TOML file.
//!
//! Defines the schema for AEM output profiles and handles deserialization.
//! Profile values use [Tera](https://keats.github.io/tera/) template syntax
//! and can reference two namespaces:
//!
//! - `xfa.*`  — raw XFA `<variables><text>` values extracted from the PDF
//! - `variables.*` — user-defined intermediate values (themselves Tera templates)
//!
//! Runtime values injected automatically:
//! - `master_language` — primary language code (from profile)
//! - `languages` — comma-separated list of all detected language codes
//! - `author` — authoring user name

use serde::Deserialize;
use std::collections::HashMap;

/// A rule for replacing matched form elements with custom templates.
///
/// Each rule matches elements by label (for fields) or title (for panels)
/// using a regex pattern, and replaces them with the specified custom template.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomElementRule {
    /// Regex pattern matched against the element's label (for fields) or
    /// title (for panels). Uses Rust regex syntax.
    pub field_name: String,

    /// Name of the custom template (without `.xml` extension).
    /// Loaded from the `custom/` subdirectory of the profile.
    pub template: String,

    /// Optional target page index. When set, the custom element is moved
    /// to the specified page. 0 = first page, 1 = second page, -1 = last page,
    /// -2 = second-to-last, etc.
    pub page: Option<i32>,

    /// Names of other custom element templates this rule depends on.
    ///
    /// A custom element is only applied when every template listed here is
    /// also matched somewhere in the form. This prevents scripts/visibility
    /// rules in one template from referencing element names that another,
    /// missing template would have produced.
    ///
    /// Dependencies may be circular. A cycle is treated as all-or-nothing:
    /// every member of the cycle is applied only when all of them match the
    /// form, otherwise none of them are applied.
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// How to place this template when its own pattern matches nothing but
    /// every `depends_on` template does.
    ///
    /// Without this, a rule that finds no node is dropped, and — because the
    /// UBS clusters are circular — takes the rest of its cycle with it. That
    /// is right for a template that only ever rewrites an existing section,
    /// and wrong for one whose block a form is entitled to by virtue of what
    /// else it carries: an addressee-driven form has an account holder and
    /// signatures whether or not it spells those sections out as headings.
    ///
    /// `None` keeps the old behaviour (match or be dropped).
    #[serde(default)]
    pub inject: Option<InjectMode>,
}

/// Where an injected custom element goes, when [`CustomElementRule::inject`]
/// is set and the rule's pattern matched nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InjectMode {
    /// Append the block to the existing page named by `page` (the account
    /// holder belongs on the configurator's own step, not a step of its own).
    Append,
    /// Insert the block as a new wizard step at `page` (signatures are their
    /// own step).
    Step,
}

/// An AEM output profile loaded from a TOML file.
///
/// All template-typed fields accept Tera syntax. Non-template fields are
/// plain strings passed through as-is.
#[derive(Debug, Clone, Deserialize)]
pub struct AemProfile {
    /// Master / primary language code (e.g. `"en"`, `"de"`).
    /// Default: `"en"`.
    pub master_language: Option<String>,

    /// Tera template for the human-readable form title / form code.
    ///
    /// Required. Example: `"{{ xfa.formrange_code }}"`.
    pub title: String,

    /// Tera template for the JCR path segment between
    /// `content/forms/af/` and the form directory.
    pub form_path: Option<String>,

    /// Tera template for the JCR folder name
    /// (e.g. `"AF_{{ xfa.formrange_code }}"`).
    ///
    /// Required.
    pub form_dir: String,

    /// Tera template for the generated form XSD file path.
    ///
    /// This is a full JCR file path used in DAM metadata `xsdRef`
    /// (e.g. `"/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_{{ variables.form_code }}.xsd"`).
    ///
    /// Required when `bind_to_xsd = true`.
    pub xsd_path: Option<String>,

    /// Reusable intermediate variables. Each value is a Tera template that
    /// can reference `xfa.*` and previously resolved `variables.*`.
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
    /// something a template can compose. Absent, or absent for the master
    /// language, the button keeps its bare template label.
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

    /// Tera template that evaluates to a comma-separated list of fragment
    /// paths relative to `fragments/`. Each path can be:
    /// - A fragment library directory (scanned recursively): `"afforms_ubs_fragmentlib"`
    /// - A specific fragment: `"afforms_ubs_fragmentlib/affrg_Address1"`
    ///
    /// When set, only the listed paths are scanned.
    /// When absent, ALL subdirectories are scanned (backward-compatible default).
    ///
    /// Example using conditional logic:
    /// ```toml
    /// fragment_paths = """{% if xfa.formrange_entity == "019" %}afforms_ubs_fragmentlib,afforms_germany_fragmentlib{% elif xfa.formrange_entity == "033" %}afforms_ubs_fragmentlib,afforms_italy_fragmentlib{% else %}afforms_ubs_fragmentlib,afforms_ch_fragmentlib{% endif %}"""
    /// ```
    ///
    /// Example selecting specific fragments:
    /// ```toml
    /// fragment_paths = "afforms_ubs_fragmentlib/affrg_Address1,afforms_ubs_fragmentlib/affrg_IBAN1"
    /// ```
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

    /// Custom element replacement rules. Each rule matches form elements by
    /// label/title regex and replaces them with a custom template.
    #[serde(default)]
    pub custom_elements: Vec<CustomElementRule>,
}
