//! A letter: a form whose first page opens with the UBS letterhead.
//!
//! The letterhead is written as the profile's letterhead component, and the
//! form around it as a letter to the UBS DoR: the metadata component's
//! `redactoLayout="letterDor"` and the letter DoR template, both derived from
//! the node's presence alone. A package's letterhead reads back into the node.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use u2s_aem_ubs_mcp::aem::{AemI18nText, AemNodeTranslated, LetterAddress, LetterParty};
use u2s_aem_ubs_mcp::{Error, UbsAemDocument, decode, encode};
use u2s_mapper_aem::jcr::tree::{JcrNode, parse_jcr_xml};

const LETTER_TEMPLATE: &str = "/content/dam/formsanddocuments/reference-dor-templates/ajila-forms-ubs/02_forms/UBS_Blank_Letter_DoR.xdp";

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/letters")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn letterhead(sender: Value, receiver: Value) -> Value {
    json!({
        "type": "Letterhead", "uuid": "00000000-0000-4000-8000-0000000000c1", "name": "PN_LetterHead",
        "sender": sender, "receiver": receiver,
        "subject": {"it": "Oggetto: Recesso dal servizio", "en": "Subject: Withdrawal from the service"},
        "your_reference": {"it": "Prot. n.", "en": "Ref. no."},
        "our_reference": null
    })
}

fn bank() -> Value {
    json!({"kind": "Fixed", "address": {
        "company": "UBS Europe SE (Succursale Italia)", "bold_company": true,
        "street": "Via del Vecchio Politecnico", "street_no": "3",
        "postal_code": "20121", "city": "Milano"
    }})
}

fn preface() -> Value {
    json!({"type": "Preface", "uuid": "00000000-0000-4000-8000-0000000000b1", "name": "PN_BR"})
}

fn field() -> Value {
    json!({
        "type": "TextField", "uuid": "00000000-0000-4000-8000-0000000000d1", "name": "TXT_Name",
        "label": {"it": "Nome", "en": "Name"}, "mandatory": false, "visible": true,
        "max_chars": null, "colspan": 12, "dor_colspan": null, "bind_ref": null
    })
}

fn page(uuid: &str, name: &str, children: Value) -> Value {
    json!({
        "type": "Panel", "uuid": uuid, "name": name,
        "title": {"it": "Dati", "en": "Details"}, "is_page": true, "visible": true,
        "is_conditional": false, "dor_num_cols": null, "colspan": 12, "dor_colspan": null,
        "bind_ref": null, "frag_ref": null, "children": children
    })
}

/// A two-page Italian and English form whose first page holds `children`.
fn document(children: Value) -> Value {
    json!({
        "variables": {"formrange_code": "AAQR", "formrange_entity": "033"},
        "languages": ["it", "en"],
        "form": {"type": "Root", "title": {"it": "Lettera", "en": "Letter"}, "children": [
            page("00000000-0000-4000-8000-0000000000a1", "PN_Page", children),
            page("00000000-0000-4000-8000-0000000000a2", "PN_Second", json!([]))
        ]}
    })
}

fn letter() -> Value {
    document(json!([
        preface(),
        letterhead(bank(), json!({"kind": "Entered"})),
        field()
    ]))
}

fn build(value: &Value) -> u2s_aem_ubs_mcp::UbsAemBuild {
    encode(&UbsAemDocument::from_json(value).expect("the document reads"))
        .expect("the document encodes")
}

fn refusal(value: Value) -> String {
    match encode(&UbsAemDocument::from_json(&value).expect("the document reads")) {
        Err(Error::InvalidDocument(message)) => message,
        Err(other) => panic!("refused for another reason: {other}"),
        Ok(_) => panic!("the document must be refused"),
    }
}

fn files(package: &[u8]) -> BTreeMap<String, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(package)).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut entry = archive.by_index(i).unwrap();
            let mut text = String::new();
            entry.read_to_string(&mut text).unwrap();
            (entry.name().to_owned(), text)
        })
        .collect()
}

fn form_xml(package: &[u8]) -> String {
    files(package)
        .into_values()
        .find(|xml| xml.contains("guideContainer"))
        .expect("the package holds the form")
}

