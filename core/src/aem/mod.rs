//! AEM Forms XML Output Module
//!
//! Converts structured form nodes into Adobe Experience Manager (AEM) Adaptive
//! Forms JCR content XML. The module defines an intermediate `AemNode` tree
//! that captures the AEM-specific semantics, then serializes it to well-formed
//! XML using `quick-xml`.
//!
//! # Architecture
//!
//! ```text
//! StructuredNode ──► convert_to_aem() ──► AemNode tree ──► generate_aem_xml() ──► XML String
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use blueprint::aem::{AemConfig, convert_to_aem, generate_aem_xml};
//!
//! let config = AemConfig::new(&context)?;
//! let root = convert_to_aem(&structured_nodes, &config);
//! let xml = generate_aem_xml(&root, &config);
//! ```

mod converter;
pub mod fragment_parser;
mod package_writer;
pub mod normalize;
pub mod parser;
pub mod profile;
pub mod script_engine;
pub mod to_structured;
pub mod to_translated;
pub mod translated;
mod xml_writer;
pub mod xml_validation;

pub use crate::template;
pub use converter::convert_to_aem;
pub use fragment_parser::{ParsedFragment, parse_fragment_content, scan_fragments};
pub use package_writer::{
    aem_translations_from_content, collect_languages, generate_aem_package,
    generate_aem_package_from_node, generate_aem_package_from_node_with_passthrough,
    generate_aem_package_from_node_with_translations, generate_aem_package_from_node_with_xml,
};
pub use parser::{
    AemScript, ParsedAemPackage, TranslationData, VisibilityCondition, detect_aem_zip,
    parse_aem_zip,
};
pub use profile::{AemConnectionProfile, AemProfile, InjectMode};
pub use script_engine::AemScriptEngine;
pub use to_structured::aem_to_structured;
pub use to_translated::aem_to_translated;
pub use translated::{
    AemI18nText, AemNodeTranslated, AemOptionTranslated, I18nDict, LowerConflict,
    translation_data_from_master_dict,
};
pub use xml_validation::{
    validate_aem_dam_xml, validate_aem_form_xml, validate_xml_wellformed,
};
pub use xml_writer::{generate_aem_xml, generate_aem_xml_with_passthrough};

use regex_lite::Regex;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

use crate::structured::{FieldId, InputValue};
use crate::xsd::XsdConfig;

// ============================================================================
// Configuration
// ============================================================================

/// A compiled custom element rule ready for matching.
#[derive(Debug, Clone)]
pub struct ResolvedCustomElement {
    /// Compiled regex pattern for matching element labels/titles.
    pub pattern: Regex,
    /// Name of the custom template to use.
    pub template: String,
    /// Optional target page index for moving the element.
    pub page: Option<i32>,
    /// Templates this rule depends on; the rule is skipped unless every
    /// listed template is also matched somewhere in the form. Dependencies
    /// may be circular — in that case the whole cycle is added only when all
    /// of its members match, otherwise none of them are added.
    pub depends_on: Vec<String>,
    /// How to place this template when its pattern matched nothing but every
    /// dependency did. `None` means match or be dropped. See
    /// [`profile::InjectMode`].
    pub inject: Option<InjectMode>,
}

/// Configuration for AEM Forms XML generation.
///
/// Created from an [`AemProfile`] directory.  All template-rendered output
/// is driven by Tera template files loaded from the profile directory.
#[derive(Debug, Clone)]
pub struct AemConfig {
    // -- Form identity -------------------------------------------------------
    /// Human-readable form title (appears in `jcr:title`).
    pub form_title: String,

    /// Internal form code (used for metadata and paths).
    pub form_code: String,

    /// The issuing entity's mandator code from the source's
    /// `formrange_entity` (019 Germany, 033 Italy, 001 Switzerland), `None`
    /// when the source names none. The UBS runtime needs it as a `mandator=`
    /// query parameter on the rendered form: reference data lookups
    /// (`getFormMetadata().mandator`) and the submit/DoR backend resolve the
    /// entity from it, so a preview link without it can render but fail to
    /// submit.
    pub mandator: Option<String>,

    /// Available languages (ISO 639-1 codes, e.g. `["de", "en", "fr"]`).
    pub languages: Vec<String>,

    /// Master / primary language code.
    pub master_language: String,

    /// Per-language wording for a repeatable's Add button; the pattern holds
    /// `{subject}` (see [`AemProfile::add_label_patterns`]).
    pub add_label_patterns: HashMap<String, String>,

    /// Language synonyms: maps a base language code to additional codes that
    /// should receive the same translations (e.g. `"de" → ["de-ch"]`,
    /// `"sp" → ["es"]`).
    pub language_synonyms: HashMap<String, Vec<String>>,

    /// Language code -> the locale string the HTML component's `localeContent`
    /// items are keyed by (see [`AemProfile::html_locales`]).
    pub html_locales: HashMap<String, String>,

    // -- Authoring metadata --------------------------------------------------
    /// Value for `jcr:createdBy` / `jcr:lastModifiedBy`.
    /// Default: `"blueprint"`.
    pub author: String,

    // -- Operational ---------------------------------------------------------
    /// When `true`, UUIDs are derived deterministically from the node name
    /// (UUID v5 with a fixed namespace), making the output reproducible
    /// across runs.
    pub deterministic_uuids: bool,

