//! Opening a FileVault package and locating its form and DAM roots.
//!
//! `open_zip` is ported from `ajila-forms-conversion-engine/core/src/aem/parser.rs`
//! (`parse_aem_zip`'s own ZIP-to-map loop, `parser.rs:124-142`; see
//! `PORTING.md`). Root discovery is new: the reference locates the form
//! page by scanning candidate paths for `guideContainer`/`fd/af/components`
//! text (`find_form_content_xml`, `parser.rs:193-247`) and never derives a
//! nested folder path or a DAM root at all -- this workspace's own encoder
//! writes a *flat* `/content/forms/af/<form_name>` path
//! (`crate::package::Paths::new`), but the real fixture package's own
//! `filter.xml` roots at `/content/forms/af/afforms_germany_all/af_aa/AF_AABF`,
//! a nested path the reference's heuristic has no reason to need.
//!
//! So `filter.xml` is the authority here, with the reference's own
//! heuristic kept as the documented fallback for a package that ships one
//! that does not parse.
#![allow(
    dead_code,
    reason = "consumed once decode::form (Phase 4a) wires up the decode() entry point; \
              remove this line when it does"
)]

use std::collections::HashMap;
use std::io::Read;

use super::super::jcr::tree::{self, JcrNode};

/// The two JCR roots a FileVault content package declares for one form:
/// where its page lives, and where its DAM asset lives. `folder_path` is
/// empty for a flat path (this crate's own encoder never writes anything
/// else), and non-empty for a nested one like the fixture's
/// `afforms_germany_all/af_aa`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRoots {
    pub folder_path: Vec<String>,
    pub form_name: String,
    /// The ZIP entry holding the form page's `.content.xml`.
    pub form_content_xml_path: String,
    /// The ZIP entry holding the DAM asset's `.content.xml`, when the
    /// package declares one. `encode` always writes one, but nothing
    /// requires a source package to.
    pub dam_content_xml_path: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ZipOpenError {
    #[error("not a valid ZIP archive: {0}")]
    InvalidZip(#[from] zip::result::ZipError),
    #[error("could not read entry {name:?}: {source}")]
    Read {
        name: String,
        source: std::io::Error,
    },
}

/// Reads every non-directory entry of a ZIP archive into memory, keyed by
/// its full path. Ported (`parser.rs:124-142`).
pub fn open_zip(bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, ZipOpenError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut files = HashMap::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_owned();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|source| ZipOpenError::Read {
                name: name.clone(),
                source,
            })?;
        files.insert(name, contents);
    }
    Ok(files)
}

#[derive(Debug, thiserror::Error)]
pub enum RootError {
    #[error(
        "META-INF/vault/filter.xml declares {0} form roots (under /content/forms/af/); \
         exactly one is required"
    )]
    AmbiguousFormRoots(usize),
    #[error(
        "META-INF/vault/filter.xml's form root {form_root:?} and DAM root {dam_root:?} \
         name different forms"
    )]
    RootMismatch { form_root: String, dam_root: String },
    #[error("no jcr_root/content/forms/af/.../.content.xml entry matches the declared root")]
    MissingFormPage,
    #[error("could not parse META-INF/vault/filter.xml: {0}")]
    FilterXmlInvalid(#[from] tree::JcrXmlError),
    #[error(
        "no form root found: META-INF/vault/filter.xml is absent and no \
         jcr_root/content/forms/af/.../.content.xml entry looks like a form page"
    )]
    NoFormFound,
}

const FORMS_PREFIX: &str = "/content/forms/af/";
const DAM_PREFIX: &str = "/content/dam/formsanddocuments/";

/// Locates a package's form and DAM roots, `filter.xml` first.
pub fn locate_roots(zip_files: &HashMap<String, Vec<u8>>) -> Result<FormRoots, RootError> {
    match zip_files.get("META-INF/vault/filter.xml") {
        Some(bytes) => locate_via_filter_xml(bytes, zip_files),
        None => locate_via_heuristic(zip_files).ok_or(RootError::NoFormFound),
    }
}

fn locate_via_filter_xml(
    filter_xml: &[u8],
    zip_files: &HashMap<String, Vec<u8>>,
) -> Result<FormRoots, RootError> {
    let xml = String::from_utf8_lossy(filter_xml);
    let tree = tree::parse_jcr_xml(&xml)?;

    let mut form_roots = Vec::new();
    let mut dam_roots = Vec::new();
    for filter in tree.children.iter().filter(|c| c.tag_name == "filter") {
        let Some(root) = filter.attr("root") else {
            continue;
        };
        if let Some(rel) = root.strip_prefix(FORMS_PREFIX) {
            form_roots.push(rel.to_owned());
        } else if let Some(rel) = root.strip_prefix(DAM_PREFIX) {
            dam_roots.push(rel.to_owned());
        }
    }

    if form_roots.len() != 1 {
        return Err(RootError::AmbiguousFormRoots(form_roots.len()));
    }
    let form_rel = &form_roots[0];
    if let Some(dam_rel) = dam_roots.first()
        && dam_rel != form_rel
    {
        return Err(RootError::RootMismatch {
            form_root: form_rel.clone(),
            dam_root: dam_rel.clone(),
        });
    }

    let (folder_path, form_name) = split_relative_path(form_rel);
    let form_content_xml_path = format!("jcr_root{FORMS_PREFIX}{form_rel}/.content.xml");
    if !zip_files.contains_key(&form_content_xml_path) {
        return Err(RootError::MissingFormPage);
    }
    let dam_content_xml_path = dam_roots.first().map(|dam_rel| {
        format!("jcr_root{DAM_PREFIX}{dam_rel}/.content.xml")
    });

    Ok(FormRoots {
        folder_path,
        form_name,
        form_content_xml_path,
        dam_content_xml_path,
    })
}

