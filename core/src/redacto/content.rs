//! Rendering of a single structured block to a Redacto text-asset body.
//!
//! Asset content is the HTML fragment the Redacto authoring UI (Quill) round-
//! trips and the renderer injects verbatim via Thymeleaf `th:utext`.

use crate::structured::{
    FootnoteNode, NoticeNode, HeadingNode, ListNode, ParagraphNode, QUILL_TAGS, StructuredNode, TableNode,
    TranslatedText, inline_text_to_html_with, render_plain_list_html, render_table_html,
    strip_footnote_marker,
};

/// The empty-paragraph spacer Redacto documents use for vertical whitespace.
pub(super) const SPACER: &str = "<p><br></p>";

/// Footnote markers, in document order, used to turn `<sup>N</sup>` in a body
/// block into a link to the footnote asset.
pub(super) type FootnoteMarkers = [String];

/// Render one structured block as a Redacto text-asset body.
///
/// Returns `None` for nodes that carry no content of their own (containers,
/// input fields, images) — the caller decides how to handle those.
pub fn render_block_html(
    node: &StructuredNode,
    language: &str,
    markers: &FootnoteMarkers,
) -> Option<String> {
    match node {
        StructuredNode::Heading(h) => Some(render_heading(h, language, markers)),
        StructuredNode::Paragraph(p) => Some(render_paragraph(p, language, markers)),
        StructuredNode::List(l) => Some(render_list(l, language, markers)),
        StructuredNode::Table(t) => Some(render_table(t, language, markers)),
        // Already markup -- pass it through rather than re-render it.
        StructuredNode::Html(h) => Some(h.markup_in(language).to_string()),
        StructuredNode::Footnote(f) => Some(render_footnote(f, language)),
        StructuredNode::Notice(n) => Some(render_notice(n, language, markers)),
        _ => None,
    }
}

/// Shared plain-text-to-paragraphs core: one `<p>` per line, HTML-escaped,
/// with interior blank lines kept as the spacer paragraph and leading /
/// trailing blank lines dropped. Returns `""` for a value with no content.
fn paragraphs_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .into_iter()
        .skip_while(|l| l.is_empty())
        .collect();
    let lines = match lines.iter().rposition(|l| !l.is_empty()) {
        Some(last) => &lines[..=last],
        None => &[][..],
    };

    let mut out = String::new();
    for line in lines {
        if line.is_empty() {
            out.push_str(SPACER);
        } else {
            out.push_str("<p>");
            out.push_str(&crate::util::escape_html(line));
            out.push_str("</p>");
        }
    }
    out
}

