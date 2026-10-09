//! The shape `verify_run` returns -- exactly the contract
//! `u2s_mcp::manifest::VerifyCapability::Run` documents, so a format's
//! verifier and the platform code that consumes it can never drift apart:
//! this module *is* the contract, not a copy of it.
//!
//! Pure and format-agnostic: nothing here names AEM, Docker or Chromium.
//! `u2s-aem-verify-core` builds a [`VerifyReport`] by driving both; this
//! crate only knows the shape the result must take.

use serde::Serialize;
use serde_json::{Value, json};

/// A blob descriptor, matching `u2s_blob::BlobRef`'s public fields exactly
/// -- kept as its own type rather than depending on `u2s-blob`'s `BlobRef`
/// (which also carries a local filesystem `path` no tool result should
/// ever leak) so this crate's JSON output cannot accidentally grow that
/// field back in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlobDescriptor {
    pub handle: String,
    pub media_type: String,
    pub byte_len: usize,
    pub digest: String,
}

impl From<&u2s_blob::BlobRef> for BlobDescriptor {
    fn from(blob: &u2s_blob::BlobRef) -> Self {
        Self {
            handle: blob.handle.clone(),
            media_type: blob.media_type.clone(),
            byte_len: blob.byte_len,
            digest: blob.digest.clone(),
        }
    }
}

/// One rendered step of the flow -- a full-page screenshot plus whatever
/// the browser reported while that page was open. `console_errors` and
/// `failed_requests` are scoped to this step, not the whole run, so a
/// finding can be read next to the screenshot it belongs to without cross
/// referencing a timeline.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub name: String,
    pub screenshot: BlobDescriptor,
    pub console_errors: Vec<String>,
    pub failed_requests: Vec<FailedRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedRequest {
    pub url: String,
    /// Absent for a request that never got a response at all (DNS
    /// failure, connection refused) -- distinct from a response that
    /// carried an error status, which is `Some(4xx|5xx)`.
    pub status: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtefactKind {
    Download,
    Package,
}

/// Something the run produced worth keeping beyond a step's screenshot --
/// the encoded package itself (so the caller need not have kept the
/// handle it sent), or whatever the submit action produced.
#[derive(Debug, Clone, Serialize)]
pub struct Artefact {
    pub kind: ArtefactKind,
    pub label: String,
    pub blob: BlobDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// What went wrong or was merely worth flagging. `kind` is a stable,
/// machine-matchable slug (see [`ErrorKind`] for the vocabulary a `verify_run`
/// tool error uses; a finding's `kind` draws from the same words for
/// anything short of a hard failure -- `"not_warmed"`, `"validation_failed"`,
/// `"teardown_failed"` -- so a caller does not need two vocabularies for
/// "this failed outright" versus "this is worth knowing").
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub kind: String,
    pub message: String,
}

impl Finding {
    pub fn error(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            kind: kind.into(),
            message: message.into(),
        }
    }

    pub fn warning(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            kind: kind.into(),
            message: message.into(),
        }
    }
}

/// The whole `verify_run` result. `dry_run: true` means `steps` and
/// `artefacts` are always empty -- a dry run validates inputs and the
/// tool's own readiness without touching Docker or a browser, which is
/// what lets a conformance vector exercise the contract without the side
/// effect `run` performs for real.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub dry_run: bool,
    pub steps: Vec<Step>,
    pub artefacts: Vec<Artefact>,
    pub findings: Vec<Finding>,
    pub duration_ms: u64,
}

impl VerifyReport {
    pub fn dry(findings: Vec<Finding>, duration_ms: u64) -> Self {
        Self {
            dry_run: true,
            steps: Vec::new(),
            artefacts: Vec::new(),
            findings,
            duration_ms,
        }
    }

