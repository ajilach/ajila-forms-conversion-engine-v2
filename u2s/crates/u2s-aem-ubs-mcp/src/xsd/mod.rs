//! XSD (XML Schema Definition) Output Module
//!
//! Describes a form's data model as an XSD schema. The schema is derived from
//! the **AEM node tree**, and every node's `bindRef` is assigned during the same
//! walk, so a form and its schema agree by construction rather than because two
//! code paths were kept in step.
//!
//! The generated vocabulary is deliberately narrow — `xs:schema`, `xs:include`,
//! `xs:element`, `xs:complexType`, `xs:sequence` — because that is what the UBS
//! toolchain consumes. There is no `xs:choice`, no `xs:simpleType` and no
//! restriction facet, and [`XsdNode`] cannot express them.
//!
//! # Profile configuration
//!
//! The module reads a TOML config from `profiles/{name}/xsd/config.toml`:
//! - `[[aemElements]]` — ordered rules for the AEM → XSD walk (see
//!   [`AemElementRule`]): which nodes to ignore, the element names a title
//!   cannot yield, and occurrence overrides
//! - `[defaultTypes]` — XSD type per AEM component kind
//! - `[elements.<name>]` — synonym mappings
//! - `rootElementName`, `maxOccursValue`, `alwaysInclude`, `schemaLocationPrefix`
//!
//! `xs:include` directives are generated automatically by indexing all `*.xsd`
//! files in `profiles/{name}/xsd/types/`. `alwaysInclude` entries come first;
//! the rest are emitted in first-appearance order, and only for a type the
//! schema actually references.
//!
//! Those same files also decide `ref=` versus `name=`/`type=`: an element name
//! declared globally under `types/` is referenced, never re-declared.

pub mod from_aem;

pub use from_aem::{
    AemXsdResult, apply_bind_refs, generate_xsd_from_aem, generate_xsd_string_from_aem,
};

use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

// ============================================================================
// XSD node types (intermediate representation)
// ============================================================================

/// An XSD node in the intermediate schema tree.
#[derive(Debug, Clone, PartialEq)]
pub enum XsdNode {
    /// `<xs:element name="..." type="..." .../>`
    Element {
        name: String,
        type_ref: Option<String>,
        min_occurs: Option<u32>,
        max_occurs: Option<Option<u32>>,
        content: Option<Box<XsdNode>>,
    },
    /// `<xs:element ref="..."/>` — reference to a global element declaration.
    Ref {
        ref_name: String,
        min_occurs: Option<u32>,
        max_occurs: Option<Option<u32>>,
    },
    /// `<xs:complexType>` (inline when `name` is `None`, named when `Some`)
    ComplexType {
        name: Option<String>,
        sequence: Vec<XsdNode>,
    },
}

/// A complete XSD schema with includes and a root element.
#[derive(Debug, Clone, PartialEq)]
pub struct XsdSchema {
    pub includes: Vec<String>,
    pub root: XsdNode,
}

impl XsdSchema {
    /// Serialize to a complete XSD XML string.
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n");

        for path in &self.includes {
            out.push_str(&format!("  <xs:include schemaLocation=\"{}\"/>\n", path));
        }
        if !self.includes.is_empty() {
            out.push('\n');
        }

        self.root.write_xml(&mut out, 2);

        out.push_str("</xs:schema>\n");
        out
    }
}

impl XsdNode {
    /// Write this node as XSD XML into `out` at the given indentation level.
    pub fn write_xml(&self, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        match self {
            XsdNode::Element {
                name,
                type_ref,
                min_occurs,
                max_occurs,
                content,
            } => {
                let occur = build_occurrence_attrs(*min_occurs, *max_occurs);
                match (type_ref, content) {
                    (Some(tr), None) => {
                        out.push_str(&format!(
                            "{}<xs:element name=\"{}\" type=\"{}\"{}/>",
                            pad, name, tr, occur
                        ));
                        out.push('\n');
                    }
                    (None, Some(child)) => {
                        out.push_str(&format!("{}<xs:element name=\"{}\"{}>\n", pad, name, occur));
                        child.write_xml(out, indent + 2);
                        out.push_str(&format!("{}</xs:element>\n", pad));
                    }
                    _ => {
                        // Fallback: element with no type and no content
                        out.push_str(&format!(
                            "{}<xs:element name=\"{}\" type=\"xs:string\"{}/>",
                            pad, name, occur
                        ));
                        out.push('\n');
                    }
                }
            }
            XsdNode::Ref {
                ref_name,
                min_occurs,
                max_occurs,
            } => {
                let occur = build_occurrence_attrs(*min_occurs, *max_occurs);
                out.push_str(&format!(
                    "{}<xs:element ref=\"{}\"{}/>",
                    pad, ref_name, occur
                ));
                out.push('\n');
            }
            XsdNode::ComplexType { name, sequence } => {
                match name {
                    Some(n) => out.push_str(&format!("{}<xs:complexType name=\"{}\">\n", pad, n)),
                    None => out.push_str(&format!("{}<xs:complexType>\n", pad)),
                }
                out.push_str(&format!("{}  <xs:sequence>\n", pad));
                for child in sequence {
                    child.write_xml(out, indent + 4);
                }
                out.push_str(&format!("{}  </xs:sequence>\n", pad));
                out.push_str(&format!("{}</xs:complexType>\n", pad));
            }
        }
    }
}

/// The occurrence attributes an element carries.
///
/// `None`/`None` means no attributes at all — which is different from
/// `minOccurs="1"`, even though the two validate identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurs {
    pub min: Option<u32>,
    pub max: Option<Option<u32>>,
}

impl Occurs {
    /// No occurrence attributes: the element is required and single.
    pub fn required() -> Self {
        Self {
            min: None,
            max: None,
        }
    }

    /// `minOccurs="0"`.
    pub fn optional() -> Self {
        Self {
            min: Some(0),
            max: None,
        }
    }

    /// `minOccurs="0" maxOccurs="{max}"`.
    pub fn optional_repeating(max: u32) -> Self {
        Self {
            min: Some(0),
            max: Some(Some(max)),
        }
    }

    /// Whether this element may occur more than once.
    pub fn repeats(&self) -> bool {
        matches!(self.max, Some(None)) || matches!(self.max, Some(Some(n)) if n > 1)
    }
}

/// How an [`AemElementRule`] spells its occurrence override.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OccursSpec {
    /// No `minOccurs`/`maxOccurs` at all.
    None,
    /// `minOccurs="0"`.
    Optional,
    /// `minOccurs="0"` plus the profile's `maxOccursValue`.
    OptionalRepeating,
}

impl OccursSpec {
    fn to_occurs(&self, max_occurs_value: u32) -> Occurs {
        match self {
            OccursSpec::None => Occurs::required(),
            OccursSpec::Optional => Occurs::optional(),
            OccursSpec::OptionalRepeating => Occurs::optional_repeating(max_occurs_value),
        }
    }
}

