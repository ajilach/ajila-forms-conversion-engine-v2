//! Shape corrections to the TEXT of the static texts, applied to the
//! multilingual tree before it is split into master texts and the dictionary.
//!
//! These differ from [`super::normalize`] in one respect that decides where they
//! live: they change a text, and the dictionary is keyed by the master text. A
//! pass over the single-language tree would rewrite the master and leave every
//! translation keyed by the old one, so these run on [`AemNodeTranslated`],
//! where each language is rewritten together and the master and its messages
//! stay one entry.
//!
//! - [`paragraph`] -- a static text's value is a paragraph. The UBS platform
//!   marks a title draw as a heading by setting `role="heading"` on its first
//!   `<p>` (`accessibility.js`), and the DoR renderer prints only such a row as a
//!   bold `<hN class="staticTitle">`. A bare-text value has no `<p>`, so its
//!   heading prints as body text. The retired engine wrapped every heading this
//!   way; an authored document does not, so the writer does.
//! - [`link_addresses`] -- PROBLEM-static-text-links. A web or e-mail address a
//!   static text only prints is made a link, in the corpus's own shape.
//!
//! Both are idempotent.

use std::sync::LazyLock;

use super::AemI18nText;
use super::translated::AemNodeTranslated;

/// Apply every text correction to the static texts of `form`, in every language.
pub fn shape_texts(form: &mut AemNodeTranslated) {
    form.visit_mut(&mut |node| match node {
        AemNodeTranslated::TextDraw { content, .. } => reshape(content, paragraph),
        AemNodeTranslated::TitleDraw { content, .. } => reshape(content, title_paragraph),
        _ => {}
    });
}

fn reshape(text: &mut AemI18nText, shape: fn(&str) -> String) {
    for value in text.0.values_mut() {
        *value = link_addresses(&shape(value));
    }
}

// ── a static text is a paragraph ────────────────────────────────────────────

/// The block elements a static text's value may already be made of. A value
/// holding any of them is left as it is: it already carries its own structure.
const BLOCKS: &[&str] = &[
    "p", "ul", "ol", "li", "table", "div", "blockquote", "h1", "h2", "h3", "h4", "h5", "h6",
];

/// `value` as a paragraph: a value with no block element of its own is wrapped
/// in `<p>`, any other is returned unchanged, and so is an empty one.
pub fn paragraph(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || has_block(trimmed) {
        return value.to_string();
    }
    format!("<p>{trimmed}</p>")
}

/// A heading's value as the paragraph the platform marks as the heading: bare
/// text is wrapped, a single other block around it (`<h3>`, `<div>`) becomes
/// the paragraph, and a value that already opens with `<p>` is left alone. A
/// list or table cannot be a heading and is left for the package check to
/// report.
pub fn title_paragraph(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || is_paragraph(trimmed) {
        return value.to_string();
    }
    if let Some(inner) = single_block(trimmed, &["div", "blockquote", "h1", "h2", "h3", "h4", "h5", "h6"]) {
        return format!("<p>{}</p>", inner.trim());
    }
    paragraph(value)
}

/// Whether `html` opens with a `<p>`, which is what the platform marks as a
/// heading (`role="heading"` on a title draw's first paragraph).
pub fn is_paragraph(html: &str) -> bool {
    html.trim_start()
        .strip_prefix('<')
        .and_then(|rest| rest.split_once('>'))
        .is_some_and(|(tag, _)| tag_name(tag).eq_ignore_ascii_case("p") && !tag.starts_with('/'))
}

/// The inside of `html` when it is exactly one element named in `names`:
/// `Key features` for `<h3>Key features</h3>`.
fn single_block<'a>(html: &'a str, names: &[&str]) -> Option<&'a str> {
    let (open, rest) = html.strip_prefix('<')?.split_once('>')?;
    let name = tag_name(open);
    if !names.iter().any(|n| name.eq_ignore_ascii_case(n)) {
        return None;
    }
    let close = format!("</{name}>");
    let inner = rest.strip_suffix(close.as_str())?;
    // Another element of the same name inside would make the outer one not the only block.
    (!inner.to_ascii_lowercase().contains(&format!("<{}", name.to_ascii_lowercase()))).then_some(inner)
}

