//! Genericity is mechanically checked here, not just asserted in prose: this
//! test greps the crate's own sources for customer-specific vocabulary and
//! fails on any hit. `Redacto` is deliberately *not* forbidden -- it is this
//! format's own platform name, not a customer's, unlike `u2s-aem`'s own
//! list (which correctly forbids it, since AEM has no reason to know a
//! second delivery target's name).

use std::fs;
use std::path::Path;

use regex::Regex;

/// Substrings that are unambiguous UBS-specific vocabulary wherever they
/// occur -- distinctive enough (a hyphenated slug, a form-code convention,
/// a footer field class, a proper noun) that no generic Redacto code would
/// ever contain them.
const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "ajila-forms",
    "formrange_",
    "affrg_",
    "Frutiger",
    "footer-form-id",
    "footer-man-code",
    "footer-j-version",
];

#[test]
fn source_contains_no_customer_vocabulary() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    // Word-boundary match: "UBS" as a substring also matches inside
    // ordinary identifiers, so this needs to isolate the standalone token
    // rather than a plain `contains`.
    let ubs_token = Regex::new(r"\bUBS\b").expect("valid pattern");
    walk(&src_dir, &ubs_token, &mut hits);
    assert!(
        hits.is_empty(),
        "customer-specific vocabulary found in u2s-redacto's own sources: {hits:?}"
    );
}

fn walk(dir: &Path, ubs_token: &Regex, hits: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("src directory is readable") {
        let entry = entry.expect("directory entry is readable");
        let path = entry.path();
        if path.is_dir() {
            walk(&path, ubs_token, hits);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("source file is UTF-8");
        for term in FORBIDDEN_SUBSTRINGS {
            if text.contains(term) {
                hits.push(format!("{}: {term:?}", path.display()));
            }
        }
        if ubs_token.is_match(&text) {
            hits.push(format!("{}: \"UBS\"", path.display()));
        }
    }
}
