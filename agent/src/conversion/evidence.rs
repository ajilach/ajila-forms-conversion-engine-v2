//! What a stage has verified, for the gate on its terminal call.
//!
//! The Author's `finish_authoring` and the Reviewer's approving `submit_review`
//! are refused until the stage itself has done the review procedure the
//! prompts describe: used the current build on its verifier, read the PDF
//! that produced, looked at the source pages, and (AEM) driven every source
//! control the form's scripts read. A rejecting review is never gated.
//!
//! The evidence is recorded from the tool calls and replies, and owned by the
//! [`ConversionAgent`](super::ConversionAgent): it is cleared when a stage
//! begins, and its verifier part whenever the build changes, because a PDF of
//! an earlier build says nothing about the current one.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use super::{OutputTarget, ReplyBlock, ToolReply};

/// The `pdf_*` tools that count as reading a PDF.
const PDF_READS: &[&str] = &[
    "pdf_render_page",
    "pdf_render_pages",
    "pdf_render_region",
    "pdf_page_text",
];
/// The `xfa_*` tools that count as looking at the source pages.
const SOURCE_RENDERS: &[&str] = &["xfa_render_page", "xfa_render_pages", "xfa_render_region"];

#[derive(Debug, Default)]
pub struct StageEvidence {
    /// Whether this stage opened the current build on the AEM verifier
    /// (`aem_verify_open`). A form opened before the build changed is the
    /// old build, so submitting it verifies nothing.
    opened: bool,
    /// Whether the target's verifier ran on the current build in this stage:
    /// `aem_verify_submit` on a form [`Self::opened`] here, or a
    /// `redacto_verify_run` that was not a dry run.
    verified: bool,
    /// The PDFs that run returned, by canonical `doc_path`.
    produced: BTreeSet<String>,
    /// The PDFs this stage read with a `pdf_*` tool, by canonical `doc_path`.
    read: HashSet<String>,
    /// Whether this stage rendered a source page.
    source_rendered: bool,
    /// Whether this stage listed the source's controls.
    controls_listed: bool,
    /// The source controls the form's scripts read, by dimension: a radio
    /// group's alternatives are one dimension, any other control its field.
    driving: BTreeSet<String>,
    /// Each listed control's dimension, by field.
    dimension_of: HashMap<String, String>,
    /// The fields this stage set with `xfa_set`, resolved to their dimension
    /// only when the gate is checked, since a field may be set before a
    /// listing says which radio group it belongs to.
    exercised: HashSet<String>,
}

impl StageEvidence {
    /// The build changed: what its verifier produced no longer describes it.
    pub fn build_changed(&mut self) {
        self.opened = false;
        self.verified = false;
        self.produced.clear();
        self.read.clear();
    }

    /// Records what a call's request alone shows: a PDF or a source page was
    /// read. Called on dispatch, for the reads that run beside the turn too.
    pub fn observe_call(&mut self, name: &str, input: &Value) {
        if PDF_READS.contains(&name) {
            if let Some(path) = input.get("doc_path").and_then(Value::as_str) {
                self.read.insert(canonical(path));
            }
        } else if SOURCE_RENDERS.contains(&name) {
            self.source_rendered = true;
        }
    }

