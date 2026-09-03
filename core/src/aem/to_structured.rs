//! AemNode → StructuredNode converter
//!
//! Converts a parsed AEM form tree into the unified `StructuredNode`
//! representation. This is the reverse of `converter.rs` (which converts
//! StructuredNode → AemNode for output).
//!
//! The converter:
//! - Maps AEM component types to StructuredNode variants
//! - Generates deterministic FieldIds from component names
//! - Applies translations from the Sling i18n dictionary
//! - Respects script-driven visibility (hidden panels become Conditional nodes)

use std::collections::HashMap;

use super::parser::{TranslationData, VisibilityCondition};
use super::{AemNode, AemOption};
use crate::structured::{
    ConditionalNode, FieldCondition, FieldId, FieldNode, FieldType, GridLayout, GridLayoutElement,
    GroupNode, HeadingLevel, HeadingNode, HtmlNode, InlineText, InputValue, NameValue,
    NoticeNode, ParagraphNode, RepeatableNode, StructuredNode, TranslatableString, TranslatedText,
    TranslationMap,
};
use crate::xfa::scripting::SomPath;

// ============================================================================
// Public API
// ============================================================================

/// Convert an AEM node tree to a list of `StructuredNode`s.
///
/// `visibility` is the script-derived visibility map (component_name → visible).
/// `translations` provides Sling i18n dictionary data.
/// `languages` is the list of available languages.
/// `master_language` is the form's master/default language.
pub fn aem_to_structured(
    root: &AemNode,
    visibility: &HashMap<String, bool>,
    translations: &TranslationData,
    languages: &[String],
    master_language: &str,
    visibility_conditions: &HashMap<String, VisibilityCondition>,
) -> Vec<StructuredNode> {
    let ctx = ConversionContext {
        visibility,
        translations,
        languages,
        master_language,
        visibility_conditions,
    };

    match root {
        AemNode::Root { children, .. } => children
            .iter()
            .filter_map(|child| convert_node(child, &ctx))
            .collect(),
        _ => {
            if let Some(node) = convert_node(root, &ctx) {
                vec![node]
            } else {
                Vec::new()
            }
        }
    }
}

// ============================================================================
// Conversion context
// ============================================================================

struct ConversionContext<'a> {
    visibility: &'a HashMap<String, bool>,
    translations: &'a TranslationData,
    languages: &'a [String],
    master_language: &'a str,
    visibility_conditions: &'a HashMap<String, VisibilityCondition>,
}

// ============================================================================
// Node conversion
// ============================================================================

