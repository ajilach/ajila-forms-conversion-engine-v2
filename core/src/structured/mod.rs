pub mod contact_field;
pub mod element_merge;
pub mod inline_html;
mod merge_engine;
mod merger;
mod structured_converter;
pub mod table_html;
mod translation_merger;

pub use element_merge::{
    MergeError as ElementMergeError, can_merge, can_merge_all, merge_nodes, merge_two,
};
pub use inline_html::{
    AEM_TAGS, InlineHtmlTags, QUILL_TAGS, inline_nodes_to_html_with, inline_text_to_html_with,
    strip_footnote_marker,
};
pub use merger::{MergeInput, RecursiveMerger, Selection, SelectionKind};
pub use structured_converter::{convert, convert_with_context};
pub use table_html::{
    render_cell_html, render_plain_list_html, render_table_html,
};
pub use translation_merger::{
    MergeError, MergeError as TranslationMergeError, calculate_structural_similarity,
    merge_translations,
};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

use crate::context::Context;
use crate::xfa::scripting::SomPath;

/// A map of language code → optional translated text.
///
/// `None` means "translation not provided"; `Some(text)` is the actual content.
/// Used by both `InlineNode::TranslatedText` and `TranslatableString::Translated`.
pub type TranslationMap = HashMap<String, Option<String>>;

// ── Semantic matching context (feature-gated) ────────────────────────────────

/// Opaque semantic matching context threaded through translation merge.
///
/// When the `semantic-matching` feature is enabled, this is an alias for
/// [`crate::semantic::SemanticMatcher`].  Otherwise it is a zero-sized dummy
/// type so that function signatures remain identical in both configurations.
#[cfg(feature = "semantic-matching")]
pub type SemanticCtx = crate::semantic::SemanticMatcher;

/// Dummy zero-sized type when semantic matching is not available.
#[cfg(not(feature = "semantic-matching"))]
pub struct SemanticCtx;

/// Check whether a space separator is needed between two adjacent text
/// segments that are being concatenated.  Returns `true` when neither side
/// already provides whitespace at the boundary.
pub(crate) fn needs_separator(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let l = left.as_bytes().last().copied().unwrap_or(b' ');
    let r = right.as_bytes().first().copied().unwrap_or(b' ');
    !l.is_ascii_whitespace() && !r.is_ascii_whitespace()
}

// ============================================================================
// FieldId — deterministic UUID derived from SOM path
// ============================================================================

/// Namespace UUID used for deterministic FieldId generation (UUID v5).
const NAMESPACE_FIELD_ID: Uuid = Uuid::from_bytes([
    0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x47, 0x89, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78,
]);

/// A deterministic field identifier derived from a SOM path.
///
/// `FieldId` wraps a UUID v5 that is computed from the field's SOM path using
/// a fixed namespace. Two fields with the same SOM path always produce the
/// same `FieldId`, making output reproducible across runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldId(Uuid);

impl FieldId {
    /// Create a `FieldId` by hashing a `SomPath` into a deterministic UUID v5.
    pub fn from_som_path(path: &SomPath) -> Self {
        Self(Uuid::new_v5(&NAMESPACE_FIELD_ID, path.as_str().as_bytes()))
    }

    /// Create a random `FieldId`.
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    /// Get the underlying UUID.
    pub fn uuid(&self) -> &Uuid {
        &self.0
    }
}

impl std::fmt::Display for FieldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for FieldId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for FieldId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        // Accept either a canonical UUID (round-tripping our own serialized
        // output) or an arbitrary identifier / SOM path (e.g. AI- or
        // hand-authored field names like "CL_ClientType"), which we hash into a
        // deterministic UUID v5 — matching `FieldId::from(&str)`.
        Ok(match Uuid::parse_str(&s) {
            Ok(uuid) => FieldId(uuid),
            Err(_) => FieldId::from(s.as_str()),
        })
    }
}

impl From<&SomPath> for FieldId {
    fn from(path: &SomPath) -> Self {
        Self::from_som_path(path)
    }
}

impl From<SomPath> for FieldId {
    fn from(path: SomPath) -> Self {
        Self::from_som_path(&path)
    }
}

