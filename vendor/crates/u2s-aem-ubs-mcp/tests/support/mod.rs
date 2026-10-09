//! Shared test helpers for the ported UBS writer/XSD/profile tests.
//!
//! Mirrors the shape of `blueprint`'s (the deleted `core` crate's) old
//! `tests/helpers.rs`, trimmed to what still applies once the mechanical
//! PDF/XFA -> `StructuredNode` pipeline is gone: everything here builds an
//! `AemNode`/`AemNodeTranslated` tree by hand or loads it from a fixture
//! package, never from a source PDF.

#![allow(dead_code)] // Not every helper is used by every test binary.

pub mod package;

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use u2s_aem_ubs_mcp::aem::{AemConfig, AemNode, AemProfile, ParsedAemPackage, ParsedFragment};
use u2s_aem_ubs_mcp::context::Context;
use u2s_aem_ubs_mcp::xsd::XsdConfig;

/// Load the UBS AEM profile (config.toml and translations), embedded in
/// the crate, the same way production does.
pub fn ubs_profile() -> AemProfile {
    u2s_aem_ubs_mcp::profiles::load_aem_profile("ubs").expect("load the embedded UBS AEM profile")
}

/// Load the UBS XSD config (config.toml + `types/` registry), embedded in the
/// crate.
pub fn ubs_xsd_config() -> XsdConfig {
    u2s_aem_ubs_mcp::profiles::load_xsd_config("ubs").expect("load the embedded UBS XSD config")
}

/// Load the UBS profile's parsed fragment library, the way `load_aem_config`
/// does for a config with `use_fragments = true`.
pub fn ubs_fragments() -> Vec<ParsedFragment> {
    let profile = ubs_profile();
    let prefix = profile
        .fragment_ref_prefix
        .as_deref()
        .unwrap_or("/content/dam/formsanddocuments/");
    let paths: Vec<String> = profile
        .fragment_paths
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    u2s_aem_ubs_mcp::profiles::load_aem_fragments("ubs", prefix, &paths)
        .expect("load the embedded UBS fragment library")
}

/// Build an `AemConfig` from the real UBS profile, for a form with the given
/// master language and XFA variables (`formrange_code`, `formrange_entity`).
pub fn ubs_config(master_language: &str, form_code: &str, entity: &str) -> AemConfig {
    let profile = ubs_profile();
    let mut vars = HashMap::new();
    vars.insert("formrange_code".to_string(), form_code.to_string());
    vars.insert("formrange_entity".to_string(), entity.to_string());
    let ctx = Context::new(master_language.to_string(), vars);
    AemConfig::from_profile(&profile, &ctx)
        .expect("build AemConfig from the UBS profile")
}

/// Like [`ubs_config`], but through `profiles::load_aem_config`, which also
/// wires up the XSD config and fragment library -- the configuration
/// production actually ships, used by the lift/lower/passthrough round-trip
/// tests.
pub fn ubs_config_for(master_language: &str, languages: &[String], form_code: &str) -> AemConfig {
    // The UBS profile derives the form's identity and paths from its XFA
    // variables; `formrange_entity` only affects the folders.
    let mut vars = HashMap::new();
    vars.insert("formrange_code".to_string(), form_code.to_string());
    vars.insert("formrange_entity".to_string(), "019".to_string());
    let ctx = Context::new(master_language.to_string(), vars);
    let mut config = u2s_aem_ubs_mcp::profiles::load_aem_config("ubs", &ctx)
        .expect("build AemConfig via load_aem_config(\"ubs\")");
    config.master_language = master_language.to_string();
    config.languages = languages.to_vec();
    config
}

/// Render `children` under a `Root` through the real UBS profile and writer,
/// with a fixed default form identity (`AAAI`, Germany, English) -- for tests
/// that only care about the scaffolding (root/toolbar/summary) or about one
/// hand-built node's own tag.
pub fn ubs_xml(children: Vec<AemNode>) -> String {
    let config = ubs_config("en", "AAAI", "019");
    let root = AemNode::Root {
        title: "Test Form".into(),
        children,
    };
    u2s_aem_ubs_mcp::aem::generate_aem_xml(&root, &config).expect("the form is written")
}

/// [`ubs_xml`] with no children at all -- for assertions that only concern
/// what `root.xml` itself emits (the toolbar, the summary panel, the
/// FormMetadata step).
pub fn ubs_root_xml() -> String {
    ubs_xml(vec![])
}

/// Wrap a bare AEM form `.content.xml` in a minimal in-memory FileVault ZIP so
/// it can go through the real `parse_aem_zip` entry point.
pub fn aem_zip_from_form_xml(form_code: &str, content_xml: &str) -> Vec<u8> {
    use std::io::Write;

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    writer
        .start_file(
            format!("jcr_root/content/forms/af/fixtures/{form_code}/.content.xml"),
            options,
        )
        .expect("start zip entry");
    writer
        .write_all(content_xml.as_bytes())
        .expect("write zip entry");
    writer.finish().expect("finish zip").into_inner()
}

/// Path to `tests/fixtures/{subpath}`.
pub fn fixture_path(subpath: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(subpath)
}

/// Read a fixture file as a `String`, panicking with the full path on failure.
pub fn read_fixture(subpath: &str) -> String {
    let path = fixture_path(subpath);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path:?}: {e}"))
}