fn convert_node(node: &AemNode, ctx: &ConversionContext) -> Option<StructuredNode> {
    match node {
        AemNode::Root { children, .. } => {
            let nodes: Vec<_> = children
                .iter()
                .filter_map(|c| convert_node(c, ctx))
                .collect();
            if nodes.is_empty() {
                None
            } else {
                Some(StructuredNode::Group(GroupNode {
                    children: nodes,
                    column_flow: false,
                }))
            }
        }

        AemNode::Panel {
            name,
            children,
            visible,
            dor_num_cols,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);

            let child_nodes: Vec<_> = children
                .iter()
                .filter_map(|c| convert_node(c, ctx))
                .collect();

            if child_nodes.is_empty() {
                return None;
            }

            // Build panel content
            let mut panel_children = Vec::new();

            // Panel titles are ignored — only TitleDraw nodes produce headings.

            // Add children, potentially in a grid layout
            if let Some(cols) = dor_num_cols {
                if *cols > 1 {
                    // Wrap children in a grid layout
                    let elements: Vec<_> = child_nodes
                        .into_iter()
                        .map(|n| GridLayoutElement { span: 1, node: n })
                        .collect();
                    panel_children.push(StructuredNode::GridLayout(GridLayout {
                        columns: *cols as usize,
                        elements,
                    }));
                } else {
                    panel_children.extend(child_nodes);
                }
            } else {
                panel_children.extend(child_nodes);
            }

            let content = if panel_children.len() == 1 {
                panel_children.pop().unwrap()
            } else {
                StructuredNode::Group(GroupNode {
                    children: panel_children,
                    column_flow: false,
                })
            };

            // If this panel has a visibility condition, always wrap as Conditional
            if let Some(cond) = ctx.visibility_conditions.get(name) {
                return Some(StructuredNode::Conditional(ConditionalNode {
                    condition: FieldCondition {
                        field_name: field_id_from_name(&cond.trigger_field),
                        value: InputValue::Text(cond.trigger_value.clone()),
                    },
                    content: Box::new(content),
                }));
            }

            if !is_visible { None } else { Some(content) }
        }

        AemNode::TextField {
            name,
            label,
            visible,
            max_chars,
            mandatory,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);
            if !is_visible {
                return None;
            }

            let label_text = translate_string(label, name, "jcr:title", ctx);
            let field_id = field_id_from_name(name);

            Some(StructuredNode::Field(FieldNode {
                name: field_id,
                som_path: Some(SomPath::new(name)),
                label: Some(inline_from_translatable(&label_text)),
                input_type: FieldType::Text {
                    regex: None,
                    max_length: *max_chars,
                    min_length: None,
                },
                value: None,
                placeholder: None,
                required: *mandatory,
            }))
        }

        AemNode::NumberField {
            name,
            label,
            visible,
            mandatory,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);
            if !is_visible {
                return None;
            }

            let label_text = translate_string(label, name, "jcr:title", ctx);
            let field_id = field_id_from_name(name);

            Some(StructuredNode::Field(FieldNode {
                name: field_id,
                som_path: Some(SomPath::new(name)),
                label: Some(inline_from_translatable(&label_text)),
                input_type: FieldType::Number {
                    min: None,
                    max: None,
                    step: None,
                },
                value: None,
                placeholder: None,
                required: *mandatory,
            }))
        }

        AemNode::DatePicker {
            name,
            label,
            visible,
            mandatory,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);
            if !is_visible {
                return None;
            }

            let label_text = translate_string(label, name, "jcr:title", ctx);
            let field_id = field_id_from_name(name);

            Some(StructuredNode::Field(FieldNode {
                name: field_id,
                som_path: Some(SomPath::new(name)),
                label: Some(inline_from_translatable(&label_text)),
                input_type: FieldType::Date,
                value: None,
                placeholder: None,
                required: *mandatory,
            }))
        }

        AemNode::Dropdown {
            name,
            label,
            options,
            visible,
            mandatory,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);
            if !is_visible {
                return None;
            }

            let label_text = translate_string(label, name, "jcr:title", ctx);
            let field_id = field_id_from_name(name);
            let option_values = convert_options(options, name, ctx);

            Some(StructuredNode::Field(FieldNode {
                name: field_id,
                som_path: Some(SomPath::new(name)),
                label: Some(inline_from_translatable(&label_text)),
                input_type: FieldType::Select {
                    options: option_values,
                },
                value: None,
                placeholder: None,
                required: *mandatory,
            }))
        }

        AemNode::RadioButton {
            name,
            label,
            options,
            visible,
            mandatory,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);
            if !is_visible {
                return None;
            }

            let label_text = translate_string(label, name, "jcr:title", ctx);
            let field_id = field_id_from_name(name);
            let option_values = convert_options(options, name, ctx);

            Some(StructuredNode::Field(FieldNode {
                name: field_id,
                som_path: Some(SomPath::new(name)),
                label: Some(inline_from_translatable(&label_text)),
                input_type: FieldType::Radio {
                    options: option_values,
                },
                value: None,
                placeholder: None,
                required: *mandatory,
            }))
        }

        AemNode::Checkbox {
            name,
            label,
            options,
            visible,
            ..
        } => {
            let is_visible = ctx.visibility.get(name).copied().unwrap_or(*visible);
            if !is_visible {
                return None;
            }

            let field_id = field_id_from_name(name);
            // Single checkbox → Bool field; multiple → CheckboxGroup
            if options.len() <= 1 {
                let label_text = options
                    .first()
                    .map(|o| TranslatableString::Plain(strip_html_tags(&o.label)));

                Some(StructuredNode::Field(FieldNode {
                    name: field_id,
                    som_path: Some(SomPath::new(name)),
                    label: label_text.map(|t| inline_from_translatable(&t)),
                    input_type: FieldType::Bool,
                    value: None,
                    placeholder: None,
                    required: false,
                }))
            } else {
                let label_text = translate_string(label, name, "jcr:title", ctx);
                let option_values = convert_options(options, name, ctx);
                Some(StructuredNode::Field(FieldNode {
                    name: field_id,
                    som_path: Some(SomPath::new(name)),
                    label: if label_text.as_str().is_empty() {
                        None
                    } else {
                        Some(inline_from_translatable(&label_text))
                    },
                    input_type: FieldType::CheckboxGroup {
                        options: option_values,
                    },
                    value: None,
                    placeholder: None,
                    required: false,
                }))
            }
        }

        AemNode::MessageBox { name, content, .. } => {
            let text = translate_string(content, name, "messageboxBody", ctx);
            if strip_html_tags(text.as_str()).trim().is_empty() {
                return None;
            }
            Some(StructuredNode::Notice(NoticeNode {
                content: inline_from_translatable(&TranslatableString::Plain(
                    strip_html_tags(text.as_str()),
                )),
                som_path: None,
                source_name: Some(name.clone()),
            }))
        }
        AemNode::TextDraw {
            name,
            content,
            attrs,
            ..
        } => {
            if attrs.dor_exclude {
                return None;
            }

            let text = translate_string(content, name, "_value", ctx);
            if text.as_str().is_empty() {
                return None;
            }

            // Strip HTML tags for plain text representation
            let plain = strip_html_tags(text.as_str());
            if plain.trim().is_empty() {
                return None;
            }

            Some(StructuredNode::Paragraph(ParagraphNode {
                content: inline_from_translatable(&TranslatableString::Plain(plain)),
                som_path: None,
                source_name: Some(name.clone()),
            }))
        }

        AemNode::TitleDraw {
            name,
            content,
            heading_level,
            ..
        } => {
            let text = translate_string(content, name, "_value", ctx);
            if text.as_str().is_empty() {
                return None;
            }

            let plain = strip_html_tags(text.as_str());
            let level = match heading_level {
                1 => HeadingLevel::H1,
                2 => HeadingLevel::H2,
                3 => HeadingLevel::H3,
                4 => HeadingLevel::H4,
                5 => HeadingLevel::H5,
                _ => HeadingLevel::H3,
            };

            Some(StructuredNode::Heading(HeadingNode {
                level,
                content: inline_from_translatable(&TranslatableString::Plain(plain)),
                som_path: None,
                source_name: Some(name.clone()),
            }))
        }

        // The markup is already per language on the node, so there is nothing
        // to look up and nothing to strip: hand it over whole. That is what lets
        // `generate_html` show the reader the table (or chart, or image) the
        // component renders, instead of a flattened run of paragraphs.
        AemNode::HtmlDisplayer { name, content, .. } => {
            if content.0.values().all(|m| m.trim().is_empty()) {
                return None;
            }
            Some(StructuredNode::Html(HtmlNode {
                content: content
                    .0
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                source_name: Some(name.clone()),
            }))
        }

        AemNode::Repeatable {
            name: _,
            title: _,
            children,
            min_occur,
            max_occur,
            ..
        } => {
            let child_nodes: Vec<_> = children
                .iter()
                .filter_map(|c| convert_node(c, ctx))
                .collect();

            if child_nodes.is_empty() {
                return None;
            }

            let mut panel_children = Vec::new();
            // Repeatable titles are ignored — only TitleDraw nodes produce headings.
            panel_children.extend(child_nodes);

            let item = if panel_children.len() == 1 {
                panel_children.pop().unwrap()
            } else {
                StructuredNode::Group(GroupNode {
                    children: panel_children,
                    column_flow: false,
                })
            };

            Some(StructuredNode::Repeatable(RepeatableNode {
                item: Box::new(item),
                min_occurrences: *min_occur,
                max_occurrences: Some(*max_occur),
            }))
        }

        AemNode::Fragment { name, .. } => {
            // Unresolved fragment — skip
            log::debug!("Skipping unresolved fragment: {name}");
            None
        }

        AemNode::Preface { .. }
        | AemNode::Appendix { .. }
        | AemNode::FootnotePlaceholder { .. }
        | AemNode::Custom { .. } => None,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Generate a deterministic FieldId from an AEM component name.