fn dictionary(package: &[u8], lang: &str) -> String {
    files(package)
        .into_iter()
        .find(|(name, _)| name.ends_with(&format!("/dictionary/{lang}.xml")))
        .map(|(_, xml)| xml)
        .unwrap_or_default()
}

fn find<'a>(node: &'a JcrNode, test: &dyn Fn(&JcrNode) -> bool) -> Option<&'a JcrNode> {
    if test(node) {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, test))
}

fn all<'a>(node: &'a JcrNode, test: &dyn Fn(&JcrNode) -> bool, out: &mut Vec<&'a JcrNode>) {
    if test(node) {
        out.push(node);
    }
    node.children.iter().for_each(|c| all(c, test, out));
}

fn named<'a>(node: &'a JcrNode, name: &str) -> &'a JcrNode {
    find(node, &|n| n.component_name() == Some(name)).unwrap_or_else(|| panic!("no `{name}`"))
}

fn letterhead_of(form: &JcrNode) -> Option<&JcrNode> {
    find(form, &|n| {
        n.resource_type()
            .is_some_and(|t| t.ends_with("/controls/letterhead"))
    })
}

fn hidden(node: &JcrNode) -> bool {
    node.attr("visible").is_some_and(|v| v.contains("false"))
}

/// The first `Letterhead` of a decoded document.
fn decoded_letterhead(doc: &UbsAemDocument) -> Option<AemNodeTranslated> {
    let mut found = None;
    doc.form.visit(&mut |node| {
        if found.is_none() && matches!(node, AemNodeTranslated::Letterhead { .. }) {
            found = Some(node.clone());
        }
    });
    found
}

fn text(pairs: &[(&str, &str)]) -> AemI18nText {
    AemI18nText(
        pairs
            .iter()
            .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
            .collect(),
    )
}

/// A letter is the profile's letterhead, written for the letter, and a letter
/// to the DoR: the metadata asks for the letter layout, the print settings name
/// the letter template, and the sender fills the header's `slot2`. Nothing of
/// the letterhead reaches the summary.
#[test]
fn a_letter_is_written_with_the_letterhead_and_the_letter_layout() {
    let build = build(&letter());
    let xml = form_xml(&build.package);
    assert_valid_form(&xml);
    let form = parse_jcr_xml(&xml).unwrap();

    let metadata = find(&form, &|n| {
        n.resource_type()
            .is_some_and(|t| t.ends_with("/controls/metadata"))
    })
    .unwrap();
    assert_eq!(metadata.attr("redactoLayout"), Some("letterDor"));
    let print = find(&form, &|n| n.attr("metaTemplateRef").is_some()).unwrap();
    assert_eq!(print.attr("metaTemplateRef"), Some(LETTER_TEMPLATE));

    let lh = letterhead_of(&form).expect("the form holds the letterhead");
    assert_eq!(lh.component_name(), Some("PN_LetterHead"));
    let info = named(lh, "AF_Sender_Info_For_Header_Slot");
    assert_eq!(info.attr("dorHeaderSlot"), Some("slot2"));
    assert_eq!(info.attr("hideLabelAndBorder"), Some("true"));

    let mut components = Vec::new();
    all(lh, &|n| n.component_name().is_some(), &mut components);
    assert!(components.len() > 40, "{}", components.len());
    for component in components {
        let name = component.component_name().unwrap();
        assert_eq!(
            component.attr("summaryExclusion"),
            Some("true"),
            "`{name}` reaches the summary"
        );
        assert_eq!(
            component.attr("dorExclusion"),
            Some("true"),
            "`{name}` reaches the DoR"
        );
    }

    // The bank's address is prefilled and hidden; the receiver is the reader's to enter.
    assert!(
        hidden(named(lh, "PN_Sender_Address")) && hidden(named(lh, "TTL_Letter_Sender_Subtitle"))
    );
    assert_eq!(
        named(lh, "AF_Sender_CompanyName").attr("_value"),
        Some("UBS Europe SE (Succursale Italia)")
    );
    assert_eq!(named(lh, "AF_Sender_Street_No").attr("_value"), Some("3"));
    assert_eq!(named(lh, "RB_Sender_ClientType").attr("_value"), Some("1"));
    assert_eq!(
        named(lh, "CB_Sender_Bold_CompanyName").attr("_value"),
        Some("1")
    );
    assert!(!hidden(named(lh, "PN_Receiver_Address")));
    assert_eq!(named(lh, "AF_Receiver_Street").attr("_value"), None);
    assert_eq!(
        named(lh, "RB_Receiver_ClientType").attr("_value"),
        Some("0")
    );

    // The subject and the reference line take their text by language; the
    // reference line the letter has not got stays off the form.
    let subject = named(lh, "AF_Subject");
    let init = subject
        .child("fd:scripts")
        .and_then(|s| s.attr("fd:init"))
        .expect("the subject's rule");
    assert!(
        init.contains("Oggetto: Recesso dal servizio") && init.contains("Subject: Withdrawal"),
        "{init}"
    );
    assert!(init.contains("getFormMetadata().language"), "{init}");
    assert_eq!(subject.attr("_value"), None);
    assert!(!hidden(named(lh, "TXT_Your_Ref")));
    assert!(hidden(named(lh, "TXT_Our_Ref")));
    let your_ref = named(lh, "TXT_Your_Ref").child("fd:scripts").unwrap();
    assert!(your_ref.attr("fd:init").unwrap().contains("Prot. n."));
    assert!(
        your_ref.attr("fd:valueCommit").is_some(),
        "the template's own rule stays"
    );

    // An unbound package binds nothing.
    assert!(!xml.contains("bindRef="));
    // The letterhead's headings read in the form's languages.
    assert!(dictionary(&build.package, "it").contains("&lt;p>&lt;b>Trasmettitore&lt;/b>&lt;/p>"));
}

