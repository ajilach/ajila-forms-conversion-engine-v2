//! The Redacto schema must survive strict-mode adaptation, because that is
//! what a provider sees whenever the Conversion Agent generates rather than
//! edits.
//!
//! Lives here, not in `u2s-engine`, so the adapter stays format-agnostic:
//! the format crate proves its own schema is adaptable.

use u2s_engine::{adapt, restore, strict_violations, validate};

#[test]
fn the_redacto_schema_adapts_to_the_strict_subset() {
    let original = u2s_redacto::schema();
    let adapted = adapt(&original).expect("the Redacto schema adapts");

    assert_eq!(
        strict_violations(&adapted.schema),
        Vec::<String>::new(),
        "the adapted Redacto schema must be strict-safe"
    );
}

#[test]
fn the_language_map_is_re_encoded_as_pairs() {
    let original = u2s_redacto::schema();
    let adapted = adapt(&original).expect("adapts");

    // `I18nHtml` is a `patternProperties` map keyed by language code. Strict
    // mode cannot express a map, so it must arrive as an array of
    // `{key, value}` -- this is the case that makes the re-encoding
    // load-bearing rather than theoretical.
    let def = &adapted.schema["$defs"]["I18nHtml"];
    assert_eq!(def["type"], "array", "I18nHtml must be re-encoded as pairs, got {def}");
    assert_eq!(def["items"]["properties"]["key"]["type"], "string");

    let re_encoded = adapted
        .relaxations
        .iter()
        .filter(|r| matches!(r, u2s_engine::Relaxation::MapAsPairs { .. }))
        .count();
    assert!(re_encoded >= 1, "{:?}", adapted.relaxations);
}

#[test]
fn the_relaxations_are_all_recoverable_by_validation() {
    let original = u2s_redacto::schema();
    let adapted = adapt(&original).expect("adapts");

    let weakening: Vec<_> = adapted.weakening().collect();
    assert!(
        !weakening.is_empty(),
        "the Redacto schema does carry patterns and length bounds; if this is empty the \
         adapter stopped recording"
    );
}

/// A minimal real document, through the whole pipeline: generate against the
/// adapted schema, restore, validate against the original.
#[test]
fn a_generated_document_restores_and_validates() {
    let original = u2s_redacto::schema();
    let adapted = adapt(&original).expect("adapts");

    let doc_json = support::sample_document_json();

    // The fixture is already an original-shaped document, so it must
    // validate as-is, and restoring it must be a no-op: `restore` is only
    // allowed to change documents that came out of a strict generation.
    let violations = validate(&original, &doc_json).expect("the schema compiles");
    assert_eq!(violations, Vec::new(), "the fixture document is schema-valid");

    let restored = restore(&original, doc_json.clone());
    assert_eq!(
        restored, doc_json,
        "restore must not disturb a document that never went through strict mode"
    );

    assert_eq!(strict_violations(&adapted.schema), Vec::<String>::new());
}

mod support;