/// Build `minOccurs`/`maxOccurs` attribute string for an element.
fn build_occurrence_attrs(min_occurs: Option<u32>, max_occurs: Option<Option<u32>>) -> String {
    let mut attrs = String::new();
    if let Some(min) = min_occurs
        && min != 1
    {
        attrs.push_str(&format!(" minOccurs=\"{}\"", min));
    }
    if let Some(max) = max_occurs {
        match max {
            Some(n) => {
                if n != 1 {
                    attrs.push_str(&format!(" maxOccurs=\"{}\"", n));
                }
            }
            None => {
                attrs.push_str(" maxOccurs=\"unbounded\"");
            }
        }
    }
    attrs
}

// ============================================================================
// Profile types (TOML-deserializable)
// ============================================================================

fn default_schema_location_prefix() -> String {
    "../".to_string()
}

fn default_root_element_name() -> String {
    "form".to_string()
}

/// TOML-deserializable XSD profile loaded from
/// `profiles/{name}/xsd/config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XsdProfile {
    /// Mapping from canonical element names to their config.
    #[serde(default)]
    pub elements: HashMap<String, ElementMapping>,

    /// Per-type element aliases: `type name → (leaf as resolved → leaf as that
    /// type calls it)`.
    ///
    /// Label resolution is context-free — one element name per label — but a
    /// word can mean different things in different blocks. `Località` and `Ort`
    /// resolve to `Place`, which is a `SignatureType` element, and in a
    /// signature row that is right; in an address row the same word is the
    /// city. Without this an Italian address can match no type at all, because
    /// two of its five leaves sit outside `AddressType`.
    ///
    /// Consulted only while a specific candidate type is being tested, so the
    /// default resolution — and every other type — is untouched.
    #[serde(default)]
    pub type_aliases: HashMap<String, HashMap<String, String>>,

    /// Mapping from canonical section names to their regex-based config.
    ///
    /// When a section's full text (heading + body until next heading) matches
    /// one of the configured patterns, the section's XSD element name is
    /// overridden with the TOML key.
    #[serde(default)]
    pub sections: HashMap<String, SectionMapping>,

    /// Prefix prepended to every auto-discovered include path.
    ///
    /// For example, if the types directory contains `AFFragments/Signature.xsd`
    /// and `schemaLocationPrefix = "../"`, the generated include will be:
    /// `<xs:include schemaLocation="../AFFragments/Signature.xsd"/>`.
    ///
    /// Defaults to `"../"`.
    #[serde(default = "default_schema_location_prefix")]
    pub schema_location_prefix: String,

    /// Optional master language code used for element name resolution.
    ///
    /// When set (for example via `masterLanguage = "en"` in
    /// `xsd/config.toml`), element names derived from multilingual labels
    /// prefer that language.
    #[serde(default)]
    pub master_language: Option<String>,

    /// Template for the root element name in generated XSD schemas.
    ///
    /// May contain `{{ form_code }}` which is replaced at generation time
    /// with the actual form code.  Defaults to `"form"`.
    ///
    /// Example: `rootElementName = "UBSAF_{{ form_code }}"`
    #[serde(default = "default_root_element_name")]
    pub root_element_name: String,

    /// Prefix used for fragment `bindRef` paths.
    ///
    /// In reference forms, fragments use a generic prefix (e.g. `/UBSAF/`)
    /// instead of the form-specific root so the same fragment can be reused
    /// across forms.  May contain `{{ form_code }}`.
    ///
    /// Defaults to the same value as `root_element_name` (i.e. fragments
    /// use the form-specific root unless overridden).
    #[serde(default)]
    pub fragment_bind_ref_prefix: Option<String>,

    /// Ordered rules driving [`from_aem`] generation. First match wins.
    ///
    /// Written in TOML as repeated `[[aemElements]]` tables, which — unlike the
    /// map-backed `[elements]` — preserve document order, and order is
    /// significant here: a rule matching `fragRef` plus `title` must come before
    /// the untitled fallback for the same `fragRef`.
    #[serde(default, rename = "aemElements")]
    pub aem_elements: Vec<AemElementRule>,

    /// `maxOccurs` emitted for any repeating element.
    ///
    /// UBS uses a flat 50 regardless of the AEM node's real `maxOccur`.
    #[serde(default = "default_max_occurs_value")]
    pub max_occurs_value: u32,

    /// Whether a titled page panel produces its own XSD level.
    ///
    /// UBS's own schemas are flat: their rule is that a panel contributes a level
    /// only if it repeats or came from a fragment. Our generated forms are much
    /// more field-dense, and flattening them makes many element names collide, so
    /// they fall back to ordinal suffixes — on AAGZ, 63 of 89 elements instead of
    /// 34, including twenty-one indistinguishable `AccountHolderSignature*`.
    /// Grouping by section keeps those names meaningful and confines the ordinals
    /// to one section, at the cost of a level UBS would not emit.
    ///
    /// Set `false` for output that follows UBS's rule exactly. Note that a
    /// *parsed* form's wizard steps are not marked as pages, so a schema derived
    /// from an existing package is flat either way.
    #[serde(default = "default_group_page_panels")]
    pub group_page_panels: bool,

    /// Include paths emitted first, before anything the walk discovers.
    ///
    /// UBS always includes the simple-element library, whether or not a type
    /// from it is referenced.
    #[serde(default)]
    pub always_include: Vec<String>,

    /// Default XSD type per AEM component kind, e.g. `numericbox = "xs:decimal"`.
    ///
    /// Used for a data leaf that no `[elements]` synonym and no `[[aemElements]]`
    /// rule types. Kinds are the ones listed on [`AemElementRule::kind`].
    #[serde(default)]
    pub default_types: HashMap<String, String>,
}

fn default_max_occurs_value() -> u32 {
    50
}

fn default_group_page_panels() -> bool {
    true
}

/// One ordered rule for the AEM → XSD walk.
///
/// Every match key is optional and they are ANDed. A rule with no match keys
/// matches every node, which is only ever useful as a final fallback.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AemElementRule {
    // --- match keys ---
    /// Node kind: `panel`, `repeatable`, `fragment`, `textbox`, `numericbox`,
    /// `datepicker`, `dropdownlist`, `checkbox`, `radiobutton`, or
    /// `field` for any data leaf.
    #[serde(default)]
    pub kind: Option<String>,
    /// Substring match against the node's `fragRef`.
    #[serde(default)]
    pub frag_ref: Option<String>,
    /// Exact match against the node's AEM `name`.
    #[serde(default)]
    pub name: Option<String>,
    /// Exact match (case-insensitive, trimmed) against `jcr:title` / label.
    #[serde(default)]
    pub title: Option<String>,
    /// Any of these names matches, as an alternative to a single `name`.
    #[serde(default)]
    pub names: Vec<String>,
    /// Any of these titles matches, as an alternative to a single `title`.
    #[serde(default)]
    pub titles: Vec<String>,
    /// Option-set equality, ignoring order, case and markup.
    #[serde(default)]
    pub options: Vec<String>,
    /// Match only nodes with this visibility.
    #[serde(default)]
    pub visible: Option<bool>,

    /// Choose the element by looking at the **next** sibling's `fragRef`.
    ///
    /// A partner-class radio ("Individual" / "Company/Entity") does not say
    /// which kind of partner follows it — the fragment after it does. The first
    /// entry whose `fragRef` the next sibling contains wins; when none matches,
    /// the rule's own `element` applies.
    #[serde(default)]
    pub next_fragment: Vec<NextFragmentRule>,

    // --- actions ---
    /// Drop the node (and, for a container, its whole subtree).
    #[serde(default)]
    pub ignore: bool,
    /// The XSD element name. Used verbatim — `to_pascal_case` would mangle an
    /// already-camel-cased name such as `IsNonResidentOfTaxHaven`.
    #[serde(default)]
    pub element: Option<String>,
    /// Force `name=`/`type=` with this type rather than resolving `ref=`.
    #[serde(default, rename = "type")]
    pub type_ref: Option<String>,
    /// Override the occurrence attributes.
    #[serde(default)]
    pub occurs: Option<OccursSpec>,
}

