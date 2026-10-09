//! Concrete cases ported from `ajila-forms-conversion-engine`'s own test
//! suite (`core/src/tests/mod.rs`), re-expressed against this engine's tools
//! and vocabulary rather than copied verbatim — that suite's exhaustive
//! whole-state-space enumeration (`Blueprint::states()`) was deliberately
//! not ported (see `crates/u2s-xfa/PORTING.md`), so its per-state assertions
//! are re-expressed here as per-control `xfa_set` calls instead.
//!
//! These complement, rather than replace, the generic corpus-wide sweeps in
//! `corpus_state_discovery.rs`/`corpus_interaction.rs`: those catch "did we
//! break something on any of the 41 forms", this catches "does this specific,
//! previously-verified fact about this specific form still hold".

#[allow(dead_code)]
mod support;

use std::path::PathBuf;

use serde_json::json;
use u2s_render_test_harness::{ServerUnderTest, call, error_text, structured};

fn corpus_form(name: &str) -> PathBuf {
    u2s_test_assets::corpus_form(name)
}

fn server() -> ServerUnderTest {
    ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env(
            "U2S_FONT_DIR",
            u2s_test_assets::font_dir().display().to_string(),
        )
        .env(
            "U2S_BLOB_DIR",
            std::env::temp_dir()
                .join("u2s-xfa-ported-cases-blobs")
                .display()
                .to_string(),
        )
}

/// Ported from `test_aaoe_dropdown_has_legal_entity_and_individual_options`
/// and `test_aaoe_exhaustive_produces_two_dropdown_states`: `AAOE_033_IT`'s
/// client-type dropdown must offer exactly "Individual" and "Legal entity".
#[tokio::test]
async fn aaoe_dropdown_has_legal_entity_and_individual_options() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;
    let form = corpus_form("AAOE_033_IT.pdf");

    let all = support::all_controls(&c, json!({ "doc_path": form.display().to_string() })).await;
    let dropdown = all
        .iter()
        .find(|ctrl| ctrl["field"].as_str().unwrap_or_default().ends_with("CL_ClientType"))
        .expect("CL_ClientType must be listed");

    let values: std::collections::BTreeSet<&str> = dropdown["options"]
        .as_array()
        .expect("options")
        .iter()
        .map(|o| o["value"].as_str().expect("value"))
        .collect();
    assert_eq!(
        values,
        std::collections::BTreeSet::from(["Individual", "Legal entity"]),
        "got {values:?}"
    );
    c.cancel().await.ok();
}

/// Ported from `test_aaks_radio_button_has_three_options`: `AAKS_019_DE`'s
/// exemptions radio group has exactly three members.
#[tokio::test]
async fn aaks_exemptions_radio_group_has_three_options() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;
    let form = corpus_form("AAKS_019_DE.pdf");

    let all = support::all_controls(&c, json!({ "doc_path": form.display().to_string() })).await;
    let members: Vec<&str> = all
        .iter()
        .filter(|ctrl| {
            ctrl["group"]
                .as_str()
                .is_some_and(|g| g.ends_with("RB_Group_Exemptions"))
        })
        .map(|ctrl| ctrl["field"].as_str().expect("field"))
        .collect();
    assert_eq!(members.len(), 3, "got {members:?}");
    c.cancel().await.ok();
}