/// Splits `"afforms_germany_all/af_aa/AF_AABF"` into
/// (`["afforms_germany_all", "af_aa"]`, `"AF_AABF"`) -- the last segment is
/// the form name, everything before it the intermediate folder path. A
/// bare `"AF_AABF"` (this crate's own flat encoder output) splits into
/// (`[]`, `"AF_AABF"`).
fn split_relative_path(rel: &str) -> (Vec<String>, String) {
    let mut segments: Vec<String> = rel.split('/').map(str::to_owned).collect();
    let form_name = segments.pop().unwrap_or_default();
    (segments, form_name)
}

/// The reference's own heuristic (`parser.rs:193-247`), kept only for a
/// package whose `filter.xml` is absent or fails to parse. Ported, minus
/// the `.content-finished.xml` fallback tier (nothing in this workspace's
/// own corpus, real or encoded, produces that file, and it is undocumented
/// in `specs/AEM.md`), and it does not attempt DAM discovery at all -- a
/// package reached only through this path decodes its form page but treats
/// its DAM asset as absent, which is a real information loss `decode`'s own
/// contract (`crates/u2s-mcp/src/manifest.rs`'s `ToolRole` doc) requires be
/// reported, not silently accepted.
fn locate_via_heuristic(zip_files: &HashMap<String, Vec<u8>>) -> Option<FormRoots> {
    let mut candidates: Vec<&String> = zip_files
        .keys()
        .filter(|path| {
            path.contains("jcr_root/content/forms/af/")
                && path.ends_with("/.content.xml")
                && !path.contains("/_jcr_content/")
                && !path.contains("/assets/")
                && !path.contains("/dictionary/")
        })
        .collect();
    candidates.sort_by(|a, b| {
        let depth_a = a.matches('/').count();
        let depth_b = b.matches('/').count();
        depth_a.cmp(&depth_b).then_with(|| a.cmp(b))
    });

    for path in candidates {
        let Some(content) = zip_files.get(path) else {
            continue;
        };
        let xml_str = String::from_utf8_lossy(content);
        if !xml_str.contains("guideContainer") && !xml_str.contains("fd/af/components") {
            continue;
        }
        let rel = path
            .strip_prefix("jcr_root/content/forms/af/")
            .and_then(|s| s.strip_suffix("/.content.xml"))?;
        let (folder_path, form_name) = split_relative_path(rel);
        return Some(FormRoots {
            folder_path,
            form_name,
            form_content_xml_path: path.clone(),
            dam_content_xml_path: None,
        });
    }
    None
}

