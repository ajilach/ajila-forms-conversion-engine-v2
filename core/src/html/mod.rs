//! HTML Form Generator Module
//!
//! Converts a structured NodeTree into a standalone, interactive HTML form
//! with embedded JavaScript for dynamic repeatables and conditionals.

use crate::structured::{
    ConditionalNode, FieldCondition, FieldId, FieldNode, FieldType, FootnoteNode, NoticeNode, GroupNode,
    HeadingLevel, HeadingNode, ImageNode, InlineNode, InlineText, InputValue, ListNode,
    ParagraphNode, RepeatableNode, StructuredNode, TableNode, TranslatedText,
    collect_footnote_nodes,
};
use crate::xfa::scripting::SomPath;
use serde::Deserialize;
use std::path::PathBuf;

// ============================================================================
// Profile types (TOML-deserializable)
// ============================================================================

/// A font-family declaration in the HTML profile TOML.
///
/// Each entry maps to one CSS `font-family`. Individual variants (regular,
/// bold, italic, bold-italic) point to TTF/WOFF2 files relative to the
/// `html/` profile directory.
#[derive(Debug, Clone, Deserialize)]
pub struct FontFamilyProfile {
    /// CSS font-family name.
    pub family: String,
    /// Path to the regular-weight font file.
    pub regular: Option<PathBuf>,
    /// Path to the bold-weight font file.
    pub bold: Option<PathBuf>,
    /// Path to the italic font file.
    pub italic: Option<PathBuf>,
    /// Path to the bold-italic font file.
    pub bold_italic: Option<PathBuf>,
}

/// TOML-deserializable HTML profile loaded from
/// `profiles/{name}/html/config.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HtmlProfile {
    /// Path to a CSS stylesheet file (relative to the `html/` directory).
    pub stylesheet: Option<PathBuf>,
    /// Path to a logo image file (relative to the `html/` directory).
    pub logo: Option<PathBuf>,
    /// Font-face declarations.
    #[serde(default)]
    pub fonts: Vec<FontFamilyProfile>,
}

// ============================================================================
// Resolved custom styles (pre-loaded data, no file I/O needed)
// ============================================================================

/// A single resolved `@font-face` variant with its base64 data-URI.
#[derive(Debug, Clone)]
pub struct ResolvedFontVariant {
    /// CSS `font-weight` value (e.g. `"normal"`, `"bold"`).
    pub weight: String,
    /// CSS `font-style` value (e.g. `"normal"`, `"italic"`).
    pub style: String,
    /// The full `data:font/ttf;base64,...` URI.
    pub data_uri: String,
}

/// A resolved font-family with all its variants ready for embedding.
#[derive(Debug, Clone)]
pub struct ResolvedFontFamily {
    /// CSS `font-family` name.
    pub family: String,
    /// Resolved variants.
    pub variants: Vec<ResolvedFontVariant>,
}

/// Pre-resolved custom styling data that is embedded directly into the HTML.
///
/// All file I/O happens *before* this struct is created (in `main.rs`);
/// the HTML generator only consumes the already-loaded data.
#[derive(Debug, Clone, Default)]
pub struct HtmlCustomStyles {
    /// Raw CSS text from the external stylesheet.
    pub stylesheet_css: Option<String>,
    /// Logo as a complete `data:image/…;base64,…` URI.
    pub logo_data_uri: Option<String>,
    /// Resolved `@font-face` families.
    pub font_faces: Vec<ResolvedFontFamily>,
}

// ============================================================================
// HtmlConfig
// ============================================================================

/// Configuration for HTML generation
#[derive(Debug, Clone)]
pub struct HtmlConfig {
    /// Form ID attribute
    pub form_id: String,
    /// Include inline CSS styles
    pub include_styles: bool,
    /// Include JavaScript for dynamic behavior
    pub include_scripts: bool,
    /// Optional custom styles (fonts, logo, CSS) to embed.
    pub custom_styles: Option<HtmlCustomStyles>,
}

impl Default for HtmlConfig {
    fn default() -> Self {
        Self {
            form_id: "generated-form".to_string(),
            include_styles: true,
            include_scripts: true,
            custom_styles: None,
        }
    }
}

/// Generate a complete HTML document from structured nodes
pub fn generate_html(nodes: &[StructuredNode], config: &HtmlConfig) -> String {
    let mut html = String::new();

    // Detect available languages from the content
    let languages = collect_languages(nodes);

    // HTML document header
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n");
    html.push_str("  <meta charset=\"UTF-8\">\n");
    html.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
    html.push_str("  <title>Generated Form</title>\n");

    if config.include_styles {
        html.push_str(&generate_styles(config.custom_styles.as_ref()));
    }

    // If multilingual, set body class to first language by default
    if languages.len() > 1 {
        html.push_str(&format!(
            "</head>\n<body class=\"lang-{}\">\n",
            escape_attr(&languages[0])
        ));
    } else {
        html.push_str("</head>\n<body>\n");
    }

    // Sticky header with optional logo and language selector
    let has_logo = config
        .custom_styles
        .as_ref()
        .and_then(|s| s.logo_data_uri.as_ref())
        .is_some();
    let has_lang_selector = languages.len() > 1;

    if has_logo || has_lang_selector {
        html.push_str("  <header class=\"site-header\">\n");

        // Logo
        if let Some(data_uri) = config
            .custom_styles
            .as_ref()
            .and_then(|s| s.logo_data_uri.as_ref())
        {
            html.push_str(&format!(
                "    <img class=\"site-logo\" src=\"{}\" alt=\"Logo\">\n",
                escape_attr(data_uri)
            ));
        }

        // Language selector
        if has_lang_selector {
            html.push_str("    <div class=\"language-selector\">\n");
            html.push_str("      <label for=\"language-select\">Language: </label>\n");
            html.push_str("      <select id=\"language-select\">\n");
            for lang in &languages {
                let label = match lang.as_str() {
                    "de" => "Deutsch",
                    "en" => "English",
                    "fr" => "Français",
                    "it" => "Italiano",
                    "es" => "Español",
                    other => other,
                };
                html.push_str(&format!(
                    "        <option value=\"{}\">{}</option>\n",
                    escape_attr(lang),
                    escape_html(label)
                ));
            }
            html.push_str("      </select>\n");
            html.push_str("    </div>\n");
        }

        html.push_str("  </header>\n");
    }

    // Main content area
    html.push_str("  <main>\n");

    // Form container
    html.push_str(&format!(
        "    <form id=\"{}\" class=\"generated-form\">\n",
        escape_attr(&config.form_id)
    ));

    // Generate form content (footnotes are rendered at the end)
    let mut ctx = GeneratorContext::new();
    for node in nodes {
        html.push_str(&generate_node(node, &mut ctx, 3));
    }

    // Render footnotes as the last elements
    let footnotes = collect_footnote_nodes(nodes);
    if !footnotes.is_empty() {
        html.push_str("      <div class=\"footnotes\">\n");
        for f in &footnotes {
            html.push_str(&generate_footnote(f, "        "));
        }
        html.push_str("      </div>\n");
    }

    html.push_str("    </form>\n");
    html.push_str("  </main>\n");

    if config.include_scripts {
        html.push_str(&generate_scripts(&config.form_id));
    }

    html.push_str("</body>\n</html>\n");

    html
}