fn field_id_from_name(name: &str) -> FieldId {
    FieldId::from(name)
}

/// Find the per-language message map for a component's property in the Sling
/// dictionary.
///
/// Builds the dictionary key from the component name hierarchy
/// (e.g. `guideContainer##rootPanel##items##fieldName##jcr:title##1234`) and
/// matches it, falling back to a bare `##name##` suffix and finally the `fd_`
/// fragment-dictionary key. Shared by [`translate_string`] (AEM → structured)
/// and the `to_translated` lift (AEM → `AemNodeTranslated`).
pub(super) fn lookup_translation_entry<'a>(
    translations: &'a TranslationData,
    default: &str,
    component_name: &str,
    property: &str,
) -> Option<&'a HashMap<String, String>> {
    // Strategy 1: Sling keys like guideContainer##rootPanel##items##...##property##hash
    let key_suffix = format!("##{}##{}", component_name, property);
    let lang_map = translations
        .entries
        .keys()
        .find(|k| k.contains(&key_suffix) || k.ends_with(&format!("##{}##", component_name)))
        .and_then(|k| translations.entries.get(k));

    // Strategy 2: fd_ prefixed keys (used by fragment dictionaries)
    lang_map.or_else(|| translations.entries.get(&format!("fd_{}", default)))
}