/// Render plain (possibly multi-line) header text as a Redacto text-asset
/// body: one `<p>` per line, HTML-escaped, wrapped in the `.right
/// .preserve-spaces` furniture wrapper the Redacto stylesheet expects.
///
/// Legacy v1 got this wrapper for free from Java's
/// `HtmlDocumentService.renderLegacyFurniture`; v2's authored-HTML header must
/// add it explicitly or the text overlaps the page logo instead of floating
/// clear of it. `.right` floats the block right; `.preserve-spaces` sets
/// `white-space: pre-wrap`, which — being CSS-inherited — covers every `<p>`
/// child from this one outer wrapper, so whitespace in the value needs no
/// special encoding.
///
/// The value is treated strictly as plain text: markup in it is escaped and
/// shows literally. Returns `""` for a value with no content (no wrapper
/// around nothing).
pub(super) fn render_header_html(text: &str) -> String {
    let inner = paragraphs_html(text);
    if inner.is_empty() {
        return String::new();
    }
    format!(r#"<div class="right preserve-spaces">{inner}</div>"#)
}

/// Legacy page-number counter, appended after the footer's field spans for
/// parity with the counter `HtmlDocumentService.renderLegacyFurniture` added
/// automatically to every v1 footer. Redacto's client-side pagination fills
/// in `.page-number`/`.page-count` from the page's `counter(page)`/
/// `counter(pages)`.
const PAGE_COUNTER: &str = "<span class=\"right\">Page <span class=\"page-number\"></span>\
    /<span class=\"page-count\"></span></span>";

/// Render the page footer's per-field spans plus the trailing page-number
/// counter.
///
/// Each field with a non-blank rendered value becomes its own
/// `<span class="{class}">value</span>`, separated from its neighbours by one
/// literal space; a field whose value is blank for this language is skipped
/// entirely rather than emitting an empty span. The value is HTML-escaped
/// like any other authored text; the class name is profile-controlled, not
/// escaped. The non-empty group of field spans is wrapped in
/// `<span class="redacto-reading-order">` so Core clones it into the tagged
/// reading order for accessibility — the page counter sits outside that
/// wrapper, matching the platform's own furniture examples, since a page
/// number is not meaningful reading-order content. The counter itself is
/// always appended, even when every field is blank, since pagination must
/// keep working on a document whose UBS footer text happens to be empty.
pub(super) fn render_footer_html(fields: &[super::FooterField]) -> String {
    let spans: Vec<String> = fields
        .iter()
        .filter(|f| !f.value.trim().is_empty())
        .map(|f| {
            format!(
                "<span class=\"{}\">{}</span>",
                f.class,
                crate::util::escape_html(&f.value)
            )
        })
        .collect();
    let joined = spans.join(" ");
    if joined.is_empty() {
        PAGE_COUNTER.to_string()
    } else {
        format!(r#"<span class="redacto-reading-order">{joined}</span>{PAGE_COUNTER}"#)
    }
}

/// Render inline content and link any footnote references it contains.
fn inline(text: &TranslatedText, language: &str, markers: &FootnoteMarkers) -> String {
    let html = inline_text_to_html_with(text, language, QUILL_TAGS);
    link_footnote_references(&html, markers)
}

fn render_heading(h: &HeadingNode, language: &str, markers: &FootnoteMarkers) -> String {
    let level = h.level.as_u8();
    format!(
        "<h{level}>{}</h{level}>",
        inline(&h.content, language, markers)
    )
}

/// A screen-only notice. Redacto renders the document a person reads, so the
/// notice belongs in it; the class is what keeps it distinguishable.
fn render_notice(n: &NoticeNode, language: &str, markers: &FootnoteMarkers) -> String {
    let body = inline(&n.content, language, markers);
    if body.trim().is_empty() {
        SPACER.to_string()
    } else {
        format!("<p class=\"screen-only-notice\">{body}</p>")
    }
}

fn render_paragraph(p: &ParagraphNode, language: &str, markers: &FootnoteMarkers) -> String {
    let body = inline(&p.content, language, markers);
    if body.trim().is_empty() {
        SPACER.to_string()
    } else {
        format!("<p>{body}</p>")
    }
}

fn render_list(list: &ListNode, language: &str, markers: &FootnoteMarkers) -> String {
    render_plain_list_html(list, language, &|t, l| inline(t, l, markers))
}

fn render_table(table: &TableNode, language: &str, markers: &FootnoteMarkers) -> String {
    render_table_html(table, language, &|t, l| inline(t, l, markers))
}

fn render_footnote(f: &FootnoteNode, language: &str) -> String {
    // Footnote bodies never link other footnotes, so no marker substitution.
    let body = inline_text_to_html_with(&f.content, language, QUILL_TAGS);
    match &f.marker {
        Some(marker) => format!(
            "<p id=\"footnote-{marker}\"><sup>{marker}</sup> {}</p>",
            strip_footnote_marker(&body, marker)
        ),
        None => format!("<p>{body}</p>"),
    }
}

/// Turn every `<sup>MARKER</sup>` occurrence into a link to the corresponding
/// footnote asset, mirroring the anchors the Redacto stylesheet expects.
fn link_footnote_references(html: &str, markers: &FootnoteMarkers) -> String {
    let mut result = html.to_string();
    for marker in markers {
        let pattern = format!("<sup>{marker}</sup>");
        if result.contains(&pattern) {
            let replacement = format!("<sup><a href=\"#footnote-{marker}\">{marker}</a></sup>");
            result = result.replace(&pattern, &replacement);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacto::FooterField;

    fn field(class: &str, value: &str) -> FooterField {
        FooterField {
            class: class.to_string(),
            value: value.to_string(),
        }
    }

    // ── Tables ───────────────────────────────────────────────────────────

    /// The exact table markup, pinned because the renderer is now shared with
    /// the AEM HTML component ([`crate::structured::render_table_html`]) and the
    /// two targets differ only in the inline vocabulary: Redacto emits Quill's
    /// `<strong>`/`<em>`, AEM emits `<b>`/`<i>`. A caption, a header row, a
    /// body row and a list cell in one go.
    #[test]
    fn a_table_renders_as_quill_table_markup() {
        use crate::structured::{
            InlineNode, InlineText, ListItem, ListNode, ParagraphNode, StructuredNode,
            TableHeader, TableNode, TableRow, TranslatedText,
        };

        fn cell(text: &str) -> StructuredNode {
            StructuredNode::Paragraph(ParagraphNode {
                content: TranslatedText::plain_with_lang("en", text),
                som_path: None,
                source_name: None,
            })
        }

        let bold = TranslatedText::single(
            "en".to_string(),
            InlineText(vec![InlineNode::Strong(Box::new(InlineNode::Text(
                "Total".into(),
            )))]),
        );
        let table = TableNode {
            caption: Some(TranslatedText::plain_with_lang("en", "Plans")),
            header: Some(TableHeader {
                cells: vec![cell("Plan"), cell("Share")],
            }),
            rows: vec![
                TableRow {
                    cells: vec![
                        cell("A"),
                        StructuredNode::List(ListNode {
                            list_style: crate::document::ListStyleType::Disc,
                            items: vec![ListItem {
                                content: TranslatedText::plain_with_lang("en", "50%"),
                                sublist: None,
                            }],
                        }),
                    ],
                },
                TableRow {
                    cells: vec![
                        StructuredNode::Paragraph(ParagraphNode {
                            content: bold,
                            som_path: None,
                            source_name: None,
                        }),
                        cell("100%"),
                    ],
                },
            ],
        };

        let html = render_block_html(&StructuredNode::Table(table), "en", &[])
            .expect("a table is content of its own");

        assert_eq!(
            html,
            concat!(
                "<table><caption>Plans</caption>",
                "<thead><tr><th>Plan</th><th>Share</th></tr></thead>",
                "<tbody>",
                "<tr><td>A</td><td><ul><li>50%</li></ul></td></tr>",
                "<tr><td><strong>Total</strong></td><td>100%</td></tr>",
                "</tbody></table>"
            )
        );
    }

    // ── Header ───────────────────────────────────────────────────────────

    /// The page header stacks its lines on the page (a validity line above the
    /// legal entity), and v2 furniture assets can honour that — the whole point
    /// of the migration away from the flattened single line. The whole block
    /// floats right, clear of the page logo.
    #[test]
    fn a_multi_line_header_becomes_one_paragraph_per_line_inside_the_right_wrapper() {
        assert_eq!(
            render_header_html("Gültig ab 02.01.2018\nUBS Europe SE"),
            r#"<div class="right preserve-spaces"><p>Gültig ab 02.01.2018</p><p>UBS Europe SE</p></div>"#
        );
    }

    /// The value is content, not markup: a template that emits a tag must not
    /// be able to inject it into the page.
    #[test]
    fn markup_in_the_header_value_is_escaped() {
        assert_eq!(
            render_header_html("<b>UBS</b> & Co."),
            r#"<div class="right preserve-spaces"><p>&lt;b&gt;UBS&lt;/b&gt; &amp; Co.</p></div>"#
        );
    }

    /// `.preserve-spaces` (`white-space: pre-wrap`) is CSS-inherited from the
    /// outer wrapper, so space runs need no special encoding — real spaces
    /// pass through untouched.
    #[test]
    fn header_space_runs_are_left_as_plain_spaces_for_the_stylesheet_to_preserve() {
        assert_eq!(
            render_header_html("66300    EN"),
            r#"<div class="right preserve-spaces"><p>66300    EN</p></div>"#
        );
    }

    /// A single space between words must stay an ordinary, wrappable space.
    #[test]
    fn single_spaces_in_the_header_are_left_alone() {
        assert_eq!(
            render_header_html("UBS Europe SE"),
            r#"<div class="right preserve-spaces"><p>UBS Europe SE</p></div>"#
        );
    }

    /// Blank lines inside the value are vertical space the source drew; blank
    /// lines around it are not.
    #[test]
    fn interior_blank_lines_in_the_header_become_spacers_and_outer_ones_are_dropped() {
        assert_eq!(
            render_header_html("\n\nfirst\n\nsecond\n\n"),
            format!(r#"<div class="right preserve-spaces"><p>first</p>{SPACER}<p>second</p></div>"#)
        );
    }

    #[test]
    fn an_empty_header_value_renders_nothing() {
        assert_eq!(render_header_html("   \n  "), "");
    }

    // ── Footer ───────────────────────────────────────────────────────────

    /// Each field becomes its own classed span, separated by one literal
    /// space, wrapped for accessibility, with the page counter after.
    #[test]
    fn footer_fields_become_one_span_each_separated_by_a_literal_space() {
        assert_eq!(
            render_footer_html(&[
                field("footer-form-id", "66300"),
                field("footer-language", "EN"),
            ]),
            format!(
                r#"<span class="redacto-reading-order"><span class="footer-form-id">66300</span> <span class="footer-language">EN</span></span>{PAGE_COUNTER}"#
            )
        );
    }

    /// A field genuinely blank for this language must not print an empty
    /// `<span>` — it is simply omitted from the joined group.
    #[test]
    fn a_blank_footer_field_is_skipped_entirely() {
        assert_eq!(
            render_footer_html(&[
                field("footer-form-id", "66300"),
                field("footer-man-code", ""),
            ]),
            format!(
                r#"<span class="redacto-reading-order"><span class="footer-form-id">66300</span></span>{PAGE_COUNTER}"#
            )
        );
    }

    /// Pagination must keep working even when every field is blank — no
    /// `redacto-reading-order` wrapper around nothing, but the counter still
    /// appears.
    #[test]
    fn the_page_counter_is_appended_even_when_every_field_is_blank() {
        assert_eq!(
            render_footer_html(&[field("footer-form-id", "")]),
            PAGE_COUNTER
        );
        assert_eq!(render_footer_html(&[]), PAGE_COUNTER);
    }

    /// A field's value is content, not markup, exactly like the header.
    #[test]
    fn a_footer_field_value_is_html_escaped() {
        assert_eq!(
            render_footer_html(&[field("footer-form-code", "<b>AAEV</b> & Co.")]),
            format!(
                r#"<span class="redacto-reading-order"><span class="footer-form-code">&lt;b&gt;AAEV&lt;/b&gt; &amp; Co.</span></span>{PAGE_COUNTER}"#
            )
        );
    }
}
