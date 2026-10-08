//! The AEM schema must survive strict-mode adaptation, because that is what a
//! provider sees whenever the Conversion Agent generates rather than edits.
//!
//! This test lives here, not in `u2s-engine`, so the adapter stays
//! format-agnostic: the format crate proves its own schema is adaptable. It is
//! also the test that fails when a model change introduces a construct the
//! strict subset cannot carry — `allOf` from a `#[serde(flatten)]`, say — which
//! is far cheaper to learn here than from a 400 mid-conversion.

use u2s_engine::{adapt, restore, strict_violations, validate};

#[test]
fn the_aem_schema_adapts_to_the_strict_subset() {
    let original = u2s_aem::schema();
    let adapted = adapt(&original).expect("the AEM schema adapts");

    assert_eq!(
        strict_violations(&adapted.schema),
        Vec::<String>::new(),
        "the adapted AEM schema must be strict-safe"
    );
}

#[test]
fn the_language_maps_are_re_encoded_as_pairs() {
    let original = u2s_aem::schema();
    let adapted = adapt(&original).expect("adapts");

    // `I18nText` and `I18nRichText` are `patternProperties` maps keyed by
    // language code. Strict mode cannot express a map, so they must arrive as
    // arrays of `{key, value}` — this is the case that makes the re-encoding
    // load-bearing rather than theoretical.
    for name in ["I18nText", "I18nRichText"] {
        let def = &adapted.schema["$defs"][name];
        assert_eq!(
            def["type"], "array",
            "{name} must be re-encoded as pairs, got {def}"
        );
        assert_eq!(def["items"]["properties"]["key"]["type"], "string");
    }

    let re_encoded = adapted
        .relaxations
        .iter()
        .filter(|r| matches!(r, u2s_engine::Relaxation::MapAsPairs { .. }))
        .count();
    assert!(re_encoded >= 2, "{:?}", adapted.relaxations);
}

#[test]
fn the_relaxations_are_all_recoverable_by_validation() {
    let original = u2s_aem::schema();
    let adapted = adapt(&original).expect("adapts");

    // Every weakening must be a keyword that `validate` re-checks against the
    // original. If adaptation ever gives up something validation cannot catch,
    // this is where it shows.
    let weakening: Vec<_> = adapted.weakening().collect();
    assert!(
        !weakening.is_empty(),
        "the AEM schema does carry patterns and minimums; if this is empty the \
         adapter stopped recording"
    );
}

/// A minimal real form, through the whole pipeline: generate against the
/// adapted schema, restore, validate against the original.
#[test]
fn a_generated_form_restores_and_validates() {
    let original = u2s_aem::schema();
    let adapted = adapt(&original).expect("adapts");

    let form_json = support::sample_form_json();

    // The fixture is already an original-shaped document, so it must validate
    // as-is, and restoring it must be a no-op: `restore` is only allowed to
    // change documents that came out of a strict generation.
    let violations = validate(&original, &form_json).expect("the schema compiles");
    assert_eq!(violations, Vec::new(), "the fixture form is schema-valid");

    let restored = restore(&original, form_json.clone());
    assert_eq!(
        restored, form_json,
        "restore must not disturb a document that never went through strict mode"
    );

    assert_eq!(strict_violations(&adapted.schema), Vec::<String>::new());
}

mod support;
