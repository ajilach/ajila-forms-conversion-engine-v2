//! AemNode → AemNodeTranslated converter ("lift")
//!
//! The inverse of [`AemNodeTranslated::lower`][crate::aem::AemNodeTranslated]:
//! it rebuilds the multilingual working tree from a parsed AEM package (a
//! single-language [`AemNode`] tree plus the Sling i18n [`TranslationData`]).
//!
//! Used to let an uploaded content-package ZIP act as a **template** that the
//! conversion agent pre-loads as its working tree and then edits, instead of
//! authoring from scratch.
//!
//! Each text field (title / label / static content / option labels) is lifted to
//! an [`AemI18nText`]: the master language is seeded with the node's own string,
//! and any per-language messages found in the dictionary are added. Every
//! non-text field is copied verbatim, so `lift` is the structural inverse of
//! `lower` (`lower(lift(node)).0 == node`).

use std::collections::{BTreeMap, HashMap};

use uuid::Uuid;

use super::parser::TranslationData;
use super::to_structured::lookup_translation_entry;
use super::translated::{AemI18nText, AemNodeTranslated, AemOptionTranslated};
use super::{AemNode, AemOption, Passthrough};

/// Lift a single-language [`AemNode`] tree (plus Sling translations) into the
/// multilingual [`AemNodeTranslated`] working tree.
///
/// `languages` is the full set of available languages (master included) and
/// `master_language` the form's default; both come from the parsed package (see
/// `Blueprint::aem_translated`).
pub fn aem_to_translated(
    root: &AemNode,
    translations: &TranslationData,
    languages: &[String],
    master_language: &str,
    raw_by_uuid: &HashMap<Uuid, Passthrough>,
) -> AemNodeTranslated {
    let ctx = LiftContext {
        translations,
        languages,
        master_language,
        raw_by_uuid,
    };
    lift_node(root, &ctx)
}

struct LiftContext<'a> {
    translations: &'a TranslationData,
    languages: &'a [String],
    master_language: &'a str,
    /// Per-node fidelity passthrough captured on load, keyed by uuid.
    raw_by_uuid: &'a HashMap<Uuid, Passthrough>,
}

impl LiftContext<'_> {
    /// The fidelity passthrough for a node (empty if none was captured).
    fn passthrough(&self, uuid: &Uuid) -> Passthrough {
        self.raw_by_uuid.get(uuid).cloned().unwrap_or_default()
    }

    /// Build the per-language text for `default` (the node's master-language
    /// string) at the given component/property. Always seeds the master
    /// language so lowering returns `default` unchanged.
    fn text(&self, default: &str, component_name: &str, property: &str) -> AemI18nText {
        let mut map = BTreeMap::new();
        map.insert(self.master_language.to_string(), default.to_string());

        if self.languages.len() > 1 && !self.translations.entries.is_empty() {
            if let Some(lang_map) =
                lookup_translation_entry(self.translations, default, component_name, property)
            {
                for (lang, message) in lang_map {
                    map.insert(lang.clone(), message.clone());
                }
            }
        }

        AemI18nText(map)
    }

    fn options(&self, options: &[AemOption], component_name: &str) -> Vec<AemOptionTranslated> {
        options
            .iter()
            .map(|opt| AemOptionTranslated {
                label: self.text(&opt.label, component_name, "options"),
                value: opt.value.clone(),
            })
            .collect()
    }

    fn children(&self, children: &[AemNode]) -> Vec<AemNodeTranslated> {
        children.iter().map(|c| lift_node(c, self)).collect()
    }
}

