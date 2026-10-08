//! The AEM schema must produce a usable skeleton — the cold-start seed the
//! Conversion Agent starts from when no reference run is close enough.
//!
//! Lives here, not in `u2s-engine`, for the same reason `strict_adaptation.rs`
//! does: the format crate proves its own schema against the engine's
//! format-agnostic machinery, so `u2s-engine` never has to name AEM.

use u2s_engine::{skeleton, validate};

/// **The honest result, not the hoped-for one.** `u2s_engine::skeleton`
/// satisfies `enum`, `const`, `oneOf` and `anyOf` — closed, enumerable
/// constraints — but a `pattern` with no `enum`/`const` alongside it is not
/// satisfiable without synthesizing a string that matches an arbitrary
/// regular expression, which is not a problem `u2s-engine` can solve
/// generically (see `skeleton`'s own module doc). The real AEM schema has
/// exactly two such fields: `form_name` and `master_language`, both plain
/// strings constrained only by a naming pattern with no enumerated values.
///
/// So this asserts the **residual** is exactly those two, named — proving
/// the skeleton generator does everything a generic function honestly can,
/// and that what remains is a known, bounded gap for the Conversion Agent's
/// first real edit to close, not a silent hole.
#[test]
fn the_aem_skeleton_needs_exactly_the_two_pattern_only_fields_repaired() {
    let schema = u2s_aem::schema();
    let doc = skeleton(&schema).expect("the real AEM schema must not be a $ref cycle");

    let violations = validate(&schema, &doc).expect("a well-formed schema validates");
    let pointers: Vec<&str> = violations.iter().map(|v| v.pointer.as_str()).collect();

    assert_eq!(
        pointers,
        vec!["/metadata/form_name", "/metadata/master_language"],
        "every other required field -- including the oneOf-typed data_model and the \
         enum-typed dor -- must already be satisfied: {violations:?}"
    );
    for violation in &violations {
        assert!(
            violation.message.contains("does not match"),
            "the residual must be exactly a pattern mismatch, nothing structural: {violation:?}"
        );
    }
}
