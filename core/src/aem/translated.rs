//! `AemNodeTranslated` — a multilingual mirror of [`AemNode`].
//!
//! Every user-visible text field (`title`, `label`, static `content`, and
//! option labels) becomes a per-language map ([`AemI18nText`]) instead of a
//! single `String`. The agent authors this tree directly from the source
//! documents in every language, then it is **lowered** to
//! `(AemNode, translations_dict)` — exactly the inputs
//! [`crate::to_aem_package_from_node_with_translations`] already consumes, so
//! the package/XML writers need no changes.
//!
//! The lowering mirrors the app editor's proven `build_translation_dict` /
//! `for_each_labeled`: the master language fills the `AemNode` strings, and each
//! *labeled* node contributes `master_text -> { lang -> text }` to the
//! translation dictionary (which is keyed by the master-language text).

use std::collections::{BTreeMap, HashMap};

use uuid::Uuid;

use super::{
    AemAttrs, AemNode, AemOption, ConditionRule, OptionAlignment, Passthrough, TextFieldKind,
};
use crate::structured::FieldId;

/// The translation dictionary shape the package writer expects:
/// master-language text → { language code → translated text }.
pub type I18nDict = HashMap<String, HashMap<String, String>>;

/// Re-key a lowered [`I18nDict`] into the [`TranslationData`] form the lift back
/// up reads translations from.
///
/// [`AemNodeTranslated::lower`] keys its dictionary by master text, while both
/// [`crate::aem_to_translated`] and [`crate::aem_to_structured`] resolve a text
/// through the Sling dictionary — whose fragment-dictionary fallback key is
/// `fd_<master text>`. Converting between the two is what makes
/// lower → edit → lift round trip without losing translations.
pub fn translation_data_from_master_dict(dict: I18nDict) -> super::parser::TranslationData {
    super::parser::TranslationData {
        entries: dict
            .into_iter()
            .map(|(text, langs)| (format!("fd_{text}"), langs))
            .collect(),
    }
}

/// A user-visible AEM text value in every available language (lang code → HTML
/// string). Serialized transparently as a plain `{ "de": "…", "en": "…" }` map.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct AemI18nText(pub BTreeMap<String, String>);

impl AemI18nText {
    /// Language codes present, in sorted order.
    pub fn languages(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// The text in `lang`, if present.
    pub fn get(&self, lang: &str) -> Option<&str> {
        self.0.get(lang).map(String::as_str)
    }

    /// The master-language text, falling back to the first available language,
    /// or `""` if the map is empty.
    pub fn master(&self, master_lang: &str) -> &str {
        self.get(master_lang)
            .or_else(|| self.0.values().next().map(String::as_str))
            .unwrap_or("")
    }

    /// Convenience constructor for a single-language value.
    pub fn single(lang: impl Into<String>, text: impl Into<String>) -> Self {
        let mut m = BTreeMap::new();
        m.insert(lang.into(), text.into());
        AemI18nText(m)
    }
}

/// Multilingual mirror of [`AemOption`]; only the label is translated.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AemOptionTranslated {
    /// Display label per language (may contain rich-text HTML).
    pub label: AemI18nText,
    /// Form value submitted when this option is selected (not translated).
    pub value: String,
}

impl AemOptionTranslated {
    fn lower(&self, master_lang: &str) -> AemOption {
        AemOption {
            label: self.label.master(master_lang).to_string(),
            value: self.value.clone(),
        }
    }
}

/// A same-language disagreement encountered while lowering: two labeled nodes
/// share the same master text but supply different translations for `lang`.
/// Inherent to the master-text-keyed dictionary; resolved last-writer-wins.
#[derive(Debug, Clone, PartialEq)]
pub struct LowerConflict {
    pub master_text: String,
    pub lang: String,
    pub existing: String,
    pub incoming: String,
}

