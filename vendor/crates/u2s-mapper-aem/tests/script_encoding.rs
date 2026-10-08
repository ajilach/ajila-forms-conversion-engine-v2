//! The three encodings of a rule, held to the forms AEM actually carries.
//!
//! A rule is a JSON object (layer 3) inside a JCR multi-value (layer 2)
//! inside an XML attribute (layer 1); see `u2s_mapper_aem::script`. A slip in
//! any layer corrupts the JavaScript silently: AEM still loads the form and
//! the rule just stops working. These tests hold each layer, and the three
//! together, to two references:
//!
//! - **The corpus**: every distinct rule attribute of the 420 reviewed UBS
//!   packages (`tests/fixtures/script-corpus.zip`, written by
//!   `scripts/extract-script-corpus.py`). Each must decode, re-encode to the
//!   same bytes, survive the XML writer, and, where it is spelled the way
//!   this encoder writes rules, rebuild from its typed view byte for byte.
//! - **Generated content**: JavaScript built from the characters that each
//!   layer escapes (`"` `\` `,` `<` `&` `'` newlines, ...), run through the
//!   writer of a whole form and the decoder of a whole package, and compared
//!   with what went in.

use std::collections::BTreeMap;
use std::io::Read;

use serde::Deserialize;
use u2s_aem::model::{AemForm, RawJcrNode};
use u2s_mapper_aem::jcr::tree::{JcrNode, parse_jcr_xml};
use u2s_mapper_aem::script::{
    BodyOrder, EventScript, RuleShape, ScriptEvent, ScriptListError, decode_script_list,
    encode_script_list,
};
use u2s_mapper_aem::xml_writer::raw_node_xml;

// ---------------------------------------------------------------- corpus --

#[derive(Deserialize)]
struct CorpusEntry {
    form: String,
    element: String,
    owner: String,
    attribute: String,
    /// The attribute value as the file spells it, still XML-escaped.
    xml: String,
}

fn corpus() -> Vec<CorpusEntry> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/script-corpus.zip");
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name("attributes.jsonl")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let entries: Vec<CorpusEntry> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("a corpus line"))
        .collect();
    assert!(entries.len() > 6000, "the corpus is the whole reviewed set");
    entries
}

/// An attribute value as the mapper's own XML reader reads it.
fn read_attribute(xml_value: &str) -> String {
    let node = parse_jcr_xml(&format!("<x a=\"{xml_value}\"/>")).expect("the corpus is well-formed");
    node.attributes.into_iter().next().unwrap().1
}

/// The value written by the mapper's XML writer as an attribute of a raw
/// node, and read back by its reader. Also checks the written text: an XML
/// reader replaces a raw newline, carriage return or tab inside an attribute
/// with a space, so the writer must spell them as character references.
fn through_xml_writer(attribute: &str, value: &str) -> Result<String, String> {
    let node = RawJcrNode {
        tag_name: "fd:scripts".into(),
        attributes: BTreeMap::from([(attribute.to_owned(), value.to_owned())]),
        children: Vec::new(),
    };
    let xml = raw_node_xml(&node).map_err(|e| e.to_string())?;
    if let Some(raw) = ['\n', '\r', '\t'].iter().find(|c| xml.trim_end().contains(**c)) {
        return Err(format!("the writer left a raw {raw:?} in an attribute"));
    }
    let read = parse_jcr_xml(&xml).map_err(|e| e.to_string())?;
    Ok(read
        .attributes
        .into_iter()
        .find(|(k, _)| k == attribute)
        .ok_or("the attribute is gone")?
        .1)
}

/// Defects of the deployed forms themselves, each named so a new one fails
/// the test instead of hiding among them: `(form, attribute, error)`.
const KNOWN_CORPUS_DEFECTS: &[(&str, &str, &str)] = &[
    // Two rules appended with an escaped comma between them: AEM reads one
    // malformed item. The address-mandatory fixer of the feedback repo wrote
    // these; the rules after the first do not run.
    ("AAJB_033", "fd:init", "escaped-separator"),
    ("AAPO_033", "fd:init", "escaped-separator"),
    ("AASV_033", "fd:init", "escaped-separator"),
    ("AAUN_033", "fd:init", "escaped-separator"),
    ("BAUL_033", "fd:init", "escaped-separator"),
    ("AATN_033", "fd:calc", "escaped-separator"),
];

fn defect_kind(error: &ScriptListError) -> &'static str {
    match error {
        ScriptListError::NotAList => "not-a-list",
        ScriptListError::StrayEscape { .. } => "stray-escape",
        ScriptListError::EmptyItem { .. } => "empty-item",
        ScriptListError::EscapedSeparator { .. } => "escaped-separator",
        ScriptListError::NotAnObject { .. } => "not-an-object",
    }
}

