//! The UBS AEM rules in `rules/aem/`, run over real documents in the rules
//! sandbox. The documents are the golden ones the vendored UBS layer keeps
//! for its own parity tests.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use u2s_rules::{CheckOutcome, ScriptBudget, run_check};

fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rules/aem")
}

fn golden(form: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vendor/crates/u2s-aem-ubs-mcp/tests/fixtures/golden")
        .join(form)
        .join("document.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn check(rule: &str, doc: &Value) -> CheckOutcome {
    let script = std::fs::read_to_string(rules_dir().join(rule).join("check.js")).unwrap();
    run_check(
        &script,
        doc,
        &json!({}),
        &Map::new(),
        &ScriptBudget::default(),
    )
    .unwrap_or_else(|e| panic!("{rule} breaks: {e:?}"))
}

/// The scripted rules: the rule directories with a `check.js` (a judged rule
/// has none, and an agent decides it).
fn rules() -> Vec<String> {
    let mut rules: Vec<String> = std::fs::read_dir(rules_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|dir| dir.join("check.js").is_file())
        .map(|dir| dir.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    rules.sort();
    rules
}

/// The old engine's output passes every rule but the label check, which finds
/// the inputs it left without a label: AABF_019's seven unlabelled date pickers
/// and bare radio buttons, and AAOS_033_IT's contribution text areas. Its own
/// review reported the same inputs as `missing`.
#[test]
fn the_golden_documents_pass_every_rule_but_the_labels_they_lack() {
    // What the retired engine's output still breaks, which these rules found in it:
    // - `input-labels`: the inputs it left without a label (see above);
    // - `text-languages-complete`: AABF_019's `TTL_AccountHolder` heading, which it wrote in
    //   English only into a form that ships German and Spanish too (two findings, one per language);
    // - `repeatable-title`: AAOS_033_IT's `RCP_df94f111`, which has no title, no heading above it
    //   and an enclosing panel title too long to be a subject, so its package ships the
    //   placeholder `(Repeatable name)`.
    let known = |form: &str, rule: &str| match (form, rule) {
        ("AAOS_033_IT", "input-labels") => 7,
        ("AABF_019", "input-labels") => 63,
        ("AABF_019", "text-languages-complete") => 2,
        ("AAOS_033_IT", "repeatable-title") => 1,
        _ => 0,
    };
    for form in ["AAOS_033_IT", "AAEV_019_EN", "AABF_019"] {
        for rule in rules() {
            let outcome = check(&rule, &golden(form));
            assert_eq!(
                outcome.violations.len(),
                known(form, &rule),
                "{form} {rule}: {:?}",
                outcome.violations.iter().take(3).collect::<Vec<_>>()
            );
        }
    }
}

/// AAEV_019_EN with one edit applied at a JSON pointer.
fn aaev_with(pointer: &str, value: Value) -> Value {
    let mut doc = golden("AAEV_019_EN");
    *doc.pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer} exists")) = value;
    doc
}

/// The first panel under the first page, and its first child, in AAEV_019_EN.
fn first_panel(doc: &Value) -> String {
    let pages = doc["form"]["children"].as_array().unwrap();
    let (i, _) = pages
        .iter()
        .enumerate()
        .find(|(_, p)| p["type"] == "Panel")
        .unwrap();
    format!("/form/children/{i}")
}

fn violations(rule: &str, doc: &Value) -> Vec<String> {
    check(rule, doc)
        .violations
        .into_iter()
        .map(|v| v.pointer)
        .collect()
}

fn messages(rule: &str, doc: &Value) -> Vec<String> {
    check(rule, doc)
        .violations
        .into_iter()
        .map(|v| v.message)
        .collect()
}

#[test]
fn a_panel_without_its_prefix_is_named_wrong() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(&format!("{page}/name"), json!("Section1"));
    assert_eq!(
        violations("naming-prefix", &doc),
        vec![format!("{page}/name")]
    );
}

#[test]
fn a_field_with_another_types_prefix_is_named_wrong() {
    let field = json!({
        "type": "DatePicker", "uuid": "00000000-0000-0000-0000-000000000001", "name": "TXT_Birth",
        "label": {"en": "Date of birth"}, "mandatory": false, "visible": true, "colspan": 12,
        "dor_colspan": null, "bind_ref": null
    });
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(&format!("{page}/children"), json!([field]));
    assert_eq!(
        violations("naming-prefix", &doc),
        vec![format!("{page}/children/0/name")]
    );
    // A missing language counts as an empty label.
    let mut doc = doc;
    doc["languages"] = json!(["en", "de"]);
    assert_eq!(
        violations("input-labels", &doc),
        vec![format!("{page}/children/0/label")]
    );
}

#[test]
fn sibling_inputs_with_one_label_are_reported_together() {
    let date = |name: &str| {
        json!({
            "type": "DatePicker", "uuid": "00000000-0000-0000-0000-000000000001", "name": name,
            "label": {"en": "Date"}, "mandatory": false, "visible": true, "colspan": 12,
            "dor_colspan": null, "bind_ref": null
        })
    };
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(
        &format!("{page}/children"),
        json!([date("DATE_A"), date("DATE_B")]),
    );
    assert_eq!(
        violations("duplicate-sibling-labels", &doc),
        vec![
            format!("{page}/children/0/label/en"),
            format!("{page}/children/1/label/en")
        ]
    );
}

#[test]
fn a_parenthetical_or_marked_up_label_is_not_a_label() {
    let field = |label: &str| {
        json!({
            "type": "TextField", "uuid": "00000000-0000-0000-0000-000000000001", "name": "TXT_A",
            "label": {"en": label}, "mandatory": false, "visible": true, "max_chars": null,
            "colspan": 12, "dor_colspan": null, "bind_ref": null, "kind": "Plain"
        })
    };
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    for label in ["(optional)", "<b>Name</b>"] {
        let doc = aaev_with(&format!("{page}/children"), json!([field(label)]));
        assert_eq!(
            violations("input-labels", &doc),
            vec![format!("{page}/children/0/label/en")],
            "{label}"
        );
    }
}

#[test]
fn a_retired_market_fragment_is_reported_and_a_kept_family_is_not() {
    let fragment = |frag_ref: &str| {
        json!({
            "type": "Fragment", "uuid": "00000000-0000-0000-0000-000000000001", "name": "PN_Person",
            "title": {"en": ""}, "frag_ref": frag_ref, "visible": true, "bind_ref": null
        })
    };
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let retired = "/content/dam/formsanddocuments/afforms_germany_fragmentlib/affrg_germany_Person";
    let kept =
        "/content/dam/formsanddocuments/afforms_germany_fragmentlib/affrg_germany_InternalBankUse";
    let doc = aaev_with(
        &format!("{page}/children"),
        json!([fragment(retired), fragment(kept)]),
    );
    assert_eq!(
        violations("retired-market-fragments", &doc),
        vec![format!("{page}/children/0/frag_ref")]
    );
}

/// The internal-bank-use block is the global fragment in every market; a
/// market one is the shape the corpus migrated away from.
#[test]
fn a_market_internal_bank_use_fragment_is_reported_and_the_global_one_is_not() {
    let fragment = |frag_ref: &str| {
        json!({
            "type": "Fragment", "uuid": "00000000-0000-0000-0000-000000000001",
            "name": "PN_FRG_InternalBankUseOnly", "title": {"en": ""}, "frag_ref": frag_ref,
            "visible": false, "bind_ref": null
        })
    };
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let market =
        "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref";
    let global = "/content/dam/formsanddocuments/afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature";
    let doc = aaev_with(
        &format!("{page}/children"),
        json!([fragment(market), fragment(global)]),
    );
    assert_eq!(
        violations("global-internal-bank-use", &doc),
        vec![format!("{page}/children/0/frag_ref")]
    );
}

#[test]
fn a_panel_of_static_text_named_as_a_table_is_a_legacy_table() {
    let draw = json!({
        "type": "TextDraw", "uuid": "00000000-0000-0000-0000-000000000001", "name": "ST_Cell",
        "content": {"en": "<p>1</p>"}, "visible": true, "colspan": 12, "dor_colspan": null
    });
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let mut doc = aaev_with(&format!("{page}/name"), json!("TBL_Fees"));
    *doc.pointer_mut(&format!("{page}/children")).unwrap() = json!([draw]);
    assert_eq!(violations("legacy-table-panels", &doc), vec![page]);
}

