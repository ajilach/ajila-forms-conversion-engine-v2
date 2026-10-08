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
//!
//! The account-holder cluster and the signature block of `AABF_019` and
//! `AAOS_033_IT` were custom templates in the retired engine; the documents now
//! author them as ordinary nodes, which render differently. Those subtrees
//! ([`REAUTHORED`]) are left out of the comparison on both sides, everything
//! else is held to the golden output as before, and the schema, to which the
//! templates contributed nothing, is compared for the document without them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use u2s_aem_ubs_mcp::{UbsAemDocument, encode};

const FORMS: &[&str] = &["AAOS_033_IT", "AAEV_019_EN", "AABF_019"];

/// The `name`s of the subtrees re-authored since the golden run: in the golden
/// packages the roots the retired custom templates wrote, in the documents the
/// ordinary nodes that replace them.
const REAUTHORED: &[&str] = &[
    // The configurator choice, `formular_adressat_radio` and `tipo_radio`.
    "RB_FormularAdressat",
    "RB_GroupTipo",
    // The account-holder cluster, `account_holder` and `account_holder_it`.
    "PN_AccountHolder",
    "PN_DatiDelIClienteIDiSeguitoIl_5156bd48",
    // The signature block, `signatures` and `signatures_it`.
    "PN_SignatureBlock",
    "PN_Signatures_de979668",
    "PN_FirmaE_de979668",
    // The step titles of the steps those land on: a step whose inputs repeat
    // hands its jump-to-field button to the rows (`panel.xml`), and the
    // templates' contents were opaque to that rule.
    "PN_FormConfigurator_a2b5bc39Title",
    "PN_SignaturesTitle",
    "PN_FormConfigurator_b550357eTitle",
    "PN_FirmaE_d942e2baTitle",
];

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
/// between elements, which it ignores. Every element whose `name` is in
/// `masked` is left out, with everything inside it.
fn structure_without(xml: &str, masked: &[&str]) -> String {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut depth = 0usize;
    // The depth of the masked element being skipped, if any.
    let mut skipping: Option<usize> = None;
    let is_masked = |e: &quick_xml::events::BytesStart| {
        e.attributes().flatten().any(|a| {
            a.key.as_ref() == b"name" && masked.contains(&String::from_utf8_lossy(&a.value).as_ref())
        })
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

fn golden_document(form: &str) -> serde_json::Value {
    let json = std::fs::read_to_string(golden_dir(form).join("document.json")).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn encode_json(form: &str, json: &serde_json::Value) -> u2s_aem_ubs_mcp::UbsAemBuild {
    let doc = UbsAemDocument::from_json(json)
        .unwrap_or_else(|e| panic!("{form}: document.json is a document: {e}"));
    encode(&doc).unwrap_or_else(|e| panic!("{form} encodes: {e}"))
}

fn encode_golden(form: &str) -> u2s_aem_ubs_mcp::UbsAemBuild {
    encode_json(form, &golden_document(form))
}

/// `node` without the [`REAUTHORED`] subtrees.
fn without_reauthored(mut node: serde_json::Value) -> serde_json::Value {
    if let Some(children) = node.get_mut("children").and_then(|c| c.as_array_mut()) {
        children.retain(|c| !c["name"].as_str().is_some_and(|n| REAUTHORED.contains(&n)));
        for child in children.iter_mut() {
            *child = without_reauthored(child.take());
        }
    }
    node
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
            let (want, got) = if name.ends_with(".xml") {
                (
                    structure_without(golden_text, REAUTHORED),
                    structure_without(encoded_text, REAUTHORED),
                )
            } else {
                (canonical(golden_text), canonical(encoded_text))
            };
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
        let mut doc = golden_document(form);
        doc["form"] = without_reauthored(doc["form"].take());
        let build = encode_json(form, &doc);
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
                Some(text)
                    if structure_without(text, REAUTHORED)
                        != structure_without(golden_text, REAUTHORED) =>
                {
                    failures.push(format!(
                        "{form}: {name} differs; {}",
                        first_difference(
                            &structure_without(golden_text, REAUTHORED),
                            &structure_without(text, REAUTHORED)
                        )
                    ))
                }
                Some(_) => {}
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The re-authored account-holder clusters behave as the custom templates did:
/// the configurator shows each party's container, each party's Add and Remove
/// drive its signature twin, and a partner generic hides the sub-panels the
/// form does not need. The packages pass the form checks.
#[test]
fn the_reauthored_clusters_pair_each_party_with_its_signature() {
    for (form, configurator, twins) in [
        (
            "AABF_019",
            "RB_FormularAdressat",
            &["RCP_SGN_CPGRP_repeat", "RCP_Sign_AHGRP_repeat", "RCP_Sign_AHGRP_AR_repeat"][..],
        ),
        (
            "AAOS_033_IT",
            "RB_GroupTipo",
            &["RCP_SGN_CPGRP_repeat", "RCP_Sign_AHGRP_repeat"][..],
        ),
    ] {
        let files = unzip(&encode_golden(form).package);
        let xml = files
            .values()
            .find(|text| text.contains("guideContainer"))
            .unwrap_or_else(|| panic!("{form}: the package holds the form"));
        for twin in twins {
            assert!(
                xml.contains(&format!("window.forms.ubs.addInstance({twin});"))
                    && xml.contains(&format!("window.forms.ubs.removeInstance({twin});")),
                "{form}: a party's buttons must drive {twin}"
            );
        }
        assert!(
            xml.contains("window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);"),
            "{form}: the contracting party hides its entity sub-panel"
        );
        assert!(
            xml.contains(&format!("{configurator}.value == \\\\&quot;1\\\\&quot;")),
            "{form}: the configurator shows the individual's container"
        );
        // The configurator opens on the individual (feedback
        // PROBLEM-formconfig-private-person-default): its option key as `_value`.
        let radio = xml
            .split('<')
            .find(|element| element.contains(&format!("name=\"{configurator}\"")))
            .unwrap_or_else(|| panic!("{form}: the configurator is in the form"));
        assert!(
            radio.contains(r#"_value="1""#),
            "{form}: the configurator must preselect option 1: {radio}"
        );
        // Switching option empties the panels the choice decides (feedback #107).
        for panel in ["PN_IndividualContainer", "PN_LegalEntityContainer"] {
            assert!(
                xml.contains(&format!("{panel}.resetData();")),
                "{form}: the configurator must empty {panel}"
            );
        }
        u2s_aem_ubs_mcp::aem::validate_aem_form_xml(xml)
            .unwrap_or_else(|errors| panic!("{form}: {errors:?}"));
    }
}
