//! Validates JSON instances against the generated schema directly, the same
//! path `json_validate` (per PLAN.md) uses at runtime. Complements
//! `validate.rs`, which exercises `AemForm::validate` — the checks here are
//! everything the *schema itself* can express: `enum`, `pattern`, integer
//! bounds and a tagged `oneOf`.

mod support;

use jsonschema::Validator;
use serde_json::Value;

fn validator() -> Validator {
    jsonschema::validator_for(&u2s_aem::schema()).expect("generated schema is itself valid")
}

fn mutate(mut json: Value, pointer: &str, value: Value) -> Value {
    *json
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("pointer {pointer} does not exist in the fixture")) = value;
    json
}

#[test]
fn sample_form_is_schema_valid() {
    let validator = validator();
    let json = support::sample_form_json();
    let errors: Vec<_> = validator
        .iter_errors(&json)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "expected no schema errors, got {errors:?}"
    );
}

#[test]
fn unknown_field_is_rejected() {
    let validator = validator();
    let json = mutate(support::sample_form_json(), "/metadata", {
        let mut metadata = support::sample_form_json()["metadata"].clone();
        metadata["unexpected_field"] = Value::String("nope".to_string());
        metadata
    });
    assert!(!validator.is_valid(&json));
}

#[test]
fn bad_language_code_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_form_json(),
        "/metadata/master_language",
        Value::String("english".to_string()),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn column_span_out_of_range_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/3/layout",
        serde_json::json!({ "width": 13 }),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn empty_option_list_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/1/options",
        serde_json::json!([]),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn malformed_date_pattern_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_form_json(),
        "/pages/0/children/4/format",
        serde_json::json!({ "kind": "pattern", "value": "not-a-clause!!" }),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn a_second_page_is_schema_valid() {
    // `metadata.toolbar` used to be a closed `ToolbarButton` enum, and this
    // test asserted the schema could not catch a duplicate in it (that
    // check lived in `AemForm::validate` instead). The toolbar is now
    // `Vec<Node>` -- a generic node has no simple discriminant to call
    // "the same one twice" -- so there is nothing left for the schema *or*
    // `AemForm::validate` to check here; see `validate.rs`'s own comment on
    // why. This test is kept as a renamed schema-shape smoke test instead:
    // a second, minimal page still validates.
    let validator = validator();
    let mut json = support::sample_form_json();
    json["pages"]
        .as_array_mut()
        .expect("pages is an array")
        .push(serde_json::json!({
            "name": "PageTwo",
            "properties": {},
            "children": [
                {
                    "type": "TextField",
                    "common": {
                        "name": "SecondPageField",
                        "resource_type": "fd/af/components/controls/textbox"
                    },
                    "field": { "label": { "en": "Field", "de": "Feld" } },
                    "layout": { "width": 12 },
                    "input": "single_line"
                }
            ]
        }));
    assert!(validator.is_valid(&json));
}