fn has_block(html: &str) -> bool {
    tags(html).any(|tag| {
        let name = tag_name(tag);
        BLOCKS.iter().any(|b| name.eq_ignore_ascii_case(b))
    })
}

/// Every tag in `html`, as the text between `<` and `>`.
fn tags(html: &str) -> impl Iterator<Item = &str> {
    html.split('<').skip(1).filter_map(|rest| rest.split_once('>').map(|(tag, _)| tag))
}

/// A tag's element name, without a closing slash: `p` for `p class="x"`,
/// `a` for `/a`.
fn tag_name(tag: &str) -> &str {
    let tag = tag.strip_prefix('/').unwrap_or(tag);
    let end = tag
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(tag.len());
    &tag[..end]
}

// ── PROBLEM-static-text-links ───────────────────────────────────────────────

/// Trailing punctuation belongs to the sentence, not to the address.
const TRAIL: &[char] = &['.', ',', ':', ';', '!', '?', '»', '”'];

/// How long an address's opening has to be before finding it again counts as
/// the link text having been concatenated onto its target.
const HEAD: usize = 8;

/// `html` with every bare web and e-mail address in its text made a link that
/// opens in a new tab. An address already inside an `<a>`, or in a tag's
/// attribute, is not touched, which makes this idempotent. An address written
/// twice over (the source's link text run onto its target) and one carrying a
/// query string are left as they are: linking them would build an href that goes
/// nowhere. Same rules as the feedback sweep `static_text_links.py`.
pub fn link_addresses(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_anchor = false;
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let Some(close) = rest.find('>') else {
                    // A `<` that opens no tag is text.
                    out.push_str(&text_run(rest, in_anchor));
                    break;
                };
                let tag = &rest[1..close];
                if tag_name(tag).eq_ignore_ascii_case("a") {
                    in_anchor = !tag.starts_with('/');
                }
                out.push_str(&rest[..=close]);
                rest = &rest[close + 1..];
            }
            found => {
                let end = found.unwrap_or(rest.len());
                out.push_str(&text_run(&rest[..end], in_anchor));
                rest = &rest[end..];
            }
        }
    }
    out
}

/// A run of text, linked unless it is already a link's text.
fn text_run(text: &str, in_anchor: bool) -> String {
    if in_anchor { text.to_string() } else { link_text(text) }
}

/// The kind of address a match is, which decides its href.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Url,
    Mail,
}

/// One text run (no markup) with its addresses linked.
fn link_text(text: &str) -> String {
    let mut found: Vec<(usize, usize, Kind)> = Vec::new();
    // Every URL's span, linked or not: an e-mail inside a URL is part of it.
    let mut claimed: Vec<(usize, usize)> = Vec::new();
    for (kind, start, end) in candidates(text) {
        if claimed.iter().any(|&(a, b)| start < b && a < end) {
            continue;
        }
        if kind == Kind::Url {
            claimed.push((start, end));
        }
        let address = trim(&text[start..end]);
        if address.is_empty() || suspect(address, &text[start + address.len()..]) {
            continue;
        }
        found.push((start, start + address.len(), kind));
    }
    found.sort_by_key(|&(start, ..)| start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, kind) in found {
        out.push_str(&text[at..start]);
        out.push_str(&anchor(&text[start..end], kind));
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// A web address: it ends at markup, a quote, an entity or query `&`, and at any
/// space, the Unicode ones included (regex-lite's `\s` is ASCII only).
static URL: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(
        r#"(?i)(?:https?://|www\.)[^\s\x{a0}\x{2000}-\x{200b}\x{202f}\x{205f}\x{3000}<>"'&;]+"#,
    )
    .expect("a valid pattern")
});

static MAIL: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"[\w.%+-]+@[\w-]+(?:\.[\w-]+)+").expect("a valid pattern"));

/// An entity that stands for text (`&nbsp;`, `&#160;`), which ends an address
/// like a space does. Any other `&` after an address starts a query string.
static TEXT_ENTITY: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"^&(?:[a-zA-Z][a-zA-Z0-9]*|#[0-9]+|#x[0-9a-fA-F]+);").expect("a valid pattern"));

