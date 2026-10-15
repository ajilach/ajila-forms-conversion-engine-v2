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
    // verify_status, verify_package_check, verify_run, plus the twelve
    // shared interactive tools (`u2s_aem_verify_core::specs_shared`).
    assert_eq!(manifest.tools.len(), 15);

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
        "verify_field",
        "verify_set",
        "verify_click",
        "verify_next",
        "verify_prev",
        "verify_goto",
        "verify_reset",
        "verify_screenshot",
        "verify_submit",
        "verify_close",
    ] {
        assert_eq!(by_name(name).verify, None, "{name}");
    }
    for name in ["verify_controls", "verify_field", "verify_screenshot"] {
        assert!(!by_name(name).side_effecting, "{name}");
    }
    for name in [
        "verify_open",
        "verify_set",
        "verify_click",
        "verify_next",
        "verify_prev",
        "verify_goto",
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
            "verify_click",
            "verify_close",
            "verify_controls",
            "verify_field",
            "verify_goto",
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

/// Real converted UBS forms beside `AF_AABF`, copied from the conversion
/// feedback corpus, each chosen for what it adds to the live coverage:
/// `BAZA_019` nests accordions inside its wizard, `AABE_019` nests a second
/// wizard marked `nonNavigable` (so its step must stay unlisted), `AAQR_033` is an Italian mandator-033 form with dropdowns,
/// checkboxes, dates and repeatables, `AATH_033` carries the most
/// repeatable groups plus a numeric field, and `BBDZ_019` has an
/// eight-section accordion (the only shape in the corpus where a jump lands
/// inside a nested container; `BAZA_019`'s accordions have one section each).
///
/// The rest are special cases found by scanning all 420 corpus forms:
/// `BBEU_019` has the most fields (mostly static text), `AADD_019` a
/// 41-step wizard, `BARN_019` a repeatable nested in a repeatable plus
/// signatures, `ACYI_019` a group whose `minOccur` is 0 and one that starts
/// at `minOccur = maxOccur = 2`, `AAOU_033` an unlimited group and
/// multi-line text, `BBPT_019` the corpus's only multi-select checkbox plus
/// telephone fields, and `BAPJ_019` a field disabled from the start.
///
/// A third round: `BAKT_019` uses AEM's stock field components instead of
/// UBS's own, `AARN_033` an HTML displayer, `AANG_019` primary and
/// secondary buttons running plain scripts, `BAPK_033` ends in a preview
/// step instead of a summary, `AAIY_019` has a separator, `AAIQ_019` the
/// deepest nesting of repeatables (and one of the largest packages),
/// `AAHC_019` the most groups that start at `minOccur` 2, and `AAGA_019`
/// the most repeatable groups (15).
const CORPUS_FORMS: [&str; 20] = [
    "BAZA_019", "AABE_019", "AAQR_033", "AATH_033", "BBDZ_019", "BBEU_019", "AADD_019",
    "BARN_019", "ACYI_019", "AAOU_033", "BBPT_019", "BAPJ_019", "BAKT_019", "AARN_033",
    "AANG_019", "BAPK_033", "AAIY_019", "AAIQ_019", "AAHC_019", "AAGA_019",
];

fn corpus_form(code: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/forms")
        .join(format!("{code}.zip"))
}

/// Offline: every corpus form is a package the verifier can open, with its
/// form code and mandator resolved from the package's own metadata.
#[tokio::test]
async fn every_corpus_form_passes_the_package_check() {
    let server = test_server("package-check-corpus");
    let client = server.connect().await;
    for code in CORPUS_FORMS {
        let checked = call_ok(
            &client,
            "verify_package_check",
            serde_json::json!({ "package_path": corpus_form(code).display().to_string() }),
        )
        .await;
        // AANG_019's package carries another form's authored metadata
        // (form code AAAJ, entity 001): the corpus's own data, which the
        // verifier must follow rather than second-guess from the file name.
        let (formcode, mandator) = match code {
            "AANG_019" => ("AAAJ", "001"),
            other => other.split_once('_').unwrap(),
        };
        assert_eq!(checked["ubs"]["formcode"], formcode, "{code}: {checked}");
        assert_eq!(checked["ubs"]["selected_mandator"], mandator, "{code}: {checked}");
        let errors = checked["findings"]
            .as_array()
            .map(|findings| findings.iter().filter(|f| f["severity"] == "error").count())
            .unwrap_or(0);
        assert_eq!(errors, 0, "{code}: {checked}");
    }
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
        "U2S_AEM_VERIFY_REGISTRY_USERNAME",
        "U2S_AEM_VERIFY_REGISTRY_PASSWORD",
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
/// Runs on the in-repo `AF_AABF` wizard (it began on the licensed
/// `AAOV_033`, which this repository cannot ship), against the
/// `u2s-aem-ubs:2026-10-10` image or later: earlier bakes persisted every
/// local submit to CLP, whose PDF/A conversion fails on an arm64 AEM.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn verify_run_submits_a_ubs_wizard_and_downloads_the_redacto_pdf() {
    let package_path = af_aabf_zip().display().to_string();

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

    // The PDF first: its helper keeps the bytes a failing render is
    // diagnosed from, which a finding about it cannot replace.
    assert_rendered_pdf(&server, &structured);

    let errors: Vec<&serde_json::Value> = structured["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter(|f| f["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "no error findings expected: {errors:?}");

    client.cancel().await.ok();
}

/// The one judgement both live submit tests pass on the PDF the Redacto
/// summary rendering produced: that it is a PDF that *shows* the form, not
/// just a file with a `%PDF` header. Reads the download back from the
/// server's blob dir and asserts it has a page, draws something on it
/// (`u2s_verify_core::pdf_content::blank_pdf`, the same check `verify_run`
/// itself reports `download_blank` from) and carries the value the wizard
/// walker types into required fields (`UtwoS`, `wizard_js::PLACEHOLDER`
/// in `u2s-aem-verify-core`), so the submitted data visibly reached the
/// rendered document without this test naming a single field of the form.
///
/// With `U2S_AEM_VERIFY_LIVE_PDF_OUT` set, the PDF is also written there
/// before anything is asserted: a failing render is diagnosed from its
/// bytes, which the blob dir does not outlive the test by.
fn assert_rendered_pdf(server: &ServerUnderTest, report: &serde_json::Value) {
    assert_rendered_pdf_with(server, report, Some(u2s_aem_verify_core::flow::wizard_js::PLACEHOLDER));
}

/// [`assert_rendered_pdf`] for a submit whose required fields were filled
/// with `marker` rather than `verify_run`'s `UtwoS`; `None` when no text
/// field was filled, so there is no value to look for.
fn assert_rendered_pdf_with(
    server: &ServerUnderTest,
    report: &serde_json::Value,
    marker: Option<&str>,
) {
    let download = report["artefacts"]
        .as_array()
        .expect("artefacts array")
        .iter()
        .find(|a| a["kind"] == "download")
        .unwrap_or_else(|| panic!("no download artefact in: {report}"));
    assert_eq!(download["blob"]["media_type"], "application/pdf");
    let handle = download["blob"]["handle"].as_str().expect("blob handle");
    let blob_dir = server
        .env
        .iter()
        .find(|(key, _)| key == "U2S_BLOB_DIR")
        .map(|(_, dir)| std::path::PathBuf::from(dir))
        .expect("test_server sets U2S_BLOB_DIR");
    let pdf = std::fs::read(blob_dir.join(handle)).expect("the download blob exists");
    if let Ok(out) = std::env::var("U2S_AEM_VERIFY_LIVE_PDF_OUT") {
        std::fs::write(&out, &pdf).unwrap_or_else(|err| panic!("writing {out}: {err}"));
        eprintln!("the submit download is at {out}");
    }

    assert!(
        pdf.starts_with(b"%PDF"),
        "the download is not a PDF: {} bytes",
        pdf.len()
    );
    let pages =
        u2s_verify_core::pdf_content::page_count(&pdf).expect("the PDF's pages can be counted");
    assert!(pages >= 1, "the PDF has no pages");
    assert_eq!(
        u2s_verify_core::pdf_content::blank_pdf(&pdf),
        Ok(false),
        "the {pages}-page PDF draws nothing on any page: the summary rendered empty"
    );
    let doc = lopdf::Document::load_mem(&pdf).expect("the PDF loads");
    let page_numbers: Vec<u32> = (1..=pages as u32).collect();
    let text = doc
        .extract_text(&page_numbers)
        .expect("the PDF's text can be extracted");
    if let Some(marker) = marker {
        assert!(
            text.contains(marker),
            "the PDF's text does not carry the filled-in value {marker}; it reads:\n{text}"
        );
    }
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
/// `verify_set`/`verify_next`/`verify_prev`/`verify_screenshot`/
/// `verify_submit`/`verify_close`), driving the same real UBS wizard form
/// (`AF_AABF`) `verify_run_submits_a_ubs_wizard_and_downloads_the_redacto_pdf`
/// walks in one shot, one step at a time instead. The only live test of a
/// value refused for not being one of its control's options, of a stale
/// `expected_revision` refused, and of a screenshot cropped to one control.
/// Deliberately does not hardcode any of the form's own field names: which
/// control is a radio,
/// dropdown, checkbox or text field is discovered from `verify_controls`'
/// own inventory, the same way `crate::flow::wizard_js::AUTO_FILL_REQUIRED`
/// discovers what a panel needs without this crate naming a single UBS
/// field anywhere in its own source (`tests/no_customer_terms.rs` in
/// `u2s-aem-verify-core` guards exactly that).
///
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn interactive_tools_drive_a_ubs_wizard_to_a_redacto_pdf() {
    let package_path = af_aabf_zip().display().to_string();

    let server = live_test_server("ubs-wizard-interactive");
    let client = server.connect().await;

    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": package_path }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();
    assert_eq!(opened["revision"], 0);
    let mut revision = 0u64;

    let controls_body = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": revision, "limit": 500 }),
    )
    .await;
    let controls = controls_body["controls"]
        .as_array()
        .expect("controls array");
    assert!(
        !controls.is_empty(),
        "a real UBS wizard form must have controls: {controls_body}"
    );

    // Only controls a visitor could fill: the first text control in model
    // order can be a hidden metadata field (on AF_AABF it holds the form's
    // mandator, and overwriting it made the submit's document generation
    // fail with "No metadata information for mandator ...").
    let find_kind = |kind: &str| {
        controls
            .iter()
            .find(|c| c["kind"] == kind && c["visible"] == true && c["access"] == "open")
            .cloned()
    };
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
                "session": session, "expected_revision": revision, "field": field, "value": first_option
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
                "session": session, "expected_revision": revision, "field": field,
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
                "session": session, "expected_revision": revision.saturating_sub(1),
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
                "session": session, "expected_revision": revision, "field": field, "value": value
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
                "session": session, "expected_revision": revision, "field": field, "value": "U2S interactive test"
            }),
        )
        .await;
        revision = interaction["revision"].as_u64().expect("revision");
    }

    // Walk to the driver's own terminal panel, filling what each step
    // requires (the shared walker every PDF test uses).
    let walked = walk_to_terminal(&client, "AF_AABF", &session, revision).await;
    revision = walked.revision;

    // One panel back and forward again, to prove verify_prev actually
    // moves the panel rather than merely answering.
    let before_prev = revision;
    let back = call_ok(
        &client,
        "verify_prev",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    revision = back["revision"].as_u64().expect("revision");
    assert_ne!(revision, before_prev);
    let forward = call_ok(
        &client,
        "verify_next",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    revision = forward["revision"].as_u64().expect("revision");
    assert!(forward["is_terminal"].as_bool().unwrap_or(false));

    // A full-page screenshot, and -- if this form still has a visible
    // control to point at -- one cropped to just that control.
    let full = client
        .call_tool(call(
            "verify_screenshot",
            serde_json::json!({ "session": session, "revision": revision }),
        ))
        .await
        .expect("verify_screenshot must answer");
    assert_ne!(full.is_error, Some(true), "{:?}", full.content);

    let on_screen = controls_of(&client, &session, revision).await;
    let visible_field = on_screen
        .iter()
        .find(|c| c["visible"] == true && !c["position"].is_null())
        .and_then(|c| c["field"].as_str());
    if let Some(field) = visible_field {
        let clipped = client
            .call_tool(call(
                "verify_screenshot",
                serde_json::json!({ "session": session, "revision": revision, "field": field }),
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
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    revision = submitted["revision"].as_u64().expect("revision");

    assert_rendered_pdf_with(&server, &submitted, walked.wrote_marker.then_some(FILL_MARKER));

    let errors: Vec<&serde_json::Value> = submitted["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter(|f| f["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "no error findings expected: {errors:?}");

    // Close, then confirm the handle is genuinely gone.
    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    call_refused(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": revision, "limit": 500 }),
        "is not open",
    )
    .await;

    client.cancel().await.ok();
}

/// The text every walk fills into required text fields.
const FILL_MARKER: &str = "Probetext";

/// A placeholder a required control will accept: the first option for a
/// choice, otherwise a value of the control's own kind. Mirrors what
/// `AUTO_FILL_REQUIRED` does for `verify_run`, without naming any field.
fn placeholder_for(control: &serde_json::Value) -> serde_json::Value {
    let first_option = control["options"][0]["value"].clone();
    match control["kind"].as_str() {
        Some("radio" | "dropdown") => first_option,
        Some("checkbox") if control["multi_select"].as_bool() == Some(true) => {
            serde_json::json!([first_option])
        }
        Some("checkbox") => first_option,
        Some("date") => serde_json::json!("2000-01-01"),
        Some("number") => serde_json::json!("1"),
        // No digits: some forms' text patterns refuse them (BAPJ_019's
        // names). Distinctive, so a rendered PDF carrying it proves the
        // filled data reached the document.
        _ => serde_json::json!(FILL_MARKER),
    }
}

/// Fills every visible, required, still-empty control the form currently
/// shows, returning the revision after the last set.
async fn fill_visible_required(
    client: &u2s_render_test_harness::Client,
    session: &str,
    mut revision: u64,
) -> u64 {
    let body = call_ok(
        client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": revision, "limit": 500 }),
    )
    .await;
    for control in body["controls"].as_array().expect("controls array") {
        let empty = control["value"].is_null() || control["value"] == "";
        let fillable = control["kind"] != "button" && control["kind"] != "other";
        if control["required"] == true && control["visible"] == true && empty && fillable {
            let interaction = call_ok(
                client,
                "verify_set",
                serde_json::json!({
                    "session": session, "expected_revision": revision,
                    "field": control["field"], "value": placeholder_for(control)
                }),
            )
            .await;
            revision = interaction["revision"].as_u64().expect("revision");
        }
    }
    revision
}

/// The success criterion for the navigation fixes: on a real UBS wizard,
/// `verify_next` moves to a different panel and `verify_prev` brings the
/// form back to the panel it started on. Before the fix the previous
/// selector looked for `button.movePrevious` while the runtime renders the
/// authored `type="movePrev"` as the class `movePrev`, so `verify_prev`
/// could not find a control at all. Uses the in-repo `AF_AABF` wizard, so
/// it needs no licensed package; it never submits, so it needs no Redacto.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn verify_prev_returns_to_the_panel_verify_next_left_on_a_ubs_wizard() {
    let server = live_test_server("ubs-wizard-prev");
    let client = server.connect().await;

    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();
    let start_panel = opened["panel"].clone();
    println!("opened on panel {start_panel}, has_next: {}", opened["has_next"]);
    assert_eq!(opened["has_next"], true, "AF_AABF is a wizard: {opened}");

    // With the required fields still empty, the panel's step validation
    // blocks the advance, and the refusal names what blocked it.
    let blocked = client
        .call_tool(call(
            "verify_next",
            serde_json::json!({ "session": session, "expected_revision": 0 }),
        ))
        .await
        .expect("verify_next must answer");
    let blocked_text = format!("{:?}", blocked.content);
    println!("next with required fields empty: {blocked_text}");
    assert_eq!(blocked.is_error, Some(true), "{blocked_text}");
    assert!(
        blocked_text.contains("blocked by the form's validation"),
        "{blocked_text}"
    );
    let start_som = start_panel["som"].as_str().expect("start panel som");
    assert!(
        !blocked_text.contains("PN_Signatures"),
        "only the blocking panel is validated, not the whole form: {blocked_text}"
    );
    assert!(blocked_text.contains(start_som), "{blocked_text}");

    let revision = fill_visible_required(&client, &session, 0).await;
    let forward = call_ok(
        &client,
        "verify_next",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    println!("after next: panel {}", forward["panel"]);
    assert!(start_panel["som"].is_string(), "the start panel must be known: {opened}");
    assert_ne!(
        forward["panel"]["som"], start_panel["som"],
        "verify_next must change the panel"
    );

    let revision = forward["revision"].as_u64().expect("revision");
    let back = call_ok(
        &client,
        "verify_prev",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    println!("after prev: panel {}", back["panel"]);
    assert_eq!(
        back["panel"]["som"], start_panel["som"],
        "verify_prev must return to the panel verify_next left"
    );

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// Controls are identified by SOM: `AF_AABF` repeats its account-holder
/// group (`PN_CPGRP`) in two containers, so names such as
/// `TXT_CompanyName` occur more than once. A bare repeated name is refused
/// naming the SOMs, and setting by SOM changes exactly that control.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn controls_are_addressed_by_som_and_a_shared_name_is_refused() {
    let server = live_test_server("ubs-som-addressing");
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();

    let body = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": 0, "limit": 500 }),
    )
    .await;
    let controls = body["controls"].as_array().expect("controls array");
    assert!(
        controls
            .iter()
            .all(|c| c["field"].as_str().is_some_and(|f| f.starts_with("guide[0]"))),
        "every field must be a SOM expression"
    );
    let mut by_name: std::collections::BTreeMap<&str, Vec<&serde_json::Value>> = Default::default();
    for control in controls {
        if control["kind"] == "text" {
            by_name.entry(control["name"].as_str().unwrap()).or_default().push(control);
        }
    }
    let (shared_name, holders) = by_name
        .iter()
        .find(|(_, holders)| holders.len() > 1)
        .expect("AF_AABF repeats at least one text field name");
    println!("{shared_name} is carried by {} controls", holders.len());

    call_refused(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": 0, "field": shared_name, "value": "x"
        }),
        "names",
    )
    .await;

    let target = holders[0]["field"].as_str().unwrap();
    let other = holders[1]["field"].as_str().unwrap();
    let set = call_ok(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": 0, "field": target, "value": "by SOM"
        }),
    )
    .await;
    assert_eq!(set["field"], target);
    assert_eq!(set["values_changed"], true, "{set}");

    let after = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": 1, "limit": 500 }),
    )
    .await;
    let value_of = |field: &str| {
        after["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["field"] == field)
            .map(|c| c["value"].clone())
    };
    assert_eq!(value_of(target), Some(serde_json::json!("by SOM")));
    assert_ne!(value_of(other), Some(serde_json::json!("by SOM")));

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

async fn controls_of(
    client: &u2s_render_test_harness::Client,
    session: &str,
    revision: u64,
) -> Vec<serde_json::Value> {
    let mut controls = Vec::new();
    let mut offset = 0u64;
    loop {
        let page = call_ok(
            client,
            "verify_controls",
            serde_json::json!({
                "session": session, "revision": revision, "offset": offset, "limit": 100
            }),
        )
        .await;
        controls.extend(page["controls"].as_array().expect("controls array").iter().cloned());
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => {
                assert_eq!(
                    controls.len() as u64,
                    page["total"].as_u64().expect("total"),
                    "walking every page must yield every control"
                );
                return controls;
            }
        }
    }
}

