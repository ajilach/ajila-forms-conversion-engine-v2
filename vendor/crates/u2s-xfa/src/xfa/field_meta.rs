//! What a field's template says about its value beyond the value itself:
//! how it is shown and typed (`<format>` and `<ui>` pictures, XFA 3.3
//! "The format element", "The ui element"), how long it may be (`<text
//! maxChars>`), whether it takes several lines (`<textEdit multiLine>`), its
//! tooltip (`<assist><toolTip>`), and whether it is mandatory (`<validate
//! nullTest>`).
//!
//! Read once, when the document is parsed, together with the field's
//! prevailing locale: the nearest `locale` attribute on the field or an
//! enclosing container (XFA 3.3 "Determining the Prevailing Locale"),
//! resolved against the document's `<localeSet>`.

use super::locale::{Locale, LocaleSet};
use super::picture::{Category, Picture, is_canonical};
use super::{XfaNode, XfaNodeKind};
use std::collections::HashMap;
use std::sync::Arc;

/// A picture clause as the template wrote it, and what parsing it gave: a
/// picture this engine does not support is kept as text (it still tells a
/// reader the expected format) and shown unformatted.
#[derive(Debug, Clone, PartialEq)]
pub struct PictureSpec {
    pub source: String,
    pub picture: Result<Picture, String>,
}

impl PictureSpec {
    fn new(source: String) -> PictureSpec {
        let picture = Picture::parse(&source);
        if let Err(e) = &picture {
            log::warn!("{e}; values with it are shown unformatted");
        }
        PictureSpec { source, picture }
    }

    /// `raw` through this picture, when it parsed and the value fits it.
    pub fn format(&self, raw: &str, locale: &Locale) -> Option<String> {
        self.picture.as_ref().ok()?.format(raw, locale)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldMeta {
    /// `<format><picture>`: how the value is shown.
    pub format: Option<PictureSpec>,
    /// `<ui>`'s picture: how the value is typed (and shown while editing).
    pub edit: Option<PictureSpec>,
    /// `<validate><picture>`: the shape a value must have.
    pub validate: Option<PictureSpec>,
    /// `<value><text maxChars>`.
    pub max_chars: Option<u32>,
    /// `<ui><textEdit multiLine="1">`.
    pub multiline: bool,
    /// `<assist><toolTip>`.
    pub tooltip: Option<String>,
    /// `<validate nullTest>`: `error` or `warning` when an empty value is
    /// reported; absent (or `disabled`) when it may stay empty.
    pub null_test: Option<String>,
    /// The prevailing locale the pictures format with.
    pub locale: Arc<Locale>,
}

fn tag(node: &XfaNode) -> Option<&str> {
    match &node.kind {
        XfaNodeKind::Element { tag_name, .. } => Some(tag_name.as_str()),
        _ => None,
    }
}

fn child<'a>(node: &'a XfaNode, name: &str) -> Option<&'a XfaNode> {
    node.children.iter().find(|c| tag(c) == Some(name))
}