#[test]
fn a_visual_editor_rule_in_passthrough_is_reported() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    // An empty passthrough is not serialized, so the edit adds it.
    let mut doc = base.clone();
    doc.pointer_mut(&page).unwrap()["passthrough"] = json!({
        "raw_children": ["<fd:rules fd:visible=\"[...]\" jcr:primaryType=\"nt:unstructured\"/>"]
    });
    assert_eq!(
        violations("visual-editor-rules", &doc),
        vec![format!("{page}/passthrough/raw_children/0")]
    );
}

/// Every rule loads, and the rules compiled into the crate are exactly the
/// rule directories.
#[test]
fn the_compiled_rules_are_the_rule_directories() {
    let read = |slug: &str, name: &str| std::fs::read_to_string(rules_dir().join(slug).join(name)).ok();
    let files = rules()
        .iter()
        .map(|slug| u2s_doc_tools::rules_dir::RuleFiles {
            rule_toml: read(slug, "rule.toml").unwrap(),
            check_js: read(slug, "check.js").unwrap(),
            fix_js: read(slug, "fix.js"),
            slug: slug.clone(),
        })
        .collect();
    let from_dir = u2s_doc_tools::rules_dir::load_rules(files).expect("the rules load");
    let compiled = u2s_doc_tools::rules_dir::load_rules(agent::rules::rule_files(agent::OutputTarget::Aem))
        .expect("the compiled rules load");
    assert_eq!(from_dir.len(), rules().len());
    let ids = |rules: &[u2s_doc_tools::native::RuleForCheck]| {
        rules
            .iter()
            .map(|r| (r.id, r.script_js.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&compiled), ids(&from_dir));
}

#[test]
fn a_calculate_rule_is_a_visual_editor_rule_too() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let mut doc = base.clone();
    // AEM's calculate property is `fd:calc`; the second `fd:rules` counts too.
    doc.pointer_mut(&page).unwrap()["passthrough"] = json!({
        "raw_children": ["<x><fd:rules jcr:primaryType=\"nt:unstructured\"/><fd:rules fd:calc=\"[...]\"/></x>"]
    });
    assert_eq!(
        violations("visual-editor-rules", &doc),
        vec![format!("{page}/passthrough/raw_children/0")]
    );
}

#[test]
fn a_bare_prefix_is_not_a_prefixed_name() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(&format!("{page}/name"), json!("PN"));
    assert_eq!(
        violations("naming-prefix", &doc),
        vec![format!("{page}/name")]
    );
}

#[test]
fn angle_brackets_that_are_not_tags_are_not_markup() {
    let field = json!({
        "type": "TextField", "uuid": "00000000-0000-0000-0000-000000000001", "name": "TXT_A",
        "label": {"en": "Amount < 100 > 10"}, "mandatory": false, "visible": true, "max_chars": null,
        "colspan": 12, "dor_colspan": null, "bind_ref": null, "kind": "Plain"
    });
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(&format!("{page}/children"), json!([field]));
    assert!(violations("input-labels", &doc).is_empty());
}

/// A checkbox may leave its label to its option, but only in a language the
/// option is written in.
/// The precedence over verbatim-source-text: an input the source gives no caption still has a
/// label, the nearest source text without its brackets, and the check says where to find it.
#[test]
fn an_input_without_a_source_caption_takes_the_nearest_source_text() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(&format!("{page}/children"), json!([text_field("TXTM_Statement", "", None)]));
    let said = messages("input-labels", &doc);
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("nearest source text"), "{}", said[0]);

    // The option's text with its brackets dropped is a label; the bracketed text is not.
    let doc = aaev_with(
        &format!("{page}/children"),
        json!([text_field("TXTM_Statement", "s. unten stehende Erklärung", None)]),
    );
    assert!(violations("input-labels", &doc).is_empty());
    let doc = aaev_with(
        &format!("{page}/children"),
        json!([text_field("TXTM_Statement", "(s. unten stehende Erklärung)", None)]),
    );
    assert_eq!(violations("input-labels", &doc), vec![format!("{page}/children/0/label/en")]);
    assert!(!messages("input-labels", &doc)[0].contains("nearest source text"));
}

#[test]
fn a_checkbox_option_speaks_only_for_its_own_language() {
    let checkbox = json!({
        "type": "Checkbox", "uuid": "00000000-0000-0000-0000-000000000001", "name": "CB_Agree",
        "label": {}, "options": [{"label": {"en": "I agree"}, "value": "1"}], "alignment": "Vertical",
        "visible": true, "colspan": 12, "dor_colspan": null, "conditions": [], "bind_ref": null
    });
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let mut doc = aaev_with(&format!("{page}/children"), json!([checkbox]));
    assert!(violations("input-labels", &doc).is_empty());
    doc["languages"] = json!(["en", "de"]);
    assert_eq!(
        violations("input-labels", &doc),
        vec![format!("{page}/children/0/label")]
    );
}

// ---- The rules that stand for the feedback guard's problems (specs/feedback/rule-coverage.md) ----

const UUID: &str = "00000000-0000-0000-0000-000000000001";

fn panel(name: &str, title: &str, is_page: bool, is_conditional: bool, children: Vec<Value>) -> Value {
    json!({
        "type": "Panel", "uuid": UUID, "name": name, "title": {"en": title}, "children": children,
        "is_page": is_page, "visible": true, "is_conditional": is_conditional, "dor_num_cols": null,
        "colspan": 12, "dor_colspan": null, "bind_ref": null
    })
}

fn page(name: &str, title: &str, children: Vec<Value>) -> Value {
    panel(name, title, true, false, children)
}

fn text_field(name: &str, label: &str, kind: Option<&str>) -> Value {
    let mut field = json!({
        "type": "TextField", "uuid": UUID, "name": name, "label": {"en": label}, "mandatory": false,
        "visible": true, "max_chars": null, "colspan": 12, "dor_colspan": null, "bind_ref": null
    });
    if let Some(kind) = kind {
        field["kind"] = json!(kind);
    }
    field
}

fn number_field(name: &str, label: &str) -> Value {
    json!({
        "type": "NumberField", "uuid": UUID, "name": name, "label": {"en": label}, "mandatory": false,
        "visible": true, "colspan": 12, "dor_colspan": null, "bind_ref": null
    })
}

fn draw(kind: &str, name: &str, text: &str) -> Value {
    let mut node = json!({
        "type": kind, "uuid": UUID, "name": name, "content": {"en": text}, "visible": true,
        "colspan": 12, "dor_colspan": null
    });
    if kind == "TitleDraw" {
        node["heading_level"] = json!(2);
    }
    node
}

fn fragment(name: &str, frag_ref: &str) -> Value {
    json!({
        "type": "Fragment", "uuid": UUID, "name": name, "title": {"en": ""}, "frag_ref": frag_ref,
        "visible": true, "bind_ref": null
    })
}

fn radio(name: &str, options: &[&str], shown: &[&str]) -> Value {
    json!({
        "type": "RadioButton", "uuid": UUID, "name": name, "label": {"en": "Who is it for?"},
        "options": options.iter().enumerate()
            .map(|(i, o)| json!({"label": {"en": o}, "value": i.to_string()})).collect::<Vec<_>>(),
        "alignment": "Vertical", "mandatory": true, "visible": true, "colspan": 12,
        "dor_colspan": null, "bind_ref": null,
        "conditions": shown.iter().enumerate()
            .map(|(i, p)| json!({"target_panel_name": p, "value": {"type": "text", "value": i.to_string()}, "show": true}))
            .collect::<Vec<_>>()
    })
}

/// AAEV_019_EN with its pages replaced; the document still has to be one the UBS layer reads.
fn doc_with(pages: Vec<Value>) -> Value {
    let mut doc = golden("AAEV_019_EN");
    doc["form"]["children"] = Value::Array(pages);
    u2s_aem_ubs_mcp::UbsAemDocument::from_json(&doc).expect("the edited document is a document");
    doc
}