    /// True iff nothing in `findings` is [`Severity::Error`] -- the
    /// question "did the tool call itself fail" is separate from this: a
    /// report can be `passed()` and still be returned inside a tool error
    /// if, say, the flow could not even start (see [`ErrorKind`]).
    pub fn passed(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// The `structured_content` shape `verify_run` returns on success --
    /// see `u2s_mcp::manifest::VerifyCapability`'s doc comment, which this
    /// mirrors field for field.
    pub fn to_structured_content(&self) -> Value {
        json!({
            "dry_run": self.dry_run,
            "steps": self.steps,
            "artefacts": self.artefacts,
            "findings": self.findings,
            "duration_ms": self.duration_ms,
        })
    }

    /// One line summarising the run for the tool result's text content
    /// block -- the part a model reads without dereferencing anything.
    pub fn summary_text(&self) -> String {
        let errors = self
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count();
        let warnings = self.findings.len() - errors;
        if self.dry_run {
            format!("dry run: {errors} error(s), {warnings} warning(s), nothing was touched")
        } else {
            format!(
                "{} step(s), {} artefact(s), {errors} error(s), {warnings} warning(s), {}ms",
                self.steps.len(),
                self.artefacts.len(),
                self.duration_ms
            )
        }
    }
}

/// The stable vocabulary a `verify_run` tool *error* (as opposed to a
/// finding inside a successful report) names itself with, so a caller can
/// match on `kind` rather than parse prose. One flow, one taxonomy --
/// `u2s-aem-verify-core::flow` is the only producer, this is where the
/// words are pinned so they cannot drift between what it throws and what
/// a test asserts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    DockerUnreachable,
    ImageMissing,
    AemNotReady,
    ChromiumNotReady,
    PackageInvalid,
    InstallFailed,
    FormNotFound,
    RenderTimeout,
    NoDownload,
    DownloadNotPdf,
    /// The submit produced a PDF, but one that draws nothing on any page
    /// (`crate::pdf_content::blank_pdf`): the rendering dependency answered
    /// a request whose payload was empty or that it could not style. The
    /// PDF is still kept as an artefact, since its bytes are the evidence.
    DownloadBlank,
    /// The one kind not named in the plan's own taxonomy, added here
    /// rather than folded into an unrelated one: writing a screenshot,
    /// the package, or a downloaded artefact to the blob store failed
    /// (an unwritable or full `U2S_BLOB_DIR`) -- an operational failure of
    /// this server, not a finding about the form under test.
    StorageFailed,
    /// A profile that configures a rendering dependency (`u2s-aem-verify-core`'s
    /// Redacto Summary URL) found it unreachable before a submit that would
    /// have depended on it -- refused up front rather than left to fail
    /// obscurely partway through `guideBridge.submit()`.
    RedactoUnreachable,
    /// A verifier that boots its own Redacto platform
    /// (`u2s-redacto-verify-core::session`) could not bring it up: a
    /// container failed to start, the migration failed, or a service never
    /// became ready.
    RedactoNotReady,
    /// `verify_run` was called on a `session_id` that already has a form
    /// open for interaction (`u2s-aem-verify-core::interactive`). The two
    /// paths share one installed-package slot per AEM session, so they
    /// cannot run at once -- the caller must `verify_close` first.
    FormOpen,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DockerUnreachable => "docker_unreachable",
            Self::ImageMissing => "image_missing",
            Self::AemNotReady => "aem_not_ready",
            Self::ChromiumNotReady => "chromium_not_ready",
            Self::PackageInvalid => "package_invalid",
            Self::InstallFailed => "install_failed",
            Self::FormNotFound => "form_not_found",
            Self::RenderTimeout => "render_timeout",
            Self::NoDownload => "no_download",
            Self::DownloadNotPdf => "download_not_pdf",
            Self::DownloadBlank => "download_blank",
            Self::StorageFailed => "storage_failed",
            Self::RedactoUnreachable => "redacto_unreachable",
            Self::RedactoNotReady => "redacto_not_ready",
            Self::FormOpen => "form_open",
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `verify_run` tool error: `error_kind` first so a caller can match on
/// it, `message` for the human/model-readable detail.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct VerifyError {
    pub kind: ErrorKind,
    pub message: String,
}

