//! The scripted UBS Redacto rules in `rules/redacto/`, run over the golden documents the vendored
//! UBS layer keeps for its parity tests and over small hand-built documents.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use u2s_rules::{ScriptBudget, run_check};

fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rules/redacto")
}

fn golden(form: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vendor/crates/u2s-redacto-ubs-mcp/tests/fixtures/golden")
        .join(form)
        .join("document.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn violations(rule: &str, doc: &Value) -> Vec<String> {
    let script = std::fs::read_to_string(rules_dir().join(rule).join("check.js")).unwrap();
    run_check(&script, doc, &json!({}), &Map::new(), &ScriptBudget::default())
        .unwrap_or_else(|e| panic!("{rule} breaks: {e:?}"))
        .violations
        .into_iter()
        .map(|v| v.pointer)
        .collect()
}

/// The scripted rules: the rule directories with a `check.js`.
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

/// A document of two languages with the assets given as `(key, kind, content)`.
fn document(assets: Vec<(&str, &str, Value)>) -> Value {
    json!({
        "sources": {"en": {"variables": {}}, "de": {"variables": {}}},
        "assets": assets.into_iter()
            .map(|(key, kind, content)| json!({"key": key, "kind": kind, "content": content}))
            .collect::<Vec<_>>(),
        "body": []
    })
}

#[test]
fn the_golden_documents_pass_every_rule() {
    for form in ["AAOS_033_IT", "AAEV_019_EN", "AABF_019"] {
        for rule in rules() {
            assert_eq!(violations(&rule, &golden(form)), Vec::<String>::new(), "{form} {rule}");
        }
    }
}

#[test]
fn an_asset_lacks_a_language_is_blank_in_one_or_carries_one_the_document_does_not_ship() {
    let doc = document(vec![
        ("a", "text", json!({"en": "<p>A</p>"})),
        ("b", "text", json!({"en": "<p>B</p>", "de": "  "})),
        ("c", "text", json!({"en": "<p>C</p>", "de": "<p>C</p>", "fr": "<p>C</p>"})),
        ("d", "text", json!({"en": "<p>D</p>", "de": "<p>D</p>"})),
        ("img", "image", json!({"zxx": "data:image/png;base64,AAAA"})),
    ]);
    assert_eq!(
        violations("assets-carry-every-language", &doc),
        vec!["/assets/0/content", "/assets/1/content/de", "/assets/2/content/fr"]
    );
    let fixed = document(vec![
        ("a", "text", json!({"en": "<p>A</p>", "de": "<p>A</p>"})),
        ("img", "image", json!({"zxx": "data:image/png;base64,AAAA"})),
    ]);
    assert!(violations("assets-carry-every-language", &fixed).is_empty());
}

#[test]
fn html_outside_the_quill_vocabulary_is_reported_like_the_model_refuses_it() {
    let doc = document(vec![
        ("script", "text", json!({"en": "<p>A</p><script>x</script>", "de": "<p>A</p>"})),
        ("bold", "text", json!({"en": "<b>A</b>", "de": "<strong>A</strong>"})),
        ("open", "text", json!({"en": "<p>A", "de": "<p>A</p>"})),
        ("order", "text", json!({"en": "<p><strong>A</p></strong>", "de": "<ul><li>A</li></ul>"})),
        ("stray", "text", json!({"en": "A</p>", "de": "<p>A<br>B<br/>C</p>"})),
        ("img", "text", json!({"en": "<p><img src=\"x\"></p>", "de": "<p><img src=\"x\" alt=\" \"></p>"})),
        ("fine", "text", json!({"en": "<table><thead><tr><th>A</th></tr></thead><tbody><tr><td>1 < 2</td></tr></tbody></table>", "de": "<p><img src=\"x\" alt=\"Logo\"> <sup>1</sup></p>"})),
        ("image", "image", json!({"zxx": "data:image/png;base64,AAAA"})),
    ]);
    assert_eq!(
        violations("html-vocabulary", &doc),
        vec![
            "/assets/0/content/en",
            "/assets/1/content/en",
            "/assets/2/content/en",
            "/assets/3/content/en",
            "/assets/3/content/en",
            "/assets/4/content/en",
            "/assets/5/content/de",
            "/assets/5/content/en",
        ]
    );
}

/// Every Redacto rule is a rule directory with a `rule.toml` that loads, scripted or judged.
#[test]
fn the_redacto_rules_load() {
    let rules = agent::rules::rules_for(agent::OutputTarget::Redacto).expect("the Redacto rules load");
    let scripted: Vec<_> = rules.scripted.iter().map(|r| r.id).collect();
    assert_eq!(scripted.len(), 2, "{scripted:?}");
    assert_eq!(rules.judged.len(), 10);
    assert!(rules.judged.iter().all(|r| r.name.starts_with("ubs-redacto-")));
}
