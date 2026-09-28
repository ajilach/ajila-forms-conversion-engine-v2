//! Parity with the retired deterministic engine (`tests/fixtures/golden/`).
//!
//! Each golden form carries `document.json`: the UBS document the old engine
//! built for that form, lifted from its own AEM tree and dictionary in the same
//! run that wrote `package.zip` and `schema.xsd`. Encoding it must reproduce
//! them:
//!
//! - every file other than a dictionary is identical once timestamps are masked;
//! - no dictionary entry translates a key differently from the golden one, and
//!   every golden translation is present unless `known-dictionary-gaps.txt`
//!   lists it. An identity entry (`fd_Text` → `Text`) is not required: AEM shows
//!   a key's own text when the dictionary lacks it, so it changes nothing;
//! - the XSD is identical.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use u2s_aem_ubs_mcp::{UbsAemDocument, encode};

const FORMS: &[&str] = &["AAOS_033_IT", "AAEV_019_EN", "AABF_019"];

fn golden_dir(form: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(form)
}

fn unzip(bytes: &[u8]) -> BTreeMap<String, String> {
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
fn canonical(text: &str) -> String {
    let dates = Regex::new(r"\d{4}-\d\d-\d\dT[0-9:.+\-Z]+").unwrap();
    dates.replace_all(text, "<DATE>").into_owned()
}

/// `xml` as one line per element event, each element's attributes sorted and
/// blank text dropped: what AEM reads from a file, without the whitespace
/// between elements, which it ignores.
fn xml_structure(xml: &str) -> String {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut depth = 0usize;
    loop {
        let event = reader
            .read_event()
            .unwrap_or_else(|e| panic!("well-formed XML: {e}"));
        let element = |e: &quick_xml::events::BytesStart| {
            let mut attrs: Vec<String> = e
                .attributes()
                .map(|a| {
                    let a = a.unwrap();
                    format!(
                        "{}={:?}",
                        String::from_utf8_lossy(a.key.as_ref()),
                        String::from_utf8_lossy(&a.value)
                    )
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
                let text = String::from_utf8_lossy(t.as_ref()).trim().to_string();
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

/// Is this entry one AEM would render the same without?
fn is_identity(key: &str, message: &str) -> bool {
    key.strip_prefix("fd_") == Some(message)
}

fn dictionary(xml: &str) -> BTreeMap<String, String> {
    let entry = Regex::new(r#"sling:key="([^"]*)"\s+sling:message="([^"]*)""#).unwrap();
    entry
        .captures_iter(xml)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

/// `<locale> <key>` lines of the golden entries the encoder knowingly does not
/// reproduce. Lines starting with `#` explain why.
fn known_gaps(form: &str) -> BTreeSet<(String, String)> {
    let path = golden_dir(form).join("known-dictionary-gaps.txt");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeSet::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (locale, key) = l.split_once(' ').expect("`<locale> <key>`");
            (locale.to_string(), key.to_string())
        })
        .collect()
}

fn first_difference(expected: &str, actual: &str) -> String {
    let line = expected
        .lines()
        .zip(actual.lines())
        .position(|(e, a)| e != a)
        .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
    format!(
        "first difference at line {}:\n  golden:  {}\n  encoded: {}",
        line + 1,
        expected.lines().nth(line).unwrap_or("<end>"),
        actual.lines().nth(line).unwrap_or("<end>")
    )
}

fn encode_golden(form: &str) -> u2s_aem_ubs_mcp::UbsAemBuild {
    let json = std::fs::read_to_string(golden_dir(form).join("document.json")).unwrap();
    let doc = UbsAemDocument::from_json(&serde_json::from_str(&json).unwrap())
        .unwrap_or_else(|e| panic!("{form}: document.json is a document: {e}"));
    encode(&doc).unwrap_or_else(|e| panic!("{form} encodes: {e}"))
}

#[test]
fn encoded_packages_match_the_golden_packages() {
    let mut failures = Vec::new();
    for form in FORMS {
        let build = encode_golden(form);
        let golden = unzip(&std::fs::read(golden_dir(form).join("package.zip")).unwrap());
        let encoded = unzip(&build.package);

        let golden_names: BTreeSet<&String> = golden.keys().collect();
        let encoded_names: BTreeSet<&String> = encoded.keys().collect();
        for name in golden_names.symmetric_difference(&encoded_names) {
            let identity_only = golden.get(*name).is_some_and(|text| {
                name.contains("/assets/dictionary/")
                    && dictionary(text).iter().all(|(k, m)| is_identity(k, m))
            });
            if !identity_only {
                let side = if golden.contains_key(*name) {
                    "golden"
                } else {
                    "encoded"
                };
                failures.push(format!("{form}: only in the {side} package: {name}"));
            }
        }

        for (name, text) in &encoded {
            let duplicates = u2s_aem_ubs_mcp::aem::duplicate_attribute_elements(text);
            if name.ends_with(".xml") && !duplicates.is_empty() {
                failures.push(format!(
                    "{form}: {name} repeats an attribute on {duplicates:?}"
                ));
            }
        }

        let gaps = known_gaps(form);
        let mut unused_gaps = gaps.clone();
        for (name, golden_text) in &golden {
            let Some(encoded_text) = encoded.get(name) else {
                continue;
            };
            if name.contains("/assets/dictionary/") {
                let locale = name.rsplit('/').next().unwrap().trim_end_matches(".xml");
                let (want, got) = (dictionary(golden_text), dictionary(encoded_text));
                for (key, message) in &want {
                    match got.get(key) {
                        Some(m) if m == message => {}
                        Some(m) => failures.push(format!(
                            "{form}: {locale} translates {key:?} as {m:?}, golden {message:?}"
                        )),
                        None if is_identity(key, message) => {}
                        None => {
                            let gap = (locale.to_string(), key.clone());
                            if !unused_gaps.remove(&gap) && !gaps.contains(&gap) {
                                failures.push(format!(
                                    "{form}: {locale} lacks {key:?} (golden {message:?})"
                                ));
                            }
                        }
                    }
                }
                continue;
            }
            let (want, got) = (canonical(golden_text), canonical(encoded_text));
            if want != got {
                failures.push(format!(
                    "{form}: {name} differs; {}",
                    first_difference(&want, &got)
                ));
            }
        }
        for (locale, key) in unused_gaps {
            failures.push(format!(
                "{form}: known-dictionary-gaps.txt lists {locale} {key:?}, which is no longer missing"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn encoded_schemas_match_the_golden_schemas() {
    for form in FORMS {
        let build = encode_golden(form);
        let golden = std::fs::read_to_string(golden_dir(form).join("schema.xsd")).unwrap();
        let xsd = build
            .xsd
            .unwrap_or_else(|| panic!("{form}: the UBS profile has a schema"));
        assert!(
            golden == xsd,
            "{form}: schema differs; {}",
            first_difference(&golden, &xsd)
        );
    }
}

/// A deployed package loads back as a document and saves unchanged, as AEM
/// reads it (the whitespace between elements aside): decoding a golden package
/// and encoding it again yields the same files, bar dictionaries, which the
/// lift rebuilds from the texts it can place. Nothing is filled in from the
/// source: the package itself records the variables and the header.
#[test]
fn decoded_golden_packages_re_encode_to_the_same_files() {
    let mut failures = Vec::new();
    for form in FORMS {
        let package = std::fs::read(golden_dir(form).join("package.zip")).unwrap();
        let doc = u2s_aem_ubs_mcp::decode(&package).unwrap_or_else(|e| panic!("{form}: {e}"));
        let build = encode(&doc).unwrap_or_else(|e| panic!("{form} re-encodes: {e}"));
        let (golden, encoded) = (unzip(&package), unzip(&build.package));
        for (name, golden_text) in &golden {
            if name.contains("/assets/dictionary/") {
                continue;
            }
            match encoded.get(name) {
                None => failures.push(format!("{form}: re-encoding drops {name}")),
                Some(text) if xml_structure(text) != xml_structure(golden_text) => {
                    failures.push(format!(
                        "{form}: {name} differs; {}",
                        first_difference(&xml_structure(golden_text), &xml_structure(text))
                    ))
                }
                Some(_) => {}
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