/// Generate just the form body (without HTML document wrapper)
pub fn generate_form_body(nodes: &[StructuredNode]) -> String {
    let mut html = String::new();
    let mut ctx = GeneratorContext::new();
    for node in nodes {
        html.push_str(&generate_node(node, &mut ctx, 0));
    }

    // Render footnotes as the last elements
    let footnotes = collect_footnote_nodes(nodes);
    if !footnotes.is_empty() {
        html.push_str("<div class=\"footnotes\">\n");
        for f in &footnotes {
            html.push_str(&generate_footnote(f, "  "));
        }
        html.push_str("</div>\n");
    }

    html
}

/// Internal context for tracking generation state
struct GeneratorContext {
    /// Counter for generating unique IDs
    id_counter: usize,
    /// Current repeatable nesting depth for index tracking
    repeatable_depth: usize,
}

impl GeneratorContext {
    fn new() -> Self {
        Self {
            id_counter: 0,
            repeatable_depth: 0,
        }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.id_counter += 1;
        format!("{}_{}", prefix, self.id_counter)
    }
}

/// Generate HTML for a single node
fn generate_node(node: &StructuredNode, ctx: &mut GeneratorContext, indent: usize) -> String {
    let ind = "  ".repeat(indent);

    match node {
        StructuredNode::Heading(h) => generate_heading(h, &ind),
        StructuredNode::Paragraph(p) => generate_paragraph(p, &ind),
        StructuredNode::Notice(n) => generate_notice(n, &ind),
        StructuredNode::Image(img) => generate_image(img, ctx, &ind),
        StructuredNode::Table(t) => generate_table(t, ctx, &ind),
        StructuredNode::Html(h) => generate_html_block(h, &ind),
        StructuredNode::Field(f) => generate_field(f, ctx, &ind),
        StructuredNode::Repeatable(r) => generate_repeatable(r, ctx, indent),
        StructuredNode::Group(g) => generate_group(g, ctx, indent),
        StructuredNode::Conditional(c) => generate_conditional(c, ctx, indent),
        StructuredNode::GridLayout(g) => generate_grid_layout(g, ctx, indent),
        StructuredNode::List(l) => generate_list(l, &ind),
        StructuredNode::Empty => String::new(),
        StructuredNode::Footnote(_) => String::new(),
    }
}

/// Emit an [`crate::structured::HtmlNode`]'s markup verbatim, one language-tagged block each --
/// the same `lang-<code>` convention `generate_translated_text` uses for
/// per-language text. Deliberately NOT escaped: the whole point of the node is
/// that it holds HTML the AEM component renders as HTML.
fn generate_html_block(h: &crate::structured::HtmlNode, ind: &str) -> String {
    let mut langs: Vec<&String> = h.content.keys().collect();
    langs.sort();
    let mut out = String::new();
    for lang in langs {
        out.push_str(&format!(
            "{}<div class=\"lang-{}\" lang=\"{}\">{}</div>\n",
            ind,
            escape_attr(lang),
            escape_attr(lang),
            h.content[lang],
        ));
    }
    out
}

fn generate_list(l: &ListNode, ind: &str) -> String {
    let tag = if l.list_style.is_ordered() {
        "ol"
    } else {
        "ul"
    };
    let style_attr = if l.list_style.needs_css() {
        format!(" style=\"list-style-type: {};\"", l.list_style.css_value())
    } else {
        String::new()
    };
    let mut html = format!(
        "{}<{} class=\"form-list\"{}>
",
        ind, tag, style_attr
    );
    for item in &l.items {
        let item_html = generate_translated_text(&item.content);
        let sub_html = item
            .sublist
            .as_ref()
            .map(|sub| generate_list(sub, &format!("{ind}    ")))
            .unwrap_or_default();
        html.push_str(&format!(
            "{}  <li>{}{}</li>
",
            ind, item_html, sub_html
        ));
    }
    html.push_str(&format!(
        "{}</{}>
",
        ind, tag
    ));
    html
}

fn generate_heading(h: &HeadingNode, ind: &str) -> String {
    let tag = match h.level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    };
    format!(
        "{}<{}>{}</{}>\n",
        ind,
        tag,
        generate_translated_text(&h.content),
        tag
    )
}

fn generate_paragraph(p: &ParagraphNode, ind: &str) -> String {
    format!("{}<p>{}</p>\n", ind, generate_translated_text(&p.content))
}

/// A screen-only notice: shown to whoever fills the form, never printed.
fn generate_notice(n: &NoticeNode, ind: &str) -> String {
    format!(
        "{}<div class=\"screen-only-notice\"><p>{}</p></div>\n",
        ind,
        generate_translated_text(&n.content)
    )
}

fn generate_footnote(f: &FootnoteNode, ind: &str) -> String {
    format!(
        "{}<div class=\"footnote\"><p>{}</p></div>\n",
        ind,
        generate_translated_text(&f.content)
    )
}

fn generate_image(img: &ImageNode, ctx: &mut GeneratorContext, ind: &str) -> String {
    let id = ctx.next_id("img");
    let alt = img
        .alt_text
        .as_ref()
        .map(|s| escape_attr(s))
        .unwrap_or_default();

    // Images are embedded as base64 data URIs
    if !img.content.is_empty() {
        let b64 = base64_encode(&img.content);
        format!(
            "{}<img id=\"{}\" src=\"data:image/png;base64,{}\" alt=\"{}\" class=\"form-image\">\n",
            ind, id, b64, alt
        )
    } else {
        format!(
            "{}<img id=\"{}\" alt=\"{}\" class=\"form-image form-image-placeholder\">\n",
            ind, id, alt
        )
    }
}

fn generate_table(t: &TableNode, ctx: &mut GeneratorContext, ind: &str) -> String {
    let mut html = format!("{}<table class=\"form-table\">\n", ind);

    if let Some(caption) = &t.caption {
        html.push_str(&format!(
            "{}  <caption>{}</caption>\n",
            ind,
            generate_translated_text(caption)
        ));
    }

    if let Some(header) = &t.header {
        html.push_str(&format!("{}  <thead>\n{}    <tr>\n", ind, ind));
        for cell in &header.cells {
            html.push_str(&format!("{}      <th>", ind));
            html.push_str(&generate_node_inline(cell, ctx));
            html.push_str("</th>\n");
        }
        html.push_str(&format!("{}    </tr>\n{}  </thead>\n", ind, ind));
    }

    html.push_str(&format!("{}  <tbody>\n", ind));
    for row in &t.rows {
        html.push_str(&format!("{}    <tr>\n", ind));
        for cell in &row.cells {
            html.push_str(&format!("{}      <td>", ind));
            html.push_str(&generate_node_inline(cell, ctx));
            html.push_str("</td>\n");
        }
        html.push_str(&format!("{}    </tr>\n", ind));
    }
    html.push_str(&format!("{}  </tbody>\n", ind));

    html.push_str(&format!("{}</table>\n", ind));
    html
}

/// Generate node content inline (for table cells)
fn generate_node_inline(node: &StructuredNode, ctx: &mut GeneratorContext) -> String {
    match node {
        StructuredNode::Paragraph(p) => generate_translated_text(&p.content),
        StructuredNode::Field(f) => {
            let field_id = ctx.next_id(&f.name.to_string());
            generate_field_input(f, ctx, &field_id)
        }
        StructuredNode::Group(g) => {
            let mut html = String::new();
            for child in &g.children {
                html.push_str(&generate_node_inline(child, ctx));
            }
            html
        }
        _ => generate_node(node, ctx, 0).trim().to_string(),
    }
}