/// Multilingual mirror of [`AemNode`]. Field names and the `#[serde(tag =
/// "type")]` representation match `AemNode` exactly; only the user-visible text
/// fields differ (`AemI18nText` instead of `String`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum AemNodeTranslated {
    Root {
        title: AemI18nText,
        children: Vec<AemNodeTranslated>,
    },
    Panel {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        title: AemI18nText,
        children: Vec<AemNodeTranslated>,
        is_page: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        visible: bool,
        is_conditional: bool,
        dor_num_cols: Option<u32>,
        colspan: u32,
        dor_colspan: Option<u32>,
        bind_ref: Option<String>,
        /// `fragRef` this panel was expanded from, when the parser inlined a
        /// fragment's children into it.
        #[serde(default)]
        frag_ref: Option<String>,
    },
    TextField {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        label: AemI18nText,
        mandatory: bool,
        visible: bool,
        max_chars: Option<usize>,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        bind_ref: Option<String>,
        /// Which single-line input component this is; carried through the
        /// translated form so a translate round-trip does not turn an email or
        /// telephone field back into a plain text box.
        #[serde(default)]
        kind: TextFieldKind,
    },
    NumberField {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        label: AemI18nText,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        bind_ref: Option<String>,
    },
    DatePicker {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        label: AemI18nText,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        bind_ref: Option<String>,
    },
    Dropdown {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        label: AemI18nText,
        options: Vec<AemOptionTranslated>,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        #[schemars(with = "Option<String>")]
        field_id: Option<FieldId>,
        conditions: Vec<ConditionRule>,
        bind_ref: Option<String>,
    },
    Checkbox {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        label: AemI18nText,
        options: Vec<AemOptionTranslated>,
        alignment: OptionAlignment,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        #[schemars(with = "Option<String>")]
        field_id: Option<FieldId>,
        conditions: Vec<ConditionRule>,
        bind_ref: Option<String>,
    },
    RadioButton {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        label: AemI18nText,
        options: Vec<AemOptionTranslated>,
        alignment: OptionAlignment,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        #[schemars(with = "Option<String>")]
        field_id: Option<FieldId>,
        conditions: Vec<ConditionRule>,
        bind_ref: Option<String>,
    },
    TextDraw {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        content: AemI18nText,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "super::default_true")]
        visible: bool,
        colspan: u32,
        dor_colspan: Option<u32>,
    },
    /// An on-screen notice: the UBS message box. Screen-only by construction,
    /// so its template carries `dorExclusion` and `summaryExclusion`.
    /// See [`AemNode::MessageBox`].
    MessageBox {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        content: AemI18nText,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "super::default_true")]
        visible: bool,
        colspan: u32,
        dor_colspan: Option<u32>,
    },
    TitleDraw {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        content: AemI18nText,
        heading_level: u8,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "super::default_true")]
        visible: bool,
        colspan: u32,
        dor_colspan: Option<u32>,
    },
    /// Static HTML block (`htmlDisplayer`) -- a table, a chart or an image.
    /// `content` is HTML markup per language, and it stays on the node rather
    /// than moving into the translation dictionary: the component renders its
    /// own `localeContent` children. See [`AemNode::HtmlDisplayer`].
    HtmlDisplayer {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        content: AemI18nText,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "super::default_true")]
        visible: bool,
        colspan: u32,
        dor_colspan: Option<u32>,
    },
    Repeatable {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        title: AemI18nText,
        children: Vec<AemNodeTranslated>,
        min_occur: u32,
        max_occur: u32,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "super::default_true")]
        visible: bool,
        bind_ref: Option<String>,
        /// `fragRef` this repeatable wraps, when a repeating panel carried a
        /// `fragRef` and its content was inlined.
        #[serde(default)]
        frag_ref: Option<String>,
    },
    Fragment {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        /// `jcr:title` of the panel this fragment replaced. Distinguishes two
        /// fragments that share a `frag_ref` when resolving XSD element names.
        #[serde(default)]
        title: AemI18nText,
        frag_ref: String,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        /// Whether the node is visible. Default `true`.
        #[serde(default = "super::default_true")]
        visible: bool,
        bind_ref: Option<String>,
    },
    Preface {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
    },
    Appendix {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
    },
    FootnotePlaceholder {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        colspan: u32,
    },
    Custom {
        uuid: Uuid,
        /// Fidelity passthrough captured on load (empty for engine-built nodes).
        #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
        passthrough: Passthrough,
        name: String,
        template_key: String,
        label: AemI18nText,
        options: Vec<AemOptionTranslated>,
        mandatory: bool,
        visible: bool,
        /// Where this node shows up: screen, summary, DoR, PDF. See [`AemAttrs`].
        #[serde(default, flatten)]
        attrs: AemAttrs,
        colspan: u32,
        dor_colspan: Option<u32>,
        bind_ref: Option<String>,
    },
}

