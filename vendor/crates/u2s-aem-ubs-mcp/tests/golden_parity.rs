//! Parity with the retired deterministic engine (`tests/fixtures/golden/`).
//!
//! Each golden form carries `document.json`: the UBS document the old engine
//! built for that form, lifted from its own AEM tree and dictionary in the same
//! run that wrote `package.zip` and `schema.xsd`. Encoding it must reproduce the
//! XSD exactly. The packages themselves are no longer compared: the normalize
//! passes now shape the form differently from the retired engine (a section
//! heading's body moves into panels of its own, `headings_leave_table_mode`).
//!
//! The account-holder cluster and the signature block of `AABF_019` and
//! `AAOS_033_IT` were custom templates in the retired engine; the documents now
//! author them as ordinary nodes. Those subtrees ([`REAUTHORED`]) are left out
//! of the document whose schema is compared, as the templates contributed
//! nothing to it.

use std::path::{Path, PathBuf};

use u2s_aem_ubs_mcp::{UbsAemDocument, encode};

mod support;
use support::package::{first_difference, unzip};

const FORMS: &[&str] = &["AAOS_033_IT", "AAEV_019_EN", "AABF_019"];

/// The `name`s of the subtrees re-authored since the golden run: in the golden
/// packages the roots the retired custom templates wrote, in the documents the
/// ordinary nodes that replace them.
const REAUTHORED: &[&str] = &[
    // The configurator choice, `formular_adressat_radio` and `tipo_radio`.
    "RB_FormularAdressat",
    "RB_GroupTipo",
    // The account-holder cluster, `account_holder` and `account_holder_it`.
    "PN_AccountHolder",
    "PN_DatiDelIClienteIDiSeguitoIl_5156bd48",
    // The signature block, `signatures` and `signatures_it`.
    "PN_SignatureBlock",
    "PN_Signatures_de979668",
    "PN_FirmaE_de979668",
    // The DoR header slot draws. The retired engine derived the legal entity's
    // from the master-page header and wrote it inside `PN_BR`; the documents
    // author it, and the validity line beside it, as their own `TextDraw`s.
    "ST_HeaderSlot2",
    "ST_HeaderSlot3",
    // The step titles of the steps those land on: a step whose inputs repeat
    // hands its jump-to-field button to the rows (`panel.xml`), and the
    // templates' contents were opaque to that rule.
    "PN_FormConfigurator_a2b5bc39Title",
    "PN_SignaturesTitle",
    "PN_FormConfigurator_b550357eTitle",
    "PN_FirmaE_d942e2baTitle",
];

fn golden_dir(form: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(form)
}

