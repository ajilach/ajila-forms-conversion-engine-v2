//! Requirement: does `xfa_controls` expose every changeable input (checkbox,
//! radio, dropdown) a real UBS form actually has?
//!
//! Ground truth comes from `support::oracle_controls`, an independent walk of
//! the parsed XFA template that shares no code with `u2s_xfa::exhaustive`
//! (the module `xfa_controls` itself is built on) — so a bug in that
//! module's field collector cannot also be baked into the check.

mod support;

use std::path::PathBuf;

use serde_json::{Value, json};
use u2s_render_test_harness::ServerUnderTest;

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
                .join("u2s-xfa-corpus-discovery-blobs")
                .display()
                .to_string(),
        )
}

fn reported_fields(controls: &[Value]) -> Vec<String> {
    controls
        .iter()
        .map(|c| c["field"].as_str().expect("field").to_string())
        .collect()
}

/// A representative slice of the corpus, not all 41 forms: enough to cover
/// every control kind (radio, checkbox, dropdown) and the corpus's densest
/// form, while keeping this test fast enough to run by default.
const FORMS: &[&str] = &[
    "AAAA_019_DE.pdf",
    "AAAB_019_DE.pdf",
    "AAOE_033_IT.pdf",
    "AAKS_019_DE.pdf",
    "AABK_019_DE.pdf",
    "AACC_019_DE.pdf",
];

/// The core claim: nothing the template declares as a checkbox, radio or
/// dropdown is silently missing from `xfa_controls`. Extra entries the
/// oracle does not know about are fine here — `xfa_controls` also reports
/// controls the render layer discovers dynamically (e.g. script-populated
/// dropdown items) that a purely static template walk cannot see.
#[tokio::test]
async fn every_form_reports_every_oracle_control() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;

    let mut missing_report = String::new();
    for name in FORMS {
        let form = corpus_form(name);
        let expected = support::oracle_controls(&form);
        assert!(!expected.is_empty(), "{name}: oracle found no controls at all");

        let controls =
            support::all_controls(&c, json!({ "doc_path": form.display().to_string() })).await;
        let reported: std::collections::BTreeSet<String> =
            reported_fields(&controls).into_iter().collect();

        let missing: Vec<&String> = expected.difference(&reported).collect();
        if !missing.is_empty() {
            missing_report.push_str(&format!(
                "{name}: {} of {} oracle controls missing from xfa_controls: {missing:?}\n",
                missing.len(),
                expected.len()
            ));
        }
    }
    c.cancel().await.ok();

    assert!(missing_report.is_empty(), "\n{missing_report}");
}

/// Internal consistency, no oracle needed: every reported field is unique,
/// every radio names its group, every checkbox has exactly two options, and
/// every button has no options, no group, and a known `click` effect.
/// A form-wide sweep rather than one hand-picked control, since a violation
/// specific to one exclGroup shape would otherwise need its own form to show
/// up.
#[tokio::test]
async fn the_controls_listing_is_internally_consistent_on_every_form() {
    support::require_fonts();
    let s = server();
    let c = s.connect().await;

    for name in FORMS {
        let form = corpus_form(name);
        let list =
            support::all_controls(&c, json!({ "doc_path": form.display().to_string() })).await;

        let mut seen = std::collections::HashSet::new();
        for ctrl in &list {
            let field = ctrl["field"].as_str().expect("field");
            assert!(seen.insert(field.to_string()), "{name}: duplicate field {field}");

            // Every field has an XFA access keyword (XFA 3.3 §17), and names
            // the container it inherits from only when that container is
            // what locks it.
            let access = ctrl["access"].as_str().expect("access");
            assert!(
                ["open", "readOnly", "protected", "nonInteractive"].contains(&access),
                "{name}: {field} has access {access}"
            );
            assert!(
                ctrl.get("access_from").is_none() || access != "open",
                "{name}: open field {field} names a locking container"
            );

            match ctrl["kind"].as_str().expect("kind") {
                "radio" => assert!(
                    ctrl["group"].is_string(),
                    "{name}: radio {field} has no group"
                ),
                "checkbox" => {
                    let options = ctrl["options"].as_array().expect("options");
                    assert_eq!(
                        options.len(),
                        2,
                        "{name}: checkbox {field} has {} options, want 2",
                        options.len()
                    );
                    let values: std::collections::HashSet<&str> = options
                        .iter()
                        .map(|o| o["value"].as_str().expect("value"))
                        .collect();
                    assert_eq!(
                        values.len(),
                        2,
                        "{name}: checkbox {field}'s two options share a value"
                    );
                }
                "button" => {
                    assert!(
                        ctrl["options"].as_array().is_none_or(Vec::is_empty),
                        "{name}: button {field} has options"
                    );
                    assert!(
                        ctrl["group"].is_null(),
                        "{name}: button {field} has a group"
                    );
                    assert!(
                        ctrl.get("click").is_none()
                            || ctrl["click"] == "instances"
                            || ctrl["click"] == "script",
                        "{name}: button {field} has click {}",
                        ctrl["click"]
                    );
                }
                _ => {}
            }
        }
    }
    c.cancel().await.ok();
}