/// Record `text`'s non-master translations into `dict`, keyed by its master
/// string (mirrors the app editor's `build_translation_dict`). Same-language
/// disagreements are logged as conflicts and resolved last-writer-wins.
fn emit_translations(
    text: &AemI18nText,
    master_lang: &str,
    languages: &[String],
    dict: &mut I18nDict,
    conflicts: &mut Vec<LowerConflict>,
) {
    let master = text.master(master_lang);
    if master.is_empty() {
        return;
    }
    for lang in languages {
        if lang == master_lang {
            continue;
        }
        let Some(t) = text.get(lang) else { continue };
        if t.is_empty() || t == master {
            continue;
        }
        let sub = dict.entry(master.to_string()).or_default();
        if let Some(existing) = sub.get(lang) {
            if existing != t {
                conflicts.push(LowerConflict {
                    master_text: master.to_string(),
                    lang: lang.clone(),
                    existing: existing.clone(),
                    incoming: t.to_string(),
                });
            }
        }
        sub.insert(lang.clone(), t.to_string());
    }
}

fn lower_options(
    options: &[AemOptionTranslated],
    master_lang: &str,
    languages: &[String],
    dict: &mut I18nDict,
    conflicts: &mut Vec<LowerConflict>,
) -> Vec<AemOption> {
    options
        .iter()
        .map(|o| {
            emit_translations(&o.label, master_lang, languages, dict, conflicts);
            o.lower(master_lang)
        })
        .collect()
}

fn lower_children(
    children: &[AemNodeTranslated],
    master_lang: &str,
    languages: &[String],
    dict: &mut I18nDict,
    conflicts: &mut Vec<LowerConflict>,
) -> Vec<AemNode> {
    children
        .iter()
        .map(|c| c.lower_node(master_lang, languages, dict, conflicts))
        .collect()
}

impl AemNodeTranslated {
    /// Lower to the single-language [`AemNode`] tree plus the master-text-keyed
    /// translation dictionary. Conflicts (if any) are discarded; use
    /// [`Self::lower_checked`] to inspect them.
    pub fn lower(&self, master_lang: &str, languages: &[String]) -> (AemNode, I18nDict) {
        let (node, dict, _) = self.lower_checked(master_lang, languages);
        (node, dict)
    }

    /// Like [`Self::lower`] but also returns any same-language translation
    /// collisions encountered (empty == clean).
    pub fn lower_checked(
        &self,
        master_lang: &str,
        languages: &[String],
    ) -> (AemNode, I18nDict, Vec<LowerConflict>) {
        let mut dict = I18nDict::new();
        let mut conflicts = Vec::new();
        let node = self.lower_node(master_lang, languages, &mut dict, &mut conflicts);
        (node, dict, conflicts)
    }

    /// Collect every node's fidelity [`Passthrough`] keyed by uuid, for the
    /// writer to re-emit. Only non-empty entries are included (engine-built nodes
    /// carry nothing). The map is derived fresh from the tree, so it always
    /// reflects the current (possibly edited/restored) state.
    pub fn passthrough_map(&self) -> HashMap<Uuid, Passthrough> {
        let mut m = HashMap::new();
        self.collect_passthrough(&mut m);
        m
    }