fn text_of(node: &XfaNode) -> Option<String> {
    let text = match &node.kind {
        XfaNodeKind::Element {
            text_content: Some(text),
            ..
        } => text.clone(),
        _ => node
            .children
            .iter()
            .filter_map(|c| match &c.kind {
                XfaNodeKind::Text { content } => Some(content.as_str()),
                _ => None,
            })
            .collect(),
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn picture_in(node: Option<&XfaNode>) -> Option<PictureSpec> {
    child(node?, "picture").and_then(text_of).map(PictureSpec::new)
}

impl FieldMeta {
    /// The metadata of the `<field>` `node`, with `locale` prevailing.
    pub fn of_field(node: &XfaNode, locale: Arc<Locale>) -> FieldMeta {
        let ui = child(node, "ui");
        let widget = ui.and_then(|ui| ui.children.iter().find(|c| tag(c).is_some_and(|t| t != "picture" && t != "extras")));
        // The edit picture sits directly in `<ui>` (Designer's placement) or
        // inside its widget.
        let edit = picture_in(ui).or_else(|| picture_in(widget));
        let validate = child(node, "validate");
        let value_text = node
            .children
            .iter()
            .find(|c| matches!(c.kind, XfaNodeKind::Value))
            .and_then(|v| child(v, "text"));
        FieldMeta {
            format: picture_in(child(node, "format")),
            edit,
            validate: picture_in(validate),
            max_chars: value_text
                .and_then(|t| t.attributes.get("maxChars"))
                .and_then(|m| m.trim().parse().ok())
                .filter(|m| *m > 0),
            multiline: widget.is_some_and(|w| {
                tag(w) == Some("textEdit") && w.attributes.get("multiLine").map(String::as_str) == Some("1")
            }),
            tooltip: child(node, "assist")
                .and_then(|a| child(a, "toolTip"))
                .and_then(text_of),
            null_test: validate
                .and_then(|v| v.attributes.get("nullTest"))
                .filter(|t| t.as_str() != "disabled")
                .cloned(),
            locale,
        }
    }

    /// Whether the field must not be left empty (`nullTest="error"`).
    pub fn mandatory(&self) -> bool {
        self.null_test.as_deref() == Some("error")
    }

    /// `raw` as the field shows it: through its format picture, or its edit
    /// picture when it has no format picture. `None` when neither applies
    /// (the raw value is shown).
    pub fn display(&self, raw: &str) -> Option<String> {
        self.format
            .as_ref()
            .or(self.edit.as_ref())
            .and_then(|p| p.format(raw, &self.locale))
    }

    /// What a person typed as the value the form stores (XFA 3.3 "Input
    /// parsing"): a value already in canonical form for the field's
    /// pictures is kept, anything else is read through the edit picture,
    /// then the format picture, and kept as typed when neither reads it (the
    /// form's own validation then judges it).
    pub fn canonical_input(&self, input: &str) -> String {
        let categories: Vec<Category> = [self.edit.as_ref(), self.format.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|p| p.picture.as_ref().ok())
            .flat_map(Picture::categories)
            .filter(|c| matches!(c, Category::Date | Category::Time | Category::Num))
            .collect();
        if input.trim().is_empty() || categories.iter().any(|c| is_canonical(*c, input)) {
            return input.to_string();
        }
        self.parse_input(input).unwrap_or_else(|| input.to_string())
    }

    /// What a person typed, read into the canonical value through the edit
    /// picture, else the format picture. `None` when neither reads it.
    pub fn parse_input(&self, input: &str) -> Option<String> {
        [self.edit.as_ref(), self.format.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|p| p.picture.as_ref().ok()?.parse_input(input, &self.locale))
    }
}

/// Give every `<field>` in `nodes` its [`FieldMeta`], its locale inherited
/// from the nearest enclosing `locale` attribute and resolved against the
/// document's `<localeSet>`.
pub fn attach(nodes: &mut [XfaNode]) {
    let locales = LocaleSet::from_nodes(nodes);
    let mut resolved: HashMap<Option<String>, Arc<Locale>> = HashMap::new();
    fn walk(
        nodes: &mut [XfaNode],
        inherited: Option<&str>,
        locales: &LocaleSet,
        resolved: &mut HashMap<Option<String>, Arc<Locale>>,
    ) {
        for node in nodes {
            let own = node.attributes.get("locale").cloned();
            let prevailing = own.as_deref().or(inherited).map(str::to_string);
            if matches!(node.kind, XfaNodeKind::Field) {
                let locale = resolved
                    .entry(prevailing.clone())
                    .or_insert_with(|| Arc::new(locales.resolve(prevailing.as_deref())))
                    .clone();
                node.field_meta = Some(Box::new(FieldMeta::of_field(node, locale)));
            }
            walk(&mut node.children, prevailing.as_deref(), locales, resolved);
        }
    }
    walk(nodes, None, &locales, &mut resolved);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_of(field_xml: &str) -> FieldMeta {
        let xml = format!(
            r#"<xdp><localeSet><locale name="it_IT"><numberSymbols><numberSymbol name="decimal">,</numberSymbol><numberSymbol name="grouping">.</numberSymbol></numberSymbols></locale></localeSet><template><subform name="Root" locale="it_IT">{field_xml}</subform></template></xdp>"#
        );
        let nodes = XfaNode::parse(xml.as_bytes()).expect("xml");
        fn find(nodes: &[XfaNode]) -> Option<&XfaNode> {
            nodes.iter().find_map(|n| {
                if matches!(n.kind, XfaNodeKind::Field) {
                    Some(n)
                } else {
                    find(&n.children)
                }
            })
        }
        *find(&nodes).expect("a field").field_meta.clone().expect("meta")
    }

    #[test]
    fn a_ubs_date_field_reads_both_pictures_and_its_tooltip() {
        let meta = meta_of(
            r#"<field name="SGN_Date1"><ui><dateTimeEdit/><picture>date{DD.MM.YYYY}</picture></ui><value><date/></value><format><picture>date{DD.MM.YYYY}</picture></format><assist><toolTip>Date</toolTip></assist></field>"#,
        );
        assert_eq!(meta.format.as_ref().map(|p| p.source.as_str()), Some("date{DD.MM.YYYY}"));
        assert_eq!(meta.edit.as_ref().map(|p| p.source.as_str()), Some("date{DD.MM.YYYY}"));
        assert_eq!(meta.tooltip.as_deref(), Some("Date"));
        assert_eq!(meta.display("2024-01-05").as_deref(), Some("05.01.2024"));
        assert_eq!(meta.parse_input("05.01.2024").as_deref(), Some("2024-01-05"));
        assert!(!meta.mandatory());
    }

    #[test]
    fn typed_input_is_read_into_its_canonical_form() {
        let date = meta_of(
            r#"<field name="D"><ui><dateTimeEdit/><picture>date{DD.MM.YYYY}</picture></ui></field>"#,
        );
        assert_eq!(date.canonical_input("05.01.2024"), "2024-01-05");
        assert_eq!(date.canonical_input("2024-01-05"), "2024-01-05", "already canonical");
        assert_eq!(date.canonical_input("soon"), "soon", "unreadable input is kept");
        let amount = meta_of(
            r#"<field name="A"><format><picture>num{zzz,zz9.99}</picture></format></field>"#,
        );
        assert_eq!(amount.canonical_input("1.234,50"), "1234.5", "it_IT reads the comma");
        assert_eq!(amount.canonical_input("1234.5"), "1234.5");
        let text = meta_of(r#"<field name="T"><ui><textEdit/></ui></field>"#);
        assert_eq!(text.canonical_input("05.01.2024"), "05.01.2024");
    }

    #[test]
    fn length_lines_and_mandatory_come_from_value_ui_and_validate() {
        let meta = meta_of(
            r#"<field name="F"><ui><textEdit multiLine="1"/></ui><value><text maxChars="4"/></value><validate nullTest="error"/></field>"#,
        );
        assert_eq!(meta.max_chars, Some(4));
        assert!(meta.multiline);
        assert!(meta.mandatory());
        assert_eq!(meta.display("anything"), None, "no picture, shown as is");
    }

    #[test]
    fn the_locale_is_inherited_from_the_enclosing_subform() {
        let meta = meta_of(
            r#"<field name="Amount"><ui><numericEdit/></ui><format><picture>num{zzz,zz9.99}</picture></format></field>"#,
        );
        assert_eq!(meta.locale.name, "it_IT");
        assert_eq!(meta.display("1234.5").as_deref(), Some("1.234,50"));
    }

    #[test]
    fn an_unsupported_picture_is_kept_as_text_and_shows_the_raw_value() {
        let meta = meta_of(
            r#"<field name="T"><format><picture>time{HH:MM Z}</picture></format></field>"#,
        );
        let format = meta.format.as_ref().expect("kept");
        assert_eq!(format.source, "time{HH:MM Z}");
        assert!(format.picture.is_err());
        assert_eq!(meta.display("10:30:00"), None);
    }
}
