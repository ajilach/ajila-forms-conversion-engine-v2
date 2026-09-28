//! The real binary, over real stdio MCP -- no Docker, no AEM, no Chromium.
//!
//! Proves the three things registration depends on (the manifest parses,
//! the declared tools match `tools/list`, `verify_run` is the one tool
//! declaring `side_effecting`/`verify`), plus that every declared tool
//! answers without touching anything external -- the Docker-backed path
//! (`verify_run` with `dry_run: false`) is `#[ignore]`d live coverage, not
//! this suite's job.

use u2s_render_test_harness::ServerUnderTest;

/// The minimal environment `Profile::from_env` requires, plus a per-test
/// blob dir so parallel tests never collide.
fn test_server(label: &str) -> ServerUnderTest {
    let blob_dir = std::env::temp_dir().join(format!(
        "u2s-aem-ubs-verify-mcp-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    ServerUnderTest::locate("u2s-aem-ubs-verify-mcp", "verify")
        .env("U2S_AEM_VERIFY_FORMAT", "aem-ubs")
        .env(
            "U2S_AEM_VERIFY_IMAGE",
            "ajila.azurecr.io/aemforms-arm:6.5.17.0",
        )
        .env("U2S_AEM_VERIFY_USER", "admin")
        .env("U2S_AEM_VERIFY_PASSWORD", "admin")
        .env("U2S_BLOB_DIR", blob_dir.display().to_string())
}

/// The real, human-authored UBS fixture `u2s-aem-ubs-mcp` itself tests
/// against -- shared here rather than duplicated, matching how
/// `u2s-aem-verify-mcp`'s own e2e suite reuses `u2s-aem-mcp`'s fixture.
fn af_aabf_zip() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-mapper-aem/tests/fixtures/AF_AABF.zip")
}

fn call(tool: &str, args: serde_json::Value) -> rmcp::model::CallToolRequestParams {
    rmcp::model::CallToolRequestParams::new(tool.to_owned()).with_arguments(
        args.as_object()
            .expect("test-provided arguments are always an object")
            .clone(),
    )
}

#[tokio::test]
async fn the_manifest_parses_and_declares_no_format_module() {
    let server = test_server("manifest");
    let client = server.connect().await;

    let raw = u2s_render_test_harness::read_manifest(&client).await;
    let manifest = u2s_mcp::ServerManifest::parse(&raw).expect("the manifest must parse");
    manifest
        .check_compatible()
        .expect("the contract major must be one this workspace understands");

    assert!(
        manifest.format.is_none(),
        "a verifier never registers a format -- that is its encoder's job"
    );
    // verify_status, verify_package_check, verify_run, plus the nine
    // shared interactive tools (`u2s_aem_verify_core::specs_shared`).
    assert_eq!(manifest.tools.len(), 12);

    let by_name = |name: &str| {
        manifest
            .tools
            .iter()
            .find(|t| t.tool == name)
            .unwrap_or_else(|| panic!("no tool named {name:?} in the manifest"))
    };

    for name in ["verify_status", "verify_package_check", "verify_run"] {
        let tool = by_name(name);
        assert_eq!(tool.role, u2s_mcp::ToolRole::Query);
        assert_eq!(
            tool.scope.output_formats,
            vec![u2s_mcp::FormatId::parse("aem-ubs").unwrap()]
        );
    }

    assert!(!by_name("verify_status").side_effecting);
    assert!(!by_name("verify_package_check").side_effecting);
    assert!(by_name("verify_run").side_effecting);
    assert_eq!(
        by_name("verify_run").verify,
        Some(u2s_mcp::manifest::VerifyCapability::Run)
    );

    // Only `verify_run` ever declares the `verify` capability -- the
    // interactive tools are a different, side-effecting-but-not-`verify`
    // surface (see `u2s_aem_verify_core::specs_shared`'s own doc).
    for name in [
        "verify_open",
        "verify_controls",
        "verify_set",
        "verify_next",
        "verify_prev",
        "verify_reset",
        "verify_screenshot",
        "verify_submit",
        "verify_close",
    ] {
        assert_eq!(by_name(name).verify, None, "{name}");
    }
    for name in ["verify_controls", "verify_screenshot"] {
        assert!(!by_name(name).side_effecting, "{name}");
    }
    for name in [
        "verify_open",
        "verify_set",
        "verify_next",
        "verify_prev",
        "verify_reset",
        "verify_submit",
        "verify_close",
    ] {
        assert!(by_name(name).side_effecting, "{name}");
    }

    client.cancel().await.ok();
}

#[tokio::test]
async fn tools_list_matches_the_manifests_own_declared_tools() {
    let server = test_server("tools-list");
    let client = server.connect().await;

    let listed = client.list_tools(None).await.expect("tools/list");
    let mut names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "verify_close",
            "verify_controls",
            "verify_next",
            "verify_open",
            "verify_package_check",
            "verify_prev",
            "verify_reset",
            "verify_run",
            "verify_screenshot",
            "verify_set",
            "verify_status",
            "verify_submit",
        ]
    );

    for tool in &listed.tools {
        assert!(
            !tool.description.as_deref().unwrap_or_default().is_empty(),
            "{}'s description is prompt surface -- it must not be empty",
            tool.name
        );
    }

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_status_reports_this_profiles_configuration() {
    let server = test_server("status");
    let client = server.connect().await;

    let result = client
        .call_tool(call("verify_status", serde_json::json!({})))
        .await
        .expect("verify_status must answer");
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);

    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["format"], "aem-ubs");
    assert_eq!(
        structured["aem_image"],
        "ajila.azurecr.io/aemforms-arm:6.5.17.0"
    );
    assert!(
        structured["docker_reachable"].is_boolean(),
        "docker_reachable must always be a real boolean, whatever Docker's state on this \
         machine is: {structured}"
    );

    client.cancel().await.ok();
}

