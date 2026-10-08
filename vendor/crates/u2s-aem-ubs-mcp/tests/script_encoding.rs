//! Every rule the UBS writer emits survives the three encodings it is stored
//! in, read back with the generic codec of `u2s-mapper-aem`.
//!
//! A rule is a JSON SCRIPTMODEL inside a JCR multi-value inside an XML
//! attribute (`u2s_mapper_aem::script`). The writer builds some of them from
//! authored text: the subject of a repeatable's Add and Remove buttons, the
//! option a conditional panel waits for. A character one layer owns (`"` for
//! JavaScript and JSON, `,` and `\` for the multi-value, `&` and `<` for XML)
//! that reaches the wrong layer unescaped corrupts the rule without failing
//! the build: AEM loads the form and the rule stops working.
//!
//! The cases are the golden documents, the golden packages loaded and saved
//! again, and documents made hostile on purpose: the same forms with those
//! characters in the texts that flow into rules.

use u2s_aem_ubs_mcp::{UbsAemDocument, encode};
use u2s_mapper_aem::jcr::tree::{JcrNode, parse_jcr_xml};
use u2s_mapper_aem::script::{RuleShape, ScriptEvent, decode_script_list};

mod support;
use support::package::unzip;

const GOLDEN: &[&str] = &["AAOS_033_IT", "AAEV_019_EN", "AABF_019"];

/// Characters every layer has an opinion about, as an option value would
/// carry them.
const HOSTILE: &str = "Kunde, \"A\" & B's \\ C\n\tD/<E>";

/// The same characters as a subject can carry them: at most four words
/// (`sane_subject`), and no `<`, since a subject is markup-stripped first.
const HOSTILE_SUBJECT: &str = r#""A", B's \ C&D"#;