    /// Total number of grid columns (used by the converter).  Default: `12`.
    pub grid_columns: u32,

    /// Default minimum occurrences for repeatable panels.  Default: `1`.
    pub repeatable_min_occur: u32,

    /// Default maximum occurrences for repeatable panels.  Default: `20`.
    pub repeatable_max_occur: u32,

    // -- Package paths -------------------------------------------------------
    /// JCR path segment between `content/forms/af/` and the form directory.
    pub form_path: String,

    /// JCR folder name for this form (from the profile's `form_dir` template).
    pub form_dir: String,

    /// JCR file path of the generated XSD used in DAM metadata `xsdRef`
    /// (e.g. `/content/dam/formsanddocuments/.../AF_AAAI.xsd`).
    /// `None` when the profile does not specify an `xsd_path`.
    pub xsd_path: Option<String>,

    // -- Package writer metadata (derived from profile variables) -------------
    /// DOR template reference path (from `variables.dor_template_ref`).
    pub dor_template_ref: String,

    /// DOR generation type.  Default: `"generate"`.
    pub dor_type: String,

    /// Path to the theme client library (from `variables.theme_ref`).
    pub theme_ref: String,

    /// The legal-entity line printed in the DoR's second header slot, derived
    /// from the source document's master-page header
    /// ([`Context::header`](crate::Context::header)) by [`header_slot_text`].
    /// `None` when the source has no header region, or nothing in it but a
    /// validity date.
    pub header_slot_text: Option<String>,

    // -- Template-based XML generation ---------------------------------------
    /// Tera template strings keyed by component name
    /// (e.g. `"root"`, `"panel"`, `"textbox"`, …).
    /// Loaded from `*.xml` files in the profile directory.
    pub component_templates: HashMap<String, String>,

    // -- Variables (available in Tera templates) ------------------------------
    /// Raw XFA variables extracted from the PDF (`xfa.*` in templates).
    pub xfa_vars: HashMap<String, String>,

    /// Resolved user-defined variables (`variables.*` in templates).
    pub user_vars: HashMap<String, String>,

    // -- XSD binding ---------------------------------------------------------
    /// When `true`, `bindRef` attributes are added to all form fields/panels
    /// pointing to the matching element path in the generated XSD schema.
    /// The XSD is also bundled into the AEM content package.
    pub bind_to_xsd: bool,

    /// XSD configuration used both for schema generation and for computing
    /// `bindRef` paths.  Must be `Some` when `bind_to_xsd` is `true`.
    pub xsd_config: Option<XsdConfig>,

    // -- Fragment support ----------------------------------------------------
    /// When `true`, panels whose XSD type matches a known fragment are
    /// replaced by `AemNode::Fragment` references.
    pub use_fragments: bool,

    /// JCR path prefix for constructing fragment `fragRef` values.
    pub fragment_ref_prefix: String,

    /// Optional list of fragment paths (relative to `fragments/`) to scan.
    /// Each path can be a directory (scanned recursively) or a specific fragment.
    /// When empty, all subdirectories of `fragments/` are scanned.
    pub fragment_paths: Vec<String>,

    /// Parsed fragments loaded from the `fragments/` subdirectory.
    pub fragments: Vec<ParsedFragment>,

    /// Default translations for predefined UI elements (toolbar buttons,
    /// message boxes, etc.) that are not part of the form content tree.
    ///
    /// Merged into the Sling i18n dictionaries at package generation time.
    /// Form-content translations take precedence over defaults.
    pub default_translations: HashMap<String, HashMap<String, String>>,

    // -- Custom elements -----------------------------------------------------
    /// Compiled custom element replacement rules.
    pub custom_elements: Vec<ResolvedCustomElement>,

    /// Custom element templates loaded from the `custom/` subdirectory.
    /// Key = template name (file stem), Value = Tera template string.
    pub custom_templates: HashMap<String, String>,
}

impl AemConfig {
    /// Create an `AemConfig` from an [`AemProfile`], component templates,
    /// and a [`Context`](crate::Context).
    ///
    /// The profile provides customer-specific settings. XFA variables from
    /// the context are available in template expressions as `xfa.*`.
    /// Component templates (loaded from `*.xml` files in the profile directory)
    /// drive all XML output.
    pub fn from_profile(
        profile: &AemProfile,
        templates: HashMap<String, String>,
        custom_templates: HashMap<String, String>,
        ctx: &crate::Context,
    ) -> Result<Self, crate::Error> {
        let xfa_vars = ctx.variables.clone();
        let user_vars = template::resolve_variables(&profile.variables, &xfa_vars)?;
        let tera_ctx = template::build_context(&xfa_vars, &user_vars);

        // --- form identity ---
        let form_title = template::render_string(&profile.title, &tera_ctx)?;

        let form_code = form_title.clone();

        let form_path = match &profile.form_path {
            Some(tmpl) => template::render_string(tmpl, &tera_ctx)?,
            None => String::new(),
        };

        let form_dir = template::render_string(&profile.form_dir, &tera_ctx)?;

        let bind_to_xsd = profile.bind_to_xsd.unwrap_or(false);
        let xsd_path = match &profile.xsd_path {
            Some(tmpl) => Some(template::render_string(tmpl, &tera_ctx)?),
            None => None,
        };

        let mandator = xfa_vars
            .get("formrange_entity")
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty());

