//! The images a person reviews a finished run by: its source rendered page by
//! page, next to what the run's output looks like on its verifier.
//!
//! The output side only exists while the verifier runs, so [`capture`] takes it
//! at the end of a run, from one last verification of the build that ships,
//! before the controller tears the verifier down. The source side can be
//! rendered at any time from the source bytes ([`render_sources`]).
//!
//! It also holds what the Reviewer hands a person rather than the Author: a
//! [`RuleConflict`], a place where two rules ask for opposite things, which
//! no fix round can settle.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use u2s_render_core::{ImageFormat, Limits, RenderError, RenderedPage, RenderedPages};

use crate::{ConversionAgent, OutputTarget, ReviewResult, ToolReply};

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

/// What a run's output looks like.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewImages {
    /// Screenshots of the rendered form, in the order the verifier took them
    /// (AEM only: Redacto renders no form).
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
/// failed submit still leaves its screenshots worth reviewing.
#[derive(Debug)]
pub struct Captured {
    pub images: ReviewImages,
    pub problems: Vec<String>,
}

/// Verifies the agent's current build once more and keeps what that produced
/// as images. Records nothing as stage evidence: no stage is running.
///
/// An error when the verification itself failed or produced nothing to show.
pub async fn capture(agent: &mut ConversionAgent) -> Result<Captured, String> {
    let tool = match agent.target() {
        OutputTarget::Aem => "aem_verify_run",
        OutputTarget::Redacto => "redacto_verify_run",
    };
    let report = match agent.execute_as(tool, &json!({}), &crate::Caller::Host).await {
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
fn read_located(mut located: Located) -> Result<Captured, String> {
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
    Ok(Captured {
        images,
        problems: located.problems,
    })
}

/// Renders every page of the source PDFs, in order, each labelled with its
/// file. Files that are not PDFs (an attached template package) are skipped.
/// XFA forms go through the XFA renderer, which draws them in their initial
/// state; pdfium only shows such a form's "please wait" placeholder.
pub fn render_sources(files: &[(String, Vec<u8>)]) -> Result<Vec<ReviewImage>, String> {
    crate::u2s::register_fonts()?;
    let dir = tempfile::Builder::new()
        .prefix("blueprint-review-")
        .tempdir()
        .map_err(|e| format!("could not create a directory to render the sources in: {e}"))?;
    let mut images = Vec::new();
    for (index, (name, bytes)) in files.iter().enumerate() {
        if !crate::conversion::is_source_pdf(name) {
            continue;
        }
        let path = dir.path().join(format!("{index}.pdf"));
        std::fs::write(&path, bytes).map_err(|e| format!("writing {name}: {e}"))?;
        let is_xfa = u2s_xfa::extract_xfa_from_pdf_bytes(bytes)
            .map_err(|e| format!("reading {name}: {e}"))?
            .is_some();
        images.extend(if is_xfa {
            let xfa = u2s_render_xfa::Renderer::new(limits());
            let target = u2s_render_xfa::Target::doc(&path, &u2s_xfa::states::StateSpec::default());
            labelled(
                name,
                walk(|from| xfa.render_pages(&target, None, Some(from), None, Some(REVIEW_DPI), None, ImageFormat::Png))
                    .map_err(|e| format!("rendering {name}: {e}"))?,
            )
        } else {
            render_pdf(&path, name)?
        });
    }
    Ok(images)
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

/// A place where rules contradict each other: whatever the Author does
/// there, one of them breaks. Only a person (whoever owns the rules) can
/// settle it, so the Reviewer reports it with `submit_review`'s
/// `rule_conflicts` instead of sending it back as an issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleConflict {
    /// The ids of the rules involved, at least two.
    pub rules: Vec<String>,
    /// The JSON Pointer of the node they disagree about.
    pub path: String,
    /// What each rule asks for there.
    pub why: String,
}

impl std::fmt::Display for RuleConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}: {}", self.rules.join(" vs "), self.path, self.why)
    }
}

