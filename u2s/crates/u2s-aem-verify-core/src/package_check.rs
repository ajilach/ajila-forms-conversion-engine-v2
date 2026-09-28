//! Offline validation of a FileVault package: is it a well-formed ZIP, does
//! `filter.xml` resolve to exactly one form, what JCR path does it render
//! at. Nothing here touches Docker, AEM or a network -- this is what lets
//! `verify_package_check` and a `dry_run` `verify_run` call answer without
//! the side effect the rest of the flow performs for real.
//!
//! Deliberately reuses `u2s_mapper_aem::decode::zip` rather than parsing
//! `filter.xml` a second time: that module is the FileVault-root authority
//! the decoder itself uses, so a form's JCR path is derived once, in one
//! place, for every consumer.
//!
//! [`inspect`] additionally hands back the form and DAM pages' own raw
//! `.content.xml` bytes, alongside [`PackageSummary`] -- format-specific
//! metadata a `crate::driver::FormDriver` needs (a UBS form's authored
//! mandator/language entities, say) lives in the binary that owns that
//! vocabulary, not here, so this module reads the XML once and hands the
//! bytes up rather than growing a field per format.

use std::collections::HashMap;

use serde::Serialize;
use u2s_mapper_aem::decode::zip::{self, FormRoots, RootError, ZipOpenError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageSummary {
    /// Where the form renders, e.g.
    /// `/content/forms/af/afforms_germany_all/af_aa/AF_AABF`.
    pub form_jcr_path: String,
    pub form_name: String,
    /// A DAM asset entry (`.../renditions/dorTemplate/...`) was found among
    /// the package's own entries. A heuristic, not a decode of the form's
    /// `dor` metadata field (which this offline check never has -- that
    /// lives in the *decoded* JSON, not the ZIP's own file names), but
    /// enough to tell a caller "this package looks like it carries a DoR
    /// template" before ever installing it.
    pub looks_like_it_has_dor: bool,
    /// The form page's own `.content.xml` contains a wizard-shaped layout
    /// marker (`mobileLayout="fd/af/layouts/mobile/step"`, or a `layout`
    /// child resourceType ending `/layouts/panel/wizard`). Informational
    /// only -- `flow.rs`'s panel-walking loop never branches on this, it
    /// always tries to advance and stops the moment no "next" control is
    /// visible, so a single-panel form behaves correctly without it. This
    /// exists purely to surface "this package looks like a wizard" in a
    /// summary or `dry_run` before ever installing it. The marker itself
    /// is empirically observed from one customer's forms (see
    /// `aaov-output/README.md`), not documented in `specs/AEM.md` as a
    /// generic-AEM contract -- treat a `false` here as "no signal", never
    /// as proof the form has one panel.
    pub looks_like_a_wizard: bool,
}

const WIZARD_LAYOUT_MARKERS: [&str; 2] = [
    "mobileLayout=\"fd/af/layouts/mobile/step\"",
    "/layouts/panel/wizard",
];

#[derive(Debug, thiserror::Error)]
pub enum PackageCheckError {
    #[error(transparent)]
    Zip(#[from] ZipOpenError),
    #[error(transparent)]
    Roots(#[from] RootError),
}

/// [`PackageSummary`] plus the raw bytes of the form and (when the package
/// declares one) DAM `.content.xml` pages -- what a format-specific
/// `FormDriver` (in the binary above this crate) parses for its own
/// vocabulary (a UBS form's metadata component, say) without this crate
/// ever needing to name that vocabulary itself.
pub struct PackageInspection {
    pub summary: PackageSummary,
    pub form_content_xml: Vec<u8>,
    pub dam_content_xml: Option<Vec<u8>>,
}

pub fn inspect(bytes: &[u8]) -> Result<PackageInspection, PackageCheckError> {
    let files = zip::open_zip(bytes)?;
    let roots = zip::locate_roots(&files)?;
    let summary = summarize(&files, &roots);
    let form_content_xml = files
        .get(&roots.form_content_xml_path)
        .cloned()
        .unwrap_or_default();
    let dam_content_xml = roots
        .dam_content_xml_path
        .as_ref()
        .and_then(|path| files.get(path))
        .cloned();

    Ok(PackageInspection {
        summary,
        form_content_xml,
        dam_content_xml,
    })
}

pub fn check(bytes: &[u8]) -> Result<PackageSummary, PackageCheckError> {
    Ok(inspect(bytes)?.summary)
}

fn summarize(files: &HashMap<String, Vec<u8>>, roots: &FormRoots) -> PackageSummary {
    let form_jcr_path = form_jcr_path(roots);
    let looks_like_it_has_dor = files.keys().any(|name| name.contains("dorTemplate"));
    let looks_like_a_wizard = files
        .get(&roots.form_content_xml_path)
        .is_some_and(|bytes| {
            let text = String::from_utf8_lossy(bytes);
            WIZARD_LAYOUT_MARKERS
                .iter()
                .any(|marker| text.contains(marker))
        });

    PackageSummary {
        form_jcr_path,
        form_name: roots.form_name.clone(),
        looks_like_it_has_dor,
        looks_like_a_wizard,
    }
}

/// The JCR path a form's `FormRoots` renders at -- `flow.rs` reuses this to
/// build the `.html` URL it navigates to, so the path is composed in
/// exactly one place regardless of which caller needs it.
pub fn form_jcr_path(roots: &FormRoots) -> String {
    let mut segments: Vec<&str> = roots.folder_path.iter().map(String::as_str).collect();
    segments.push(&roots.form_name);
    format!("/content/forms/af/{}", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a minimal FileVault ZIP with a flat form root -- just enough
    /// for `locate_roots`' `filter.xml` path to succeed, without pulling
    /// in a real fixture file. `with_wizard` writes the empirically
    /// observed wizard layout marker into the form page's own
    /// `.content.xml`, exactly where `check()` looks for it.
    fn minimal_package(form_name: &str, with_dor: bool, with_wizard: bool) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = ::zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options: ::zip::write::FileOptions<'_, ()> = ::zip::write::FileOptions::default();

            zip.start_file("META-INF/vault/filter.xml", options)
                .unwrap();
            zip.write_all(
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<workspaceFilter version="1.0">
    <filter root="/content/forms/af/{form_name}"/>
</workspaceFilter>"#
                )
                .as_bytes(),
            )
            .unwrap();

            zip.start_file(
                format!("jcr_root/content/forms/af/{form_name}/.content.xml"),
                options,
            )
            .unwrap();
            let content_xml = if with_wizard {
                r#"<jcr:root><layout mobileLayout="fd/af/layouts/mobile/step"/></jcr:root>"#
            } else {
                "<jcr:root/>"
            };
            zip.write_all(content_xml.as_bytes()).unwrap();

            if with_dor {
                zip.start_file(
                    format!(
                        "jcr_root/content/dam/formsanddocuments/{form_name}/_jcr_content/renditions/dorTemplate/en"
                    ),
                    options,
                )
                .unwrap();
                zip.write_all(b"xdp bytes").unwrap();
            }

            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn a_flat_form_root_resolves_its_jcr_path() {
        let bytes = minimal_package("ConformanceForm", false, false);
        let summary = check(&bytes).expect("a well-formed package must check out");
        assert_eq!(summary.form_jcr_path, "/content/forms/af/ConformanceForm");
        assert_eq!(summary.form_name, "ConformanceForm");
        assert!(!summary.looks_like_it_has_dor);
        assert!(!summary.looks_like_a_wizard);
    }

    #[test]
    fn a_dor_rendition_entry_is_detected() {
        let bytes = minimal_package("ConformanceForm", true, false);
        let summary = check(&bytes).expect("checks out");
        assert!(summary.looks_like_it_has_dor);
    }

    #[test]
    fn a_wizard_layout_signature_is_detected() {
        let bytes = minimal_package("ConformanceForm", false, true);
        let summary = check(&bytes).expect("checks out");
        assert!(summary.looks_like_a_wizard);
    }

    #[test]
    fn something_that_is_not_a_zip_is_a_zip_error() {
        let err = check(b"not a zip file at all").expect_err("must not parse");
        assert!(matches!(err, PackageCheckError::Zip(_)));
    }

    #[test]
    fn a_zip_with_no_filter_xml_and_no_recognisable_form_page_is_a_roots_error() {
        let mut buf = Vec::new();
        {
            let mut zip = ::zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options: ::zip::write::FileOptions<'_, ()> = ::zip::write::FileOptions::default();
            zip.start_file("README.txt", options).unwrap();
            zip.write_all(b"not a form package").unwrap();
            zip.finish().unwrap();
        }
        let err = check(&buf).expect_err("must not resolve a form root");
        assert!(matches!(err, PackageCheckError::Roots(_)));
    }

    #[test]
    fn inspect_exposes_the_forms_own_raw_content_xml() {
        let bytes = minimal_package("ConformanceForm", false, true);
        let inspection = inspect(&bytes).expect("checks out");
        assert!(
            String::from_utf8_lossy(&inspection.form_content_xml)
                .contains("fd/af/layouts/mobile/step"),
            "inspect must hand back the form page's own raw .content.xml bytes"
        );
        assert_eq!(
            inspection.dam_content_xml, None,
            "minimal_package declares no DAM .content.xml"
        );
    }
}
