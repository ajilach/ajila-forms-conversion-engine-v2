//! Source coverage: which texts of the source form did not reach the document.
//!
//! The source side is every user-visible text of each source PDF's XFA
//! template: draws, captions, the display items of choice lists, the master
//! pages. The template holds every variant, so a section only a configurator
//! choice reveals counts too. The document side is every text of the AEM JSON
//! document in that PDF's language. Both are split into blocks (a paragraph, a
//! list item, a table cell) and normalized before they are compared.
//!
//! A source text is covered when a document block equals it, when a document
//! table row contains it (cell boundaries are where the two sides legitimately
//! disagree), or, for a long text, when any document text contains it. A short
//! label has to match exactly, or a dropped `Name` would count as covered by a
//! surviving `Last Name`.
//!
//! The comparison is the retired engine's (`coverage_against` and `normalize` in
//! `core/src/review.rs` before 87e8a42), moved onto the XFA template and the AEM
//! JSON. A miss is a lead to check against the pages, not a defect by itself: a
//! template also carries texts only scripts use.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use u2s_xfa::xfa::{XfaNode, XfaNodeKind};

/// How many missing texts one language reports.
const MAX_MISSING: usize = 150;

/// From this many characters on, a source text counts as covered when a
/// document text contains it: a paragraph the Author split or merged differently
/// is still there, and a text this long does not occur inside another by chance.
const LONG_TEXT: usize = 40;

/// Block-level HTML elements: a text on either side of one is a separate text.
const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "table", "thead", "tbody",
    "tr", "td", "th", "caption",
];

/// The coverage of `document` against each source PDF, as the tool replies.
/// `language` limits it to the PDFs of one language.
pub fn check(sources: &[(String, Vec<u8>)], document: &Value, language: Option<&str>) -> Result<Value, String> {
    let carried: Vec<&str> = document
        .get("languages")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut reports = Vec::new();
    let mut skipped = Vec::new();
    for (name, pdf) in sources {
        let Some(context) = crate::source::read(pdf)? else {
            skipped.push(json!({ "source": name, "reason": "carries no XFA form" }));
            continue;
        };
        if language.is_some_and(|l| l != context.language) {
            continue;
        }
        // A language the document does not carry is no 0% coverage: it is a
        // language that was never authored, or one keyed differently.
        if !carried.contains(&context.language.as_str()) {
            reports.push(json!({
                "source": name,
                "language": context.language,
                "error": format!(
                    "the document carries no {:?} (its languages: {carried:?}), so this PDF's texts \
                     cannot be compared",
                    context.language
                ),
            }));
            continue;
        }
        let source = source_texts(pdf)?;
        let target = document_texts(document, &context.language);
        reports.push(report(name, &context.language, &source, &target));
    }
    if reports.is_empty() {
        return Err(match language {
            Some(l) => format!("no source PDF in language {l:?}; get_source_info lists the languages"),
            None => "no source PDF carries an XFA form".into(),
        });
    }
    Ok(json!({
        "sources": reports,
        "skipped": skipped,
        "note": "A missing text is a lead: find it on the rendered source page. Expected misses are \
                 texts only scripts use, page furniture, and texts a referenced fragment renders \
                 itself (the banking relationship, the internal-bank-use block, the partner blocks), \
                 which are not in the document.",
    }))
}

fn report(name: &str, language: &str, source: &[String], target: &DocumentTexts) -> Value {
    let (covered, missing) = compare(source, target);
    let total = covered + missing.len();
    let truncated = missing.len() > MAX_MISSING;
    json!({
        "source": name,
        "language": language,
        "source_texts": total,
        "covered": covered,
        "coverage": if total == 0 { 1.0 } else { covered as f64 / total as f64 },
        "missing": missing.into_iter().take(MAX_MISSING).collect::<Vec<_>>(),
        "truncated": truncated,
    })
}

/// The document's texts in one language: every block, and every table row.
#[derive(Debug, Default)]
pub struct DocumentTexts {
    blocks: BTreeSet<String>,
    rows: Vec<String>,
}

/// How many distinct source texts are covered, and the ones that are not, in
/// first-seen order.
fn compare(source: &[String], target: &DocumentTexts) -> (usize, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut covered = 0;
    let mut missing = Vec::new();
    for text in source.iter().map(|t| normalize(t)).filter(|t| !is_marker(t)) {
        if !seen.insert(text.clone()) {
            continue;
        }
        // A row contains a text as whole words, or a dropped `No` would count as
        // covered by any row holding `Notification`.
        let words = format!(" {text} ");
        let found = target.blocks.contains(&text)
            || target.rows.iter().any(|row| format!(" {row} ").contains(words.as_str()))
            || (text.chars().count() >= LONG_TEXT && target.blocks.iter().any(|b| b.contains(text.as_str())));
        if found {
            covered += 1;
        } else {
            missing.push(text);
        }
    }
    (covered, missing)
}

