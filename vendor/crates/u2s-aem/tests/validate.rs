//! One negative test per `AemForm::validate` check: every cross-node
//! constraint the type system alone cannot enforce. Each test starts from
//! the shared valid fixture and mutates exactly the JSON pointer needed to
//! trigger one check, so a failure here points at the check that broke.

mod support;

use serde_json::Value;
use u2s_aem::AemForm;

fn mutate(mut json: Value, pointer: &str, value: Value) -> Value {
    *json
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("pointer {pointer} does not exist in the fixture")) = value;
    json
}

fn violations_for(json: Value) -> Vec<u2s_aem::Violation> {
    let form = AemForm::from_json(&json).expect("mutation stays structurally valid JSON");
    form.validate()
        .expect_err("mutation should fail semantic validation")
}

fn assert_violation_at(json: Value, pointer: &str) {
    let violations = violations_for(json);
    assert!(
        violations.iter().any(|v| v.pointer == pointer),
        "expected a violation at {pointer}, got {violations:?}"
    );
}

#[test]
fn empty_pages_is_rejected() {
    let json = mutate(support::sample_form_json(), "/pages", Value::Array(vec![]));
    assert_violation_at(json, "/pages");
}

#[test]
fn page_with_no_children_is_rejected() {
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children",
        Value::Array(vec![]),
    );
    assert_violation_at(json, "/pages/0/children");
}

#[test]
fn master_language_not_in_languages_is_rejected() {
    let json = mutate(
        support::sample_form_json(),
        "/metadata/languages",
        serde_json::json!(["de"]),
    );
    assert_violation_at(json, "/metadata/languages");
}

#[test]
fn missing_master_translation_is_rejected() {
    // `Page.title` used to be a typed `I18nText` field; a page's own title
    // is now an ordinary `properties` entry (see `Page`'s own doc), so this
    // exercises the same check (`AemForm::validate`'s translation-coverage
    // pass) against `ConditionalPanel`'s `jcr:title` property instead --
    // still a `Node::Component`'s own `JcrValue::Text`, which
    // `Node::i18n_texts` surfaces the same way it does a typed leaf kind's
    // fields.
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/0/properties/jcr:title/value",
        serde_json::json!({ "de": "Bedingt" }),
    );
    assert_violation_at(json, "/pages/0/children/0/properties/jcr:title/value");
}

#[test]
fn translation_in_undeclared_language_is_rejected() {
    // `pointer_mut` can only replace an existing key, not add one, so the
    // whole map is replaced with one carrying an extra "fr" entry.
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/0/properties/jcr:title/value",
        serde_json::json!({ "en": "Conditional", "de": "Bedingt", "fr": "Conditionnel" }),
    );
    assert_violation_at(json, "/pages/0/children/0/properties/jcr:title/value");
}

#[test]
fn duplicate_component_name_is_rejected() {
    // "Amount" (children/3) renamed to collide with "CountryDropdown"
    // (children/1).
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/3/common/name",
        Value::String("CountryDropdown".to_string()),
    );
    assert_violation_at(json, "/pages/0/children/3/common/name");
}

// `dangling_visibility_trigger_is_rejected` and
// `visibility_value_outside_trigger_options_is_rejected` used to exercise
// `AemForm::validate`'s check of the typed `VisibilityRule` field against
// the tree. Visibility is now ordinary agent-authored `fd:rules`/
// `fd:visible` content on a `Component` (see `u2s-mapper-aem`'s module
// doc on why), so this model has no typed field left for that check to
// read -- "this panel's visibility references a real trigger and a real
// option value" is a rule's job now, not `AemForm::validate`'s. Removed,
// not replaced: there is no equivalent structural check left to test.

// `max_occur_below_min_occur_is_rejected` used to exercise the typed
// `Node::Repeatable::{min_occur,max_occur}` fields. A repeatable is now two
// ordinary `Component` panels the agent authors directly (see the fixture's
// own `Dependants`/`DependantsInstance`), with `minOccur`/`maxOccur` as
// plain string properties -- there is no longer a typed pair for
// `AemForm::validate` to compare, so this becomes a rule's job too.
// Removed, not replaced, for the same reason as the visibility checks
// above.

#[test]
fn layout_width_plus_offset_over_twelve_is_rejected() {
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/3/layout",
        serde_json::json!({ "width": 12, "offset": 1 }),
    );
    assert_violation_at(json, "/pages/0/children/3/layout");
}

// `duplicate_toolbar_button_is_rejected` used to exercise the closed
// `ToolbarButton` enum's `Eq + Hash`-based dedupe check in
// `AemForm::validate`. The toolbar is now `Vec<Node>` (see
// `FormMetadata::toolbar`'s own doc) -- a generic node has no simple
// discriminant to call "the same one twice" -- so there is nothing left
// here for `AemForm::validate` to check; removed, not replaced.
