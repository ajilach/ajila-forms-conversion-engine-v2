//! The fragment-library query tool's logic: a mechanical, deployment-wide
//! catalogue browsable from every AEM output format, not a per-profile
//! concern.
//!
//! **This module is not an encoder.** Nothing here decides which fragment
//! a form should use -- that is the Conversion Agent's job, exactly like
//! every other judgment call this crate refuses to make in Rust (see the
//! crate's own module doc). What this module does is mechanical: scan a
//! directory of AEM fragment `.content.xml` files once, extract each
//! fragment's JCR path, title and a short preview, and answer a plain text
//! search over that -- the same "mechanical service, LLM judgment" shape
//! `mcp-xfa-data` already uses on the input side of this workspace.
//!
//! The scan reads each fragment's `.content.xml` as a stream of XML
//! attribute values looking for `jcr:title`/`sling:message`, not as a full
//! `AemNode` parse -- a fragment is opaque reference data here, never
//! something this crate converts.

use std::io::BufRead;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::search;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentEntry {
    /// The JCR path to write as a `frag_ref` -- always
    /// `/content/dam/formsanddocuments/...`, matching
    /// [`u2s_aem::model::FragmentRef`]'s own pattern, since it is derived
    /// from the same directory layout that path names.
    pub frag_ref: String,
    pub title: String,
    /// The first non-empty line of body text found, if any -- enough for
    /// an agent to judge fit without shipping the whole fragment.
    pub preview: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FragmentLibraryError {
    #[error("cannot read fragment library directory {path}: {source}")]
    ReadDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub struct FragmentLibrary {
    entries: Vec<FragmentEntry>,
}

impl FragmentLibrary {
    /// An empty library -- what a dataset with no fragment corpus yet
    /// looks like. Not an error: [`search`](Self::search) on it simply
    /// returns no hits, and that is the correct, legitimate answer for a
    /// fresh deployment -- see [`scan`](Self::scan)'s own doc on
    /// `U2S_AEM_FRAGMENT_DIR` being unset.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Scans every `.content.xml` under `dir`, recursively. A directory
    /// that does not exist or cannot be read is a hard error -- distinct
    /// from [`empty`](Self::empty), which is what an unset configuration
    /// variable maps to. A configured-but-broken path is a misconfiguration,
    /// the same "refuse rather than silently degrade" discipline this
    /// workspace already applies to `U2S_FONT_DIR`.
    pub fn scan(dir: &Path) -> Result<Self, FragmentLibraryError> {
        let mut entries = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            let read =
                std::fs::read_dir(&current).map_err(|source| FragmentLibraryError::ReadDir {
                    path: current.display().to_string(),
                    source,
                })?;
            for item in read {
                let item = item.map_err(|source| FragmentLibraryError::ReadDir {
                    path: current.display().to_string(),
                    source,
                })?;
                let path = item.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.file_name().and_then(|n| n.to_str()) == Some(".content.xml")
                    && let Some(entry) = scan_one(dir, &path)
                {
                    entries.push(entry);
                }
            }
        }
        entries.sort_by(|a, b| a.frag_ref.cmp(&b.frag_ref));
        Ok(Self { entries })
    }

    /// Case-insensitive substring match over title and preview -- a
    /// mechanical filter, never a ranking of fit. The caller (the
    /// Conversion Agent, through the MCP tool) judges which hit actually
    /// belongs. An empty query returns no hits: unlike [`crate::catalog`],
    /// a fragment library is not closed or small enough to browse whole
    /// (see [`crate::search::matches`]'s own doc on this asymmetry).
    pub fn search(&self, query: &str) -> Vec<&FragmentEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                search::matches(
                    query,
                    &[&entry.title, entry.preview.as_deref().unwrap_or("")],
                )
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The fragment's own JCR path is its position in the library directory,
/// relative to `library_root`, mirrored under
/// `/content/dam/formsanddocuments` -- the same "path IS identity"
/// convention `u2s-aem::model::FragmentRef` already expects.
fn scan_one(library_root: &Path, content_xml: &Path) -> Option<FragmentEntry> {
    let fragment_dir = content_xml.parent()?;
    let relative = fragment_dir.strip_prefix(library_root).ok()?;
    let mut segments = Vec::new();
    for component in relative.components() {
        segments.push(component.as_os_str().to_str()?.to_owned());
    }
    if segments.is_empty() {
        return None;
    }
    let frag_ref = format!("/content/dam/formsanddocuments/{}", segments.join("/"));

    let file = std::fs::File::open(content_xml).ok()?;
    let (title, preview) = extract_title_and_preview(std::io::BufReader::new(file));

    Some(FragmentEntry {
        frag_ref,
        title: title.unwrap_or_else(|| segments.last().cloned().unwrap_or_default()),
        preview,
    })
}

/// One pass over the XML: the first `jcr:title` attribute found becomes
/// the title, the first `sling:message` becomes the preview. Malformed
/// XML yields whatever was found before the parse gave up, never a panic
/// -- a broken fragment file should not take the rest of the library scan
/// down with it.
fn extract_title_and_preview<R: BufRead>(input: R) -> (Option<String>, Option<String>) {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut title = None;
    let mut preview = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(tag)) | Ok(Event::Empty(tag)) => {
                for attr in tag.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                    let Ok(value) = attr.unescape_value() else {
                        continue;
                    };
                    if key == "jcr:title" && title.is_none() {
                        title = Some(value.into_owned());
                    } else if key == "sling:message" && preview.is_none() {
                        preview = Some(value.into_owned());
                    }
                }
                if title.is_some() && preview.is_some() {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    (title, preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_fragment(dir: &Path, relative: &str, title: &str, message: Option<&str>) -> PathBuf {
        let fragment_dir = dir.join(relative);
        std::fs::create_dir_all(&fragment_dir).unwrap();
        let content_xml = fragment_dir.join(".content.xml");
        let message_attr = message
            .map(|m| format!(" sling:message=\"{m}\""))
            .unwrap_or_default();
        std::fs::write(
            &content_xml,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:jcr="http://www.jcp.org/jcr/1.0" jcr:title="{title}"{message_attr}/>
"#
            ),
        )
        .unwrap();
        content_xml
    }

    #[test]
    fn scanning_an_empty_directory_yields_an_empty_library() {
        let dir = tempdir();
        let library = FragmentLibrary::scan(dir.path()).expect("scan");
        assert!(library.is_empty());
    }

    #[test]
    fn a_scanned_fragment_is_findable_by_its_title() {
        let dir = tempdir();
        write_fragment(
            dir.path(),
            "afforms_ch_fragmentlib/address",
            "Address Block",
            None,
        );
        let library = FragmentLibrary::scan(dir.path()).expect("scan");
        assert_eq!(library.len(), 1);
        let hits = library.search("address");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].frag_ref,
            "/content/dam/formsanddocuments/afforms_ch_fragmentlib/address"
        );
    }

    #[test]
    fn search_is_case_insensitive_and_matches_the_preview_too() {
        let dir = tempdir();
        write_fragment(
            dir.path(),
            "lib/beneficial-owner",
            "Beneficial Owner",
            Some("Please declare the beneficial owner"),
        );
        let library = FragmentLibrary::scan(dir.path()).expect("scan");
        assert_eq!(library.search("BENEFICIAL").len(), 1);
        assert_eq!(library.search("declare").len(), 1);
        assert!(library.search("nonexistent").is_empty());
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        let dir = tempdir();
        write_fragment(dir.path(), "lib/x", "X", None);
        let library = FragmentLibrary::scan(dir.path()).expect("scan");
        assert!(library.search("   ").is_empty());
    }

    #[test]
    fn the_empty_constructor_never_touches_the_filesystem() {
        let library = FragmentLibrary::empty();
        assert!(library.is_empty());
        assert!(library.search("anything").is_empty());
    }

    #[test]
    fn a_missing_directory_is_a_hard_error_not_an_empty_library() {
        let missing = std::env::temp_dir().join("u2s-mapper-aem-does-not-exist-1234567");
        let result = FragmentLibrary::scan(&missing);
        assert!(
            result.is_err(),
            "a configured-but-broken path must not silently become empty"
        );
    }

    fn tempdir() -> TempDir {
        TempDir::new()
    }

    /// A tiny self-cleaning temp directory, so this crate does not need a
    /// `tempfile` dev-dependency for four small tests.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            // One counter for the whole test binary: tests run in parallel,
            // and two created within the clock's resolution must still get
            // directories of their own.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "u2s-mapper-aem-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