    fn collect_passthrough(&self, m: &mut HashMap<Uuid, Passthrough>) {
        let record = |m: &mut HashMap<Uuid, Passthrough>, uuid: &Uuid, p: &Passthrough| {
            if !p.is_empty() {
                m.insert(*uuid, p.clone());
            }
        };
        match self {
            AemNodeTranslated::Root { children, .. } => {
                for c in children {
                    c.collect_passthrough(m);
                }
            }
            AemNodeTranslated::Panel { uuid, passthrough, children, .. }
            | AemNodeTranslated::Repeatable { uuid, passthrough, children, .. } => {
                record(m, uuid, passthrough);
                for c in children {
                    c.collect_passthrough(m);
                }
            }
            AemNodeTranslated::TextField { uuid, passthrough, .. }
            | AemNodeTranslated::NumberField { uuid, passthrough, .. }
            | AemNodeTranslated::DatePicker { uuid, passthrough, .. }
            | AemNodeTranslated::Dropdown { uuid, passthrough, .. }
            | AemNodeTranslated::Checkbox { uuid, passthrough, .. }
            | AemNodeTranslated::RadioButton { uuid, passthrough, .. }
            | AemNodeTranslated::TextDraw { uuid, passthrough, .. }
            | AemNodeTranslated::MessageBox { uuid, passthrough, .. }
            | AemNodeTranslated::TitleDraw { uuid, passthrough, .. }
            | AemNodeTranslated::HtmlDisplayer { uuid, passthrough, .. }
            | AemNodeTranslated::Fragment { uuid, passthrough, .. }
            | AemNodeTranslated::Preface { uuid, passthrough, .. }
            | AemNodeTranslated::Appendix { uuid, passthrough, .. }
            | AemNodeTranslated::FootnotePlaceholder { uuid, passthrough, .. }
            | AemNodeTranslated::Custom { uuid, passthrough, .. } => {
                record(m, uuid, passthrough);
            }
        }
    }