fn lift_node(node: &AemNode, ctx: &LiftContext) -> AemNodeTranslated {
    match node {
        AemNode::Root { title, children } => AemNodeTranslated::Root {
            // Root has no component name; titles are not dictionary-keyed.
            title: AemI18nText::single(ctx.master_language, title.clone()),
            children: ctx.children(children),
        },
        AemNode::Panel {
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
        } => AemNodeTranslated::Panel {
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            title: ctx.text(title, name, "jcr:title"),
            children: ctx.children(children),
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
        AemNode::TextField {
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
        } => AemNodeTranslated::TextField {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            label: ctx.text(label, name, "jcr:title"),
            mandatory: *mandatory,
            visible: *visible,
            max_chars: *max_chars,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            bind_ref: bind_ref.clone(),
            kind: *kind,
        },
        AemNode::NumberField {
            uuid,
            name,
            label,
            mandatory,
            visible,
            colspan,
            dor_colspan,
            bind_ref,
            attrs,
        } => AemNodeTranslated::NumberField {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            label: ctx.text(label, name, "jcr:title"),
            mandatory: *mandatory,
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            bind_ref: bind_ref.clone(),
        },
        AemNode::DatePicker {
            uuid,
            name,
            label,
            mandatory,
            visible,
            colspan,
            dor_colspan,
            bind_ref,
            attrs,
        } => AemNodeTranslated::DatePicker {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            label: ctx.text(label, name, "jcr:title"),
            mandatory: *mandatory,
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            bind_ref: bind_ref.clone(),
        },
        AemNode::Dropdown {
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
        } => AemNodeTranslated::Dropdown {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            label: ctx.text(label, name, "jcr:title"),
            options: ctx.options(options, name),
            mandatory: *mandatory,
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            field_id: field_id.clone(),
            conditions: conditions.clone(),
            bind_ref: bind_ref.clone(),
        },
        AemNode::Checkbox {
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
        } => AemNodeTranslated::Checkbox {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            label: ctx.text(label, name, "jcr:title"),
            options: ctx.options(options, name),
            alignment: *alignment,
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            field_id: field_id.clone(),
            conditions: conditions.clone(),
            bind_ref: bind_ref.clone(),
        },
        AemNode::RadioButton {
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
        } => AemNodeTranslated::RadioButton {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            label: ctx.text(label, name, "jcr:title"),
            options: ctx.options(options, name),
            alignment: *alignment,
            mandatory: *mandatory,
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            field_id: field_id.clone(),
            conditions: conditions.clone(),
            bind_ref: bind_ref.clone(),
        },
        AemNode::TextDraw {
            uuid,
            name,
            content,
            attrs,
            colspan,
            dor_colspan,
            visible,
        } => AemNodeTranslated::TextDraw {
            visible: *visible,
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            content: ctx.text(content, name, "_value"),
            attrs: attrs.clone(),
            colspan: *colspan,
            dor_colspan: *dor_colspan,
        },
        AemNode::MessageBox {
            uuid,
            name,
            content,
            attrs,
            colspan,
            dor_colspan,
            visible,
        } => AemNodeTranslated::MessageBox {
            visible: *visible,
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            content: ctx.text(content, name, "messageboxBody"),
            attrs: attrs.clone(),
            colspan: *colspan,
            dor_colspan: *dor_colspan,
        },
        AemNode::TitleDraw {
            uuid,
            name,
            content,
            heading_level,
            colspan,
            dor_colspan,
            attrs,
            visible,
        } => AemNodeTranslated::TitleDraw {
            attrs: attrs.clone(),
            visible: *visible,
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            content: ctx.text(content, name, "_value"),
            heading_level: *heading_level,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
        },
        // The markup is already per language on the node, so there is no
        // dictionary lookup to do -- copy the map straight across. That is what
        // makes `lower(lift(node)) == node` hold for this variant.
        AemNode::HtmlDisplayer {
            uuid,
            name,
            content,
            attrs,
            visible,
            colspan,
            dor_colspan,
        } => AemNodeTranslated::HtmlDisplayer {
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            content: content.clone(),
            attrs: attrs.clone(),
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
        },
        AemNode::Repeatable {
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
        } => AemNodeTranslated::Repeatable {
            attrs: attrs.clone(),
            visible: *visible,
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            title: ctx.text(title, name, "jcr:title"),
            children: ctx.children(children),
            min_occur: *min_occur,
            max_occur: *max_occur,
            bind_ref: bind_ref.clone(),
            frag_ref: frag_ref.clone(),
        },
        AemNode::Fragment {
            uuid,
            name,
            title,
            frag_ref,
            bind_ref,
            attrs,
            visible,
        } => AemNodeTranslated::Fragment {
            attrs: attrs.clone(),
            visible: *visible,
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            title: ctx.text(title, name, "jcr:title"),
            frag_ref: frag_ref.clone(),
            bind_ref: bind_ref.clone(),
        },
        AemNode::Preface { uuid, name } => AemNodeTranslated::Preface {
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
        },
        AemNode::Appendix { uuid, name } => AemNodeTranslated::Appendix {
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
        },
        AemNode::FootnotePlaceholder {
            uuid,
            name,
            colspan,
        } => AemNodeTranslated::FootnotePlaceholder {
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            colspan: *colspan,
        },
        AemNode::Custom {
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
        } => AemNodeTranslated::Custom {
            attrs: attrs.clone(),
            uuid: *uuid,
            passthrough: ctx.passthrough(uuid),
            name: name.clone(),
            template_key: template_key.clone(),
            label: ctx.text(label, name, "jcr:title"),
            options: ctx.options(options, name),
            mandatory: *mandatory,
            visible: *visible,
            colspan: *colspan,
            dor_colspan: *dor_colspan,
            bind_ref: bind_ref.clone(),
        },
    }
}