/// Is `item` spelled the way [`EventScript::to_json`] writes a rule: compact,
/// with its keys in one of the two orders and the fixed values? Then
/// rebuilding it from its typed view must give the same bytes, which is what
/// proves the encoder escapes `content` the way AEM does.
fn is_this_encoders_spelling(item: &str) -> bool {
    let Some(shape) = RuleShape::of(item) else {
        return false;
    };
    let value: serde_json::Value = serde_json::from_str(item).unwrap();
    let is = |pointer: &str, want: serde_json::Value| value.pointer(pointer) == Some(&want);
    let strings = |keys: &[&str]| {
        keys.iter()
            .all(|k| value.pointer(&format!("/script/{k}")).is_some_and(|v| v.is_string()))
    };
    let top = shape.keys == ["script", "nodeName", "version", "enabled"]
        || shape.keys == ["script", "nodeName", "version", "enabled", "_archetype"];
    let fixed = is("/nodeName", "SCRIPTMODEL".into()) && is("/version", 1.into()) && is("/enabled", true.into());
    let field_first = shape.script_keys == ["field", "event", "model", "content"]
        && shape.model.as_deref() == Some("EVENT_SCRIPTS")
        && value.pointer("/script/model").and_then(|m| m.as_object()).is_some_and(|m| m.len() == 1);
    let content_first = shape.script_keys == ["content", "event", "field"];
    shape.compact
        && top
        && fixed
        && shape.event.as_deref().and_then(ScriptEvent::from_name).is_some()
        && strings(&["field", "event", "content"])
        && (value.get("_archetype").is_none() || shape.archetype.is_some())
        && (field_first || content_first)
}

#[test]
fn every_corpus_rule_decodes_and_re_encodes_to_its_own_bytes() {
    let mut failures = Vec::new();
    let mut defects = Vec::new();
    let (mut lists, mut items, mut typed) = (0, 0, 0);
    for entry in corpus() {
        let value = read_attribute(&entry.xml);
        let at = format!("{} {} {} {}", entry.form, entry.element, entry.owner, entry.attribute);

        // Layer 1: the writer and reader of this crate give the value back.
        match through_xml_writer(&entry.attribute, &value) {
            Ok(back) if back == value => {}
            Ok(_) => failures.push(format!("{at}: the XML writer changed the value")),
            Err(e) => failures.push(format!("{at}: {e}")),
        }

        if !value.starts_with('[') {
            // A plain property such as `fd:trusted="{Boolean}true"`.
            continue;
        }
        // Layer 2 and 3.
        let decoded = match decode_script_list(&value) {
            Ok(decoded) => decoded,
            Err(error) => {
                defects.push((entry.form.clone(), entry.attribute.clone(), defect_kind(&error)));
                continue;
            }
        };
        lists += 1;
        if encode_script_list(&decoded) != value {
            failures.push(format!("{at}: re-encoding changed the value"));
        }
        for item in &decoded {
            items += 1;
            let shape = RuleShape::of(item).expect("a decoded item is an object");
            // A code-editor rule names the event its attribute stores.
            if let Some(event) = shape.event.as_deref() {
                let stored_in = ScriptEvent::from_name(event).map(ScriptEvent::attribute);
                // `fd:initialize` is not an attribute AEM reads; three
                // corpus forms carry it, and the rule in it never runs.
                if stored_in != Some(entry.attribute.as_str()) && entry.attribute != "fd:initialize" {
                    failures.push(format!("{at}: a {event:?} rule in {}", entry.attribute));
                }
            }
            if is_this_encoders_spelling(item) {
                typed += 1;
                match EventScript::from_json(item) {
                    Some(script) if script.to_json() == *item => {}
                    _ => failures.push(format!(
                        "{at}: spelled as this encoder writes rules, but does not rebuild from its typed view: {item}"
                    )),
                }
            }
        }
    }
    let mut known: Vec<(String, String, &str)> = KNOWN_CORPUS_DEFECTS
        .iter()
        .map(|(f, a, k)| (f.to_string(), a.to_string(), *k))
        .collect();
    known.sort();
    defects.sort();
    defects.dedup();
    assert_eq!(defects, known, "the corpus defects changed");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    // The corpus is mostly rules this encoder can write: if that changes, the
    // typed view stopped recognising them.
    assert!(lists > 6000 && items > 7000 && typed > 5000, "{lists} lists, {items} items, {typed} typed");
}

// ---------------------------------------------------- generated content --

