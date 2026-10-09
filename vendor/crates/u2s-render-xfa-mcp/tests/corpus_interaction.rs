//! Requirement: does checking a checkbox / picking a radio / choosing a
//! dropdown value actually change the rendering, not just an internal value?
//!
//! Uses each control's own `positions` entry (crates/u2s-xfa/src/states.rs)
//! to crop exactly its own box via `xfa_render_region` before and after
//! `xfa_set` — a tight, fast check that also doubles as an end-to-end proof
//! that the reported position is real (a wrong rect would crop the wrong
//! pixels and this test would not reliably see them change).

// `support` also carries the oracle used by corpus_state_discovery.rs; this
// file only needs `require_fonts` and `all_controls`.
#[allow(dead_code)]
mod support;

use std::path::PathBuf;

use serde_json::{Value, json};
use u2s_render_test_harness::{Client, ServerUnderTest, call, structured};

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
                .join("u2s-xfa-corpus-interaction-blobs")
                .display()
                .to_string(),
        )
}

/// The value to move a control to, or `None` when it cannot be moved: a
/// radio already on its own (only) value has nothing else to select, since a
/// person deselects a radio by picking a *different* button, not by
/// re-clicking the one that's already on.
fn alternative_value(ctrl: &Value) -> Option<String> {
    let current = ctrl["value"].as_str();
    ctrl["options"]
        .as_array()?
        .iter()
        .filter_map(|o| o["value"].as_str())
        .find(|v| Some(*v) != current)
        .map(str::to_string)
}

/// The region's image data, base64-encoded exactly as the tool returns it —
/// deterministic given the same pixels, so comparing the encoded string is
/// as good as comparing decoded bytes and needs no image dependency here.
async fn crop(c: &Client, session: &str, revision: u64, pos: &Value) -> String {
    let region = call(
        c,
        "xfa_render_region",
        json!({
            "session": session,
            "revision": revision,
            "page": pos["page"],
            "rect_pt": { "x": pos["x"], "y": pos["y"], "width": pos["width"], "height": pos["height"] },
            "dpi": 150,
            "format": "png",
        }),
    )
    .await;
    region
        .content
        .iter()
        .find_map(|b| b.as_image().map(|i| i.data.clone()))
        .expect("inline region image")
}

/// A representative slice of the corpus, chosen to cover all three control
/// kinds with controls whose own box is expected to visibly redraw:
/// `AAAB_019_DE`'s radio group, `AAOE_033_IT`'s dropdown, `AALP_019_EN`'s
/// checkboxes, and `AALQ_019_DE`, whose checkboxes
/// (`Table_AssetClasses.Column{New,Old}.Layout.CB`) live in a 10-row table:
/// each row is its own instance with its own indexed path (`Layout[3].CB`),
/// so one row's checkbox is addressable on its own.
const FORMS: &[&str] = &[
    "AAAB_019_DE.pdf",
    "AAOE_033_IT.pdf",
    "AALP_019_EN.pdf",
    "AALQ_019_DE.pdf",
];

/// Sampled controls per form, capped to keep this test's runtime reasonable
/// across three real forms while still exercising every control kind a form
/// actually has.
const SAMPLE_CAP: usize = 4;