fn generate_field(f: &FieldNode, ctx: &mut GeneratorContext, ind: &str) -> String {
    let mut html = format!("{}<div class=\"form-field\">\n", ind);

    // Generate a unique ID for this field instance
    let field_id = ctx.next_id(&f.name.to_string());

    // For checkboxes, wrap input and label together (like radio buttons)
    if matches!(f.input_type, FieldType::Bool) {
        if let Some(label) = &f.label {
            let label_text = generate_translated_text(label);
            if !label_text.is_empty() {
                html.push_str(&format!("{}  <label class=\"checkbox-option\">\n", ind));
                html.push_str(&format!("{}    ", ind));
                html.push_str(&generate_field_input(f, ctx, &field_id));
                html.push_str(&format!("\n{}    <span>{}</span>\n", ind, label_text));
                html.push_str(&format!("{}  </label>\n", ind));
            } else {
                // No label text, just render the input
                html.push_str(&format!("{}  ", ind));
                html.push_str(&generate_field_input(f, ctx, &field_id));
                html.push('\n');
            }
        } else {
            // No label at all, just render the input
            html.push_str(&format!("{}  ", ind));
            html.push_str(&generate_field_input(f, ctx, &field_id));
            html.push('\n');
        }
    } else {
        // For non-checkbox fields, use the original approach
        // Generate label if present
        if let Some(label) = &f.label {
            let label_text = generate_translated_text(label);
            if !label_text.is_empty() {
                html.push_str(&format!(
                    "{}  <label for=\"{}\">{}</label>\n",
                    ind,
                    escape_attr(&field_id),
                    label_text
                ));
            }
        }

        // Generate the input element
        html.push_str(&format!("{}  ", ind));
        html.push_str(&generate_field_input(f, ctx, &field_id));
        html.push('\n');
    }

    html.push_str(&format!("{}</div>\n", ind));
    html
}

fn generate_field_input(f: &FieldNode, _ctx: &mut GeneratorContext, field_id: &str) -> String {
    let id = escape_attr(field_id);
    let name = escape_attr(&f.name.to_string());
    let required_attr = if f.required { " required" } else { "" };
    let placeholder = f
        .placeholder
        .as_ref()
        .map(|p| match p {
            crate::structured::TranslatableString::Plain(s) => {
                format!(" placeholder=\"{}\"", escape_attr(s))
            }
            crate::structured::TranslatableString::Translated(map) => {
                // For translated placeholders, generate data attributes for each language
                let mut attrs = String::new();
                for (lang, text) in map {
                    let display_text = text.as_deref().unwrap_or("MISSING TRANSLATION");
                    attrs.push_str(&format!(
                        " data-placeholder-{}=\"{}\"",
                        escape_attr(lang),
                        escape_attr(display_text)
                    ));
                }
                // Use first language as default placeholder
                if let Some((_, text)) = map.iter().next() {
                    let display_text = text.as_deref().unwrap_or("MISSING TRANSLATION");
                    attrs.push_str(&format!(" placeholder=\"{}\"", escape_attr(display_text)));
                }
                attrs
            }
        })
        .unwrap_or_default();

    match &f.input_type {
        FieldType::Text {
            regex,
            max_length,
            min_length,
        } => {
            let mut attrs = format!(
                "<input type=\"text\" id=\"{}\" name=\"{}\"{}",
                id, name, placeholder
            );
            if let Some(pattern) = regex {
                attrs.push_str(&format!(" pattern=\"{}\"", escape_attr(pattern)));
            }
            if let Some(max) = max_length {
                attrs.push_str(&format!(" maxlength=\"{}\"", max));
            }
            if let Some(min) = min_length {
                attrs.push_str(&format!(" minlength=\"{}\"", min));
            }
            if let Some(InputValue::Text(v)) = &f.value {
                attrs.push_str(&format!(" value=\"{}\"", escape_attr(v)));
            }
            attrs.push_str(&format!(" class=\"form-input\"{}>", required_attr));
            attrs
        }

        FieldType::Textarea { max_length } => {
            let mut attrs = format!("<textarea id=\"{}\" name=\"{}\"{}", id, name, placeholder);
            if let Some(max) = max_length {
                attrs.push_str(&format!(" maxlength=\"{}\"", max));
            }
            attrs.push_str(&format!(" class=\"form-input\"{}>", required_attr));
            if let Some(InputValue::Text(v)) = &f.value {
                attrs.push_str(&escape_html(v));
            }
            attrs.push_str("</textarea>");
            attrs
        }

        FieldType::Number { min, max, step } => {
            let mut attrs = format!(
                "<input type=\"number\" id=\"{}\" name=\"{}\"{}",
                id, name, placeholder
            );
            if let Some(m) = min {
                attrs.push_str(&format!(" min=\"{}\"", m));
            }
            if let Some(m) = max {
                attrs.push_str(&format!(" max=\"{}\"", m));
            }
            if let Some(s) = step {
                attrs.push_str(&format!(" step=\"{}\"", s));
            }
            if let Some(InputValue::Number(v)) = &f.value {
                attrs.push_str(&format!(" value=\"{}\"", v));
            }
            attrs.push_str(&format!(" class=\"form-input\"{}>", required_attr));
            attrs
        }

        FieldType::Date => {
            let mut attrs = format!(
                "<input type=\"date\" id=\"{}\" name=\"{}\"{}",
                id, name, placeholder
            );
            if let Some(InputValue::Text(v)) = &f.value {
                attrs.push_str(&format!(" value=\"{}\"", escape_attr(v)));
            }
            attrs.push_str(&format!(" class=\"form-input\"{}>", required_attr));
            attrs
        }

        FieldType::Email => {
            let mut attrs = format!(
                "<input type=\"email\" id=\"{}\" name=\"{}\"{}",
                id, name, placeholder
            );
            if let Some(InputValue::Text(v)) = &f.value {
                attrs.push_str(&format!(" value=\"{}\"", escape_attr(v)));
            }
            attrs.push_str(&format!(" class=\"form-input\"{}>", required_attr));
            attrs
        }

        FieldType::Tel => {
            let mut attrs = format!(
                "<input type=\"tel\" id=\"{}\" name=\"{}\"{}",
                id, name, placeholder
            );
            if let Some(InputValue::Text(v)) = &f.value {
                attrs.push_str(&format!(" value=\"{}\"", escape_attr(v)));
            }
            attrs.push_str(&format!(" class=\"form-input\"{}>", required_attr));
            attrs
        }

        FieldType::Bool => {
            let checked = matches!(&f.value, Some(InputValue::Bool(true)));
            let checked_attr = if checked { " checked" } else { "" };
            format!(
                "<input type=\"checkbox\" id=\"{}\" name=\"{}\" class=\"form-checkbox\"{}{}>",
                id, name, checked_attr, required_attr
            )
        }

        FieldType::Radio { options } => {
            let mut html = format!("<div class=\"radio-group\" data-field=\"{}\">\n", name);
            let selected = f.value.as_ref().and_then(|v| {
                if let InputValue::Text(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }
            });

            for (i, opt) in options.iter().enumerate() {
                let option_id = format!("{}_{}", id, i);
                let opt_value = match &opt.value {
                    InputValue::Text(s) => s.as_str(),
                    _ => match &opt.name {
                        crate::structured::TranslatableString::Plain(s) => s.as_str(),
                        crate::structured::TranslatableString::Translated(map) => {
                            map.values().find_map(|o| o.as_deref()).unwrap_or("")
                        }
                    },
                };
                let checked = selected == Some(opt_value);
                let checked_attr = if checked { " checked" } else { "" };

                // Generate option label with translation support
                let label_html = match &opt.name {
                    crate::structured::TranslatableString::Plain(s) => escape_html(s),
                    crate::structured::TranslatableString::Translated(map) => {
                        let mut spans = String::new();
                        for (lang, text) in map {
                            let display_text = text.as_deref().unwrap_or("MISSING TRANSLATION");
                            spans.push_str(&format!(
                                "<span class=\"lang-{}\" lang=\"{}\">{}</span>",
                                escape_attr(lang),
                                escape_attr(lang),
                                escape_html(display_text)
                            ));
                        }
                        spans
                    }
                };

                html.push_str(&format!(
                    "  <label class=\"radio-option\">\n    <input type=\"radio\" id=\"{}\" name=\"{}\" value=\"{}\" class=\"form-radio\"{}{}>\n    <span>{}</span>\n  </label>\n",
                    escape_attr(&option_id),
                    name,
                    escape_attr(opt_value),
                    checked_attr,
                    required_attr,
                    label_html
                ));
            }
            html.push_str("</div>");
            html
        }

        FieldType::CheckboxGroup { options } => {
            let mut html = format!("<div class=\"checkbox-group\" data-field=\"{}\">\n", name);

            for (i, opt) in options.iter().enumerate() {
                let option_id = format!("{}_{}", id, i);
                let opt_value = match &opt.value {
                    InputValue::Text(s) => s.as_str(),
                    _ => match &opt.name {
                        crate::structured::TranslatableString::Plain(s) => s.as_str(),
                        crate::structured::TranslatableString::Translated(map) => {
                            map.values().find_map(|o| o.as_deref()).unwrap_or("")
                        }
                    },
                };

                let label_html = match &opt.name {
                    crate::structured::TranslatableString::Plain(s) => escape_html(s),
                    crate::structured::TranslatableString::Translated(map) => {
                        let mut spans = String::new();
                        for (lang, text) in map {
                            let display_text = text.as_deref().unwrap_or("MISSING TRANSLATION");
                            spans.push_str(&format!(
                                "<span class=\"lang-{}\" lang=\"{}\">{}</span>",
                                escape_attr(lang),
                                escape_attr(lang),
                                escape_html(display_text)
                            ));
                        }
                        spans
                    }
                };

                html.push_str(&format!(
                    "  <label class=\"checkbox-option\">\n    <input type=\"checkbox\" id=\"{}\" name=\"{}\" value=\"{}\" class=\"form-checkbox\"{}>\n    <span>{}</span>\n  </label>\n",
                    escape_attr(&option_id),
                    name,
                    escape_attr(opt_value),
                    required_attr,
                    label_html
                ));
            }
            html.push_str("</div>");
            html
        }

        FieldType::Select { options } => {
            let mut html = format!(
                "<select id=\"{}\" name=\"{}\" class=\"form-select\"{}>\n",
                id, name, required_attr
            );

            let selected = f.value.as_ref().and_then(|v| {
                if let InputValue::Text(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }
            });

            for opt in options {
                let opt_value = match &opt.value {
                    InputValue::Text(s) => s.as_str(),
                    _ => match &opt.name {
                        crate::structured::TranslatableString::Plain(s) => s.as_str(),
                        crate::structured::TranslatableString::Translated(map) => {
                            map.values().find_map(|o| o.as_deref()).unwrap_or("")
                        }
                    },
                };
                let selected_attr = if selected == Some(opt_value) {
                    " selected"
                } else {
                    ""
                };

                // Generate option text with translation support
                let option_text = match &opt.name {
                    crate::structured::TranslatableString::Plain(s) => escape_html(s),
                    crate::structured::TranslatableString::Translated(map) => {
                        // For select options, use first language as display text
                        // and add data-text-* attributes on the <option> element
                        if let Some(text) = map.values().find_map(|o| o.as_deref()) {
                            escape_html(text)
                        } else {
                            String::new()
                        }
                    }
                };

                // Build data-text attributes for translation switching
                let data_attrs = match &opt.name {
                    crate::structured::TranslatableString::Plain(_) => String::new(),
                    crate::structured::TranslatableString::Translated(map) => {
                        let mut attrs = String::new();
                        for (l, t) in map {
                            let display_text = t.as_deref().unwrap_or("MISSING TRANSLATION");
                            attrs.push_str(&format!(
                                " data-text-{}=\"{}\"",
                                escape_attr(l),
                                escape_attr(display_text)
                            ));
                        }
                        attrs
                    }
                };

                html.push_str(&format!(
                    "  <option value=\"{}\"{}{}>{}</option>\n",
                    escape_attr(opt_value),
                    selected_attr,
                    data_attrs,
                    option_text
                ));
            }
            html.push_str("</select>");
            html
        }
    }
}