        Ok(Self {
            form_title,
            form_code,
            mandator,
            languages: vec!["en".into()],
            master_language: profile
                .master_language
                .clone()
                .unwrap_or_else(|| "en".into()),
            language_synonyms: profile.language_synonyms.clone(),
            add_label_patterns: profile.add_label_patterns.clone(),
            html_locales: profile.html_locales.clone(),

            author: "blueprint".into(),
            deterministic_uuids: false,
            grid_columns: 12,
            repeatable_min_occur: 1,
            repeatable_max_occur: 20,

            form_path,
            form_dir,
            xsd_path,

            dor_template_ref: user_vars
                .get("dor_template_ref")
                .cloned()
                .unwrap_or_default(),
            dor_type: user_vars
                .get("dor_type")
                .cloned()
                .unwrap_or_else(|| "generate".into()),
            theme_ref: user_vars.get("theme_ref").cloned().unwrap_or_default(),
            header_slot_text: ctx.header.as_deref().and_then(header_slot_text),

            component_templates: templates,
            xfa_vars,
            user_vars,

            bind_to_xsd,
            xsd_config: None,

            use_fragments: profile.use_fragments.unwrap_or(false),
            fragment_ref_prefix: profile
                .fragment_ref_prefix
                .clone()
                .unwrap_or_else(|| "/content/dam/formsanddocuments/".into()),
            fragment_paths: match &profile.fragment_paths {
                Some(tmpl) => {
                    let rendered = template::render_string(tmpl, &tera_ctx)?;
                    rendered
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                None => Vec::new(),
            },
            fragments: Vec::new(),

            default_translations: profile.default_translations.clone(),

            custom_elements: profile
                .custom_elements
                .iter()
                .map(|rule| {
                    let pattern = Regex::new(&rule.field_name).map_err(|e| {
                        crate::Error::AemConfig(format!(
                            "Invalid regex in custom_elements.field_name '{}': {}",
                            rule.field_name, e
                        ))
                    })?;
                    Ok(ResolvedCustomElement {
                        pattern,
                        template: rule.template.clone(),
                        page: rule.page,
                        depends_on: rule.depends_on.clone(),
                        inject: rule.inject,
                    })
                })
                .collect::<Result<Vec<_>, crate::Error>>()?,
            custom_templates,
        })
    }

    /// The JCR folder name for this form (from the profile's `form_dir` template).
    pub fn form_dir(&self) -> String {
        self.form_dir.clone()
    }

    /// Return the canonical DAM XSD reference path for metadata attributes.
    /// Returns `None` when `xsd_path` is not configured.
    pub fn xsd_ref(&self) -> Option<String> {
        self.xsd_path.as_ref().map(|p| {
            if p.starts_with('/') {
                p.clone()
            } else {
                format!("/{}", p)
            }
        })
    }

    /// Return the ZIP entry path where the XSD file should be written.
    /// Returns `None` when `xsd_path` is not configured.
    pub fn xsd_zip_path(&self) -> Option<String> {
        self.xsd_ref()
            .map(|r| format!("jcr_root/{}", r.trim_start_matches('/')))
    }

    /// The code this profile files `lang` under: a synonym resolves to the
    /// language it is a synonym of, anything else to itself.
    ///
    /// The two ends of a conversion name a language differently. Detection
    /// yields ISO 639-1 (`es` for Spanish), while a profile's dictionaries and
    /// default translations are keyed by whatever the target platform files them
    /// under (`sp`, with `es` declared as its synonym). Folding the one onto the
    /// other is what keeps a language's content and its default translations in
    /// the same bucket.
    pub fn canonical_language(&self, lang: &str) -> String {
        for (primary, synonyms) in &self.language_synonyms {
            if synonyms.iter().any(|s| s == lang) {
                return primary.clone();
            }
        }
        lang.to_string()
    }

    /// The Add-button label for `subject` in `lang`, or `None` when the profile
    /// gives no wording for that language.
    ///
    /// The word order is the profile's business: `Add {subject}` in English,
    /// `{subject} hinzufügen` in German. Without a pattern the caller keeps
    /// whatever its template says, which is how a profile that never configured
    /// this keeps its old output.
    pub fn add_label(&self, lang: &str, subject: &str) -> Option<String> {
        let subject = subject.trim();
        if subject.is_empty() {
            return None;
        }
        let pattern = self.add_label_patterns.get(&self.canonical_language(lang))?;
        Some(pattern.replace("{subject}", subject))
    }

    /// [`canonical_language`](Self::canonical_language) over `languages`,
    /// deduplicated and order-preserving.
    pub fn canonical_languages(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(self.languages.len());
        for lang in &self.languages {
            let canonical = self.canonical_language(lang);
            if !out.contains(&canonical) {
                out.push(canonical);
            }
        }
        out
    }

    /// The locale string the HTML component's `localeContent` item for `lang`
    /// is keyed by.
    ///
    /// `[html_locales]` in the profile decides; a language the table does not
    /// name keeps its own code, which is the shipped default. See
    /// [`AemProfile::html_locales`] for why this is profile data.
    pub fn html_locale(&self, lang: &str) -> String {
        self.html_locales
            .get(lang)
            .cloned()
            .unwrap_or_else(|| lang.to_string())
    }