/// The one thing only this binary must do that the generic verifier never
/// has to: `verify_package_check`'s structured result carries a UBS-only
/// `ubs` object naming the form's own authored mandator/language entities
/// and which one this server would open the form as -- derived offline
/// from the real fixture's own metadata component, before ever installing
/// it on an AEM instance.
#[tokio::test]
async fn verify_package_check_reports_the_ubs_metadata_extension() {
    let server = test_server("package-check-ubs");
    let client = server.connect().await;

    let result = client
        .call_tool(call(
            "verify_package_check",
            serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
        ))
        .await
        .expect("verify_package_check must answer");
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);

    let structured = result.structured_content.expect("structured content");
    assert_eq!(
        structured["form_jcr_path"],
        "/content/forms/af/afforms_germany_all/af_aa/AF_AABF"
    );
    assert_eq!(structured["ubs"]["formcode"], "AABF");
    assert!(
        structured["ubs"]["selected_mandator"].is_string(),
        "a real UBS fixture must resolve to a selected mandator: {structured}"
    );

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_package_check_refuses_a_file_that_is_not_a_zip_at_all() {
    let server = test_server("package-check-invalid");
    let client = server.connect().await;

    let bad_path = std::env::temp_dir().join(format!(
        "u2s-aem-ubs-verify-mcp-not-a-zip-{}.bin",
        std::process::id()
    ));
    std::fs::write(&bad_path, b"this is not a zip file").unwrap();

    let result = client
        .call_tool(call(
            "verify_package_check",
            serde_json::json!({ "package_path": bad_path.display().to_string() }),
        ))
        .await
        .expect("must still answer over the protocol");
    assert_eq!(
        result.is_error,
        Some(true),
        "a non-zip file must be refused"
    );

    let _ = std::fs::remove_file(&bad_path);
    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_run_dry_run_touches_nothing_and_reports_the_package_as_valid() {
    let server = test_server("dry-run");
    let client = server.connect().await;

    let result = client
        .call_tool(call(
            "verify_run",
            serde_json::json!({
                "package_path": af_aabf_zip().display().to_string(),
                "dry_run": true,
            }),
        ))
        .await
        .expect("verify_run must answer");
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);

    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["dry_run"], true);
    assert_eq!(structured["steps"].as_array().map(Vec::len), Some(0));
    assert_eq!(structured["artefacts"].as_array().map(Vec::len), Some(0));
    let errors = structured["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter(|f| f["severity"] == "error")
        .count();
    assert_eq!(
        errors, 0,
        "a valid package must report no error findings: {structured}"
    );

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_run_without_either_package_argument_is_a_tool_error() {
    let server = test_server("missing-arg");
    let client = server.connect().await;

    let result = client
        .call_tool(call("verify_run", serde_json::json!({ "dry_run": true })))
        .await
        .expect("must still answer over the protocol");
    assert_eq!(result.is_error, Some(true));

    client.cancel().await.ok();
}

/// A misregistration -- this binary launched with a `U2S_AEM_VERIFY_FORMAT`
/// other than `aem-ubs` -- is a startup error, not a server that quietly
/// serves the wrong profile: `main.rs`'s `expected_format: Some("aem-ubs")`
/// is exactly what `u2s_aem_verify_core::server::run_main`'s own format
/// check exists for. Uses the same `boot_failure_contract` every other
/// u2s server's own "must fail closed at startup" coverage does.
#[test]
fn the_binary_refuses_to_start_under_the_wrong_format() {
    let server = test_server("wrong-format");
    u2s_render_test_harness::boot_failure_contract(
        &server,
        &[("U2S_AEM_VERIFY_FORMAT", "aem")],
        &["only ever serves", "\"aem-ubs\""],
    );
}

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
}

