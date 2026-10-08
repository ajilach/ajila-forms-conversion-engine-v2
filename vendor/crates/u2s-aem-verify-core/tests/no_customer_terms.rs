//! The genericity claim `src/lib.rs`'s own doc makes real, not just a doc
//! comment: this crate's *logic* -- the JS it evaluates, the DOM selectors
//! and JCR/field names it hardcodes -- must never bake in a specific
//! customer's Adaptive Forms platform. That belongs to the
//! `crate::driver::FormDriver` a binary built on top of this crate
//! supplies (`u2s-aem-ubs-verify-mcp`'s own `driver.rs`/`ubs_metadata.rs`,
//! today).
//!
//! Deliberately scoped to *identifiers a real UBS form's own DOM or data
//! model would carry* (`window.forms.ubs`, `summaryComponent`,
//! `afAcceptLang`, a JCR `formrange_`/`affrg_` attribute, `ajila-forms`
//! resource types) -- not the bare words "UBS" or "Redacto", which this
//! crate's own doc comments legitimately use in prose explaining *why* a
//! generic design choice was made (the same precedent
//! `u2s-mapper-aem/tests/no_customer_terms.rs` sets for its own `decode/`
//! and `jcr/` modules: history and rationale in a comment is not a naming
//! *policy* baked into the code).

use std::fs;
use std::path::Path;

const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "window.forms.ubs",
    "ajila-forms",
    "formrange_",
    "affrg_",
    "summaryComponent",
    "afAcceptLang",
    "txtMandator",
    "txtLanguage",
];

/// Strips `//`/`///`/`//!` line comments before scanning -- this crate's
/// own doc comments legitimately *illustrate* the `FormDriver` seam with a
/// concrete UBS example (exactly the precedent
/// `u2s-mapper-aem/tests/no_customer_terms.rs` already sets: explanatory
/// prose is not the naming policy this test guards against). What must
/// stay clean is executable code -- a JS string literal, a match arm, a
/// hardcoded field name -- not a comment describing why the seam exists.
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_source_file_hardcodes_a_customer_specific_dom_or_data_identifier() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();

    for entry in fs::read_dir(&src_dir).expect("src/ must exist") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("{} must be readable: {err}", path.display()));
        let code = strip_line_comments(&raw);
        for term in FORBIDDEN_SUBSTRINGS {
            if code.contains(term) {
                hits.push(format!("{}: {term:?}", path.display()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "customer-specific DOM/data identifiers found in u2s-aem-verify-core's own executable \
         code: {hits:?} -- that belongs in a FormDriver implementation, not this shared crate"
    );
}