/// The XML is a form AEM installs.
fn assert_valid_form(xml: &str) {
    u2s_aem_ubs_mcp::aem::validate_aem_form_xml(xml)
        .unwrap_or_else(|violations| panic!("the form is not valid: {violations:#?}"));
}

/// A form without a letterhead is no letter: no flag, no letter template, no
/// letterhead texts in its dictionaries.
#[test]
fn a_form_without_a_letterhead_is_no_letter() {
    let build = build(&document(json!([preface(), field()])));
    let xml = form_xml(&build.package);
    let form = parse_jcr_xml(&xml).unwrap();
    assert!(letterhead_of(&form).is_none());
    assert!(!xml.contains("redactoLayout"), "{xml}");
    assert!(!xml.contains("UBS_Blank_Letter_DoR"));
    assert!(xml.contains("UBS_General_Italy_DOR.xdp"));
    assert!(!dictionary(&build.package, "it").contains("Trasmettitore"));
}

/// In the bound package every field of the letterhead is bound to a path
/// under the one `Letter` element the schema references, and the type that
/// element has in UBS's own library declares that path.
#[test]
fn the_bound_letterhead_binds_exact_schema_paths() {
    let build = build(&letter());
    let bound = form_xml(
        build
            .bound_package
            .as_ref()
            .expect("the profile builds a bound twin"),
    );
    let form = parse_jcr_xml(&bound).unwrap();
    let lh = letterhead_of(&form).unwrap();
    let mut bound_fields = Vec::new();
    all(lh, &|n| n.attr("bindRef").is_some(), &mut bound_fields);
    assert!(bound_fields.len() > 25, "{}", bound_fields.len());

    let schema = build.xsd.expect("the profile writes the schema");
    assert_eq!(schema.matches("ref=\"Letter\"").count(), 1, "{schema}");
    assert!(schema.contains("LetterAddress.xsd"), "{schema}");

    let config = u2s_aem_ubs_mcp::profiles::load_xsd_config("ubs").unwrap();
    let letter_type = config
        .global_elements
        .get("Letter")
        .expect("Letter is a global element");
    for field in bound_fields {
        let path = field.attr("bindRef").unwrap();
        let (letter, rest) = path
            .split_once("/Letter/")
            .unwrap_or_else(|| panic!("{path} is not under Letter"));
        assert_eq!(letter, "/UBSAF_AAQR/Details", "{path}");
        let mut type_name = letter_type.clone();
        for step in rest.split('/') {
            let ty = config
                .registered_types
                .get(&type_name)
                .unwrap_or_else(|| panic!("{path}: no type {type_name}"));
            type_name = ty
                .elements
                .iter()
                .find(|e| e.name == step)
                .unwrap_or_else(|| panic!("{path}: {type_name} declares no {step}"))
                .type_ref
                .clone();
        }
    }
}