    /// Expand `languages` to include synonyms.
    ///
    /// For each language in `self.languages`, if it has synonyms defined in
    /// `language_synonyms`, those are added to the result. Synonyms are folded
    /// onto their primary code first, so a language detected under its synonym
    /// still brings the whole family. The result is sorted alphabetically.
    pub fn expand_languages(&self) -> Vec<String> {
        let canonical = self.canonical_languages();
        let mut expanded = canonical.clone();
        for lang in &canonical {
            if let Some(synonyms) = self.language_synonyms.get(lang) {
                for syn in synonyms {
                    if !expanded.contains(syn) {
                        expanded.push(syn.clone());
                    }
                }
            }
        }
        expanded.sort();
        expanded
    }
}

#[cfg(test)]
impl AemConfig {
    /// Create an `AemConfig` for testing without requiring a real `Context`.
    ///
    /// The caller provides the form code directly.
    /// All other fields use sensible defaults.
    pub fn test_default(form_code: &str) -> Self {
        Self {
            form_title: form_code.into(),
            form_code: form_code.into(),
            mandator: None,
            languages: vec!["en".into()],
            master_language: "en".into(),
            add_label_patterns: HashMap::new(),
            html_locales: HashMap::new(),
            language_synonyms: {
                let mut map = HashMap::new();
                map.insert("de".into(), vec!["de-ch".into()]);
                map.insert("sp".into(), vec!["es".into()]);
                map
            },

            author: "blueprint".into(),
            deterministic_uuids: false,
            grid_columns: 12,
            repeatable_min_occur: 1,
            repeatable_max_occur: 20,

            form_path: "test/path".into(),
            form_dir: format!("AF_{form_code}"),
            xsd_path: Some("/content/dam/formsanddocuments/test/path/AF_TEST/schema.xsd".into()),

            dor_template_ref: String::new(),
            dor_type: "generate".into(),
            theme_ref: String::new(),
            header_slot_text: None,

            component_templates: HashMap::new(),
            xfa_vars: HashMap::new(),
            user_vars: HashMap::new(),

            bind_to_xsd: false,
            xsd_config: None,

            use_fragments: false,
            fragment_ref_prefix: "/content/dam/formsanddocuments/".into(),
            fragment_paths: Vec::new(),
            fragments: Vec::new(),

            default_translations: HashMap::new(),

            custom_elements: Vec::new(),
            custom_templates: HashMap::new(),
        }
    }
}

// ============================================================================
// AEM Node Types
// ============================================================================

/// Which single-line input component a [`AemNode::TextField`] renders as.
///
/// A form's PDF source has no type for an email address or a phone number — both
/// arrive as plain text — so the kind is decided from the field's label in the
/// structured model ([`crate::structured::contact_field`]) and carried through
/// here. It selects the template, and with it the resource type, the validation
/// clause, the `autofillFieldKeyword` and the phonebox styling.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub enum TextFieldKind {
    /// `controls/textbox` — the default.
    #[default]
    Plain,
    /// `controls/email`, with the corpus's email validation clause.
    Email,
    /// `controls/telephone`, with the phonebox styling and the `^([+]|00)…`
    /// display and validation clause.
    Telephone,
    /// `controls/textboxMultiline` — a text area. A multi-line field named
    /// `TXTM_` on a plain `textbox` is what `PROBLEM-naming-conventions` reads
    /// as a wrong prefix, and the corpus has the dedicated component.
    Multiline,
}

impl TextFieldKind {
    /// The profile template that renders this kind.
    pub fn template_key(self) -> &'static str {
        match self {
            TextFieldKind::Plain => "textbox",
            TextFieldKind::Email => "email",
            TextFieldKind::Telephone => "telephone",
            TextFieldKind::Multiline => "textbox_multiline",
        }
    }

    /// The JCR element-name stem, mirroring the template key so a package reads
    /// the way AEM's own export does (`email_<uuid>`, `telephone_<uuid>`).
    pub fn element_stem(self) -> &'static str {
        self.template_key()
    }
}

/// Alignment of options in checkbox / radio button groups.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum OptionAlignment {
    Horizontal,
    Vertical,
}

/// A single option in a checkbox or radio button group.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AemOption {
    /// Display label (may contain rich text HTML).
    pub label: String,
    /// Form value submitted when this option is selected.
    pub value: String,
}

/// A visibility condition rule that links a trigger field value to a
/// conditional panel.
///
/// When the trigger field's value matches `value`, the target panel's
/// visibility is set to the `show` value.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ConditionRule {
    /// AEM `name` of the conditional panel to show/hide.
    pub target_panel_name: String,
    /// The value that, when matched, triggers the show/hide.
    pub value: InputValue,
    /// `true` → show panel when matched; `false` → hide.
    pub show: bool,
}