/// The same, with an edit applied to the node at `pointer` after the document is built.
fn set(mut doc: Value, pointer: &str, key: &str, value: Value) -> Value {
    doc.pointer_mut(pointer).unwrap_or_else(|| panic!("{pointer} exists"))[key] = value;
    u2s_aem_ubs_mcp::UbsAemDocument::from_json(&doc).expect("the edited document is a document");
    doc
}

#[test]
fn an_email_or_telephone_label_on_a_plain_field_names_the_kind() {
    let doc = doc_with(vec![page(
        "PN_Contact",
        "Contact",
        vec![
            text_field("TXT_Mail", "E-Mail address", Some("Plain")),
            text_field("TXT_Phone", "Telefonnummer / Phone", None),
            number_field("NB_Mobile", "Mobile number"),
            text_field("TXT_Order", "Telefonische Bestellung", Some("Plain")),
            text_field("TXT_Fine", "Mail address confirmation", Some("Plain")),
            text_field("EML_Already", "Email", Some("Email")),
            text_field("TXTM_Fax", "Fax", Some("Multiline")),
        ],
    )]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("email-phone-kind", &doc),
        vec![format!("{p}/0/kind"), format!("{p}/1"), format!("{p}/2")]
    );
    let fixed = doc_with(vec![page(
        "PN_Contact",
        "Contact",
        vec![
            text_field("EML_Mail", "E-Mail address", Some("Email")),
            text_field("TEL_Phone", "Telefonnummer / Phone", Some("Telephone")),
            text_field("TEL_Mobile", "Mobile number", Some("Telephone")),
        ],
    )]);
    assert!(violations("email-phone-kind", &fixed).is_empty());
}

#[test]
fn an_unlabelled_field_is_classified_by_its_name_and_only_the_master_label_counts() {
    // No label: the name, split on `_` and on camel humps.
    let doc = doc_with(vec![page(
        "PN_Contact",
        "Contact",
        vec![
            text_field("TXT_NumeroDiTelefonoFisso_eafeddae", "", Some("Plain")),
            text_field("TXT_Amountimobili", "", Some("Plain")),
        ],
    )]);
    assert_eq!(
        violations("email-phone-kind", &doc),
        vec!["/form/children/0/children/0/kind".to_string()]
    );
    // The guard reads the master language's label: English here, so the German one is not read.
    let mut doc = doc_with(vec![page("PN_Contact", "Contact", vec![text_field("TXT_Contact", "Contact", None)])]);
    doc["languages"] = json!(["en", "de"]);
    doc["form"]["children"][0]["children"][0]["label"]["de"] = json!("Telefon");
    assert!(violations("email-phone-kind", &doc).is_empty());
    // Where the form ships no English, the first language is the master.
    doc["languages"] = json!(["de", "it"]);
    doc["form"]["children"][0]["children"][0]["label"] = json!({"de": "Telefon", "it": "Telefono"});
    assert_eq!(
        violations("email-phone-kind", &doc),
        vec!["/form/children/0/children/0".to_string()]
    );
}

/// A configurator choice and its two panels, each holding a field.
fn configurator(second_panel: Value) -> Vec<Value> {
    vec![page(
        "PN_Config",
        "Configurator",
        vec![
            radio("RB_Type", &["Individual", "Legal entity"], &["PN_Individual", "PN_Entity"]),
            panel("PN_Individual", "", false, true, vec![text_field("TXT_Name", "Name", None)]),
            second_panel,
        ],
    )]
}

#[test]
fn a_configurator_choice_must_drive_two_resettable_conditional_panels() {
    let entity = || panel("PN_Entity", "", false, true, vec![text_field("TXT_Company", "Company", None)]);
    assert!(violations("configurator-panels", &doc_with(configurator(entity()))).is_empty());
    let p = "/form/children/0/children";

    // A panel that is not conditional has no visibility script to tie the reset to.
    let doc = doc_with(configurator(panel(
        "PN_Entity", "", false, false, vec![text_field("TXT_Company", "Company", None)],
    )));
    assert_eq!(violations("configurator-panels", &doc), vec![format!("{p}/2/is_conditional")]);

    // A panel of static text only has nothing to clear.
    let doc = doc_with(configurator(panel(
        "PN_Entity", "", false, true, vec![draw("TextDraw", "ST_Info", "Please note")],
    )));
    assert_eq!(violations("configurator-panels", &doc), vec![format!("{p}/2")]);

    // The reset names the panel, so another node with the same name breaks it.
    let mut pages = configurator(entity());
    pages[0]["children"].as_array_mut().unwrap().push(text_field("PN_Entity", "Clash", None));
    assert_eq!(violations("configurator-panels", &doc_with(pages)), vec![format!("{p}/2/name")]);

    // A choice naming only one panel is not wired as a configurator.
    let doc = doc_with(configurator(entity()));
    let doc = set(doc, &format!("{p}/0"), "conditions", json!([
        {"target_panel_name": "PN_Individual", "value": {"type": "text", "value": "0"}, "show": true}
    ]));
    assert_eq!(violations("configurator-panels", &doc), vec![format!("{p}/0")]);

    // A choice inside a driven panel that itself decides panels would be reset with it.
    let inner = radio("RB_Inner", &["Yes", "No"], &["PN_Yes", "PN_No"]);
    let pages = vec![page(
        "PN_Config",
        "Configurator",
        vec![
            radio("RB_Type", &["Individual", "Legal entity"], &["PN_Individual", "PN_Entity"]),
            panel("PN_Individual", "", false, true, vec![inner]),
            entity(),
            panel("PN_Yes", "", false, true, vec![text_field("TXT_Yes", "Yes", None)]),
            panel("PN_No", "", false, true, vec![text_field("TXT_No", "No", None)]),
        ],
    )];
    assert_eq!(violations("configurator-panels", &doc_with(pages)), vec![format!("{p}/1/children/0")]);

    // Other wordings are ordinary questions and are left alone.
    let pages = vec![page(
        "PN_Config",
        "Order",
        vec![radio("RB_Kind", &["Buy", "Sell"], &["PN_A"])],
    )];
    assert!(violations("configurator-panels", &doc_with(pages)).is_empty());
}

#[test]
fn a_configurator_with_markup_in_its_options_is_still_recognised() {
    let mut pages = configurator(panel(
        "PN_Entity", "", false, false, vec![text_field("TXT_Company", "Company", None)],
    ));
    pages[0]["children"][0]["options"][0]["label"]["en"] = json!("<b>Individual</b>");
    assert_eq!(
        violations("configurator-panels", &doc_with(pages)),
        vec!["/form/children/0/children/2/is_conditional".to_string()]
    );
}