/// What a document says about its letterhead comes back from its package.
#[test]
fn a_letterhead_reads_back_from_its_package() {
    let value = letter();
    let doc = UbsAemDocument::from_json(&value).unwrap();
    let decoded = decode(&encode(&doc).unwrap().package).unwrap();
    assert_eq!(decoded_letterhead(&decoded), decoded_letterhead(&doc));

    // Without a sender, and with a fixed person as the receiver.
    let receiver = json!({"kind": "Fixed", "address": {
        "title": "Dott.", "first_name": "Mario", "last_name": "Rossi", "city": "Roma"
    }});
    let mut node = letterhead(Value::Null, receiver);
    node["our_reference"] = json!({"it": "Rif.", "en": "Our ref."});
    let doc = UbsAemDocument::from_json(&document(json!([preface(), node]))).unwrap();
    let decoded = decode(&encode(&doc).unwrap().package).unwrap();
    assert_eq!(decoded_letterhead(&decoded), decoded_letterhead(&doc));
}

/// The letter layout is the letterhead's, not something a package carries:
/// a letter whose letterhead is removed is no letter any more.
#[test]
fn the_letter_layout_is_not_carried_without_the_letterhead() {
    let decoded = decode(&build(&letter()).package).unwrap();
    let mut value = serde_json::to_value(&decoded).unwrap();
    assert!(!value.to_string().contains("redactoLayout"));
    let children = value["form"]["children"][0]["children"]
        .as_array_mut()
        .unwrap();
    children.retain(|c| c["type"] != "Letterhead");
    let xml = form_xml(&build(&value).package);
    assert!(!xml.contains("redactoLayout"));
    assert!(!xml.contains("UBS_Blank_Letter_DoR"));
}

/// AAQR_033's letterhead, the profile's own: both sides entered, no subject,
/// no reference lines.
#[test]
fn the_canonical_letterhead_reads_back() {
    let doc = decode(&fixture("AAQR_033.zip")).expect("AAQR_033 decodes");
    let Some(AemNodeTranslated::Letterhead {
        name,
        sender,
        receiver,
        subject,
        your_reference,
        our_reference,
        ..
    }) = decoded_letterhead(&doc)
    else {
        panic!("AAQR_033 holds a letterhead");
    };
    assert_eq!(name, "panel_letterhead");
    assert_eq!(sender, Some(LetterParty::Entered));
    assert_eq!(receiver, LetterParty::Entered);
    assert_eq!(subject, AemI18nText::default());
    assert_eq!((your_reference, our_reference), (None, None));
}

/// BATA, a letter AEM authors flagged `letterDor`: the bank as the sender,
/// in bold, and the subject and reference lines it prefills, in the form's
/// one language.
#[test]
fn a_flagged_letter_reads_back() {
    let doc = decode(&fixture("BATA.zip")).expect("BATA decodes");
    assert_eq!(doc.languages, ["it"]);
    let Some(AemNodeTranslated::Letterhead {
        sender,
        receiver,
        subject,
        your_reference,
        our_reference,
        ..
    }) = decoded_letterhead(&doc)
    else {
        panic!("BATA holds a letterhead");
    };
    let address = LetterAddress {
        company: Some("UBS Europe SE (Succursale Italia)".into()),
        bold_company: true,
        ..LetterAddress::default()
    };
    assert_eq!(
        sender,
        Some(LetterParty::Fixed {
            address: Box::new(address)
        })
    );
    assert_eq!(receiver, LetterParty::Entered);
    assert!(
        subject
            .get("it")
            .is_some_and(|s| s.starts_with("Oggetto: Esercizio del diritto di recesso")),
        "{subject:?}"
    );
    assert_eq!(your_reference, Some(text(&[("it", "Raccomandata A.R.")])));
    assert_eq!(our_reference, Some(text(&[("it", "Prot.")])));
    assert!(
        !serde_json::to_string(&doc)
            .unwrap()
            .contains("redactoLayout")
    );
}

