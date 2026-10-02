//! The whole `verify_run` flow: offline dump check, an import into a
//! session's own Redacto platform ([`crate::session`]), and a render of
//! every declared language. Each step's findings accumulate rather than short-circuiting
//! the run on the first problem, so a caller sees everything wrong in one
//! call.

use std::time::{Duration, Instant};

use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::types::{Finding, VerifyReport};

use crate::dump_check;
use crate::platform::{self, DocumentId, ImportError};
use crate::profile::RenderProfile;
use crate::session::RedactoSession;

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

/// The full flow: dump check, import into `session`'s platform database,
/// and a render per language. The caller holds `session`'s lock for the
/// whole call, so the render always sees this call's import. `blob_store`
/// is not taken here: this crate names no blob type of its own
/// ([`RenderedArtefact`]); the binary on top stores the rendered bytes.
pub async fn run(
    dump_bytes: &[u8],
    profile: &RenderProfile,
    docker: &DockerLifecycle,
    session: &RedactoSession,
) -> RunOutcome {
    let started = Instant::now();
    let mut findings = Vec::new();
    let mut rendered = Vec::new();
    let finish = |findings, rendered| RunOutcome {
        // Left empty on purpose: an `Artefact` carries a `BlobDescriptor`
        // this crate cannot mint. The binary on top stores each of
        // `rendered`'s bytes and appends the resulting artefacts itself.
        report: VerifyReport {
            dry_run: false,
            steps: Vec::new(),
            artefacts: Vec::new(),
            findings,
            duration_ms: started.elapsed().as_millis() as u64,
        },
        rendered,
    };

    let check = dump_check::check(dump_bytes);
    if !check.ok {
        findings.push(Finding::error(
            "dump_invalid",
            check.problem.clone().unwrap_or_else(|| "the dump did not decode".to_owned()),
        ));
        // A dump that does not even decode cannot be usefully imported --
        // stop here rather than handing psql bytes it will reject anyway
        // for a second, less informative reason.
        return finish(findings, rendered);
    }
    let raw_id = check.document_id.clone().expect("check.ok implies Some");
    let document_id = match DocumentId::parse(&raw_id) {
        Ok(id) => id,
        Err(message) => {
            findings.push(Finding::error("dump_invalid", message));
            return finish(findings, rendered);
        }
    };

    match platform::import(docker, &session.postgres.id, &document_id, dump_bytes).await {
        Ok(import) => {
            findings.push(Finding::warning(
                "import_ok",
                format!(
                    "imported {} row(s) for document {} into the platform database",
                    import.total_rows(),
                    document_id.as_str()
                ),
            ));
            if import.table_counts.get("documents").copied().unwrap_or(0) != 1 {
                findings.push(Finding::error(
                    "unexpected_document_count",
                    format!(
                        "expected exactly one documents row for {}, found {:?}",
                        document_id.as_str(),
                        import.table_counts.get("documents")
                    ),
                ));
                return finish(findings, rendered);
            }
        }
        Err(err) => {
            let kind = match err {
                ImportError::Psql { .. } | ImportError::NotUtf8(_) => "import_failed",
                ImportError::Docker(_) => "platform_unreachable",
            };
            findings.push(Finding::error(kind, err.to_string()));
            return finish(findings, rendered);
        }
    }

    for language in &check.languages {
        match render_one(profile, &session.rendering_base_url, document_id.as_str(), language).await {
            Ok(bytes) => {
                match blank_render(&bytes) {
                    Ok(false) => {}
                    Ok(true) => findings.push(Finding::error(
                        "render_blank",
                        format!(
                            "the {language} render has no text on any page: the platform could \
                             not style the document, e.g. because it does not ship the stylesheet \
                             the document names (its core service logs the cause)"
                        ),
                    )),
                    Err(message) => findings.push(Finding::error("render_failed", message)),
                }
                rendered.push(RenderedArtefact {
                    language: language.clone(),
                    bytes,
                    media_type: "application/pdf",
                });
            }
            Err(message) => findings.push(Finding::error("render_failed", message)),
        }
    }
    finish(findings, rendered)
}

/// `POST {base}/bin/redacto/rendering/integration?profile=json-default&format={pdf|pdf-ua}`,
/// the platform's own rendering API
/// (`ajila-redacto-platform/.context/assets/api/redacto-rendering.openapi.json`).
/// Called after [`platform::import`] put `document_id` into the database
/// the platform's `core` service reads.
async fn render_one(
    profile: &RenderProfile,
    rendering_base_url: &str,
    document_id: &str,
    language: &str,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "{}/bin/redacto/rendering/integration?profile=json-default&format={}",
        rendering_base_url.trim_end_matches('/'),
        profile.render_format
    );
    let (user, password) = &profile.basic_auth;
    let request = client
        .post(&url)
        .json(&serde_json::json!({ "documentId": document_id, "language": language }))
        .basic_auth(user, Some(password));

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

/// Whether a rendered PDF has no text at all, judged by its pages declaring
/// no font: text needs one, and a render the platform could not style comes
/// back as a valid PDF of blank pages. Pure.
fn blank_render(pdf: &[u8]) -> Result<bool, String> {
    let doc = lopdf::Document::load_mem(pdf).map_err(|e| format!("the rendered PDF cannot be read: {e}"))?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Ok(true);
    }
    for page in pages.values() {
        let fonts = doc
            .get_page_fonts(*page)
            .map_err(|e| format!("the rendered PDF's fonts cannot be read: {e}"))?;
        if !fonts.is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF whose page declares the given fonts.
    fn pdf_with_fonts(fonts: lopdf::Dictionary) -> Vec<u8> {
        use lopdf::{Document, Object, dictionary};
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! { "Font" => fonts },
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("the PDF saves");
        bytes
    }

    /// The platform answers a render it could not style (a stylesheet it does
    /// not ship) with a valid PDF of blank pages: no page declares a font.
    #[test]
    fn a_render_without_a_font_is_blank() {
        assert_eq!(blank_render(&pdf_with_fonts(lopdf::Dictionary::new())), Ok(true));
        let font = lopdf::dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" };
        assert_eq!(blank_render(&pdf_with_fonts(lopdf::dictionary! { "F1" => font })), Ok(false));
        assert!(blank_render(b"%PDF-1.7 not really").is_err());
    }

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
