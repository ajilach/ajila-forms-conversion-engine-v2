//! The images a person reviews a finished run by: its source rendered page by
//! page, next to what the run's output looks like on its verifier.
//!
//! The output side only exists while the verifier runs, so [`capture`] takes it
//! at the end of a run, from one last verification per language of the build
//! that ships, before the controller tears the verifier down. The source side
//! can be rendered at any time from the source bytes ([`render_sources`]),
//! each file tagged with its language so a person compares like with like.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use u2s_render_core::{ImageFormat, Limits, RenderError, RenderedPage, RenderedPages};

use crate::{ConversionAgent, ToolReply};

/// The resolution pages are rendered at: sharp enough to read a form's small
/// print side by side, small enough that a long form stays a few megabytes.
const REVIEW_DPI: f32 = 110.0;

/// One image of a review, PNG-encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewImage {
    /// What it shows, e.g. `panel-2` or `Document of Record, page 3`.
    pub label: String,
    pub png: Vec<u8>,
}

/// What a run's output looks like, in each language it ships, in the order
/// the form declares them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Review {
    pub languages: Vec<LanguageReview>,
}

impl Review {
    pub fn is_empty(&self) -> bool {
        self.languages.iter().all(|language| language.images.is_empty())
    }
}

/// What a run's output looks like in one language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageReview {
    /// The document's code for it (ISO 639-1, `es` rather than the form
    /// metadata's `sp`), the one [`SourceReview::language`] uses. Empty for
    /// images captured before the language was recorded.
    pub language: String,
    pub images: ReviewImages,
}

/// What a run's output looks like in one language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewImages {
    /// Screenshots of the rendered form, in the order the verifier took them.
    pub form: Vec<ReviewImage>,
    /// The pages of every PDF the verifier produced.
    pub output: Vec<ReviewImage>,
}

impl ReviewImages {
    pub fn is_empty(&self) -> bool {
        self.form.is_empty() && self.output.is_empty()
    }

