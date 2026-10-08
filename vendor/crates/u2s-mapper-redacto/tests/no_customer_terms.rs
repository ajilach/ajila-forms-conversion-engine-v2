//! The genericity claim this crate's own module doc makes, checked
//! mechanically rather than only asserted in prose: the files that decide
//! *how an already-decided document is spelled as rows and SQL* must never
//! bake in a naming policy that only serves one Redacto profile.
//!
//! **Deliberately narrower than `u2s-redacto`'s own `no_customer_terms.rs`**
//! -- mirroring `u2s-mapper-aem`'s own precedent for the same split. This
//! crate's `lib.rs`/`ids.rs`/`sql.rs` module docs legitimately credit the
//! reference implementation (`ajila-forms-conversion-engine`) code and
//! reasoning were ported or adapted from; that is provenance, not a naming
//! *policy*. What must stay clean is [`config`](crate::config) and
//! [`rows`](crate::rows) -- the two modules that decide what a document is
//! written as -- plus the decoder, which must recognise a real dump's own
//! shape without leaning on any one profile's own vocabulary to do it.

use std::fs;
use std::path::Path;

use regex::Regex;

const CHECKED_FILES: &[&str] = &["config.rs", "rows.rs"];
const CHECKED_DIRS: &[&str] = &["decode"];

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
fn encode_and_decode_decision_files_contain_no_customer_vocabulary() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    let ubs_token = Regex::new(r"\bUBS\b").expect("valid pattern");

    for name in CHECKED_FILES {
        check_file(&src_dir.join(name), &ubs_token, &mut hits);
    }
    for dir in CHECKED_DIRS {
        walk(&src_dir.join(dir), &ubs_token, &mut hits);
    }

    assert!(
        hits.is_empty(),
        "customer-specific vocabulary found in u2s-mapper-redacto's encode/decode decision \
         files: {hits:?}"
    );
}

fn walk(dir: &Path, ubs_token: &Regex, hits: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            walk(&path, ubs_token, hits);
        } else {
            check_file(&path, ubs_token, hits);
        }
    }
}

fn check_file(path: &Path, ubs_token: &Regex, hits: &mut Vec<String>) {
    if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for term in FORBIDDEN_SUBSTRINGS {
        if text.contains(term) {
            hits.push(format!("{}: {term:?}", path.display()));
        }
    }
    if ubs_token.is_match(&text) {
        hits.push(format!("{}: \"UBS\"", path.display()));
    }
}
