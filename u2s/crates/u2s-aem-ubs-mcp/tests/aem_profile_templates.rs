//! Invariants over the UBS profile's own template files
//! (`profiles/ubs/aem/custom/*.xml`), ported from `blueprint` (the deleted
//! `core` crate)'s `tests/mod.rs` (`ajilach/ajila-forms-conversion-engine` at
//! commit `f5f596a`, the last commit before `core/` was deleted).
//!
//! These read the templates straight off disk -- no `AemNode` tree, no
//! rendering -- so they hold regardless of the (now-gone) mechanical
//! conversion.

use std::collections::HashSet;
use std::path::Path;

fn custom_templates_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles/ubs/aem/custom")
}

fn read_custom_template(name: &str) -> String {
    let path = custom_templates_dir().join(format!("{name}.xml"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

/// UBS directive (2026-08-20, "AF Fragments and Common Fields with XSD
/// List"): a form references the generic PARTNER fragments and the generic
/// signature fragment from `afforms_ubs_fragmentlib`, never the market
/// person/signature fragments those libraries are being emptied of. The
/// custom templates are the engine's only source of partner and signature
/// fragment references, so pin them: every fragRef they emit is either a UBS
/// generic or one of the deliberately market-specific leftovers (internal
/// bank use).
#[test]
fn test_custom_templates_reference_only_ubs_generic_fragments() {
    let dir = custom_templates_dir();
    let mut seen_generics = 0usize;
    for entry in std::fs::read_dir(&dir).expect("custom templates dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("xml") {
            continue;
        }
        let xml = std::fs::read_to_string(&path).expect("read template");
        for cap in xml.split("fragRef=\"").skip(1) {
            let frag = cap.split('\"').next().unwrap_or_default();
            let market_ok = frag.contains("internalbankuse") || frag.contains("InternalBankUse");
            assert!(
                frag.contains("afforms_ubs_fragmentlib") || market_ok,
                "{:?} references the market fragment {frag:?}; partner and signature \
                 blocks must use the UBS generics (affrg_ContractualPartnerGeneric1, \
                 affrg_PartnertoPartnerGeneric1, affrg_SignatureGeneric1)",
                path.file_name().unwrap()
            );
            if frag.contains("afforms_ubs_fragmentlib") {
                seen_generics += 1;
            }
        }
        // The host authors the signer-name fill: each signatures template
        // must carry the hidden anchor field with a Calculate document per
        // pair.
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("signatures") {
            assert!(
                xml.contains("TXT_Signatures_Name_Calc") && xml.contains("fd:calc"),
                "{name} must carry the signer-name calc anchor"
            );
            assert!(
                xml.contains("TXT_Name_Generic"),
                "{name}'s calc must write the generic signature's name field"
            );
        }
    }
    assert!(
        seen_generics >= 8,
        "expected the partner and signature panels across the four templates, found {seen_generics}"
    );
}

/// The internal-bank-use block uses the global fragment, in both markets.
///
/// The deployed corpus migrated: of the 78 Italian packages issued
/// 2026-09-01, 57 reference
/// `afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature`
/// and 4 still carry `afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref`.
/// The German reference AABF_019 uses the global fragment too.
#[test]
fn custom_templates_use_the_global_internal_bank_use_fragment() {
    let dir = custom_templates_dir();
    let mut seen = 0usize;
    for entry in std::fs::read_dir(&dir).expect("custom templates dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("xml") {
            continue;
        }
        let xml = std::fs::read_to_string(&path).expect("read template");
        if xml.contains("internalbankuse") || xml.contains("InternalBankUse") {
            assert!(
                xml.contains(
                    "afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature"
                ),
                "{:?} must reference the global internal-bank-use fragment, not a market one",
                path.file_name().unwrap()
            );
            seen += 1;
        }
    }
    assert!(
        seen > 0,
        "expected at least one custom template referencing the internal-bank-use block"
    );
}

/// The form configurator opens on "Private Person": the radio ships with
/// option `1` already selected.
///
/// UBS asked for the preselection (feedback
/// PROBLEM-formconfig-private-person-default) and the mechanism is the
/// component's `_value`, not a `default` attribute -- the widget renders an
/// option checked by comparing its key to `_value`, so a `default` deploys
/// cleanly and preselects nothing.
#[test]
fn the_form_configurator_preselects_private_person() {
    let xml = read_custom_template("formular_adressat_radio");

    assert!(
        xml.contains(r#"_value="1""#),
        "the configurator radio must preselect option 1:\n{xml}"
    );
    assert!(
        xml.contains("options=\"[1="),
        "option 1 must be the Private Person key the preselection points at:\n{xml}"
    );
    assert!(
        !xml.contains(r#"default="1""#),
        "`default` does not preselect anything -- use `_value`:\n{xml}"
    );
}

/// The UBS configurator itself goes through the `formular_adressat_radio` /
/// `tipo_radio` custom templates, whose panels live in the `account_holder`
/// and `signatures` templates they depend on. Those cannot be derived from
/// the node tree, so the script is written out in the template -- and has to
/// stay in step with the panels those templates actually define.
#[test]
fn the_custom_configurator_radios_reset_the_panels_their_templates_define() {
    for (radio, panel_sources, expected_panels) in [
        (
            "formular_adressat_radio",
            vec!["account_holder", "signatures"],
            vec![
                "PN_IndividualContainer",
                "PN_LegalGuardianContainer",
                "PN_LegalEntityContainer",
                "PN_165356b3",
                "PN_165356b3_copy_1",
                "PN_204491df",
            ],
        ),
        (
            "tipo_radio",
            vec!["account_holder_it", "signatures_it"],
            vec![
                "PN_IndividualContainer",
                "PN_LegalEntityContainer",
                "PN_165356b3",
                "PN_204491df",
            ],
        ),
    ] {
        let script = read_custom_template(radio);
        assert!(
            script.contains("fd:valueCommit="),
            "{radio} must carry the reset"
        );
        for panel in &expected_panels {
            assert!(
                script.contains(&format!("{panel}.resetData();")),
                "{radio} must empty {panel}"
            );
        }
        // Every panel the reset names must really exist in the templates
        // this radio depends on -- otherwise the script silently throws at
        // runtime.
        let defined: HashSet<String> = panel_sources
            .iter()
            .flat_map(|src| {
                let body = read_custom_template(src);
                body.match_indices("name=\"PN_")
                    .map(|(i, _)| {
                        let rest = &body[i + "name=\"".len()..];
                        rest[..rest.find('"').unwrap()].to_string()
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for panel in &expected_panels {
            assert!(
                defined.contains(*panel),
                "{radio} resets {panel}, which {panel_sources:?} does not define"
            );
        }
    }
}
