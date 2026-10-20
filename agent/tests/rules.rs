//! The UBS AEM rules in `rules/aem/`, run over real documents in the rules
//! sandbox. The documents are the golden ones the vendored UBS layer keeps
//! for its own parity tests.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
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

/// The old engine's output passes every rule but those that find what it got
/// wrong; each finding below was looked at and is a real defect of that output.
#[test]
fn the_golden_documents_pass_every_rule_but_the_defects_they_carry() {
    // What the retired engine's output still breaks, which these rules found in it:
    // - `input-labels`: the inputs it left without a label: AABF_019's seven unlabelled date
    //   pickers and bare radio buttons, and AAOS_033_IT's contribution text areas; its own review
    //   reported the same inputs as `missing`;
    // - `text-languages-complete`: AABF_019's `TTL_AccountHolder` heading and its party and
    //   signature Repeatables' titles, written in English only into a form that ships German and
    //   Spanish too (one finding per language);
    // - `form-title`: AABF_019's form title, in English only;
    // - `repeatable-title`: AAOS_033_IT's `RCP_df94f111` and AABF_019's `RCP_69344a5b`, with no
    //   usable title, and AAOS_033_IT's `RCP_Sign_AHGRP` titled with an optional plural
    //   ("Legale/i rappresentante/i");
    // - `configurator-panels`: the configurator choices `RB_GroupTipo` and `RB_FormularAdressat`,
    //   vertical where a configurator is a horizontal radio row;
    // - `options-wellformed`: options keyed by the source's own values (`true`, `RB_1`) instead
    //   of "1" to "n" (UBS-GEN-3);
    // - `grid-widths`: inputs one column wide, under the three an input takes;
    // - `headings-rendered-once`: level-4 sub-headings right under a page's level-2 heading;
    // - `issuer-information`: AAOS_033_IT, an entity-033 form, without the Italy infobox;
    // - `label-visibility`: AAOS_033_IT's radio groups labelled with a 339-character sentence;
    // - `quotes`: AAEV_019_EN quoting 'grantor' and 'simple' with straight single quotes;
    // - `dates`: AABF_019's month-and-year date pickers;
    // - `signatures`: AABF_019's signature fragments outside a Repeatable.
    let known = |form: &str, rule: &str| match (form, rule) {
        ("AAOS_033_IT", "configurator-panels") => 1,
        ("AAOS_033_IT", "grid-widths") => 2,
        ("AAOS_033_IT", "headings-rendered-once") => 1,
        ("AAOS_033_IT", "input-labels") => 7,
        ("AAOS_033_IT", "issuer-information") => 1,
        ("AAOS_033_IT", "label-visibility") => 2,
        ("AAOS_033_IT", "options-wellformed") => 11,
        ("AAOS_033_IT", "repeatable-title") => 2,
        ("AAEV_019_EN", "quotes") => 1,
        ("AABF_019", "configurator-panels") => 1,
        ("AABF_019", "dates") => 4,
        ("AABF_019", "form-title") => 2,
        ("AABF_019", "grid-widths") => 1,
        ("AABF_019", "headings-rendered-once") => 5,
        ("AABF_019", "input-labels") => 63,
        ("AABF_019", "options-wellformed") => 25,
        ("AABF_019", "repeatable-title") => 1,
        ("AABF_019", "signatures") => 3,
        ("AABF_019", "text-languages-complete") => 16,
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
    assert_eq!(violations("tables-are-html-tables", &doc), vec![page]);
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
            check_js: Some(read(slug, "check.js").unwrap()),
            fix_js: read(slug, "fix.js"),
            slug: slug.clone(),
        })
        .collect();
    let from_dir = u2s_doc_tools::rules_dir::load_rules(files).expect("the rules load");
    let compiled = u2s_doc_tools::rules_dir::load_rules(agent::rules::rule_files())
        .expect("the compiled rules load");
    assert_eq!(from_dir.len(), rules().len());
    let ids = |rules: &[u2s_doc_tools::native::RuleForCheck]| {
        rules
            .iter()
            .map(|r| (r.id, r.check.clone()))
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

/// The configurator band as the form's first page: a horizontal choice and
/// the two panels it shows, each holding a field, then `second_page`.
fn configurator(second_page: Value) -> Vec<Value> {
    let mut choice = radio("RB_Type", &["Individual", "Legal entity"], &["PN_Individual", "PN_Entity"]);
    choice["alignment"] = json!("Horizontal");
    vec![
        page(
            "PN_FormConfigurator",
            "Form configurator",
            vec![
                choice,
                panel("PN_Individual", "", false, true, vec![text_field("TXT_Name", "Name", None)]),
                panel("PN_Entity", "", false, true, vec![text_field("TXT_Company", "Company", None)]),
            ],
        ),
        second_page,
    ]
}

/// The band is the first page (UBS-CFG-2), its choices are horizontal radio
/// rows (backlog "Configurator rendering"), and its reset is the writer's
/// (UBS-CFG-4).
#[test]
fn the_configurator_is_one_band_on_the_first_page() {
    let next = || page("PN_Next", "Next", vec![text_field("TXT_Next", "Next", None)]);
    assert!(violations("configurator-panels", &doc_with(configurator(next()))).is_empty());
    let p = "/form/children/0/children";

    // A dropdown, or a vertical radio row, is no configurator choice.
    let doc = set(doc_with(configurator(next())), &format!("{p}/0"), "alignment", json!("Vertical"));
    assert_eq!(violations("configurator-panels", &doc), vec![format!("{p}/0/alignment")]);

    // An option's panel is shown, never hidden, by its condition.
    let doc = doc_with(configurator(next()));
    let doc = set(doc, &format!("{p}/0/conditions/1"), "show", json!(false));
    assert_eq!(violations("configurator-panels", &doc), vec![format!("{p}/0/conditions/1")]);

    // A second band, on a later page.
    let band = panel("PN_FormConfigurator2", "", false, false, vec![text_field("TXT_B", "B", None)]);
    let doc = doc_with(configurator(page("PN_Next", "Next", vec![band])));
    assert_eq!(
        violations("configurator-panels", &doc),
        vec!["/form/children/1/children/0/name".to_string()]
    );

    // A reset written by hand, here a loaded one, gives way to the writer's.
    let mut pages = configurator(next());
    pages[0]["children"][0]["passthrough"] = json!({"raw_children": [
        "<fd:scripts fd:valueCommit=\"[{&quot;script&quot;:{&quot;content&quot;:&quot;PN_Entity.resetData();&quot;}}]\" jcr:primaryType=\"nt:unstructured\"/>"
    ]});
    assert_eq!(
        violations("configurator-panels", &doc_with(pages)),
        vec![format!("{p}/0/passthrough/raw_children/0")]
    );

    // A choice outside the band is an ordinary question and is left alone.
    let pages = vec![page("PN_A", "Order", vec![radio("RB_Kind", &["Buy", "Sell"], &["PN_A"])])];
    assert!(violations("configurator-panels", &doc_with(pages)).is_empty());
}

/// Where a subtitle takes the first page's title, the band is a Panel directly inside it.
#[test]
fn a_configurator_band_may_sit_inside_the_first_page() {
    let mut pages = configurator(page("PN_Next", "Next", vec![text_field("TXT_Next", "Next", None)]));
    pages[0]["is_page"] = json!(false);
    let first = page("PN_Agreement", "(hereinafter the Agreement)", vec![pages[0].clone()]);
    let doc = doc_with(vec![first, pages[1].clone()]);
    assert!(violations("configurator-panels", &doc).is_empty(), "{:?}", violations("configurator-panels", &doc));
}

#[test]
fn a_step_is_a_page_and_its_heading_is_its_title() {
    let body = || draw("TextDraw", "ST_Body", "Some text");
    // A panel directly under the form that is not a page.
    let doc = doc_with(vec![panel("PN_Loose", "Loose", false, false, vec![body()])]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/0/is_page".to_string()]);

    // After the first page, an untitled page that starts with a heading, or holds a heading among
    // other content.
    let first = || page("PN_First", "Subtitle", vec![body()]);
    let doc = doc_with(vec![first(), page("PN_A", "", vec![draw("TitleDraw", "TTL_A", "Account"), body()])]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/1/children/0".to_string()]);
    let doc = doc_with(vec![
        first(),
        page("PN_A", "", vec![body(), draw("TitleDraw", "TTL_A", "Account"), body()]),
    ]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/1/children/1".to_string()]);

    // An untitled page that opens with a heading, at any level, has its step title in the wrong
    // place: the feedback guard promotes it (PROBLEM-step-title-panel, `wrap:promote`; AAAR_019's
    // "2. SEPA core direct debit", a level-3 heading the source does not rule).
    let heading = || {
        let mut heading = draw("TitleDraw", "TTL_A", "Account");
        heading["heading_level"] = json!(3);
        heading
    };
    let doc = doc_with(vec![first(), page("PN_A", "", vec![heading(), body()])]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/1/children/0".to_string()]);

    // The first page is the exception: its title is the subtitle, so its first section heading
    // stays a level-2 heading after the page header and any title-area content (UBS-SEC-4).
    let section = || draw("TitleDraw", "TTL_Section", "1. Account");
    let entity = header_line("ST_HeaderSlot2", "slot2", "<p><b>UBS Europe SE</b></p>");
    for content in [vec![section(), body()], vec![preface(), entity, body(), section(), body()]] {
        let doc = doc_with(vec![page("PN_A", "", content)]);
        assert!(violations("step-titles", &doc).is_empty(), "{:?}", violations("step-titles", &doc));
    }

    // The first page's subtitle and section heading, the same heading as a page's title, and a
    // heading alone in its own panel are all fine.
    let mut subtitle = draw("TextDraw", "ST_Subtitle", "Subtitle");
    subtitle["css"] = json!("subtitle-after-form-title");
    let doc = doc_with(vec![
        page("PN_D", "", vec![subtitle, draw("TitleDraw", "TTL_D", "D"), body()]),
        page("PN_A", "Account", vec![body()]),
        page("PN_C", "", vec![panel("PN_CTitle", "", false, false, vec![draw("TitleDraw", "TTL_C", "C")]), body()]),
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
    let doc = doc_with(vec![first(), page("PN_A", "Account", vec![marked("TTL_Marked", "foo stepTitle"), body()])]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/1/children/0/css".to_string()]);

    // On an untitled page the step title is its first level-2 heading: marked and alone in its
    // own panel is the step title written by hand, and fine; a marker on a later heading is not.
    let doc = doc_with(vec![first(), page(
        "PN_A",
        "",
        vec![panel("PN_ATitle", "", false, false, vec![marked("TTL_A", "stepTitle")]), body()],
    )]);
    assert!(violations("step-titles", &doc).is_empty(), "{:?}", violations("step-titles", &doc));
    let doc = doc_with(vec![first(), page(
        "PN_A",
        "",
        vec![
            panel("PN_ATitle", "", false, false, vec![draw("TitleDraw", "TTL_A", "A")]),
            body(),
            marked("TTL_Section", "stepTitle"),
        ],
    )]);
    assert_eq!(violations("step-titles", &doc), vec!["/form/children/1/children/2/css".to_string()]);
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

/// The Italy infobox: on an entity-033 form, once on screen at the end of the
/// last page, and a hidden copy reaches the PDF only in the PDF-only shape
/// (UBS-PAGE-6, backlog "Notices").
#[test]
fn a_hidden_infobox_copy_must_reach_the_pdf_from_the_last_page() {
    let infobox = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";
    let italy = |mut doc: Value| {
        doc["variables"]["formrange_entity"] = json!("033");
        doc
    };
    let mut copy = fragment("PN_ItalyInfoboxDoR", infobox);
    copy["visible"] = json!(false);
    let on_screen = fragment("PN_Infobox", infobox);
    let doc = italy(doc_with(vec![
        page("PN_A", "A", vec![on_screen.clone(), copy.clone()]),
        page("PN_B", "B", vec![draw("TextDraw", "ST_B", "Text")]),
    ]));
    let p = "/form/children/0/children/1";
    let shown = "/form/children/0/children/0".to_string();
    assert_eq!(
        violations("issuer-information", &doc),
        vec![p.to_string(), p.to_string(), shown.clone()]
    );
    let doc = set(doc, p, "dor_exclude", json!(true));
    assert_eq!(
        violations("issuer-information", &doc),
        vec![p.to_string(), format!("{p}/dor_exclude"), p.to_string(), shown]
    );

    // The shape the normalize pass writes, after the infobox at the end of the last page.
    copy["always_in_pdf"] = json!(true);
    copy["summary_exclude"] = json!(true);
    let doc = italy(doc_with(vec![
        page("PN_A", "A", vec![draw("TextDraw", "ST_A", "Text")]),
        page("PN_B", "B", vec![draw("TextDraw", "ST_B", "Text"), on_screen.clone(), copy]),
    ]));
    assert!(violations("issuer-information", &doc).is_empty(), "{:?}", violations("issuer-information", &doc));

    // Another entity's form carries no infobox.
    let doc = doc_with(vec![page("PN_A", "A", vec![on_screen])]);
    assert_eq!(violations("issuer-information", &doc), vec!["/form/children/0/children/0".to_string()]);
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
    assert!(violations("variables", &aaev_with_variables(&golden("AAEV_019_EN"), "BA1", "019")).is_empty());
    // UBS-MET-1: the entity agrees with the issuer the page header prints (UBS Europe SE, 019).
    assert_eq!(
        violations("variables", &aaev_with_variables(&golden("AAEV_019_EN"), "AAEV", "001")),
        vec!["/variables/formrange_entity".to_string()]
    );
}

fn aaev_with_variables(doc: &Value, code: &str, entity: &str) -> Value {
    let mut doc = doc.clone();
    doc["variables"]["formrange_code"] = json!(code);
    doc["variables"]["formrange_entity"] = json!(entity);
    doc
}

#[test]
fn footnote_references_and_the_placeholder_go_together() {
    let reference = "<p>Fee<span data-af-footnote-id=\"a\"><sup>#</sup></span></p>\
        <p id=\"a\" class=\"footnoteDescription\" style=\"display: none;\"><span class=\"footnoteDescText\">Note</span></p>";
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

    // A reference in a DoR header slot line counts as much as one on a page.
    let doc = doc_with(vec![page("PN_A", "A", vec![header_line("ST_HeaderSlot2", "slot2", reference), note()])]);
    assert!(violations("footnotes", &doc).is_empty(), "{:?}", violations("footnotes", &doc));

    // A form still on accordion footnotes is the guard's other branch, left alone.
    let mut accordion = draw("TextDraw", "ST_Notes", "<p>1 Note</p>");
    accordion["css"] = json!("ubsAccordionFootnote");
    let doc = doc_with(vec![page("PN_A", "A", vec![draw("TextDraw", "ST_A", reference), accordion])]);
    assert!(violations("footnotes", &doc).is_empty());
}

/// BANH_001: a page title with a reference and its description in another text of the page made
/// AEM answer 500 (the step-title draw got the reference without the description). No Panel or
/// Repeatable title keeps a reference; a static text keeps it with its description.
#[test]
fn footnote_references_stay_out_of_titles() {
    let marked = "Signature(s)<span data-af-footnote-id=\"a\"><sup>#</sup></span>";
    let description = "<p id=\"a\" class=\"footnoteDescription\" style=\"display: none;\">\
        <span class=\"footnoteDescText\">Note</span></p>";
    let note = || json!({"type": "FootnotePlaceholder", "uuid": UUID, "name": "FN_A", "colspan": 12});
    let doc = doc_with(vec![page("PN_A", marked, vec![draw("TextDraw", "ST_Note", description), note()])]);
    let found = violations("footnotes", &doc);
    assert!(found.contains(&"/form/children/0/title/en".to_string()), "{found:?}");
    // The headings rule leaves such a title to this one rather than strip the marker.
    assert!(violations("headings-rendered-once", &doc).is_empty(), "{:?}", violations("headings-rendered-once", &doc));

    let rows = repeatable("RCP_Client", marked, vec![text_field("TXT_Name", "Name", None)]);
    let doc = doc_with(vec![page("PN_A", "A", vec![rows, draw("TextDraw", "ST_Note", description), note()])]);
    assert!(violations("footnotes", &doc).contains(&"/form/children/0/children/0/title/en".to_string()));

    let text = format!("<p>{marked}</p>{description}");
    let doc = doc_with(vec![page("PN_A", "Signature(s)", vec![draw("TextDraw", "ST_Client", &text), note()])]);
    assert!(violations("footnotes", &doc).is_empty(), "{:?}", violations("footnotes", &doc));
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

/// BANH_001: "Client" was "Kunde" in one text and "Vertragspartner" in another, which the
/// dictionary cannot hold, so the build failed. A space at the end of one master text keeps both.
#[test]
fn texts_that_read_the_same_translate_the_same() {
    let doc = |rows_title: &str, draw_de: &str| {
        let mut name = text_field("TXT_Name", "Client", None);
        name["label"] = json!({"en": "Client", "de": "Kunde"});
        let mut rows = repeatable("RCP_SGN_Client", rows_title, vec![]);
        rows["title"] = json!({"en": rows_title, "de": "Vertragspartner"});
        let mut note = draw("TextDraw", "ST_Client", "Client");
        note["content"] = json!({"en": "Client", "de": draw_de});
        let mut a_page = page("PN_A", "Account", vec![name, rows, note]);
        a_page["title"] = json!({"en": "Account", "de": "Konto"});
        let mut doc = doc_with(vec![a_page]);
        doc["languages"] = json!(["en", "de"]);
        doc
    };
    let p = "/form/children/0/children";
    // The repeatable's title collides with the label; the static text is keyed `<p>Client</p>`.
    assert_eq!(
        violations("unique-translation-keys", &doc("Client", "Vertragspartner")),
        vec![format!("{p}/1/title/de")]
    );
    assert!(violations("unique-translation-keys", &doc("Client ", "Vertragspartner")).is_empty());
    // Two static texts collide once both are wrapped, a bare text trimmed: the space goes inside.
    let mut twice = doc("Client ", "Vertragspartner");
    let mut other = draw("TextDraw", "ST_Other", "Client ");
    other["content"] = json!({"en": "Client ", "de": "Kunde"});
    twice["form"]["children"][0]["children"].as_array_mut().unwrap().push(other);
    assert_eq!(violations("unique-translation-keys", &twice), vec![format!("{p}/3/content/de")]);
    twice["form"]["children"][0]["children"][3]["content"] = json!({"en": "<p>Client </p>", "de": "Kunde"});
    assert!(violations("unique-translation-keys", &twice).is_empty());
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

/// Quotation marks follow the text's language and the country of the issuing
/// entity (UBS-TXT-2); AAEV_019_EN is a form of 019, UBS Europe SE in Germany.
#[test]
fn texts_quote_as_their_language_and_entity_do() {
    let quoted = |name: &str, texts: Value| {
        let mut node = draw("TextDraw", name, "");
        node["content"] = texts;
        node
    };
    let with_languages = |mut doc: Value| {
        doc["languages"] = json!(["en", "de", "it", "es"]);
        doc
    };
    let doc = with_languages(doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            // Straight quotes are no language's marks, in English as in German.
            quoted("ST_Pair", json!({
                "en": "<p>Click \"Preview\".</p>",
                "de": "<p style=\"margin:0\">Klicken Sie auf \"Vorschau\".</p>",
            })),
            // An escaped quote is a quote.
            quoted("ST_Escaped", json!({"es": "<p>Haga clic en &quot;Enviar&quot;</p>"})),
            // A quotation left open.
            quoted("ST_Odd", json!({"it": "<p>Il «modulo</p>"})),
            // Guillemets are Swiss German, not German German.
            quoted("ST_Swiss", json!({"de": "<p>die «E-Dokumente»</p>"})),
        ],
    )]));
    let p = "/form/children/0/children";
    let mut found = violations("quotes", &doc);
    found.sort();
    assert_eq!(
        found,
        vec![
            format!("{p}/0/content/de"),
            format!("{p}/0/content/en"),
            format!("{p}/1/content/es"),
            format!("{p}/2/content/it"),
            format!("{p}/3/content/de"),
        ]
    );

    // A label, an option label and a page title reach the dictionaries too.
    let mut field = text_field("TXT_Ref", "Reference", None);
    field["label"]["de"] = json!("Referenz \"Kunde\"");
    let mut choice = radio("RB_Kind", &["Buy", "Sell"], &[]);
    choice["options"][1]["label"]["it"] = json!("\"Vendere\"");
    let mut titled = page("PN_A", "A", vec![field, choice]);
    titled["title"]["es"] = json!("“Datos”");
    let doc = with_languages(doc_with(vec![titled]));
    let mut found = violations("quotes", &doc);
    found.sort();
    assert_eq!(
        found,
        vec![
            format!("{p}/0/label/de"),
            format!("{p}/1/options/1/label/it"),
            "/form/children/0/title/es".to_string(),
        ]
    );

    // Each language's own marks, an apostrophe, and a `"` that only delimits an attribute.
    let fine = json!({
        "en": "<p>Click “Preview”, the client’s copy.</p>",
        "de": "<p style=\"margin:0\">Klicken Sie auf „Vorschau“.</p>",
        "it": "<p><a href=\"https://www.ubs.com\" target=\"_blank\">www.ubs.com</a> «modulo»</p>",
        "es": "<p>Haga clic en «Enviar»</p>",
    });
    let doc = with_languages(doc_with(vec![page("PN_A", "A", vec![quoted("ST_Ok", fine.clone())])]));
    assert!(violations("quotes", &doc).is_empty(), "{:?}", violations("quotes", &doc));

    // A Swiss form quotes German with guillemets.
    let mut doc = with_languages(doc_with(vec![page(
        "PN_A",
        "A",
        vec![quoted("ST_Swiss", json!({"de": "<p>die «E-Dokumente»</p>"}))],
    )]));
    doc["variables"]["formrange_entity"] = json!("001");
    assert!(violations("quotes", &doc).is_empty(), "{:?}", violations("quotes", &doc));
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

/// A form carries the banking relationship only where its source does, so a
/// form without one is no break here; that the source has none is judged. The
/// issuer is the header's `slot2` line and no body text (UBS-PAGE-2).
#[test]
fn the_banking_relationship_preface_is_there_once_on_the_first_page() {
    let body = || draw("TextDraw", "ST_Body", "Some text");
    let ok = doc_with(vec![page("PN_A", "A", vec![preface(), body()]), page("PN_B", "B", vec![body()])]);
    assert!(violations("page-header", &ok).is_empty());

    let none = doc_with(vec![page("PN_A", "A", vec![body()])]);
    assert!(violations("page-header", &none).is_empty());

    let twice = doc_with(vec![page("PN_A", "A", vec![preface(), preface()])]);
    assert_eq!(
        violations("page-header", &twice),
        vec!["/form/children/0/children/1".to_string()]
    );

    let late = doc_with(vec![page("PN_A", "A", vec![body()]), page("PN_B", "B", vec![preface()])]);
    assert_eq!(
        violations("page-header", &late),
        vec!["/form/children/1/children/0".to_string()]
    );

    let mut entity = draw("TextDraw", "ST_Entity", "<p><b>UBS Europe SE</b></p>");
    entity["content"]["de"] = json!("ubs europe se");
    let line = doc_with(vec![page(
        "PN_A",
        "A",
        vec![preface(), entity.clone(), draw("TextDraw", "ST_Other", "UBS Europe SE, Milan"), header_line("ST_HeaderSlot2", "slot2", "<p><b>UBS Europe SE</b></p>")],
    )]);
    assert_eq!(
        violations("issuer-information", &line),
        vec![
            "/form/children/0/children/1/content/de".to_string(),
            "/form/children/0/children/1/content/en".to_string()
        ]
    );
    // Without the banking relationship the issuer is still the header's alone.
    let alone = doc_with(vec![page("PN_A", "A", vec![entity])]);
    assert_eq!(
        violations("issuer-information", &alone),
        vec![
            "/form/children/0/children/0/content/de".to_string(),
            "/form/children/0/children/0/content/en".to_string()
        ]
    );

    // Italy writes the whole line with its qualifier, the way a letter closes above its officers;
    // a sentence that only mentions the bank is no entity line.
    let italy = doc_with(vec![page(
        "PN_A",
        "A",
        vec![
            preface(),
            draw("TextDraw", "ST_Closing", "<p>UBS Europe SE (Succursale Italia)</p>"),
            draw("TextDraw", "ST_Comma", "UBS Europe SE, Succursale Italia"),
            draw("TextDraw", "ST_Mention", "UBS Europe SE (Succursale Italia) con sede in Milano"),
            // The closing as Italian letters write it, with a dash, and with its unit under it: the
            // feedback guard's ENTITY_LINE reads the first line only.
            draw("TextDraw", "ST_Per", "<p>Per UBS Europe SE (Succursale Italia)</p>"),
            draw("TextDraw", "ST_Dash", "UBS Europe SE \u{2013} Succursale Italia."),
            draw("TextDraw", "ST_Unit", "<p>UBS Europe SE (Succursale Italia)</p><p>Front Support Office</p>"),
        ],
    )]);
    assert_eq!(
        violations("issuer-information", &italy),
        [1, 2, 4, 5, 6].map(|i| format!("/form/children/0/children/{i}/content/en")).to_vec()
    );
}

/// A master-page line authored for the DoR header `slot`, with the flags that
/// keep it off the page and in the header.
fn header_line(name: &str, slot: &str, text: &str) -> Value {
    let mut node = draw("TextDraw", name, text);
    node["visible"] = json!(false);
    node["always_in_pdf"] = json!(true);
    node["show_if_hidden"] = json!(true);
    node["summary_exclude"] = json!(true);
    node["dor_header_slot"] = json!(slot);
    node
}

#[test]
fn the_master_page_lines_are_hidden_draws_in_their_header_slots() {
    let entity = || header_line("ST_HeaderSlot2", "slot2", "<p><b>UBS Europe SE</b></p>");
    let validity = || header_line("ST_HeaderSlot3", "slot3", "<p>Valid from 09.10.2025</p>");
    let ok = doc_with(vec![page("PN_A", "A", vec![preface(), entity(), validity()])]);
    assert!(violations("dor-header-slots", &ok).is_empty());
    // Without a banking relationship slot 1 is free, and an input may print its value in a slot.
    let mut account = text_field("TXT_Account", "Account no.", None);
    account["dor_header_slot"] = json!("slot1");
    let free = doc_with(vec![page("PN_A", "A", vec![account, entity()])]);
    assert!(violations("dor-header-slots", &free).is_empty());

    let taken = doc_with(vec![page("PN_A", "A", vec![preface(), header_line("ST_Entity", "slot1", "<p>UBS Europe SE</p>")])]);
    assert_eq!(violations("dor-header-slots", &taken), vec!["/form/children/0/children/1/dor_header_slot".to_string()]);

    // Slot 1 is never a master-page line's, and a line is a TextDraw.
    let line = doc_with(vec![page("PN_A", "A", vec![header_line("ST_Entity", "slot1", "<p>UBS Europe SE</p>")])]);
    assert_eq!(violations("dor-header-slots", &line), vec!["/form/children/0/children/0/dor_header_slot".to_string()]);
    let mut title = draw("TitleDraw", "TTL_Entity", "UBS Europe SE");
    title["dor_header_slot"] = json!("slot2");
    let heading = doc_with(vec![page("PN_A", "A", vec![title])]);
    assert_eq!(violations("dor-header-slots", &heading), vec!["/form/children/0/children/0".to_string()]);

    let unknown = doc_with(vec![page("PN_A", "A", vec![header_line("ST_Entity", "slot4", "<p>UBS Europe SE</p>")])]);
    assert_eq!(violations("dor-header-slots", &unknown), vec!["/form/children/0/children/0/dor_header_slot".to_string()]);

    let twice = doc_with(vec![page("PN_A", "A", vec![entity(), header_line("ST_Other", "slot2", "<p>Milan</p>")])]);
    assert_eq!(violations("dor-header-slots", &twice), vec!["/form/children/0/children/1/dor_header_slot".to_string()]);

    let mut shown = entity();
    shown["visible"] = json!(true);
    shown.as_object_mut().unwrap().remove("show_if_hidden");
    shown["dor_exclude"] = json!(true);
    let bare = doc_with(vec![page("PN_A", "A", vec![shown])]);
    assert_eq!(
        violations("dor-header-slots", &bare),
        vec![
            "/form/children/0/children/0/visible".to_string(),
            "/form/children/0/children/0".to_string(),
            "/form/children/0/children/0/dor_exclude".to_string(),
        ]
    );
}

/// The bank's signature (UBS-SIG-6): the entity name is a heading right before it, which is the
/// one place the issuer is body text, and the signature is titled "Responsible person".
#[test]
fn the_banks_signature_is_headed_by_the_entity_and_titled_responsible_person() {
    let signature = || fragment("PN_SGN_UBS", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1");
    let signing = |children: Vec<Value>| {
        doc_with(vec![page("PN_A", "A", vec![preface()]), page("PN_SIG", "Signature(s)", children)])
    };
    let headed = signing(vec![
        draw("TitleDraw", "TTL_UBSEuropeSE", "UBS Europe SE"),
        repeatable("RCP_SGN_UBS", "Responsible person", vec![signature()]),
    ]);
    assert!(violations("issuer-information", &headed).is_empty(), "{:?}", violations("issuer-information", &headed));
    assert!(violations("signatures", &headed).is_empty(), "{:?}", violations("signatures", &headed));

    // Titled with the entity and without the heading: the signature rule reports both.
    let titled = signing(vec![repeatable("RCP_SGN_UBS", "UBS Europe SE", vec![signature()])]);
    assert!(!violations("signatures", &titled).is_empty());

    // The entity line anywhere else is the header's, not body text.
    let stray = signing(vec![
        draw("TitleDraw", "TTL_UBSEuropeSE", "UBS Europe SE"),
        draw("TextDraw", "ST_Body", "Some text"),
    ]);
    assert_eq!(violations("issuer-information", &stray), vec!["/form/children/1/children/0/content/en".to_string()]);
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
    // Every repeatable has a title of its own (UBS-REP-1): the page's title or a heading above it
    // does not name it.
    let doc = doc_with(vec![page("PN_A", "Rows", vec![repeatable("RCP_Row", "", vec![])])]);
    assert_eq!(violations("repeatable-title", &doc), vec![format!("{p}/0/title")]);
    let doc = doc_with(vec![page("PN_A", "A", vec![repeatable("RCP_Row", "Row", vec![])])]);
    assert!(violations("repeatable-title", &doc).is_empty(), "{:?}", violations("repeatable-title", &doc));
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
        vec![format!("{p}/0/children/0"), format!("{p}/0/children/1")]
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

    // A name that no node has, a fragment and a name two nodes share. A value no option has is
    // ubs-aem-options-wellformed's.
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
        ]
    );
    // A matching value on a wired panel.
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![radio("RB_Type", &["One", "Two"], &["PN_One", "PN_Two"]), target("PN_One", true), target("PN_Two", true)],
    )]);
    assert!(violations("condition-targets", &doc).is_empty());
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
    // The configurator, a panel of text and a panel holding rows are reported; the rows carry
    // the writer's buttons.
    assert_eq!(
        violations("jump-to-field-placement", &doc),
        vec![format!("{p}/0/jump_to_field"), format!("{p}/1/jump_to_field"), format!("{p}/3/jump_to_field")]
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
fn options_are_numbered_one_to_n_and_what_compares_names_one() {
    // UBS-GEN-3: the source's own values are not kept, a value is the option's number.
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![dropdown("DD_Blank", &["a", " ", "c"]), dropdown("DD_Twice", &["1", "2", "1"]), dropdown("DD_Fine", &["1", "2"])],
    )]);
    let p = "/form/children/0/children";
    assert_eq!(
        violations("options-wellformed", &doc),
        vec![format!("{p}/0/options"), format!("{p}/1/options")]
    );
    // CNV-INP-8: a default and a condition value name one of the options.
    let mut choice = dropdown("DD_Fine", &["1", "2"]);
    choice["default_value"] = json!("3");
    let mut radio = radio("RB_Type", &["One", "Two"], &["PN_One"]);
    radio["options"] = json!([{"label": {"en": "One"}, "value": "1"}, {"label": {"en": "Two"}, "value": "2"}]);
    radio["conditions"][0]["value"] = json!({"type": "text", "value": "7"});
    let doc = doc_with(vec![page(
        "PN_A",
        "A",
        vec![choice, radio, panel("PN_One", "", false, true, vec![text_field("TXT_One", "One", None)])],
    )]);
    assert_eq!(
        violations("options-wellformed", &doc),
        vec![format!("{p}/0/default_value"), format!("{p}/1/conditions/0/value")]
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
const CONDITIONED: &str = "/form/children/0/children/4";

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

/// A letter's letterhead: the bank writes (`sender` fixed) to a receiver the person enters.
fn letterhead(name: &str) -> Value {
    json!({
        "type": "Letterhead", "uuid": UUID, "name": name,
        "sender": {"kind": "Fixed", "address": {"company": "UBS Europe SE (Succursale Italia)",
            "street": "Via del Vecchio Politecnico", "street_no": "3", "postal_code": "20121", "city": "Milano"}},
        "receiver": {"kind": "Entered"},
        "subject": {"en": "Oggetto: Notifica"},
        "your_reference": null, "our_reference": null
    })
}

#[test]
fn a_letter_has_one_letterhead_at_the_top_of_its_first_page() {
    let body = || draw("TextDraw", "ST_Body", "Egregi Signori,");
    let ok = doc_with(vec![page("PN_A", "A", vec![preface(), letterhead("PN_LetterHead"), body()])]);
    assert!(violations("letter-letterhead", &ok).is_empty());
    let alone = doc_with(vec![page("PN_A", "A", vec![letterhead("PN_LetterHead"), body()])]);
    assert!(violations("letter-letterhead", &alone).is_empty());

    let twice = doc_with(vec![page("PN_A", "A", vec![letterhead("PN_LetterHead"), letterhead("PN_Second")])]);
    assert_eq!(violations("letter-letterhead", &twice), vec!["/form/children/0/children/1".to_string()]);

    let late = doc_with(vec![page("PN_A", "A", vec![body()]), page("PN_B", "B", vec![letterhead("PN_LetterHead")])]);
    assert_eq!(violations("letter-letterhead", &late), vec!["/form/children/1/children/0".to_string()]);

    let below = doc_with(vec![page("PN_A", "A", vec![body(), letterhead("PN_LetterHead")])]);
    assert_eq!(violations("letter-letterhead", &below), vec!["/form/children/0/children/1".to_string()]);

    // The banking relationship opens the page, the letterhead follows it.
    let ahead = doc_with(vec![page("PN_A", "A", vec![letterhead("PN_LetterHead"), preface(), body()])]);
    assert_eq!(violations("letter-letterhead", &ahead), vec!["/form/children/0/children/0".to_string()]);

    // A panel under `form` that is no wizard step (the form metadata) is not the first page.
    let metadata = panel("FormMetadata", "", false, false, vec![]);
    let after_metadata = doc_with(vec![metadata, page("PN_A", "A", vec![letterhead("PN_LetterHead"), body()])]);
    assert!(violations("letter-letterhead", &after_metadata).is_empty());
}

#[test]
fn a_letters_sender_takes_slot2_so_no_draw_does() {
    let entity = || header_line("ST_HeaderSlot2", "slot2", "<p><b>UBS Europe SE</b> (Succursale Italia)</p>");
    let validity = || header_line("ST_HeaderSlot3", "slot3", "<p>Valido dal 09.10.2025</p>");
    let taken = doc_with(vec![page("PN_A", "A", vec![preface(), letterhead("PN_LetterHead"), entity(), validity()])]);
    assert_eq!(violations("dor-header-slots", &taken), vec!["/form/children/0/children/2/dor_header_slot".to_string()]);

    // Nor does a field: the sender has slot2 to itself.
    let mut account = text_field("TXT_Account", "Account no.", None);
    account["dor_header_slot"] = json!("slot2");
    let field = doc_with(vec![page("PN_A", "A", vec![preface(), letterhead("PN_LetterHead"), account])]);
    assert_eq!(violations("dor-header-slots", &field), vec!["/form/children/0/children/2/dor_header_slot".to_string()]);

    // A letter whose source prints no sender leaves slot2 to the entity line.
    let mut silent = letterhead("PN_LetterHead");
    silent["sender"] = Value::Null;
    let free = doc_with(vec![page("PN_A", "A", vec![preface(), silent, entity(), validity()])]);
    assert!(violations("dor-header-slots", &free).is_empty());
}

/// A repeatable of signatures is named after who signs, which may be an entity line of up to eight
/// words (the writer's `MAX_SIGNATURE_SUBJECT_WORDS`); any other repeatable keeps four.
#[test]
fn a_signature_block_takes_an_entity_line_as_its_title() {
    let signature = || fragment("PN_SGN_UBS", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1");
    let bank = doc_with(vec![page(
        "PN_A",
        "",
        vec![repeatable("RCP_SGN_UBS", "UBS Europe SE (Succursale Italia)", vec![signature()])],
    )]);
    assert!(violations("repeatable-title", &bank).is_empty());

    // A repeatable of signatures takes up to eight words as its subject, any other six.
    let long = "UBS Europe SE Succursale Italia Front Office";
    let bank = doc_with(vec![page("PN_A", "", vec![repeatable("RCP_SGN_UBS", long, vec![signature()])])]);
    assert!(violations("repeatable-title", &bank).is_empty());
    let rows = doc_with(vec![page(
        "PN_A",
        "",
        vec![repeatable("RCP_Rows", long, vec![text_field("TXT_Row", "Row", None)])],
    )]);
    assert_eq!(violations("repeatable-title", &rows), vec!["/form/children/0/children/0/title".to_string()]);
}

/// A letter closes with its entity line, then a unit line, then the bank's officers: there the
/// closing lines are body text in the source's order, and the officers' repeatable takes their
/// own caption as title (decision 2026-10-14). Outside a letter the title precedence stands.
#[test]
fn a_letters_closing_entity_line_is_body_text_above_the_officers() {
    let signature = || fragment("PN_SGN_UBS", "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1");
    let closing = || draw("TextDraw", "ST_Closing", "<p>UBS Europe SE (Succursale Italia)</p><p>Front Support Office</p>");
    let letter = doc_with(vec![page(
        "PN_A",
        "",
        vec![preface(), letterhead("PN_LetterHead"), closing(), repeatable("RCP_SGN_UBS", "Cognome/Nome", vec![signature()])],
    )]);
    assert!(violations("issuer-information", &letter).is_empty(), "{:?}", violations("issuer-information", &letter));

    // Without officers below it the line is the header's issuer, letter or not.
    let alone = doc_with(vec![page("PN_A", "", vec![preface(), letterhead("PN_LetterHead"), closing()])]);
    assert_eq!(violations("issuer-information", &alone), vec!["/form/children/0/children/2/content/en".to_string()]);
}

/// A form has as many internal-bank-use blocks as its source, each where the source shows it,
/// also one right after another (UBS-IBU-1); a content panel among them reaches the PDF alone.
#[test]
fn internal_bank_use_blocks_stand_where_the_source_has_them() {
    let global = "/content/dam/formsanddocuments/afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature";
    let content = |name: &str| {
        let mut block = panel(name, "", false, false, vec![
            draw("TitleDraw", "TTL_Bank", "For internal bank use only"),
            text_field("TXT_OuRef", "OU ref.", None),
        ]);
        block["visible"] = json!(false);
        block["summary_exclude"] = json!(true);
        block["always_in_pdf"] = json!(true);
        block
    };
    let doc = doc_with(vec![
        page("PN_A", "A", vec![fragment("PN_FRG_InternalBankUseOnly", global), text_field("TXT_A", "A", None)]),
        page("PN_B", "B", vec![content("PN_InternalBankUseOnly"), content("PN_InternalBankUseOnly2")]),
    ]);
    assert!(violations("global-internal-bank-use", &doc).is_empty(), "{:?}", violations("global-internal-bank-use", &doc));
    let doc = set(doc, "/form/children/1/children/1", "always_in_pdf", json!(false));
    assert_eq!(violations("global-internal-bank-use", &doc), vec!["/form/children/1/children/1".to_string()]);
}

/// A caption repeating an input's label is dropped (CNV-TXT-8), unless the input hides its
/// label: then the caption is the question it shows.
#[test]
fn a_caption_stays_beside_an_input_that_hides_its_label() {
    let mut choice = radio("RB_Kind", &["Buy", "Sell"], &[]);
    choice["label"] = json!({"en": "Order type"});
    let caption = || draw("TextDraw", "ST_Caption", "Order type");
    let doc = doc_with(vec![page("PN_A", "A", vec![caption(), choice.clone()])]);
    assert_eq!(violations("headings-rendered-once", &doc), vec!["/form/children/0/children/0".to_string()]);
    choice["hide_title"] = json!(true);
    let doc = doc_with(vec![page("PN_A", "A", vec![caption(), choice])]);
    assert!(violations("headings-rendered-once", &doc).is_empty(), "{:?}", violations("headings-rendered-once", &doc));
}

/// A partner generic's Initialize lists name its sub-panels directly and a component inside one
/// by its path, which the build checks against the fragment library.
#[test]
fn a_partner_generic_reaches_inside_its_sub_panels_by_path() {
    let generic = "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1";
    let mut party = fragment("PN_CPGRP", generic);
    party["init_hide"] = json!(["PN_EntityBasic", "PN_Nothing"]);
    party["init_show"] = json!(["PN_Address.PN_AddressBlock.TXT_State_AddressBlock"]);
    let doc = doc_with(vec![page("PN_A", "A", vec![repeatable("RCP_CPGRP", "Client", vec![party])])]);
    assert_eq!(
        violations("account-holder-wiring", &doc),
        vec!["/form/children/0/children/0/children/0/init_hide".to_string()]
    );
}