/// Every URL, then every e-mail address, in `text`, as (kind, start, end).
fn candidates(text: &str) -> Vec<(Kind, usize, usize)> {
    let (url, mail) = (&*URL, &*MAIL);
    let preceded_by = |at: usize, set: &dyn Fn(char) -> bool| {
        text[..at].chars().next_back().is_some_and(set)
    };
    let mut out = Vec::new();
    for m in url.find_iter(text) {
        if !preceded_by(m.start(), &|c| c.is_alphanumeric() || "_@./-".contains(c)) {
            out.push((Kind::Url, m.start(), m.end()));
        }
    }
    for m in mail.find_iter(text) {
        if !preceded_by(m.start(), &|c| c.is_alphanumeric() || "_.%+-".contains(c)) {
            out.push((Kind::Mail, m.start(), m.end()));
        }
    }
    out
}

/// `address` without trailing sentence punctuation or an unbalanced `)`.
fn trim(mut address: &str) -> &str {
    loop {
        let before = address;
        address = address.trim_end_matches(TRAIL);
        if address.ends_with(')') && address.matches(')').count() > address.matches('(').count() {
            address = &address[..address.len() - 1];
        }
        if address == before {
            return address;
        }
    }
}

/// Whether `address` must not be linked: it is written twice over, or a query
/// string (an `&` right after it that is not a text entity) cut it short.
fn suspect(address: &str, after: &str) -> bool {
    let low = address.to_lowercase();
    low.matches("www.").count() > 1
        || low.matches("http").count() > 1
        || low.matches('@').count() > 1
        || (address.len() > 2 * HEAD
            && address.is_char_boundary(HEAD)
            && address[1..].contains(&address[..HEAD]))
        || (after.starts_with('&') && (after.starts_with("&amp;") || !TEXT_ENTITY.is_match(after)))
}