/// Repeated panels through the form's own buttons: an Add button adds an
/// instance whose fields are addressable by SOM, its Remove button takes it
/// away again, and pressing Add past `maxOccur` is either refused (the form
/// hid the button) or reported with a warning, never a silent success.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn verify_click_adds_and_removes_repeated_panel_instances() {
    let server = live_test_server("ubs-click-instances");
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();
    let mut revision = 0u64;

    let controls = controls_of(&client, &session, revision).await;
    let add = controls
        .iter()
        .find(|c| {
            c["click"] == "instances"
                && c["visible"] == true
                && c["enabled"] == true
                && c["name"].as_str().is_some_and(|n| n.contains("Add"))
        })
        .expect("AF_AABF's first panel has a visible Add button")
        .clone();
    let add_field = add["field"].as_str().unwrap().to_owned();
    let container = add_field.rsplit_once('.').unwrap().0.to_owned();
    println!("pressing {add_field}");

    let text = controls
        .iter()
        .find(|c| c["kind"] == "text")
        .expect("a text field")
        .clone();
    call_refused(
        &client,
        "verify_click",
        serde_json::json!({
            "session": session, "expected_revision": revision, "field": text["field"]
        }),
        "is not a button",
    )
    .await;
    call_refused(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": revision, "field": add_field, "value": "x"
        }),
        "Press it with verify_click",
    )
    .await;

    let added = call_ok(
        &client,
        "verify_click",
        serde_json::json!({ "session": session, "expected_revision": revision, "field": add_field }),
    )
    .await;
    revision = added["revision"].as_u64().unwrap();
    assert_eq!(added["kind"], "click");
    assert_eq!(added["instances_changed"], true, "{added}");
    let appeared: Vec<&str> = added["appeared"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(
        appeared.iter().any(|f| f.starts_with(&container) && f.contains("[1]")),
        "the second instance's controls must appear: {added}"
    );

    let controls = controls_of(&client, &session, revision).await;
    let second = controls
        .iter()
        .find(|c| {
            c["kind"] == "text"
                && c["instance_index"] == 1
                && c["field"].as_str().is_some_and(|f| f.starts_with(&container))
        })
        .expect("a text field in the second instance")
        .clone();
    let second_field = second["field"].as_str().unwrap().to_owned();
    let first_field = second_field.replacen("[1]", "[0]", 1);
    let set = call_ok(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": revision,
            "field": second_field, "value": "second instance"
        }),
    )
    .await;
    revision = set["revision"].as_u64().unwrap();
    let controls = controls_of(&client, &session, revision).await;
    let value_of = |field: &str| controls.iter().find(|c| c["field"] == field).map(|c| c["value"].clone());
    assert_eq!(value_of(&second_field), Some(serde_json::json!("second instance")));
    assert_ne!(value_of(&first_field), Some(serde_json::json!("second instance")));

    let remove = controls
        .iter()
        .find(|c| {
            c["click"] == "instances"
                && c["visible"] == true
                && c["field"].as_str().is_some_and(|f| f.starts_with(&container) && f.contains("[1]"))
        })
        .expect("the second instance has a visible Remove button")
        .clone();
    let removed = call_ok(
        &client,
        "verify_click",
        serde_json::json!({
            "session": session, "expected_revision": revision, "field": remove["field"]
        }),
    )
    .await;
    revision = removed["revision"].as_u64().unwrap();
    assert_eq!(removed["instances_changed"], true, "{removed}");
    assert!(
        removed["disappeared"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f.as_str())
            .any(|f| f.starts_with(&container) && f.contains("[1]")),
        "the second instance's visible controls must disappear: {removed}"
    );
    assert!(
        controls_of(&client, &session, revision)
            .await
            .iter()
            .all(|c| c["field"] != serde_json::json!(second_field)),
        "the removed instance's fields must be gone from the inventory"
    );

    let mut limited = false;
    for _ in 0..8 {
        let result = client
            .call_tool(call(
                "verify_click",
                serde_json::json!({
                    "session": session, "expected_revision": revision, "field": add_field
                }),
            ))
            .await
            .expect("verify_click must answer");
        if result.is_error == Some(true) {
            let text = format!("{:?}", result.content);
            assert!(text.contains("not visible and enabled"), "{text}");
            println!("at the limit the form hides its Add button: {text}");
            limited = true;
            break;
        }
        let pressed = result.structured_content.expect("structured result");
        revision = pressed["revision"].as_u64().unwrap();
        if pressed["instances_changed"] == false {
            let warning = pressed["warning"].as_str().unwrap_or_default();
            assert!(warning.contains("maxOccur"), "{pressed}");
            println!("at the limit: {warning}");
            limited = true;
            break;
        }
    }
    assert!(limited, "pressing Add must stop adding instances at maxOccur");

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// A blocked `verify_next` runs the form's validation, so afterwards
/// `verify_controls` reports `valid: false` and the form's own message on
/// exactly the fields the refusal named.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn a_blocked_next_leaves_the_failing_fields_marked_invalid() {
    let server = live_test_server("ubs-validation-state");
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();

    let before = controls_of(&client, &session, 0).await;
    assert!(
        before.iter().all(|c| c["error_message"].is_null()),
        "nothing is invalid before the form validates"
    );

    let blocked = client
        .call_tool(call(
            "verify_next",
            serde_json::json!({ "session": session, "expected_revision": 0 }),
        ))
        .await
        .expect("verify_next must answer");
    assert_eq!(blocked.is_error, Some(true));
    let refusal = format!("{:?}", blocked.content);

    let after = controls_of(&client, &session, 0).await;
    let invalid: Vec<&serde_json::Value> = after.iter().filter(|c| c["valid"] == false).collect();
    assert!(!invalid.is_empty(), "the blocking fields must be marked invalid");
    for control in &invalid {
        let field = control["field"].as_str().unwrap();
        assert!(refusal.contains(field), "{field} is invalid but the refusal did not name it");
        assert!(
            control["error_message"].as_str().is_some_and(|m| !m.is_empty()),
            "{control}"
        );
    }
    println!("{} invalid field(s), e.g. {}", invalid.len(), invalid[0]["error_message"]);

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// `panels` lists the wizard's steps with the active one marked, and
/// `verify_goto` jumps straight to any visible step -- forward past steps
/// whose required fields are still empty, and back -- after which
/// `verify_next`/`verify_prev` keep working from the new position.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn verify_goto_jumps_between_listed_panels() {
    let server = live_test_server("ubs-goto");
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();
    let panels = opened["panels"].as_array().expect("panels").clone();
    let visible: Vec<&serde_json::Value> = panels.iter().filter(|p| p["visible"] == true).collect();
    assert!(visible.len() >= 4, "AF_AABF is a multi-step wizard: {panels:?}");
    assert_eq!(panels.iter().filter(|p| p["active"] == true).count(), 1, "{panels:?}");
    assert_eq!(visible[0]["active"], true, "the first visible step is shown first");
    assert!(panels.iter().all(|p| p["layout"] == "wizard"), "{panels:?}");

    let third = visible[3]["som"].as_str().unwrap().to_owned();
    let jumped = call_ok(
        &client,
        "verify_goto",
        serde_json::json!({ "session": session, "expected_revision": 0, "panel": third }),
    )
    .await;
    assert_eq!(jumped["kind"], "goto");
    assert_eq!(jumped["panel"]["som"], serde_json::json!(third), "{jumped}");
    let mut revision = jumped["revision"].as_u64().unwrap();

    let same = call_ok(
        &client,
        "verify_goto",
        serde_json::json!({ "session": session, "expected_revision": revision, "panel": third }),
    )
    .await;
    assert_eq!(same["panel"]["som"], serde_json::json!(third));
    revision = same["revision"].as_u64().unwrap();

    let back = call_ok(
        &client,
        "verify_prev",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    assert_eq!(back["panel"]["som"], visible[2]["som"], "{back}");
    revision = back["revision"].as_u64().unwrap();

    if let Some(hidden) = panels.iter().find(|p| p["visible"] == false) {
        call_refused(
            &client,
            "verify_goto",
            serde_json::json!({
                "session": session, "expected_revision": revision, "panel": hidden["som"]
            }),
            "not visible",
        )
        .await;
    }

    let first = visible[0]["som"].as_str().unwrap();
    let home = call_ok(
        &client,
        "verify_goto",
        serde_json::json!({ "session": session, "expected_revision": revision, "panel": first }),
    )
    .await;
    assert_eq!(home["panel"]["som"], serde_json::json!(first));
    let controls = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": home["revision"], "limit": 500 }),
    )
    .await;
    let active: Vec<&serde_json::Value> = controls["panels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["active"] == true)
        .collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["som"], serde_json::json!(first));

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// The XFA-style listing surface: `verify_controls` pages through every
/// control and filters by kind, `verify_field` returns one control exactly
/// as the list shows it, and malformed paging arguments are refused.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn controls_page_filter_and_field_lookup() {
    let server = live_test_server("ubs-listing");
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": af_aabf_zip().display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();

    let all = controls_of(&client, &session, 0).await;
    let unique: std::collections::BTreeSet<&str> =
        all.iter().filter_map(|c| c["field"].as_str()).collect();
    assert_eq!(unique.len(), all.len(), "pages must not overlap");
    assert!(all.iter().all(|c| c["access"] == "open" || c["access"] == "readOnly"));
    println!("AF_AABF lists {} controls", all.len());

    let buttons = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": 0, "kinds": ["button"], "limit": 500 }),
    )
    .await;
    let listed = buttons["controls"].as_array().unwrap();
    assert!(!listed.is_empty());
    assert!(listed.iter().all(|c| c["kind"] == "button"), "{buttons}");
    assert_eq!(buttons["total"].as_u64().unwrap() as usize, listed.len());

    let sample = &all[all.len() / 2];
    let one = call_ok(
        &client,
        "verify_field",
        serde_json::json!({ "session": session, "revision": 0, "field": sample["field"] }),
    )
    .await;
    assert_eq!(&one, sample);

    call_refused(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": 0, "kinds": ["slider"] }),
        "kinds must be",
    )
    .await;
    call_refused(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": 0, "limit": 0 }),
        "limit must be",
    )
    .await;

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// Runs one corpus form through every interactive capability, so a form
/// shape `AF_AABF` does not have (nested accordions, a nested wizard,
/// another mandator, many repeatables) cannot break one unnoticed. Each
/// step names the form in its assertion messages. `unlisted` names panels
/// that must not appear in `panels` (steps of a container marked
/// `nonNavigable`). With `nested_goto`, the jump must land on a step inside
/// a nested container, not on a wizard step.
async fn exercise_form(code: &str, unlisted: &[&str], nested_goto: bool) {
    let server = live_test_server(&format!("ubs-corpus-{code}"));
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": corpus_form(code).display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();
    let start = opened["panel"]["som"].clone();
    assert!(start.is_string(), "{code}: the start panel must be known: {opened}");

    // Panels: at most one active step per container.
    let panels = opened["panels"].as_array().expect("panels").clone();
    assert!(!panels.is_empty(), "{code}: a wizard form lists its steps");
    let mut active_per_parent: std::collections::BTreeMap<&str, usize> = Default::default();
    for panel in panels.iter().filter(|p| p["active"] == true) {
        *active_per_parent.entry(panel["parent"].as_str().unwrap()).or_default() += 1;
    }
    assert!(active_per_parent.values().all(|n| *n == 1), "{code}: {panels:?}");
    for name in unlisted {
        assert!(
            panels.iter().all(|p| p["name"] != *name),
            "{code}: {name} sits in a nonNavigable container and must not be listed"
        );
    }
    let depths: std::collections::BTreeSet<u64> =
        panels.iter().filter_map(|p| p["depth"].as_u64()).collect();
    println!("{code}: {} panels at depths {depths:?}", panels.len());

    // Rendering: the opened form draws as a real image.
    assert_renders(&client, code, &session, 0).await;

    // Listing: every page, unique SOMs.
    let mut revision = 0u64;
    let controls = controls_of(&client, &session, revision).await;
    let unique: std::collections::BTreeSet<&str> =
        controls.iter().filter_map(|c| c["field"].as_str()).collect();
    assert_eq!(unique.len(), controls.len(), "{code}: every field is a unique SOM");
    let not_inputs: Vec<&str> = controls
        .iter()
        .filter_map(|c| c["class_name"].as_str())
        .filter(|class| class.contains("guideTextDraw") || class.contains("guidePanel"))
        .collect();
    assert!(not_inputs.is_empty(), "{code}: static text and panels are not controls: {not_inputs:?}");

    // Setting: one visible, enabled control of every settable kind.
    for kind in ["radio", "dropdown", "checkbox", "text", "date", "number"] {
        let Some(control) = controls.iter().find(|c| {
            c["kind"] == kind
                && c["visible"] == true
                && c["access"] == "open"
                && (kind == "text" || kind == "date" || kind == "number" || !c["options"].as_array().unwrap().is_empty())
        }) else {
            println!("{code}: no visible {kind} on the first panel");
            continue;
        };
        let set = call_ok(
            &client,
            "verify_set",
            serde_json::json!({
                "session": session, "expected_revision": revision,
                "field": control["field"], "value": placeholder_for(control)
            }),
        )
        .await;
        assert_eq!(set["kind"], "set", "{code}");
        revision = set["revision"].as_u64().unwrap();
    }

    // Repeatables: an Add button on the first panel adds an instance and
    // its Remove button takes it away.
    let controls = controls_of(&client, &session, revision).await;
    let add = controls.iter().find(|c| {
        c["click"] == "instances"
            && c["visible"] == true
            && c["access"] == "open"
            && c["name"].as_str().is_some_and(|n| n.contains("Add"))
    });
    match add {
        None => println!("{code}: no visible Add button on the first panel"),
        Some(add) => {
            let add_field = add["field"].as_str().unwrap().to_owned();
            let container = add_field.rsplit_once('.').unwrap().0.to_owned();
            let added = call_ok(
                &client,
                "verify_click",
                serde_json::json!({
                    "session": session, "expected_revision": revision, "field": add_field
                }),
            )
            .await;
            revision = added["revision"].as_u64().unwrap();
            assert!(
                added["instances_changed"] == true || added["warning"].is_string(),
                "{code}: pressing {add_field} must add an instance or say why not: {added}"
            );
            if added["instances_changed"] == true {
                let after = controls_of(&client, &session, revision).await;
                let added_instance = after
                    .iter()
                    .filter_map(|c| c["instance_index"].as_u64().map(|i| (i, c)))
                    .filter(|(_, c)| c["field"].as_str().is_some_and(|f| f.starts_with(&container)))
                    .map(|(i, _)| i)
                    .max()
                    .expect("the new instance's controls are listed");
                let remove = after.iter().find(|c| {
                    c["click"] == "instances"
                        && c["visible"] == true
                        && c["access"] == "open"
                        && c["instance_index"] == added_instance
                        && c["field"].as_str().is_some_and(|f| f.starts_with(&container))
                });
                match remove {
                    None => println!("{code}: instance {added_instance} has no visible Remove button"),
                    Some(remove) => {
                        let removed = call_ok(
                            &client,
                            "verify_click",
                            serde_json::json!({
                                "session": session, "expected_revision": revision,
                                "field": remove["field"]
                            }),
                        )
                        .await;
                        revision = removed["revision"].as_u64().unwrap();
                        assert_eq!(removed["instances_changed"], true, "{code}: {removed}");
                    }
                }
            }
        }
    }

    // Next and back: fill what blocks the step, advance, return.
    let mut advanced = None;
    let mut last_refusal = String::new();
    for _ in 0..4 {
        revision = fill_visible_required(&client, &session, revision).await;
        let result = client
            .call_tool(call(
                "verify_next",
                serde_json::json!({ "session": session, "expected_revision": revision }),
            ))
            .await
            .expect("verify_next must answer");
        if result.is_error == Some(true) {
            let text = format!("{:?}", result.content);
            assert!(text.contains("blocked by the form's validation"), "{code}: {text}");
            last_refusal = text;
            continue;
        }
        advanced = result.structured_content;
        break;
    }
    let advanced = advanced.unwrap_or_else(|| {
        panic!("{code}: verify_next never got past the first step: {last_refusal}")
    });
    revision = advanced["revision"].as_u64().unwrap();
    assert_ne!(advanced["panel"]["som"], start, "{code}: verify_next must move");
    let back = call_ok(
        &client,
        "verify_prev",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    revision = back["revision"].as_u64().unwrap();
    assert_eq!(back["panel"]["som"], start, "{code}: verify_prev must return");

    // Goto: a nested step when the form has one, else another top-level step.
    let panels = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": revision, "limit": 1 }),
    )
    .await["panels"]
        .as_array()
        .unwrap()
        .clone();
    let target = panels
        .iter()
        .filter(|p| p["visible"] == true && p["active"] == false)
        .max_by_key(|p| p["depth"].as_u64().unwrap_or(0))
        .unwrap_or_else(|| panic!("{code}: a second visible panel to jump to: {panels:?}"))
        .clone();
    if nested_goto {
        assert!(
            target["depth"].as_u64().unwrap_or(0) >= 1,
            "{code}: the jump must target a nested step: {target}"
        );
    }
    let jumped = call_ok(
        &client,
        "verify_goto",
        serde_json::json!({
            "session": session, "expected_revision": revision, "panel": target["som"]
        }),
    )
    .await;
    revision = jumped["revision"].as_u64().unwrap();
    let after_jump = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": revision, "limit": 1 }),
    )
    .await;
    let now_active = after_jump["panels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["som"] == target["som"])
        .map(|p| p["active"].clone());
    assert_eq!(
        now_active,
        Some(serde_json::json!(true)),
        "{code}: {} (depth {}) must be the active step after the jump",
        target["som"],
        target["depth"]
    );
    println!("{code}: jumped to {} at depth {}", target["name"], target["depth"]);

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_baza_019_nested_accordions() {
    exercise_form("BAZA_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aabe_019_nested_wizard() {
    exercise_form("AABE_019", &["panel_forInternalBankUse"], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aaqr_033_italian_choices_and_repeatables() {
    exercise_form("AAQR_033", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_bbdz_019_multi_section_accordion() {
    exercise_form("BBDZ_019", &[], true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_bbeu_019_most_fields() {
    exercise_form("BBEU_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aadd_019_forty_one_steps() {
    exercise_form("AADD_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_barn_019_nested_repeatables() {
    exercise_form("BARN_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_acyi_019_min_occur_zero_and_two() {
    exercise_form("ACYI_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aaou_033_unlimited_group() {
    exercise_form("AAOU_033", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_bbpt_019_multi_select_and_telephone() {
    exercise_form("BBPT_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_bapj_019_disabled_field() {
    exercise_form("BAPJ_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_bakt_019_stock_components() {
    exercise_form("BAKT_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aarn_033_html_displayer() {
    exercise_form("AARN_033", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aang_019_script_buttons() {
    exercise_form("AANG_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_bapk_033_preview_ending() {
    exercise_form("BAPK_033", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aaiy_019_separator() {
    exercise_form("AAIY_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aaiq_019_deepest_nesting() {
    exercise_form("AAIQ_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aahc_019_many_min_two_groups() {
    exercise_form("AAHC_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aaga_019_most_groups() {
    exercise_form("AAGA_019", &[], false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn corpus_aath_033_many_repeatables() {
    exercise_form("AATH_033", &[], false).await;
}

/// The form renders: `verify_screenshot` returns a PNG with real
/// dimensions and content, inline or as a blob.
async fn assert_renders(
    client: &u2s_render_test_harness::Client,
    code: &str,
    session: &str,
    revision: u64,
) {
    let result = client
        .call_tool(call(
            "verify_screenshot",
            serde_json::json!({ "session": session, "revision": revision }),
        ))
        .await
        .expect("verify_screenshot must answer");
    assert_ne!(result.is_error, Some(true), "{code}: {:?}", result.content);
    let meta = result.structured_content.expect("screenshot metadata");
    assert_eq!(meta["mime"], "image/png", "{code}: {meta}");
    assert!(meta["width_px"].as_u64().unwrap_or(0) >= 200, "{code}: {meta}");
    assert!(meta["height_px"].as_u64().unwrap_or(0) >= 200, "{code}: {meta}");
    assert!(
        meta["byte_len"].as_u64().unwrap_or(0) > 10_000,
        "{code}: a rendered form is more than a blank image: {meta}"
    );
}

/// Fills the fields a refusal names (`SOM: message` pairs), the way an
/// agent reading the refusal would: a required field can sit off screen
/// (in a collapsed section, confirmed on `BBDZ_019`), where a walk over
/// visible fields never reaches it. Returns the revision and whether the
/// text marker was written.
async fn fill_named_in_refusal(
    client: &u2s_render_test_harness::Client,
    session: &str,
    mut revision: u64,
    refusal: &str,
) -> (u64, bool) {
    let mut wrote_marker = false;
    let soms: std::collections::BTreeSet<&str> = refusal
        .split(|c: char| c.is_whitespace() || c == ';' || c == '"' || c == '\\')
        .map(|token| token.trim_end_matches(':'))
        .filter(|token| token.starts_with("guide[0]") && token.ends_with(']'))
        .collect();
    for som in soms {
        let control = call_ok(
            client,
            "verify_field",
            serde_json::json!({ "session": session, "revision": revision, "field": som }),
        )
        .await;
        let settable = control["access"] == "open"
            && !matches!(control["kind"].as_str(), Some("button" | "other" | "signature"));
        if !settable {
            continue;
        }
        let value = placeholder_for(&control);
        if value.is_null() {
            continue;
        }
        wrote_marker |= value == serde_json::json!(FILL_MARKER);
        let set = call_ok(
            client,
            "verify_set",
            serde_json::json!({
                "session": session, "expected_revision": revision, "field": som, "value": value
            }),
        )
        .await;
        revision = set["revision"].as_u64().unwrap();
    }
    (revision, wrote_marker)
}

/// What [`walk_to_terminal`] leaves behind.
struct Walked {
    revision: u64,
    /// Whether any required text field was filled with [`FILL_MARKER`], so
    /// the rendered PDF can be expected to carry it.
    wrote_marker: bool,
}

/// Walks the form to the step its driver calls terminal, filling every
/// visible required field on the way, then whatever a blocked step's
/// refusal names, and retrying. Returns where it ended.
async fn walk_to_terminal(
    client: &u2s_render_test_harness::Client,
    code: &str,
    session: &str,
    mut revision: u64,
) -> Walked {
    let mut wrote_marker = false;
    for _ in 0..60 {
        let flags = call_ok(
            client,
            "verify_controls",
            serde_json::json!({ "session": session, "revision": revision, "limit": 1 }),
        )
        .await;
        if flags["is_terminal"] == true {
            assert_eq!(flags["has_next"], false, "{code}: the terminal step has no next");
            return Walked {
                revision,
                wrote_marker,
            };
        }
        let mut last_refusal = String::new();
        let mut moved = false;
        for _ in 0..3 {
            let before = revision;
            revision = fill_visible_required(client, session, revision).await;
            if revision != before {
                let controls = controls_of(client, session, revision).await;
                wrote_marker |= controls.iter().any(|c| c["value"] == FILL_MARKER);
            }
            if !last_refusal.is_empty() {
                let (after, marker) =
                    fill_named_in_refusal(client, session, revision, &last_refusal).await;
                revision = after;
                wrote_marker |= marker;
            }
            let result = client
                .call_tool(call(
                    "verify_next",
                    serde_json::json!({ "session": session, "expected_revision": revision }),
                ))
                .await
                .expect("verify_next must answer");
            if result.is_error == Some(true) {
                last_refusal = format!("{:?}", result.content);
                continue;
            }
            revision = result.structured_content.unwrap()["revision"].as_u64().unwrap();
            moved = true;
            break;
        }
        assert!(moved, "{code}: stuck before the terminal step: {last_refusal}");
    }
    panic!("{code}: no terminal step within 60 steps");
}

/// Opens a corpus form for one of the special-case tests below.
async fn open_corpus(
    client: &u2s_render_test_harness::Client,
    code: &str,
) -> String {
    call_ok(
        client,
        "verify_open",
        serde_json::json!({ "package_path": corpus_form(code).display().to_string() }),
    )
    .await["session"]
        .as_str()
        .expect("a session handle")
        .to_owned()
}

async fn goto_named(
    client: &u2s_render_test_harness::Client,
    session: &str,
    revision: u64,
    panel: &str,
) -> u64 {
    call_ok(
        client,
        "verify_goto",
        serde_json::json!({ "session": session, "expected_revision": revision, "panel": panel }),
    )
    .await["revision"]
        .as_u64()
        .unwrap()
}

fn named<'a>(controls: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    controls
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no control named {name}"))
}

/// The corpus's only multi-select checkbox (`BBPT_019`'s `CB_Daily`): it is
/// recognised as multi-select, an array of options sets it, and it reads
/// back as that array. Its raw value is AEM's comma-joined form.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn special_multi_select_checkbox_round_trips() {
    let server = live_test_server("ubs-special-multi-select");
    let client = server.connect().await;
    let session = open_corpus(&client, "BBPT_019").await;
    let revision = goto_named(&client, &session, 0, "PN_Annex1").await;

    let controls = controls_of(&client, &session, revision).await;
    let daily = named(&controls, "CB_Daily");
    assert_eq!(daily["kind"], "checkbox");
    assert_eq!(daily["multi_select"], true, "{daily}");
    let set = call_ok(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": revision,
            "field": daily["field"], "value": ["Monday", "Wednesday"]
        }),
    )
    .await;
    assert_eq!(set["values_changed"], true, "{set}");
    let read = call_ok(
        &client,
        "verify_field",
        serde_json::json!({ "session": session, "revision": set["revision"], "field": daily["field"] }),
    )
    .await;
    assert_eq!(read["value"], serde_json::json!(["Monday", "Wednesday"]), "{read}");
    assert_eq!(read["raw_value"], "Monday,Wednesday");

    let telephones = controls
        .iter()
        .filter(|c| c["class_name"].as_str().is_some_and(|k| k.contains("guideTelephone")))
        .count();
    assert!(telephones > 0 && controls.iter().all(|c| !(c["class_name"].as_str().unwrap_or("").contains("guideTelephone") && c["kind"] != "text")));

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// A field the form disabled (`BAPJ_019`'s `TXT_Clause3`) is `readOnly`
/// and refused by `verify_set`, as `xfa_set` refuses a locked field; a
/// signature is its own kind and refused too; and a name field reports the
/// no-digits pattern it is validated against.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn special_read_only_and_signature_fields_are_refused() {
    let server = live_test_server("ubs-special-read-only");
    let client = server.connect().await;
    let session = open_corpus(&client, "BAPJ_019").await;
    let controls = controls_of(&client, &session, 0).await;

    let last_name = named(&controls, "TXT_ClientLastName_BAPJ");
    assert_eq!(last_name["pattern"], "^([^0-9]*)$", "{last_name}");

    let clause = named(&controls, "TXT_Clause3");
    assert_eq!(clause["access"], "readOnly", "{clause}");
    call_refused(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": 0, "field": clause["field"], "value": "x"
        }),
        "is readOnly",
    )
    .await;

    let signature = controls
        .iter()
        .find(|c| c["kind"] == "signature")
        .expect("BAPJ_019 has signature fields");
    call_refused(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": 0, "field": signature["field"], "value": "x"
        }),
        "is a signature",
    )
    .await;

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// A repeatable nested in a repeatable (`BARN_019`): adding an outer
/// instance brings its own inner group, whose fields are addressed by SOM
/// through both indices; `instance_index` is the nearest group's index.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn special_nested_repeatable_instances() {
    let server = live_test_server("ubs-special-nested");
    let client = server.connect().await;
    let session = open_corpus(&client, "BARN_019").await;
    let mut revision = goto_named(&client, &session, 0, "PN_Request_Authorization").await;

    let controls = controls_of(&client, &session, revision).await;
    let add = named(&controls, "BT_Add_Security");
    assert_eq!(add["click"], "instances");
    let added = call_ok(
        &client,
        "verify_click",
        serde_json::json!({ "session": session, "expected_revision": revision, "field": add["field"] }),
    )
    .await;
    revision = added["revision"].as_u64().unwrap();
    assert_eq!(added["instances_changed"], true, "{added}");

    let outer = "PN_Security_Details_RP[1].Panelcontains_Security_Details[0].";
    let controls = controls_of(&client, &session, revision).await;
    let nested: Vec<&serde_json::Value> = controls
        .iter()
        .filter(|c| c["field"].as_str().is_some_and(|f| f.contains(outer)))
        .collect();
    assert!(!nested.is_empty(), "the new outer instance carries its inner group");
    assert!(nested.iter().all(|c| c["instance_index"] == 0), "nearest group's index: {nested:?}");
    let text = nested
        .iter()
        .find(|c| c["kind"] == "text" && c["access"] == "open")
        .expect("a text field in the nested instance");
    let first_instance_field = text["field"].as_str().unwrap().replace("_RP[1]", "_RP[0]");
    let set = call_ok(
        &client,
        "verify_set",
        serde_json::json!({
            "session": session, "expected_revision": revision,
            "field": text["field"], "value": "nested"
        }),
    )
    .await;
    revision = set["revision"].as_u64().unwrap();
    let after = controls_of(&client, &session, revision).await;
    let value_of = |field: &str| after.iter().find(|c| c["field"] == field).map(|c| c["value"].clone());
    assert_eq!(value_of(text["field"].as_str().unwrap()), Some(serde_json::json!("nested")));
    assert_ne!(value_of(&first_instance_field), Some(serde_json::json!("nested")));

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// A 41-step wizard (`AADD_019`): every step is listed, a jump reaches the
/// last visible one, and stepping back from it lands on the one before.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn special_forty_one_step_wizard() {
    let server = live_test_server("ubs-special-many-steps");
    let client = server.connect().await;
    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": corpus_form("AADD_019").display().to_string() }),
    )
    .await;
    let session = opened["session"].as_str().unwrap().to_owned();
    let steps: Vec<&serde_json::Value> = opened["panels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["depth"] == 0 && p["visible"] == true)
        .collect();
    assert!(steps.len() >= 30, "AADD_019 lists its steps: {}", steps.len());
    let last = steps[steps.len() - 1];
    let jumped = call_ok(
        &client,
        "verify_goto",
        serde_json::json!({ "session": session, "expected_revision": 0, "panel": last["som"] }),
    )
    .await;
    assert_eq!(jumped["panel"]["som"], last["som"]);
    let back = call_ok(
        &client,
        "verify_prev",
        serde_json::json!({ "session": session, "expected_revision": jumped["revision"] }),
    )
    .await;
    assert_eq!(back["panel"]["som"], steps[steps.len() - 2]["som"], "{back}");

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// A group that starts at `minOccur = maxOccur = 2` (`ACYI_019`'s
/// `PN_Sign_LR`) lists both instances from the start, and pressing one of
/// its Remove buttons never takes it below two: refused, or reported with
/// a `minOccur` warning.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn special_group_at_min_occur_two_keeps_its_instances() {
    let server = live_test_server("ubs-special-min-two");
    let client = server.connect().await;
    let session = open_corpus(&client, "ACYI_019").await;
    let revision = goto_named(&client, &session, 0, "PN_SignatureS_b6ba73ae").await;

    let controls = controls_of(&client, &session, revision).await;
    let in_group = |c: &&serde_json::Value, index: &str| {
        c["field"].as_str().is_some_and(|f| f.contains(&format!("PN_Sign_LR[{index}]")))
    };
    assert!(controls.iter().any(|c| in_group(&c, "0")) && controls.iter().any(|c| in_group(&c, "1")));
    let remove = controls
        .iter()
        .find(|c| in_group(c, "0") && c["click"] == "instances")
        .expect("a Remove button in the group");
    let result = client
        .call_tool(call(
            "verify_click",
            serde_json::json!({
                "session": session, "expected_revision": revision, "field": remove["field"]
            }),
        ))
        .await
        .expect("verify_click must answer");
    if result.is_error == Some(true) {
        let text = format!("{:?}", result.content);
        assert!(text.contains("not visible and enabled"), "{text}");
    } else {
        let pressed = result.structured_content.unwrap();
        assert_eq!(pressed["instances_changed"], false, "{pressed}");
        assert!(pressed["warning"].as_str().is_some_and(|w| w.contains("minOccur")), "{pressed}");
    }

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// Renders a corpus form, walks it to its terminal step and submits it,
/// asserting the downloaded PDF is real and, when the walk typed the
/// marker into a required text field and `document_shows_filled_fields`,
/// that it shows it. A form can leave the fields the walk fills out of its
/// document on purpose (`summaryExclusion` on their step), which is why
/// that expectation is stated per form rather than assumed.
async fn render_walk_and_download_pdf(code: &str, document_shows_filled_fields: bool) {
    let server = live_test_server(&format!("ubs-pdf-{code}"));
    let client = server.connect().await;
    let session = open_corpus(&client, code).await;
    assert_renders(&client, code, &session, 0).await;
    let walked = walk_to_terminal(&client, code, &session, 0).await;
    assert_renders(&client, code, &session, walked.revision).await;
    let submitted = call_ok(
        &client,
        "verify_submit",
        serde_json::json!({ "session": session, "expected_revision": walked.revision }),
    )
    .await;
    assert_rendered_pdf_with(
        &server,
        &submitted,
        (walked.wrote_marker && document_shows_filled_fields).then_some(FILL_MARKER),
    );
    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// Every corpus form whose submit renders through Redacto (its DAM
/// `redactoSummary` flag is on and it has a summary panel) downloads its
/// PDF, from the `u2s-aem-ubs:2026-10-10` image on: that bake's UBS bundle
/// no longer persists to CLP in local bypass mode, whose PDF/A conversion
/// needs AEM's native x86 services. The rest cannot here: the preview-style
/// forms have Redacto off and take AEM's native renderer (covered by
/// `special_preview_ending_form_reaches_submit`), `BBPT_019` lacks its XSD
/// and AEM cannot export `BBEU_019`'s model at all.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aadd_019_forty_one_steps() {
    render_walk_and_download_pdf("AADD_019", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_acyi_019_groups_at_min_two() {
    render_walk_and_download_pdf("ACYI_019", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aaqr_033_italian() {
    render_walk_and_download_pdf("AAQR_033", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_bakt_019_stock_components() {
    render_walk_and_download_pdf("BAKT_019", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aarn_033_html_displayer() {
    render_walk_and_download_pdf("AARN_033", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aabe_019_nested_wizard() {
    render_walk_and_download_pdf("AABE_019", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aath_033_many_repeatables() {
    render_walk_and_download_pdf("AATH_033", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aaou_033_unlimited_group() {
    render_walk_and_download_pdf("AAOU_033", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aaga_019_most_groups() {
    render_walk_and_download_pdf("AAGA_019", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aahc_019_many_min_two_groups() {
    render_walk_and_download_pdf("AAHC_019", true).await;
}

/// The only required text fields `AAIY_019`'s walk fills sit on its first
/// step, which the form authors with `summaryExclusion="true"`: its
/// document is the checklist alone, so the filled value is rightly absent.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aaiy_019_separator() {
    render_walk_and_download_pdf("AAIY_019", false).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aaiq_019_deepest_nesting() {
    render_walk_and_download_pdf("AAIQ_019", true).await;
}

#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn pdf_aang_019_foreign_metadata() {
    render_walk_and_download_pdf("AANG_019", true).await;
}

/// A form that ends in a preview step instead of a summary (`BAPK_033`)
/// still has a terminal step: the walk reaches it, `has_next` is false
/// there, and `verify_submit` submits instead of refusing. The submit
/// either downloads the rendered PDF or reports, in its `no_download`
/// finding, which request failed -- a form without a summary takes AEM's
/// native rendering path, which this workspace's ARM image cannot run.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn special_preview_ending_form_reaches_submit() {
    let server = live_test_server("ubs-special-preview");
    let client = server.connect().await;
    let session = open_corpus(&client, "BAPK_033").await;
    let revision = walk_to_terminal(&client, "BAPK_033", &session, 0).await.revision;
    let panel = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": revision, "limit": 1 }),
    )
    .await;
    assert_eq!(panel["panel"]["name"], "preview", "{panel}");

    let submitted = call_ok(
        &client,
        "verify_submit",
        serde_json::json!({ "session": session, "expected_revision": revision }),
    )
    .await;
    let downloaded = submitted["artefacts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["kind"] == "download");
    if downloaded {
        assert_rendered_pdf_with(&server, &submitted, None);
    } else {
        let no_download = submitted["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["kind"] == "no_download")
            .unwrap_or_else(|| panic!("neither a download nor a no_download finding: {submitted}"));
        let message = no_download["message"].as_str().unwrap();
        assert!(
            message.contains("the server refused the submission") || message.contains("after the submit"),
            "a missing download must say why: {no_download}"
        );
    }

    call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    client.cancel().await.ok();
}

/// Two sessions of the same package on one instance are independent tabs:
/// a value set in one never shows in the other, `verify_status` lists both,
/// and closing one leaves the other (and the install they share) working.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn two_sessions_of_one_package_are_independent() {
    let server = live_test_server("ubs-two-sessions");
    let client = server.connect().await;
    let package = serde_json::json!({ "package_path": af_aabf_zip().display().to_string() });

    let first = call_ok(&client, "verify_open", package.clone()).await;
    let second = call_ok(&client, "verify_open", package).await;
    let first = first["session"].as_str().expect("a session handle").to_owned();
    let second = second["session"].as_str().expect("a session handle").to_owned();
    assert_ne!(first, second);

    let status = call_ok(&client, "verify_status", serde_json::json!({})).await;
    let listed: Vec<&str> = status["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .filter_map(|entry| entry["session"].as_str())
        .collect();
    assert!(
        listed.contains(&first.as_str()) && listed.contains(&second.as_str()),
        "verify_status must list both sessions: {status}"
    );

    let controls = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": first, "revision": 0, "limit": 500 }),
    )
    .await;
    let text = controls["controls"]
        .as_array()
        .expect("controls array")
        .iter()
        .find(|c| c["kind"] == "text" && c["visible"] == true && c["enabled"] == true)
        .expect("AF_AABF's first panel has a visible text field")
        .clone();
    let field = text["field"].as_str().expect("field name");
    call_ok(
        &client,
        "verify_set",
        serde_json::json!({
            "session": first, "expected_revision": 0, "field": field, "value": "only in the first"
        }),
    )
    .await;

    let in_second = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": second, "revision": 0, "limit": 500 }),
    )
    .await;
    let second_value = in_second["controls"]
        .as_array()
        .expect("controls array")
        .iter()
        .find(|c| c["field"] == field)
        .map(|c| c["value"].clone());
    assert_ne!(
        second_value,
        Some(serde_json::json!("only in the first")),
        "a value set in one session must not show in the other"
    );

    call_ok(&client, "verify_close", serde_json::json!({ "session": first })).await;
    call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": second, "revision": 0, "limit": 500 }),
    )
    .await;
    call_refused(
        &client,
        "verify_controls",
        serde_json::json!({ "session": first, "revision": 1, "limit": 500 }),
        "is not open",
    )
    .await;
    call_ok(&client, "verify_close", serde_json::json!({ "session": second })).await;
    client.cancel().await.ok();
}

/// `verify_run` walks beside a session open on its instance, which may be
/// another caller's: the session's form is still installed and answering
/// after the run uninstalls its own package. Needs a real open session to
/// share the instance with, so live only.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn verify_run_walks_beside_an_open_session() {
    let package_path = af_aabf_zip().display().to_string();

    let server = live_test_server("ubs-run-beside-open-form");
    let client = server.connect().await;

    let opened = call_ok(
        &client,
        "verify_open",
        serde_json::json!({ "package_path": package_path.clone(), "instance_id": "shared" }),
    )
    .await;
    let session = opened["session"].as_str().expect("a session handle").to_owned();

    call_ok(
        &client,
        "verify_run",
        serde_json::json!({ "package_path": package_path, "instance_id": "shared" }),
    )
    .await;

    let controls = call_ok(
        &client,
        "verify_controls",
        serde_json::json!({ "session": session, "revision": 0, "limit": 500, "instance_id": "shared" }),
    )
    .await;
    assert!(
        controls["controls"].as_array().is_some_and(|c| !c.is_empty()),
        "the open session must still show its form after the run: {controls}"
    );

    call_ok(&client, "verify_close", serde_json::json!({ "session": session, "instance_id": "shared" })).await;
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

/// Closing a session must leave the instance's browser for the next one:
/// the instance keeps one Chromium container for all of a caller's
/// sessions, so a second `verify_open` after a `verify_close` (a
/// conversion's Reviewer after its Author) has to find it answering. Needs the same live environment as
/// the tests above.
#[tokio::test]
#[ignore = "needs a real Docker daemon and the UBS AEM image (U2S_AEM_VERIFY_IMAGE)"]
async fn a_closed_form_leaves_the_browser_for_the_next_open() {
    let package_path = af_aabf_zip().display().to_string();
    let server = live_test_server("reopen-after-close");
    let client = server.connect().await;

    for round in ["first", "second"] {
        let opened = call_ok(
            &client,
            "verify_open",
            serde_json::json!({ "package_path": package_path }),
        )
        .await;
        let session = opened["session"]
            .as_str()
            .unwrap_or_else(|| panic!("the {round} open hands back a session: {opened}"))
            .to_owned();
        call_ok(&client, "verify_close", serde_json::json!({ "session": session })).await;
    }
    client.cancel().await.ok();
}