#[test]
fn a_second_letterhead_is_refused() {
    let mut second = letterhead(bank(), json!({"kind": "Entered"}));
    second["uuid"] = json!("00000000-0000-4000-8000-0000000000c2");
    second["name"] = json!("PN_LetterHead2");
    let message = refusal(document(json!([
        preface(),
        letterhead(bank(), json!({"kind": "Entered"})),
        second
    ])));
    assert!(message.contains("at most one letterhead"), "{message}");
}

#[test]
fn a_letterhead_off_the_first_page_or_before_the_preface_is_refused() {
    let before = refusal(document(json!([
        letterhead(bank(), json!({"kind": "Entered"})),
        preface()
    ])));
    assert!(
        before.contains("direct child of the first page"),
        "{before}"
    );

    let mut value = document(json!([preface()]));
    value["form"]["children"][1]["children"] =
        json!([letterhead(bank(), json!({"kind": "Entered"}))]);
    let later = refusal(value);
    assert!(later.contains("direct child of the first page"), "{later}");
}

/// The sender is printed in `slot2`, so a header line there would hide it.
#[test]
fn a_slot2_draw_beside_a_sender_is_refused() {
    let draw = json!({
        "type": "TextDraw", "uuid": "00000000-0000-4000-8000-0000000000e1", "name": "ST_HeaderSlot2",
        "content": {"it": "<p>UBS Europe SE</p>", "en": "<p>UBS Europe SE</p>"},
        "visible": false, "colspan": 12, "dor_colspan": null,
        "always_in_pdf": true, "show_if_hidden": true, "summary_exclude": true, "dor_header_slot": "slot2"
    });
    let message = refusal(document(json!([
        preface(),
        draw.clone(),
        letterhead(bank(), json!({"kind": "Entered"}))
    ])));
    assert!(
        message.contains("`ST_HeaderSlot2`") && message.contains("slot2"),
        "{message}"
    );
    // Without a sender the slot is the header line's.
    build(&document(json!([
        preface(),
        draw,
        letterhead(Value::Null, json!({"kind": "Entered"}))
    ])));
}

/// The letter's header only prints when a slot has content.
#[test]
fn a_letter_with_nothing_in_its_header_is_refused() {
    let message = refusal(document(json!([letterhead(
        Value::Null,
        json!({"kind": "Entered"})
    )])));
    assert!(message.contains("no sender"), "{message}");
    build(&document(json!([letterhead(
        bank(),
        json!({"kind": "Entered"})
    )])));
}

#[test]
fn an_address_or_text_the_letter_cannot_print_is_refused() {
    let both = json!({"kind": "Fixed", "address": {"company": "UBS", "last_name": "Rossi"}});
    let message = refusal(document(json!([preface(), letterhead(bank(), both)])));
    assert!(message.contains("company and a person"), "{message}");

    let mut marked = letterhead(bank(), json!({"kind": "Entered"}));
    marked["subject"] = json!({"it": "<b>Oggetto</b>", "en": "Subject"});
    let message = refusal(document(json!([preface(), marked])));
    assert!(message.contains("markup"), "{message}");

    let mut unknown = letterhead(bank(), json!({"kind": "Entered"}));
    unknown["sender"]["address"]["zip"] = json!("20121");
    assert!(UbsAemDocument::from_json(&document(json!([preface(), unknown]))).is_err());
}