#[test]
fn a_step_is_a_page_and_its_heading_is_its_title() {
    let body = || draw("TextDraw", "ST_Body", "Some text");
    // A panel directly under the form that is not a page.
    let doc = doc_with(vec![panel("PN_Loose", "Loose", false, false, vec![body()])]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/0/is_page".to_string()]);

    // An untitled page that starts with a heading, or holds a heading among other content.
    let doc = doc_with(vec![page(
        "PN_A", "", vec![draw("TitleDraw", "TTL_A", "Account"), body()],
    )]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/0/children/0".to_string()]);
    let doc = doc_with(vec![page(
        "PN_A", "", vec![body(), draw("TitleDraw", "TTL_A", "Account"), body()],
    )]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/0/children/1".to_string()]);

    // The same heading as the page's title, an untitled page of body text, a heading alone in its
    // own panel and the first page's subtitle are all fine.
    let mut subtitle = draw("TextDraw", "ST_Subtitle", "Subtitle");
    subtitle["css"] = json!("subtitle-after-form-title");
    let doc = doc_with(vec![
        page("PN_A", "Account", vec![body()]),
        page("PN_C", "", vec![panel("PN_CTitle", "", false, false, vec![draw("TitleDraw", "TTL_C", "C")]), body()]),
        page("PN_D", "", vec![subtitle, draw("TitleDraw", "TTL_D", "D"), body()]),
    ]);
    assert!(violations("step-titles", &doc).is_empty(), "{:?}", violations("step-titles", &doc));

    // An untitled, headless panel under the form has nothing to render as a step title, and the
    // special panels are not steps: neither is reported.
    let doc = doc_with(vec![
        panel("PN_Loose", "", false, false, vec![body()]),
        panel("PN_Preview", "Preview", false, false, vec![body()]),
    ]);
    assert!(violations("step-titles", &doc).is_empty(), "{:?}", violations("step-titles", &doc));

    let marked = |name: &str, css: &str| {
        let mut heading = draw("TitleDraw", name, "Heading");
        heading["css"] = json!(css);
        heading
    };
    // On a titled page the writer marks its own heading, so an authored marker is a second one.
    let doc = doc_with(vec![page("PN_A", "Account", vec![marked("TTL_Marked", "foo stepTitle"), body()])]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/0/children/0/css".to_string()]);

    // On an untitled page the step title is its first level-2 heading: marked and alone in its
    // own panel is the step title written by hand, and fine; a marker on a later heading is not.
    let doc = doc_with(vec![page(
        "PN_A",
        "",
        vec![panel("PN_ATitle", "", false, false, vec![marked("TTL_A", "stepTitle")]), body()],
    )]);
    assert!(violations("step-titles", &doc).is_empty(), "{:?}", violations("step-titles", &doc));
    let doc = doc_with(vec![page(
        "PN_A",
        "",
        vec![
            panel("PN_ATitle", "", false, false, vec![draw("TitleDraw", "TTL_A", "A")]),
            body(),
            marked("TTL_Section", "stepTitle"),
        ],
    )]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/0/children/2/css".to_string()]);
}

#[test]
fn a_banking_relationship_fragment_is_the_canonical_one_and_pn_br_has_its_margin() {
    let canonical = "/content/forms/af/afforms_ubs_fragmentlib/affrg_BankingRelationship1";
    let market = "/content/dam/formsanddocuments/afforms_germany_fragmentlib/affrg_BankingRelationship1";
    let custody = "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_CustodyAccountBankingRelationship";
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            fragment("PN_FRG_Banking", market),
            fragment("PN_FRG_Banking2", canonical),
            fragment("PN_FRG_Custody", custody),
            panel("PN_BR", "", false, false, vec![fragment("PN_FRG_Banking3", canonical)]),
        ],
    )]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("banking-relationship", &doc),
        vec![format!("{p}/0/frag_ref"), format!("{p}/3")]
    );
    // The guard wants the class alone, not among others.
    let doc = set(doc, &format!("{p}/3"), "css", json!("ubs-margin-20 other"));
    let doc = set(doc, &format!("{p}/0"), "frag_ref", json!(canonical));
    assert_eq!(violations("banking-relationship", &doc), vec![format!("{p}/3/css")]);
    let doc = set(doc, &format!("{p}/3"), "css", json!("ubs-margin-20"));
    assert!(violations("banking-relationship", &doc).is_empty());

    // A `Preface` is how the prompt has the Author build the block, and its template writes the
    // margin: named `PN_BR`, it is not a hand-built wrapper.
    let preface = json!({"type": "Preface", "uuid": UUID, "name": "PN_BR"});
    let doc = doc_with(vec![page("PN_A", "A", vec![preface])]);
    assert!(violations("banking-relationship", &doc).is_empty());
}

#[test]
fn numbered_signature_blocks_are_a_family_and_one_repeatable_is_not() {
    let sign = |name: &str| {
        panel(name, "", false, false, vec![text_field("TXT_SignatureName", "Name", None)])
    };
    let doc = doc_with(vec![page("PN_A", "A", vec![sign("PN_Signature1"), sign("PN_Signature2")])]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("signature-family", &doc),
        vec![format!("{p}/0/name"), format!("{p}/1/name")]
    );
    // Fragments of a signature family number the same way.
    let generic = "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1";
    let doc = doc_with(vec![page(
        "PN_A", "A", vec![fragment("PN_SGN_1", generic), fragment("PN_SGN_2", generic)],
    )]);
    assert_eq!(
        violations("signature-family", &doc),
        vec![format!("{p}/0/name"), format!("{p}/1/name")]
    );
    // Not counted from 1, or a single block: no family.
    let doc = doc_with(vec![page("PN_A", "A", vec![sign("PN_Signature1"), sign("PN_Signature3")])]);
    assert!(violations("signature-family", &doc).is_empty());
    let one = json!({
        "type": "Repeatable", "uuid": UUID, "name": "RCP_SGN_Client", "title": {"en": "Client"},
        "children": [fragment("PN_SGN_Client", generic)], "min_occur": 2, "max_occur": 2,
        "visible": true, "bind_ref": null
    });
    assert!(violations("signature-family", &doc_with(vec![page("PN_A", "A", vec![one])])).is_empty());
}

#[test]
fn a_hidden_infobox_copy_must_reach_the_pdf_from_the_last_page() {
    let infobox = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";
    let mut copy = fragment("PN_ItalyInfoboxDoR", infobox);
    copy["visible"] = json!(false);
    let on_screen = fragment("PN_Infobox", infobox);
    let doc = doc_with(vec![
        page("PN_A", "A", vec![on_screen.clone(), copy.clone()]),
        page("PN_B", "B", vec![draw("TextDraw", "ST_B", "Text")]),
    ]);
    let p = "/form/children/0/children/1";
    assert_eq!(violations("infobox-dor-copy", &doc), vec![p.to_string(), p.to_string()]);
    let doc = set(doc, p, "dor_exclude", json!(true));
    assert_eq!(
        violations("infobox-dor-copy", &doc),
        vec![p.to_string(), format!("{p}/dor_exclude"), p.to_string()]
    );

    // The shape the normalize pass writes, on the last page.
    copy["always_in_pdf"] = json!(true);
    copy["summary_exclude"] = json!(true);
    let doc = doc_with(vec![
        page("PN_A", "A", vec![on_screen]),
        page("PN_B", "B", vec![draw("TextDraw", "ST_B", "Text"), copy]),
    ]);
    assert!(violations("infobox-dor-copy", &doc).is_empty());
}

#[test]
fn what_a_loaded_package_carries_in_passthrough_is_checked() {
    let raw = |attrs: Value, children: Value| json!({"raw_attributes": attrs, "raw_children": children});
    let mut date = json!({
        "type": "DatePicker", "uuid": UUID, "name": "DATE_A", "label": {"en": "Date"},
        "mandatory": false, "visible": true, "colspan": 12, "dor_colspan": null, "bind_ref": null
    });
    date["passthrough"] = raw(json!({"defaultToCurrentDate": "true", "excludeFromDoRIfHidden": "true"}), json!([]));
    let mut footnote = json!({"type": "FootnotePlaceholder", "uuid": UUID, "name": "FN_A", "colspan": 12});
    footnote["passthrough"] = raw(json!({"dorExclusion": "true"}), json!([]));
    let mut hidden = text_field("TXT_B", "B", None);
    hidden["passthrough"] = raw(json!({}), json!([
        "<fd:scripts fd:visible=\"[{&quot;script&quot;:{}}]\" jcr:primaryType=\"nt:unstructured\"/>",
        "<panel_x sling:resourceType=\"fd/af/components/panel\" jcr:primaryType=\"nt:unstructured\"/>",
        "<x dorExclusion=\"true\" name=\"X\"/>",
    ]));
    let doc = doc_with(vec![page("PN_A", "A", vec![date, footnote, hidden])]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("passthrough-attributes", &doc),
        vec![
            format!("{p}/0/passthrough/raw_attributes/excludeFromDoRIfHidden"),
            format!("{p}/0/passthrough/raw_attributes/defaultToCurrentDate"),
            format!("{p}/1/passthrough/raw_attributes/dorExclusion"),
            format!("{p}/2/passthrough/raw_children/0"),
            format!("{p}/2/passthrough/raw_children/1"),
            format!("{p}/2/passthrough/raw_children/2"),
        ]
    );
    // The shapes that are fine: an Initialize rule beside the visibility rule, both exclusions.
    let mut fine = text_field("TXT_B", "B", None);
    fine["passthrough"] = raw(json!({"defaultToCurrentDate": "false"}), json!([
        "<fd:scripts fd:visible=\"[{&quot;script&quot;:{}}]\" fd:init=\"[{&quot;script&quot;:{}}]\"/>",
        "<x dorExclusion=\"true\" summaryExclusion=\"true\" name=\"X\"/>",
    ]));
    assert!(violations("passthrough-attributes", &doc_with(vec![page("PN_A", "A", vec![fine])])).is_empty());
}

#[test]
fn the_form_code_and_entity_must_be_well_formed() {
    let doc = golden("AAEV_019_EN");
    let doc = aaev_with_variables(&doc, "aaev", "19");
    assert_eq!(
        violations("variables", &doc),
        vec!["/variables/formrange_code".to_string(), "/variables/formrange_entity".to_string()]
    );
    let mut missing = golden("AAEV_019_EN");
    missing["variables"].as_object_mut().unwrap().remove("formrange_entity");
    assert_eq!(violations("variables", &missing), vec!["/variables".to_string()]);
    assert!(violations("variables", &aaev_with_variables(&golden("AAEV_019_EN"), "AAEV", "019")).is_empty());
    assert!(violations("variables", &aaev_with_variables(&golden("AAEV_019_EN"), "BA1", "001")).is_empty());
}

fn aaev_with_variables(doc: &Value, code: &str, entity: &str) -> Value {
    let mut doc = doc.clone();
    doc["variables"]["formrange_code"] = json!(code);
    doc["variables"]["formrange_entity"] = json!(entity);
    doc
}

#[test]
fn footnote_references_and_the_placeholder_go_together() {
    let reference = "<p>Fee<span data-af-footnote-id=\"a\"><sup>#</sup></span></p>";
    let note = || json!({"type": "FootnotePlaceholder", "uuid": UUID, "name": "FN_A", "colspan": 12});
    let doc = doc_with(vec![page("PN_A", "A", vec![draw("TextDraw", "ST_A", reference)])]);
    assert_eq!(
        violations("footnotes", &doc),
        vec!["/form/children/0/children/0/content/en".to_string()]
    );
    let doc = doc_with(vec![page("PN_A", "A", vec![draw("TextDraw", "ST_A", reference), note()])]);
    assert!(violations("footnotes", &doc).is_empty());
    let doc = doc_with(vec![page("PN_A", "A", vec![draw("TextDraw", "ST_A", "<p>Fee</p>"), note()])]);
    assert_eq!(violations("footnotes", &doc), vec!["/form/children/0/children/1".to_string()]);

    // A reference in the header counts as much as one in the form.
    let mut doc = doc;
    doc["header"] = json!(reference);
    assert!(violations("footnotes", &doc).is_empty(), "{:?}", violations("footnotes", &doc));

    // A form still on accordion footnotes is the guard's other branch, left alone.
    let mut accordion = draw("TextDraw", "ST_Notes", "<p>1 Note</p>");
    accordion["css"] = json!("ubsAccordionFootnote");
    let doc = doc_with(vec![page("PN_A", "A", vec![draw("TextDraw", "ST_A", reference), accordion])]);
    assert!(violations("footnotes", &doc).is_empty());
}

// ---- The rules that stand for what the prompts used to ask for ----

fn repeatable(name: &str, title: &str, children: Vec<Value>) -> Value {
    json!({
        "type": "Repeatable", "uuid": UUID, "name": name, "title": {"en": title}, "children": children,
        "min_occur": 1, "max_occur": 4, "visible": true, "bind_ref": null
    })
}

fn dropdown(name: &str, values: &[&str]) -> Value {
    json!({
        "type": "Dropdown", "uuid": UUID, "name": name, "label": {"en": "Choice"},
        "options": values.iter().map(|v| json!({"label": {"en": "Option"}, "value": v})).collect::<Vec<_>>(),
        "mandatory": false, "visible": true, "colspan": 12, "dor_colspan": null, "conditions": [],
        "bind_ref": null
    })
}

fn html(name: &str, markup: &str) -> Value {
    json!({
        "type": "HtmlDisplayer", "uuid": UUID, "name": name, "content": {"en": markup},
        "visible": true, "colspan": 12, "dor_colspan": null
    })
}

fn preface() -> Value {
    json!({"type": "Preface", "uuid": UUID, "name": "PN_BR"})
}

#[test]
fn a_text_is_written_in_every_language_of_the_form_and_no_other() {
    let doc = |label_de: Value, extra: Value| {
        let mut name = text_field("TXT_Name", "Name", None);
        name["label"] = json!({"en": "Name", "de": label_de});
        let mut city = text_field("TXT_City", "City", None);
        city["label"] = json!({"en": "City", "de": "Ort", "fr": extra});
        let mut a_page = page("PN_A", "Account", vec![name, city, draw("TextDraw", "ST_Note", "Note")]);
        a_page["title"] = json!({"en": "Account", "de": "Konto"});
        let mut doc = doc_with(vec![a_page]);
        doc["languages"] = json!(["en", "de"]);
        doc
    };
    let p = "/form/children/0/children";
    // A blank entry, an unlisted language, and a text with no entry for the second language.
    assert_eq!(
        violations("text-languages-complete", &doc(json!(" "), json!("Ville"))),
        vec![format!("{p}/0/label/de"), format!("{p}/1/label/fr"), format!("{p}/2/content")]
    );
    let mut fixed = doc(json!("Name"), json!(""));
    fixed["form"]["children"][0]["children"][1]["label"].as_object_mut().unwrap().remove("fr");
    fixed["form"]["children"][0]["children"][2]["content"] = json!({"en": "Note", "de": "Hinweis"});
    assert!(violations("text-languages-complete", &fixed).is_empty());
    // A page title missing a language is reported at the title; a text empty in every language is not.
    let mut untitled = fixed.clone();
    untitled["form"]["children"][0]["title"] = json!({"en": "Account"});
    untitled["form"]["children"][0]["children"][2]["content"] = json!({});
    assert_eq!(
        violations("text-languages-complete", &untitled),
        vec!["/form/children/0/title".to_string()]
    );
}

#[test]
fn html_that_does_not_survive_into_the_document_is_reported() {
    let table = "<table><thead><tr><th>A</th></tr></thead><tbody><tr><td>1</td></tr></tbody></table>";
    let image = "<img src=\"data:image/png;base64,AAAA\" alt=\"x\">";
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            html("TBL_Fees", &format!("{table}<script>alert(1)</script>")),
            html("TBL_Loose", "<p>one</p><p>two</p>"),
            html("IMG_Logo", "<img src=\"https://example.com/logo.png\">"),
            html("CRT_Pie", "<p>none</p>"),
            html("TBL_Form", &format!("{table}<input type=\"text\">")),
            html("TBL_Ok", table),
            html("IMG_Ok", image),
            html("CRT_Ok", "<svg viewBox=\"0 0 1 1\"><circle r=\"1\"/></svg>"),
            html("IMG_Link", &format!("{image}<a href=\"javascript:void(0)\">x</a>")),
        ],
    )]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("html-displayer-markup", &doc),
        vec![
            format!("{p}/0/content/en"),
            format!("{p}/1/content/en"),
            format!("{p}/2/content/en"),
            format!("{p}/2/content/en"),
            format!("{p}/3/content/en"),
            format!("{p}/4/content/en"),
            format!("{p}/8/content/en"),
        ]
    );
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![html("TBL_Ok", table), html("IMG_Ok", image), html("ST_Text", "<p>Anything</p>")],
    )]);
    assert!(violations("html-displayer-markup", &doc).is_empty());
}