    /// Records what a call's successful reply shows: the source's controls,
    /// a source control set, a verifier run and the PDFs it returned.
    pub fn observe_reply(&mut self, name: &str, input: &Value, reply: &ToolReply) {
        if matches!(reply, ToolReply::Error(_)) {
            return;
        }
        match name {
            "xfa_controls" => {
                let Some(report) = json_of(reply) else { return };
                self.controls_listed = true;
                for control in report["controls"].as_array().into_iter().flatten() {
                    let Some(field) = control["field"].as_str() else {
                        continue;
                    };
                    let dimension = control["group"].as_str().unwrap_or(field).to_string();
                    self.dimension_of
                        .insert(field.to_string(), dimension.clone());
                    // A hidden control is required once a listing shows it,
                    // which a listing after revealing its section does.
                    if control["affects_layout"] == true && control["visible"] == true {
                        self.driving.insert(dimension);
                    }
                }
            }
            "xfa_set" => {
                if let Some(field) = input["field"].as_str() {
                    self.exercised.insert(field.to_string());
                }
            }
            "aem_verify_open" => self.opened = true,
            "aem_verify_close" => self.opened = false,
            "aem_verify_submit" if !self.opened => {}
            "redacto_verify_run" if input["dry_run"] == true => {}
            "aem_verify_submit" | "redacto_verify_run" => {
                let Some(report) = json_of(reply) else { return };
                self.verified = true;
                for artefact in report["artefacts"].as_array().into_iter().flatten() {
                    let blob = &artefact["blob"];
                    if blob["media_type"] == "application/pdf"
                        && let Some(path) = blob["doc_path"].as_str()
                    {
                        self.produced.insert(canonical(path));
                    }
                }
            }
            _ => {}
        }
    }

    /// What the stage still has to do before its terminal call is accepted,
    /// each as an instruction; empty when nothing is missing. `built` is
    /// whether a current build exists.
    pub fn missing(&self, target: OutputTarget, built: bool) -> Vec<String> {
        let mut missing = Vec::new();
        if !built {
            missing.push(match target {
                OutputTarget::Aem => {
                    "there is no current build of the document: build_aem_package".to_string()
                }
                OutputTarget::Redacto => {
                    "there is no current build of the document: build_redacto_dump".to_string()
                }
            });
        }
        if !self.verified {
            missing.push(match target {
                OutputTarget::Aem => {
                    "open the current build on the AEM verifier (aem_verify_close any form opened \
                     before it, then aem_verify_open), walk it to the last page and submit it \
                     (aem_verify_submit)"
                        .to_string()
                }
                OutputTarget::Redacto => {
                    "import and render the current build (redacto_verify_run)".to_string()
                }
            });
        }
        if self.verified && self.produced.is_empty() {
            missing.push(match target {
                OutputTarget::Aem => "the submission returned no PDF to read: the form must produce its \
                                      Document of Record; find out why it did not"
                    .to_string(),
                OutputTarget::Redacto => "the run returned no rendered PDF to read: find out why the \
                                          render produced none"
                    .to_string(),
            });
        }
        let unread: Vec<&str> = self
            .produced
            .iter()
            .filter(|p| !self.read.contains(*p))
            .map(String::as_str)
            .collect();
        match target {
            // One read of the submission suffices: every PDF it returns is
            // the same document of record.
            OutputTarget::Aem
                if !self.produced.is_empty() && unread.len() == self.produced.len() =>
            {
                missing.push(format!(
                    "read the PDF the submission returned with pdf_render_pages (doc_path {})",
                    unread.join(", ")
                ));
            }
            // Redacto renders one PDF per language, and each is its own text.
            OutputTarget::Redacto if !unread.is_empty() => {
                missing.push(format!(
                    "read every rendered PDF with pdf_render_pages; not read yet: {}",
                    unread.join(", ")
                ));
            }
            _ => {}
        }
        if !self.source_rendered {
            missing.push(
                "render the source pages (xfa_render_pages) and compare them with the output"
                    .to_string(),
            );
        }
        if target == OutputTarget::Aem {
            if !self.controls_listed {
                missing.push("list the source's controls (xfa_open, xfa_controls)".to_string());
            }
            let exercised: HashSet<&str> = self
                .exercised
                .iter()
                .map(|field| self.dimension_of.get(field).unwrap_or(field).as_str())
                .collect();
            let unexercised: Vec<&str> = self
                .driving
                .iter()
                .filter(|d| !exercised.contains(d.as_str()))
                .map(String::as_str)
                .collect();
            if !unexercised.is_empty() {
                missing.push(format!(
                    "set each source control the form's scripts read (affects_layout) with xfa_set and \
                     compare its effect with the same choice on the AEM verifier; not set yet: {}",
                    unexercised.join(", ")
                ));
            }
        }
        missing
    }
}