/// Fidelity passthrough for a node loaded from an existing AEM package: the raw
/// attributes and child elements the typed model does NOT represent, captured on
/// load so a load→edit→save round-trip preserves them. Empty for engine-built
/// (from-XFA) nodes, so their output is unchanged.
///
/// Lives on [`AemNodeTranslated`](crate::aem::translated::AemNodeTranslated) (the
/// persisted, editable working tree) and is carried to the writer via a
/// uuid-keyed side-map at build time. Values are XML-decoded strings (as
/// `parse_jcr_xml` stores them), preserving JCR type prefixes/arrays verbatim
/// (`{Boolean}false`, `[true,true,true]`); they are re-escaped on write.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Passthrough {
    /// Raw attributes not owned by the node's typed fields (attribute name → raw
    /// value). Keyed/ordered for stable output.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raw_attributes: BTreeMap<String, String>,
    /// Verbatim XML of child elements the converter does not model (e.g.
    /// `fd:rules`, non-condition `fd:scripts`). Excludes `items`/`layout`/
    /// `cq:responsive`, which the writer regenerates from typed fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_children: Vec<String>,
}

impl Passthrough {
    /// `true` when there is nothing to carry (engine-built nodes) — used to keep
    /// serialized output identical to today for from-XFA trees.
    pub fn is_empty(&self) -> bool {
        self.raw_attributes.is_empty() && self.raw_children.is_empty()
    }
}

/// The JCR attributes every AEM component can carry that decide where it shows
/// up: on screen, on the summary step, in the Document of Record, in the PDF.
///
/// They are one struct rather than a field per variant because the deployed
/// corpus puts the same handful on panels, fields, draws and buttons alike, and
/// because the feedback sweeps read them per open tag with no regard for the
/// component type (`PROBLEM-dor-exclusion-implies-summary` sets
/// `summaryExclusion` on 7'207 tags across eleven component types). Flattened
/// into the node's own JSON object, so the agent addresses them as ordinary
/// fields (`set_aem_translated_field … "summary_exclude"`) and a tree persisted
/// before they existed still loads.
///
/// The UBS DoR is rendered by Redacto from the *summary* data, which is why
/// these three are not interchangeable:
/// - `dor_exclude` (`dorExclusion`) is Adobe's own switch and is not read on
///   the Redacto path at all,
/// - `summary_exclude` (`summaryExclusion`) is what actually keeps a node out of
///   the summary and therefore out of the rendered DoR,
/// - `always_in_pdf` (`alwaysInPdf`) puts a summary-excluded or hidden node back
///   into the PDF alone, which is how the internal-bank-use block and the DoR
///   copy of the Italy infobox reach the reader.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct AemAttrs {
    /// `dorExclusion="true"` — excluded from the Document of Record.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dor_exclude: bool,
    /// `summaryExclusion="true"` — excluded from the summary step, and with it
    /// from the Redacto-rendered DoR. Everything excluded from the DoR must also
    /// be excluded from the summary (owner directive 2026-08-26), so a node with
    /// `dor_exclude` carries this too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub summary_exclude: bool,
    /// `dorExcludeTitle="true"` — the node's own title is left out of the DoR.
    /// On a wizard step this is the convention: the heading lives in the step's
    /// `{name}Title` sub-panel.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dor_exclude_title: bool,
    /// `alwaysInPdf="true"` — reaches the PDF even though it is hidden or
    /// summary-excluded.
    #[serde(default, skip_serializing_if = "is_false")]
    pub always_in_pdf: bool,
    /// `showIfHidden="true"` — a hidden node still reaches the summary data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub show_if_hidden: bool,
    /// `jumpToFieldButtonVisible="true"` — the summary's Edit button for this
    /// step. It belongs on the step-title panel, never on the title draw.
    #[serde(default, skip_serializing_if = "is_false")]
    pub jump_to_field: bool,
    /// `css` — the theme class list. Carries meaning here: `stepTitle` marks a
    /// step heading, `subtitle-after-form-title` the first page's subtitle,
    /// `ubs-margin-20` the banking-relationship wrapper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    /// `dorHeaderSlot` — the DoR header slot this text is printed in
    /// (`"slot2"` for the legal-entity line below the banking relationship).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dor_header_slot: Option<String>,
}

impl AemAttrs {
    /// Excluded from the DoR, and therefore from the summary as well.
    pub fn dor_excluded() -> Self {
        Self {
            dor_exclude: true,
            summary_exclude: true,
            ..Self::default()
        }
    }

    /// Reaches the PDF and nothing else: kept out of the summary on screen, put
    /// back into the printed document. The shape the internal-bank-use panels
    /// and the DoR copy of the Italy infobox need; `dor_exclude` must stay off,
    /// or the node is dropped again.
    pub fn pdf_only() -> Self {
        Self {
            summary_exclude: true,
            always_in_pdf: true,
            ..Self::default()
        }
    }
}

/// `#[serde(skip_serializing_if)]` predicate — a `false` flag is the default and
/// is left out of the JSON, so a tree reads as it did before these existed.
fn is_false(b: &bool) -> bool {
    !*b
}

/// `#[serde(default)]` for a `visible` field: a node with no `visible` in its
/// JSON is visible.
fn default_true() -> bool {
    true
}

