//! Proves the generalized battery against the three real servers this
//! workspace ships — not a mock, not a fabricated manifest.
//!
//! Requires the server binaries and fixtures already built: `cargo build -p
//! u2s-xfa-mcp -p u2s-render-pdf-mcp -p u2s-render-xfa-mcp` and `cargo test -p
//! u2s-render-xfa -p u2s-render-pdf` (which generate their fixture corpora).

use std::path::{Path, PathBuf};

use u2s_mcp::conformance;
use u2s_render_test_harness::ServerUnderTest;

fn xfa_data_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-render-xfa/fixtures/generated")
        .canonicalize()
        .expect("run `cargo test -p u2s-render-xfa` first to generate fixtures")
}

fn render_pdf_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-render-pdf/fixtures/generated")
        .canonicalize()
        .expect("run `cargo test -p u2s-render-pdf` first to generate fixtures")
}

async fn assert_passes(server: ServerUnderTest, fixture_dir: &Path) {
    let client = server.connect().await;
    let report = conformance::run(&client, fixture_dir).await;
    assert!(
        report.passed(),
        "{} failed conformance:\n{:#?}",
        server.binary.display(),
        report.findings
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn u2s_xfa_mcp_passes_conformance() {
    assert_passes(
        ServerUnderTest::locate("u2s-xfa-mcp", "xfa"),
        &xfa_data_fixtures(),
    )
    .await;
}

#[tokio::test]
async fn u2s_render_pdf_mcp_passes_conformance() {
    let server = ServerUnderTest::locate("u2s-render-pdf-mcp", "pdf").env(
        "U2S_BLOB_DIR",
        std::env::temp_dir()
            .join("u2s-mcp-conformance-blobs")
            .display()
            .to_string(),
    );
    assert_passes(server, &render_pdf_fixtures()).await;
}

#[tokio::test]
async fn u2s_render_xfa_mcp_passes_conformance() {
    let font_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/fonts");
    if !font_dir.exists() {
        eprintln!(
            "skipping: {} not present — run ./scripts/fetch-fonts.sh",
            font_dir.display()
        );
        return;
    }
    let server = ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env(
            "U2S_BLOB_DIR",
            std::env::temp_dir()
                .join("u2s-mcp-conformance-blobs")
                .display()
                .to_string(),
        )
        .env(
            "U2S_FONT_DIR",
            font_dir.canonicalize().unwrap().display().to_string(),
        );
    assert_passes(server, &xfa_data_fixtures()).await;
}

/// A manifest whose declared `expect.structured` does not match the tool's
/// real result must fail — not because a fixture is missing, but because
/// this is the check that has no analogue in the pre-existing harness at all.
/// Proven against the real `xfa_packets` vector by asking a question its
/// declared expectation cannot answer honestly: swap in a fixture with a
/// different packet count than the manifest's `count: 1`.
#[tokio::test]
async fn a_result_mismatched_against_the_manifest_is_a_conformance_failure() {
    let client = ServerUnderTest::locate("u2s-xfa-mcp", "xfa")
        .connect()
        .await;

    // Read the real manifest, then corrupt its declared expectation in place
    // rather than the tool's behaviour — proves the *comparison*, not the tool.
    let mut manifest = u2s_render_test_harness::read_manifest(&client).await;
    manifest["test_vectors"][0]["expect"]["structured"]["count"] = serde_json::json!(999);
    let manifest = u2s_mcp::ServerManifest::parse(&manifest).expect("still parses");

    let vector = &manifest.test_vectors[0];
    let resolved = conformance::substitute_fixtures(&vector.args, &xfa_data_fixtures());
    let mut params = rmcp::model::CallToolRequestParams::new(vector.tool.clone());
    if let Some(obj) = resolved.as_object() {
        params = params.with_arguments(obj.clone());
    }
    let result = client.call_tool(params).await.expect("call");
    let actual = result.structured_content.unwrap_or(serde_json::Value::Null);
    let expected = &vector.expect["structured"];

    assert!(
        !conformance::subset_matches(&actual, expected),
        "a corrupted expectation must not subset-match the real result"
    );

    client.cancel().await.ok();
}