/// The `rule_conflicts` of a `submit_review` call: none when the argument is
/// absent or null, an error naming the bad entry when one is malformed, so the
/// Reviewer can correct the call rather than have a conflict silently dropped.
pub fn rule_conflicts_of(input: &Value) -> Result<Vec<RuleConflict>, String> {
    let entries = match &input["rule_conflicts"] {
        Value::Null => return Ok(Vec::new()),
        Value::Array(entries) => entries,
        _ => return Err("rule_conflicts must be an array of {rules, path, why}".into()),
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let text = |field: &str| entry[field].as_str().map(str::trim).filter(|t| !t.is_empty());
            let rules: Vec<String> = entry["rules"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|rule| rule.as_str().map(str::trim).filter(|r| !r.is_empty()))
                .map(String::from)
                .collect();
            match (rules.len() >= 2, text("path"), text("why")) {
                (true, Some(path), Some(why)) => Ok(RuleConflict {
                    rules,
                    path: path.to_string(),
                    why: why.to_string(),
                }),
                _ => Err(format!(
                    "rule_conflicts[{index}] needs `rules` (the ids of at least two rules), `path` (the \
                     node's JSON Pointer) and `why` (what each rule asks for there)"
                )),
            }
        })
        .collect()
}

/// A bullet per conflict.
pub fn conflict_list(conflicts: &[RuleConflict]) -> String {
    conflicts.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n")
}

/// The heading [`crate::REVIEWER_ADDENDUM`] has engine defects reported under.
const ENGINE_DEFECTS: &str = "ENGINE DEFECTS";