/// Ported from `test_aaab_click_rb3_changes_section_title_to_loeschung`:
/// `AAAB_019_DE`'s section title reads "Neuanlage (…)" by default and
/// changes to "Löschung" once `RB_3` — the third member of
/// `RB_Group_Neuanlage` — is selected. Located through the same
/// `xfa_search_text` tool an agent would use, not an internal API, so this
/// is the real MCP surface's behavior, not the library's.
#[tokio::test]
async fn aaab_selecting_rb3_changes_the_section_title_to_loeschung() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;
    let form = corpus_form("AAAB_019_DE.pdf");

    let opened = call(&c, "xfa_open", json!({ "doc_path": form.display().to_string() })).await;
    let so = structured(&opened);
    let session = so["session"].as_str().expect("session").to_string();

    let before = call(&c, "xfa_search_text", json!({ "session": session, "revision": 0, "query": "Neuanlage" })).await;
    let before_matches = structured(&before)["matches"].as_array().expect("matches").clone();
    assert!(!before_matches.is_empty(), "default state must show the Neuanlage title");
    let page = before_matches[0]["page"].as_u64().expect("page");

    let all = support::all_controls(&c, json!({ "session": session, "revision": 0 })).await;
    let rb3 = all
        .iter()
        .find(|ctrl| ctrl["field"].as_str().unwrap_or_default().ends_with("RB_3"))
        .expect("RB_3 must be listed")
        .clone();
    let field = rb3["field"].as_str().expect("field").to_string();
    let value = rb3["options"][0]["value"].as_str().expect("option value").to_string();

    let set = call(&c, "xfa_set", json!({ "session": session, "expected_revision": 0, "field": field, "value": value })).await;
    let revision = structured(&set)["revision"].as_u64().expect("revision");
    assert_eq!(revision, 1);

    let page_text = call(&c, "xfa_page_text", json!({ "session": session, "revision": revision, "page": page })).await;
    let text = structured(&page_text)["text"].as_str().expect("text");
    assert!(
        text.contains("Löschung"),
        "page {page} after selecting RB_3 should contain the new title, got {text:?}"
    );
    assert!(
        !text.contains("Neuanlage (möglich"),
        "the old title should be gone, got {text:?}"
    );

    call(&c, "xfa_close", json!({ "session": session })).await;
    c.cancel().await.ok();
}

/// `AAGS_019_DE`: choosing the first signature option prefills the sheet
/// number with "1" and locks it, the way the form's own change script does
/// in Acrobat (`TF_Sheet.rawValue = "1"; TF_Sheet.access = "protected"`);
/// the second option clears and unlocks it. The lock must be reported, must
/// be readable afterwards, and must refuse a direct set the way Acrobat
/// refuses typing into the field.
#[tokio::test]
async fn aags_the_signature_option_prefills_and_locks_the_sheet_number() {
    const SHEET: &str = "UBSForms_66352.Page.AccountHolder.Date_Sheet.STP_Sheet.TF_Sheet";
    const OPTION: &str = "UBSForms_66352.Page.SpecimenSignatures.STP_RB_Vertical.RB_Group_yyy";

    support::require_fonts();
    let s = server();
    let c = s.connect().await;
    let form = corpus_form("AAGS_019_DE.pdf");

    let opened = call(&c, "xfa_open", json!({ "doc_path": form.display().to_string() })).await;
    let session = structured(&opened)["session"].as_str().expect("session").to_string();

    let before = call(&c, "xfa_field", json!({ "session": session, "revision": 0, "field": SHEET })).await;
    let before = structured(&before);
    assert_eq!(before["kind"], "text");
    assert_eq!(before["access"], "open", "{before}");

    let first = call(
        &c,
        "xfa_set",
        json!({ "session": session, "expected_revision": 0, "field": format!("{OPTION}.RB_1"), "value": "1" }),
    )
    .await;
    let first = structured(&first).clone();
    assert!(
        first["side_effects"]
            .as_array()
            .expect("side_effects")
            .iter()
            .any(|e| e["field"] == SHEET && e["to"] == "1"),
        "the sheet number is prefilled: {first}"
    );
    assert_eq!(
        first["access_changes"],
        json!([{ "field": SHEET, "from": "open", "to": "protected" }])
    );
    let revision = first["revision"].as_u64().expect("revision");

    let locked = call(&c, "xfa_field", json!({ "session": session, "revision": revision, "field": SHEET })).await;
    let locked = structured(&locked);
    assert_eq!((locked["value"].as_str(), locked["access"].as_str()), (Some("1"), Some("protected")));

    let refused = call(
        &c,
        "xfa_set",
        json!({ "session": session, "expected_revision": revision, "field": SHEET, "value": "7" }),
    )
    .await;
    assert_eq!(refused.is_error, Some(true), "a protected field cannot be set");
    assert!(error_text(&refused).contains("protected"), "{}", error_text(&refused));

    let second = call(
        &c,
        "xfa_set",
        json!({ "session": session, "expected_revision": revision, "field": format!("{OPTION}.RB_2"), "value": "2" }),
    )
    .await;
    let second = structured(&second);
    assert_eq!(
        second["access_changes"],
        json!([{ "field": SHEET, "from": "protected", "to": "open" }])
    );
    assert!(
        second["side_effects"]
            .as_array()
            .expect("side_effects")
            .iter()
            .any(|e| e["field"] == SHEET && e["to"] == ""),
        "the sheet number is cleared again: {second}"
    );

    call(&c, "xfa_close", json!({ "session": session })).await;
    c.cancel().await.ok();
}
