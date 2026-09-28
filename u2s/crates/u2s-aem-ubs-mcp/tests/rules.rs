//! The UBS AEM rules in `rules/`, run over real documents in the rules
//! sandbox.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use u2s_rules::{CheckOutcome, ScriptBudget, run_check};

fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("rules")
}

fn golden(form: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
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

fn rules() -> Vec<String> {
    let mut rules: Vec<String> = std::fs::read_dir(rules_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
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
    let from_dir = u2s_doc_tools::rules_dir::load_rules_dir(&rules_dir()).expect("the rules load");
    let compiled = u2s_doc_tools::rules_dir::load_rules(u2s_aem_ubs_mcp::rule_files())
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