fn generate_repeatable(r: &RepeatableNode, ctx: &mut GeneratorContext, indent: usize) -> String {
    let ind = "  ".repeat(indent);
    let container_id = ctx.next_id("repeatable");
    ctx.repeatable_depth += 1;

    let min = r.min_occurrences;
    let max = r.max_occurrences.map(|m| m.to_string()).unwrap_or_default();

    let mut html = format!(
        "{}<div class=\"repeatable-container\" id=\"{}\" data-min=\"{}\" data-max=\"{}\">\n",
        ind, container_id, min, max
    );

    // Template for cloning (hidden)
    html.push_str(&format!(
        "{}  <template class=\"repeatable-template\">\n",
        ind
    ));
    html.push_str(&format!(
        "{}    <div class=\"repeatable-item\" data-index=\"{{INDEX}}\">\n",
        ind
    ));
    html.push_str(&generate_node(&r.item, ctx, indent + 3));
    html.push_str(&format!(
        "{}      <button type=\"button\" class=\"remove-item-btn\" aria-label=\"Remove item\">×</button>\n",
        ind
    ));
    html.push_str(&format!("{}    </div>\n", ind));
    html.push_str(&format!("{}  </template>\n", ind));

    // Items container
    html.push_str(&format!(
        "{}  <div class=\"repeatable-items\"></div>\n",
        ind
    ));

    // Add button
    html.push_str(&format!(
        "{}  <button type=\"button\" class=\"add-item-btn\">+ Add Item</button>\n",
        ind
    ));

    html.push_str(&format!("{}</div>\n", ind));

    ctx.repeatable_depth -= 1;
    html
}

fn generate_group(g: &GroupNode, ctx: &mut GeneratorContext, indent: usize) -> String {
    let ind = "  ".repeat(indent);
    let mut html = format!("{}<div class=\"form-group\">\n", ind);
    for child in &g.children {
        html.push_str(&generate_node(child, ctx, indent + 1));
    }
    html.push_str(&format!("{}</div>\n", ind));
    html
}

fn generate_grid_layout(
    g: &crate::structured::GridLayout,
    ctx: &mut GeneratorContext,
    indent: usize,
) -> String {
    let ind = "  ".repeat(indent);
    let child_ind = "  ".repeat(indent + 1);

    // Generate CSS grid with proportional columns
    let grid_columns = format!("repeat({}, 1fr)", g.columns);
    let mut html = format!(
        "{}<div class=\"grid-layout\" style=\"display: grid; grid-template-columns: {}; gap: 1rem;\">\n",
        ind, grid_columns
    );

    // Generate each grid element with its span
    for element in &g.elements {
        let span_style = if element.span > 1 {
            format!(" style=\"grid-column: span {};\"", element.span)
        } else {
            String::new()
        };

        html.push_str(&format!(
            "{}<div class=\"grid-item\"{}>\n",
            child_ind, span_style
        ));
        html.push_str(&generate_node(&element.node, ctx, indent + 2));
        html.push_str(&format!("{}</div>\n", child_ind));
    }

    html.push_str(&format!("{}</div>\n", ind));
    html
}