    fn lower_node(
        &self,
        master_lang: &str,
        languages: &[String],
        dict: &mut I18nDict,
        conflicts: &mut Vec<LowerConflict>,
    ) -> AemNode {
        // Helper to lower a labeled text field: emit translations + return master.
        macro_rules! text {
            ($t:expr) => {{
                emit_translations($t, master_lang, languages, dict, conflicts);
                $t.master(master_lang).to_string()
            }};
        }
        match self {
            // Root is NOT a labeled node (no uuid) — fill master title, emit nothing.
            AemNodeTranslated::Root { title, children } => AemNode::Root {
                title: title.master(master_lang).to_string(),
                children: lower_children(children, master_lang, languages, dict, conflicts),
            },
            AemNodeTranslated::Panel {
                uuid,
                name,
                title,
                children,
                is_page,
                attrs,
                visible,
                is_conditional,
                dor_num_cols,
                colspan,
                dor_colspan,
                bind_ref,
                frag_ref,
                ..
            } => AemNode::Panel {
                uuid: *uuid,
                name: name.clone(),
                title: text!(title),
                children: lower_children(children, master_lang, languages, dict, conflicts),
                is_page: *is_page,
                attrs: attrs.clone(),
                visible: *visible,
                is_conditional: *is_conditional,
                dor_num_cols: *dor_num_cols,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                bind_ref: bind_ref.clone(),
                frag_ref: frag_ref.clone(),
            },
            AemNodeTranslated::TextField {
                uuid,
                name,
                label,
                mandatory,
                visible,
                max_chars,
                colspan,
                dor_colspan,
                bind_ref,
                kind,
                attrs,
                ..
            } => AemNode::TextField {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                label: text!(label),
                mandatory: *mandatory,
                visible: *visible,
                max_chars: *max_chars,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                bind_ref: bind_ref.clone(),
                kind: *kind,
            },
            AemNodeTranslated::NumberField {
                uuid,
                name,
                label,
                mandatory,
                visible,
                colspan,
                dor_colspan,
                bind_ref,
                attrs,
                ..
            } => AemNode::NumberField {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                label: text!(label),
                mandatory: *mandatory,
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                bind_ref: bind_ref.clone(),
            },
            AemNodeTranslated::DatePicker {
                uuid,
                name,
                label,
                mandatory,
                visible,
                colspan,
                dor_colspan,
                bind_ref,
                attrs,
                ..
            } => AemNode::DatePicker {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                label: text!(label),
                mandatory: *mandatory,
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                bind_ref: bind_ref.clone(),
            },
            AemNodeTranslated::Dropdown {
                uuid,
                name,
                label,
                options,
                mandatory,
                visible,
                colspan,
                dor_colspan,
                field_id,
                conditions,
                bind_ref,
                attrs,
                ..
            } => AemNode::Dropdown {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                label: text!(label),
                options: lower_options(options, master_lang, languages, dict, conflicts),
                mandatory: *mandatory,
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                field_id: field_id.clone(),
                conditions: conditions.clone(),
                bind_ref: bind_ref.clone(),
            },
            AemNodeTranslated::Checkbox {
                uuid,
                name,
                label,
                options,
                alignment,
                visible,
                colspan,
                dor_colspan,
                field_id,
                conditions,
                bind_ref,
                attrs,
                ..
            } => AemNode::Checkbox {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                label: text!(label),
                options: lower_options(options, master_lang, languages, dict, conflicts),
                alignment: *alignment,
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                field_id: field_id.clone(),
                conditions: conditions.clone(),
                bind_ref: bind_ref.clone(),
            },
            AemNodeTranslated::RadioButton {
                uuid,
                name,
                label,
                options,
                alignment,
                mandatory,
                visible,
                colspan,
                dor_colspan,
                field_id,
                conditions,
                bind_ref,
                attrs,
                ..
            } => AemNode::RadioButton {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                label: text!(label),
                options: lower_options(options, master_lang, languages, dict, conflicts),
                alignment: *alignment,
                mandatory: *mandatory,
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                field_id: field_id.clone(),
                conditions: conditions.clone(),
                bind_ref: bind_ref.clone(),
            },
            AemNodeTranslated::TextDraw {
                uuid,
                name,
                content,
                attrs,
                visible,
                colspan,
                dor_colspan,
                ..
            } => AemNode::TextDraw {
                visible: *visible,
                uuid: *uuid,
                name: name.clone(),
                content: text!(content),
                attrs: attrs.clone(),
                colspan: *colspan,
                dor_colspan: *dor_colspan,
            },
            AemNodeTranslated::MessageBox {
                uuid,
                name,
                content,
                attrs,
                visible,
                colspan,
                dor_colspan,
                ..
            } => AemNode::MessageBox {
                visible: *visible,
                uuid: *uuid,
                name: name.clone(),
                content: text!(content),
                attrs: attrs.clone(),
                colspan: *colspan,
                dor_colspan: *dor_colspan,
            },
            AemNodeTranslated::TitleDraw {
                uuid,
                name,
                content,
                heading_level,
                colspan,
                dor_colspan,
                attrs,
                visible,
                ..
            } => AemNode::TitleDraw {
                attrs: attrs.clone(),
                visible: *visible,
                uuid: *uuid,
                name: name.clone(),
                content: text!(content),
                heading_level: *heading_level,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
            },
            // No `text!()` here on purpose: the markup is carried per language
            // on the node, so it must NOT be folded into the master-text-keyed
            // dictionary. Copying the whole map is also what makes lower -> lift
            // a fixpoint for this variant.
            AemNodeTranslated::HtmlDisplayer {
                uuid,
                name,
                content,
                attrs,
                visible,
                colspan,
                dor_colspan,
                ..
            } => AemNode::HtmlDisplayer {
                uuid: *uuid,
                name: name.clone(),
                content: content.clone(),
                attrs: attrs.clone(),
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
            },
            AemNodeTranslated::Repeatable {
                uuid,
                name,
                title,
                children,
                min_occur,
                max_occur,
                bind_ref,
                frag_ref,
                attrs,
                visible,
                ..
            } => AemNode::Repeatable {
                attrs: attrs.clone(),
                visible: *visible,
                uuid: *uuid,
                name: name.clone(),
                title: text!(title),
                children: lower_children(children, master_lang, languages, dict, conflicts),
                min_occur: *min_occur,
                max_occur: *max_occur,
                bind_ref: bind_ref.clone(),
                frag_ref: frag_ref.clone(),
            },
            AemNodeTranslated::Fragment {
                uuid,
                name,
                title,
                frag_ref,
                bind_ref,
                attrs,
                visible,
                ..
            } => AemNode::Fragment {
                attrs: attrs.clone(),
                visible: *visible,
                uuid: *uuid,
                name: name.clone(),
                title: text!(title),
                frag_ref: frag_ref.clone(),
                bind_ref: bind_ref.clone(),
            },
            AemNodeTranslated::Preface { uuid, name, .. } => AemNode::Preface {
                uuid: *uuid,
                name: name.clone(),
            },
            AemNodeTranslated::Appendix { uuid, name, .. } => AemNode::Appendix {
                uuid: *uuid,
                name: name.clone(),
            },
            AemNodeTranslated::FootnotePlaceholder {
                uuid,
                name,
                colspan,
                ..
            } => AemNode::FootnotePlaceholder {
                uuid: *uuid,
                name: name.clone(),
                colspan: *colspan,
            },
            AemNodeTranslated::Custom {
                uuid,
                name,
                template_key,
                label,
                options,
                mandatory,
                visible,
                colspan,
                dor_colspan,
                bind_ref,
                attrs,
                ..
            } => AemNode::Custom {
                attrs: attrs.clone(),
                uuid: *uuid,
                name: name.clone(),
                template_key: template_key.clone(),
                label: text!(label),
                options: lower_options(options, master_lang, languages, dict, conflicts),
                mandatory: *mandatory,
                visible: *visible,
                colspan: *colspan,
                dor_colspan: *dor_colspan,
                bind_ref: bind_ref.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pairs: &[(&str, &str)]) -> AemI18nText {
        AemI18nText(pairs.iter().map(|(l, v)| (l.to_string(), v.to_string())).collect())
    }

    fn langs() -> Vec<String> {
        vec!["de".into(), "en".into()]
    }

    fn sample() -> AemNodeTranslated {
        AemNodeTranslated::Root {
            title: t(&[("de", "Formular"), ("en", "Form")]),
            children: vec![
                AemNodeTranslated::Panel {
                    uuid: Uuid::nil(),
                    passthrough: Default::default(),
                    name: "panel".into(),
                    title: t(&[("de", "Abschnitt"), ("en", "Section")]),
                    children: vec![
                        AemNodeTranslated::TextField {
                            attrs: AemAttrs::default(),
                            uuid: Uuid::nil(),
                    passthrough: Default::default(),
                            name: "f1".into(),
                            label: t(&[("de", "Nachname"), ("en", "Last name")]),
                            mandatory: true,
                            visible: true,
                            max_chars: None,
                            colspan: 6,
                            dor_colspan: None,
                            bind_ref: None,
                            kind: TextFieldKind::Plain,
                        },
                        AemNodeTranslated::Dropdown {
                            attrs: AemAttrs::default(),
                            uuid: Uuid::nil(),
                    passthrough: Default::default(),
                            name: "f2".into(),
                            label: t(&[("de", "Währung"), ("en", "Currency")]),
                            options: vec![AemOptionTranslated {
                                label: t(&[("de", "Ja"), ("en", "Yes")]),
                                value: "Y".into(),
                            }],
                            mandatory: false,
                            visible: true,
                            colspan: 6,
                            dor_colspan: None,
                            field_id: None,
                            conditions: vec![],
                            bind_ref: None,
                        },
                    ],
                    is_page: true,
                    attrs: AemAttrs::default(),
                    visible: true,
                    is_conditional: false,
                    dor_num_cols: None,
                    colspan: 12,
                    dor_colspan: None,
                    bind_ref: None,
                    frag_ref: None,
                },
            ],
        }
    }

    #[test]
    fn serde_round_trips() {
        let n = sample();
        let json = serde_json::to_string(&n).unwrap();
        let back: AemNodeTranslated = serde_json::from_str(&json).unwrap();
        assert_eq!(n, back);
        // Transparent map shape.
        assert!(json.contains("\"de\":\"Formular\""));
    }

    #[test]
    fn lowers_master_node_and_dict() {
        let (node, dict) = sample().lower("de", &langs());
        // Master AemNode carries the German strings.
        match &node {
            AemNode::Root { title, children } => {
                assert_eq!(title, "Formular");
                match &children[0] {
                    AemNode::Panel { title, children, name, .. } => {
                        assert_eq!(name, "panel");
                        assert_eq!(title, "Abschnitt");
                        match &children[0] {
                            AemNode::TextField { label, name, mandatory, colspan, .. } => {
                                assert_eq!(label, "Nachname");
                                assert_eq!(name, "f1");
                                assert!(*mandatory);
                                assert_eq!(*colspan, 6);
                            }
                            _ => panic!(),
                        }
                        match &children[1] {
                            AemNode::Dropdown { label, options, .. } => {
                                assert_eq!(label, "Währung");
                                assert_eq!(options[0].label, "Ja");
                                assert_eq!(options[0].value, "Y");
                            }
                            _ => panic!(),
                        }
                    }
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
        // Dict keyed by master text → { en → translation }; Root.title absent.
        assert_eq!(dict.get("Abschnitt").unwrap().get("en").unwrap(), "Section");
        assert_eq!(dict.get("Nachname").unwrap().get("en").unwrap(), "Last name");
        assert_eq!(dict.get("Ja").unwrap().get("en").unwrap(), "Yes");
        assert!(!dict.contains_key("Formular"), "Root.title must not enter the dict");
    }

    #[test]
    fn missing_language_falls_back() {
        let txt = t(&[("en", "Only English")]);
        assert_eq!(txt.master("de"), "Only English");
        // Empty text yields no dict entry.
        let mut dict = I18nDict::new();
        let mut conflicts = Vec::new();
        emit_translations(&AemI18nText::default(), "de", &langs(), &mut dict, &mut conflicts);
        assert!(dict.is_empty());
    }

    /// The AEM editor edits a *lowered* tree and records the lift back up, so a
    /// lower → lift round trip must preserve every translation. Without the
    /// `fd_`-keyed re-keying the lift finds no dictionary entry and every
    /// non-master language is silently dropped.
    #[test]
    fn lower_then_lift_round_trips_translations() {
        let tree = AemNodeTranslated::Root {
            title: t(&[("de", "Formular"), ("en", "Form")]),
            children: vec![AemNodeTranslated::TextField {
                attrs: AemAttrs::default(),
                uuid: Uuid::from_u128(7),
                passthrough: Default::default(),
                name: "f1".into(),
                label: t(&[("de", "Nachname"), ("en", "Last name")]),
                mandatory: false,
                visible: true,
                max_chars: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: TextFieldKind::Plain,
            }],
        };

        let (node, dict) = tree.lower("de", &langs());
        let translations = translation_data_from_master_dict(dict);
        let lifted = crate::aem::aem_to_translated(
            &node,
            &translations,
            &langs(),
            "de",
            &std::collections::HashMap::new(),
        );

        match &lifted {
            AemNodeTranslated::Root { children, .. } => match &children[0] {
                AemNodeTranslated::TextField { label, .. } => {
                    assert_eq!(label.get("de"), Some("Nachname"));
                    assert_eq!(
                        label.get("en"),
                        Some("Last name"),
                        "the non-master label must survive the round trip"
                    );
                }
                other => panic!("expected a text field, got {other:?}"),
            },
            other => panic!("expected a root, got {other:?}"),
        }
    }

    /// Fidelity passthrough lives outside `AemNode`, so a round trip has to
    /// re-attach it explicitly or editing an agent-authored tree strips it.
    #[test]
    fn lift_reattaches_passthrough_by_uuid() {
        let uuid = Uuid::from_u128(9);
        let mut raw_attributes = BTreeMap::new();
        raw_attributes.insert("myProp".to_string(), "{Boolean}true".to_string());
        let passthrough = Passthrough {
            raw_attributes,
            raw_children: vec![],
        };
        let tree = AemNodeTranslated::Root {
            title: t(&[("de", "Formular")]),
            children: vec![AemNodeTranslated::TextField {
                attrs: AemAttrs::default(),
                uuid,
                passthrough: passthrough.clone(),
                name: "f1".into(),
                label: t(&[("de", "Nachname")]),
                mandatory: false,
                visible: true,
                max_chars: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: TextFieldKind::Plain,
            }],
        };

        let (node, dict) = tree.lower("de", &langs());
        let lifted = crate::aem::aem_to_translated(
            &node,
            &translation_data_from_master_dict(dict),
            &langs(),
            "de",
            &tree.passthrough_map(),
        );

        assert_eq!(
            lifted.passthrough_map().get(&uuid),
            Some(&passthrough),
            "passthrough must be restored from the map, not lost with the lowering"
        );
    }

    #[test]
    fn same_lang_collision_is_reported_last_writer_wins() {
        let n = AemNodeTranslated::Root {
            title: t(&[]),
            children: vec![
                AemNodeTranslated::TextDraw {
                    visible: true,
                    uuid: Uuid::nil(),
                    passthrough: Default::default(),
                    name: "a".into(),
                    content: t(&[("de", "Hinweis"), ("en", "Note A")]),
                    attrs: AemAttrs::default(),
                    colspan: 12,
                    dor_colspan: None,
                },
                AemNodeTranslated::TextDraw {
                    visible: true,
                    uuid: Uuid::nil(),
                    passthrough: Default::default(),
                    name: "b".into(),
                    content: t(&[("de", "Hinweis"), ("en", "Note B")]),
                    attrs: AemAttrs::default(),
                    colspan: 12,
                    dor_colspan: None,
                },
            ],
        };
        let (_, dict, conflicts) = n.lower_checked("de", &langs());
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].lang, "en");
        // Last writer wins.
        assert_eq!(dict.get("Hinweis").unwrap().get("en").unwrap(), "Note B");
    }

    #[test]
    fn lowered_tree_builds_bilingual_package() {
        // Guards the mono-lingual regression: a bilingual AemNodeTranslated must
        // produce a package whose i18n dictionary carries the other language.
        use std::io::Read;

        let tree = AemNodeTranslated::Root {
            title: t(&[("en", "Form"), ("de", "Formular")]),
            children: vec![AemNodeTranslated::TextField {
                attrs: AemAttrs::default(),
                uuid: Uuid::nil(),
                passthrough: Default::default(),
                name: "f1".into(),
                label: t(&[("en", "Last name"), ("de", "Nachname")]),
                mandatory: false,
                visible: true,
                max_chars: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: TextFieldKind::Plain,
            }],
        };

        let mut config = crate::AemConfig::test_default("TEST");
        config.languages = vec!["en".into(), "de".into()];
        config.master_language = "en".into();

        let (root, dict) = tree.lower("en", &config.languages);
        assert_eq!(dict.get("Last name").unwrap().get("de").unwrap(), "Nachname");

        let zip_bytes =
            crate::generate_aem_package_from_node_with_translations(&root, &config, dict);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        let de_path = format!(
            "jcr_root/content/forms/af/{}/AF_TEST/_jcr_content/guideContainer/assets/dictionary/de.xml",
            config.form_path
        );
        let mut de_xml = String::new();
        archive
            .by_name(&de_path)
            .unwrap_or_else(|_| panic!("German dictionary must exist at {de_path}"))
            .read_to_string(&mut de_xml)
            .unwrap();
        assert!(
            de_xml.contains("sling:message=\"Nachname\""),
            "German dictionary must carry the translated label, got: {de_xml}"
        );
    }

    #[test]
    fn different_langs_compose_without_conflict() {
        let n = AemNodeTranslated::Root {
            title: t(&[]),
            children: vec![
                AemNodeTranslated::TextDraw {
                    visible: true,
                    uuid: Uuid::nil(),
                    passthrough: Default::default(),
                    name: "a".into(),
                    content: t(&[("de", "Wort"), ("en", "Word")]),
                    attrs: AemAttrs::default(),
                    colspan: 12,
                    dor_colspan: None,
                },
                AemNodeTranslated::TextDraw {
                    visible: true,
                    uuid: Uuid::nil(),
                    passthrough: Default::default(),
                    name: "b".into(),
                    content: t(&[("de", "Wort"), ("fr", "Mot")]),
                    attrs: AemAttrs::default(),
                    colspan: 12,
                    dor_colspan: None,
                },
            ],
        };
        let languages: Vec<String> = vec!["de".into(), "en".into(), "fr".into()];
        let (_, dict, conflicts) = n.lower_checked("de", &languages);
        assert!(conflicts.is_empty());
        let sub = dict.get("Wort").unwrap();
        assert_eq!(sub.get("en").unwrap(), "Word");
        assert_eq!(sub.get("fr").unwrap(), "Mot");
    }
}
