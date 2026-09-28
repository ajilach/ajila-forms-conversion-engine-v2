//! Requirement: does checking a checkbox / picking a radio / choosing a
//! dropdown value actually change the rendering, not just an internal value?
//!
//! Uses each control's own `positions` entry (crates/u2s-xfa/src/states.rs)
//! to crop exactly its own box via `xfa_render_region` before and after
//! `xfa_set` — a tight, fast check that also doubles as an end-to-end proof
//! that the reported position is real (a wrong rect would crop the wrong
//! pixels and this test would not reliably see them change).

// `support` also carries the oracle used by corpus_state_discovery.rs; this
// file only needs `require_fonts`.
#[allow(dead_code)]
mod support;

use std::path::PathBuf;

use serde_json::{Value, json};
use u2s_render_test_harness::{Client, ServerUnderTest, call, structured};

fn corpus_form(name: &str) -> PathBuf {
    u2s_xfa::corpus::test_support::form(name)
}

fn server() -> ServerUnderTest {
    ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env(
            "U2S_FONT_DIR",
            u2s_xfa::fonts::test_support::font_dir()
                .expect("fonts")
                .display()
                .to_string(),
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
/// checkboxes. Deliberately not `AALQ_019_DE`, whose only checkboxes
/// (`Table_AssetClasses.Column{New,Old}.Layout.CB`) each live inside a
/// 10-row repeated table section with no per-row index in their SOM path —
/// every row's `Control` carries the identical `field` string (a real,
/// separate limitation: `xfa_set` has no way to address one specific row),
/// which `positions.len() > 1` already flags and this test's sampling skips
/// rather than silently exercising.
const FORMS: &[&str] = &["AAAB_019_DE.pdf", "AAOE_033_IT.pdf", "AALP_019_EN.pdf"];

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

        let controls = call(&c, "xfa_controls", json!({ "session": session, "revision": base_revision })).await;
        let sc = structured(&controls);
        let list = sc["controls"].as_array().expect("controls array").clone();

        // One control per exclGroup (siblings would just re-prove the same
        // mechanism), deterministically ordered so failures reproduce.
        let mut seen_groups = std::collections::HashSet::new();
        let mut sampled: Vec<Value> = Vec::new();
        for ctrl in &list {
            if !ctrl["visible"].as_bool().unwrap_or(false) {
                continue;
            }
            // Exactly one position: a control repeated across several table
            // rows shares one ambiguous SOM path (see the module doc's
            // `AALQ_019_DE` note) — `xfa_set` cannot address a single row of
            // it, so cropping any one of its positions would not be testing
            // what this test claims to test.
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