/// A small deterministic generator: the same strings on every run, no
/// dependency.
struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The pieces every layer has an opinion about, and some it should not.
const PIECES: &[&str] = &[
    "a", "Z", "0", " ", "  ", ",", "\\", "\\\\", "\\,", ",\\", "\"", "'", "<", ">", "&", "&amp;",
    "&quot;", "&#xa;", "\n", "\r\n", "\t", "\\n", "\\\"", "[", "]", "{", "}", "=", ":", ";", "/",
    "//", "</script>", "é", "ü", "ß", "中", "\u{2028}", "${x}", "`", "\\u0041",
    "window.forms.ubs.hideAFHideDor(this.PN_A);", "if (a && b) { c(\"x, y\"); }",
];

fn generated_content(generator: &mut Generator) -> String {
    let len = generator.below(24);
    (0..len).map(|_| PIECES[generator.below(PIECES.len())]).collect()
}

fn generated_script(generator: &mut Generator) -> EventScript {
    EventScript {
        field: if generator.below(2) == 0 {
            "this".into()
        } else {
            "guide.guideRootPanel.PN_A.TXT_B".into()
        },
        event: ScriptEvent::ALL[generator.below(ScriptEvent::ALL.len())],
        content: generated_content(generator),
        order: if generator.below(2) == 0 {
            BodyOrder::FieldFirst
        } else {
            BodyOrder::ContentFirst
        },
        archetype: (generator.below(3) == 0).then(|| "generated, \"marker\"".into()),
    }
}

#[test]
fn generated_rules_survive_every_layer() {
    let mut generator = Generator(0x5eed_1234_abcd_0001);
    for round in 0..5000 {
        let scripts: Vec<EventScript> = (0..1 + generator.below(4))
            .map(|_| generated_script(&mut generator))
            .collect();
        let value = encode_script_list(scripts.iter().map(EventScript::to_json));
        let read = through_xml_writer("fd:click", &value)
            .unwrap_or_else(|e| panic!("round {round}: {e}\n{value}"));
        assert_eq!(read, value, "round {round}: layer 1");
        let items = decode_script_list(&read).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let back: Vec<EventScript> = items
            .iter()
            .map(|item| EventScript::from_json(item).unwrap_or_else(|| panic!("round {round}: {item}")))
            .collect();
        assert_eq!(back, scripts, "round {round}: layers 2 and 3");
    }
}

// -------------------------------------------------------- whole forms --

/// A minimal valid form whose panel and text field each carry `scripts` as an
/// `fd:scripts` child.
fn form_with_scripts(scripts: &BTreeMap<String, String>) -> AemForm {
    let scripts_node = serde_json::json!({ "tag_name": "fd:scripts", "attributes": scripts });
    serde_json::from_value(serde_json::json!({
        "metadata": {
            "form_name": "ScriptForm",
            "title": { "en": "Scripts" },
            "master_language": "en",
            "languages": ["en"],
            "dor": "none",
            "data_model": { "kind": "unbound" },
            "toolbar": []
        },
        "pages": [{
            "name": "PageOne",
            "properties": { "jcr:title": { "kind": "text", "value": { "en": "One" } } },
            "children": [{
                "type": "Component",
                "common": {
                    "name": "PN_Panel",
                    "resource_type": "fd/af/components/panel",
                    "passthrough": { "raw_children": [scripts_node.clone()] }
                },
                "properties": {},
                "children": [{
                    "type": "TextField",
                    "common": {
                        "name": "TXT_Field",
                        "resource_type": "fd/af/components/controls/textbox",
                        "passthrough": { "raw_children": [scripts_node] }
                    },
                    "field": { "label": { "en": "Field" } },
                    "layout": { "width": 12 },
                    "input": "single_line"
                }]
            }]
        }]
    }))
    .expect("a form")
}

/// A character XML 1.0 cannot carry is refused, not written: AEM's parser
/// would reject the package. (A rule's JSON never holds one raw, since JSON
/// escapes control characters; a hand-edited passthrough value can.)
#[test]
fn a_value_holding_a_character_xml_cannot_carry_is_refused() {
    let error = through_xml_writer("fd:click", "[{\"a\":\"\u{1}\"}]").unwrap_err();
    assert!(error.contains("U+0001"), "{error}");
}