/// The legal-entity line for the DoR's second header slot, from the source
/// document's master-page header.
///
/// UBS forms print `UBS Europe SE (Succursale Italia)` under the banking
/// relationship in the finished document, and the corpus carries it as a hidden
/// static text with `dorHeaderSlot="slot2"`. The text is the form's own: the
/// analysis already recovers the master-page header into
/// [`Context::header`](crate::Context::header), where it arrives as stacked
/// lines -- typically a validity or edition line and the entity name.
///
/// The validity line is dropped (it is the form's version, not the issuer) and
/// the first line that survives is the entity, which the corpus prints in bold.
/// Returns `None` when nothing is left, so a form whose header holds only a date
/// gets no slot-2 text rather than a wrong one.
pub fn header_slot_text(header: &str) -> Option<String> {
    /// A line that dates the form rather than naming its issuer.
    fn is_validity_line(line: &str) -> bool {
        const PREFIXES: &[&str] = &[
            "gültig", "gueltig", "valid", "valido", "valida", "valable", "edition",
            "ausgabe", "version", "stand",
        ];
        let lower = line.to_lowercase();
        if PREFIXES.iter().any(|p| lower.starts_with(p)) {
            return true;
        }
        // A bare date (`02.01.2018`, `1/2018`) is the same thing without a word.
        let digits = line.chars().filter(|c| c.is_ascii_digit()).count();
        digits >= 4 && line.chars().all(|c| c.is_ascii_digit() || " ./-".contains(c))
    }

    let mut lines = header
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_validity_line(l));
    let first = lines.next()?;
    let rest = lines.collect::<Vec<_>>().join(" ");

    // The corpus bolds the entity and leaves what qualifies it plain:
    // `<b>UBS Europe SE</b> (Succursale Italia)`. A parenthesis is where the
    // one ends and the other begins.
    let (entity, qualifier) = match first.find(" (") {
        Some(at) => (&first[..at], first[at..].trim()),
        None => (first, ""),
    };
    let tail = [qualifier, rest.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");

    // The markup is stored the way a JCR rich-text value stores it: the tags
    // themselves XML-escaped, so the attribute stays well-formed.
    let mut value = format!("&lt;b>{}&lt;/b>", crate::util::escape_html(entity));
    if !tail.is_empty() {
        value.push(' ');
        value.push_str(&crate::util::escape_html(&tail));
    }
    Some(value)
}