#[test]
fn a_node_name_is_used_once_but_the_party_names_repeat() {
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            text_field("TXT_Name", "Name", None),
            panel("PN_Inner", "", false, false, vec![text_field("TXT_Name", "Name again", None)]),
            text_field("TXT_Name", "A third", None),
            fragment("PN_CPGRP", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1"),
            fragment("PN_CPGRP", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1"),
            repeatable("RCP_SGN_CPGRP", "Client", vec![]),
            repeatable("RCP_SGN_CPGRP", "Client", vec![]),
        ],
    )]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("unique-node-names", &doc),
        vec![format!("{p}/1/children/0/name"), format!("{p}/2/name")]
    );
}

#[test]
fn only_the_forms_first_level_panels_are_pages() {
    let body = || draw("TextDraw", "ST_Body", "Some text");
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![panel("PN_Inner", "Inner", true, false, vec![body()]), body()],
    )]);
    assert_eq!(
        violations("step-titles", &doc),
        vec!["/form/children/0/children/0/is_page".to_string()]
    );
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![panel("PN_Inner", "Inner", false, false, vec![body()]), body()],
    )]);
    assert!(violations("step-titles", &doc).is_empty());
}

#[test]
fn a_heading_is_rendered_once() {
    let mut subtitle = draw("TextDraw", "ST_Subtitle", "Account");
    subtitle["css"] = json!("subtitle-after-form-title");
    let doc = doc_with(vec![page(
        "PN_A",
        "Account",
        vec![
            draw("TitleDraw", "TTL_Copy", "<b>Account:</b>"),
            panel("PN_Inner", "", false, false, vec![draw("TextDraw", "ST_Copy", "account")]),
            draw("TextDraw", "ST_One", "Same"),
            draw("TextDraw", "ST_Two", "Same"),
            draw("TextDraw", "ST_Three", "Different"),
            subtitle,
        ],
    )]);
    let p = "/form/children/0/children";
    // The draw repeating its neighbour comes first, then the two repeating the page's title.
    assert_eq!(
        violations("headings-rendered-once", &doc),
        vec![format!("{p}/3"), format!("{p}/0"), format!("{p}/1/children/0")]
    );
    let doc = doc_with(vec![page(
        "PN_A",
        "Account",
        vec![
            draw("TextDraw", "ST_One", "Same"),
            draw("TextDraw", "ST_Two", "Other"),
            draw("TitleDraw", "TTL_Section", "Fees"),
        ],
    )]);
    assert!(violations("headings-rendered-once", &doc).is_empty());
}

