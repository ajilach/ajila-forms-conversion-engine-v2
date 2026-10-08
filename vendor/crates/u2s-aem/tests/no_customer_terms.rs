//! Genericity is mechanically checked here, not just asserted in prose: this
//! test greps the crate's own sources for customer-specific vocabulary and
//! fails on any hit. A later customer profile layer (see the "Extension
//! points" section of PLAN-u2s-aem.md) belongs in a different crate; this
//! test is what keeps this one honest if someone reaches for a shortcut.

use std::fs;
use std::path::Path;

use regex::Regex;

/// Substrings that are unambiguous customer vocabulary wherever they occur —
/// distinctive enough (a hyphenated slug, an XFA variable prefix, a fragment
/// naming convention, a proper noun) that no generic AEM code would ever
/// contain them.
const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "ajila-forms",
    "formrange_",
    "affrg_",
    "Redacto",
    "Frutiger",
    "Kundendaten",
    "Banking Relationship",
];

#[test]
fn source_contains_no_customer_vocabulary() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    // Word-boundary match: "UBS" as a substring also matches inside
    // ordinary identifiers like `subschema`, so this needs to isolate the
    // standalone token rather than a plain `contains`.
    let ubs_token = Regex::new(r"\bUBS\b").expect("valid pattern");
    walk(&src_dir, &ubs_token, &mut hits);
    assert!(
        hits.is_empty(),
        "customer-specific vocabulary found in u2s-aem's own sources: {hits:?}"
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