/// One `fragRef` → element pair for [`AemElementRule::next_fragment`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NextFragmentRule {
    /// Substring matched against the next sibling's `fragRef`.
    pub frag_ref: String,
    /// The element to emit when it matches.
    pub element: String,
}

/// The node facts an [`AemElementRule`] is matched against.
pub struct AemRuleSubject<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub title: &'a str,
    pub frag_ref: Option<&'a str>,
    pub options: Option<Vec<&'a str>>,
    pub visible: bool,
    /// `fragRef` of the next non-presentational sibling, if it has one.
    pub next_frag_ref: Option<&'a str>,
}

/// Normalise an option label for set comparison: strip a leading `N=` value
/// prefix and any HTML markup, collapse whitespace, lower-case.
///
/// Mirrors the UBS tool's `normalizeOptions`, so an option set written in the
/// config matches however AEM happens to have serialised it.
/// Remove HTML tags, keeping the text between them.
pub fn strip_markup(text: &str) -> String {
    if !text.contains('<') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => out.push(ch),
        }
    }
    out
}

pub fn normalize_option_label(label: &str) -> String {
    let without_prefix = match label.find('=') {
        Some(idx) if label[..idx].chars().all(|c| c.is_ascii_digit()) && idx > 0 => {
            &label[idx + 1..]
        }
        _ => label,
    };

    strip_markup(without_prefix)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl XsdProfile {
    /// The first `[[aemElements]]` rule matching `subject`, if any.
    pub fn match_aem_rule(&self, subject: &AemRuleSubject<'_>) -> Option<&AemElementRule> {
        self.aem_elements.iter().find(|rule| rule.matches(subject))
    }

    /// The default XSD type for an AEM component kind.
    pub fn default_type_for(&self, kind: &str) -> String {
        self.default_types
            .get(kind)
            .cloned()
            .unwrap_or_else(|| "xs:string".to_string())
    }
}

impl AemElementRule {
    /// The element this rule names for `subject`, if it names one.
    ///
    /// A `next_fragment` entry matching the following sibling wins over the
    /// rule's own `element`.
    pub fn element_for(&self, subject: &AemRuleSubject<'_>) -> Option<String> {
        if let Some(next) = subject.next_frag_ref {
            for candidate in &self.next_fragment {
                if next.contains(candidate.frag_ref.as_str()) {
                    return Some(candidate.element.clone());
                }
            }
        }
        self.element.clone()
    }

    fn matches(&self, subject: &AemRuleSubject<'_>) -> bool {
        if let Some(kind) = &self.kind {
            let ok = kind == subject.kind
                || (kind == "field"
                    && matches!(
                        subject.kind,
                        "textbox"
                            | "numericbox"
                            | "datepicker"
                            | "dropdownlist"
                            | "checkbox"
                            | "radiobutton"
                    ));
            if !ok {
                return false;
            }
        }

        if let Some(needle) = &self.frag_ref {
            match subject.frag_ref {
                Some(fr) if fr.contains(needle.as_str()) => {}
                _ => return false,
            }
        }

        if !self.name_matches(subject.name) {
            return false;
        }
        if !self.title_matches(subject.title) {
            return false;
        }

        if !self.options.is_empty() {
            let Some(actual) = &subject.options else {
                return false;
            };
            let expected: HashSet<String> = self
                .options
                .iter()
                .map(|o| normalize_option_label(o))
                .collect();
            let found: HashSet<String> = actual.iter().map(|o| normalize_option_label(o)).collect();
            if expected != found {
                return false;
            }
        }

        if let Some(want) = self.visible
            && want != subject.visible
        {
            return false;
        }

        true
    }

    fn name_matches(&self, actual: &str) -> bool {
        match (&self.name, self.names.is_empty()) {
            (None, true) => true,
            (Some(n), true) => n == actual,
            (None, false) => self.names.iter().any(|n| n == actual),
            (Some(n), false) => n == actual || self.names.iter().any(|x| x == actual),
        }
    }

    fn title_matches(&self, actual: &str) -> bool {
        let eq = |want: &String| want.trim().eq_ignore_ascii_case(actual.trim());
        match (&self.title, self.titles.is_empty()) {
            (None, true) => true,
            (Some(t), true) => eq(t),
            (None, false) => self.titles.iter().any(eq),
            (Some(t), false) => eq(t) || self.titles.iter().any(eq),
        }
    }
}

impl Default for XsdProfile {
    fn default() -> Self {
        Self {
            elements: HashMap::new(),
            type_aliases: HashMap::new(),
            sections: HashMap::new(),
            schema_location_prefix: default_schema_location_prefix(),
            master_language: None,
            root_element_name: default_root_element_name(),
            fragment_bind_ref_prefix: None,
            aem_elements: Vec::new(),
            max_occurs_value: default_max_occurs_value(),
            group_page_panels: default_group_page_panels(),
            always_include: Vec::new(),
            default_types: HashMap::new(),
        }
    }
}

/// Configuration for an element synonym mapping.
///
/// When a field label matches one of the `synonyms`, the resulting XSD
/// element uses the canonical name (the TOML key) and the specified type.
#[derive(Debug, Clone, Deserialize)]
pub struct ElementMapping {
    /// Synonym strings to match against field labels (case-insensitive substring).
    pub synonyms: Vec<String>,

    /// XSD type for this element (e.g. `"xs:string"`, `"xs:decimal"`, or a
    /// predefined type name like `"CurrencyType"`).
    #[serde(rename = "type")]
    pub type_ref: String,
}

/// Configuration for a section name override.
///
/// When a section's full text content (heading + body) matches one of the
/// `patterns` (regex, case-insensitive), the resulting XSD element uses
/// the canonical name (the TOML key) instead of the heading-derived name.
#[derive(Debug, Clone, Deserialize)]
pub struct SectionMapping {
    /// Regex patterns to match against the full section text.
    /// Matched case-insensitively.
    pub patterns: Vec<String>,
}

// ============================================================================
// Registered complex types (auto-discovered from types/ directory)
// ============================================================================

/// A child element within a registered complex type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeChildElement {
    /// Element name (e.g. `"City"`).
    pub name: String,
    /// Element type (e.g. `"xs:string"`).
    pub type_ref: String,
}

/// A complex type registered from the `types/` directory.
///
/// Contains the flattened list of child elements (including those inherited
/// via `xs:extension base`), and the `schemaLocation` path of the file that
/// declares it.
#[derive(Debug, Clone)]
pub struct RegisteredComplexType {
    /// The complexType name (e.g. `"SignatureType"`).
    pub name: String,
    /// Flattened child elements (name + type), including inherited elements.
    pub elements: Vec<TypeChildElement>,
    /// The `schemaLocation` path for the file that declares this type.
    pub file: String,
}