/// The intermediate AEM node tree.
///
/// Each variant maps to a specific AEM Adaptive Forms component and carries
/// all the data needed for XML serialization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum AemNode {
    /// Top-level form container — produces the full JCR page structure.
    Root {
        title: String,
        children: Vec<AemNode>,
    },

    /// Generic panel container (`guidePanel`).
    Panel {
        uuid: Uuid,
        name: String,
        title: String,
        children: Vec<AemNode>,
        /// Whether this panel represents a page/wizard step.
        is_page: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the panel is visible. Default `true`.
        visible: bool,
        /// Whether this panel wraps a conditional branch.
        is_conditional: bool,
        /// Number of columns for Document of Record layout (`dorNumCols`).
        /// Derived from `GridLayout.columns`. `None` means no `dorNumCols` attribute.
        dor_num_cols: Option<u32>,
        /// Adaptive-form responsive column width (out of 12).
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        /// Set on elements that are children of a `GridLayout` panel.
        dor_colspan: Option<u32>,
        /// XSD path for `bindRef` attribute (e.g. `/form/personal_data`).
        /// `None` when `bind_to_xsd` is `false` or the panel has no corresponding XSD element.
        bind_ref: Option<String>,
        /// `fragRef` of the fragment this panel was expanded from, when the
        /// parser inlined a fragment's children into it.
        ///
        /// `None` for ordinary panels. Keeping it means a package that is loaded
        /// and saved again still knows which panels came from a fragment, which
        /// is what lets the XSD walk emit a fragment element rather than
        /// descending into the inlined children.
        #[serde(default)]
        frag_ref: Option<String>,
    },

    /// Single-line text input (`guideTextBox`), or one of its typed variants —
    /// see [`TextFieldKind`].
    TextField {
        uuid: Uuid,
        name: String,
        label: String,
        mandatory: bool,
        visible: bool,
        max_chars: Option<usize>,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
        /// Which single-line input component this is. Defaults to
        /// [`TextFieldKind::Plain`] so packages serialised before the typed
        /// variants existed still load.
        #[serde(default)]
        kind: TextFieldKind,
    },

    /// Numeric input (`guideNumericBox`).
    NumberField {
        uuid: Uuid,
        name: String,
        label: String,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
    },

    /// Date picker (`guideDatePicker`).
    DatePicker {
        uuid: Uuid,
        name: String,
        label: String,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
    },

    /// Drop-down / select list (`guideDropDownList`).
    Dropdown {
        uuid: Uuid,
        name: String,
        label: String,
        options: Vec<AemOption>,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
        /// The `FieldId` of the original structured field (for condition wiring).
        #[schemars(with = "Option<String>")]
        field_id: Option<FieldId>,
        /// Visibility condition rules populated during the second pass.
        conditions: Vec<ConditionRule>,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
    },

    /// Checkbox group (`guideCheckBox`).
    Checkbox {
        uuid: Uuid,
        name: String,
        /// Group label from `jcr:title` (empty for single-option Bool checkboxes).
        label: String,
        options: Vec<AemOption>,
        alignment: OptionAlignment,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
        /// The `FieldId` of the original structured field (for condition wiring).
        #[schemars(with = "Option<String>")]
        field_id: Option<FieldId>,
        /// Visibility condition rules populated during the second pass.
        conditions: Vec<ConditionRule>,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
    },

    /// Radio button group (`guideRadioButton`).
    RadioButton {
        uuid: Uuid,
        name: String,
        label: String,
        options: Vec<AemOption>,
        alignment: OptionAlignment,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
        /// The `FieldId` of the original structured field (for condition wiring).
        #[schemars(with = "Option<String>")]
        field_id: Option<FieldId>,
        /// Visibility condition rules populated during the second pass.
        conditions: Vec<ConditionRule>,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
    },

    /// Static text / heading (`guideTextDraw`).
    TextDraw {
        uuid: Uuid,
        name: String,
        content: String,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "default_true")]
        visible: bool,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
    },

    /// Title draw for h3–h6 headings (`guideTextDraw` with `headingLevel`).
    TitleDraw {
        uuid: Uuid,
        name: String,
        content: String,
        heading_level: u8,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "default_true")]
        visible: bool,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
    },

    /// Static HTML block (`htmlDisplayer`) -- a table, a chart or an image,
    /// rendered as markup rather than as a tree of draws.
    ///
    /// Unlike every other draw the markup is carried PER LANGUAGE on the node:
    /// the component reads its own `localeContent` children, not the Sling i18n
    /// dictionary, and the XML writer has no dictionary to consult. So
    /// [`AemI18nText`] lives here on the mono tree too, and the node
    /// contributes nothing to the translation dictionary when it is lowered.
    HtmlDisplayer {
        uuid: Uuid,
        name: String,
        /// Language code -> HTML markup.
        content: AemI18nText,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "default_true")]
        visible: bool,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
    },

    /// An on-screen notice: the UBS message box.
    ///
    /// The source marks such a note `relevant="-print"` — it addresses whoever
    /// fills the form and never the printed document, which is why it carries
    /// `dorExclusion` and `summaryExclusion` from its template. Rendering it as
    /// an ordinary static draw would put it in the DoR.
    MessageBox {
        uuid: Uuid,
        name: String,
        /// The notice body, as rich text.
        content: String,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "default_true")]
        visible: bool,
        colspan: u32,
        /// Column span in Document of Record layout (`dorColspan`).
        dor_colspan: Option<u32>,
    },

    /// Repeatable panel with add/remove buttons.
    Repeatable {
        uuid: Uuid,
        name: String,
        title: String,
        children: Vec<AemNode>,
        min_occur: u32,
        max_occur: u32,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "default_true")]
        visible: bool,
        /// XSD path for `bindRef` attribute on the repeatable inner panel.
        bind_ref: Option<String>,
        /// `fragRef` of the fragment this repeatable wraps, when a repeating
        /// panel carried a `fragRef` and its content was inlined.
        ///
        /// A repeating fragment (`maxOccur` on a `fragRef` panel) is the shape
        /// behind an XSD element such as
        /// `<xs:element name="AuthRepSignature" type="SignatureType" maxOccurs="50"/>`,
        /// so the reference has to survive the unwrapping.
        #[serde(default)]
        frag_ref: Option<String>,
    },

    /// Fragment reference — replaces a panel whose XSD type matches a
    /// known fragment. The fragment's internal structure is loaded by AEM
    /// at runtime from the `fragRef` path.
    Fragment {
        uuid: Uuid,
        name: String,
        /// `jcr:title` of the panel this fragment replaced.
        ///
        /// Several panels may share one `frag_ref` — a form can hold two
        /// `affrg_SignatureGeneric1` fragments, one for the client and one for
        /// the authorized representative — and the title is the only thing that
        /// tells them apart when resolving the XSD element name. Empty when the
        /// replaced panel had no title.
        #[serde(default)]
        title: String,
        /// JCR path to the fragment (e.g.
        /// `"/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_Address1"`).
        frag_ref: String,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "default_true")]
        visible: bool,
        /// XSD path for `bindRef` attribute.
        bind_ref: Option<String>,
    },

    /// Optional profile-driven snippet inserted as the first item in the
    /// first page panel when the `preface` template exists.
    Preface { uuid: Uuid, name: String },

    /// Optional profile-driven snippet inserted as the last item in the
    /// last page panel when the `appendix` template exists.
    Appendix { uuid: Uuid, name: String },

    /// Footnote placeholder component, placed at the end of a page panel
    /// that contains inline footnote references. AEM renders collected
    /// footnotes at this location.
    FootnotePlaceholder {
        uuid: Uuid,
        name: String,
        colspan: u32,
    },

    /// Custom element — replaces a matched field/panel with a custom template.
    /// Created by the `apply_custom_elements()` pass when a node's label/title
    /// matches a `[[custom_elements]]` rule.
    Custom {
        uuid: Uuid,
        name: String,
        /// The custom template key (file stem from `custom/` directory).
        template_key: String,
        /// The label or title of the matched element.
        label: String,
        /// Options from the original element (if it was a Dropdown/RadioButton/Checkbox).
        options: Vec<AemOption>,
        /// Whether the original element was mandatory.
        mandatory: bool,
        /// Whether the original element was visible.
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Column span of the original element.
        colspan: u32,
        /// DOR column span from the original element.
        dor_colspan: Option<u32>,
        /// Bind ref from the original element.
        bind_ref: Option<String>,
    },
}