    /// Both sides, each with the name the store and the CLI file it by.
    pub fn sides(&self) -> [(&'static str, &[ReviewImage]); 2] {
        [(FORM_SIDE, &self.form), (OUTPUT_SIDE, &self.output)]
    }

    /// [`Self::sides`], to fill in.
    pub(crate) fn sides_mut(&mut self) -> [(&'static str, &mut Vec<ReviewImage>); 2] {
        [(FORM_SIDE, &mut self.form), (OUTPUT_SIDE, &mut self.output)]
    }
}

const FORM_SIDE: &str = "form";
const OUTPUT_SIDE: &str = "output";

/// What [`capture`] took, with what the verification reported as wrong: a
/// failed submit still leaves its screenshots worth reviewing, and a language
/// that failed leaves the others.
#[derive(Debug)]
pub struct Captured {
    pub review: Review,
    pub problems: Vec<String>,
}

/// What one language's verification left, as [`Captured`] has it.
#[derive(Debug)]
struct LanguageCapture {
    images: ReviewImages,
    problems: Vec<String>,
}

/// Verifies the agent's current build once more in every language its form
/// declares and keeps what that produced as images. Records nothing as stage
/// evidence: no stage is running.
///
/// An error when no language could be verified or produced anything to show;
/// each language that failed is a problem.
pub async fn capture(agent: &mut ConversionAgent) -> Result<Captured, String> {
    let languages = review_languages(&agent.aem_languages()?)?;
    let mut review = Review::default();
    let mut problems = Vec::new();
    for (language, code) in languages {
        match capture_language(agent, &code).await {
            Ok(captured) => {
                problems.extend(captured.problems.into_iter().map(|p| format!("{language}: {p}")));
                review.languages.push(LanguageReview {
                    language,
                    images: captured.images,
                });
            }
            Err(e) => problems.push(format!("{language}: {e}")),
        }
    }
    if review.languages.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(Captured { review, problems })
}

/// Each language of the form, as `(the document's code, the code the
/// verifier opens it with)`: the form's metadata files Spanish as `sp`, a
/// source and the document as `es`. A language listed twice under two codes
/// is captured once, under the first.
fn review_languages(codes: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut languages: Vec<(String, String)> = Vec::new();
    for code in codes {
        let language = u2s_aem_ubs_mcp::iso_language(code).map_err(|e| e.to_string())?;
        if !languages.iter().any(|(known, _)| *known == language) {
            languages.push((language, code.clone()));
        }
    }
    if languages.is_empty() {
        return Err("the form declares no language to open it in".into());
    }
    Ok(languages)
}

/// One verification, with the form opened in the verifier's `code`.
async fn capture_language(agent: &mut ConversionAgent, code: &str) -> Result<LanguageCapture, String> {
    let tool = "aem_verify_run";
    let report = match agent.execute_as(tool, &json!({ "language": code }), &crate::Caller::Host).await {
        ToolReply::Error(e) => return Err(format!("{tool} failed: {e}")),
        reply => crate::conversion::json_of(&reply).ok_or_else(|| format!("{tool} returned no report"))?,
    };
    let located = locate(&report);
    tokio::task::spawn_blocking(move || read_located(located))
        .await
        .map_err(|e| format!("rendering the review images failed: {e}"))?
}

/// The files a verifier report names, by what they are.
#[derive(Debug, Default, PartialEq)]
struct Located {
    /// `(step name, path)` of every screenshot.
    screenshots: Vec<(String, PathBuf)>,
    /// `(artefact label, path)` of every PDF.
    pdfs: Vec<(String, PathBuf)>,
    /// The report's error findings, and any file it names but did not keep.
    problems: Vec<String>,
}

/// Reads a verifier report: its steps' screenshots, its PDF artefacts and its
/// error findings. Blob paths are the `doc_path`s the agent adds to a reply.
fn locate(report: &Value) -> Located {
    let mut located = Located::default();
    let mut path_of = |what: String, blob: &Value| match blob["doc_path"].as_str() {
        Some(path) => Some((what, PathBuf::from(path))),
        None => {
            located.problems.push(format!("{what}: the verifier kept no file"));
            None
        }
    };
    let screenshots: Vec<_> = report["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| path_of(step["name"].as_str().unwrap_or("step").to_string(), &step["screenshot"]))
        .collect();
    let pdfs: Vec<_> = report["artefacts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|artefact| artefact["blob"]["media_type"] == "application/pdf")
        .filter_map(|artefact| path_of(artefact["label"].as_str().unwrap_or("PDF").to_string(), &artefact["blob"]))
        .collect();
    located.screenshots = screenshots;
    located.pdfs = pdfs;
    located.problems.extend(
        report["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|finding| finding["severity"] == "error")
            .filter_map(|finding| finding["message"].as_str().map(String::from)),
    );
    located
}

/// Reads the screenshots and renders the PDFs [`locate`] found.
fn read_located(mut located: Located) -> Result<LanguageCapture, String> {
    let mut images = ReviewImages::default();
    for (label, path) in located.screenshots {
        let png = std::fs::read(&path).map_err(|e| format!("reading screenshot {label}: {e}"))?;
        images.form.push(ReviewImage { label, png });
    }
    for (label, path) in &located.pdfs {
        let pages = render_pdf(path, label)?;
        for page in &pages {
            if renders_blank(&page.png)? {
                located.problems.push(format!("{} renders blank", page.label));
            }
        }
        images.output.extend(pages);
    }
    if images.is_empty() {
        let why = if located.problems.is_empty() {
            String::new()
        } else {
            format!(": {}", located.problems.join("; "))
        };
        return Err(format!("the verification produced nothing to review{why}"));
    }
    Ok(LanguageCapture {
        images,
        problems: located.problems,
    })
}

/// One source PDF, rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceReview {
    pub file: String,
    /// The language its XFA form declares, the document's code for it; `None`
    /// for a plain PDF, which says none.
    pub language: Option<String>,
    pub pages: Vec<ReviewImage>,
}

/// Renders every page of the source PDFs, in order, each page labelled with
/// its file. Files that are not PDFs (an attached template package) are
/// skipped. XFA forms go through the XFA renderer, which draws them in their
/// initial state; pdfium only shows such a form's "please wait" placeholder.
pub fn render_sources(files: &[(String, Vec<u8>)]) -> Result<Vec<SourceReview>, String> {
    crate::u2s::register_fonts()?;
    let dir = tempfile::Builder::new()
        .prefix("blueprint-review-")
        .tempdir()
        .map_err(|e| format!("could not create a directory to render the sources in: {e}"))?;
    let mut sources = Vec::new();
    for (index, (name, bytes)) in files.iter().enumerate() {
        if !crate::conversion::is_source_pdf(name) {
            continue;
        }
        let path = dir.path().join(format!("{index}.pdf"));
        std::fs::write(&path, bytes).map_err(|e| format!("writing {name}: {e}"))?;
        // Only an XFA form has a context to read.
        let context = crate::source::read(bytes).map_err(|e| format!("reading {name}: {e}"))?;
        let (language, pages) = if let Some(context) = context {
            let xfa = u2s_render_xfa::Renderer::new(limits());
            let target = u2s_render_xfa::Target::doc(&path, &u2s_xfa::states::StateSpec::default());
            let pages = labelled(
                name,
                walk(|from| xfa.render_pages(&target, None, Some(from), None, Some(REVIEW_DPI), None, ImageFormat::Png))
                    .map_err(|e| format!("rendering {name}: {e}"))?,
            );
            (Some(context.language), pages)
        } else {
            (None, render_pdf(&path, name)?)
        };
        sources.push(SourceReview {
            file: name.clone(),
            language,
            pages,
        });
    }
    Ok(sources)
}

/// Lighter than this, a pixel counts as paper: anti-aliasing leaves text
/// edges well below it, and a page carrying only that is one nobody can
/// read either.
const PAPER_LUMA: u8 = 250;

/// Whether a rendered page shows nothing: every pixel is paper-white. The
/// verifier's own `download_blank` finding judges the PDF's content stream;
/// this judges what a person would see, which also catches text drawn with
/// a font the renderer could not load. Pure.
fn renders_blank(png: &[u8]) -> Result<bool, String> {
    let image = image::load_from_memory(png).map_err(|e| format!("reading a rendered page: {e}"))?;
    Ok(image.to_luma8().pixels().all(|pixel| pixel.0[0] >= PAPER_LUMA))
}

/// Renders every page of a plain PDF with pdfium.
fn render_pdf(path: &Path, label: &str) -> Result<Vec<ReviewImage>, String> {
    let pdfium = crate::pdfium::renderer(limits())?;
    let pages = walk(|from| pdfium.render_pages(path, None, Some(from), None, Some(REVIEW_DPI), None, ImageFormat::Png))
        .map_err(|e| format!("rendering {label}: {e}"))?;
    Ok(labelled(label, pages))
}

/// The renderers' limits, with batches large enough that a form walks in a
/// few calls: these images go to a person, not into a model's context.
fn limits() -> Limits {
    Limits {
        max_images_per_call: 16,
        max_response_bytes: 64 * 1024 * 1024,
        ..Limits::default()
    }
}

/// Every page of a document, following a renderer's cursor from page 1.
fn walk(
    mut batch: impl FnMut(u32) -> Result<RenderedPages, RenderError>,
) -> Result<Vec<RenderedPage>, RenderError> {
    let mut pages = Vec::new();
    let mut from = Some(1);
    while let Some(start) = from {
        let rendered = batch(start)?;
        pages.extend(rendered.pages);
        from = rendered.next_from;
    }
    Ok(pages)
}

fn labelled(label: &str, pages: Vec<RenderedPage>) -> Vec<ReviewImage> {
    pages
        .into_iter()
        .map(|page| ReviewImage {
            label: format!("{label}, page {}", page.page),
            png: page.data,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn png_of(pixels: image::GrayImage) -> Vec<u8> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(pixels)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        png.into_inner()
    }

    /// A page of paper only is blank; one with a single dark mark is not.
    #[test]
    fn a_rendered_page_is_blank_only_when_every_pixel_is_paper() {
        let white = image::GrayImage::from_pixel(40, 60, image::Luma([255]));
        assert_eq!(renders_blank(&png_of(white.clone())), Ok(true));
        let mut off_white = white.clone();
        off_white.put_pixel(3, 3, image::Luma([PAPER_LUMA]));
        assert_eq!(renders_blank(&png_of(off_white)), Ok(true), "a near-white speck is still paper");
        let mut marked = white;
        marked.put_pixel(20, 30, image::Luma([40]));
        assert_eq!(renders_blank(&png_of(marked)), Ok(false));
        assert!(renders_blank(b"not a png").is_err());
    }

    fn form(name: &str) -> (String, Vec<u8>) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../forms").join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        (name.to_string(), bytes)
    }

    /// An AEM report: every step's screenshot, the PDF artefact but not the
    /// package, and the error findings but not the warnings.
    #[test]
    fn an_aem_report_locates_its_screenshots_and_pdf() {
        let report = json!({
            "steps": [
                { "name": "form", "screenshot": { "media_type": "image/png", "doc_path": "/b/1.png" } },
                { "name": "panel-2", "screenshot": { "media_type": "image/png", "doc_path": "/b/2.png" } },
                { "name": "after-submit", "screenshot": { "media_type": "image/png" } },
            ],
            "artefacts": [
                { "kind": "package", "label": "package", "blob": { "media_type": "application/zip", "doc_path": "/b/p.zip" } },
                { "kind": "download", "label": "Document of Record", "blob": { "media_type": "application/pdf", "doc_path": "/b/dor.pdf" } },
            ],
            "findings": [
                { "severity": "warning", "kind": "auto_filled", "message": "filled 3 fields" },
                { "severity": "error", "kind": "no_download", "message": "no PDF was downloaded" },
            ],
        });
        let located = locate(&report);
        assert_eq!(
            located.screenshots,
            [("form".to_string(), PathBuf::from("/b/1.png")), ("panel-2".to_string(), PathBuf::from("/b/2.png"))]
        );
        assert_eq!(located.pdfs, [("Document of Record".to_string(), PathBuf::from("/b/dor.pdf"))]);
        assert_eq!(
            located.problems,
            ["after-submit: the verifier kept no file", "no PDF was downloaded"]
        );
    }

    /// A verification with nothing to show is an error, saying why.
    #[test]
    fn a_report_with_nothing_to_show_is_an_error() {
        let located = Located {
            problems: vec!["the form did not open".into()],
            ..Located::default()
        };
        let err = read_located(located).unwrap_err();
        assert!(err.contains("the form did not open"), "{err}");
    }

    /// A located screenshot is kept as it is, and a PDF becomes its pages.
    #[test]
    fn located_files_become_review_images() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot.png");
        std::fs::write(&shot, PNG_MAGIC).unwrap();
        let pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../vendor/crates/u2s-render-pdf/fixtures/generated/unicode-text.pdf");
        let captured = read_located(Located {
            screenshots: vec![("form".into(), shot)],
            pdfs: vec![("Document of Record".into(), pdf)],
            problems: Vec::new(),
        })
        .unwrap();
        assert_eq!(captured.images.form, [ReviewImage { label: "form".into(), png: PNG_MAGIC.to_vec() }]);
        assert!(!captured.images.output.is_empty());
        assert_eq!(captured.images.output[0].label, "Document of Record, page 1");
        assert!(captured.images.output.iter().all(|i| i.png.starts_with(PNG_MAGIC)));
    }

    /// A PDF whose page draws nothing still becomes a review image, and is
    /// named as a problem: the verifier's own `download_blank` finding
    /// judges the content stream, this judges what the person would see.
    #[test]
    fn a_pdf_that_renders_blank_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("blank.pdf");
        std::fs::write(&pdf, u2s_verify_core::pdf_content::fixtures::blank_page()).unwrap();
        let captured = read_located(Located {
            screenshots: Vec::new(),
            pdfs: vec![("the form's submit download".into(), pdf)],
            problems: Vec::new(),
        })
        .unwrap();
        assert_eq!(captured.images.output.len(), 1);
        assert_eq!(captured.problems, ["the form's submit download, page 1 renders blank"]);
    }

    /// An XFA source renders through the XFA renderer, every page, as PNG;
    /// a file that is not a PDF is skipped.
    #[test]
    fn an_xfa_source_renders_every_page() {
        let (name, bytes) = form("AAEV_019_EN.pdf");
        let page_count = {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("f.pdf");
            std::fs::write(&path, &bytes).unwrap();
            crate::u2s::register_fonts().unwrap();
            let target = u2s_render_xfa::Target::doc(&path, &u2s_xfa::states::StateSpec::default());
            u2s_render_xfa::Renderer::new(limits()).info(&target).unwrap().page_count
        };
        let sources =
            render_sources(&[(name.clone(), bytes), ("template.zip".into(), b"PK".to_vec())]).unwrap();
        assert_eq!(sources.len(), 1);
        let images = &sources[0].pages;
        assert_eq!(images.len(), page_count as usize);
        assert_eq!(images[0].label, format!("{name}, page 1"));
        assert!(images.iter().all(|i| i.png.starts_with(PNG_MAGIC)));
    }

    /// Each XFA source is tagged with its own language, in the document's
    /// code: the Spanish PDF as `es`, as the review's selector pairs it with
    /// the output captured under the metadata's `sp`.
    #[test]
    fn each_source_is_tagged_with_its_language() {
        let files = [form("AAAA_019_DE.pdf"), form("AAAA_019_SP.pdf")];
        let sources = render_sources(&files).unwrap();
        let tagged: Vec<_> = sources.iter().map(|s| (s.file.as_str(), s.language.as_deref())).collect();
        assert_eq!(tagged, [("AAAA_019_DE.pdf", Some("de")), ("AAAA_019_SP.pdf", Some("es"))]);
        assert!(sources.iter().all(|s| !s.pages.is_empty()));
    }

    /// The verifier's codes become the document's languages, the first (the
    /// one the verifier opens by default) first; a language listed under
    /// two codes is captured once.
    #[test]
    fn verifier_languages_pair_with_the_documents() {
        let codes = ["de", "en", "sp", "es"].map(String::from);
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            review_languages(&codes).unwrap(),
            [pair("de", "de"), pair("en", "en"), pair("es", "sp")]
        );
        assert!(review_languages(&[]).is_err());
    }

    /// A PDF without XFA renders through pdfium.
    #[test]
    fn a_plain_pdf_source_renders_through_pdfium() {
        let pdf = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../vendor/crates/u2s-render-pdf/fixtures/generated/unicode-text.pdf"),
        )
        .unwrap();
        let sources = render_sources(&[("plain.pdf".into(), pdf)]).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].language, None, "a plain PDF declares no language");
        assert!(!sources[0].pages.is_empty());
        assert!(sources[0].pages.iter().all(|i| i.png.starts_with(PNG_MAGIC)));
    }
}