/// The letterhead's components as a JavaScript model a rule can run against:
/// each a global of its name holding `value`, `visible` and its children by
/// name, and `resetData()` emptying every field under it.
fn letterhead_model(node: &JcrNode) -> String {
    fn walk(node: &JcrNode, parent: Option<usize>, next: &mut usize, out: &mut String) {
        let mut own = parent;
        if let Some(name) = node.component_name() {
            let id = *next;
            *next += 1;
            let value = serde_json::to_string(&node.attr("_value")).unwrap();
            out.push_str(&format!("var n{id} = mk({value});\nvar {name} = n{id};\n"));
            if let Some(p) = parent {
                out.push_str(&format!(
                    "n{p}.kids.push(n{id});\nn{p}[\"{name}\"] = n{id};\n"
                ));
            }
            own = Some(id);
        }
        for child in &node.children {
            walk(child, own, next, out);
        }
    }
    let mut out = String::from(
        "function mk(value) { return {value: value, visible: true, kids: [], \
         resetData: function () { clear(this); }}; }\n\
         function clear(o) { o.kids.forEach(function (k) { if (k.kids.length === 0) { k.value = null; } clear(k); }); }\n",
    );
    walk(node, None, &mut 0, &mut out);
    out
}

/// The contents of `field`'s rules on `attribute`.
fn rules(field: &JcrNode, attribute: &str) -> Vec<String> {
    let Some(raw) = field.child("fd:scripts").and_then(|s| s.attr(attribute)) else {
        return Vec::new();
    };
    u2s_mapper_aem::script::decode_script_list(raw)
        .unwrap()
        .iter()
        .map(|json| {
            let rule: Value = serde_json::from_str(json).unwrap();
            rule["script"]["content"].as_str().unwrap().to_owned()
        })
        .collect()
}

/// The source prints a fixed address as it is, so nothing clears it: changing
/// an entered sender's person/company choice resets the sender's own panels
/// (in the shape the feedback guard expects of a configurator choice) and
/// leaves the fixed receiver's prefilled values as they are. A fixed side's
/// choice carries no reset at all.
#[test]
fn a_person_company_change_resets_only_its_own_side() {
    let receiver = json!({"kind": "Fixed", "address": {
        "first_name": "Mario", "last_name": "Rossi", "street": "Via Roma", "city": "Roma"
    }});
    let build = build(&document(json!([
        preface(),
        letterhead(json!({"kind": "Entered"}), receiver)
    ])));
    let form = parse_jcr_xml(&form_xml(&build.package)).unwrap();
    let lh = letterhead_of(&form).unwrap();

    let receiver_choice = named(lh, "RB_Receiver_ClientType");
    assert!(rules(receiver_choice, "fd:valueCommit").is_empty());
    assert_eq!(
        rules(receiver_choice, "fd:init"),
        ["AF_Receiver_ClientType.value = this.value;"]
    );

    let sender_choice = named(lh, "RB_Sender_ClientType");
    let commits = rules(sender_choice, "fd:valueCommit");
    assert_eq!(commits.len(), 2, "{commits:?}");
    assert_eq!(
        commits[1],
        "// [configurator-reset-on-change] Generated automatically. Do not edit: will be overwritten. \
         Create your own different script.\nPN_Sender_Company.resetData();\nPN_Sender_Names.resetData();"
    );

    let mut source = letterhead_model(lh);
    source
        .push_str("AF_Sender_LastName.value = \"Bianchi\";\nRB_Sender_ClientType.value = \"1\";\n");
    for rule in &commits {
        source.push_str(&format!(
            "(function () {{\n{rule}\n}}).call(RB_Sender_ClientType);\n"
        ));
    }
    source.push_str(
        "JSON.stringify([AF_Receiver_FirstName.value, AF_Receiver_LastName.value, AF_Receiver_Street.value, \
         AF_Receiver_City.value, AF_Sender_LastName.value, PN_Sender_Company.visible, PN_Sender_Names.visible, \
         AF_Sender_ClientType.value])",
    );
    let mut context = boa_engine::Context::default();
    let result = context
        .eval(boa_engine::Source::from_bytes(source.as_bytes()))
        .unwrap_or_else(|e| panic!("the rules run: {e}"));
    let state: Value = serde_json::from_str(
        &result
            .to_string(&mut context)
            .unwrap()
            .to_std_string_escaped(),
    )
    .unwrap();
    assert_eq!(
        state,
        json!(["Mario", "Rossi", "Via Roma", "Roma", null, true, false, "1"])
    );
}