/// `test_server`'s environment, plus whichever `U2S_AEM_VERIFY_*` variables
/// this process was itself launched with -- so a developer exporting the
/// real UBS profile for a booted AEM+Redacto instance
/// (`docker/aem/README.md`) and running `cargo test -p u2s-aem-ubs-verify-mcp \
/// --test e2e -- --ignored` gets that real instance, not the offline
/// fixture values every other test in this file uses.
fn live_test_server(label: &str) -> ServerUnderTest {
    let mut server = test_server(label);
    for key in [
        "U2S_AEM_VERIFY_IMAGE",
        "U2S_AEM_VERIFY_USER",
        "U2S_AEM_VERIFY_PASSWORD",
        "U2S_AEM_VERIFY_PLATFORM",
        "U2S_AEM_VERIFY_SUBMIT",
        "U2S_AEM_VERIFY_REDACTO_URL",
        "U2S_AEM_VERIFY_DATA_VOLUME",
        "U2S_AEM_VERIFY_CONTAINER_PORT",
        "U2S_AEM_VERIFY_UBS_MANDATOR",
    ] {
        if let Ok(value) = std::env::var(key) {
            server = server.env(key, value);
        }
    }
    server
}

/// The deepest live coverage this crate has: a real UBS wizard form
/// (`AAOV_033`), walked panel by panel in headless Chromium against a real
/// AEM instance running `ajila-forms-ubs`, and submitted for real through
/// UBS's own `window.forms.ubs.navigation.submit(...)` routine -- ending
/// in a browser download this test reads back as a PDF the Redacto
/// rendering dependency actually produced.
///
/// This is the same live package (`AAOV_033 (1).zip`, see
/// `aaov-output/README.md` at the repo root) the generic verifier's own
/// wizard-walking work was developed and proven against, and where the
/// generic driver correctly stopped: `AAOV_033`'s summary panel renders no
/// submit button at all (`window.forms.ubs.isFWB()` is hardcoded `true` on
/// the `ajila-forms-ubs` branch this workspace's AEM image is baked from,
/// which hides the toolbar's `submit` element on that panel). This
/// binary's own `crate::driver::UbsDriver` is what closes that gap: its
/// terminal-panel signal is "the summary panel is showing"
/// (`.summaryComponent`), and its own submit call is UBS's routine, which
/// populates `summaryComponent` before submitting -- the field
/// `ajila-forms-ubs`'s `DorRenderingExecutor.isSummaryOutput()` checks is
/// non-empty before routing the submission through Redacto rather than
/// AEM's native (and, on this workspace's ARM Docker image, non-functional
/// -- `XMLForm.exe`/`convertpdf.exe` are 32-bit x86 binaries `qemu-i386`
/// cannot load a loader for) XDP rendering path.
///
/// Needs `U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH` pointing at that real
/// package -- never committed to this repository, since it is built from
/// licensed UBS content (the same precedent
/// `u2s-aem-verify-mcp/tests/e2e.rs`'s own live-submit test already sets).
/// `#[ignore]`d for the same reason as every other live test in this file:
/// needs a real Docker daemon, the AEM image booted from
/// `U2S_AEM_VERIFY_DATA_VOLUME` with `ajila-forms-ubs` already deployed
/// onto it (`docker/aem/README.md`), and the Redacto summary Sling bundle
/// reachable at `U2S_AEM_VERIFY_REDACTO_URL`.
#[tokio::test]
#[ignore = "needs a real Docker daemon, a real AEM+Redacto environment, and \
            U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH"]