/// A list or footnote marker rather than a text: nothing but digits and
/// punctuation (`1`, `–`, `2.`, also `100%`), or a roman numeral written as a
/// list marker (`(iv)`, `iv)`, `iv.`), never a bare word such as `civil`.
fn is_marker(text: &str) -> bool {
    if !text.chars().any(char::is_alphabetic) {
        return true;
    }
    let marked = text.ends_with(')') || text.ends_with('.');
    let inner = text.trim_start_matches('(').trim_end_matches([')', '.']);
    let roman = |set: &str| !inner.is_empty() && inner.chars().all(|c| set.contains(c));
    marked && inner.len() <= 5 && (roman("ivxlc") || roman("IVXLC"))
}

/// Normalize a text for matching: collapse whitespace (a no-break space too),
/// and drop a trailing colon or asterisk, which a label carries on paper and an
/// input's label does not. Entities are the document side's business
/// ([`decode_entities`]); the XFA text arrives decoded.
pub fn normalize(text: &str) -> String {
    let collapsed = text.replace('\u{a0}', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_end_matches([':', '*', ' ']).to_string()
}

/// The text of HTML with its character references resolved: numeric ones
/// (`&#8217;`, `&#x2019;`) and the named ones an editor writes.
fn decode_entities(html: &str) -> String {
    const NAMED: &[(&str, char)] = &[
        ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''), ("nbsp", '\u{a0}'),
        ("ndash", '\u{2013}'), ("mdash", '\u{2014}'), ("lsquo", '\u{2018}'), ("rsquo", '\u{2019}'),
        ("ldquo", '\u{201c}'), ("rdquo", '\u{201d}'), ("laquo", '\u{ab}'), ("raquo", '\u{bb}'),
        ("euro", '\u{20ac}'), ("hellip", '\u{2026}'),
    ];
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let decoded = tail.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &tail[..end];
            let c = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                NAMED.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The blocks of an HTML fragment: the text between block-level tags, with
/// inline markup (`<b>`, `<span>`, `<sup>`) dropped rather than splitting a
/// sentence.
pub fn html_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if BLOCK_TAGS.contains(&name.as_str()) {
                    flush(&mut current, &mut blocks);
                }
            }
            _ if in_tag => tag.push(c),
            _ => current.push(c),
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}

fn flush(current: &mut String, blocks: &mut Vec<String>) {
    if !current.trim().is_empty() {
        blocks.push(std::mem::take(current));
    }
    current.clear();
}

/// The text of each `<tr>` of an HTML fragment, its cells joined by a space.
fn html_rows(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<tr") {
        let start = at + start;
        let end = lower[start..].find("</tr>").map_or(html.len(), |e| start + e);
        rows.push(html_blocks(&html[start..end]).join(" "));
        at = end.min(html.len());
        if at == html.len() {
            break;
        }
        at += "</tr>".len();
    }
    rows
}

/// Every text of the AEM JSON document in `language`: titles, labels, contents,
/// option labels and the header. An `HtmlDisplayer`'s table rows count as rows.
pub fn document_texts(document: &Value, language: &str) -> DocumentTexts {
    let mut texts = DocumentTexts::default();
    let mut add = |text: &str| {
        // Blocks and rows are cut at the tags first, then their entities
        // resolved, so an escaped `&lt;` in the text is not read as a tag.
        for block in html_blocks(text).iter().map(|b| decode_entities(b)) {
            let block = normalize(&block);
            if !block.is_empty() {
                texts.blocks.insert(block);
            }
        }
        for row in html_rows(text) {
            let row = normalize(&decode_entities(&row));
            if !row.is_empty() {
                texts.rows.push(row);
            }
        }
    };
    // The header holds one master-page line per line.
    if let Some(header) = document.get("header").and_then(Value::as_str) {
        header.lines().for_each(&mut add);
    }
    fn walk(value: &Value, language: &str, add: &mut dyn FnMut(&str)) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    match (key.as_str(), value) {
                        ("title" | "label" | "content", Value::Object(texts)) => {
                            if let Some(text) = texts.get(language).and_then(Value::as_str) {
                                add(text);
                            }
                        }
                        _ => walk(value, language, add),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, language, add)),
            _ => {}
        }
    }
    if let Some(form) = document.get("form") {
        walk(form, language, &mut add);
    }
    texts
}