/// Reads and parses one ZIP entry as JCR XML, by path.
pub fn parse_entry(
    zip_files: &HashMap<String, Vec<u8>>,
    path: &str,
) -> Result<JcrNode, EntryError> {
    let bytes = zip_files
        .get(path)
        .ok_or_else(|| EntryError::Missing(path.to_owned()))?;
    let xml = String::from_utf8_lossy(bytes);
    tree::parse_jcr_xml(&xml).map_err(|source| EntryError::Invalid {
        path: path.to_owned(),
        source,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum EntryError {
    #[error("no ZIP entry at {0:?}")]
    Missing(String),
    #[error("{path:?} is not valid JCR XML: {source}")]
    Invalid {
        path: String,
        source: tree::JcrXmlError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Builds a minimal ZIP in memory: `filter.xml` (if given) plus one
    /// `.content.xml` entry at `form_path`, so root discovery can be tested
    /// without a real package.
    fn a_zip(filter_xml: Option<&str>, form_path: &str, form_xml: &str) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if let Some(filter_xml) = filter_xml {
            zip.start_file("META-INF/vault/filter.xml", options).unwrap();
            zip.write_all(filter_xml.as_bytes()).unwrap();
        }
        zip.start_file(form_path, options).unwrap();
        zip.write_all(form_xml.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    const A_FORM_PAGE: &str = r#"<jcr:root sling:resourceType="fd/af/components/guideContainer"/>"#;

    #[test]
    fn open_zip_reads_every_file_and_skips_directories() {
        let bytes = a_zip(None, "jcr_root/content/forms/af/X/.content.xml", A_FORM_PAGE);
        let files = open_zip(&bytes).unwrap();
        assert_eq!(
            files.get("jcr_root/content/forms/af/X/.content.xml"),
            Some(&A_FORM_PAGE.as_bytes().to_vec())
        );
    }

    #[test]
    fn locate_roots_via_filter_xml_splits_a_nested_path() {
        let filter = r#"<workspaceFilter version="1.0">
            <filter root="/content/forms/af/afforms_germany_all/af_aa/AF_AABF"/>
            <filter root="/content/dam/formsanddocuments/afforms_germany_all/af_aa/AF_AABF"/>
        </workspaceFilter>"#;
        let bytes = a_zip(
            Some(filter),
            "jcr_root/content/forms/af/afforms_germany_all/af_aa/AF_AABF/.content.xml",
            A_FORM_PAGE,
        );
        let files = open_zip(&bytes).unwrap();
        let roots = locate_roots(&files).unwrap();
        assert_eq!(
            roots.folder_path,
            vec!["afforms_germany_all".to_owned(), "af_aa".to_owned()]
        );
        assert_eq!(roots.form_name, "AF_AABF");
        assert!(roots.dam_content_xml_path.is_some());
    }

    #[test]
    fn locate_roots_via_filter_xml_handles_a_flat_path() {
        let filter = r#"<workspaceFilter version="1.0">
            <filter root="/content/forms/af/TestForm"/>
            <filter root="/content/dam/formsanddocuments/TestForm"/>
        </workspaceFilter>"#;
        let bytes = a_zip(
            Some(filter),
            "jcr_root/content/forms/af/TestForm/.content.xml",
            A_FORM_PAGE,
        );
        let files = open_zip(&bytes).unwrap();
        let roots = locate_roots(&files).unwrap();
        assert!(roots.folder_path.is_empty());
        assert_eq!(roots.form_name, "TestForm");
    }

    #[test]
    fn two_form_roots_are_ambiguous() {
        let filter = r#"<workspaceFilter version="1.0">
            <filter root="/content/forms/af/A"/>
            <filter root="/content/forms/af/B"/>
        </workspaceFilter>"#;
        let bytes = a_zip(Some(filter), "jcr_root/content/forms/af/A/.content.xml", A_FORM_PAGE);
        let files = open_zip(&bytes).unwrap();
        assert!(matches!(
            locate_roots(&files),
            Err(RootError::AmbiguousFormRoots(2))
        ));
    }

    #[test]
    fn a_form_root_and_dam_root_naming_different_forms_is_a_mismatch() {
        let filter = r#"<workspaceFilter version="1.0">
            <filter root="/content/forms/af/A"/>
            <filter root="/content/dam/formsanddocuments/B"/>
        </workspaceFilter>"#;
        let bytes = a_zip(Some(filter), "jcr_root/content/forms/af/A/.content.xml", A_FORM_PAGE);
        let files = open_zip(&bytes).unwrap();
        assert!(matches!(
            locate_roots(&files),
            Err(RootError::RootMismatch { .. })
        ));
    }

    #[test]
    fn a_declared_root_with_no_matching_content_xml_is_missing_form_page() {
        let filter = r#"<workspaceFilter version="1.0">
            <filter root="/content/forms/af/A"/>
        </workspaceFilter>"#;
        // The content.xml is at the wrong path -- filter.xml points elsewhere.
        let bytes = a_zip(Some(filter), "jcr_root/content/forms/af/WRONG/.content.xml", A_FORM_PAGE);
        let files = open_zip(&bytes).unwrap();
        assert!(matches!(
            locate_roots(&files),
            Err(RootError::MissingFormPage)
        ));
    }

    #[test]
    fn falls_back_to_the_heuristic_when_filter_xml_is_absent() {
        let bytes = a_zip(None, "jcr_root/content/forms/af/Nested/Deep/.content.xml", A_FORM_PAGE);
        let files = open_zip(&bytes).unwrap();
        let roots = locate_roots(&files).unwrap();
        assert_eq!(roots.folder_path, vec!["Nested".to_owned()]);
        assert_eq!(roots.form_name, "Deep");
        assert!(roots.dam_content_xml_path.is_none());
    }

    #[test]
    fn no_form_page_anywhere_is_a_named_error() {
        let bytes = a_zip(None, "jcr_root/content/dam/something/.content.xml", "<root/>");
        let files = open_zip(&bytes).unwrap();
        assert!(matches!(locate_roots(&files), Err(RootError::NoFormFound)));
    }

    #[test]
    fn locates_roots_in_the_real_fixture_package() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/AF_AABF.zip"),
        )
        .expect("the committed fixture");
        let files = open_zip(&bytes).unwrap();
        let roots = locate_roots(&files).unwrap();
        assert_eq!(
            roots.folder_path,
            vec!["afforms_germany_all".to_owned(), "af_aa".to_owned()]
        );
        assert_eq!(roots.form_name, "AF_AABF");
        assert!(roots.dam_content_xml_path.is_some());

        let form_xml = parse_entry(&files, &roots.form_content_xml_path).unwrap();
        assert_eq!(form_xml.tag_name, "jcr:root");
    }
}