/// Read a real deployed UBS package fixture (`tests/fixtures/ubs-packages/`).
/// The form code of a package: its form folder, `.../AF_<code>/.content.xml`
/// under `jcr_root/content/forms/af` (some packages nest `jcr_root` in a
/// folder of their own). The deployed packages predate the metadata draw
/// that names it, and the form's title is not its code.
#[allow(dead_code)]
pub fn package_form_code(zip_bytes: &[u8]) -> String {
    let archive = zip::ZipArchive::new(Cursor::new(zip_bytes)).expect("the package is a zip");
    let codes: std::collections::BTreeSet<String> = archive
        .file_names()
        .filter(|name| name.contains("jcr_root/content/forms/af/") && !name.starts_with("__MACOSX/"))
        .filter_map(|name| name.strip_suffix("/.content.xml"))
        .filter_map(|dir| dir.rsplit('/').next()?.strip_prefix("AF_"))
        .map(str::to_owned)
        .collect();
    assert_eq!(codes.len(), 1, "a package holds one form folder: {codes:?}");
    codes.into_iter().next().unwrap()
}

pub fn read_package_fixture(name: &str) -> Vec<u8> {
    let path = fixture_path("ubs-packages").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read package fixture {path:?}: {e}"))
}

/// Parse a UBS `af-xsd-automation` fixture form's bare `.content.xml`
/// (`tests/fixtures/ubs-xsd/{form_code}/source.content.xml`) into an
/// `AemNode` tree.
pub fn parse_fixture_form(form_code: &str) -> AemNode {
    let xml = read_fixture(&format!("ubs-xsd/{form_code}/source.content.xml"));
    let zip = aem_zip_from_form_xml(form_code, &xml);
    let parsed = u2s_aem_ubs_mcp::aem::parse_aem_zip(&zip)
        .unwrap_or_else(|e| panic!("parse fixture form {form_code}: {e}"));
    parsed.root
}

/// Every element path in a schema, in depth-first document order.
///
/// Paths are absolute and rooted at the schema's root element, so they are
/// directly comparable with `bindRef` values.
pub fn xsd_element_paths_in_order(schema: &u2s_aem_ubs_mcp::xsd::XsdSchema) -> Vec<String> {
    fn go(node: &u2s_aem_ubs_mcp::xsd::XsdNode, parent: &str, out: &mut Vec<String>) {
        use u2s_aem_ubs_mcp::xsd::XsdNode;
        match node {
            XsdNode::Element { name, content, .. } => {
                let path = format!("{parent}/{name}");
                out.push(path.clone());
                if let Some(child) = content {
                    go(child, &path, out);
                }
            }
            XsdNode::Ref { ref_name, .. } => out.push(format!("{parent}/{ref_name}")),
            XsdNode::ComplexType { sequence, .. } => {
                for child in sequence {
                    go(child, parent, out);
                }
            }
        }
    }

    let mut out = Vec::new();
    go(&schema.root, "", &mut out);
    out
}

/// The `bind_ref` of any node that can carry one.
pub fn node_bind_ref(node: &AemNode) -> Option<&str> {
    match node {
        AemNode::Panel { bind_ref, .. }
        | AemNode::Repeatable { bind_ref, .. }
        | AemNode::TextField { bind_ref, .. }
        | AemNode::NumberField { bind_ref, .. }
        | AemNode::DatePicker { bind_ref, .. }
        | AemNode::Dropdown { bind_ref, .. }
        | AemNode::Checkbox { bind_ref, .. }
        | AemNode::RadioButton { bind_ref, .. }
        | AemNode::Fragment { bind_ref, .. } => bind_ref.as_deref(),
        _ => None,
    }
}

/// Recursively walk an `AemNode` tree, calling `callback` on every node.
pub fn walk_aem_nodes(node: &AemNode, callback: &mut impl FnMut(&AemNode)) {
    callback(node);
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => {
            for child in children {
                walk_aem_nodes(child, callback);
            }
        }
        _ => {}
    }
}

/// Collect the language union the same way `aem_to_translated`'s callers do:
/// every language any translation entry carries, plus the package's own
/// detected language.
pub fn lift_languages(package: &ParsedAemPackage) -> Vec<String> {
    if package.translations.entries.is_empty() {
        vec![package.language.clone()]
    } else {
        let mut langs: Vec<String> = package
            .translations
            .entries
            .values()
            .flat_map(|m| m.keys().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !langs.contains(&package.language) {
            langs.push(package.language.clone());
        }
        langs
    }
}

/// Lift a parsed package into its editable multilingual working tree.
pub fn lift_package(package: &ParsedAemPackage) -> u2s_aem_ubs_mcp::aem::AemNodeTranslated {
    let languages = lift_languages(package);
    u2s_aem_ubs_mcp::aem::aem_to_translated(
        &package.root,
        &package.translations,
        &languages,
        &package.language,
        &package.raw_by_uuid,
    )
}

/// The full opening tag of the node named `name` in rendered JCR XML,
/// quote-aware (a rich-text `_value` carries a literal `>`).
pub fn open_tag_of(xml: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let at = xml.find(&needle)?;
    let start = xml[..at].rfind('<')?;
    let mut in_quotes = false;
    for (i, c) in xml[start..].char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '>' if !in_quotes => return Some(xml[start..start + i + 1].to_string()),
            _ => {}
        }
    }
    None
}

/// Every open tag of `xml` as `(tag_name, full_tag_text)`, quote-aware.
pub fn open_tags(xml: &str) -> Vec<(String, String)> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' || matches!(bytes.get(i + 1), Some(b'/') | Some(b'!') | Some(b'?')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        let mut quoted = false;
        while j < bytes.len() {
            match bytes[j] {
                b'"' => quoted = !quoted,
                b'>' if !quoted => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let tag = &xml[start..=j];
        let name: String = tag[1..]
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
            .collect();
        out.push((name, tag.to_string()));
        i = j + 1;
    }
    out
}