/// Every user-visible text of a PDF's XFA template, one entry per block.
pub fn source_texts(pdf: &[u8]) -> Result<Vec<String>, String> {
    let packets = u2s_xfa::extract::extract_xfa_packets(pdf)
        .map_err(|e| format!("could not read the PDF: {e}"))?
        .ok_or("the PDF carries no XFA form")?;
    let packet = packets
        .iter()
        .find(|p| p.name == "template")
        .or_else(|| packets.iter().find(|p| p.name == "xdp"))
        .ok_or("the XFA has no template packet")?;
    let roots = XfaNode::parse(&packet.content).map_err(|e| format!("the XFA template does not parse: {e}"))?;
    let mut out = Vec::new();
    for root in &roots {
        collect(root, &mut out);
    }
    Ok(out)
}

fn tag(node: &XfaNode) -> &str {
    match &node.kind {
        XfaNodeKind::Element { tag_name, .. } => tag_name,
        XfaNodeKind::Template => "template",
        XfaNodeKind::Subform => "subform",
        XfaNodeKind::Field => "field",
        XfaNodeKind::PageSet => "pageSet",
        XfaNodeKind::PageArea => "pageArea",
        XfaNodeKind::ContentArea => "contentArea",
        XfaNodeKind::Draw => "draw",
        XfaNodeKind::Value => "value",
        XfaNodeKind::Text { .. } => "#text",
        XfaNodeKind::Bind => "bind",
        XfaNodeKind::ExclGroup => "exclGroup",
    }
}

fn child<'a>(node: &'a XfaNode, name: &str) -> Option<&'a XfaNode> {
    node.children.iter().find(|c| tag(c) == name)
}

