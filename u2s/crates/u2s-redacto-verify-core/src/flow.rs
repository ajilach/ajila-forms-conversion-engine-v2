//! The whole `verify_run` flow: offline dump check, a real Postgres import,
//! and -- only when the profile configures one -- a render call against an
//! already-running platform. Each step's own findings accumulate rather
//! than short-circuiting the whole run on the first problem, so a caller
//! sees everything wrong in one call.

use std::time::{Duration, Instant};

use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::types::{Finding, VerifyReport};

use crate::profile::RenderProfile;
use crate::session::{self, SessionPool};
use crate::{dump_check, session::SessionError};

#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    #[error("cannot reach the Docker daemon: is it running?")]
    DockerUnreachable,
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// A rendered artefact's bytes, kept alongside its own blob so a caller can
/// choose how to store it -- this crate names no blob store of its own,
/// consistent with `u2s-verify-core`'s own genericity.
pub struct RenderedArtefact {
    pub language: String,
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
}

pub struct RunOutcome {
    pub report: VerifyReport,
    pub rendered: Vec<RenderedArtefact>,
}

/// `dry_run: true`: only [`dump_check::check`] runs -- no Docker, no
/// network. Never fails as a `FlowError`; a bad dump is a [`Finding`], not
/// a tool error, exactly `verify_package_check`'s own contract on the AEM
/// side.
pub fn dry_run(dump_bytes: &[u8]) -> VerifyReport {
    let started = Instant::now();
    let mut findings = Vec::new();
    let report = dump_check::check(dump_bytes);
    if !report.ok {
        findings.push(Finding::error(
            "dump_invalid",
            report.problem.unwrap_or_else(|| "the dump did not decode".to_owned()),
        ));
    }
    VerifyReport::dry(findings, started.elapsed().as_millis() as u64)
}

/// The full flow: dump check, a real Postgres import on `session_id`'s own
/// session, and an optional render call. `blob_store` is a callback rather
/// than a `u2s_blob::BlobStore` reference so this crate stays free to name
/// a blob type of its own ([`RenderedArtefact`]) instead of depending on
/// `u2s-blob`'s -- reserved for the binary on top, which already links it
/// for `encode`/`decode`.
pub async fn run(
    pool: &SessionPool,
    session_id: &str,
    dump_bytes: &[u8],
    profile: &RenderProfile,
) -> Result<RunOutcome, FlowError> {
    let started = Instant::now();
    let mut findings = Vec::new();
    let mut rendered = Vec::new();

    let check = dump_check::check(dump_bytes);
    if !check.ok {
        findings.push(Finding::error(
            "dump_invalid",
            check.problem.clone().unwrap_or_else(|| "the dump did not decode".to_owned()),
        ));
        // A dump that does not even decode cannot be usefully imported --
        // stop here rather than handing psql bytes it will reject anyway
        // for a second, less informative reason.
        return Ok(RunOutcome {
            report: VerifyReport {
                dry_run: false,
                steps: Vec::new(),
                artefacts: Vec::new(),
                findings,
                duration_ms: started.elapsed().as_millis() as u64,
            },
            rendered,
        });
    }

    let docker = DockerLifecycle::connect().await.map_err(|_| FlowError::DockerUnreachable)?;
    pool.sweep(&docker).await;
    let mut guard = pool.ensure(session_id, &docker).await?;
    let state = guard.as_mut().expect("ensure() always leaves Some behind");

    let import = session::import(&docker, &state.container_id, dump_bytes).await?;
    findings.push(Finding::warning(
        "import_ok",
        format!(
            "imported {} row(s) across {} table(s) into a throwaway Postgres session",
            import.total_rows(),
            import.table_counts.len()
        ),
    ));
    if import.table_counts.get("documents").copied().unwrap_or(0) != 1 {
        findings.push(Finding::error(
            "unexpected_document_count",
            format!("expected exactly one documents row, found {:?}", import.table_counts.get("documents")),
        ));
    }

    match &profile.rendering_base_url {
        None => {
            findings.push(Finding::warning(
                "rendering_skipped",
                "no rendering endpoint configured for this profile -- only the Postgres import \
                 was checked, not the rendered output. Set the profile's rendering URL to also \
                 render this document (the dump must already be imported into THAT platform's \
                 own database -- see RenderProfile's own doc)."
                    .to_owned(),
            ));
        }
        Some(base_url) => {
            let document_id = check.document_id.clone().expect("check.ok implies Some");
            for language in &check.languages {
                match render_one(base_url, profile, &document_id, language).await {
                    Ok(bytes) => rendered.push(RenderedArtefact {
                        language: language.clone(),
                        bytes,
                        media_type: "application/pdf",
                    }),
                    Err(message) => {
                        findings.push(Finding::error("render_failed", message));
                    }
                }
            }
        }
    }

    Ok(RunOutcome {
        // Left empty on purpose: an `Artefact` carries a `BlobDescriptor`
        // this crate cannot mint (it names no blob store -- see this
        // function's own doc). The binary on top stores each of
        // `rendered`'s bytes and appends the resulting artefacts itself.
        report: VerifyReport {
            dry_run: false,
            steps: Vec::new(),
            artefacts: Vec::new(),
            findings,
            duration_ms: started.elapsed().as_millis() as u64,
        },
        rendered,
    })
}

/// `POST {base}/bin/redacto/rendering/integration?profile=json-default&format={pdf|pdf-ua}`,
/// the platform's own rendering API
/// (`ajila-redacto-platform/.context/assets/api/redacto-rendering.openapi.json`).
/// Assumes `document_id` is already imported into whatever database that
/// endpoint reads from -- see [`RenderProfile::rendering_base_url`]'s own
/// doc on why this crate never arranges that itself.
async fn render_one(
    base_url: &str,
    profile: &RenderProfile,
    document_id: &str,
    language: &str,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "{}/bin/redacto/rendering/integration?profile=json-default&format={}",
        base_url.trim_end_matches('/'),
        profile.render_format
    );
    let mut request = client
        .post(&url)
        .json(&serde_json::json!({ "documentId": document_id, "language": language }));
    if let Some((user, password)) = &profile.basic_auth {
        request = request.basic_auth(user, Some(password));
    }

    let response = request.send().await.map_err(|e| format!("{url}: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if !bytes.starts_with(b"%PDF-") {
        return Err(format!("{url} did not return a PDF (got {} bytes)", bytes.len()));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_on_garbage_reports_an_error_finding() {
        let report = dry_run(b"not a dump");
        assert!(report.dry_run);
        assert!(!report.passed());
        assert_eq!(report.findings[0].kind, "dump_invalid");
    }

    #[test]
    fn dry_run_on_a_real_fixture_passes() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../u2s-mapper-redacto/tests/fixtures/redacto-AAEV_019.sql"
        ))
        .expect("fixture reads");
        let report = dry_run(&bytes);
        assert!(report.dry_run);
        assert!(report.passed(), "{:?}", report.findings);
    }
}