fn golden_document(form: &str) -> serde_json::Value {
    let json = std::fs::read_to_string(golden_dir(form).join("document.json")).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn encode_json(form: &str, json: &serde_json::Value) -> u2s_aem_ubs_mcp::UbsAemBuild {
    let doc = UbsAemDocument::from_json(json)
        .unwrap_or_else(|e| panic!("{form}: document.json is a document: {e}"));
    encode(&doc).unwrap_or_else(|e| panic!("{form} encodes: {e}"))
}

fn encode_golden(form: &str) -> u2s_aem_ubs_mcp::UbsAemBuild {
    encode_json(form, &golden_document(form))
}

/// `node` without the [`REAUTHORED`] subtrees.
fn without_reauthored(mut node: serde_json::Value) -> serde_json::Value {
    if let Some(children) = node.get_mut("children").and_then(|c| c.as_array_mut()) {
        children.retain(|c| !c["name"].as_str().is_some_and(|n| REAUTHORED.contains(&n)));
        for child in children.iter_mut() {
            *child = without_reauthored(child.take());
        }
    }
    node
}

#[test]
fn encoded_schemas_match_the_golden_schemas() {
    for form in FORMS {
        let mut doc = golden_document(form);
        doc["form"] = without_reauthored(doc["form"].take());
        let build = encode_json(form, &doc);
        let golden = std::fs::read_to_string(golden_dir(form).join("schema.xsd")).unwrap();
        let xsd = build
            .xsd
            .unwrap_or_else(|| panic!("{form}: the UBS profile has a schema"));
        assert!(
            golden == xsd,
            "{form}: schema differs; {}",
            first_difference(&golden, &xsd)
        );
    }
}

/// The issued forms keep the legal entity's DoR header slot draw inside the
/// banking relationship's wrapper. Lifted, it is the document's own `TextDraw`
/// right after the `Preface`, in its slot and with its text.
#[test]
fn a_decoded_header_slot_is_a_draw_after_the_preface() {
    for (form, text) in [
        ("AAOS_033_IT", "<b>UBS Europe SE</b> (Succursale Italia)"),
        ("AAEV_019_EN", "<b>UBS Europe SE</b>"),
    ] {
        let package = std::fs::read(golden_dir(form).join("package.zip")).unwrap();
        let doc = serde_json::to_value(u2s_aem_ubs_mcp::decode(&package).unwrap()).unwrap();
        let page = &doc["form"]["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["is_page"] == true)
            .unwrap_or_else(|| panic!("{form}: a first page"))["children"];
        let at = page
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c["type"] == "Preface")
            .unwrap_or_else(|| panic!("{form}: the preface"));
        let draw = &page[at + 1];
        assert_eq!(draw["type"], "TextDraw", "{form}: {draw}");
        assert_eq!(draw["dor_header_slot"], "slot2", "{form}: {draw}");
        assert_eq!(draw["visible"], false, "{form}: {draw}");
        let content = draw["content"].as_object().unwrap();
        assert!(
            content.values().all(|t| t.as_str().unwrap().contains(text)),
            "{form}: {draw}"
        );
    }
}

/// The re-authored account-holder clusters behave as the custom templates did:
/// the configurator shows each party's container, each party's Add and Remove
/// drive its signature twin, and a partner generic hides the sub-panels the
/// form does not need. The packages pass the form checks.
#[test]
fn the_reauthored_clusters_pair_each_party_with_its_signature() {
    for (form, configurator, twins) in [
        (
            "AABF_019",
            "RB_FormularAdressat",
            &["RCP_SGN_CPGRP_repeat", "RCP_Sign_AHGRP_repeat", "RCP_Sign_AHGRP_AR_repeat"][..],
        ),
        (
            "AAOS_033_IT",
            "RB_GroupTipo",
            &["RCP_SGN_CPGRP_repeat", "RCP_Sign_AHGRP_repeat"][..],
        ),
    ] {
        let files = unzip(&encode_golden(form).package);
        let xml = files
            .values()
            .find(|text| text.contains("guideContainer"))
            .unwrap_or_else(|| panic!("{form}: the package holds the form"));
        for twin in twins {
            assert!(
                xml.contains(&format!("window.forms.ubs.addInstance({twin});"))
                    && xml.contains(&format!("window.forms.ubs.removeInstance({twin});")),
                "{form}: a party's buttons must drive {twin}"
            );
        }
        assert!(
            xml.contains("window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);"),
            "{form}: the contracting party hides its entity sub-panel"
        );
        assert!(
            xml.contains(&format!("{configurator}.value == \\\\&quot;1\\\\&quot;")),
            "{form}: the configurator shows the individual's container"
        );
        // The configurator opens on the individual (feedback
        // PROBLEM-formconfig-private-person-default): its option key as `_value`.
        let radio = xml
            .split('<')
            .find(|element| element.contains(&format!("name=\"{configurator}\"")))
            .unwrap_or_else(|| panic!("{form}: the configurator is in the form"));
        assert!(
            radio.contains(r#"_value="1""#),
            "{form}: the configurator must preselect option 1: {radio}"
        );
        // Switching option empties the panels the choice decides (feedback #107).
        for panel in ["PN_IndividualContainer", "PN_LegalEntityContainer"] {
            assert!(
                xml.contains(&format!("{panel}.resetData();")),
                "{form}: the configurator must empty {panel}"
            );
        }
        u2s_aem_ubs_mcp::aem::validate_aem_form_xml(xml)
            .unwrap_or_else(|errors| panic!("{form}: {errors:?}"));
    }
}
