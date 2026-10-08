//! Real platform coverage: boots a session's own Redacto platform, imports a
//! real fixture, renders it, and imports it again to prove the replace (the
//! deterministic ids would otherwise collide). Then tears the platform down
//! and checks nothing labelled for it is left. `#[ignore]`d like every other
//! Docker-backed test in this workspace; needs a Docker daemon holding the
//! platform images and `U2S_REDACTO_VERIFY_UBS_{MIGRATION,CORE,RENDERING}_IMAGE`
//! naming them (the values in `.env.example`).

use u2s_redacto_verify_core::flow::{self, RunOutcome};
use u2s_redacto_verify_core::platform::{self, DocumentId};
use u2s_redacto_verify_core::profile::RenderProfile;
use u2s_redacto_verify_core::session;
use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::session::{PooledSession, Reach, owner_label};

const AAEV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../u2s-mapper-redacto/tests/fixtures/redacto-AAEV_019.sql");

#[tokio::test]
#[ignore = "needs a Docker daemon with the Redacto platform images"]
async fn a_booted_platform_imports_renders_reimports_and_goes_away() {
    let bytes = std::fs::read(AAEV).expect("fixture reads");
    let profile = RenderProfile::from_env("redacto-live-test", "U2S_REDACTO_VERIFY_UBS", "pdf-ua")
        .expect("the U2S_REDACTO_VERIFY_UBS_*_IMAGE variables are set");
    let docker = DockerLifecycle::connect().await.expect("Docker is reachable");
    // What an earlier, failed run of this test left behind.
    u2s_verify_core::session::remove_leftovers(profile.name, &Reach::Published).await;

    let platform_session = session::boot(&docker, &profile, "live-import")
        .await
        .expect("the platform boots");

    for round in 1..=2 {
        let RunOutcome { report, rendered } = flow::run(&bytes, &profile, &docker, &platform_session).await;
        assert!(report.passed(), "round {round}: {:?}", report.findings);
        assert!(
            report.findings.iter().any(|f| f.kind == "import_ok"),
            "round {round}: {:?}",
            report.findings
        );
        assert!(!rendered.is_empty(), "round {round}: one PDF per declared language");
        for artefact in &rendered {
            assert!(artefact.bytes.starts_with(b"%PDF-"), "round {round}");
        }
    }

    // The second import replaced the first rather than adding to it.
    let id = DocumentId::parse("aaev_019").unwrap();
    let counted = docker
        .exec(
            &platform_session.postgres.id,
            ["psql", "-U", platform::DB_USER, "-d", platform::DB_NAME, "-t", "-A", "-F", "|", "-c"]
                .map(str::to_owned)
                .into_iter()
                .chain([platform::count_query(&id)])
                .collect(),
            None,
        )
        .await
        .expect("count");
    let counts = platform::parse_counts(&counted.output);
    assert_eq!(counts.get("documents"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("assets"), Some(&18), "{counts:?}");

    platform_session.teardown(&docker).await;
    let label = owner_label(profile.name);
    assert!(docker.find_by_label(&label).await.unwrap().is_empty());
    assert!(docker.find_networks_by_label(&label).await.unwrap().is_empty());
}