async fn verify_run_submits_a_ubs_wizard_and_downloads_the_redacto_pdf() {
    let package_path = std::env::var("U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH").expect(
        "U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH must name a real, wizard-shaped UBS FileVault \
         package to submit",
    );

    let server = live_test_server("ubs-wizard-submit");
    let client = server.connect().await;

    let result = client
        .call_tool(call(
            "verify_run",
            serde_json::json!({ "package_path": package_path, "submit": true }),
        ))
        .await
        .expect("verify_run must answer");
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);

    let structured = result.structured_content.expect("structured content");
    let steps = structured["steps"].as_array().expect("steps array");
    assert!(
        steps.len() > 2,
        "a wizard with more than one panel must produce more than the flat \"form\"/\
         \"after-submit\" pair: {structured}"
    );
    let step_names: Vec<&str> = steps
        .iter()
        .map(|s| s["name"].as_str().expect("step name"))
        .collect();
    assert_eq!(step_names.first(), Some(&"form"));
    assert_eq!(step_names.last(), Some(&"after-submit"));

    let errors: Vec<&serde_json::Value> = structured["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter(|f| f["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "no error findings expected: {errors:?}");

    let download = structured["artefacts"]
        .as_array()
        .expect("artefacts array")
        .iter()
        .find(|a| a["kind"] == "download")
        .unwrap_or_else(|| panic!("no download artefact in: {structured}"));
    assert_eq!(download["blob"]["media_type"], "application/pdf");
    assert!(
        download["blob"]["byte_len"].as_u64().unwrap_or(0) > 0,
        "the downloaded PDF must not be empty: {download}"
    );

    client.cancel().await.ok();
}

/// Calls `tool`, asserts it did not come back a tool error, and returns its
/// structured content -- the "call, check, unwrap" sequence every test in
/// this file below repeats, pulled out once these interactive tests made
/// it common enough to be worth naming.
async fn call_ok(
    client: &u2s_render_test_harness::Client,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let result = client
        .call_tool(call(tool, args))
        .await
        .unwrap_or_else(|err| panic!("{tool} must answer: {err}"));
    assert_ne!(result.is_error, Some(true), "{tool}: {:?}", result.content);
    result
        .structured_content
        .unwrap_or_else(|| panic!("{tool}: no structured content"))
}

/// Calls `tool` and asserts it *did* come back a tool error containing
/// `needle` somewhere in its text content -- the refusal-path counterpart
/// to [`call_ok`].
async fn call_refused(
    client: &u2s_render_test_harness::Client,
    tool: &str,
    args: serde_json::Value,
    needle: &str,
) {
    let result = client
        .call_tool(call(tool, args))
        .await
        .unwrap_or_else(|err| panic!("{tool} must answer: {err}"));
    assert_eq!(result.is_error, Some(true), "{tool} should have been refused: {:?}", result.content);
    let text: Vec<String> = result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|t| t.text.clone())
        .collect();
    assert!(
        text.iter().any(|t| t.contains(needle)),
        "{tool}'s refusal should mention {needle:?}: {text:?}"
    );
}

/// The interactive control tools (`verify_open`/`verify_controls`/
/// `verify_set`/`verify_next`/`verify_prev`/`verify_reset`/
/// `verify_screenshot`/`verify_submit`/`verify_close`), driving the same
/// real UBS wizard form (`AAOV_033`) `verify_run_submits_a_ubs_wizard_and_downloads_the_redacto_pdf`
/// walks in one shot, one step at a time instead. Deliberately does not
/// hardcode any of `AAOV_033`'s own field names: which control is a radio,
/// dropdown, checkbox or text field is discovered from `verify_controls`'
/// own inventory, the same way `crate::flow::wizard_js::AUTO_FILL_REQUIRED`
/// discovers what a panel needs without this crate naming a single UBS
/// field anywhere in its own source (`tests/no_customer_terms.rs` in
/// `u2s-aem-verify-core` guards exactly that).
///
/// Needs `U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH`, same as the sibling
/// one-shot test above, and the same real Docker/AEM/Redacto environment.
#[tokio::test]
#[ignore = "needs a real Docker daemon, a real AEM+Redacto environment, and \
            U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH"]