#[tokio::test]
async fn setting_a_control_redraws_its_own_box_and_reset_reverts_it() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;

    let mut findings = String::new();
    for name in FORMS {
        let form = corpus_form(name);
        let opened = call(&c, "xfa_open", json!({ "doc_path": form.display().to_string() })).await;
        let so = structured(&opened);
        let session = so["session"].as_str().expect("session").to_string();
        let base_revision = so["revision"].as_u64().expect("revision");

        let list =
            support::all_controls(&c, json!({ "session": session, "revision": base_revision }))
                .await;

        // One control per exclGroup (siblings would just re-prove the same
        // mechanism), deterministically ordered so failures reproduce.
        let mut seen_groups = std::collections::HashSet::new();
        let mut sampled: Vec<Value> = Vec::new();
        for ctrl in &list {
            if !ctrl["visible"].as_bool().unwrap_or(false) {
                continue;
            }
            // Only choices have a value to move to: buttons are pressed (see
            // the repeatable-section test below), free-value fields take any
            // text. And only an open one can be set at all.
            if !["radio", "checkbox", "dropdown"].contains(&ctrl["kind"].as_str().unwrap_or_default())
                || ctrl["access"] != "open"
            {
                continue;
            }
            // Exactly one position, so there is one box to crop. Every field,
            // each instance of a repeated section included, has its own path;
            // a second position only means the layout draws it twice.
            if ctrl["positions"].as_array().map(Vec::len) != Some(1) {
                continue;
            }
            if let Some(group) = ctrl["group"].as_str()
                && !seen_groups.insert(group.to_string())
            {
                continue;
            }
            sampled.push(ctrl.clone());
            if sampled.len() >= SAMPLE_CAP {
                break;
            }
        }
        assert!(
            !sampled.is_empty(),
            "{name}: no visible, positioned control found to test"
        );

        let mut revision = base_revision;
        let mut redrew = 0;
        let mut unmovable = 0;
        for ctrl in &sampled {
            let field = ctrl["field"].as_str().expect("field").to_string();
            let Some(value) = alternative_value(ctrl) else {
                unmovable += 1;
                continue;
            };
            let pos = ctrl["positions"][0].clone();

            let before = crop(&c, &session, revision, &pos).await;

            let set = call(
                &c,
                "xfa_set",
                json!({ "session": session, "expected_revision": revision, "field": field, "value": value }),
            )
            .await;
            let interaction = structured(&set);
            let new_revision = interaction["revision"].as_u64().expect("revision");
            assert_eq!(
                new_revision,
                revision + 1,
                "{name}: xfa_set on {field} did not advance the revision"
            );

            let after = crop(&c, &session, new_revision, &pos).await;
            let side_effects_explain = !interaction["side_effects"]
                .as_array()
                .map(Vec::is_empty)
                .unwrap_or(true)
                || !interaction["appeared"].as_array().map(Vec::is_empty).unwrap_or(true)
                || !interaction["disappeared"].as_array().map(Vec::is_empty).unwrap_or(true);

            if before != after {
                redrew += 1;
            } else if !side_effects_explain {
                findings.push_str(&format!(
                    "{name}: setting {field}={value} changed neither its own box nor anything the interaction report explains\n"
                ));
            }

            // Reset before the next sampled control, so every control in
            // this form starts from the same baseline.
            let reset = call(&c, "xfa_reset", json!({ "session": session, "expected_revision": new_revision })).await;
            let rs = structured(&reset);
            revision = rs["revision"].as_u64().expect("revision");
            assert_eq!(revision, new_revision + 1);

            let reverted = crop(&c, &session, revision, &pos).await;
            assert_eq!(
                reverted, before,
                "{name}: xfa_reset on {field} did not restore the pre-set rendering"
            );
        }

        assert!(
            redrew > 0 || unmovable == sampled.len(),
            "{name}: none of {} sampled controls redrew their own box",
            sampled.len()
        );

        call(&c, "xfa_close", json!({ "session": session })).await;
    }
    c.cancel().await.ok();

    assert!(findings.is_empty(), "\n{findings}");
}

/// The corpus's repeatable-section pattern, end to end (`AACC_019_DE`): a
/// `Client_Section_DYN` section with `<occur max="5"/>` and, inside it,
/// `STP_PlusMinus.Button_Add`/`Button_Minus`, whose click scripts call the
/// shared `soPlusMinus.insertNode`/`removeNode` (XFA 3.3 §9 instance manager)
/// and renumber every section through `resolveNodes(...[*]...)`.
#[tokio::test]
async fn clicking_add_on_aacc_creates_a_second_client_section_and_minus_removes_it() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;

    let form = corpus_form("AACC_019_DE.pdf");
    let opened = call(
        &c,
        "xfa_open",
        json!({ "doc_path": form.display().to_string() }),
    )
    .await;
    let session = structured(&opened)["session"]
        .as_str()
        .expect("session")
        .to_string();

    let list = support::all_controls(&c, json!({ "session": session, "revision": 0 })).await;
    let add = list
        .iter()
        .find(|c| {
            c["field"]
                .as_str()
                .unwrap_or_default()
                .ends_with("Client_Section_DYN.STP_PlusMinus.Button_Add")
        })
        .expect("the section's add button is listed")
        .clone();
    assert_eq!(add["kind"], "button");
    assert_eq!(add["click"], "instances", "{add}");
    let add_field = add["field"].as_str().unwrap().to_string();
    let section = add_field
        .strip_suffix(".STP_PlusMinus.Button_Add")
        .unwrap()
        .to_string();
    let count_under = |list: &[Value], prefix: &str| {
        list.iter()
            .filter(|c| c["field"].as_str().unwrap_or_default().starts_with(prefix))
            .count()
    };
    let first_count = count_under(&list, &format!("{section}."));
    assert!(first_count > 0);

    let clicked = call(
        &c,
        "xfa_click",
        json!({ "session": session, "expected_revision": 0, "field": add_field }),
    )
    .await;
    let sc = structured(&clicked);
    assert_eq!(sc["instances_changed"], true, "{sc}");
    let second = format!("{section}[1].");
    assert!(
        sc["appeared"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap_or_default().starts_with(&second)),
        "{sc}"
    );

    let after_list = support::all_controls(&c, json!({ "session": session, "revision": 1 })).await;
    assert_eq!(
        count_under(&after_list, &second),
        first_count,
        "the new section has the same controls as the first"
    );

    let minus = call(
        &c,
        "xfa_click",
        json!({
            "session": session,
            "expected_revision": 1,
            "field": format!("{section}[1].STP_PlusMinus.Button_Minus"),
        }),
    )
    .await;
    let sm = structured(&minus);
    assert_eq!(sm["instances_changed"], true, "{sm}");
    let gone = sm["disappeared"].as_array().unwrap();
    assert!(!gone.is_empty(), "{sm}");
    assert!(
        gone.iter()
            .all(|p| p.as_str().unwrap_or_default().starts_with(&second)),
        "only the second section's fields go: {sm}"
    );

    call(&c, "xfa_close", json!({ "session": session })).await;
    c.cancel().await.ok();
}