#[test]
fn the_banking_relationship_preface_is_there_once_on_the_first_page() {
    let body = || draw("TextDraw", "ST_Body", "Some text");
    let ok = doc_with(vec![page("PN_A", "A", vec![preface(), body()]), page("PN_B", "B", vec![body()])]);
    assert!(violations("banking-relationship-present", &ok).is_empty());

    let none = doc_with(vec![page("PN_A", "A", vec![body()])]);
    assert_eq!(violations("banking-relationship-present", &none), vec!["/form".to_string()]);

    let twice = doc_with(vec![page("PN_A", "A", vec![preface(), preface()])]);
    assert_eq!(
        violations("banking-relationship-present", &twice),
        vec!["/form/children/0/children/1".to_string()]
    );

    let late = doc_with(vec![page("PN_A", "A", vec![body()]), page("PN_B", "B", vec![preface()])]);
    assert_eq!(
        violations("banking-relationship-present", &late),
        vec!["/form/children/1/children/0".to_string()]
    );

    let mut entity = draw("TextDraw", "ST_Entity", "<p><b>UBS Europe SE</b></p>");
    entity["content"]["de"] = json!("ubs europe se");
    let line = doc_with(vec![page("PN_A", "A", vec![preface(), entity, draw("TextDraw", "ST_Other", "UBS Europe SE, Milan")])]);
    assert_eq!(
        violations("banking-relationship-present", &line),
        vec![
            "/form/children/0/children/1/content/de".to_string(),
            "/form/children/0/children/1/content/en".to_string()
        ]
    );
}

/// The precedence over sub-headings-are-title-draws: "UBS Europe SE" heading the bank's
/// signature block is that signature Repeatable's title, never a draw, and the check says so.
#[test]
fn the_banks_signature_heading_is_its_repeatables_title_not_a_draw() {
    let signature = || fragment("PN_SGN_UBS", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1");
    let titled = doc_with(vec![
        page("PN_A", "A", vec![preface()]),
        page("PN_SIG", "Signature(s)", vec![repeatable("RCP_SGN_UBS", "UBS Europe SE", vec![signature()])]),
    ]);
    assert!(violations("banking-relationship-present", &titled).is_empty());

    let drawn = doc_with(vec![
        page("PN_A", "A", vec![preface()]),
        page(
            "PN_SIG",
            "Signature(s)",
            vec![
                draw("TitleDraw", "TTL_UBSEuropeSE", "UBS Europe SE"),
                repeatable("RCP_SGN_UBS", "", vec![signature()]),
            ],
        ),
    ]);
    assert_eq!(
        violations("banking-relationship-present", &drawn),
        vec!["/form/children/1/children/0/content/en".to_string()]
    );
    let said = messages("banking-relationship-present", &drawn);
    assert!(said[0].contains("signature Repeatable's `title`"), "{}", said[0]);
}

#[test]
fn a_repeatable_is_named_once_and_something_names_it() {
    let p = "/form/children/0/children";
    // Nothing names it: no title, no heading above it, a page title too long to be a subject.
    let doc = doc_with(vec![page(
        "PN_A",
        "A long page title with many words",
        vec![repeatable("RCP_Row", "", vec![text_field("TXT_A", "A", None)])],
    )]);
    assert_eq!(violations("repeatable-title", &doc), vec![format!("{p}/0/title")]);
    // A title equal to the node's own name is the converter's "nothing names this".
    let doc = doc_with(vec![page(
        "PN_A",
        "A long page title with many words",
        vec![repeatable("RCP_Row", "RCP_Row", vec![text_field("TXT_A", "A", None)])],
    )]);
    assert_eq!(violations("repeatable-title", &doc), vec![format!("{p}/0/title")]);
    // The page's title, a heading right above it, or its own title name it.
    for children in [
        vec![repeatable("RCP_Row", "", vec![])],
        vec![draw("TitleDraw", "TTL_Rows", "Rows"), repeatable("RCP_Row", "", vec![])],
        vec![repeatable("RCP_Row", "Row", vec![])],
    ] {
        let title = if children.len() == 1 && children[0]["title"]["en"] == "" { "Rows" } else { "A long page title with many words" };
        let doc = doc_with(vec![page("PN_A", title, children)]);
        assert!(violations("repeatable-title", &doc).is_empty(), "{:?}", violations("repeatable-title", &doc));
    }
    // A sentence is no subject.
    let doc = doc_with(vec![page(
        "PN_A",
        "A long page title with many words",
        vec![repeatable("RCP_Row", "Please list every person who holds the account.", vec![])],
    )]);
    assert_eq!(violations("repeatable-title", &doc), vec![format!("{p}/0/title")]);
    // A heading inside that names the row names it twice; one for another thing does not.
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![repeatable(
            "RCP_Client",
            "Client",
            vec![
                draw("TitleDraw", "TTL_Row", "Client 1"),
                draw("TitleDraw", "TTL_Same", "client"),
                draw("TitleDraw", "TTL_Other", "Clients of the bank"),
                repeatable("RCP_Nested", "Child", vec![draw("TitleDraw", "TTL_Child", "Client 2")]),
            ],
        )],
    )]);
    assert_eq!(
        violations("repeatable-title", &doc),
        vec![format!("{p}/0/children/0/content/en"), format!("{p}/0/children/1/content/en")]
    );
}

#[test]
fn a_hand_written_repeatable_rule_is_reported_and_the_templates_own_is_not() {
    let template = "<fd:scripts fd:click=\"[{&quot;script&quot;:{&quot;content&quot;:&quot;// [repeating-panel] Generated automatically. \
        window.forms.ubs.addInstance(this.parent.RCP_A)&quot;},&quot;_archetype&quot;:&quot;repeating-panel&quot;}]\" \
        fd:visible=\"[{&quot;script&quot;:{&quot;content&quot;:&quot;this.parent.RCP_A.instanceManager.instances.length &quot;},&quot;_archetype&quot;:&quot;repeating-panel&quot;}]\"/>";
    let by_hand = "<fd:scripts fd:click=\"[{&quot;script&quot;:{&quot;content&quot;:&quot;this.parent.RCP_A.instanceManager.addInstance()&quot;}}]\"/>";
    let mut repeating = repeatable("RCP_A", "A", vec![text_field("TXT_A", "A", None)]);
    repeating["passthrough"] = json!({"raw_children": [template, by_hand], "raw_attributes": {"x": "instanceManager.minOccur"}});
    let doc = doc_with(vec![page("PN_A", "A", vec![repeating])]);
    let p = "/form/children/0/children/0/passthrough";
    assert_eq!(
        violations("repeatable-no-handwritten-rules", &doc),
        vec![format!("{p}/raw_attributes/x"), format!("{p}/raw_children/1")]
    );
    let mut clean = repeatable("RCP_A", "A", vec![text_field("TXT_A", "A", None)]);
    clean["passthrough"] = json!({"raw_children": [template]});
    assert!(violations("repeatable-no-handwritten-rules", &doc_with(vec![page("PN_A", "A", vec![clean])])).is_empty());
}