async fn interactive_tools_drive_aaov_033_to_a_redacto_pdf() {
    let package_path = std::env::var("U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH").expect(
        "U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH must name a real, wizard-shaped UBS FileVault \
         package to open",
    );

    let server = live_test_server("ubs-wizard-interactive");
    let client = server.connect().await;

    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": package_path }),
    )
    .await;
    let form = opened["form"].as_str().expect("a form handle").to_owned();
    assert_eq!(opened["revision"], 0);
    let mut revision = 0u64;

    let controls_body = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "form": form, "revision": revision }),
    )
    .await;
    let controls = controls_body["controls"]
        .as_array()
        .expect("controls array");
    assert!(
        !controls.is_empty(),
        "a real UBS wizard form must have controls: {controls_body}"
    );

    let find_kind = |kind: &str| controls.iter().find(|c| c["kind"] == kind).cloned();
    let radio = find_kind("radio");
    let dropdown = find_kind("dropdown");
    let checkbox = find_kind("checkbox");
    let text = find_kind("text");
    // Every kind this test knows how to exercise is recorded in the
    // assertion message even when absent, so a form that turns out not to
    // carry one is a visible skip, not a silent one.
    println!(
        "controls found -- radio: {}, dropdown: {}, checkbox: {}, text: {}",
        radio.is_some(),
        dropdown.is_some(),
        checkbox.is_some(),
        text.is_some()
    );

    for control in [&radio, &dropdown] {
        let Some(control) = control else { continue };
        let field = control["field"].as_str().expect("field name");
        let first_option = control["options"][0]["value"]
            .as_str()
            .expect("a radio/dropdown must have at least one option")
            .to_owned();
        let interaction = call_ok(
            &client,
            "verify_set",
            serde_json::json!({
                "form": form, "expected_revision": revision, "field": field, "value": first_option
            }),
        )
        .await;
        revision = interaction["revision"].as_u64().expect("revision");

        // A value that is not one of the control's own options is refused
        // naming the real ones -- checked once, against whichever control
        // this form actually offers.
        call_refused(
            &client,
            "verify_set",
            serde_json::json!({
                "form": form, "expected_revision": revision, "field": field,
                "value": "definitely-not-a-real-option"
            }),
            "cannot be set to",
        )
        .await;

        // A stale expected_revision is refused naming the current one.
        call_refused(
            &client,
            "verify_set",
            serde_json::json!({
                "form": form, "expected_revision": revision.saturating_sub(1),
                "field": field, "value": first_option
            }),
            "behind it",
        )
        .await;
    }

    if let Some(checkbox) = &checkbox {
        let field = checkbox["field"].as_str().expect("field name");
        let value = if checkbox["multi_select"].as_bool() == Some(true) {
            serde_json::json!([checkbox["options"][0]["value"]])
        } else {
            checkbox["options"][0]["value"].clone()
        };
        let interaction = call_ok(
            &client,
            "verify_set",
            serde_json::json!({
                "form": form, "expected_revision": revision, "field": field, "value": value
            }),
        )
        .await;
        revision = interaction["revision"].as_u64().expect("revision");
    }

    if let Some(text) = &text {
        let field = text["field"].as_str().expect("field name");
        let interaction = call_ok(
            &client,
            "verify_set",
            serde_json::json!({
                "form": form, "expected_revision": revision, "field": field, "value": "U2S interactive test"
            }),
        )
        .await;
        revision = interaction["revision"].as_u64().expect("revision");
    }

    // Walk forward until the driver's own terminal-panel signal fires,
    // bounded generously so a stuck walk fails the test instead of hanging
    // the whole suite.
    let mut is_terminal = false;
    for _ in 0..25 {
        let panel = call_ok(
            &client,
            "verify_controls",
            serde_json::json!({ "form": form, "revision": revision }),
        )
        .await;
        is_terminal = panel["is_terminal"].as_bool().unwrap_or(false);
        if is_terminal || panel["has_next"].as_bool() != Some(true) {
            break;
        }
        let interaction = call_ok(
            &client,
            "verify_next",
            serde_json::json!({ "form": form, "expected_revision": revision }),
        )
        .await;
        revision = interaction["revision"].as_u64().expect("revision");
    }
    assert!(
        is_terminal,
        "the wizard walk must reach the driver's own terminal panel before submitting"
    );

    // One panel back and forward again, to prove verify_prev actually
    // moves the panel rather than merely answering.
    let before_prev = revision;
    let back = call_ok(
        &client,
        "verify_prev",
        serde_json::json!({ "form": form, "expected_revision": revision }),
    )
    .await;
    revision = back["revision"].as_u64().expect("revision");
    assert_ne!(revision, before_prev);
    let forward = call_ok(
        &client,
        "verify_next",
        serde_json::json!({ "form": form, "expected_revision": revision }),
    )
    .await;
    revision = forward["revision"].as_u64().expect("revision");
    assert!(forward["is_terminal"].as_bool().unwrap_or(false));

    // A full-page screenshot, and -- if this form still has a visible
    // control to point at -- one cropped to just that control.
    let full = client
        .call_tool(call(
            "verify_screenshot",
            serde_json::json!({ "form": form, "revision": revision }),
        ))
        .await
        .expect("verify_screenshot must answer");
    assert_ne!(full.is_error, Some(true), "{:?}", full.content);

    let visible_field = controls
        .iter()
        .find(|c| c["visible"].as_bool() == Some(true))
        .and_then(|c| c["field"].as_str());
    if let Some(field) = visible_field {
        let clipped = client
            .call_tool(call(
                "verify_screenshot",
                serde_json::json!({ "form": form, "revision": revision, "field": field }),
            ))
            .await
            .expect("verify_screenshot with field must answer");
        assert_ne!(clipped.is_error, Some(true), "{:?}", clipped.content);
    }

    // Submit and confirm the same PDF artefact verify_run's own submit
    // step produces.
    let submitted = call_ok(
        &client,
        "verify_submit",
        serde_json::json!({ "form": form, "expected_revision": revision }),
    )
    .await;
    revision = submitted["revision"].as_u64().expect("revision");

    let errors: Vec<&serde_json::Value> = submitted["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter(|f| f["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "no error findings expected: {errors:?}");

    let download = submitted["artefacts"]
        .as_array()
        .expect("artefacts array")
        .iter()
        .find(|a| a["kind"] == "download")
        .unwrap_or_else(|| panic!("no download artefact in: {submitted}"));
    assert_eq!(download["blob"]["media_type"], "application/pdf");
    assert!(
        download["blob"]["byte_len"].as_u64().unwrap_or(0) > 0,
        "the downloaded PDF must not be empty: {download}"
    );

    // Close, then confirm the handle is genuinely gone.
    call_ok(&client, "verify_close", serde_json::json!({ "form": form })).await;
    call_refused(
        &client,
        "verify_controls",
        serde_json::json!({ "form": form, "revision": revision }),
        "is not open",
    )
    .await;

    client.cancel().await.ok();
}

