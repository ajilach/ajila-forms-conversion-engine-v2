//! The genericity claim this crate's rename made real, not just a doc
//! comment: the *encode* direction -- the files that decide what a stock
//! resource type is spelled as, mechanically -- must never bake in a
//! naming policy that only serves one AEM profile. This greps exactly
//! those files for customer-specific vocabulary and fails on any hit.
//!
//! **Deliberately narrower than `u2s-aem`'s own `no_customer_terms.rs`.**
//! That crate's whole job is holding zero customer vocabulary anywhere, so
//! it scans all of `src/`. This crate's job is different: its `decode`
//! module exists specifically to recognise a real deployment's own overlay
//! resource types (AEM.md §14's overlay convention) and is proven against
//! the real, human-authored UBS fixture in both its own unit tests and
//! `tests/roundtrip_real_package.rs` -- so `decode/` legitimately names
//! `ajila-forms-ubs` literals as data it classifies, and `jcr/`'s own doc
//! comments legitimately credit the reference implementation
//! (`ajila-forms-conversion-engine`) code was ported from. Neither is a
//! naming *policy*: nothing in `decode/` chooses what to *write*, only
//! what an already-written resource type is recognised as, and the suffix
//! match itself is UBS-blind (see `decode::form::decode_node`'s own doc).
//!
//! What must stay clean is the other direction: the modules that decide
//! what a stock component *is written as* when nothing else in the tree
//! says otherwise (structural literals like `guideContainer`'s own
//! resource type), plus the catalogue that teaches that vocabulary and the
//! canonical-comparison and search logic layered over it. A customer
//! literal creeping into any of these would mean the mechanical encoder
//! had quietly started making a profile's judgment call again.

use std::fs;
use std::path::Path;

use regex::Regex;

const CHECKED_FILES: &[&str] = &[
    "xml_writer.rs",
    "xsd.rs",
    "dam.rs",
    "package.rs",
    "i18n.rs",
    "canonical.rs",
    "search.rs",
];

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
fn the_mechanical_encoder_files_carry_no_customer_vocabulary() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let ubs_token = Regex::new(r"\bUBS\b").expect("valid pattern");
    let mut hits = Vec::new();

    for name in CHECKED_FILES {
        let path = src_dir.join(name);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("{} must be readable: {err}", path.display()));
        for term in FORBIDDEN_SUBSTRINGS {
            if text.contains(term) {
                hits.push(format!("{}: {term:?}", path.display()));
            }
        }
        if ubs_token.is_match(&text) {
            hits.push(format!("{}: \"UBS\"", path.display()));
        }
    }

    assert!(
        hits.is_empty(),
        "customer-specific vocabulary found in the mechanical encoder: {hits:?}"
    );
}

/// `catalog::FOUNDATION`'s own *values* -- `resource_type` and
/// `guide_node_class` -- are the actual vocabulary a Conversion Agent is
/// taught to write, so those must stay stock even though the table's
/// `description` strings are allowed to name a documented UBS deviation
/// (see that module's own doc). Checked separately from `CHECKED_FILES`
/// above because `catalog.rs` as a whole legitimately contains "UBS" in
/// prose.
#[test]
fn the_catalogue_carries_only_stock_foundation_resource_types() {
    for entry in u2s_mapper_aem::catalog::FOUNDATION {
        assert!(
            !entry.resource_type.to_lowercase().contains("ubs")
                && !entry.resource_type.to_lowercase().contains("ajila"),
            "{} is not a stock Foundation resource type",
            entry.resource_type
        );
        if let Some(guide_node_class) = entry.guide_node_class {
            assert!(
                !guide_node_class.to_lowercase().contains("ubs")
                    && !guide_node_class.to_lowercase().contains("ajila"),
                "{guide_node_class} is not a stock guideNodeClass"
            );
        }
    }
}