fn generate_conditional(c: &ConditionalNode, ctx: &mut GeneratorContext, indent: usize) -> String {
    let ind = "  ".repeat(indent);

    // Skip "default" conditionals (where field_name is the "unknown" sentinel)
    // These represent the initial/default form state before any selections
    // Their content typically duplicates content in other specific conditionals
    if c.condition.field_name == FieldId::from_som_path(&SomPath::new("unknown")) {
        return String::new();
    }

    let condition_attr = encode_condition(&c.condition);
    let condition_id = ctx.next_id("conditional");

    let mut html = format!(
        "{}<div class=\"conditional\" id=\"{}\" data-condition=\"{}\" hidden>\n",
        ind, condition_id, condition_attr
    );
    html.push_str(&generate_node(&c.content, ctx, indent + 1));
    html.push_str(&format!("{}</div>\n", ind));

    html
}

/// Encode a condition as a data attribute value
fn encode_condition(cond: &FieldCondition) -> String {
    let value_str = match &cond.value {
        InputValue::Text(s) => format!("text:{}", s),
        InputValue::Number(n) => format!("number:{}", n),
        InputValue::Bool(b) => format!("bool:{}", b),
    };
    escape_attr(&format!("{}={}", cond.field_name, value_str))
}

fn generate_inline_text(text: &InlineText) -> String {
    let mut html = String::new();
    for node in &text.0 {
        html.push_str(&generate_inline_node(node));
    }
    html
}

fn generate_translated_text(text: &TranslatedText) -> String {
    // Emit all languages with lang-tagged spans
    let mut html = String::new();
    for (lang, inline) in text.iter() {
        let inner = generate_inline_text(inline);
        if inner.is_empty() {
            html.push_str(&format!(
                "<span class=\"lang-{}\" lang=\"{}\">MISSING TRANSLATION</span>",
                escape_attr(lang),
                escape_attr(lang),
            ));
        } else {
            html.push_str(&format!(
                "<span class=\"lang-{}\" lang=\"{}\">{}</span>",
                escape_attr(lang),
                escape_attr(lang),
                inner,
            ));
        }
    }
    html
}

fn generate_inline_node(node: &InlineNode) -> String {
    match node {
        InlineNode::Text(s) => escape_html(s),
        InlineNode::Link(link) => {
            format!(
                "<a href=\"{}\">{}</a>",
                escape_attr(&link.href),
                generate_inline_text(&link.content)
            )
        }
        InlineNode::Strong(inner) => {
            format!("<strong>{}</strong>", generate_inline_node(inner))
        }
        InlineNode::Emphasis(inner) => {
            format!("<em>{}</em>", generate_inline_node(inner))
        }
        InlineNode::Superscript(inner) => {
            format!("<sup>{}</sup>", generate_inline_node(inner))
        }
    }
}

/// Generate embedded CSS styles
fn generate_styles(custom: Option<&HtmlCustomStyles>) -> String {
    let mut css = String::from("  <style>\n");

    // -- @font-face rules (injected first so they're available to all styles) --
    if let Some(custom) = custom {
        for family in &custom.font_faces {
            for variant in &family.variants {
                css.push_str(&format!(
                    "    @font-face {{\n      font-family: '{}';\n      src: url({}) format('truetype');\n      font-weight: {};\n      font-style: {};\n    }}\n\n",
                    family.family, variant.data_uri, variant.weight, variant.style
                ));
            }
        }
    }

    // -- Default styles --
    css.push_str(
        r#"    :root {
      --primary: #2563eb;
      --primary-hover: #1d4ed8;
      --danger: #dc2626;
      --danger-hover: #b91c1c;
      --border: #d1d5db;
      --bg: #f9fafb;
      --text: #111827;
      --text-muted: #6b7280;
    }

    * {
      box-sizing: border-box;
    }

    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
      line-height: 1.6;
      color: var(--text);
      background: var(--bg);
      margin: 0;
    }

    /* Sticky header with logo + language selector */
    .site-header {
      position: sticky;
      top: 0;
      z-index: 100;
      background: #f0f4f8;
      padding: 0.75rem 2rem;
      border-bottom: 1px solid #d1d5db;
      display: flex;
      align-items: center;
      gap: 1rem;
      font-family: system-ui, -apple-system, sans-serif;
      font-size: 0.875rem;
    }

    .site-logo {
      max-height: 2.5rem;
      width: auto;
    }

    .language-selector {
      margin-left: auto;
      display: flex;
      align-items: center;
      gap: 0.5rem;
    }

    .language-selector label {
      font-weight: 600;
      color: #374151;
    }

    .language-selector select {
      padding: 0.25rem 0.5rem;
      border: 1px solid #d1d5db;
      border-radius: 4px;
      background: white;
      font-size: 0.875rem;
      cursor: pointer;
    }

    main {
      max-width: 800px;
      margin: 0 auto;
      padding: 2rem;
    }

    .generated-form {
      background: white;
      padding: 2rem;
      border-radius: 8px;
      box-shadow: 0 1px 3px rgba(0,0,0,0.1);
    }

    /* Language support - by default show all languages */
    span[lang] {
      display: inline;
    }

    /* When a specific language is selected, hide others */
    body.lang-de span[lang]:not(.lang-de),
    body.lang-en span[lang]:not(.lang-en),
    body.lang-fr span[lang]:not(.lang-fr),
    body.lang-it span[lang]:not(.lang-it),
    body.lang-es span[lang]:not(.lang-es) {
      display: none;
    }

    h1, h2, h3, h4, h5, h6 {
      margin: 1.5rem 0 0.75rem;
      line-height: 1.3;
    }

    h1 { font-size: 1.875rem; }
    h2 { font-size: 1.5rem; }
    h3 { font-size: 1.25rem; }
    h4 { font-size: 1.125rem; }
    h5 { font-size: 1rem; }
    h6 { font-size: 0.875rem; }

    p {
      margin: 0.5rem 0;
    }

    .form-field {
      margin-bottom: 1rem;
    }

    .form-field label {
      display: block;
      font-weight: 500;
      margin-bottom: 0.25rem;
      color: var(--text);
    }

    .form-input,
    .form-select {
      width: 100%;
      padding: 0.5rem 0.75rem;
      border: 1px solid var(--border);
      border-radius: 4px;
      font-size: 1rem;
      transition: border-color 0.15s, box-shadow 0.15s;
    }

    .form-input:focus,
    .form-select:focus {
      outline: none;
      border-color: var(--primary);
      box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.1);
    }

    .form-checkbox,
    .form-radio {
      width: 1rem;
      height: 1rem;
      margin-right: 0.5rem;
    }

    .radio-group {
      display: flex;
      flex-direction: column;
      gap: 0.5rem;
    }

    .radio-option,
    .checkbox-option {
      display: flex;
      align-items: center;
      cursor: pointer;
    }

    .radio-option span,
    .checkbox-option span {
      user-select: none;
    }

    .form-group {
      margin-bottom: 1.5rem;
      padding: 1rem;
      border: 1px solid var(--border);
      border-radius: 4px;
      background: var(--bg);
    }

    .form-table {
      width: 100%;
      border-collapse: collapse;
      margin: 1rem 0;
    }

    .form-table th,
    .form-table td {
      padding: 0.5rem;
      border: 1px solid var(--border);
      text-align: left;
    }

    .form-table th {
      background: var(--bg);
      font-weight: 600;
    }

    .form-image {
      max-width: 100%;
      height: auto;
      margin: 1rem 0;
    }

    .form-image-placeholder {
      background: var(--bg);
      min-height: 100px;
      display: flex;
      align-items: center;
      justify-content: center;
    }

    /* Repeatable sections */
    .repeatable-container {
      margin: 1rem 0;
      padding: 1rem;
      border: 2px dashed var(--border);
      border-radius: 8px;
    }

    .repeatable-items {
      display: flex;
      flex-direction: column;
      gap: 1rem;
    }

    .repeatable-item {
      position: relative;
      padding: 1rem;
      padding-right: 2.5rem;
      border: 1px solid var(--border);
      border-radius: 4px;
      background: white;
    }

    .remove-item-btn {
      position: absolute;
      top: 0.5rem;
      right: 0.5rem;
      width: 1.5rem;
      height: 1.5rem;
      padding: 0;
      border: none;
      background: var(--danger);
      color: white;
      border-radius: 50%;
      cursor: pointer;
      font-size: 1rem;
      line-height: 1;
      display: flex;
      align-items: center;
      justify-content: center;
    }

    .remove-item-btn:hover {
      background: var(--danger-hover);
    }

    .remove-item-btn:disabled {
      opacity: 0.5;
      cursor: not-allowed;
    }

    .add-item-btn {
      margin-top: 1rem;
      padding: 0.5rem 1rem;
      border: 1px solid var(--primary);
      background: white;
      color: var(--primary);
      border-radius: 4px;
      cursor: pointer;
      font-size: 0.875rem;
      transition: background 0.15s, color 0.15s;
    }

    .add-item-btn:hover {
      background: var(--primary);
      color: white;
    }

    .add-item-btn:disabled {
      opacity: 0.5;
      cursor: not-allowed;
    }

    /* Conditionals */
    .conditional {
      /* Hidden by default via [hidden] attribute */
    }

    .conditional[data-visible="true"] {
      display: block !important;
    }

    /* Nested repeatables */
    .repeatable-container .repeatable-container {
      border-color: var(--primary);
      background: rgba(37, 99, 235, 0.02);
    }

    .repeatable-container .repeatable-container .repeatable-container {
      border-color: #7c3aed;
      background: rgba(124, 58, 237, 0.02);
    }