// ============================================================================
// Resolved config (ready for generation)
// ============================================================================

/// Resolved XSD configuration, ready for schema generation.
///
/// Contains the parsed profile, a name→file index, and a registry of
/// complex types with their child elements for auto-matching.
#[derive(Debug, Clone)]
pub struct XsdConfig {
    /// The parsed profile.
    pub profile: XsdProfile,

    /// Index from declared type/element name → `schemaLocation` path.
    ///
    /// Built from all `*.xsd` files found recursively under `types/`
    /// using [`extract_declared_names`]. Only entries whose key appears in
    /// `used_type_refs` during generation produce an `<xs:include>` directive.
    pub type_to_file: HashMap<String, String>,

    /// Registry of complex types with their flattened child elements.
    ///
    /// Built by parsing all `*.xsd` files in `types/`, extracting
    /// `xs:complexType` definitions with their child elements, and resolving
    /// `xs:extension base` inheritance and `xs:element ref` references.
    pub registered_types: HashMap<String, RegisteredComplexType>,

    /// Reverse map from complex type name → global element name.
    ///
    /// E.g. `"AddressType" → "Address"`, `"IndividualBasicType" → "IndividualBasic"`.
    /// Built from global element declarations in the parsed XSD files.
    pub type_to_element_name: HashMap<String, String>,

    /// Every fragment in the profile's library: `fragRef` → the XSD type it
    /// binds to (its `fragmentModelRoot`).
    ///
    /// Deliberately independent of the AEM profile's `fragment_paths`. That
    /// setting scopes which fragments the converter may *substitute* while
    /// building a form; resolving a `fragRef` that a form already references is a
    /// different question, and a legacy form referencing, say,
    /// `afforms_ch_fragmentlib` must still get its elements.
    pub fragment_types: HashMap<String, String>,

    /// Every global element declaration: element name → type name.
    ///
    /// E.g. `"BankingRelationship" → "BankingRelationshipType"`. Membership in
    /// this map is what decides whether an element is emitted as
    /// `<xs:element ref="X"/>` (declared globally) or as
    /// `<xs:element name="X" type="T"/>` (not declared globally).
    pub global_elements: HashMap<String, String>,

    /// Optional master language code (e.g. `"en"`).
    ///
    /// When set, element names derived from headings and field labels will
    /// prefer the translation in this language instead of picking an
    /// arbitrary first entry from the translation map.
    pub master_language: Option<String>,

    /// Optional form code (e.g. `"ABFA"`).
    ///
    /// Used to expand `{{ form_code }}` in the profile's `root_element_name`
    /// template.
    pub form_code: Option<String>,
}

impl XsdConfig {
    /// Build an `XsdConfig` from a profile, a type-to-file index, and registered types.
    pub fn new(
        profile: XsdProfile,
        type_to_file: HashMap<String, String>,
        registered_types: HashMap<String, RegisteredComplexType>,
        type_to_element_name: HashMap<String, String>,
    ) -> Self {
        let master_language = profile.master_language.clone();
        Self {
            profile,
            type_to_file,
            registered_types,
            type_to_element_name,
            fragment_types: HashMap::new(),
            global_elements: HashMap::new(),
            master_language,
            form_code: None,
        }
    }

    /// Build an `XsdConfig` from just a profile (empty type index and registry).
    pub fn from_profile(profile: XsdProfile) -> Self {
        let master_language = profile.master_language.clone();
        Self {
            profile,
            type_to_file: HashMap::new(),
            registered_types: HashMap::new(),
            type_to_element_name: HashMap::new(),
            fragment_types: HashMap::new(),
            global_elements: HashMap::new(),
            master_language,
            form_code: None,
        }
    }

    /// Set the fragment library index (`fragRef` → XSD type).
    pub fn with_fragment_types(mut self, fragment_types: HashMap<String, String>) -> Self {
        self.fragment_types = fragment_types;
        self
    }

    /// Set the global element declarations (element name → type name).
    pub fn with_global_elements(mut self, global_elements: HashMap<String, String>) -> Self {
        self.global_elements = global_elements;
        self
    }

    /// Whether `name` is declared as a global element in the type library.
    ///
    /// Global elements are referenced with `<xs:element ref="..."/>`; everything
    /// else is declared inline with `name=`/`type=`.
    pub fn is_global_element(&self, name: &str) -> bool {
        self.global_elements.contains_key(name)
    }

    /// Set the master language for element name resolution.
    pub fn with_master_language(mut self, lang: impl Into<String>) -> Self {
        self.master_language = Some(lang.into());
        self
    }

    /// Set the form code for root element name expansion.
    pub fn with_form_code(mut self, code: impl Into<String>) -> Self {
        self.form_code = Some(code.into());
        self
    }

    /// Render a profile template string, exposing `form_code` to Tera.
    ///
    /// When no form code is set, `form_code` is bound to an empty string so
    /// `{{ form_code }}` renders as empty. If the template has a syntax error,
    /// the raw template is returned unchanged.
    fn render_template(&self, template: &str) -> String {
        let mut ctx = tera::Context::new();
        ctx.insert("form_code", self.form_code.as_deref().unwrap_or(""));
        tera::Tera::one_off(template, &ctx, false).unwrap_or_else(|_| template.to_string())
    }

    /// Compute the root element name by rendering the profile's
    /// `root_element_name` template (e.g. `UBSAF_{{ form_code }}`).
    pub fn root_element_name(&self) -> String {
        self.render_template(&self.profile.root_element_name)
    }

    /// Compute the fragment bind-ref prefix.
    ///
    /// If the profile specifies `fragmentBindRefPrefix`, render it as a Tera
    /// template. Otherwise fall back to the root element name (so fragments
    /// use the same root as the form by default).
    pub fn fragment_bind_ref_prefix(&self) -> String {
        match &self.profile.fragment_bind_ref_prefix {
            Some(template) => self.render_template(template),
            None => self.root_element_name(),
        }
    }
}

/// Extract the names of all globally declared `xs:complexType`, `xs:simpleType`,
/// and `xs:element` definitions from an XSD file's text content.
///
/// Uses a simple line-by-line scan rather than full XML parsing.
/// Each matched line must start (after trimming) with one of the three tag
/// prefixes and contain a `name="..."` attribute on the same line.
///
/// # Example
///
/// Given the fragment:
/// ```xml
/// <xs:complexType name="SignatureType">
///   <xs:sequence>
///     <xs:element name="Place" type="xs:string" minOccurs="0"/>
/// ```
/// This returns `["SignatureType"]` (the element `"Place"` is not a global
/// declaration so it is typically inside a complexType body and still matched;
/// callers should ensure the file only contains global-scope declarations that
/// they actually want indexed, which is true for the `types/` convention).
pub fn extract_declared_names(content: &str) -> Vec<String> {
    const PREFIXES: &[&str] = &["<xs:complexType ", "<xs:simpleType ", "<xs:element "];

    let mut names = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
            continue;
        }
        if let Some(name_start) = trimmed.find("name=\"") {
            let after = &trimmed[name_start + 6..];
            if let Some(name_end) = after.find('"') {
                names.push(after[..name_end].to_string());
            }
        }
    }
    names
}