impl ReviewResult {
    /// Whether the report asks the Author for anything. A blank report asks
    /// for nothing, and so does one that only lists engine defects: those are
    /// for the people who maintain the engine, not for a fix round.
    pub fn has_authorable_issues(&self) -> bool {
        let report = self.report.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '#' | '*' | '_' | '-'));
        !report.is_empty() && !report.to_ascii_uppercase().starts_with(ENGINE_DEFECTS)
    }

    /// Whether only rule conflicts stand between the form and approval: a fix
    /// round could not change that, so the run stops for a person instead.
    pub fn needs_operator(&self) -> bool {
        !self.approved && !self.rule_conflicts.is_empty() && !self.has_authorable_issues()
    }

    /// The verdict as the run's trace records it: the report, then the
    /// conflicts for a person.
    pub fn trace_report(&self) -> String {
        if self.rule_conflicts.is_empty() {
            return self.report.clone();
        }
        let conflicts = format!("RULE CONFLICTS (for a person):\n{}", conflict_list(&self.rule_conflicts));
        if self.report.trim().is_empty() {
            conflicts
        } else {
            format!("{}\n\n{conflicts}", self.report.trim_end())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn review(approved: bool, report: &str, conflicts: usize) -> ReviewResult {
        ReviewResult {
            approved,
            report: report.into(),
            rule_conflicts: (0..conflicts)
                .map(|i| RuleConflict {
                    rules: vec![format!("rule-{i}"), "other".into()],
                    path: format!("/form/children/{i}"),
                    why: "they disagree".into(),
                })
                .collect(),
        }
    }

    /// `rule_conflicts` is optional, and each entry needs two rules, a path
    /// and a reason; a malformed one is an error naming it.
    #[test]
    fn rule_conflicts_are_read_from_submit_review() {
        assert_eq!(rule_conflicts_of(&json!({ "approved": false })), Ok(Vec::new()));
        assert_eq!(rule_conflicts_of(&json!({ "rule_conflicts": null })), Ok(Vec::new()));
        let read = rule_conflicts_of(&json!({ "rule_conflicts": [
            { "rules": ["sub-headings-are-title-draws", " 2943a027 "], "path": "/form/children/5/children/3", "why": "one asks for a title draw, the other forbids it" },
        ] }))
        .unwrap();
        assert_eq!(
            read,
            [RuleConflict {
                rules: vec!["sub-headings-are-title-draws".into(), "2943a027".into()],
                path: "/form/children/5/children/3".into(),
                why: "one asks for a title draw, the other forbids it".into(),
            }]
        );
        assert_eq!(
            read[0].to_string(),
            "sub-headings-are-title-draws vs 2943a027 at /form/children/5/children/3: one asks for a title draw, the other forbids it"
        );
        let one_rule = json!({ "rule_conflicts": [
            { "rules": ["a", "b"], "path": "/x", "why": "w" },
            { "rules": ["a"], "path": "/x", "why": "w" },
        ] });
        assert!(rule_conflicts_of(&one_rule).unwrap_err().contains("rule_conflicts[1]"));
        assert!(rule_conflicts_of(&json!({ "rule_conflicts": [{ "rules": ["a", "b"], "path": " ", "why": "w" }] })).is_err());
        assert!(rule_conflicts_of(&json!({ "rule_conflicts": "a vs b" })).is_err());
    }

    /// Only conflicts, with nothing for the Author, need a person; a report
    /// listing only engine defects asks the Author for nothing either.
    #[test]
    fn a_review_needs_an_operator_only_when_conflicts_are_all_that_is_left() {
        assert!(review(false, "", 1).needs_operator());
        assert!(review(false, "  \n", 2).needs_operator());
        assert!(review(false, "## ENGINE DEFECTS\n- /form: fixed writer output", 1).needs_operator());
        assert!(review(false, "**Engine defects**: none the author can fix", 1).needs_operator());
        assert!(!review(false, "1. /form/children/2: label missing", 1).needs_operator(), "a mix goes back to the author");
        assert!(!review(false, "", 0).needs_operator(), "no conflicts: an ordinary rejection");
        assert!(!review(true, "", 1).needs_operator(), "an approval is never held for a person");
    }

    /// The trace keeps the report and adds the conflicts after it.
    #[test]
    fn the_traced_report_carries_the_conflicts() {
        assert_eq!(review(false, "fix x", 0).trace_report(), "fix x");
        assert_eq!(
            review(false, "", 1).trace_report(),
            "RULE CONFLICTS (for a person):\n- rule-0 vs other at /form/children/0: they disagree"
        );
        let mixed = review(false, "fix x\n", 1).trace_report();
        assert!(mixed.starts_with("fix x\n\nRULE CONFLICTS"), "{mixed}");
    }

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

    /// A Redacto report has no steps, one rendered PDF per language.
    #[test]
    fn a_redacto_report_locates_one_pdf_per_language() {
        let report = json!({
            "steps": [],
            "artefacts": [
                { "kind": "download", "label": "rendered (de)", "blob": { "media_type": "application/pdf", "doc_path": "/b/de.pdf" } },
                { "kind": "download", "label": "rendered (en)", "blob": { "media_type": "application/pdf", "doc_path": "/b/en.pdf" } },
            ],
            "findings": [],
        });
        let located = locate(&report);
        assert!(located.screenshots.is_empty());
        assert_eq!(located.pdfs.len(), 2);
        assert!(located.problems.is_empty());
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
        let images =
            render_sources(&[(name.clone(), bytes), ("template.zip".into(), b"PK".to_vec())]).unwrap();
        assert_eq!(images.len(), page_count as usize);
        assert_eq!(images[0].label, format!("{name}, page 1"));
        assert!(images.iter().all(|i| i.png.starts_with(PNG_MAGIC)));
    }

    /// A PDF without XFA renders through pdfium.
    #[test]
    fn a_plain_pdf_source_renders_through_pdfium() {
        let pdf = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../vendor/crates/u2s-render-pdf/fixtures/generated/unicode-text.pdf"),
        )
        .unwrap();
        let images = render_sources(&[("plain.pdf".into(), pdf)]).unwrap();
        assert!(!images.is_empty());
        assert!(images.iter().all(|i| i.png.starts_with(PNG_MAGIC)));
    }
}