/// A malformed escape is an error when the value is read, never an empty
/// value that a save would write back.
#[test]
fn a_malformed_escape_is_an_error_when_read() {
    assert!(parse_jcr_xml(r#"<x fd:click="[&bogus;]"/>"#).is_err());
}

/// A text field cannot carry both a validation message, which is written as
/// its `fd:rules`, and an `fd:rules` of its own: JCR holds one child of a name.
#[test]
fn a_validation_message_beside_a_carried_fd_rules_is_refused() {
    let mut json = serde_json::to_value(form_with_scripts(&BTreeMap::new())).unwrap();
    let field = json.pointer_mut("/pages/0/children/0/children/0").unwrap();
    field["validation"] = serde_json::json!({ "pattern": "999", "message": { "en": "Digits" } });
    field["common"]["passthrough"]["raw_children"] =
        serde_json::json!([{ "tag_name": "fd:rules", "attributes": {} }]);
    let form: AemForm = serde_json::from_value(json).unwrap();
    let form = form.validate().expect("a valid form");
    let master = form.form().metadata.master_language.clone();
    let result = u2s_mapper_aem::xml_writer::write_form_xml(
        &form,
        &u2s_mapper_aem::xml_writer::WriteCtx { master: &master, bind_refs: &Default::default() },
    );
    let error = result.expect_err("the form must be refused").to_string();
    assert!(error.contains("TXT_Field"), "{error}");
}

fn scripts_of<'a>(node: &'a JcrNode, name: &str) -> Option<&'a JcrNode> {
    if node.attributes.iter().any(|(k, v)| k == "name" && v == name) {
        return node.children.iter().find(|c| c.tag_name == "fd:scripts");
    }
    node.children.iter().find_map(|c| scripts_of(c, name))
}

#[test]
fn generated_rules_survive_the_form_writer_on_panels_and_fields() {
    let mut generator = Generator(0x5eed_0000_f0f0_0002);
    for round in 0..300 {
        let scripts: BTreeMap<String, String> = [ScriptEvent::Initialize, ScriptEvent::Click, ScriptEvent::ValueCommit]
            .into_iter()
            .map(|event| {
                let rules: Vec<String> = (0..1 + generator.below(3))
                    .map(|_| EventScript { event, ..generated_script(&mut generator) }.to_json())
                    .collect();
                (event.attribute().to_owned(), encode_script_list(&rules))
            })
            .collect();
        let form = form_with_scripts(&scripts)
            .validate()
            .unwrap_or_else(|e| panic!("round {round}: the form validates: {e:?}"));
        let master = form.form().metadata.master_language.clone();
        let xml = u2s_mapper_aem::xml_writer::write_form_xml(
            &form,
            &u2s_mapper_aem::xml_writer::WriteCtx { master: &master, bind_refs: &Default::default() },
        )
        .unwrap();
        let root = parse_jcr_xml(&xml).expect("the written form is well-formed");
        for host in ["PN_Panel", "TXT_Field"] {
            let written = scripts_of(&root, host)
                .unwrap_or_else(|| panic!("round {round}: {host} lost its fd:scripts"));
            let attributes: BTreeMap<String, String> = written
                .attributes
                .iter()
                .filter(|(k, _)| k.starts_with("fd:"))
                .cloned()
                .collect();
            assert_eq!(attributes, scripts, "round {round}: {host}");
        }
    }
}

#[test]
fn corpus_rules_survive_a_package_encode_and_decode() {
    // Every corpus list, a few hundred per form so a failure points at one.
    let lists: Vec<(String, String)> = corpus()
        .into_iter()
        .filter(|e| e.attribute != "fd:initialize")
        .filter_map(|e| {
            let value = read_attribute(&e.xml);
            (value.starts_with('[') && ScriptEvent::from_attribute(&e.attribute).is_some())
                .then_some((e.attribute, value))
        })
        .collect();
    for chunk in lists.chunks(200) {
        // One attribute name holds one value: spread the chunk over forms.
        let mut by_attribute: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (attribute, value) in chunk {
            by_attribute.entry(attribute.clone()).or_default().push(value.clone());
        }
        let depth = by_attribute.values().map(Vec::len).max().unwrap();
        for i in 0..depth {
            let scripts: BTreeMap<String, String> = by_attribute
                .iter()
                .filter_map(|(a, values)| values.get(i).map(|v| (a.clone(), v.clone())))
                .collect();
            let form = form_with_scripts(&scripts).validate().expect("a valid form");
            let package = u2s_mapper_aem::encode(&form).expect("the form encodes");
            let decoded = u2s_mapper_aem::decode::decode(&package.bytes).expect("the package decodes");
            let json = serde_json::to_value(decoded.form()).unwrap();
            // Each host keeps its own: the panel and the field it holds.
            for host in ["/pages/0/children/0", "/pages/0/children/0/children/0"] {
                let raw = json
                    .pointer(&format!("{host}/common/passthrough/raw_children"))
                    .and_then(|c| c.as_array())
                    .unwrap_or_else(|| panic!("{host} kept no passthrough children"));
                let kept: BTreeMap<String, String> = raw
                    .iter()
                    .find(|c| c["tag_name"] == "fd:scripts")
                    .and_then(|c| serde_json::from_value(c["attributes"].clone()).ok())
                    .unwrap_or_else(|| panic!("{host} lost its fd:scripts"));
                let kept: BTreeMap<String, String> =
                    kept.into_iter().filter(|(k, _)| k.starts_with("fd:")).collect();
                assert_eq!(kept, scripts, "{host}: the corpus rules did not survive the package");
            }
        }
    }
}
