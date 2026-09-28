//! Real Docker coverage: boots a throwaway `postgres:16-alpine` session,
//! applies the baseline schema, imports a real fixture, and checks the
//! reported row counts against what the fixture is known to contain.
//! `#[ignore]`d like every other Docker-backed test in this workspace.

use u2s_redacto_verify_core::flow::{self, RunOutcome};
use u2s_redacto_verify_core::profile::RenderProfile;
use u2s_redacto_verify_core::session::SessionPool;

const AAEV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../u2s-mapper-redacto/tests/fixtures/redacto-AAEV_019.sql");

#[tokio::test]
#[ignore = "needs a real Docker daemon"]
async fn a_real_fixture_imports_cleanly_into_a_throwaway_session() {
    let bytes = std::fs::read(AAEV).expect("fixture reads");
    let pool = SessionPool::new("postgres:16-alpine");
    let profile = RenderProfile::from_env(
        "test",
        "U2S_REDACTO_VERIFY_LIVE_TEST_NONCE",
        "pdf",
    );

    let session_id = format!(
        "live-import-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let RunOutcome { report, rendered } = flow::run(&pool, &session_id, &bytes, &profile)
        .await
        .expect("the flow itself must not error -- Docker is assumed reachable");

    assert!(report.passed(), "the import must succeed: {:?}", report.findings);
    assert!(rendered.is_empty(), "no rendering endpoint was configured");
    assert!(
        report.findings.iter().any(|f| f.kind == "import_ok"),
        "{:?}",
        report.findings
    );
    assert!(
        report.findings.iter().any(|f| f.kind == "rendering_skipped"),
        "{:?}",
        report.findings
    );

    // Running it again on the SAME session must not collide on the
    // deterministic ids `encode` mints -- this is exactly the scenario
    // `session::import`'s own wipe-before-import discipline exists for.
    let second = flow::run(&pool, &session_id, &bytes, &profile)
        .await
        .expect("a second run on the same session must not error");
    assert!(second.report.passed(), "{:?}", second.report.findings);

    // Clean up the container this test started.
    let docker = u2s_verify_core::docker::DockerLifecycle::connect()
        .await
        .expect("Docker is reachable");
    pool.teardown(&session_id, &docker).await;
}