// ============================================================================
// XSD type parsing (using quick-xml)
// ============================================================================

/// Intermediate representation of a child element within a complexType,
/// before inheritance and ref resolution.
#[derive(Debug, Clone)]
enum RawChild {
    /// A child with explicit name and type: `<xs:element name="X" type="Y"/>`.
    Named { name: String, type_ref: String },
    /// A child referencing a global element: `<xs:element ref="X"/>`.
    Ref(String),
}

/// Intermediate representation of a complexType parsed from an XSD file.
#[derive(Debug, Clone)]
struct RawComplexType {
    /// The complexType name.
    name: String,
    /// Optional base type name from `xs:extension base="..."`.
    base: Option<String>,
    /// Direct child elements (before inheritance resolution).
    children: Vec<RawChild>,
}

/// Intermediate result of parsing a single XSD file.
#[derive(Debug, Clone)]
pub struct ParsedSchema {
    /// Complex types defined in this file.
    complex_types: Vec<RawComplexType>,
    /// Global element declarations: name → type.
    global_elements: HashMap<String, String>,
}

/// Parse an XSD file and extract complex types with their children
/// and global element declarations.
pub fn parse_schema(content: &str) -> ParsedSchema {
    let mut reader = Reader::from_str(content);

    let mut complex_types = Vec::new();
    let mut global_elements = HashMap::new();

    // Parsing state
    let mut depth: u32 = 0;
    let mut in_complex_type = false;
    let mut current_ct_name = String::new();
    let mut current_ct_base: Option<String> = None;
    let mut current_ct_children: Vec<RawChild> = Vec::new();
    let mut ct_start_depth: u32 = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                depth += 1;
                let local_name = e.local_name();
                let tag = std::str::from_utf8(local_name.as_ref()).unwrap_or("");

                match tag {
                    "complexType" => {
                        if let Some(name) = get_attr(e, b"name") {
                            in_complex_type = true;
                            current_ct_name = name;
                            current_ct_base = None;
                            current_ct_children = Vec::new();
                            ct_start_depth = depth;
                        }
                    }
                    "extension" if in_complex_type => {
                        if let Some(base) = get_attr(e, b"base") {
                            current_ct_base = Some(base);
                        }
                    }
                    "element" if in_complex_type => {
                        // Child element within a complexType
                        if let Some(ref_name) = get_attr(e, b"ref") {
                            current_ct_children.push(RawChild::Ref(ref_name));
                        } else if let Some(name) = get_attr(e, b"name") {
                            let type_ref =
                                get_attr(e, b"type").unwrap_or_else(|| "xs:string".to_string());
                            current_ct_children.push(RawChild::Named { name, type_ref });
                        }
                    }
                    "element" if !in_complex_type && depth <= 2 => {
                        // Global element declaration
                        if let Some(name) = get_attr(e, b"name")
                            && let Some(type_ref) = get_attr(e, b"type")
                        {
                            global_elements.insert(name, type_ref);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local_name = e.local_name();
                let tag = std::str::from_utf8(local_name.as_ref()).unwrap_or("");

                if tag == "element" {
                    if in_complex_type {
                        if let Some(ref_name) = get_attr(e, b"ref") {
                            current_ct_children.push(RawChild::Ref(ref_name));
                        } else if let Some(name) = get_attr(e, b"name") {
                            let type_ref =
                                get_attr(e, b"type").unwrap_or_else(|| "xs:string".to_string());
                            current_ct_children.push(RawChild::Named { name, type_ref });
                        }
                    } else if depth <= 1 {
                        // Global element declaration (self-closing)
                        if let Some(name) = get_attr(e, b"name")
                            && let Some(type_ref) = get_attr(e, b"type")
                        {
                            global_elements.insert(name, type_ref);
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                if in_complex_type && depth == ct_start_depth {
                    // Close current complexType
                    complex_types.push(RawComplexType {
                        name: current_ct_name.clone(),
                        base: current_ct_base.take(),
                        children: std::mem::take(&mut current_ct_children),
                    });
                    in_complex_type = false;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    ParsedSchema {
        complex_types,
        global_elements,
    }
}

/// Extract an attribute value from a quick-xml event element.
fn get_attr(e: &quick_xml::events::BytesStart, attr_name: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == attr_name {
            return std::str::from_utf8(&attr.value).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Build the registered complex types registry from multiple parsed schemas.
///
/// This function:
/// 1. Collects all complex types and global elements from all parsed schemas.
/// 2. Resolves `xs:element ref="X"` references using global element declarations.
/// 3. Resolves `xs:extension base="Y"` inheritance by prepending base type elements.
/// 4. Associates each type with its `schemaLocation` path.
///
/// Returns `(registered_types, type_to_element_name, global_elements)`:
/// - `type_to_element_name` maps complex type names to a global element name
///   (e.g. `"AddressType" → "Address"`); when several elements share a type the
///   alphabetically first one wins, so the result is deterministic.
/// - `global_elements` maps every global element name to its type. This is the
///   forward direction and is what decides `xs:element ref=` versus
///   `xs:element name=`/`type=`.
pub fn build_registered_types(
    parsed_schemas: &[(ParsedSchema, String)], // (schema, schemaLocation)
) -> (
    HashMap<String, RegisteredComplexType>,
    HashMap<String, String>,
    HashMap<String, String>,
) {
    // Collect all global elements across all files (for ref resolution)
    let mut all_global_elements: HashMap<String, String> = HashMap::new();
    for (schema, _) in parsed_schemas {
        for (name, type_ref) in &schema.global_elements {
            all_global_elements.insert(name.clone(), type_ref.clone());
        }
    }

    // Collect all raw complex types with their file paths
    let mut raw_types: HashMap<String, (RawComplexType, String)> = HashMap::new();
    for (schema, file) in parsed_schemas {
        for ct in &schema.complex_types {
            raw_types.insert(ct.name.clone(), (ct.clone(), file.clone()));
        }
    }

    // Resolve children: first resolve refs, then resolve inheritance
    let mut resolved: HashMap<String, RegisteredComplexType> = HashMap::new();

    // We need iterative resolution since inheritance can be multi-level
    // First pass: resolve refs for all types (without inheritance)
    let mut direct_elements: HashMap<String, Vec<TypeChildElement>> = HashMap::new();
    for (name, (ct, _)) in &raw_types {
        let mut elements = Vec::new();
        for child in &ct.children {
            match child {
                RawChild::Named {
                    name: n,
                    type_ref: t,
                } => {
                    elements.push(TypeChildElement {
                        name: n.clone(),
                        type_ref: t.clone(),
                    });
                }
                RawChild::Ref(ref_name) => {
                    // Look up the global element to get its type
                    let type_ref = all_global_elements
                        .get(ref_name)
                        .cloned()
                        .unwrap_or_else(|| "xs:string".to_string());
                    elements.push(TypeChildElement {
                        name: ref_name.clone(),
                        type_ref,
                    });
                }
            }
        }
        direct_elements.insert(name.clone(), elements);
    }

    // Resolve inheritance iteratively (handles multi-level chains)
    let mut inherited_elements: HashMap<String, Vec<TypeChildElement>> = HashMap::new();
    let max_iterations = raw_types.len();
    for _ in 0..max_iterations {
        let mut progress = false;
        for (name, (ct, _)) in &raw_types {
            if inherited_elements.contains_key(name) {
                continue;
            }
            match &ct.base {
                None => {
                    // No base → just use direct elements
                    inherited_elements.insert(name.clone(), direct_elements[name].clone());
                    progress = true;
                }
                Some(base_name) => {
                    // Base must be resolved first
                    if let Some(base_elements) = inherited_elements.get(base_name) {
                        let mut all_elements = base_elements.clone();
                        all_elements.extend(direct_elements[name].iter().cloned());
                        inherited_elements.insert(name.clone(), all_elements);
                        progress = true;
                    }
                    // else: base not yet resolved, try next iteration
                }
            }
        }
        if !progress {
            break;
        }
    }

    // Handle any types whose base couldn't be resolved (missing base type)
    for (name, (ct, _)) in &raw_types {
        if !inherited_elements.contains_key(name) {
            // Base type not found — use direct elements only
            inherited_elements.insert(name.clone(), direct_elements[name].clone());
            if ct.base.is_some() {
                // Silently ignore missing base types
            }
        }
    }

    // Build the final registry
    for (name, (_, file)) in &raw_types {
        if let Some(elements) = inherited_elements.get(name) {
            resolved.insert(
                name.clone(),
                RegisteredComplexType {
                    name: name.clone(),
                    elements: elements.clone(),
                    file: file.clone(),
                },
            );
        }
    }

    // Build reverse map: type name → element name (e.g. "AddressType" → "Address").
    //
    // Several global elements may share one type: `AFFragments/Signature.xsd`
    // declares `Signature`, `CardHolderSignature`, `CardHolderPartnerSignature`
    // and `CardLegalRepSignature`, all of `SignatureType`. Which one a given
    // fragment should become is a per-usage decision the type library cannot
    // answer — that is what an `[[aemElements]]` rule is for — so this map only
    // provides a neutral default:
    //
    //   1. the type name minus its `Type` suffix, when that is itself a global
    //      element (`SignatureType` → `Signature`)
    //   2. otherwise the alphabetically first, so the result never depends on
    //      `HashMap` iteration order
    let mut by_name: Vec<(&String, &String)> = all_global_elements.iter().collect();
    by_name.sort_unstable();
    let mut type_to_element_name: HashMap<String, String> = HashMap::new();
    for (elem_name, type_name) in &by_name {
        type_to_element_name
            .entry((*type_name).clone())
            .or_insert_with(|| (*elem_name).clone());
    }
    for (_, type_name) in &by_name {
        let canonical = type_name.trim_end_matches("Type");
        if !canonical.is_empty() && all_global_elements.contains_key(canonical) {
            type_to_element_name.insert((*type_name).clone(), canonical.to_string());
        }
    }

    (resolved, type_to_element_name, all_global_elements)
}

/// Build a full [`XsdConfig`] from pre-discovered type source files.
///
/// `sources` is a list of `(relative_path_from_types_dir, xsd_content)` pairs,
/// for example `("AFFragments/Address.xsd", "<xs:schema ...")`.
pub fn build_xsd_config_from_type_sources(
    profile: XsdProfile,
    sources: &[(String, String)],
) -> XsdConfig {
    let mut type_to_file = HashMap::new();
    let mut parsed_schemas = Vec::new();

    for (relative_path, content) in sources {
        let schema_location = format!("{}{}", profile.schema_location_prefix, relative_path);
        for name in extract_declared_names(content) {
            type_to_file.insert(name, schema_location.clone());
        }
        parsed_schemas.push((parse_schema(content), schema_location));
    }

    let (registered_types, type_to_element_name, global_elements) =
        build_registered_types(&parsed_schemas);
    XsdConfig::new(
        profile,
        type_to_file,
        registered_types,
        type_to_element_name,
    )
    .with_global_elements(global_elements)
}

#[cfg(test)]
mod tests {
    use super::{build_registered_types, parse_schema, to_pascal_case, to_xsd_element_name};

    #[test]
    fn to_xsd_element_name_joins_words_without_separators() {
        assert_eq!(to_xsd_element_name("Email address"), "EmailAddress");
        assert_eq!(to_xsd_element_name("Domain name"), "DomainName");
        assert_eq!(to_xsd_element_name("IBAN"), "IBAN");
    }

    #[test]
    fn to_xsd_element_name_maps_slash_to_or() {
        assert_eq!(to_xsd_element_name("Company/Entity"), "CompanyOrEntity");
    }

    /// The reason this helper exists at all: `to_pascal_case` lower-cases the
    /// tail of every word, which mangles an already-camel-cased name.
    #[test]
    fn to_xsd_element_name_preserves_inner_capitals_unlike_to_pascal_case() {
        assert_eq!(
            to_xsd_element_name("IsNonResidentOfTaxHaven"),
            "IsNonResidentOfTaxHaven"
        );
        assert_eq!(
            to_pascal_case("IsNonResidentOfTaxHaven"),
            "Isnonresidentoftaxhaven"
        );
    }

    #[test]
    fn to_xsd_element_name_falls_back_for_empty_input() {
        assert_eq!(to_xsd_element_name("   "), "Unknown");
    }

    /// Several global elements may share one complex type. The winner must not
    /// depend on `HashMap` iteration order, or the generated schema changes
    /// between runs.
    #[test]
    fn type_to_element_name_prefers_the_canonical_element_deterministically() {
        let schema = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    <xs:element name="Signature" type="SignatureType"/>
    <xs:element name="CardHolderSignature" type="SignatureType"/>
    <xs:element name="CardLegalRepSignature" type="SignatureType"/>
    <xs:element name="AccountHolder" type="ContractualPartnerType"/>
    <xs:element name="Lessee" type="ContractualPartnerType"/>
    <xs:complexType name="SignatureType">
        <xs:sequence>
            <xs:element name="Place" type="xs:string"/>
        </xs:sequence>
    </xs:complexType>
</xs:schema>"#;

        let parsed = vec![(
            parse_schema(schema),
            "../AFFragments/Signature.xsd".to_string(),
        )];
        let (_, type_to_element_name, global_elements) = build_registered_types(&parsed);

        // `SignatureType` → `Signature`, not the alphabetically first
        // `CardHolderSignature`: defaulting every signature fragment to a
        // card-specific element would be plausible and wrong.
        assert_eq!(
            type_to_element_name
                .get("SignatureType")
                .map(String::as_str),
            Some("Signature")
        );

        // With no element named after the type, the alphabetically first wins —
        // arbitrary, but never dependent on `HashMap` iteration order.
        assert_eq!(
            type_to_element_name
                .get("ContractualPartnerType")
                .map(String::as_str),
            Some("AccountHolder")
        );

        // The forward map keeps all of them, which is what `ref=` resolution needs.
        assert_eq!(global_elements.len(), 5);
        assert_eq!(
            global_elements.get("Signature").map(String::as_str),
            Some("SignatureType")
        );
    }
}

// ============================================================================
// Complex type matching
// ============================================================================

/// Find the best set of 1..n pairwise-disjoint registered complex types
/// whose combined elements cover all `children`.
///
/// **Disjoint** means no two selected types share an element with the same
/// `(name, type_ref)` pair.
///
/// **Ranking** – among all valid covers, pick the one with the fewest total
/// type elements (tightest fit).  Every selected type must contribute at
/// least one child.
///
/// Returns an empty `Vec` if no cover exists or `children` is empty.
/// Collect the leaf element names of a registered complex type by recursively
/// expanding children whose `type_ref` is itself a registered complex type.
fn collect_leaf_names(
    rt: &RegisteredComplexType,
    registered_types: &HashMap<String, RegisteredComplexType>,
) -> HashSet<String> {
    let mut leaves = HashSet::new();
    let mut stack: Vec<&RegisteredComplexType> = vec![rt];
    let mut visited = HashSet::new();
    while let Some(current) = stack.pop() {
        if !visited.insert(&current.name as &str as *const str) {
            continue;
        }
        for elem in &current.elements {
            if let Some(child_type) = registered_types.get(&elem.type_ref) {
                stack.push(child_type);
            } else {
                leaves.insert(elem.name.clone());
            }
        }
    }
    leaves
}

pub fn find_matching_types<'a>(
    children: &[(String, String)],
    registered_types: &'a HashMap<String, RegisteredComplexType>,
) -> Vec<&'a RegisteredComplexType> {
    if children.is_empty() {
        return Vec::new();
    }

    // Pre-filter: keep only types that cover at least one child.
    let candidates: Vec<&RegisteredComplexType> = registered_types
        .values()
        .filter(|rt| {
            children.iter().any(|(name, type_ref)| {
                rt.elements
                    .iter()
                    .any(|e| e.name == *name && e.type_ref == *type_ref)
            })
        })
        .collect();

    if candidates.is_empty() {
        return Vec::new();
    }

    // Pre-compute leaf element names for each candidate (recursively expanded).
    let candidate_leaves: Vec<HashSet<String>> = candidates
        .iter()
        .map(|rt| collect_leaf_names(rt, registered_types))
        .collect();

    // For each candidate, compute a bitmask of which children it covers.
    let child_masks: Vec<u64> = candidates
        .iter()
        .map(|rt| {
            let mut mask = 0u64;
            for (i, (name, type_ref)) in children.iter().enumerate() {
                if rt
                    .elements
                    .iter()
                    .any(|e| e.name == *name && e.type_ref == *type_ref)
                {
                    mask |= 1u64 << i;
                }
            }
            mask
        })
        .collect();

    let full_mask = (1u64 << children.len()) - 1;

    // Best solution found so far: (total_elements, indices).
    let mut best: Option<(usize, Vec<usize>)> = None;

    /// The fixed inputs of the cover search.
    struct Cover<'a> {
        candidates: &'a [&'a RegisteredComplexType],
        candidate_leaves: &'a [HashSet<String>],
        child_masks: &'a [u64],
        full_mask: u64,
    }

    impl Cover<'_> {
        /// Recursive search with pruning.
        fn search(
            &self,
            start: usize,
            covered: u64,
            selected: &mut Vec<usize>,
            total_elems: usize,
            best: &mut Option<(usize, Vec<usize>)>,
        ) {
            if covered == self.full_mask {
                // Valid cover — check if it's better than the best.
                if best
                    .as_ref()
                    .is_none_or(|(best_total, _)| total_elems < *best_total)
                {
                    *best = Some((total_elems, selected.clone()));
                }
                return;
            }

            // Prune: if current total already >= best, stop.
            if let Some((best_total, _)) = best
                && total_elems >= *best_total
            {
                return;
            }

            for i in start..self.candidates.len() {
                let mask_i = self.child_masks[i];

                // Must contribute at least one new child.
                if mask_i & !covered == 0 {
                    continue;
                }

                // Must be leaf-disjoint with all already-selected types:
                // no shared leaf element names after recursive expansion.
                let disjoint = selected
                    .iter()
                    .all(|&j| self.candidate_leaves[i].is_disjoint(&self.candidate_leaves[j]));
                if !disjoint {
                    continue;
                }

                selected.push(i);
                self.search(
                    i + 1,
                    covered | mask_i,
                    selected,
                    total_elems + self.candidates[i].elements.len(),
                    best,
                );
                selected.pop();
            }
        }
    }

    let mut selected = Vec::new();
    Cover {
        candidates: &candidates,
        candidate_leaves: &candidate_leaves,
        child_masks: &child_masks,
        full_mask,
    }
    .search(0, 0, &mut selected, 0, &mut best);

    match best {
        Some((_, indices)) => indices.iter().map(|&i| candidates[i]).collect(),
        None => Vec::new(),
    }
}

// ============================================================================
// Synonym resolution helpers
// ============================================================================

/// Result of resolving a field label against the `[elements]` config.
#[derive(Debug, Clone)]
pub struct ResolvedElement {
    /// The canonical name (TOML key).
    pub name: String,
    /// The XSD type.
    pub type_ref: String,
}

/// Attempt to resolve a label against the `[elements]` config.
///
/// Finds the best match by picking the longest matching synonym
/// (most specific). Matching is case-insensitive substring.
pub fn resolve_element(label: &str, profile: &XsdProfile) -> Option<ResolvedElement> {
    resolve_element_matching(label, profile, SynonymMatch::Substring)
}

/// Resolve a field label against `[elements]`, requiring the **whole** label to
/// equal a synonym (case-insensitive, trimmed).
///
/// This is what names elements in the generated schema. Substring matching is
/// unusable there: labels in these forms are often whole sentences, and the
/// table holds two-letter synonyms, so `"UNKNOWN"` matches `"No"` →
/// `StreetNumber` and `"…istanza di fallimento in data ;"` matches `"Data"` →
/// `Date`. Whole-label matching keeps the real wins — `Straße` → `Street`,
/// `Cognome` → `LastName`, `Data` → `Date`/`xs:date` — and fires on no sentence.
pub fn resolve_element_whole_label(label: &str, profile: &XsdProfile) -> Option<ResolvedElement> {
    resolve_element_matching(label, profile, SynonymMatch::WholeLabel)
}

/// How a synonym is compared against a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SynonymMatch {
    /// The synonym appears anywhere in the label. Kept for
    /// [`compute_bind_refs`], whose leaf names feed fragment matching.
    Substring,
    /// The trimmed label equals the synonym.
    WholeLabel,
}

fn resolve_element_matching(
    label: &str,
    profile: &XsdProfile,
    mode: SynonymMatch,
) -> Option<ResolvedElement> {
    let label_lower = label.trim().to_lowercase();
    let mut best: Option<(usize, ResolvedElement)> = None;
    for (name, mapping) in &profile.elements {
        for synonym in &mapping.synonyms {
            let syn_lower = synonym.trim().to_lowercase();
            let hit = match mode {
                SynonymMatch::Substring => label_lower.contains(&syn_lower),
                SynonymMatch::WholeLabel => label_lower == syn_lower,
            };
            if hit {
                let len = syn_lower.len();
                if best.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
                    best = Some((
                        len,
                        ResolvedElement {
                            name: name.clone(),
                            type_ref: mapping.type_ref.clone(),
                        },
                    ));
                }
            }
        }
    }
    best.map(|(_, resolved)| resolved)
}

/// Attempt to resolve a section name by matching the full section text against
/// the `[sections]` config patterns.
///
/// Finds the best match by picking the pattern whose regex match is longest
/// (most specific). Patterns are matched case-insensitively.
///
/// Returns `Some(configured_name)` if a pattern matches, `None` otherwise.
pub fn resolve_section_name(section_text: &str, profile: &XsdProfile) -> Option<String> {
    resolve_section_name_with_heading(section_text, None, profile)
}

/// Resolve a section name by matching patterns against heading and body text.
///
/// If `heading_text` is provided, patterns are first tried against only the
/// heading. Any heading match wins over a body-text-only match regardless of
/// match length. Among heading-only matches (or body-only matches), the longest
/// match wins.
///
/// When no heading pattern matches but the heading appears "meaningful" (not
/// just a step number or generic prefix), body-text matches are suppressed
/// to let the PascalCase fallback take over.
pub fn resolve_section_name_with_heading(
    section_text: &str,
    heading_text: Option<&str>,
    profile: &XsdProfile,
) -> Option<String> {
    let mut best_heading: Option<(usize, String)> = None;
    let mut best_body: Option<(usize, String)> = None;

    for (name, mapping) in &profile.sections {
        for pattern in &mapping.patterns {
            let case_insensitive_pattern = format!("(?i){}", pattern);
            if let Ok(re) = regex_lite::Regex::new(&case_insensitive_pattern) {
                // Try heading first (higher priority)
                if let Some(ht) = heading_text
                    && let Some(m) = re.find(ht)
                {
                    let len = m.len();
                    if best_heading
                        .as_ref()
                        .is_none_or(|(best_len, _)| len > *best_len)
                    {
                        best_heading = Some((len, name.clone()));
                    }
                }
                // Try full text (lower priority)
                if let Some(m) = re.find(section_text) {
                    let len = m.len();
                    if best_body
                        .as_ref()
                        .is_none_or(|(best_len, _)| len > *best_len)
                    {
                        best_body = Some((len, name.clone()));
                    }
                }
            }
        }
    }

    // Heading matches always take priority
    if best_heading.is_some() {
        return best_heading.map(|(_, name)| name);
    }

    // Body-text matches only apply when the heading is generic (a step number,
    // a bare digit, etc.) — otherwise let the caller fall back to PascalCase.
    if let Some(ht) = heading_text
        && !is_generic_heading(ht)
    {
        return None;
    }

    best_body.map(|(_, name)| name)
}

/// Returns true if a heading is "generic" (not descriptive enough to use as a
/// section name). Generic headings are e.g. "Step 4", "1", "Schritt 2", etc.
fn is_generic_heading(heading: &str) -> bool {
    let trimmed = heading.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Pure digits or digits with a trailing dot/paren
    if trimmed
        .trim_end_matches(['.', ')', ' '])
        .chars()
        .all(|c| c.is_ascii_digit())
    {
        return true;
    }
    // "Step N", "Schritt N", "Étape N", "Fase N", "Paso N"
    let lower = trimmed.to_lowercase();
    let prefixes = ["step", "schritt", "étape", "fase", "paso"];
    for prefix in &prefixes {
        if let Some(stripped) = lower.strip_prefix(prefix) {
            let rest = stripped.trim_start();
            if rest.is_empty() || rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

/// Convert a label string to a snake_case identifier suitable for XSD names.
///
/// - Strips non-alphanumeric characters
/// - Splits on whitespace
/// - All words are lowercased and joined with underscores
///
/// Example: `"Date of Birth"` → `"date_of_birth"`
pub fn to_snake_case(label: &str) -> String {
    let words: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    if words.is_empty() {
        return "unknown".to_string();
    }

    words
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Derive an XSD element name from a label, using UBS normalisation rules.
///
/// Unlike [`to_pascal_case`], the tail of each word is preserved verbatim, so
/// an already-camel-cased label survives the round trip:
/// `to_pascal_case("IsNonResidentOfTaxHaven")` yields `Isnonresidentoftaxhaven`,
/// which is why config-supplied names must never go through it.
///
/// - `/` becomes ` Or `, so `"Company/Entity"` → `"CompanyOrEntity"`
/// - the label is split on non-alphanumeric characters
/// - each segment's first character is upper-cased; the rest is left alone
///
/// Example: `"Email address"` → `"EmailAddress"`, `"IBAN"` → `"IBAN"`.
pub fn to_xsd_element_name(label: &str) -> String {
    // AEM labels are rich text, so a label can carry markup. Left in, the tag
    // names become part of the element name: `<b>South Korea</b>/<b>…` yields
    // `BSouthKoreaOrBInstitutional…`, with a stray `B` per tag.
    let replaced = strip_markup(label).replace('/', " Or ");
    let segments: Vec<&str> = replaced
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    if segments.is_empty() {
        return "Unknown".to_string();
    }

    segments
        .iter()
        .map(|seg| {
            let mut chars = seg.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Derive an element name from an AEM component `name`, for a field whose label
/// cannot supply one.
///
/// Names this engine generates look like `CB_Erbschaft_f20c853f` — a prefix, the
/// camel-cased source text, and eight hex characters of the node uuid. When that
/// shape is recognised the scaffolding is stripped, leaving `Erbschaft`.
/// Otherwise the name came from somewhere else (a hand-authored form uses
/// `BG_SELECTION`, `emailCheckbox1`) and is used whole.
///
/// Returns `None` when nothing usable remains.
pub fn element_name_from_component_name(name: &str) -> Option<String> {
    let is_short_uuid = |s: &str| {
        s.len() == 8
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
    };

    let segments: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();
    let generated = segments.len() >= 2 && segments.last().is_some_and(|s| is_short_uuid(s));

    let mut kept: Vec<&str> = if generated {
        // Drop the uuid tail and the component-kind prefix.
        segments[1..segments.len() - 1].to_vec()
    } else {
        segments.clone()
    };

    // A field with no label, no options and no name text — `TXT_c008f79a` —
    // leaves nothing after stripping. It still carries data, so name it after the
    // whole component rather than dropping it from the schema; the uuid is
    // deterministic, so the name is stable.
    if kept.is_empty() {
        kept = segments;
    }

    let derived = to_xsd_element_name(&kept.join(" "));
    (!derived.is_empty() && derived != "Unknown").then_some(derived)
}

/// Convert a label string to a PascalCase identifier suitable for XSD names.
///
/// - Strips non-alphanumeric characters
/// - Splits on whitespace / punctuation boundaries
/// - Each word is title-cased (first letter uppercase, rest lowercase)
///   unless the word is fully uppercase (treated as an acronym and kept as-is)
///
/// Example: `"Date of Birth"` → `"DateOfBirth"`
/// Example: `"IBAN"` → `"IBAN"`
pub fn to_pascal_case(label: &str) -> String {
    let words: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    if words.is_empty() {
        return "Unknown".to_string();
    }

    words
        .iter()
        .map(|w| {
            if w.chars().all(|c| c.is_uppercase() || !c.is_alphabetic()) {
                // Acronym or number — keep as-is
                w.to_string()
            } else {
                // Title-case
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        let upper: String = first.to_uppercase().collect();
                        upper + &chars.as_str().to_lowercase()
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join("")
}