/// `verify_run` shares the session's one installed-package slot with the
/// interactive tools, so it must refuse rather than race an open form --
/// confirmed live, since a bogus handle offline (`u2s_aem_verify_core::specs_shared`'s
/// own vectors) cannot exercise this without a real open form to collide
/// with.
#[tokio::test]
#[ignore = "needs a real Docker daemon, a real AEM+Redacto environment, and \
            U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH"]
async fn verify_run_on_a_session_with_an_open_form_is_refused() {
    let package_path = std::env::var("U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH").expect(
        "U2S_AEM_VERIFY_LIVE_WIZARD_PACKAGE_PATH must name a real, wizard-shaped UBS FileVault \
         package to open",
    );

    let server = live_test_server("ubs-run-vs-open-form");
    let client = server.connect().await;

    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": package_path.clone(), "session_id": "shared" }),
    )
    .await;
    let form = opened["form"].as_str().expect("a form handle").to_owned();

    call_refused(
        &client,
        "verify_run",
        serde_json::json!({ "package_path": package_path, "session_id": "shared" }),
        "form_open",
    )
    .await;

    call_ok(&client, "verify_close", serde_json::json!({ "form": form, "session_id": "shared" })).await;
    client.cancel().await.ok();
}

/// The generic battery every u2s server passes, run against this one --
/// every declared test vector is offline by design, so this must pass
/// without Docker just like the render servers' own conformance runs do.
#[tokio::test]
async fn the_server_passes_the_shared_conformance_battery() {
    let server = test_server("conformance");
    let client = server.connect().await;

    let report = u2s_mcp::conformance::run(&client, &fixtures_dir()).await;
    assert!(
        report.passed(),
        "the shared battery must pass: {:?}",
        report.findings
    );

    client.cancel().await.ok();
}
