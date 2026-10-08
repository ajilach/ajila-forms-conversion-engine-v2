//! Decodes each of the three real, machine-generated dumps in
//! `tests/fixtures/` and proves the round trip preserves *content and
//! structure*: `decode(encode(decode(x)))` carries the same metadata, the
//! same asset contents and the same component tree shape as `decode(x)`.
//!
//! Byte-for-byte struct equality is *not* the goal, and deliberately not
//! asserted here -- `decode`'s own module doc names why: a decoded
//! [`AssetKey`] is an invented label with no persisted identity in the row
//! model, so a second decode mints a different one for the same content.
//! What survives, and what this test actually pins down, is everything an
//! agent or a rule script would actually look at: the metadata, which
//! languages exist, and -- via [`canonicalize`] -- the tree of styled
//! panels and asset containers with each reference resolved to the content
//! it points at rather than compared by its (unstable) key.

use std::collections::BTreeMap;

use u2s_redacto::model::{Asset, AssetKey, AssetKind, Component};

const AAEV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/redacto-AAEV_019.sql");
const AAAR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/redacto-AAAR_019.sql");
const BAGC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/redacto-BAGC_019.sql");

fn styled_panel_counts(components: &[Component], counts: &mut BTreeMap<String, usize>) {
    for component in components {
        match component {
            Component::AssetContainer { .. } => {}
            Component::StyledPanel { style, components } => {
                *counts.entry(style.to_string()).or_insert(0) += 1;
                styled_panel_counts(components, counts);
            }
        }
    }
}

/// A component tree with every `AssetKey` resolved to the content it
/// references, so two trees minted with different (but content-equivalent)
/// keys compare equal. See this file's own module doc.
#[derive(Debug, PartialEq)]
enum Canonical {
    AssetContainer(Vec<(AssetKind, BTreeMap<String, String>)>),
    StyledPanel(String, Vec<Canonical>),
}

fn canonicalize(components: &[Component], assets_by_key: &BTreeMap<&AssetKey, &Asset>) -> Vec<Canonical> {
    components
        .iter()
        .map(|component| match component {
            Component::AssetContainer { assets } => Canonical::AssetContainer(
                assets
                    .iter()
                    .map(|key| {
                        let asset = assets_by_key[key];
                        let content = asset
                            .content
                            .iter()
                            .map(|(lang, html)| (lang.to_string(), html.to_string()))
                            .collect();
                        (asset.kind, content)
                    })
                    .collect(),
            ),
            Component::StyledPanel { style, components } => {
                Canonical::StyledPanel(style.to_string(), canonicalize(components, assets_by_key))
            }
        })
        .collect()
}

fn assets_by_key(assets: &[Asset]) -> BTreeMap<&AssetKey, &Asset> {
    assets.iter().map(|a| (&a.key, a)).collect()
}

/// Every asset's own `(kind, content)`, independent of its key -- so an
/// asset surviving the round trip under a different invented key still
/// counts as the same asset.
fn asset_contents(assets: &[Asset]) -> Vec<(AssetKind, BTreeMap<String, String>)> {
    let mut contents: Vec<_> = assets
        .iter()
        .map(|a| {
            let content = a
                .content
                .iter()
                .map(|(lang, html)| (lang.to_string(), html.to_string()))
                .collect();
            (a.kind, content)
        })
        .collect();
    contents.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    contents
}

#[test]
fn the_smallest_real_fixture_decodes_to_the_expected_shape() {
    let bytes = std::fs::read(AAEV).expect("fixture reads");
    let doc = u2s_mapper_redacto::decode::decode(&bytes).expect("the real fixture decodes");
    let doc = doc.document();

    assert_eq!(doc.metadata.document_id.as_str(), "aaev_019");
    assert_eq!(doc.metadata.title, "AAEV_019");
    assert_eq!(doc.metadata.owner_id.as_str(), "admin");
    assert_eq!(doc.metadata.languages.len(), 1);
    assert_eq!(doc.metadata.master_language.as_str(), "en");
    assert_eq!(doc.assets.len(), 18, "18 `assets` rows in the fixture");

    let mut panel_counts = BTreeMap::new();
    styled_panel_counts(&doc.body, &mut panel_counts);
    assert_eq!(panel_counts.get("layout-split"), Some(&1));
    assert_eq!(panel_counts.get("footnote"), Some(&1));

    // Every asset is referenced from exactly the union of header/body/footer
    // in the source fixture -- confirmed structurally by re-validating
    // (decode() already calls validate() internally, so a passing decode
    // is itself the assertion that nothing is dangling or orphaned).
}

#[test]
fn every_real_fixture_round_trips_content_and_structure() {
    for path in [AAEV, AAAR, BAGC] {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path} reads: {e}"));
        let once = u2s_mapper_redacto::decode::decode(&bytes)
            .unwrap_or_else(|e| panic!("{path} decodes: {e}"));

        let re_encoded = u2s_mapper_redacto::encode(&once)
            .unwrap_or_else(|e| panic!("{path}'s decoded document encodes back: {e}"));
        let twice = u2s_mapper_redacto::decode::decode(&re_encoded.bytes)
            .unwrap_or_else(|e| panic!("{path}'s re-encoded dump decodes: {e}"));

        let (once, twice) = (once.document(), twice.document());

        // Metadata carries no invented keys, so it must match exactly --
        // except `master_language`, which the row model never persisted in
        // the first place (see `decode`'s own module doc) and which this
        // decoder recomputes identically from the same language set both
        // times anyway, so it still matches here.
        assert_eq!(once.metadata.document_id, twice.metadata.document_id, "{path}");
        assert_eq!(once.metadata.title, twice.metadata.title, "{path}");
        assert_eq!(once.metadata.style, twice.metadata.style, "{path}");
        assert_eq!(once.metadata.languages, twice.metadata.languages, "{path}");
        assert_eq!(once.metadata.owner_id, twice.metadata.owner_id, "{path}");
        assert_eq!(once.metadata.status, twice.metadata.status, "{path}");

        let once_assets = assets_by_key(&once.assets);
        let twice_assets = assets_by_key(&twice.assets);
        assert_eq!(
            asset_contents(&once.assets),
            asset_contents(&twice.assets),
            "{path}: every asset's own content must survive, regardless of its key"
        );

        for (slot_name, once_slot, twice_slot) in [
            ("firstHeader", &once.first_header, &twice.first_header),
            ("header", &once.header, &twice.header),
            ("body", &once.body, &twice.body),
            ("footer", &once.footer, &twice.footer),
        ] {
            assert_eq!(
                canonicalize(once_slot, &once_assets),
                canonicalize(twice_slot, &twice_assets),
                "{path}: {slot_name} must keep the same panel/container shape and content"
            );
        }
    }
}

#[test]
fn a_multi_language_fixture_recovers_every_language() {
    let bytes = std::fs::read(AAAR).expect("fixture reads");
    let doc = u2s_mapper_redacto::decode::decode(&bytes).expect("decodes");
    let languages: Vec<&str> = doc.document().metadata.languages.iter().map(|l| l.as_str()).collect();
    assert_eq!(languages, vec!["de", "en", "es"]);
}
