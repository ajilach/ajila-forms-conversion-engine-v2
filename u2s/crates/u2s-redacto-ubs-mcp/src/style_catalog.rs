//! `style_search`'s own logic: which CSS class fits a panel or a furniture
//! block is a judgment call this server refuses to make in Rust, exactly
//! the reasoning `u2s-mapper-aem::fragment_library` already gives for
//! `fragment_search` -- this module is mechanical, a plain substring search
//! over a catalogue, never a verdict on fit.
//!
//! The catalogue has two sources: the platform's own **published class
//! vocabulary** (`ajila-redacto-platform/.context/architecture/document-config.md`'s
//! own table -- `.right`, `.logo`, `.page-number`, `.page-count`,
//! `.preserve-spaces`, `.redacto-reading-order`, plus the panel styles
//! `layout-split`/`layout-split-block`/`footnote`/`new-page`), which every
//! deployment carries regardless of configuration, and a **scanned
//! stylesheet directory** (`U2S_REDACTO_STYLE_DIR`) of the deployment's own
//! `.css` files, for a tenant's own additional classes.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::search;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleEntryKind {
    /// Part of the platform's own fixed contract -- exists on every
    /// deployment, scanned directory or not.
    Published,
    /// Found in a scanned `.css` file.
    Scanned,
}

impl StyleEntryKind {
    fn as_str(self) -> &'static str {
        match self {
            StyleEntryKind::Published => "published",
            StyleEntryKind::Scanned => "scanned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleEntry {
    /// The class name to write, without its leading `.`.
    pub class: String,
    pub kind: StyleEntryKind,
    pub preview: String,
}

impl StyleEntry {
    pub fn kind_str(&self) -> &'static str {
        self.kind.as_str()
    }
}

/// The platform's own published vocabulary -- part of the format's contract,
/// not deployment data, so it is compiled in rather than scanned.
fn published_entries() -> Vec<StyleEntry> {
    const PUBLISHED: &[(&str, &str)] = &[
        ("right", "float right inside a slot"),
        ("logo", "brand image in the header, sized by the stylesheet"),
        ("page-number", "replaced with counter(page)"),
        ("page-count", "replaced with counter(pages)"),
        ("preserve-spaces", "keep runs of spaces (a space-aligned form number)"),
        (
            "redacto-reading-order",
            "this text is meaningful: cloned into the tagged reading order",
        ),
        ("layout-split", "styledPanel: column-count 2, column-fill balance"),
        ("layout-split-block", "styledPanel: a two-column grid block"),
        ("footnote", "styledPanel: the trailing footnote assets"),
        ("new-page", "styledPanel: forces a page break before this panel"),
    ];
    PUBLISHED
        .iter()
        .map(|(class, preview)| StyleEntry {
            class: (*class).to_owned(),
            kind: StyleEntryKind::Published,
            preview: (*preview).to_owned(),
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum StyleCatalogError {
    #[error("cannot read style directory {path}: {source}")]
    ReadDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub struct StyleCatalog {
    entries: Vec<StyleEntry>,
}

impl StyleCatalog {
    /// The published vocabulary alone -- what a deployment with no scanned
    /// style directory looks like. Not degraded: the published entries are
    /// always real answers, never a placeholder for "not configured yet".
    pub fn published_only() -> Self {
        Self {
            entries: published_entries(),
        }
    }

    /// Scans every `.css` file directly under `dir` (not recursively --
    /// this workspace's own stylesheet bundles are flat) for class
    /// selectors, in addition to the published vocabulary. A directory that
    /// does not exist or cannot be read is a hard error, the same
    /// "misconfiguration refuses to start" discipline
    /// `U2S_AEM_FRAGMENT_DIR`/`U2S_FONT_DIR` already apply elsewhere in this
    /// workspace.
    pub fn scan(dir: &Path) -> Result<Self, StyleCatalogError> {
        let mut entries = published_entries();
        let read = std::fs::read_dir(dir).map_err(|source| StyleCatalogError::ReadDir {
            path: dir.display().to_string(),
            source,
        })?;
        for item in read {
            let item = item.map_err(|source| StyleCatalogError::ReadDir {
                path: dir.display().to_string(),
                source,
            })?;
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("css") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            entries.extend(scan_classes(&text));
        }
        entries.sort_by(|a, b| a.class.cmp(&b.class));
        entries.dedup_by(|a, b| a.class == b.class && a.kind == b.kind);
        Ok(Self { entries })
    }

    /// Case-insensitive substring match over the class name and preview,
    /// mirroring `fragment_library::FragmentLibrary::search`'s own
    /// asymmetry: mechanical filtering, never a ranking of fit.
    pub fn search(&self, query: &str) -> Vec<&StyleEntry> {
        self.entries
            .iter()
            .filter(|entry| search::matches(query, &[&entry.class, &entry.preview]))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn class_rule_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?s)\.([A-Za-z][A-Za-z0-9_-]*)\s*\{([^}]*)\}").expect("valid built-in pattern")
    })
}

/// One entry per distinct class selector found in `text`, with the first
/// non-blank declaration line as its preview. A mechanical scan of plain
/// CSS text, not a real parser -- this workspace's own stylesheet bundles
/// are hand-authored and small, and a class this misses just does not show
/// up in a search, which is a strictly worse but never wrong answer (the
/// published vocabulary above never depends on scanning).
fn scan_classes(text: &str) -> Vec<StyleEntry> {
    class_rule_regex()
        .captures_iter(text)
        .map(|caps| {
            let class = caps[1].to_owned();
            let preview = caps[2]
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("")
                .trim_end_matches(';')
                .to_owned();
            StyleEntry {
                class,
                kind: StyleEntryKind::Scanned,
                preview,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_only_carries_the_platform_vocabulary() {
        let catalog = StyleCatalog::published_only();
        assert!(catalog.search("page-number").iter().any(|e| e.class == "page-number"));
        assert!(catalog.search("footnote").iter().any(|e| e.class == "footnote"));
    }

    #[test]
    fn scan_finds_a_class_from_a_real_css_file() {
        let dir = TempDir::new();
        std::fs::write(
            dir.path().join("tenant.css"),
            ".tab-box {\n  border: 1px solid black;\n  padding: 2mm;\n}\n",
        )
        .unwrap();
        let catalog = StyleCatalog::scan(dir.path()).expect("scan");
        let hits = catalog.search("tab-box");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, StyleEntryKind::Scanned);
        assert!(hits[0].preview.contains("border"));
    }

    #[test]
    fn scan_still_carries_the_published_vocabulary_alongside_scanned_classes() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("tenant.css"), ".tab-box { color: red; }\n").unwrap();
        let catalog = StyleCatalog::scan(dir.path()).expect("scan");
        assert!(catalog.search("tab-box").iter().any(|e| e.kind == StyleEntryKind::Scanned));
        assert!(catalog.search("footnote").iter().any(|e| e.kind == StyleEntryKind::Published));
    }

    #[test]
    fn a_missing_directory_is_a_hard_error() {
        let missing = std::env::temp_dir().join("u2s-redacto-ubs-mcp-does-not-exist-1234567");
        assert!(StyleCatalog::scan(&missing).is_err());
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "u2s-redacto-ubs-mcp-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
