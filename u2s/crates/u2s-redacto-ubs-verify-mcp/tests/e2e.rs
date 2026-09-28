//! The real binary, over real stdio MCP.
//!
//! Proves the manifest parses with no format module, `verify_status` and
//! `verify_dump_check` answer without touching Docker, and `verify_run`
//! with `dry_run: true` performs the same offline check wrapped in its own
//! result shape. `verify_run` with `dry_run: false` (a real Postgres
//! import) is `#[ignore]`d live coverage -- `u2s-redacto-verify-core`'s own
//! `tests/live_import.rs` already exercises the underlying flow directly;
//! this only additionally proves the MCP plumbing around it.

use u2s_render_test_harness::ServerUnderTest;

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../u2s-mapper-redacto/tests/fixtures")
}

#[tokio::test]
async fn the_manifest_parses_with_no_format_module() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let raw = u2s_render_test_harness::read_manifest(&client).await;
    let manifest = u2s_mcp::ServerManifest::parse(&raw).expect("the manifest must parse");
    manifest.check_compatible().expect("the contract major must be one this workspace understands");

    assert!(manifest.format.is_none(), "a verifier must never register a format of its own");
    assert_eq!(manifest.tools.len(), 3);
    assert_eq!(manifest.tools[0].tool, "verify_status");
    assert_eq!(manifest.tools[1].tool, "verify_dump_check");
    assert_eq!(manifest.tools[2].tool, "verify_run");
    assert_eq!(manifest.tools[2].role, u2s_mcp::ToolRole::Query);

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_status_answers_without_touching_docker_state() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let result = client
        .call_tool(rmcp::model::CallToolRequestParams::new("verify_status".to_owned()))
        .await
        .expect("verify_status must answer");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["profile"]["name"], "redacto-ubs");
    assert_eq!(structured["profile"]["rendering_configured"], false);
    assert_eq!(structured["rendering_reachable"], serde_json::Value::Null);

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_dump_check_reports_a_real_fixture_as_ok() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let fixture_path = fixtures_dir().join("redacto-AAEV_019.sql");
    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("verify_dump_check".to_owned()).with_arguments(
                serde_json::json!({ "artifact_path": fixture_path.display().to_string() })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("verify_dump_check must answer");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["ok"], true);
    assert_eq!(structured["document_id"], "aaev_019");
    assert_eq!(structured["asset_count"], 18);

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_dump_check_reports_garbage_as_not_ok() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("verify_dump_check".to_owned())
                .with_arguments(serde_json::json!({ "artifact_blob": "not-a-real-handle" }).as_object().unwrap().clone()),
        )
        .await
        .expect("verify_dump_check must still answer over the protocol");
    assert_eq!(result.is_error, Some(true), "an unreadable blob handle is a tool error");

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_run_dry_run_performs_only_the_offline_check() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let fixture_path = fixtures_dir().join("redacto-AAEV_019.sql");
    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("verify_run".to_owned()).with_arguments(
                serde_json::json!({
                    "artifact_path": fixture_path.display().to_string(),
                    "dry_run": true
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .expect("verify_run must answer");
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["dry_run"], true);
    assert_eq!(structured["steps"], serde_json::json!([]));
    assert_eq!(structured["artefacts"], serde_json::json!([]));
    assert_eq!(structured["findings"], serde_json::json!([]));

    client.cancel().await.ok();
}

#[tokio::test]
async fn verify_run_dry_run_reports_a_bad_dump_as_a_finding_not_a_tool_error() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let blob_dir = std::env::temp_dir().join(format!(
        "u2s-redacto-ubs-verify-mcp-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&blob_dir).unwrap();
    let garbage_path = blob_dir.join("garbage.sql");
    std::fs::write(&garbage_path, b"not a dump at all").unwrap();

    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("verify_run".to_owned()).with_arguments(
                serde_json::json!({ "artifact_path": garbage_path.display().to_string(), "dry_run": true })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("verify_run must answer");
    assert_ne!(result.is_error, Some(true), "a structurally bad dump is a finding, not a tool error");
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["findings"][0]["kind"], "dump_invalid");
    assert_eq!(structured["findings"][0]["severity"], "error");

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&blob_dir);
}

/// The generic battery every u2s server passes, run against this one so the
/// verifier is held to the same convention as every other.
#[tokio::test]
async fn the_server_passes_the_shared_conformance_battery() {
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto");
    let client = server.connect().await;

    let report = u2s_mcp::conformance::run(&client, &fixtures_dir()).await;
    assert!(report.passed(), "the shared battery must pass: {:?}", report.findings);

    client.cancel().await.ok();
}

/// Real Postgres coverage over the MCP surface itself -- `#[ignore]`d like
/// every other Docker-backed test in this workspace.
#[tokio::test]
#[ignore = "needs a real Docker daemon"]
async fn verify_run_actually_imports_a_real_fixture() {
    let blob_dir = std::env::temp_dir().join(format!(
        "u2s-redacto-ubs-verify-mcp-live-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let server = ServerUnderTest::locate("u2s-redacto-ubs-verify-mcp", "redacto")
        .env("U2S_BLOB_DIR", blob_dir.display().to_string());
    let client = server.connect().await;

    // A unique session id, so this test's own container (which the server
    // process leaves running for reuse -- see this crate's own module doc
    // on why teardown is the idle-timeout sweep's job, not a tool this
    // server exposes) never collides with a concurrent run of this same
    // test or of `u2s-redacto-verify-core`'s own `live_import.rs`.
    let session_id = format!(
        "e2e-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );

    let fixture_path = fixtures_dir().join("redacto-AAEV_019.sql");
    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("verify_run".to_owned()).with_arguments(
                serde_json::json!({
                    "artifact_path": fixture_path.display().to_string(),
                    "session_id": session_id,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .expect("verify_run must answer");
    assert_ne!(result.is_error, Some(true), "a real fixture must import cleanly: {:?}", result.content);
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["dry_run"], false);
    assert!(
        structured["findings"].as_array().unwrap().iter().any(|f| f["kind"] == "import_ok"),
        "{structured}"
    );

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&blob_dir);

    // The server process (now exited) owned the session pool in memory, so
    // nothing inside it can tear this container down any more -- clean up
    // directly, the same way a real deployment's idle-timeout sweep
    // eventually would.
    if let Ok(docker) = u2s_verify_core::docker::DockerLifecycle::connect().await {
        let _ = docker.teardown(&format!("u2s-redacto-verify-{session_id}")).await;
    }
}