/// Try to look up a translation for the given component and property.
///
/// Builds the Sling dictionary key from the component name hierarchy
/// (e.g. `guideContainer##rootPanel##items##fieldName##jcr:title##1234`)
/// and looks for it in the translation data.
fn translate_string(
    default: &str,
    component_name: &str,
    property: &str,
    ctx: &ConversionContext,
) -> TranslatableString {
    if ctx.languages.len() <= 1 || ctx.translations.entries.is_empty() {
        return TranslatableString::Plain(default.to_string());
    }

    if let Some(lang_map) =
        lookup_translation_entry(ctx.translations, default, component_name, property)
    {
        let mut translation_map: TranslationMap = HashMap::new();

        // Add master language with the default value
        translation_map.insert(ctx.master_language.to_string(), Some(default.to_string()));

        // Add available translations
        for (lang, message) in lang_map {
            translation_map.insert(lang.clone(), Some(message.clone()));
        }

        return TranslatableString::Translated(translation_map);
    }

    TranslatableString::Plain(default.to_string())
}

/// Convert AEM options to StructuredNode NameValue options.
///
/// Strips HTML tags from labels and looks up translations via the Sling
/// `fd_` dictionary key format.
fn convert_options(
    options: &[AemOption],
    component_name: &str,
    ctx: &ConversionContext,
) -> Vec<NameValue> {
    options
        .iter()
        .map(|opt| {
            let plain_label = strip_html_tags(&opt.label);
            let name = translate_string(&plain_label, component_name, "options", ctx);
            NameValue {
                name,
                value: InputValue::Text(opt.value.clone()),
            }
        })
        .collect()
}

/// Create InlineText from a TranslatableString.
fn inline_from_translatable(text: &TranslatableString) -> TranslatedText {
    match text {
        TranslatableString::Plain(s) => TranslatedText::plain(s.as_str()),
        TranslatableString::Translated(map) => {
            let mut tt_map = std::collections::HashMap::new();
            for (lang, value) in map {
                if let Some(s) = value {
                    tt_map.insert(lang.clone(), InlineText::plain(s.as_str()));
                } else {
                    tt_map.insert(lang.clone(), InlineText::empty());
                }
            }
            TranslatedText::new(tt_map)
        }
    }
}

/// Simple HTML tag stripper.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#xa;", "\n")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<p><b>Hello</b> world</p>"), "Hello world");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<br/>text"), "text");
    }

    #[test]
    fn test_field_id_from_name() {
        let id1 = field_id_from_name("txtMandator");
        let id2 = field_id_from_name("txtMandator");
        assert_eq!(id1, id2); // Deterministic
    }

    #[test]
    fn test_simple_conversion() {
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![AemNode::TextField {
                attrs: crate::AemAttrs::default(),
                uuid: Uuid::nil(),
                name: "myField".into(),
                label: "My Field".into(),
                mandatory: false,
                visible: true,
                max_chars: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: crate::aem::TextFieldKind::Plain,
            }],
        };

        let result = aem_to_structured(
            &root,
            &HashMap::new(),
            &TranslationData::default(),
            &["en".to_string()],
            "en",
            &HashMap::new(),
        );

        assert_eq!(result.len(), 1);
        match &result[0] {
            StructuredNode::Field(f) => {
                assert_eq!(f.label.as_ref().unwrap().as_plain_text(), "My Field");
            }
            _ => panic!("Expected Field node"),
        }
    }
}