#[test]
fn a_condition_needs_a_conditional_panel_and_an_option_to_match() {
    let p = "/form/children/0/children";
    let target = |name: &str, conditional: bool| {
        panel(name, "", false, conditional, vec![text_field(&format!("TXT_{name}"), "Field", None)])
    };
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![radio("RB_Type", &["One", "Two"], &["PN_One", "PN_Two"]), target("PN_One", true), target("PN_Two", false)],
    )]);
    // The second target is not conditional, so the writer gives it no visibility hook.
    assert_eq!(violations("condition-targets", &doc), vec![format!("{p}/2/is_conditional")]);

    // A name that no node has, a fragment, a name two nodes share, and a value no option has.
    let mut choice = radio("RB_Type", &["One", "Two"], &["PN_Gone", "PN_Frag"]);
    choice["conditions"].as_array_mut().unwrap().push(json!({
        "target_panel_name": "PN_Dup", "value": {"type": "text", "value": "9"}, "show": true
    }));
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            choice,
            fragment("PN_Frag", "/content/dam/formsanddocuments/afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature"),
            target("PN_Dup", true),
            panel("PN_Dup", "", false, true, vec![text_field("TXT_Other", "Other", None)]),
        ],
    )]);
    assert_eq!(
        violations("condition-targets", &doc),
        vec![
            format!("{p}/0/conditions/0/target_panel_name"),
            format!("{p}/0/conditions/1/target_panel_name"),
            format!("{p}/0/conditions/2/target_panel_name"),
            format!("{p}/0/conditions/2/value"),
        ]
    );
    // A matching value on a wired panel.
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![radio("RB_Type", &["One", "Two"], &["PN_One", "PN_Two"]), target("PN_One", true), target("PN_Two", true)],
    )]);
    assert!(violations("condition-targets", &doc).is_empty());
    let doc = set(doc, &format!("{p}/0/conditions/1"), "value", json!({"type": "text", "value": "7"}));
    assert_eq!(violations("condition-targets", &doc), vec![format!("{p}/0/conditions/1/value")]);
}

#[test]
fn a_node_kept_for_the_pdf_does_not_drop_itself_with_dor_exclude() {
    let mut copy = fragment("PN_Copy", "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox");
    copy["always_in_pdf"] = json!(true);
    copy["summary_exclude"] = json!(true);
    copy["dor_exclude"] = json!(true);
    let mut hidden = draw("TextDraw", "ST_Hidden", "Hidden");
    hidden["dor_exclude"] = json!(true);
    let doc = doc_with(vec![page("PN_A", "A", vec![copy, hidden])]);
    assert_eq!(
        violations("placement-flags", &doc),
        vec!["/form/children/0/children/0/dor_exclude".to_string()]
    );
    let doc = set(doc, "/form/children/0/children/0", "dor_exclude", json!(false));
    assert!(violations("placement-flags", &doc).is_empty());
}

#[test]
fn the_edit_button_is_not_set_where_the_writer_or_the_summary_has_no_use_for_it() {
    let flagged = |mut node: Value| {
        node["jump_to_field"] = json!(true);
        node
    };
    let field = || text_field("TXT_A", "A", None);
    let body = || draw("TextDraw", "ST_Body", "Some text");
    let p = "/form/children/0/children";
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            flagged(panel("PN_FormConfigurator", "", false, false, vec![field()])),
            flagged(panel("PN_Text", "", false, false, vec![body()])),
            flagged(panel("PN_Fields", "", false, false, vec![field()])),
            flagged(panel("PN_Rows", "", false, false, vec![repeatable("RCP_Row", "Row", vec![field()])])),
        ],
    )]);
    assert_eq!(
        violations("jump-to-field-placement", &doc),
        vec![format!("{p}/0/jump_to_field"), format!("{p}/1/jump_to_field")]
    );
    // A titled page carries the writer's button on its step-title panel already; an untitled one
    // keeps the only one it has.
    let titled = doc_with(vec![flagged(page("PN_Page", "Declaration", vec![field()]))]);
    assert_eq!(
        violations("jump-to-field-placement", &titled),
        vec!["/form/children/0/jump_to_field".to_string()]
    );
    let untitled = doc_with(vec![flagged(page("PN_Page", "", vec![field()]))]);
    assert!(violations("jump-to-field-placement", &untitled).is_empty());
}

#[test]
fn a_column_width_is_on_the_twelve_column_grid() {
    let mut wide = text_field("TXT_Wide", "Wide", None);
    wide["colspan"] = json!(13);
    let mut none = text_field("TXT_None", "None", None);
    none["colspan"] = json!(0);
    let mut dor = text_field("TXT_Dor", "Dor", None);
    dor["dor_colspan"] = json!(14);
    let mut half = text_field("TXT_Half", "Half", None);
    half["colspan"] = json!(6);
    half["dor_colspan"] = json!(12);
    let doc = doc_with(vec![page("PN_A", "A", vec![wide, none, dor, half])]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("grid-widths", &doc),
        vec![format!("{p}/0/colspan"), format!("{p}/1/colspan"), format!("{p}/2/dor_colspan")]
    );
}

#[test]
fn an_option_has_a_value_of_its_own() {
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![dropdown("DD_Blank", &["a", " ", "c"]), dropdown("DD_Twice", &["a", "b", "a"]), dropdown("DD_Fine", &["1", "2"])],
    )]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("options-wellformed", &doc),
        vec![format!("{p}/0/options/1/value"), format!("{p}/1/options/2/value")]
    );
}

#[test]
fn a_partner_generic_is_wrapped_in_a_repeatable() {
    let generic = "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1";
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            fragment("PN_CPG", generic),
            repeatable("RCP_AHGRP", "Representative", vec![fragment(
                "PN_AHGRP",
                "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_PartnertoPartnerGeneric1",
            )]),
            fragment("PN_Address", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_AddressGeneric1"),
        ],
    )]);
    assert_eq!(
        violations("account-holder-wiring", &doc),
        vec!["/form/children/0/children/0".to_string()]
    );
    let fixed = doc_with(vec![page(
        "PN_A",
        "A",
        vec![repeatable("RCP_CPGRP", "Client", vec![fragment("PN_CPGRP", generic)])],
    )]);
    assert!(violations("account-holder-wiring", &fixed).is_empty());
}

/// The canonical language script of `ubs-aem-language-specific-content`, showing the node in
/// the languages `codes` lists.
fn language_gate(codes: &[&str]) -> String {
    let test = codes
        .iter()
        .map(|c| format!("language.indexOf(\"{c}\") !== -1"))
        .collect::<Vec<_>>()
        .join(" || ");
    format!(
        "var language = (window.forms.ubs.getFormMetadata().language || \"\").toLowerCase();\n\
         if ({test}) {{\n    window.forms.ubs.showAFShowDor(this);\n    this.visible = true;\n\
         }} else {{\n    window.forms.ubs.hideAFHideDor(this);\n    this.visible = false;\n}}"
    )
}

/// An `fd:scripts` element carrying `content` as the Initialize script of `field`, escaped as
/// a package carries it: JSON, then JCR's backslash escapes, then XML.
fn fd_init(field: &str, content: &str) -> String {
    let models = json!([{
        "script": {
            "field": field, "event": "Initialize",
            "model": {"nodeName": "EVENT_SCRIPTS"}, "content": content
        },
        "nodeName": "SCRIPTMODEL", "version": 1, "enabled": true
    }]);
    let jcr = models.to_string().replace('\\', "\\\\").replace(',', "\\,");
    let xml = jcr
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!("<fd:scripts fd:init=\"{xml}\" jcr:primaryType=\"nt:unstructured\"/>")
}

