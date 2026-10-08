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
        .join("../u2s/crates/u2s-aem-ubs-mcp/tests/fixtures/golden")
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
    let expected_label_findings = [("AAOS_033_IT", 7), ("AAEV_019_EN", 0), ("AABF_019", 63)];
    for (form, labels) in expected_label_findings {
        for rule in rules() {
            let outcome = check(&rule, &golden(form));
            let expected = if rule == "input-labels" { labels } else { 0 };
            assert_eq!(
                outcome.violations.len(),
                expected,
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