"#,
    );

    css.push_str("  </style>\n");

    // -- Custom stylesheet (appended after defaults so it can override) --
    if let Some(custom) = custom {
        if let Some(ref stylesheet) = custom.stylesheet_css {
            css.push_str("  <style>\n");
            // Indent each line for consistent formatting
            for line in stylesheet.lines() {
                css.push_str("    ");
                css.push_str(line);
                css.push('\n');
            }
            css.push_str("  </style>\n");
        }
    }

    css
}

/// Generate embedded JavaScript for dynamic behavior
fn generate_scripts(form_id: &str) -> String {
    format!(
        r#"  <script>
(function() {{
  'use strict';

  const form = document.getElementById('{}');
  if (!form) return;

  // =====================
  // REPEATABLE SECTIONS
  // =====================

  function initRepeatables() {{
    const containers = form.querySelectorAll('.repeatable-container');
    containers.forEach(initRepeatableContainer);
  }}

  function initRepeatableContainer(container) {{
    const template = container.querySelector(':scope > .repeatable-template');
    const itemsContainer = container.querySelector(':scope > .repeatable-items');
    const addBtn = container.querySelector(':scope > .add-item-btn');

    if (!template || !itemsContainer || !addBtn) return;

    const min = parseInt(container.dataset.min) || 0;
    const max = container.dataset.max ? parseInt(container.dataset.max) : Infinity;
    let index = 0;

    function getItemCount() {{
      return itemsContainer.querySelectorAll(':scope > .repeatable-item').length;
    }}

    function updateButtonStates() {{
      const count = getItemCount();
      addBtn.disabled = count >= max;

      // Update remove buttons
      const removeButtons = itemsContainer.querySelectorAll(':scope > .repeatable-item > .remove-item-btn');
      removeButtons.forEach(btn => {{
        btn.disabled = count <= min;
      }});
    }}

    function updateFieldIndices(item, newIndex) {{
      // Update data-index
      item.dataset.index = newIndex;

      // Update name and id attributes with new index
      const fields = item.querySelectorAll('input, select, textarea');
      fields.forEach(field => {{
        if (field.name) {{
          field.name = field.name.replace(/\[\d+\]/, `[${{newIndex}}]`);
        }}
        if (field.id) {{
          field.id = field.id.replace(/_\d+$/, `_${{newIndex}}`);
        }}
      }});

      // Update labels
      const labels = item.querySelectorAll('label');
      labels.forEach(label => {{
        if (label.htmlFor) {{
          label.htmlFor = label.htmlFor.replace(/_\d+$/, `_${{newIndex}}`);
        }}
      }});
    }}

    function reindexItems() {{
      const items = itemsContainer.querySelectorAll(':scope > .repeatable-item');
      items.forEach((item, i) => {{
        updateFieldIndices(item, i);
      }});
      index = items.length;
    }}

    function addItem() {{
      if (getItemCount() >= max) return;

      const content = template.content.cloneNode(true);
      const item = content.querySelector('.repeatable-item');

      // Replace {{{{INDEX}}}} placeholders with actual index
      const html = item.innerHTML.replace(/\{{{{INDEX}}}}/g, index.toString());
      item.innerHTML = html;
      item.dataset.index = index;

      // Update field names/ids to include index
      const fields = item.querySelectorAll('input, select, textarea');
      fields.forEach(field => {{
        if (field.name && !field.name.includes('[')) {{
          field.name = `${{field.name}}[${{index}}]`;
        }}
        if (field.id) {{
          field.id = `${{field.id}}_${{index}}`;
        }}
      }});

      // Update labels
      const labels = item.querySelectorAll('label');
      labels.forEach(label => {{
        if (label.htmlFor) {{
          label.htmlFor = `${{label.htmlFor}}_${{index}}`;
        }}
      }});

      // Add remove handler
      const removeBtn = item.querySelector('.remove-item-btn');
      if (removeBtn) {{
        removeBtn.addEventListener('click', () => removeItem(item));
      }}

      itemsContainer.appendChild(item);
      index++;

      // Initialize nested repeatables
      const nestedContainers = item.querySelectorAll('.repeatable-container');
      nestedContainers.forEach(initRepeatableContainer);

      // Initialize conditionals in the new item
      const nestedConditionals = item.querySelectorAll('.conditional');
      nestedConditionals.forEach(el => evaluateConditional(el));

      updateButtonStates();
      updateConditionals();
    }}

    function removeItem(item) {{
      if (getItemCount() <= min) return;
      item.remove();
      reindexItems();
      updateButtonStates();
      updateConditionals();
    }}

    // Add button handler
    addBtn.addEventListener('click', addItem);

    // Create initial items (minimum required)
    for (let i = 0; i < min; i++) {{
      addItem();
    }}

    updateButtonStates();
  }}

  // =====================
  // CONDITIONAL SECTIONS
  // =====================

  function parseCondition(condStr) {{
    const [fieldPart, valuePart] = condStr.split('=');
    if (!fieldPart || !valuePart) return null;

    const [type, ...rest] = valuePart.split(':');
    const value = rest.join(':');

    return {{
      fieldName: fieldPart,
      type: type,
      value: value
    }};
  }}

    function isFieldVisible(field) {{
        if (!field) return false;
        // Exclude controls inside hidden conditional branches.
        if (field.closest('[hidden]')) return false;
        return field.offsetParent !== null || field.getClientRects().length > 0;
    }}

    function pickField(candidates, fieldName, preferredField) {{
        if (preferredField && preferredField.name === fieldName) {{
            return preferredField;
        }}

        const visibleCandidate = candidates.find(isFieldVisible);
        return visibleCandidate || candidates[0] || null;
    }}

    function getFieldValue(fieldName, preferredField = null) {{
        const escapedName = CSS.escape(fieldName);
        const candidates = Array.from(form.querySelectorAll(`[name="${{escapedName}}"]`));
        if (candidates.length === 0) return null;

        // Radio groups can have many inputs with the same name; prefer checked visible one.
        if (candidates[0].type === 'radio') {{
            const visibleRadios = candidates.filter(isFieldVisible);
            const radioPool = visibleRadios.length > 0 ? visibleRadios : candidates;
            const checked = radioPool.find(r => r.checked);
            return checked ? {{ type: 'radio', value: checked.value }} : null;
        }}

        const field = pickField(candidates, fieldName, preferredField);
        if (!field) return null;

        if (field.type === 'checkbox') {{
            return {{ type: 'checkbox', value: field.checked.toString() }};
        }}

        if (field.tagName === 'SELECT') {{
            const selectedOption = field.options[field.selectedIndex];
            return {{ type: 'select', value: field.value, text: selectedOption ? selectedOption.textContent.trim() : '' }};
        }}

        return {{ type: 'text', value: field.value }};
    }}

  function conditionMatches(condition, fieldValue) {{
    if (!fieldValue) return false;
    if (condition.value === fieldValue.value) return true;
    // For select elements, also match against the display text of the selected option
    if (fieldValue.type === 'select' && fieldValue.text && condition.value === fieldValue.text) return true;
    return false;
  }}

    function evaluateConditional(el, preferredField = null) {{
    const condStr = el.dataset.condition;
    if (!condStr) return;

    const condition = parseCondition(condStr);
    if (!condition) return;

    const fieldValue = getFieldValue(condition.fieldName, preferredField);
    const matches = conditionMatches(condition, fieldValue);

    if (matches) {{
      el.removeAttribute('hidden');
      el.dataset.visible = 'true';
    }} else {{
      el.setAttribute('hidden', '');
      el.dataset.visible = 'false';
    }}
  }}

    function updateConditionals(preferredField = null) {{
    const conditionals = form.querySelectorAll('.conditional');
        conditionals.forEach(el => evaluateConditional(el, preferredField));
  }}

  function initConditionals() {{
    // Initial evaluation
    updateConditionals();

    // Listen for changes on all form fields
        form.addEventListener('change', function(event) {{
            updateConditionals(event.target);
        }});
        form.addEventListener('input', function(event) {{
            updateConditionals(event.target);
        }});
  }}

  // =====================
  // INITIALIZATION
  // =====================

  initRepeatables();
  initConditionals();

  // =====================
  // LANGUAGE SWITCHING
  // =====================

  const langSelect = document.getElementById('language-select');
  if (langSelect) {{
    function switchLanguage(lang) {{
      // Update body class
      document.body.className = document.body.className
        .replace(/\blang-\w+\b/g, '')
        .trim();
      document.body.classList.add('lang-' + lang);

      // Update placeholders with data-placeholder-<lang> attributes
      form.querySelectorAll('[data-placeholder-' + lang + ']').forEach(function(el) {{
        el.placeholder = el.getAttribute('data-placeholder-' + lang);
      }});

      // Update select option text with data-text-<lang> attributes
      form.querySelectorAll('option[data-text-' + lang + ']').forEach(function(el) {{
        el.textContent = el.getAttribute('data-text-' + lang);
      }});
    }}

    langSelect.addEventListener('change', function() {{
      switchLanguage(this.value);
    }});

    // Apply initial language
    switchLanguage(langSelect.value);
  }}

}})();
  </script>
"#,
        escape_attr(form_id)
    )
}