fn anchor(address: &str, kind: Kind) -> String {
    let href = match kind {
        Kind::Mail => format!("mailto:{address}"),
        Kind::Url if address.to_lowercase().starts_with("www.") => format!("https://{address}"),
        Kind::Url => address.to_string(),
    };
    format!(r#"<a href="{href}" target="_blank">{address}</a>"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_text_becomes_a_paragraph() {
        assert_eq!(paragraph("1. General information"), "<p>1. General information</p>");
        assert_eq!(paragraph("  Key features "), "<p>Key features</p>");
        assert_eq!(paragraph("<b>Prices</b>"), "<p><b>Prices</b></p>");
        assert_eq!(paragraph("a<br>b"), "<p>a<br>b</p>");
    }

    #[test]
    fn a_value_with_its_own_blocks_is_left_alone() {
        for value in [
            "<p>Key features</p>",
            "<p class=\"x\">a</p><p>b</p>",
            "<ul><li>a</li></ul>",
            "<ol start=\"5\"><li>a</li></ol>",
            "<table><tr><td>a</td></tr></table>",
            "",
            "   ",
        ] {
            assert_eq!(paragraph(value), value, "{value:?}");
        }
    }

    #[test]
    fn a_bare_web_address_is_linked() {
        assert_eq!(
            link_addresses("<p>(website: www.bafin.de)</p>"),
            r#"<p>(website: <a href="https://www.bafin.de" target="_blank">www.bafin.de</a>)</p>"#
        );
        assert_eq!(
            link_addresses("<p>online at http://www.ubs.com/de/de/wealthmanagement.html.</p>"),
            "<p>online at <a href=\"http://www.ubs.com/de/de/wealthmanagement.html\" \
             target=\"_blank\">http://www.ubs.com/de/de/wealthmanagement.html</a>.</p>"
        );
    }

    #[test]
    fn a_bare_e_mail_address_is_linked() {
        assert_eq!(
            link_addresses("<p>Email: sh-ubs-pb-ger@ubs.com<br>Website</p>"),
            "<p>Email: <a href=\"mailto:sh-ubs-pb-ger@ubs.com\" \
             target=\"_blank\">sh-ubs-pb-ger@ubs.com</a><br>Website</p>"
        );
    }

    #[test]
    fn trailing_sentence_punctuation_stays_outside_the_link() {
        assert_eq!(
            link_addresses("<p>See www.bankenverband.de.</p>"),
            "<p>See <a href=\"https://www.bankenverband.de\" \
             target=\"_blank\">www.bankenverband.de</a>.</p>"
        );
    }

    #[test]
    fn an_existing_link_and_an_attribute_are_not_touched() {
        for value in [
            r#"<p>Write to <a href="mailto:a@ubs.com">a@ubs.com</a></p>"#,
            r#"<p><a href="https://www.ubs.com" target="_blank">www.ubs.com</a></p>"#,
            r#"<p title="www.ubs.com">x</p>"#,
        ] {
            assert_eq!(link_addresses(value), value, "{value:?}");
        }
    }

    #[test]
    fn an_address_written_twice_over_is_not_linked() {
        for value in [
            "<p>www.conciliatorebancario.itwww.conciliatorebancario.it</p>",
            "<p>vera.martocchi@ubs.comvera.martocchi</p>",
            "<p>https://x.example/?a=1&amp;b=2</p>",
        ] {
            assert!(!link_addresses(value).contains("<a "), "{value:?}");
        }
    }

    /// A `<` that opens no tag is text, even before a character of several bytes.
    #[test]
    fn a_stray_angle_bracket_is_text() {
        for value in ["<p>Hinweis</p>Betrag < 5 €", "<ä", "a <"] {
            assert_eq!(link_addresses(value), value, "{value:?}");
        }
    }

    /// An address ends at any space, a non-breaking one included: XFA texts
    /// carry U+00A0 as a character.
    #[test]
    fn an_address_ends_at_a_non_breaking_space() {
        assert_eq!(
            link_addresses("<p>See www.ubs.com\u{a0}oder mehr</p>"),
            "<p>See <a href=\"https://www.ubs.com\" target=\"_blank\">www.ubs.com</a>\u{a0}oder mehr</p>"
        );
    }

    /// A query string cuts the match short whether its `&` is escaped or not,
    /// and an e-mail inside such a URL is not linked on its own.
    #[test]
    fn a_url_with_a_query_string_is_left_whole() {
        for value in [
            "<p>see www.x.com/?a=1&b=2</p>",
            "<p>https://x.example/?to=a@b.com&amp;x=1</p>",
        ] {
            assert!(!link_addresses(value).contains("<a "), "{value:?}");
        }
        // An entity that is text, not a query, still ends the address as a space does.
        assert!(link_addresses("<p>www.ubs.com&nbsp;mehr</p>").contains("href=\"https://www.ubs.com\""));
    }

    /// A heading is marked by its `<p>`, so a title draw written as another
    /// single block becomes that paragraph.
    #[test]
    fn a_title_written_as_another_block_becomes_a_paragraph() {
        assert_eq!(title_paragraph("<h3>Key features</h3>"), "<p>Key features</p>");
        assert_eq!(title_paragraph("<div class=\"x\"><b>A</b></div>"), "<p><b>A</b></p>");
        assert_eq!(title_paragraph("Key features"), "<p>Key features</p>");
        assert_eq!(title_paragraph("<p>Key features</p>"), "<p>Key features</p>");
        assert!(is_paragraph(&title_paragraph("<h3>A</h3>")));
    }

    #[test]
    fn shaping_is_idempotent() {
        let raw = "Email: a@ubs.com, www.ubs.com.";
        let once = link_addresses(&paragraph(raw));
        assert_eq!(link_addresses(&paragraph(&once)), once);
    }

    #[test]
    fn every_language_of_a_draw_is_shaped() {
        let json = serde_json::json!({
            "type": "Root", "title": {"en": "Form"},
            "children": [{
                "type": "TitleDraw", "uuid": "00000000-0000-0000-0000-000000000001",
                "name": "TTL_Heading", "heading_level": 3,
                "content": {"en": "Contact", "de": "Kontakt", "es": "<p>Contacto</p>"},
                "colspan": 12, "dor_colspan": null
            }]
        });
        let mut form: AemNodeTranslated = serde_json::from_value(json).unwrap();
        shape_texts(&mut form);
        let AemNodeTranslated::Root { children, .. } = &form else { unreachable!() };
        let AemNodeTranslated::TitleDraw { content, .. } = &children[0] else { unreachable!() };
        assert_eq!(content.get("en"), Some("<p>Contact</p>"));
        assert_eq!(content.get("de"), Some("<p>Kontakt</p>"));
        assert_eq!(content.get("es"), Some("<p>Contacto</p>"));
    }
}