// ============================================================================
// Helpers
// ============================================================================

impl AemNode {
    /// `max_occur` value standing for "unbounded".
    ///
    /// AEM spells this `maxOccur="-1"`, which does not fit the `u32` the model
    /// uses, so it is carried as this sentinel and written back out as `-1`.
    pub const UNBOUNDED_OCCUR: u32 = u32::MAX;

    /// The node's presentation attributes ([`AemAttrs`]), or `None` for the
    /// three variants that have none: `Root` and the profile-driven `Preface` /
    /// `Appendix` / `FootnotePlaceholder` snippets, whose whole tag is fixed by
    /// their template.
    pub fn attrs(&self) -> Option<&AemAttrs> {
        match self {
            AemNode::Panel { attrs, .. }
            | AemNode::TextField { attrs, .. }
            | AemNode::NumberField { attrs, .. }
            | AemNode::DatePicker { attrs, .. }
            | AemNode::Dropdown { attrs, .. }
            | AemNode::Checkbox { attrs, .. }
            | AemNode::RadioButton { attrs, .. }
            | AemNode::TextDraw { attrs, .. }
            | AemNode::TitleDraw { attrs, .. }
            | AemNode::HtmlDisplayer { attrs, .. }
            | AemNode::MessageBox { attrs, .. }
            | AemNode::Repeatable { attrs, .. }
            | AemNode::Fragment { attrs, .. }
            | AemNode::Custom { attrs, .. } => Some(attrs),
            AemNode::Root { .. }
            | AemNode::Preface { .. }
            | AemNode::Appendix { .. }
            | AemNode::FootnotePlaceholder { .. } => None,
        }
    }

    /// Mutable counterpart of [`AemNode::attrs`], for the normalisation passes
    /// that set exclusion flags on a finished tree.
    pub fn attrs_mut(&mut self) -> Option<&mut AemAttrs> {
        match self {
            AemNode::Panel { attrs, .. }
            | AemNode::TextField { attrs, .. }
            | AemNode::NumberField { attrs, .. }
            | AemNode::DatePicker { attrs, .. }
            | AemNode::Dropdown { attrs, .. }
            | AemNode::Checkbox { attrs, .. }
            | AemNode::RadioButton { attrs, .. }
            | AemNode::TextDraw { attrs, .. }
            | AemNode::TitleDraw { attrs, .. }
            | AemNode::HtmlDisplayer { attrs, .. }
            | AemNode::MessageBox { attrs, .. }
            | AemNode::Repeatable { attrs, .. }
            | AemNode::Fragment { attrs, .. }
            | AemNode::Custom { attrs, .. } => Some(attrs),
            AemNode::Root { .. }
            | AemNode::Preface { .. }
            | AemNode::Appendix { .. }
            | AemNode::FootnotePlaceholder { .. } => None,
        }
    }

    /// Get the element tag name used in JCR XML (e.g. `"panel_<uuid>"`).
    pub fn element_name(&self) -> String {
        match self {
            AemNode::Root { .. } => "jcr:root".into(),
            AemNode::Panel { uuid, .. } => format!("panel_{}", uuid.as_simple()),
            AemNode::TextField { uuid, kind, .. } => {
                format!("{}_{}", kind.element_stem(), uuid.as_simple())
            }
            AemNode::NumberField { uuid, .. } => format!("numericbox_{}", uuid.as_simple()),
            AemNode::DatePicker { uuid, .. } => format!("datepicker_{}", uuid.as_simple()),
            AemNode::Dropdown { uuid, .. } => format!("dropdownlist_{}", uuid.as_simple()),
            AemNode::Checkbox { uuid, .. } => format!("checkbox_{}", uuid.as_simple()),
            AemNode::RadioButton { uuid, .. } => format!("radiobutton_{}", uuid.as_simple()),
            AemNode::TextDraw { uuid, .. } => format!("textdraw_{}", uuid.as_simple()),
            AemNode::TitleDraw { uuid, .. } => format!("titledraw_{}", uuid.as_simple()),
            AemNode::HtmlDisplayer { uuid, .. } => {
                format!("htmldisplayer_{}", uuid.as_simple())
            }
            AemNode::MessageBox { uuid, .. } => format!("messagebox_{}", uuid.as_simple()),
            AemNode::Repeatable { uuid, .. } => format!("repeatable_{}", uuid.as_simple()),
            AemNode::Fragment { uuid, .. } => format!("fragment_{}", uuid.as_simple()),
            AemNode::Preface { uuid, .. } => format!("preface_{}", uuid.as_simple()),
            AemNode::Appendix { uuid, .. } => format!("appendix_{}", uuid.as_simple()),
            AemNode::FootnotePlaceholder { uuid, .. } => {
                format!("guidefootnoteplaceho_{}", uuid.as_simple())
            }
            AemNode::Custom { uuid, name, .. } => {
                if name.is_empty() {
                    format!("custom_{}", uuid.as_simple())
                } else {
                    name.clone()
                }
            }
        }
    }
}