/// The containers whose texts the reader sees, and what of each is text.
fn collect(node: &XfaNode, out: &mut Vec<String>) {
    match tag(node) {
        // Scripts, bindings, tooltips, validation messages and the like are
        // not on the page.
        "script" | "bind" | "variables" | "desc" | "assist" | "validate" | "event" | "calculate"
        | "extras" | "proto" => return,
        "draw" => {
            if let Some(value) = child(node, "value") {
                blocks_of(value, out);
            }
        }
        "field" | "exclGroup" | "subform" => {
            if let Some(value) = child(node, "caption").and_then(|c| child(c, "value")) {
                blocks_of(value, out);
            }
            // A choice list's display items are its options; a check box's or
            // a radio button's items are the values it saves.
            let choice_list = child(node, "ui").is_some_and(|ui| child(ui, "choiceList").is_some());
            if choice_list {
                for items in node.children.iter().filter(|c| tag(c) == "items") {
                    if items.attributes.get("save").is_some_and(|s| s == "1") {
                        continue;
                    }
                    for item in &items.children {
                        let text = plain_text(item);
                        if !text.trim().is_empty() {
                            out.push(text);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    for c in &node.children {
        if !matches!(tag(c), "value" | "caption" | "items") {
            collect(c, out);
        }
    }
}

/// A value's text as blocks: plain text is one block, rich text (`exData`)
/// one block per paragraph.
fn blocks_of(node: &XfaNode, out: &mut Vec<String>) {
    let mut current = String::new();
    // `rich` is whether `node` sits in an `exData`: there a newline is the
    // XML's formatting, in a plain `text` it is a line break of its own.
    fn walk(node: &XfaNode, current: &mut String, out: &mut Vec<String>, rich: bool) {
        // An image value is base64 data, not text.
        if tag(node) == "image" {
            return;
        }
        let rich = rich || tag(node) == "exData";
        let block = BLOCK_TAGS.contains(&tag(node).to_ascii_lowercase().as_str());
        if block {
            flush(current, out);
        }
        // An element's `text_content` repeats its text children, so it is read
        // only where there are none.
        let text = match &node.kind {
            XfaNodeKind::Text { content } => Some(content),
            XfaNodeKind::Element { text_content: Some(text), .. } if node.children.is_empty() => Some(text),
            _ => None,
        };
        if let Some(text) = text {
            if rich {
                current.push_str(text);
            } else {
                for (i, line) in text.split('\n').enumerate() {
                    if i > 0 {
                        flush(current, out);
                    }
                    current.push_str(line);
                }
            }
        }
        for c in &node.children {
            walk(c, current, out, rich);
        }
        if block {
            flush(current, out);
        }
    }
    walk(node, &mut current, out, false);
    flush(&mut current, out);
}

fn plain_text(node: &XfaNode) -> String {
    let mut out = Vec::new();
    blocks_of(node, &mut out);
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(blocks: &[&str], rows: &[&str]) -> DocumentTexts {
        DocumentTexts {
            blocks: blocks.iter().map(|b| normalize(b)).collect(),
            rows: rows.iter().map(|r| normalize(r)).collect(),
        }
    }

    #[test]
    fn markup_whitespace_entities_and_a_trailing_colon_do_not_matter() {
        assert_eq!(html_blocks("<p>Name and <b>address</b></p><p>Second</p>"), ["Name and address", "Second"]);
        assert_eq!(normalize(&decode_entities("  Last&nbsp;name &amp; first:\n")), "Last name & first");
        assert_eq!(decode_entities("it&#8217;s &#x2013; &euro;5 &amp;lt; &unknown;"), "it\u{2019}s \u{2013} \u{20ac}5 &lt; &unknown;");
        let (covered, missing) = compare(&["Name and address".into()], &texts(&["Name   and address"], &[]));
        assert_eq!((covered, missing.len()), (1, 0));
    }

    /// A short label has to match exactly; a table row and a long text may
    /// contain it.
    #[test]
    fn only_rows_and_long_texts_cover_by_containment() {
        let target = texts(&["Last name"], &["Fee 0,010% 0,00 - 0,15%", "Notification by post"]);
        let (_, missing) = compare(&["Name".into(), "Fee 0,010%".into(), "No".into()], &target);
        // A row covers whole words only: `No` is not in `Notification`.
        assert_eq!(missing, ["Name", "No"]);

        let long = "The client confirms that the information given in this form is complete";
        let target = texts(&[&format!("{long} and correct.")], &[]);
        assert_eq!(compare(&[long.into()], &target), (1, vec![]));
    }

    /// List markers are not texts to cover; a word made of roman-numeral
    /// letters is.
    #[test]
    fn only_list_markers_are_markers() {
        for marker in ["1", "2.", "\u{2013}", "100%", "(iv)", "iv)", "ii.", "(IV)"] {
            assert!(is_marker(marker), "{marker}");
        }
        for text in ["civil", "ill", "Vi", "CI", "Mix)", "(iv"] {
            assert!(!is_marker(text), "{text}");
        }
    }

    /// What the template walk reads: draw values (plain lines and rich
    /// paragraphs), captions, the display items of a choice list, master
    /// pages; not scripts, tooltips, saved values, check-box items or images.
    #[test]
    fn the_template_walk_reads_what_the_page_shows() {
        let xml = r#"<template>
  <pageSet><pageArea><draw><value><text>Valid from 2020</text></value></draw></pageArea></pageSet>
  <subform name="form">
    <draw><value><text>First line
Second line</text></value></draw>
    <draw><value><exData contentType="text/html"><body><p>Rich <span>one</span>
 continued</p><p>Rich two</p></body></exData></value></draw>
    <draw><value><image>iVBORw0KGgo</image></value></draw>
    <field name="kind"><ui><choiceList/></ui><caption><value><text>Kind</text></value></caption>
      <items><text>Private</text><text>Company</text></items>
      <items save="1"><text>P</text><text>C</text></items>
      <assist><toolTip>Pick one</toolTip></assist>
      <event><script>this.rawValue = "Scripted";</script></event>
    </field>
    <field name="agree"><ui><checkButton/></ui><caption><value><text>I agree</text></value></caption>
      <items><text>1</text><text>0</text></items>
    </field>
  </subform>
</template>"#;
        let roots = XfaNode::parse(xml.as_bytes()).unwrap();
        let mut texts = Vec::new();
        for root in &roots {
            collect(root, &mut texts);
        }
        let texts: Vec<String> = texts.iter().map(|t| normalize(t)).filter(|t| !t.is_empty()).collect();
        assert_eq!(
            texts,
            [
                "Valid from 2020", "First line", "Second line", "Rich one continued", "Rich two", "Kind",
                "Private", "Company", "I agree",
            ]
        );
    }

    /// A language the document does not carry is reported as such, not as a
    /// form with nothing covered.
    #[test]
    fn a_language_the_document_does_not_carry_is_named() {
        let pdf = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../forms/AAEV_019_EN.pdf")).unwrap();
        let doc = json!({"languages": ["de"], "form": {"type": "Root", "children": []}});
        let report = check(&[("AAEV_019_EN.pdf".into(), pdf)], &doc, None).unwrap();
        let error = report["sources"][0]["error"].as_str().unwrap();
        assert!(error.contains("\"en\"") && error.contains("de"), "{error}");
    }

    #[test]
    fn the_document_texts_are_the_languages_titles_labels_contents_and_options() {
        let doc = json!({
            "header": "Valid from 2020\nUBS Europe SE",
            "form": {"type": "Root", "children": [{
                "type": "Panel", "title": {"en": "Details", "de": "Angaben"}, "children": [
                    {"type": "TextField", "label": {"en": "Name:"}},
                    {"type": "RadioButton", "label": {"en": "Kind"}, "options": [{"label": {"en": "<p>Private</p>"}, "value": "1"}]},
                    {"type": "HtmlDisplayer", "content": {"en": "<table><tr><td>A</td><td>B</td></tr></table>"}}
                ]
            }]}
        });
        let texts = document_texts(&doc, "en");
        for block in ["Details", "Name", "Kind", "Private", "A", "B", "Valid from 2020", "UBS Europe SE"] {
            assert!(texts.blocks.contains(block), "{block}: {texts:?}");
        }
        assert!(!texts.blocks.contains("Angaben"));
        assert_eq!(texts.rows, ["A B"]);
    }

    /// The golden AAEV_019_EN document against its own source: most of the
    /// source is there, and a paragraph taken out of the document shows up
    /// missing.
    #[test]
    fn a_paragraph_taken_out_of_the_document_is_reported_missing() {
        let pdf = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../forms/AAEV_019_EN.pdf")).unwrap();
        let golden = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../u2s/crates/u2s-aem-ubs-mcp/tests/fixtures/golden/AAEV_019_EN/document.json"
        );
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
        let sources = vec![("AAEV_019_EN.pdf".to_string(), pdf)];

        let report = check(&sources, &doc, None).unwrap();
        let first = &report["sources"][0];
        let coverage = first["coverage"].as_f64().unwrap();
        println!("AAEV_019_EN coverage {coverage:.3}, missing {}", first["missing"]);
        assert!(coverage >= COVERAGE_FLOOR, "coverage {coverage}: {}", first["missing"]);

        // Take one covered paragraph out of the document: it must turn up missing.
        let paragraph = "The QI must withhold tax at the highest rate applicable to any partner, \
                         beneficiary, or owner.";
        assert!(!first["missing"].as_array().unwrap().contains(&json!(paragraph)));
        blank_content(&mut doc["form"], &format!("<p>{paragraph}</p>"));
        let report = check(&sources, &doc, None).unwrap();
        let missing = report["sources"][0]["missing"].as_array().unwrap();
        assert!(missing.contains(&json!(paragraph)), "{missing:?}");
    }

    /// Measured 2026-10-07, less a margin: AABF_019 0.889 (de, en) and 0.822 (es);
    /// AAOS_033_IT 0.651, whose misses are mostly texts its fragments render.
    const FLOOR_AABF: f64 = 0.75;
    const FLOOR_AAOS: f64 = 0.55;

    /// Every language of the multilingual and the Italian golden form is
    /// compared against its own PDF, under the code the document keys it by.
    #[test]
    fn every_source_language_is_compared() {
        for (form, pdfs, floor) in [
            ("AABF_019", &["AABF_019_DE.pdf", "AABF_019_EN.pdf", "AABF_019_SP.pdf"][..], FLOOR_AABF),
            ("AAOS_033_IT", &["AAOS_033_IT.pdf"][..], FLOOR_AAOS),
        ] {
            let sources: Vec<(String, Vec<u8>)> = pdfs
                .iter()
                .map(|p| {
                    let path = format!("{}/../forms/{p}", env!("CARGO_MANIFEST_DIR"));
                    (p.to_string(), std::fs::read(path).unwrap())
                })
                .collect();
            let golden = format!(
                "{}/../u2s/crates/u2s-aem-ubs-mcp/tests/fixtures/golden/{form}/document.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let doc: Value = serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let report = check(&sources, &doc, None).unwrap();
            for source in report["sources"].as_array().unwrap() {
                let coverage = source["coverage"].as_f64().unwrap();
                println!(
                    "{form} {} {coverage:.3} ({} texts) missing {}",
                    source["language"], source["source_texts"], source["missing"]
                );
                assert!(coverage >= floor, "{form} {}: {coverage}", source["language"]);
            }
        }
    }

    /// Measured on the golden document (0.815 on 2026-10-07), less a margin.
    /// The retired engine's document lacks the three section headings, the two
    /// footnotes and a dash bullet, which the tool rightly reports.
    const COVERAGE_FLOOR: f64 = 0.7;

    /// Empties every content that is exactly `html`.
    fn blank_content(node: &mut Value, html: &str) {
        if node.get("content").and_then(|c| c.get("en")).and_then(Value::as_str) == Some(html) {
            node["content"]["en"] = json!("");
        }
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            children.iter_mut().for_each(|c| blank_content(c, html));
        }
    }
}