/// AABF_019 (German, English, Spanish) with the node at `pointer` authored hidden and gated
/// by `content`.
fn aabf_gated(pointer: &str, content: &str) -> Value {
    let mut doc = golden("AABF_019");
    let node = doc.pointer_mut(pointer).unwrap();
    let name = node["name"].as_str().unwrap().to_string();
    node["visible"] = json!(false);
    node["passthrough"] = json!({ "raw_children": [fd_init(&name, content)] });
    doc
}

/// `PN_Execution_bc1928b0`: a panel no choice shows.
const UNCONDITIONED: &str = "/form/children/2";
/// `PN_e58dc1d5`: a panel the configurator shows.
const CONDITIONED: &str = "/form/children/0/children/3";

#[test]
fn a_canonical_language_script_passes() {
    let doc = aabf_gated(UNCONDITIONED, &language_gate(&["de"]));
    assert_eq!(violations("language-specific-content", &doc), Vec::<String>::new());
    let doc = aabf_gated(UNCONDITIONED, &language_gate(&["de", "es", "sp"]));
    assert_eq!(violations("language-specific-content", &doc), Vec::<String>::new());
}

#[test]
fn a_language_script_that_does_not_lower_case_is_reported() {
    let script = language_gate(&["de"]).replace(".toLowerCase()", "");
    let doc = aabf_gated(UNCONDITIONED, &script);
    assert_eq!(
        violations("language-specific-content", &doc),
        vec![format!("{UNCONDITIONED}/passthrough/raw_children/0")]
    );
}

#[test]
fn a_language_the_form_does_not_ship_is_reported() {
    let doc = aabf_gated(UNCONDITIONED, &language_gate(&["fr"]));
    assert_eq!(
        violations("language-specific-content", &doc),
        vec![format!("{UNCONDITIONED}/passthrough/raw_children/0")]
    );
}

#[test]
fn spanish_tested_under_one_code_only_is_reported() {
    let doc = aabf_gated(UNCONDITIONED, &language_gate(&["es"]));
    assert_eq!(
        violations("language-specific-content", &doc),
        vec![format!("{UNCONDITIONED}/passthrough/raw_children/0")]
    );
}

#[test]
fn a_language_script_without_the_dor_calls_is_reported() {
    let script = language_gate(&["de"])
        .replace("window.forms.ubs.showAFShowDor(this);", "")
        .replace("window.forms.ubs.hideAFHideDor(this);", "");
    let doc = aabf_gated(UNCONDITIONED, &script);
    assert_eq!(
        violations("language-specific-content", &doc),
        vec![format!("{UNCONDITIONED}/passthrough/raw_children/0")]
    );
}

#[test]
fn a_language_gated_node_authored_visible_is_reported() {
    let mut doc = aabf_gated(UNCONDITIONED, &language_gate(&["de"]));
    doc.pointer_mut(UNCONDITIONED).unwrap()["visible"] = json!(true);
    assert_eq!(
        violations("language-specific-content", &doc),
        vec![format!("{UNCONDITIONED}/visible")]
    );
}

#[test]
fn a_language_script_on_a_condition_target_is_reported() {
    let doc = aabf_gated(CONDITIONED, &language_gate(&["de"]));
    assert_eq!(
        violations("language-specific-content", &doc),
        vec![CONDITIONED.to_string()]
    );
}

#[test]
fn a_script_that_does_not_read_the_language_is_not_judged() {
    let mut doc = aabf_gated(UNCONDITIONED, "this.visible = false;");
    doc.pointer_mut(UNCONDITIONED).unwrap()["visible"] = json!(true);
    assert_eq!(violations("language-specific-content", &doc), Vec::<String>::new());
}

/// The script reaches the package as the node's one `fd:scripts` child, in the form AEM reads.
#[test]
fn a_language_script_is_written_into_the_package() {
    use std::io::Read;
    let doc = aabf_gated(UNCONDITIONED, &language_gate(&["de"]));
    let doc = u2s_aem_ubs_mcp::UbsAemDocument::from_json(&doc).unwrap();
    let build = u2s_aem_ubs_mcp::encode(&doc).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(build.package)).unwrap();
    let xml = (0..archive.len())
        .map(|i| {
            let mut xml = String::new();
            archive.by_index(i).unwrap().read_to_string(&mut xml).unwrap();
            xml
        })
        .find(|xml| xml.contains("guideContainer"))
        .expect("the package holds the form");
    let start = xml.find("name=\"PN_Execution_bc1928b0\"").expect("the gated panel is written");
    let element = &xml[xml[..start].rfind('<').unwrap()..];
    let tag = element[1..].split(|c: char| c.is_whitespace() || c == '>').next().unwrap();
    let end = element.find(&format!("</{tag}>")).expect("the panel closes");
    let panel = &element[..end];
    // What follows the panel's own `items` is the panel's own children: layout, responsive, and
    // what its passthrough carries.
    let own = &panel[panel.rfind("</items>").expect("the panel has items")..];
    assert_eq!(own.matches("<fd:scripts").count(), 1, "{own}");
    assert!(own.contains("getFormMetadata().language"), "{own}");
    assert!(own.contains("&quot;event&quot;:&quot;Initialize&quot;"), "{own}");
}

/// A static text draw in AAEV_019_EN's first panel, at the given width.
fn text_draw(colspan: u64) -> Value {
    json!({
        "type": "TextDraw", "uuid": "00000000-0000-0000-0000-000000000002", "name": "ST_Column",
        "content": {"en": "<p>Left column</p>"}, "visible": true, "colspan": colspan, "dor_colspan": null
    })
}

#[test]
fn a_half_width_text_is_a_text_column() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let doc = aaev_with(&format!("{page}/children"), json!([text_draw(6)]));
    assert_eq!(
        violations("single-column-text", &doc),
        vec![format!("{page}/children/0/colspan")]
    );
    let fixed = aaev_with(&format!("{page}/children"), json!([text_draw(12)]));
    assert!(violations("single-column-text", &fixed).is_empty());
    let mut dor = text_draw(12);
    dor["dor_colspan"] = json!(6);
    let doc = aaev_with(&format!("{page}/children"), json!([dor]));
    assert_eq!(
        violations("single-column-text", &doc),
        vec![format!("{page}/children/0/dor_colspan")]
    );
}

#[test]
fn a_narrow_panel_of_text_is_a_text_column() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let column = json!({
        "type": "Panel", "uuid": "00000000-0000-0000-0000-000000000003", "name": "PN_Column",
        "visible": true, "colspan": 6, "dor_colspan": null, "children": [text_draw(12)]
    });
    let doc = aaev_with(&format!("{page}/children"), json!([column]));
    assert_eq!(
        violations("single-column-text", &doc),
        vec![format!("{page}/children/0")]
    );
    // A narrow panel that also holds a field is a field layout, not a text column.
    let mut mixed = column;
    mixed["children"].as_array_mut().unwrap().push(json!({
        "type": "TextField", "uuid": "00000000-0000-0000-0000-000000000005", "name": "TF_Name",
        "label": {"en": "Name"}, "visible": true, "colspan": 12, "dor_colspan": null
    }));
    let doc = aaev_with(&format!("{page}/children"), json!([mixed]));
    assert!(violations("single-column-text", &doc).is_empty());
}

#[test]
fn html_in_css_columns_is_multi_column_text() {
    let base = golden("AAEV_019_EN");
    let page = first_panel(&base);
    let html = |content: &str| {
        json!([{
            "type": "HtmlDisplayer", "uuid": "00000000-0000-0000-0000-000000000004", "name": "HTML_Terms",
            "content": {"en": content}, "visible": true, "colspan": 12, "dor_colspan": null
        }])
    };
    let doc = aaev_with(&format!("{page}/children"), html("<div style=\"column-count: 2\"><p>Terms</p></div>"));
    assert_eq!(
        violations("single-column-text", &doc),
        vec![format!("{page}/children/0/content/en")]
    );
    for clean in [
        "<div style=\"display: grid; grid-template-columns: 1fr 1fr\"><p>Terms</p></div>",
        "<p>Columns: name, date</p>",
    ] {
        let doc = aaev_with(&format!("{page}/children"), html(clean));
        assert!(violations("single-column-text", &doc).is_empty(), "{clean}");
    }
}