impl VerifyError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(handle: &str) -> BlobDescriptor {
        BlobDescriptor {
            handle: handle.to_owned(),
            media_type: "image/png".to_owned(),
            byte_len: 100,
            digest: "deadbeef".to_owned(),
        }
    }

    #[test]
    fn a_dry_run_report_carries_no_steps_or_artefacts() {
        let report = VerifyReport::dry(vec![Finding::warning("not_warmed", "no warm image")], 5);
        assert!(report.dry_run);
        assert!(report.steps.is_empty());
        assert!(report.artefacts.is_empty());
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn passed_is_false_when_any_finding_is_an_error() {
        let mut report = VerifyReport::dry(vec![], 0);
        assert!(report.passed(), "no findings at all must still pass");

        report.findings.push(Finding::warning("not_warmed", "x"));
        assert!(report.passed(), "a warning alone must not fail the report");

        report.findings.push(Finding::error("form_not_found", "x"));
        assert!(!report.passed(), "one error must fail the report");
    }

    #[test]
    fn structured_content_matches_the_verify_run_contract_shape() {
        let report = VerifyReport {
            dry_run: false,
            steps: vec![Step {
                name: "page-1".to_owned(),
                screenshot: blob("aaaa.png"),
                console_errors: vec!["TypeError: x".to_owned()],
                failed_requests: vec![FailedRequest {
                    url: "https://example/x.js".to_owned(),
                    status: Some(404),
                }],
            }],
            artefacts: vec![Artefact {
                kind: ArtefactKind::Download,
                label: "submitted form".to_owned(),
                blob: blob("bbbb.pdf"),
            }],
            findings: vec![Finding::error("form_not_found", "no form at that path")],
            duration_ms: 1234,
        };

        let value = report.to_structured_content();
        assert_eq!(value["dry_run"], json!(false));
        assert_eq!(value["steps"][0]["name"], json!("page-1"));
        assert_eq!(value["steps"][0]["screenshot"]["handle"], json!("aaaa.png"));
        assert_eq!(
            value["steps"][0]["failed_requests"][0]["status"],
            json!(404)
        );
        assert_eq!(value["artefacts"][0]["kind"], json!("download"));
        assert_eq!(value["findings"][0]["severity"], json!("error"));
        assert_eq!(value["duration_ms"], json!(1234));
    }

    #[test]
    fn a_failed_request_with_no_response_serialises_status_as_null() {
        let request = FailedRequest {
            url: "https://example/x".to_owned(),
            status: None,
        };
        let value = serde_json::to_value(&request).expect("serialises");
        assert_eq!(value["status"], Value::Null);
    }

    #[test]
    fn error_kind_strings_are_stable_and_distinct() {
        let all = [
            ErrorKind::DockerUnreachable,
            ErrorKind::ImageMissing,
            ErrorKind::AemNotReady,
            ErrorKind::ChromiumNotReady,
            ErrorKind::PackageInvalid,
            ErrorKind::InstallFailed,
            ErrorKind::FormNotFound,
            ErrorKind::RenderTimeout,
            ErrorKind::NoDownload,
            ErrorKind::DownloadNotPdf,
            ErrorKind::DownloadBlank,
            ErrorKind::StorageFailed,
            ErrorKind::RedactoUnreachable,
            ErrorKind::RedactoNotReady,
        ];
        let strings: std::collections::BTreeSet<&str> = all.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            strings.len(),
            all.len(),
            "every error kind must be distinct"
        );
    }

    #[test]
    fn a_verify_error_displays_its_kind_and_message() {
        let err = VerifyError::new(ErrorKind::AemNotReady, "timed out after 900s");
        assert_eq!(err.to_string(), "aem_not_ready: timed out after 900s");
    }
}
