//! Reading a written package the way AEM reads it, for the tests that hold the
//! writer's output to a reference: the golden packages of the retired engine
//! (`golden_parity.rs`) and the writer's own snapshots (`writer_snapshots.rs`).

use std::collections::BTreeMap;
use std::io::Read;

use regex_lite::Regex;

/// Every file of a zip, by name, as text.
pub fn unzip(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a valid zip");
    (0..archive.len())
        .map(|i| {
            let mut file = archive.by_index(i).unwrap();
            let mut text = String::new();
            file.read_to_string(&mut text)
                .unwrap_or_else(|e| panic!("{} is not UTF-8: {e}", file.name()));
            (file.name().to_string(), text)
        })
        .collect()
}

/// `text` with every timestamp masked: the writer stamps the build time.
pub fn canonical(text: &str) -> String {
    let dates = Regex::new(r"\d{4}-\d\d-\d\dT[0-9:.+\-Z]+").unwrap();
    dates.replace_all(text, "<DATE>").into_owned()
}

/// `xml` as one line per element event, each element's attributes sorted and
/// blank text dropped: what AEM reads from a file, without the whitespace
/// between elements, which it ignores. Attribute values are compared as AEM
/// reads them: normalised (a raw line break is a space) and unescaped, so
/// `&gt;` and a raw `>` are the same value and a raw newline is not `&#xa;`. Every
/// element whose `name` is in `masked` is left out, with everything inside it.
pub fn structure_without(xml: &str, masked: &[&str]) -> String {
    use quick_xml::events::{BytesStart, Event};
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut depth = 0usize;
    // The depth of the masked element being skipped, if any.
    let mut skipping: Option<usize> = None;
    let value = |a: &quick_xml::events::attributes::Attribute| unescaped(&String::from_utf8_lossy(&a.value));
    let is_masked = |e: &BytesStart| {
        e.attributes()
            .flatten()
            .any(|a| a.key.as_ref() == b"name" && masked.contains(&value(&a).as_str()))
    };
    loop {
        let event = reader
            .read_event()
            .unwrap_or_else(|e| panic!("well-formed XML: {e}"));
        if let Some(at) = skipping {
            match &event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == at {
                        skipping = None;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match &event {
            Event::Start(e) if is_masked(e) => {
                skipping = Some(depth);
                depth += 1;
                continue;
            }
            Event::Empty(e) if is_masked(e) => continue,
            _ => {}
        }
        let element = |e: &BytesStart| {
            let mut attrs: Vec<String> = e
                .attributes()
                .map(|a| {
                    let a = a.unwrap();
                    format!("{}={:?}", String::from_utf8_lossy(a.key.as_ref()), value(&a))
                })
                .collect();
            attrs.sort();
            format!(
                "<{} {}>",
                String::from_utf8_lossy(e.name().as_ref()),
                attrs.join(" ")
            )
        };
        let line = match &event {
            Event::Start(e) | Event::Empty(e) => Some(element(e)),
            Event::Text(t) => {
                let text = t
                    .unescape()
                    .unwrap_or_else(|e| panic!("text AEM can read: {e}"))
                    .trim()
                    .to_string();
                (!text.is_empty()).then(|| format!("text {text:?}"))
            }
            Event::Eof => break,
            _ => None,
        };
        if let Event::End(_) = event {
            depth = depth.saturating_sub(1);
        }
        if let Some(line) = line {
            out.push_str(&"  ".repeat(depth));
            out.push_str(&line);
            out.push('\n');
        }
        if let Event::Start(_) = event {
            depth += 1;
        }
    }
    canonical(&out)
}

/// The `sling:key` to `sling:message` entries of a dictionary file.
pub fn dictionary(xml: &str) -> BTreeMap<String, String> {
    let entry = Regex::new(r#"sling:key="([^"]*)"\s+sling:message="([^"]*)""#).unwrap();
    entry
        .captures_iter(xml)
        .map(|c| (unescaped(&c[1]), unescaped(&c[2])))
        .collect()
}

/// An attribute value as XML reads it: two writers may spell one value
/// differently (`>` or `&gt;`), and both are the same value. XML's
/// attribute-value normalisation comes first (a raw newline, carriage return
/// or tab reads as a space), then the references: a writer that leaves a line
/// break raw reads differently from one that writes `&#xa;`.
pub fn unescaped(raw: &str) -> String {
    quick_xml::escape::unescape(&raw.replace(['\n', '\r', '\t'], " "))
        .unwrap_or_else(|e| panic!("{raw:?} is not an attribute value AEM can read: {e}"))
        .into_owned()
}

/// Where `expected` and `actual` first part, line by line.
pub fn first_difference(expected: &str, actual: &str) -> String {
    let line = expected
        .lines()
        .zip(actual.lines())
        .position(|(e, a)| e != a)
        .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
    format!(
        "first difference at line {}:\n  expected: {}\n  actual:   {}",
        line + 1,
        expected.lines().nth(line).unwrap_or("<end>"),
        actual.lines().nth(line).unwrap_or("<end>")
    )
}