/// A path as the evidence keys it, so the same file named two ways (macOS's
/// `/var` and `/private/var`) counts once.
fn canonical(path: &str) -> String {
    Path::new(path)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// The JSON object a u2s reply carries: its whole text, or the one line of it
/// that is the structured report (a verifier puts a summary line first).
fn json_of(reply: &ToolReply) -> Option<Value> {
    let texts: Vec<&str> = match reply {
        ToolReply::Text(text) => vec![text.as_str()],
        ToolReply::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ReplyBlock::Text(t) => Some(t.as_str()),
                ReplyBlock::Image { .. } => None,
            })
            .collect(),
        ToolReply::Error(_) => return None,
    };
    texts
        .iter()
        .flat_map(|text| std::iter::once(*text).chain(text.lines()))
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .find(Value::is_object)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text(value: Value) -> ToolReply {
        ToolReply::Text(value.to_string())
    }

    fn controls() -> ToolReply {
        text(json!({ "controls": [
            { "field": "form.p1.yes", "group": "form.p1.choice", "affects_layout": true, "visible": true },
            { "field": "form.p1.no", "group": "form.p1.choice", "affects_layout": true, "visible": true },
            { "field": "form.p1.extra", "affects_layout": true, "visible": false },
            { "field": "form.p1.plain", "affects_layout": false, "visible": true },
        ], "space_size": 8, "saturated": false }))
    }

    fn submitted(path: &str) -> ToolReply {
        ToolReply::Text(format!(
            "summary line\n{}",
            json!({ "artefacts": [{ "kind": "download", "label": "Document of Record",
                                    "blob": { "media_type": "application/pdf", "doc_path": path } }] })
        ))
    }

    /// Everything the AEM gate asks for, on `pdf`.
    fn complete_aem(pdf: &str) -> StageEvidence {
        let mut e = StageEvidence::default();
        e.observe_reply("xfa_controls", &json!({}), &controls());
        e.observe_reply(
            "xfa_set",
            &json!({ "field": "form.p1.no" }),
            &text(json!({})),
        );
        e.observe_call("xfa_render_pages", &json!({ "doc_path": "source.pdf" }));
        e.observe_reply("aem_verify_open", &json!({}), &text(json!({})));
        e.observe_reply("aem_verify_submit", &json!({}), &submitted(pdf));
        e.observe_call("pdf_render_pages", &json!({ "doc_path": pdf }));
        e
    }

    #[test]
    fn a_complete_aem_stage_misses_nothing() {
        assert_eq!(
            complete_aem("/blobs/a.pdf").missing(OutputTarget::Aem, true),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_empty_stage_misses_every_step() {
        let missing = StageEvidence::default().missing(OutputTarget::Aem, false);
        let all = missing.join("\n");
        for step in [
            "build_aem_package",
            "aem_verify_submit",
            "xfa_render_pages",
            "xfa_controls",
        ] {
            assert!(all.contains(step), "{step} missing from {all}");
        }
    }

    #[test]
    fn an_unread_submission_is_missing() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.read.clear();
        let missing = e.missing(OutputTarget::Aem, true);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("/blobs/a.pdf"), "{missing:?}");
    }

    #[test]
    fn a_source_control_left_unset_is_named() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.exercised.clear();
        let missing = e.missing(OutputTarget::Aem, true);
        assert_eq!(missing.len(), 1, "{missing:?}");
        // The radio group is one dimension; the hidden control and the one no
        // script reads are not required.
        assert!(
            missing[0].ends_with("not set yet: form.p1.choice"),
            "{missing:?}"
        );
    }

    #[test]
    fn a_control_revealed_later_is_required_once_listed() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.observe_reply(
            "xfa_controls",
            &json!({}),
            &text(json!({ "controls": [{ "field": "form.p1.extra", "affects_layout": true, "visible": true }] })),
        );
        assert!(e.missing(OutputTarget::Aem, true)[0].ends_with("form.p1.extra"));
        e.observe_reply(
            "xfa_set",
            &json!({ "field": "form.p1.extra" }),
            &text(json!({})),
        );
        assert!(e.missing(OutputTarget::Aem, true).is_empty());
    }

    #[test]
    fn a_failed_call_is_no_evidence() {
        let mut e = StageEvidence::default();
        e.observe_reply(
            "aem_verify_submit",
            &json!({}),
            &ToolReply::Error("not on the last panel".into()),
        );
        assert!(!e.verified);
    }

    #[test]
    fn a_new_build_voids_the_verifier_evidence_only() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.build_changed();
        let missing = e.missing(OutputTarget::Aem, true);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("aem_verify_submit"), "{missing:?}");
    }

    #[test]
    fn redacto_needs_every_rendered_language_read_and_no_controls() {
        let mut e = StageEvidence::default();
        e.observe_call("xfa_render_pages", &json!({ "doc_path": "source.pdf" }));
        e.observe_reply(
            "redacto_verify_run",
            &json!({}),
            &text(json!({ "artefacts": [
                { "label": "rendered (de)", "blob": { "media_type": "application/pdf", "doc_path": "/blobs/de.pdf" } },
                { "label": "rendered (fr)", "blob": { "media_type": "application/pdf", "doc_path": "/blobs/fr.pdf" } },
            ] })),
        );
        e.observe_call("pdf_render_pages", &json!({ "doc_path": "/blobs/de.pdf" }));
        let missing = e.missing(OutputTarget::Redacto, true);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].ends_with("/blobs/fr.pdf"), "{missing:?}");
        e.observe_call("pdf_page_text", &json!({ "doc_path": "/blobs/fr.pdf" }));
        assert!(e.missing(OutputTarget::Redacto, true).is_empty());
    }

    /// A form opened before the build changed is the old build: submitting
    /// it verifies nothing until the current build is opened.
    #[test]
    fn a_form_opened_before_the_build_changed_verifies_nothing() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.build_changed();
        e.observe_reply("aem_verify_submit", &json!({}), &submitted("/blobs/b.pdf"));
        e.observe_call("pdf_render_pages", &json!({ "doc_path": "/blobs/b.pdf" }));
        let missing = e.missing(OutputTarget::Aem, true);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("aem_verify_open"), "{missing:?}");
        e.observe_reply("aem_verify_open", &json!({}), &text(json!({})));
        e.observe_reply("aem_verify_submit", &json!({}), &submitted("/blobs/b.pdf"));
        assert!(e.missing(OutputTarget::Aem, true).is_empty());
    }

    /// A verification that returns no PDF leaves nothing to compare, so it
    /// does not pass the gate; a Redacto dry run renders nothing at all.
    #[test]
    fn a_verification_without_a_pdf_does_not_pass() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.build_changed();
        e.observe_reply("aem_verify_open", &json!({}), &text(json!({})));
        e.observe_reply("aem_verify_submit", &json!({}), &text(json!({ "artefacts": [] })));
        let missing = e.missing(OutputTarget::Aem, true);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("no PDF"), "{missing:?}");

        let mut e = StageEvidence::default();
        e.observe_call("xfa_render_pages", &json!({ "doc_path": "source.pdf" }));
        e.observe_reply("redacto_verify_run", &json!({ "dry_run": true }), &text(json!({ "artefacts": [] })));
        assert!(e.missing(OutputTarget::Redacto, true)[0].contains("redacto_verify_run"));
    }

    /// A radio set before the listing that names its group counts for the
    /// group once the listing is in.
    #[test]
    fn a_control_set_before_its_listing_counts_for_its_group() {
        let mut e = complete_aem("/blobs/a.pdf");
        e.exercised.clear();
        e.observe_reply("xfa_set", &json!({ "field": "form.p1.yes" }), &text(json!({})));
        e.observe_reply("xfa_controls", &json!({}), &controls());
        assert!(e.missing(OutputTarget::Aem, true).is_empty());
    }
}