impl From<&str> for FieldId {
    fn from(s: &str) -> Self {
        Self::from_som_path(&SomPath::new(s))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum StructuredNode {
    Heading(HeadingNode),
    Paragraph(ParagraphNode),
    Image(ImageNode),
    Table(TableNode),
    Html(HtmlNode),
    Field(FieldNode),
    //UnorderedList(UnorderedListNode),
    //OrderedList(OrderedListNode),
    Repeatable(RepeatableNode),
    Group(GroupNode),
    Conditional(ConditionalNode),
    Empty,
    GridLayout(GridLayout),
    List(ListNode),
    Footnote(FootnoteNode),
    Notice(NoticeNode),
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    pub content: TranslatedText,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sublist: Option<Box<ListNode>>,
}

impl ListItem {
    /// Create a simple list item with no sublist.
    pub fn simple(content: TranslatedText) -> Self {
        Self {
            content,
            sublist: None,
        }
    }

    /// Get the plain text content of this item (excluding sublist).
    pub fn as_plain_text(&self) -> String {
        self.content.as_plain_text()
    }

    /// Get the plain text in a specific language (excluding sublist).
    pub fn plain_text_in(&self, lang: &str) -> String {
        self.content.plain_text_in(lang)
    }

    /// Collect languages from this item's content.
    pub fn collect_languages(&self, langs: &mut std::collections::BTreeSet<String>) {
        self.content.collect_languages(langs);
        if let Some(sub) = &self.sublist {
            for sub_item in &sub.items {
                sub_item.content.collect_languages(langs);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListNode {
    pub list_style: crate::document::ListStyleType,
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GridLayout {
    pub columns: usize,
    pub elements: Vec<GridLayoutElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GridLayoutElement {
    pub span: usize,
    pub node: StructuredNode,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TableNode {
    pub header: Option<TableHeader>,
    pub rows: Vec<TableRow>,
    pub caption: Option<TranslatedText>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TableRow {
    pub cells: Vec<StructuredNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TableHeader {
    pub cells: Vec<StructuredNode>,
}

/// A block of raw HTML markup, one string per language.
///
/// Produced only by [`crate::aem::aem_to_structured`], lifting an
/// [`crate::aem::AemNode::HtmlDisplayer`] back into the structured tree so the
/// HTML preview and the coverage check see the table (or chart, or image) that
/// the AEM HTML component actually renders, rather than a flattened run of
/// paragraphs. Nothing on the PDF -> structured path builds one, and the
/// agent's structured editor refuses to author one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HtmlNode {
    /// Language code -> HTML markup.
    pub content: HashMap<String, String>,
    /// The AEM `name` this block was lifted from, so converting back reuses it
    /// instead of minting a fresh one. `None` for a block with no origin.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_name: Option<String>,
}

impl HtmlNode {
    /// The markup in `language`, falling back to any other language that has
    /// some, then to the empty string.
    ///
    /// A locale present but BLANK counts as absent: a hand-authored package can
    /// carry an empty `html` attribute, and rendering nothing there is worse
    /// than rendering another language's table. Mirrors the writer, which skips
    /// an empty locale rather than emitting an empty item.
    pub fn markup_in(&self, language: &str) -> &str {
        self.content
            .get(language)
            .filter(|m| !m.trim().is_empty())
            .or_else(|| self.content.values().find(|m| !m.trim().is_empty()))
            .map(String::as_str)
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageNode {
    #[serde(skip, default)]
    pub content: Vec<u8>,
    pub alt_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupNode {
    pub children: Vec<StructuredNode>,

    /// `true` when the source laid this group out as a multi-column text flow
    /// (detected as `GroupKind::ColumnSection`).
    ///
    /// The children are stored in reading order — left column top-to-bottom,
    /// then right column — so consumers that ignore this flag still render the
    /// document correctly, just in a single column. Output targets that can
    /// express a column flow (e.g. the Redacto `layout-split` panel) use it to
    /// restore the original two-column appearance.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub column_flow: bool,
}

impl GroupNode {
    /// A plain group of children, with no column flow.
    pub fn new(children: Vec<StructuredNode>) -> Self {
        GroupNode {
            children,
            column_flow: false,
        }
    }

    /// A group whose children were laid out as a multi-column text flow.
    pub fn columns(children: Vec<StructuredNode>) -> Self {
        GroupNode {
            children,
            column_flow: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RepeatableNode {
    pub item: Box<StructuredNode>,
    pub min_occurrences: u32,
    pub max_occurrences: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConditionalNode {
    pub condition: FieldCondition,
    pub content: Box<StructuredNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldCondition {
    #[schemars(with = "String")]
    pub field_name: FieldId,
    pub value: InputValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum InputValue {
    Text(String),
    Number(#[schemars(with = "String")] Decimal),
    Bool(bool),
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FieldType {
    Text {
        regex: Option<String>,
        max_length: Option<usize>,
        min_length: Option<usize>,
    },
    Textarea {
        max_length: Option<usize>,
    },
    Number {
        #[schemars(with = "Option<String>")]
        min: Option<Decimal>,
        #[schemars(with = "Option<String>")]
        max: Option<Decimal>,
        #[schemars(with = "Option<String>")]
        step: Option<Decimal>,
    },
    Date,
    Email,
    Tel,
    Bool,
    Radio {
        options: Vec<NameValue>,
    },
    Select {
        options: Vec<NameValue>,
    },
    /// Multi-select checkbox group (AEM `guideCheckBox` with multiple options).
    CheckboxGroup {
        options: Vec<NameValue>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NameValue {
    pub name: TranslatableString,
    pub value: InputValue,
}

/// A string that can have translations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum TranslatableString {
    Plain(String),
    Translated(TranslationMap),
}

impl TranslatableString {
    /// Get the string in the specified language, or the first available
    pub fn get(&self, lang: &str) -> Option<&str> {
        match self {
            TranslatableString::Plain(s) => Some(s),
            TranslatableString::Translated(map) => map
                .get(lang)
                .and_then(|o| o.as_deref())
                .or_else(|| map.values().find_map(|o| o.as_deref())),
        }
    }

    /// Get the string in the specified language, or the first available, or empty string
    pub fn get_or_default(&self, lang: &str) -> &str {
        self.get(lang).unwrap_or("")
    }

    /// Returns the plain string if Plain, or the first available translation.
    /// Useful in tests that work with single-language documents.
    pub fn as_str(&self) -> &str {
        match self {
            TranslatableString::Plain(s) => s.as_str(),
            TranslatableString::Translated(map) => {
                map.values().find_map(|o| o.as_deref()).unwrap_or("")
            }
        }
    }

    /// Check if any contained string contains the given substring.
    pub fn contains(&self, needle: &str) -> bool {
        match self {
            TranslatableString::Plain(s) => s.contains(needle),
            TranslatableString::Translated(map) => map
                .values()
                .filter_map(|o| o.as_deref())
                .any(|s| s.contains(needle)),
        }
    }

    /// Merge two `TranslatableString` values, combining their translations into a
    /// single `Translated` map. `Plain` values are inserted under their respective
    /// language keys. Already-`Translated` maps are merged directly.
    pub fn merge(&self, self_lang: &str, other: &Self, other_lang: &str) -> Self {
        let mut map: TranslationMap = HashMap::new();
        match self {
            TranslatableString::Plain(s) => {
                map.insert(self_lang.to_string(), Some(s.clone()));
            }
            TranslatableString::Translated(m) => {
                map.extend(m.clone());
            }
        }
        match other {
            TranslatableString::Plain(s) => {
                map.insert(other_lang.to_string(), Some(s.clone()));
            }
            TranslatableString::Translated(m) => {
                map.extend(m.clone());
            }
        }
        TranslatableString::Translated(map)
    }

    /// Structural text comparison with language-aware semantics.
    ///
    /// Rules:
    /// - Plain vs Plain: direct string equality.
    /// - Translated vs Translated: at least one shared language key must exist
    ///   and have the same value.
    /// - Plain vs Translated: treated as a plain-text fallback and considered
    ///   equal when any translated value matches the plain text.
    pub fn structural_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TranslatableString::Plain(a), TranslatableString::Plain(b)) => a == b,
            (TranslatableString::Translated(a), TranslatableString::Translated(b)) => {
                translated_maps_match_on_shared_language(a, b)
            }
            (TranslatableString::Plain(a), TranslatableString::Translated(b))
            | (TranslatableString::Translated(b), TranslatableString::Plain(a)) => b
                .values()
                .filter_map(|o| o.as_deref())
                .any(|value| value == a),
        }
    }
}

fn translated_maps_match_on_shared_language(left: &TranslationMap, right: &TranslationMap) -> bool {
    left.iter()
        .filter_map(|(lang, left_text)| {
            let lt = left_text.as_deref()?;
            let rt = right.get(lang)?.as_deref()?;
            Some((lt, rt))
        })
        .any(|(left_text, right_text)| left_text == right_text)
}

impl std::fmt::Display for TranslatableString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslatableString::Plain(s) => write!(f, "{}", s),
            TranslatableString::Translated(map) => {
                // Display first available value
                if let Some(s) = map.values().find_map(|o| o.as_deref()) {
                    write!(f, "{}", s)
                } else {
                    Ok(())
                }
            }
        }
    }
}

impl From<String> for TranslatableString {
    fn from(s: String) -> Self {
        TranslatableString::Plain(s)
    }
}

impl From<&str> for TranslatableString {
    fn from(s: &str) -> Self {
        TranslatableString::Plain(s.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldNode {
    #[schemars(with = "String")]
    pub name: FieldId,
    #[serde(skip, default)]
    pub som_path: Option<SomPath>,
    pub label: Option<TranslatedText>,
    pub input_type: FieldType,
    pub value: Option<InputValue>,
    pub placeholder: Option<TranslatableString>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphNode {
    pub content: TranslatedText,
    #[serde(skip, default)]
    pub som_path: Option<SomPath>,
    #[serde(skip, default)]
    pub source_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeadingNode {
    pub level: HeadingLevel,
    pub content: TranslatedText,
    #[serde(skip, default)]
    pub som_path: Option<SomPath>,
    #[serde(skip, default)]
    pub source_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FootnoteNode {
    pub content: TranslatedText,
    /// The footnote marker (e.g. "1", "2") parsed from the leading text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(skip, default)]
    pub som_path: Option<SomPath>,
    #[serde(skip, default)]
    pub source_name: Option<String>,
}

/// A notice addressed to whoever fills the form, and to nobody else.
///
/// The source marks these `relevant="-print"`: they belong on screen and are
/// deliberately kept off the printed document. The engine used to drop every
/// such element, a rule written for the add/remove buttons that carry the same
/// attribute — and with them went prose the bank wrote on purpose, which then
/// had to be retyped into AEM by hand.
///
/// Only static text becomes a notice. A screen-only button or input is still
/// dropped: it is furniture, not content.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoticeNode {
    pub content: TranslatedText,
    #[serde(skip, default)]
    pub som_path: Option<SomPath>,
    #[serde(skip, default)]
    pub source_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct InlineText(pub Vec<InlineNode>);

impl InlineText {
    /// Create an empty inline text
    pub fn empty() -> Self {
        InlineText(Vec::new())
    }

    /// Create inline text from a plain string
    pub fn plain(text: impl Into<String>) -> Self {
        InlineText(vec![InlineNode::Text(text.into())])
    }

    /// Create inline text from nodes, consolidating consecutive nodes of the same type
    pub fn new(nodes: Vec<InlineNode>) -> Self {
        let mut result = InlineText(nodes);
        result.consolidate();
        result
    }

    /// Consolidate consecutive InlineNodes of the same type into single nodes
    pub fn consolidate(&mut self) {
        if self.0.len() <= 1 {
            return;
        }

        let nodes = std::mem::take(&mut self.0);
        let mut consolidated = Vec::with_capacity(nodes.len());
        let mut iter = nodes.into_iter();

        if let Some(mut current) = iter.next() {
            for next in iter {
                let merged = match (&mut current, &next) {
                    // Merge consecutive Text nodes
                    (InlineNode::Text(text), InlineNode::Text(next_text)) => {
                        text.push_str(next_text);
                        true
                    }
                    // Merge consecutive Strong nodes with Text content
                    (InlineNode::Strong(inner), InlineNode::Strong(next_inner)) => {
                        if let (InlineNode::Text(text), InlineNode::Text(next_text)) =
                            (inner.as_mut(), next_inner.as_ref())
                        {
                            text.push_str(next_text);
                            true
                        } else {
                            false
                        }
                    }
                    // Merge consecutive Emphasis nodes with Text content
                    (InlineNode::Emphasis(inner), InlineNode::Emphasis(next_inner)) => {
                        if let (InlineNode::Text(text), InlineNode::Text(next_text)) =
                            (inner.as_mut(), next_inner.as_ref())
                        {
                            text.push_str(next_text);
                            true
                        } else {
                            false
                        }
                    }
                    // Different types
                    _ => false,
                };

                if !merged {
                    consolidated.push(current);
                    current = next;
                }
            }
            consolidated.push(current);
        }

        self.0 = consolidated;
    }

    /// Check if the inline text is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
            || self.0.iter().all(|node| match node {
                InlineNode::Text(s) => s.is_empty(),
                _ => false,
            })
    }

    /// Get the plain text content (stripping formatting)
    pub fn as_plain_text(&self) -> String {
        fn collect_text(node: &InlineNode, out: &mut String) {
            match node {
                InlineNode::Text(s) => out.push_str(s),
                InlineNode::Link(link) => {
                    for child in &link.content.0 {
                        collect_text(child, out);
                    }
                }
                InlineNode::Strong(inner)
                | InlineNode::Emphasis(inner)
                | InlineNode::Superscript(inner) => {
                    collect_text(inner, out);
                }
            }
        }
        let mut result = String::new();
        for node in &self.0 {
            collect_text(node, &mut result);
        }
        result
    }

    /// Return a new `InlineText` with `Strong` and `Emphasis` wrappers removed,
    /// keeping only the inner text content. Adjacent text nodes are consolidated.
    pub fn to_plain(&self) -> Self {
        fn strip(node: &InlineNode) -> InlineNode {
            match node {
                InlineNode::Text(_) => node.clone(),
                InlineNode::Strong(inner)
                | InlineNode::Emphasis(inner)
                | InlineNode::Superscript(inner) => strip(inner),
                InlineNode::Link(link) => InlineNode::Link(LinkNode {
                    href: link.href.clone(),
                    content: link.content.to_plain(),
                }),
            }
        }
        InlineText::new(self.0.iter().map(strip).collect())
    }

    /// Concatenate another `InlineText` onto this one.
    ///
    /// This method appends all nodes from `other` to `self`, inserting a space
    /// separator between them if needed (when neither side provides whitespace
    /// at the boundary). After concatenation, consecutive nodes of the same type
    /// are consolidated.
    pub fn concat(&mut self, other: InlineText) {
        if other.0.is_empty() {
            return;
        }
        if self.0.is_empty() {
            self.0 = other.0;
            return;
        }

        // Check if we need a separator between the last node of self and first of other
        let needs_sep = self
            .0
            .last()
            .and_then(|n| n.trailing_text())
            .zip(other.0.first().and_then(|n| n.leading_text()))
            .map(|(left, right)| needs_separator(left, right))
            .unwrap_or(false);

        if needs_sep {
            self.0.push(InlineNode::Text(" ".to_string()));
        }

        self.0.extend(other.0);
        self.consolidate();
    }

    /// Check if two InlineText are structurally equal (compare plain text)
    pub fn structural_eq(&self, other: &Self) -> bool {
        self.as_plain_text() == other.as_plain_text()
    }
}

impl Default for InlineText {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum InlineNode {
    Text(String),
    Link(LinkNode),
    Strong(Box<InlineNode>),
    Emphasis(Box<InlineNode>),
    Superscript(Box<InlineNode>),
}

/// Per-language inline text with independent formatting per language.
///
/// Each language gets its own `InlineText` tree, allowing bold/italic/etc.
/// to be positioned independently across languages.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TranslatedText(pub HashMap<String, InlineText>);

/// The key [`TranslatedText::plain`] files text under when no real language is
/// known. Never a genuine language code, so nothing that scans a document for
/// its shipped languages (`collect_languages`, `AemConfig::base_language`) may
/// treat it as one -- doing so once let a single such node force `"default"`
/// into a real form's language list, which then outranked every genuine
/// language when nothing among them matched the profile's fixed master.
pub const NO_LANGUAGE: &str = "default";

impl TranslatedText {
    /// Create an empty translated text with no languages.
    pub fn empty() -> Self {
        TranslatedText(HashMap::new())
    }

    /// Create a translated text from a plain string with a single language.
    pub fn plain_with_lang(lang: impl Into<String>, text: impl Into<String>) -> Self {
        let mut map = HashMap::new();
        map.insert(lang.into(), InlineText::plain(text));
        TranslatedText(map)
    }

    /// Create a translated text from a plain string (no language, uses the
    /// [`NO_LANGUAGE`] sentinel key).
    pub fn plain(text: impl Into<String>) -> Self {
        Self::plain_with_lang(NO_LANGUAGE, text)
    }

    /// Create a translated text with a single language entry.
    pub fn single(lang: impl Into<String>, text: InlineText) -> Self {
        let mut map = HashMap::new();
        map.insert(lang.into(), text);
        TranslatedText(map)
    }

    /// Create a translated text from a map of language → InlineText.
    pub fn new(map: HashMap<String, InlineText>) -> Self {
        TranslatedText(map)
    }

    /// Check if the translated text has no languages or all languages are empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty() || self.0.values().all(|t| t.is_empty())
    }

    /// Get the InlineText for a specific language.
    pub fn get(&self, lang: &str) -> Option<&InlineText> {
        self.0.get(lang)
    }

    /// Get the InlineText for a specific language, or the first available.
    pub fn get_or_first(&self, lang: &str) -> Option<&InlineText> {
        self.0.get(lang).or_else(|| self.0.values().next())
    }

    /// Get a mutable reference to the InlineText for a specific language.
    pub fn get_mut(&mut self, lang: &str) -> Option<&mut InlineText> {
        self.0.get_mut(lang)
    }

    /// Get the plain text content (first available language, stripping formatting).
    pub fn as_plain_text(&self) -> String {
        self.0
            .values()
            .next()
            .map(|t| t.as_plain_text())
            .unwrap_or_default()
    }

    /// Get the plain text in a specific language (stripping formatting).
    /// Falls back to the first available language.
    pub fn plain_text_in(&self, lang: &str) -> String {
        self.0
            .get(lang)
            .or_else(|| self.0.values().next())
            .map(|t| t.as_plain_text())
            .unwrap_or_default()
    }

    /// Get the plain text in ALL available languages (for regex matching across languages).
    pub fn all_plain_texts(&self) -> Vec<String> {
        self.0.values().map(|t| t.as_plain_text()).collect()
    }

    /// Collect all language codes from this translated text.
    pub fn collect_languages(&self, langs: &mut BTreeSet<String>) {
        langs.extend(self.0.keys().cloned());
    }

    /// Return the set of languages that have missing translations (empty InlineText).
    pub fn missing_translation_languages(&self) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, text)| text.is_empty())
            .map(|(lang, _)| lang.clone())
            .collect()
    }

    /// Insert or replace the InlineText for a specific language.
    pub fn insert(&mut self, lang: impl Into<String>, text: InlineText) {
        self.0.insert(lang.into(), text);
    }

    /// Get all available languages.
    pub fn languages(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    /// Get iterator over (language, InlineText) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &InlineText)> {
        self.0.iter()
    }

    /// Return a new `TranslatedText` with `Strong` and `Emphasis` wrappers removed
    /// from all languages, keeping only the inner text content.
    pub fn to_plain(&self) -> Self {
        TranslatedText(
            self.0
                .iter()
                .map(|(lang, text)| (lang.clone(), text.to_plain()))
                .collect(),
        )
    }

    /// Concatenate another TranslatedText onto this one.
    /// For each language present in both, concatenates the InlineText.
    /// Languages only in `other` are added as-is.
    pub fn concat(&mut self, other: TranslatedText) {
        for (lang, text) in other.0 {
            if let Some(existing) = self.0.get_mut(&lang) {
                existing.concat(text);
            } else {
                self.0.insert(lang, text);
            }
        }
    }

    /// Check if two TranslatedText are structurally equal.
    pub fn structural_eq(&self, other: &Self) -> bool {
        let self_langs: BTreeSet<&String> = self.0.keys().collect();
        let other_langs: BTreeSet<&String> = other.0.keys().collect();

        let shared_langs: Vec<&&String> = self_langs.intersection(&other_langs).collect();

        if !shared_langs.is_empty() {
            return shared_langs.iter().any(|lang| {
                let self_text = self
                    .0
                    .get(**lang)
                    .map(|t| t.as_plain_text())
                    .unwrap_or_default();
                let other_text = other
                    .0
                    .get(**lang)
                    .map(|t| t.as_plain_text())
                    .unwrap_or_default();
                self_text == other_text
            });
        }

        // If no shared languages, compare plain text of first available
        if self_langs.is_empty() && other_langs.is_empty() {
            return true;
        }

        false
    }
}

impl Default for TranslatedText {
    fn default() -> Self {
        Self::empty()
    }
}

impl InlineNode {
    /// Return the trailing plain-text content of this node (if any).
    pub(crate) fn trailing_text(&self) -> Option<&str> {
        match self {
            InlineNode::Text(s) => Some(s.as_str()),
            InlineNode::Strong(inner)
            | InlineNode::Emphasis(inner)
            | InlineNode::Superscript(inner) => inner.trailing_text(),
            InlineNode::Link(link) => link.content.0.last().and_then(|n| n.trailing_text()),
        }
    }

    /// Return the leading plain-text content of this node (if any).
    pub(crate) fn leading_text(&self) -> Option<&str> {
        match self {
            InlineNode::Text(s) => Some(s.as_str()),
            InlineNode::Strong(inner)
            | InlineNode::Emphasis(inner)
            | InlineNode::Superscript(inner) => inner.leading_text(),
            InlineNode::Link(link) => link.content.0.first().and_then(|n| n.leading_text()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkNode {
    pub href: String,
    pub content: InlineText,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

impl HeadingLevel {
    /// Create a heading level from a u8 (clamped to 1-6)
    pub fn from_u8(level: u8) -> Self {
        match level {
            1 => HeadingLevel::H1,
            2 => HeadingLevel::H2,
            3 => HeadingLevel::H3,
            4 => HeadingLevel::H4,
            5 => HeadingLevel::H5,
            _ => HeadingLevel::H6,
        }
    }

    /// Get the numeric level (1-6)
    pub fn as_u8(&self) -> u8 {
        match self {
            HeadingLevel::H1 => 1,
            HeadingLevel::H2 => 2,
            HeadingLevel::H3 => 3,
            HeadingLevel::H4 => 4,
            HeadingLevel::H5 => 5,
            HeadingLevel::H6 => 6,
        }
    }
}

// ============================================================================
// Structural Equality
// ============================================================================
//
// Structural equality compares nodes by their type, field names, and text content,
// ignoring field values. This is used for merging multiple form states.

/// Controls what is compared when checking structural equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareMode {
    /// Full structural comparison including text content.
    Full,
    /// Ignore text content (for translation merging where structure matches
    /// but text differs by language).
    IgnoreText,
}

impl StructuredNode {
    /// Returns the SOM path of this node, if it carries one.
    ///
    /// SOM paths are available on Field, Paragraph, and Heading nodes.
    pub fn som_path(&self) -> Option<&SomPath> {
        match self {
            StructuredNode::Field(f) => f.som_path.as_ref(),
            StructuredNode::Paragraph(p) => p.som_path.as_ref(),
            StructuredNode::Heading(h) => h.som_path.as_ref(),
            StructuredNode::Conditional(c) => c.content.som_path(),
            StructuredNode::Footnote(n) => n.som_path.as_ref(),
            StructuredNode::Notice(n) => n.som_path.as_ref(),
            _ => None,
        }
    }

    /// Returns the best available language-independent anchor key for this node.
    ///
    /// Prefers SOM path when available, falls back to `source_name` (the XFA
    /// draw node `name` attribute, which is language-independent for same-template
    /// forms).
    ///
    /// For container nodes (Group, Repeatable, GridLayout, Table) that lack
    /// their own SOM path, a key is derived from the first anchored child.
    /// These derived keys are prefixed with a type tag (e.g. `g:`, `r:`) to
    /// prevent collisions with direct SOM-path anchors at the same list level.
    pub fn anchor_key(&self) -> Option<String> {
        if let Some(sp) = self.som_path() {
            return Some(sp.as_str().to_owned());
        }
        match self {
            StructuredNode::Paragraph(p) => p.source_name.clone(),
            StructuredNode::Heading(h) => h.source_name.clone(),
            StructuredNode::Footnote(n) => n.source_name.clone(),
            StructuredNode::Notice(n) => n.source_name.clone(),
            StructuredNode::Group(g) => g
                .children
                .iter()
                .find_map(|c| c.anchor_key())
                .map(|k| format!("g:{k}")),
            StructuredNode::Repeatable(r) => r.item.anchor_key().map(|k| format!("r:{k}")),
            StructuredNode::GridLayout(gl) => gl
                .elements
                .iter()
                .find_map(|e| e.node.anchor_key())
                .map(|k| format!("gl:{k}")),
            StructuredNode::Table(t) => t
                .header
                .as_ref()
                .and_then(|h| h.cells.iter().find_map(|c| c.anchor_key()))
                .or_else(|| {
                    t.rows
                        .first()
                        .and_then(|r| r.cells.iter().find_map(|c| c.anchor_key()))
                })
                .map(|k| format!("t:{k}")),
            _ => None,
        }
    }

    /// Check if two nodes are structurally equal.
    ///
    /// Structural equality compares:
    /// - Node type (variant)
    /// - Field names
    /// - Text content (for text-bearing nodes like Paragraph, Heading)
    /// - Heading level
    /// - Child structure (recursively)
    ///
    /// It does NOT compare:
    /// - Field values (InputValue)
    /// - Image content
    pub fn structural_eq(&self, other: &Self) -> bool {
        self.structural_cmp(other, CompareMode::Full)
    }

    /// Check if two nodes are structurally equal, ignoring all text content.
    ///
    /// This is used for translation merging, where the same document in different
    /// languages has identical structure but different text. It compares:
    /// - Node type (variant)
    /// - Heading level
    /// - Field names and input type structure (but NOT labels, placeholders, text)
    /// - Children count and structure (recursively)
    ///
    /// It does NOT compare:
    /// - Any text content (Paragraph, Heading, InlineText, captions)
    /// - Field labels and placeholders (may be translated)
    /// - Radio/Select option names (may be translated)
    /// - Field values
    /// - Image content
    pub fn structural_eq_ignore_text(&self, other: &Self) -> bool {
        self.structural_cmp(other, CompareMode::IgnoreText)
    }

    /// Unified structural comparison parameterized by [`CompareMode`].
    fn structural_cmp(&self, other: &Self, mode: CompareMode) -> bool {
        match (self, other) {
            (StructuredNode::Heading(a), StructuredNode::Heading(b)) => {
                a.level.as_u8() == b.level.as_u8()
                    && (mode == CompareMode::IgnoreText || a.content.structural_eq(&b.content))
            }
            (StructuredNode::Paragraph(a), StructuredNode::Paragraph(b)) => {
                // In IgnoreText mode all paragraphs match (text differs by language)
                mode == CompareMode::IgnoreText || a.content.structural_eq(&b.content)
            }
            (StructuredNode::Image(a), StructuredNode::Image(b)) => a.alt_text == b.alt_text,
            (StructuredNode::Table(a), StructuredNode::Table(b)) => a.structural_cmp(b, mode),
            (StructuredNode::Field(a), StructuredNode::Field(b)) => {
                // In IgnoreText mode (used for translation merging), Fields match by
                // input type structure only — FieldIds are derived from SOM paths which
                // can differ across languages for the same logical field.
                if mode == CompareMode::IgnoreText {
                    a.input_type.structural_eq(&b.input_type)
                } else {
                    a.structural_eq(b)
                }
            }
            (StructuredNode::Repeatable(a), StructuredNode::Repeatable(b)) => {
                a.min_occurrences == b.min_occurrences
                    && a.max_occurrences == b.max_occurrences
                    && a.item.structural_cmp(&b.item, mode)
            }
            (StructuredNode::Group(a), StructuredNode::Group(b)) => {
                // In IgnoreText mode (used for translation merging), Groups match by type
                // only — the child count may differ across languages because rich text
                // can produce a different number of <p> elements. merge_node_lists will
                // use LCS to align the children correctly.
                mode == CompareMode::IgnoreText
                    || structured_node_slices_eq(&a.children, &b.children, mode)
            }
            (StructuredNode::Conditional(a), StructuredNode::Conditional(b)) => {
                if mode == CompareMode::IgnoreText {
                    // In IgnoreText mode (translation merging), match conditionals
                    // by their condition (field_name + value) so that the LCS
                    // correctly pairs e.g. Cond(CL_ClientType=="Firma") across
                    // languages, even when content structure differs.
                    a.condition == b.condition
                } else {
                    // In Full mode (exhaustive state merging), two ConditionalNodes
                    // are structurally equal only when both their condition AND their
                    // content match.  Comparing only content would silently equate
                    // Cond(fieldA, P) with Cond(fieldB, P) and drop one.
                    a.condition == b.condition && a.content.structural_cmp(&b.content, mode)
                }
            }
            (StructuredNode::Empty, StructuredNode::Empty) => true,
            (StructuredNode::GridLayout(a), StructuredNode::GridLayout(b)) => {
                a.columns == b.columns
                    && a.elements.len() == b.elements.len()
                    && a.elements.iter().zip(b.elements.iter()).all(|(ea, eb)| {
                        ea.span == eb.span && ea.node.structural_cmp(&eb.node, mode)
                    })
            }
            (StructuredNode::List(a), StructuredNode::List(b)) => {
                list_nodes_structural_cmp(a, b, mode)
            }
            (StructuredNode::Footnote(a), StructuredNode::Footnote(b)) => {
                mode == CompareMode::IgnoreText || a.content.structural_eq(&b.content)
            }
            (StructuredNode::Notice(a), StructuredNode::Notice(b)) => {
                mode == CompareMode::IgnoreText || a.content.structural_eq(&b.content)
            }
            (StructuredNode::Html(a), StructuredNode::Html(b)) => {
                mode == CompareMode::IgnoreText || a.content == b.content
            }
            // Different variants are never structurally equal
            _ => false,
        }
    }

    /// Get a structural discriminant for this node type.
    /// Used for quick inequality checks before deep comparison.
    pub fn structural_discriminant(&self) -> u8 {
        match self {
            StructuredNode::Heading(_) => 0,
            StructuredNode::Paragraph(_) => 1,
            StructuredNode::Image(_) => 2,
            StructuredNode::Table(_) => 3,
            StructuredNode::Field(_) => 4,
            StructuredNode::Repeatable(_) => 5,
            StructuredNode::Group(_) => 6,
            StructuredNode::Conditional(_) => 7,
            StructuredNode::Empty => 8,
            StructuredNode::GridLayout(_) => 9,
            StructuredNode::List(_) => 10,
            StructuredNode::Footnote(_) => 11,
            StructuredNode::Html(_) => 12,
            StructuredNode::Notice(_) => 13,
        }
    }

    /// Collect all language codes used in translatable content within this node tree.
    ///
    /// Recursively walks the node and its children, gathering language keys from
    /// `TranslatedText` inline nodes and `TranslatableString::Translated` maps.
    pub fn collect_languages(&self, langs: &mut BTreeSet<String>) {
        match self {
            StructuredNode::Heading(h) => h.content.collect_languages(langs),
            StructuredNode::Paragraph(p) => p.content.collect_languages(langs),
            StructuredNode::Notice(n) => n.content.collect_languages(langs),
            StructuredNode::Field(f) => {
                if let Some(label) = &f.label {
                    label.collect_languages(langs);
                }
                if let Some(TranslatableString::Translated(map)) = &f.placeholder {
                    langs.extend(map.keys().cloned());
                }
                match &f.input_type {
                    FieldType::Radio { options }
                    | FieldType::Select { options }
                    | FieldType::CheckboxGroup { options } => {
                        for opt in options {
                            if let TranslatableString::Translated(map) = &opt.name {
                                langs.extend(map.keys().cloned());
                            }
                        }
                    }
                    _ => {}
                }
            }
            StructuredNode::Table(t) => {
                if let Some(caption) = &t.caption {
                    caption.collect_languages(langs);
                }
                if let Some(header) = &t.header {
                    for cell in &header.cells {
                        cell.collect_languages(langs);
                    }
                }
                for row in &t.rows {
                    for cell in &row.cells {
                        cell.collect_languages(langs);
                    }
                }
            }
            StructuredNode::Group(g) => {
                for child in &g.children {
                    child.collect_languages(langs);
                }
            }
            StructuredNode::Repeatable(r) => r.item.collect_languages(langs),
            StructuredNode::Conditional(c) => c.content.collect_languages(langs),
            StructuredNode::GridLayout(g) => {
                for elem in &g.elements {
                    elem.node.collect_languages(langs);
                }
            }
            StructuredNode::List(l) => {
                for item in &l.items {
                    item.content.collect_languages(langs);
                    if let Some(sub) = &item.sublist {
                        for sub_item in &sub.items {
                            sub_item.content.collect_languages(langs);
                        }
                    }
                }
            }
            StructuredNode::Footnote(n) => n.content.collect_languages(langs),
            _ => {}
        }
    }
}

/// Structural list comparison helper used by [`StructuredNode::structural_cmp`].
fn list_nodes_structural_cmp(a: &ListNode, b: &ListNode, mode: CompareMode) -> bool {
    a.list_style == b.list_style
        && a.items.len() == b.items.len()
        && (mode == CompareMode::IgnoreText
            || a.items.iter().zip(b.items.iter()).all(|(ia, ib)| {
                ia.content.structural_eq(&ib.content)
                    && match (&ia.sublist, &ib.sublist) {
                        (Some(sa), Some(sb)) => list_nodes_structural_cmp(sa, sb, mode),
                        (None, None) => true,
                        _ => false,
                    }
            }))
}

/// Compare two node slices using structural comparison for each element.
fn structured_node_slices_eq(
    left: &[StructuredNode],
    right: &[StructuredNode],
    mode: CompareMode,
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.structural_cmp(b, mode))
}

impl FieldNode {
    /// Returns the SOM path string of this field, or an empty string if unavailable.
    pub fn som_path_str(&self) -> &str {
        self.som_path.as_ref().map(|p| p.as_str()).unwrap_or("")
    }

    /// Check if two fields are structurally equal.
    /// Compares name, text-bearing metadata, and input type structure, but NOT value.
    ///
    /// When two fields share the same `name` (FieldId), they represent the same
    /// logical field regardless of label/placeholder differences (which can arise
    /// from state-dependent label attachment in the layout pipeline).
    pub fn structural_eq(&self, other: &Self) -> bool {
        if self.name == other.name {
            // Same field identity – only require matching input type structure.
            self.input_type.structural_eq(&other.input_type)
        } else {
            self.label.structural_eq(&other.label)
                && self.placeholder.structural_eq(&other.placeholder)
                && self.input_type.structural_eq(&other.input_type)
        }
    }
}

trait OptionStructuralEq<T> {
    fn structural_eq(&self, other: &Self) -> bool;
}

impl OptionStructuralEq<TranslatedText> for Option<TranslatedText> {
    fn structural_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (None, None) => true,
            (Some(a), Some(b)) => a.structural_eq(b),
            _ => false,
        }
    }
}

impl OptionStructuralEq<TranslatableString> for Option<TranslatableString> {
    fn structural_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (None, None) => true,
            (Some(a), Some(b)) => a.structural_eq(b),
            _ => false,
        }
    }
}

impl FieldType {
    /// Check if two field types are structurally equal.
    /// For Radio/Select options, we compare by value only (names may be translated).
    pub fn structural_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                FieldType::Text {
                    regex: r1,
                    max_length: max1,
                    min_length: min1,
                },
                FieldType::Text {
                    regex: r2,
                    max_length: max2,
                    min_length: min2,
                },
            ) => r1 == r2 && max1 == max2 && min1 == min2,
            (
                FieldType::Number {
                    min: min1,
                    max: max1,
                    step: step1,
                },
                FieldType::Number {
                    min: min2,
                    max: max2,
                    step: step2,
                },
            ) => min1 == min2 && max1 == max2 && step1 == step2,
            (
                FieldType::Textarea { max_length: max1 },
                FieldType::Textarea { max_length: max2 },
            ) => max1 == max2,
            (FieldType::Date, FieldType::Date) => true,
            (FieldType::Email, FieldType::Email) => true,
            (FieldType::Tel, FieldType::Tel) => true,
            (FieldType::Bool, FieldType::Bool) => true,
            (FieldType::Radio { options: opts1 }, FieldType::Radio { options: opts2 })
            | (FieldType::Select { options: opts1 }, FieldType::Select { options: opts2 })
            | (
                FieldType::CheckboxGroup { options: opts1 },
                FieldType::CheckboxGroup { options: opts2 },
            ) => option_name_values_structural_eq(opts1, opts2),
            _ => false,
        }
    }
}

/// Compare option vectors by value and translatable name structure.
fn option_name_values_structural_eq(opts1: &[NameValue], opts2: &[NameValue]) -> bool {
    opts1.len() == opts2.len()
        && opts1
            .iter()
            .zip(opts2.iter())
            .all(|(o1, o2)| o1.value == o2.value && o1.name.structural_eq(&o2.name))
}

impl TableNode {
    /// Check if two tables are structurally equal.
    pub fn structural_eq(&self, other: &Self) -> bool {
        self.structural_cmp(other, CompareMode::Full)
    }

    /// Check if two tables are structurally equal, ignoring text content.
    /// Used for translation merging.
    pub fn structural_eq_ignore_text(&self, other: &Self) -> bool {
        self.structural_cmp(other, CompareMode::IgnoreText)
    }

    /// Unified structural comparison parameterized by [`CompareMode`].
    fn structural_cmp(&self, other: &Self, mode: CompareMode) -> bool {
        // Compare header structure
        let header_eq = match (&self.header, &other.header) {
            (None, None) => true,
            (Some(h1), Some(h2)) => structured_node_slices_eq(&h1.cells, &h2.cells, mode),
            _ => false,
        };

        // Compare row structure
        let rows_eq = self.rows.len() == other.rows.len()
            && self
                .rows
                .iter()
                .zip(other.rows.iter())
                .all(|(r1, r2)| structured_node_slices_eq(&r1.cells, &r2.cells, mode));

        // Caption is only compared in Full mode
        let caption_eq =
            mode == CompareMode::IgnoreText || self.caption.structural_eq(&other.caption);

        header_eq && rows_eq && caption_eq
    }
}

/// Document envelope containing the structured content and context metadata.
///
/// This is the top-level structure that wraps the document's structured nodes
/// along with the processing context that was enriched throughout the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentEnvelope {
    /// Context metadata enriched throughout processing
    pub context: Context,

    /// The structured document content
    pub content: Vec<StructuredNode>,

    /// The number of exhaustive form states that were merged to produce this envelope.
    /// Used to detect mismatches between language variants of the same form.
    #[serde(default = "default_state_count")]
    pub state_count: usize,
}

fn default_state_count() -> usize {
    1
}

// ============================================================================
// Footnote collection
// ============================================================================

/// Recursively collect all footnote nodes from a structured tree, in document
/// order.
///
/// Descends into `Group` and `Conditional` wrappers, which are the only
/// containers footnotes are ever nested in.
pub fn collect_footnote_nodes(nodes: &[StructuredNode]) -> Vec<&FootnoteNode> {
    let mut out = Vec::new();
    collect_footnote_nodes_into(nodes, &mut out);
    out
}

/// Append the footnote nodes found in `nodes` to `out`.
pub fn collect_footnote_nodes_into<'a>(
    nodes: &'a [StructuredNode],
    out: &mut Vec<&'a FootnoteNode>,
) {
    for node in nodes {
        match node {
            StructuredNode::Footnote(f) => out.push(f),
            StructuredNode::Group(g) => collect_footnote_nodes_into(&g.children, out),
            StructuredNode::Conditional(c) => {
                collect_footnote_nodes_into(std::slice::from_ref(c.content.as_ref()), out);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A locale present but blank counts as absent, so a hand-authored package
    /// with an empty `html` attribute still renders another language's table
    /// rather than nothing at all.
    #[test]
    fn blank_markup_falls_back_to_a_language_that_has_some() {
        let node = HtmlNode {
            content: HashMap::from([
                ("de".to_string(), "   ".to_string()),
                ("en".to_string(), "<table></table>".to_string()),
            ]),
            source_name: None,
        };
        assert_eq!(node.markup_in("de"), "<table></table>");
        assert_eq!(node.markup_in("en"), "<table></table>");
        assert_eq!(node.markup_in("fr"), "<table></table>");

        let all_blank = HtmlNode {
            content: HashMap::from([("de".to_string(), String::new())]),
            source_name: None,
        };
        assert_eq!(all_blank.markup_in("de"), "");
    }
}