// =====================
// UTILITY FUNCTIONS
// =====================

/// Collect all language codes used in TranslatedText/TranslatableString nodes.
/// Returns a sorted, deduplicated list of language codes.
fn collect_languages(nodes: &[StructuredNode]) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut langs = BTreeSet::new();
    for node in nodes {
        node.collect_languages(&mut langs);
    }
    langs.into_iter().collect()
}

use crate::util::{base64_encode, escape_html};

fn escape_attr(s: &str) -> String {
    escape_html(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured::NameValue;

    #[test]
    fn test_generate_simple_field() {
        use crate::structured::FieldId;
        let field_id: FieldId = "test_field".into();
        let field = FieldNode {
            name: field_id.clone(),
            som_path: None,
            label: Some(TranslatedText::plain("Test Label")),
            input_type: FieldType::Text {
                regex: None,
                max_length: None,
                min_length: None,
            },
            value: None,
            placeholder: Some(crate::structured::TranslatableString::Plain(
                "Enter text".to_string(),
            )),
            required: false,
        };

        let node = StructuredNode::Field(field);
        let mut ctx = GeneratorContext::new();
        let html = generate_node(&node, &mut ctx, 0);

        assert!(html.contains("form-field"));
        assert!(html.contains("Test Label"));
        assert!(html.contains(&field_id.to_string()));
        assert!(html.contains("Enter text"));
    }

    #[test]
    fn test_generate_radio_field() {
        let field = FieldNode {
            name: "choice".into(),
            som_path: None,
            label: Some(TranslatedText::plain("Choose one")),
            input_type: FieldType::Radio {
                options: vec![
                    NameValue {
                        name: crate::structured::TranslatableString::Plain("Option A".to_string()),
                        value: InputValue::Text("Option A".to_string()),
                    },
                    NameValue {
                        name: crate::structured::TranslatableString::Plain("Option B".to_string()),
                        value: InputValue::Text("Option B".to_string()),
                    },
                ],
            },
            value: Some(InputValue::Text("Option A".to_string())),
            placeholder: None,
            required: false,
        };

        let node = StructuredNode::Field(field);
        let mut ctx = GeneratorContext::new();
        let html = generate_node(&node, &mut ctx, 0);

        assert!(html.contains("radio-group"));
        assert!(html.contains("Option A"));
        assert!(html.contains("Option B"));
        assert!(html.contains("checked"));
    }

    #[test]
    fn test_generate_repeatable() {
        let inner = StructuredNode::Field(FieldNode {
            name: "item".into(),
            som_path: None,
            label: Some(TranslatedText::plain("Item")),
            input_type: FieldType::Text {
                regex: None,
                max_length: None,
                min_length: None,
            },
            value: None,
            placeholder: None,
            required: false,
        });

        let repeatable = RepeatableNode {
            item: Box::new(inner),
            min_occurrences: 1,
            max_occurrences: Some(5),
        };

        let node = StructuredNode::Repeatable(repeatable);
        let mut ctx = GeneratorContext::new();
        let html = generate_node(&node, &mut ctx, 0);

        assert!(html.contains("repeatable-container"));
        assert!(html.contains("repeatable-template"));
        assert!(html.contains("data-min=\"1\""));
        assert!(html.contains("data-max=\"5\""));
        assert!(html.contains("add-item-btn"));
    }

    #[test]
    fn test_generate_conditional() {
        use crate::structured::FieldId;

        let content = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Conditional content"),
            som_path: None,
            source_name: None,
        });

        let conditional = ConditionalNode {
            condition: FieldCondition {
                field_name: FieldId::from("toggle"),
                value: InputValue::Bool(true),
            },
            content: Box::new(content),
        };

        let node = StructuredNode::Conditional(conditional);
        let mut ctx = GeneratorContext::new();
        let html = generate_node(&node, &mut ctx, 0);

        assert!(html.contains("conditional"));
        assert!(html.contains("data-condition"));
        assert!(html.contains("hidden"));
        assert!(html.contains("Conditional content"));
    }

    #[test]
    fn test_duplicate_field_names_get_unique_ids() {
        use crate::structured::FieldId;
        let field_id: FieldId = "FullName".into();
        let field_id_str = field_id.to_string();

        // Two fields with the same name should get unique IDs
        let make_field = || FieldNode {
            name: "FullName".into(),
            som_path: None,
            label: Some(TranslatedText::plain("Full Name")),
            input_type: FieldType::Text {
                regex: None,
                max_length: None,
                min_length: None,
            },
            value: None,
            placeholder: None,
            required: false,
        };

        let nodes = vec![
            StructuredNode::Field(make_field()),
            StructuredNode::Field(make_field()),
        ];
        let html = generate_html(&nodes, &HtmlConfig::default());

        // Extract all id="..." values
        let ids: Vec<&str> = html
            .match_indices("id=\"")
            .map(|(start, _)| {
                let rest = &html[start + 4..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();

        // Check there are no duplicates
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(*id), "Duplicate id found: {}", id);
        }

        // The two fields should have distinct IDs (based on the FieldId UUID)
        let field_ids: Vec<&&str> = ids
            .iter()
            .filter(|id| id.starts_with(&field_id_str))
            .collect();
        assert_eq!(
            field_ids.len(),
            2,
            "Expected 2 field IDs starting with {}, got {:?}",
            field_id_str,
            field_ids
        );
        assert_ne!(field_ids[0], field_ids[1], "Field IDs should differ");

        // label for= should match the corresponding input id=
        let id_1 = format!("{}_1", field_id_str);
        let id_2 = format!("{}_2", field_id_str);
        assert!(
            html.contains(&format!("for=\"{}\"", id_1)),
            "label for= should use unique ID"
        );
        assert!(
            html.contains(&format!("id=\"{}\"", id_1)),
            "input id= should use unique ID"
        );
        assert!(
            html.contains(&format!("for=\"{}\"", id_2)),
            "second label for= should use unique ID"
        );
        assert!(
            html.contains(&format!("id=\"{}\"", id_2)),
            "second input id= should use unique ID"
        );

        // name= should stay as the original field name (not suffixed)
        assert!(
            html.contains(&format!("name=\"{}\"", field_id_str)),
            "name= should use the original field name"
        );
    }

    #[test]
    fn test_custom_font_face_embedded() {
        let custom = HtmlCustomStyles {
            stylesheet_css: None,
            logo_data_uri: None,
            font_faces: vec![ResolvedFontFamily {
                family: "TestFont".to_string(),
                variants: vec![
                    ResolvedFontVariant {
                        weight: "normal".to_string(),
                        style: "normal".to_string(),
                        data_uri: "data:font/ttf;base64,AAAA".to_string(),
                    },
                    ResolvedFontVariant {
                        weight: "bold".to_string(),
                        style: "normal".to_string(),
                        data_uri: "data:font/ttf;base64,BBBB".to_string(),
                    },
                ],
            }],
        };
        let config = HtmlConfig {
            custom_styles: Some(custom),
            ..HtmlConfig::default()
        };
        let nodes = vec![StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Hello"),
            som_path: None,
            source_name: None,
        })];
        let html = generate_html(&nodes, &config);

        assert!(
            html.contains("@font-face"),
            "Should contain @font-face rule"
        );
        assert!(
            html.contains("font-family: 'TestFont'"),
            "Should reference TestFont family"
        );
        assert!(
            html.contains("data:font/ttf;base64,AAAA"),
            "Should embed regular variant base64 data"
        );
        assert!(
            html.contains("data:font/ttf;base64,BBBB"),
            "Should embed bold variant base64 data"
        );
        assert!(
            html.contains("font-weight: bold"),
            "Should set font-weight for bold variant"
        );
    }

    #[test]
    fn test_custom_stylesheet_appended() {
        let custom = HtmlCustomStyles {
            stylesheet_css: Some("body { background: red; }".to_string()),
            logo_data_uri: None,
            font_faces: vec![],
        };
        let config = HtmlConfig {
            custom_styles: Some(custom),
            ..HtmlConfig::default()
        };
        let nodes = vec![StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Hello"),
            som_path: None,
            source_name: None,
        })];
        let html = generate_html(&nodes, &config);

        // Default styles should still be present
        assert!(
            html.contains("--primary: #2563eb"),
            "Default CSS variables should be present"
        );
        // Custom stylesheet should appear in a separate <style> block after defaults
        assert!(
            html.contains("body { background: red; }"),
            "Custom stylesheet should be embedded"
        );
        // Custom CSS should come after the default closing </style>
        let default_end = html.find("--primary: #2563eb").unwrap();
        let custom_start = html.find("body { background: red; }").unwrap();
        assert!(
            custom_start > default_end,
            "Custom CSS should appear after default styles"
        );
    }

    #[test]
    fn test_scripts_use_visible_or_event_target_for_duplicate_field_names() {
        let scripts = generate_scripts("test_form");

        assert!(
            scripts.contains("function isFieldVisible(field)"),
            "Generated script should include visibility-aware field filtering"
        );
        assert!(
            scripts.contains("function getFieldValue(fieldName, preferredField = null)"),
            "Generated script should accept a preferred event target field"
        );
        assert!(
            scripts.contains("const visibleCandidate = candidates.find(isFieldVisible);"),
            "Generated script should prefer visible fields when names are duplicated"
        );
        assert!(
            scripts.contains("updateConditionals(event.target);"),
            "Generated script should pass the changed field to conditional updates"
        );
    }

    #[test]
    fn test_logo_in_header() {
        let custom = HtmlCustomStyles {
            stylesheet_css: None,
            logo_data_uri: Some("data:image/png;base64,iVBOR".to_string()),
            font_faces: vec![],
        };
        let config = HtmlConfig {
            custom_styles: Some(custom),
            ..HtmlConfig::default()
        };
        let nodes = vec![StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Hello"),
            som_path: None,
            source_name: None,
        })];
        let html = generate_html(&nodes, &config);

        assert!(
            html.contains("<header class=\"site-header\">"),
            "Should have a sticky header"
        );
        assert!(
            html.contains("site-logo"),
            "Should contain logo img with site-logo class"
        );
        assert!(
            html.contains("data:image/png;base64,iVBOR"),
            "Should embed the logo data URI"
        );
        assert!(html.contains("<main>"), "Should have a <main> element");
    }

    #[test]
    fn test_header_main_structure_without_custom() {
        let config = HtmlConfig::default();
        let nodes = vec![StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Hello"),
            som_path: None,
            source_name: None,
        })];
        let html = generate_html(&nodes, &config);

        // Without multilingual content or logo, no header should be emitted
        assert!(
            !html.contains("<header"),
            "No header without logo or multiple languages"
        );
        // Main should still wrap the form
        assert!(html.contains("<main>"), "Should have <main> element");
        assert!(html.contains("</main>"), "Should close <main>");
    }
}