fn golden_json(form: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(form)
        .join("document.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn form_xml(package: &[u8]) -> String {
    unzip(package)
        .into_values()
        .find(|text| text.contains("guideContainer"))
        .expect("the package holds the form")
}

/// A rule attribute: the `name` of the component that owns it, the
/// attribute, its value as AEM reads it.
struct Rule {
    owner: String,
    attribute: String,
    value: String,
}

fn rules(xml: &str) -> Vec<Rule> {
    fn walk(node: &JcrNode, owner: &str, out: &mut Vec<Rule>) {
        let owner = node
            .attributes
            .iter()
            .find(|(k, _)| k == "name")
            .map_or(owner, |(_, v)| v.as_str());
        if node.tag_name == "fd:scripts" || node.tag_name == "fd:rules" {
            for (attribute, value) in &node.attributes {
                if attribute.starts_with("fd:") {
                    out.push(Rule {
                        owner: owner.to_owned(),
                        attribute: attribute.clone(),
                        value: value.clone(),
                    });
                }
            }
        }
        for child in &node.children {
            walk(child, owner, out);
        }
    }
    let mut out = Vec::new();
    walk(&parse_jcr_xml(xml).expect("the form is well-formed"), "", &mut out);
    out
}

/// Attributes whose written text holds a raw newline, carriage return or
/// tab: XML reads those back as spaces.
fn raw_whitespace_in_attributes(xml: &str) -> Vec<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut found = Vec::new();
    loop {
        match reader.read_event().expect("well-formed XML") {
            Event::Start(e) | Event::Empty(e) => {
                for a in e.attributes().flatten() {
                    if a.value.iter().any(|b| matches!(b, b'\n' | b'\r' | b'\t')) {
                        found.push(String::from_utf8_lossy(a.key.as_ref()).into_owned());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    found
}

/// Every rule of `xml` decodes, item by item, as a JSON object, and a
/// SCRIPTMODEL names the event of the attribute it is stored in. Returns
/// every rule attribute as (owner, attribute, decoded items), in document
/// order: many components share a name (every repeatable has a `BT_Add`).
fn check_rules(case: &str, xml: &str) -> Vec<(String, String, Vec<String>)> {
    let raw = raw_whitespace_in_attributes(xml);
    assert!(raw.is_empty(), "{case}: raw whitespace in attributes {raw:?}");
    let mut decoded = Vec::new();
    for rule in rules(xml) {
        if !rule.value.starts_with('[') {
            continue;
        }
        let at = format!("{case}: {} {}", rule.owner, rule.attribute);
        let items = decode_script_list(&rule.value).unwrap_or_else(|e| panic!("{at}: {e}\n{}", rule.value));
        for item in &items {
            let item: serde_json::Value = serde_json::from_str(item).unwrap();
            if let Some(event) = item.pointer("/script/event").and_then(|e| e.as_str()) {
                assert_eq!(
                    ScriptEvent::from_name(event).map(ScriptEvent::attribute),
                    Some(rule.attribute.as_str()),
                    "{at}: a {event:?} rule"
                );
            }
        }
        decoded.push((rule.owner, rule.attribute, items));
    }
    decoded
}

fn content_of(items: &[String]) -> String {
    items
        .iter()
        .filter_map(|item| {
            let item: serde_json::Value = serde_json::from_str(item).ok()?;
            item.pointer("/script/content")?.as_str().map(str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` as a JavaScript double-quoted string literal: JSON's string
/// escaping, which is valid JavaScript.
fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

fn encode_value(case: &str, json: &serde_json::Value) -> String {
    let doc = UbsAemDocument::from_json(json).unwrap_or_else(|e| panic!("{case}: a document: {e}"));
    let build = encode(&doc).unwrap_or_else(|e| panic!("{case}: encodes: {e}"));
    if let Some(bound) = &build.bound_package {
        check_rules(&format!("{case} (bound)"), &form_xml(bound));
    }
    form_xml(&build.package)
}

#[test]
fn every_rule_of_the_golden_forms_decodes() {
    for form in GOLDEN {
        let xml = encode_value(form, &golden_json(form));
        let decoded = check_rules(form, &xml);
        assert!(!decoded.is_empty(), "{form}: the form carries its rules");
    }
}

#[test]
fn every_rule_of_a_reloaded_golden_package_decodes() {
    for form in GOLDEN {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/golden")
            .join(form)
            .join("package.zip");
        let doc = u2s_aem_ubs_mcp::decode(&std::fs::read(path).unwrap()).unwrap();
        let build = encode(&doc).unwrap();
        check_rules(&format!("{form} reloaded"), &form_xml(&build.package));
    }
}

/// A repeatable's subject is passed to the accessibility helpers as a
/// JavaScript string: its quotes, backslash and comma must come out of the
/// three layers as that same string.
#[test]
fn a_hostile_repeatable_subject_reaches_the_buttons_intact() {
    let mut json = golden_json("AABF_019");
    let repeatable = json
        .pointer_mut("/form/children/0/children/6/children/1/children/0")
        .unwrap();
    assert_eq!(repeatable["name"], "RCP_CPGRP");
    repeatable["title"] =
        serde_json::json!({ "de": HOSTILE_SUBJECT, "en": HOSTILE_SUBJECT, "es": HOSTILE_SUBJECT });
    let xml = encode_value("hostile subject", &json);
    let decoded = check_rules("hostile subject", &xml);
    let label = js_string(HOSTILE_SUBJECT);
    // The party's Add and Remove, each also labelling the signature twin.
    let carrying: Vec<&String> = decoded
        .iter()
        .filter(|(_, attribute, items)| attribute == "fd:click" && content_of(items).contains(&label))
        .map(|(owner, _, _)| owner)
        .collect();
    assert!(
        carrying.contains(&&"BT_Add".to_owned()) && carrying.contains(&&"BT_Remove".to_owned()),
        "the party's Add and Remove must pass the subject as {label}; rules carrying it: {carrying:?}"
    );
}

/// `AABF_019` with its standing-order choice's first option, and the
/// conditions on it, set to [`HOSTILE`].
fn hostile_condition_document() -> serde_json::Value {
    let mut json = golden_json("AABF_019");
    let radio = json.pointer_mut("/form/children/1/children/0").unwrap();
    assert_eq!(radio["name"], "RB_StandingOrder_8059c781");
    radio["options"][0]["value"] = HOSTILE.into();
    for condition in radio["conditions"].as_array_mut().unwrap() {
        if condition["value"]["value"] == "RB_1" {
            condition["value"]["value"] = HOSTILE.into();
        }
    }
    json
}

/// A conditional panel's rule compares the trigger with the option value as
/// a JavaScript string: the value must come out of the three layers intact.
#[test]
fn a_hostile_condition_value_reaches_the_visibility_rule_intact() {
    let json = hostile_condition_document();
    let xml = encode_value("hostile condition", &json);
    let decoded = check_rules("hostile condition", &xml);
    let comparison = format!("RB_StandingOrder_8059c781.value == {}", js_string(HOSTILE));
    for panel in ["PN_a5338fd2", "PN_1e84a8ac"] {
        for attribute in ["fd:visible", "fd:init"] {
            let (_, _, items) = decoded
                .iter()
                .find(|(owner, a, _)| owner == panel && a == attribute)
                .unwrap_or_else(|| panic!("{panel} has a {attribute} rule"));
            let content = content_of(items);
            assert!(
                content.contains(&comparison),
                "{panel} {attribute}: the rule must compare with {comparison}:\n{content}"
            );
        }
    }
}

/// Loading the package back reads the hostile value out of the rule as it
/// was written, so a load and save does not corrupt it.
#[test]
fn a_hostile_condition_value_survives_a_reload() {
    let json = hostile_condition_document();
    let doc = UbsAemDocument::from_json(&json).unwrap();
    let package = encode(&doc).unwrap().package;
    let reloaded = serde_json::to_value(u2s_aem_ubs_mcp::decode(&package).unwrap()).unwrap();
    let mut radios = Vec::new();
    fn find<'a>(node: &'a serde_json::Value, name: &str, out: &mut Vec<&'a serde_json::Value>) {
        if node["name"] == name {
            out.push(node);
        }
        for child in node["children"].as_array().into_iter().flatten() {
            find(child, name, out);
        }
    }
    find(&reloaded["form"], "RB_StandingOrder_8059c781", &mut radios);
    let [radio] = radios[..] else {
        panic!("one standing-order choice, found {}", radios.len())
    };
    let values: Vec<&str> = radio["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["value"]["value"].as_str())
        .collect();
    assert_eq!(values.iter().filter(|v| **v == HOSTILE).count(), 2, "{values:?}");
}

/// The shapes of the rules in the reviewed UBS forms, from the generic
/// writer's corpus (`u2s-mapper-aem/tests/fixtures/script-corpus.zip`).
fn corpus_shapes() -> std::collections::BTreeSet<RuleShape> {
    use std::io::Read;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-mapper-aem/tests/fixtures/script-corpus.zip");
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive.by_name("attributes.jsonl").unwrap().read_to_string(&mut text).unwrap();
    let mut shapes = std::collections::BTreeSet::new();
    for line in text.lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        let xml = entry["xml"].as_str().unwrap();
        let node = parse_jcr_xml(&format!("<x a=\"{xml}\"/>")).unwrap();
        let value = &node.attributes[0].1;
        if let Ok(items) = decode_script_list(value) {
            shapes.extend(items.iter().filter_map(|item| RuleShape::of(item)));
        }
    }
    shapes
}

/// Every rule the UBS writer emits is spelled the way some rule of the
/// reviewed forms is: the same keys in the same order, the same event and
/// model, compact. A rule spelled some other way is one AEM has never been
/// seen to load.
#[test]
fn every_rule_the_writer_emits_has_a_shape_the_reviewed_forms_use() {
    let known = corpus_shapes();
    assert!(known.len() > 20, "the corpus holds the reviewed shapes, {} found", known.len());
    let mut unknown = std::collections::BTreeMap::new();
    let mut checked = 0;
    let mut cases: Vec<(String, String)> = GOLDEN
        .iter()
        .map(|form| (form.to_string(), encode_value(form, &golden_json(form))))
        .collect();
    cases.push(("hostile condition".into(), encode_value("hostile condition", &hostile_condition_document())));
    for (case, xml) in cases {
        for (owner, attribute, items) in check_rules(&case, &xml) {
            for item in items {
                let shape = RuleShape::of(&item).expect("a decoded item is an object");
                checked += 1;
                if !known.contains(&shape) {
                    unknown
                        .entry(format!("{shape:?}"))
                        .or_insert_with(|| format!("{case}: {owner} {attribute}"));
                }
            }
        }
    }
    assert!(checked > 100, "only {checked} rules checked");
    assert!(
        unknown.is_empty(),
        "rules spelled in shapes no reviewed form uses:\n{}",
        unknown
            .iter()
            .map(|(shape, at)| format!("{at}: {shape}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A character XML 1.0 cannot carry, in a text that reaches a rule, is
/// refused when the document is encoded: AEM would reject the package.
#[test]
fn a_text_holding_a_character_xml_cannot_carry_is_refused() {
    let mut json = golden_json("AABF_019");
    let repeatable = json
        .pointer_mut("/form/children/0/children/6/children/1/children/0")
        .unwrap();
    repeatable["title"] = serde_json::json!({ "de": "A\u{1}B", "en": "A\u{1}B", "es": "A\u{1}B" });
    let doc = UbsAemDocument::from_json(&json).unwrap();
    match encode(&doc) {
        Err(u2s_aem_ubs_mcp::Error::InvalidDocument(message)) => {
            assert!(message.contains("U+0001"), "{message}")
        }
        Err(other) => panic!("refused for another reason: {other}"),
        Ok(_) => panic!("the document must be refused"),
    }
}
