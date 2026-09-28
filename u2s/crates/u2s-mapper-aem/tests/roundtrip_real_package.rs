//! `canonical::compare(AF_AABF.zip, encode(decode(AF_AABF.zip)))`, the
//! actual "full real-package fidelity" gate the design plan calls for (its
//! own "still needs" item #1). Decoding and re-encoding the real fixture
//! already has its own tests in `decode::tests`; this one additionally
//! measures *how close* the round trip is, and pins that measurement so a
//! future regression is caught instead of silently absorbed into "well,
//! there were always some differences".
//!
//! The count asserted below is not zero, and is not expected to become
//! zero -- see the categories named in this test's own assertions. Each
//! one is either a documented, deferred gap (DOR rendition binaries have
//! no decode path yet) or content this crate's own model was never meant
//! to reproduce (a real deployment's own shared, multi-tenant JCR
//! scaffolding and vault packaging metadata sit outside any single form's
//! own content). What this test actually pins down is that the
//! *reachable* categories -- everything under the form's own
//! `guideContainer` subtree and its own DAM asset `.content.xml`, most of
//! all its dictionaries -- stay lossless as the decoder grows, by
//! asserting an upper bound tight enough that a real regression there
//! fails the test.

use std::collections::BTreeMap;

use u2s_mapper_aem::canonical::{self, Difference};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/AF_AABF.zip");

/// This form's own two content roots, as they appear in the fixture's own
/// `filter.xml` -- distinguishes "the form's own DAM asset `.content.xml`"
/// (a real, reachable decode target) from its *parent* folders (shared
/// multi-tenant instance scaffolding, out of scope -- see
/// `categorize`'s own doc).
const DAM_ASSET_PATH: &str =
    "jcr_root/content/dam/formsanddocuments/afforms_germany_all/af_aa/AF_AABF/.content.xml";

fn categorize(path: &str) -> &'static str {
    if path.contains("dictionary/") {
        "dictionary entries"
    } else if path.contains("renditions/dorTemplate") {
        "DOR renditions (no binary rendition decode yet)"
    } else if path.contains("META-INF") {
        "META-INF vault packaging metadata"
    } else if path.starts_with(DAM_ASSET_PATH) {
        "this form's own DAM asset"
    } else if path.contains("jcr_root/content/.content.xml") || path.contains("jcr_root/.content.xml") {
        "root JCR scaffolding (shared multi-tenant instance content)"
    } else if path.contains("jcr_root/content/forms") || path.contains("jcr_root/content/dam") {
        "folder .content.xml scaffolding (shared multi-tenant instance content)"
    } else {
        "other (unexpected -- investigate)"
    }
}

#[test]
fn the_real_fixture_round_trip_stays_within_its_documented_gaps() {
    let bytes = std::fs::read(FIXTURE).expect("fixture reads");
    let form = u2s_mapper_aem::decode::decode(&bytes).expect("the real fixture decodes");
    let encoded = u2s_mapper_aem::encode(&form).expect("the decoded form encodes back");
    let diffs = canonical::compare(&bytes, &encoded.bytes);

    let mut by_category: BTreeMap<&'static str, Vec<&Difference>> = BTreeMap::new();
    for diff in &diffs {
        by_category.entry(categorize(&diff.0)).or_default().push(diff);
    }

    if let Some(unexpected) = by_category.get("other (unexpected -- investigate)") {
        panic!(
            "{} canonical difference(s) outside every documented gap category -- \
             a real regression, not an accepted one:\n{}",
            unexpected.len(),
            unexpected.iter().map(|d| d.0.as_str()).collect::<Vec<_>>().join("\n")
        );
    }

    // Upper bounds per category, not exact counts: each is loose enough to
    // absorb a harmless reordering but tight enough that a genuine new loss
    // (a promotion heuristic regressing, a decoder falling back to
    // `Component` where it used to recognise a shape) fails the test.
    let bounds: &[(&str, usize)] = &[
        ("dictionary entries", 220),
        ("DOR renditions (no binary rendition decode yet)", 4),
        ("META-INF vault packaging metadata", 17),
        // Down to one accepted gap: `jcr:content`'s own `jcr:lastModified`
        // audit timestamp, which `dam_chrome` does not carry (it captures
        // `<metadata>`'s own remainder and `jcr:content`'s sibling
        // children, not `jcr:content`'s own attributes -- see
        // `decode::decode_dam_chrome`'s own doc).
        ("this form's own DAM asset", 1),
        ("root JCR scaffolding (shared multi-tenant instance content)", 8),
        // Includes the DAM asset's own *parent* folders
        // (`dam/formsanddocuments/.content.xml`,
        // `afforms_germany_all/.content.xml`, `af_aa/.content.xml`) now
        // that `categorize` tells them apart from the form's own DAM asset
        // `.content.xml` -- a recategorization, not a regression.
        ("folder .content.xml scaffolding (shared multi-tenant instance content)", 31),
    ];
    for (category, bound) in bounds {
        let count = by_category.get(category).map_or(0, Vec::len);
        assert!(
            count <= *bound,
            "{category}: {count} differences, expected at most {bound} -- \
             a regression (or, if intentional, update this bound with the reason)"
        );
    }

    let total: usize = diffs.len();
    assert!(
        total <= 276,
        "{total} total canonical differences between the real fixture and its own \
         decode/encode round trip, expected at most 276 -- see the per-category \
         bounds above for which grew"
    );
}
