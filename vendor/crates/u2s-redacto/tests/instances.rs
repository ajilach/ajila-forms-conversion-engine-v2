//! Validates JSON instances against the generated schema directly, the same
//! path `json_validate` (per PLAN.md) uses at runtime. Complements
//! `validate.rs`, which exercises `RedactoDocument::validate` -- the checks
//! here are everything the *schema itself* can express: `enum`, `pattern`,
//! length bounds and a tagged component union.

mod support;

use jsonschema::Validator;
use serde_json::Value;

fn validator() -> Validator {
    jsonschema::validator_for(&u2s_redacto::schema()).expect("generated schema is itself valid")
}

fn mutate(mut json: Value, pointer: &str, value: Value) -> Value {
    *json
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("pointer {pointer} does not exist in the fixture")) = value;
    json
}

#[test]
fn sample_document_is_schema_valid() {
    let validator = validator();
    let json = support::sample_document_json();
    let errors: Vec<_> = validator.iter_errors(&json).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "expected no schema errors, got {errors:?}");
}

#[test]
fn unknown_field_is_rejected() {
    let validator = validator();
    let json = mutate(support::sample_document_json(), "/metadata", {
        let mut metadata = support::sample_document_json()["metadata"].clone();
        metadata["unexpected_field"] = Value::String("nope".to_string());
        metadata
    });
    assert!(!validator.is_valid(&json));
}

#[test]
fn bad_language_code_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_document_json(),
        "/metadata/master_language",
        Value::String("english".to_string()),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn over_long_document_id_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_document_json(),
        "/metadata/document_id",
        Value::String("a".repeat(51)),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn style_without_css_extension_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_document_json(),
        "/metadata/style",
        Value::String("default".to_string()),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn unknown_component_type_is_rejected() {
    let validator = validator();
    let json = mutate(
        support::sample_document_json(),
        "/body/0",
        serde_json::json!({ "type": "mysteryBox", "assets": ["intro"] }),
    );
    assert!(!validator.is_valid(&json));
}

#[test]
fn a_third_asset_language_is_schema_valid() {
    // The schema does not close the set of languages a document may
    // declare -- adding a third language and matching content is still
    // structurally valid; whether every asset actually carries it is
    // `RedactoDocument::validate`'s job, exercised in `validate.rs`.
    let validator = validator();
    let mut json = support::sample_document_json();
    json["metadata"]["languages"] = serde_json::json!(["en", "de", "fr"]);
    assert!(validator.is_valid(&json));
}
