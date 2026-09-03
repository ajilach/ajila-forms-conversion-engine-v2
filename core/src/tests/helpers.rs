#![allow(dead_code)] // Helpers may not all be used in current tests

use std::io::{Cursor, Read};
use std::sync::Once;

use crate::structured::{
    ConditionalNode, FieldId, FieldNode, FieldType, InlineNode, ListNode, StructuredNode,
};

/// Ensure UBS profile fonts are loaded into the global font manager.
///
/// This is safe to call from many tests — the loading happens exactly once.
pub fn ensure_ubs_fonts_loaded() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        crate::profiles::load_profile_fonts("ubs")
            .expect("Failed to load UBS profile fonts for tests");
    });
}

/// Build a path to a file in the `input/` test data directory.
///
/// Uses `CARGO_MANIFEST_DIR` so tests work regardless of the current working
/// directory (e.g. running from workspace root vs. package directory).
///
/// Also ensures UBS profile fonts are loaded (lazily, once).
pub fn input_path(filename: &str) -> String {
    ensure_ubs_fonts_loaded();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    format!("{}/input/{}", manifest_dir, filename)
}

/// Build a path to a directory or file inside the `profiles/` directory.
///
/// Profiles are at the workspace root, so we go up one level from CARGO_MANIFEST_DIR.
pub fn profiles_path(subpath: &str) -> String {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    format!("{}/../profiles/{}", manifest_dir, subpath)
}

/// Build a path to a file in the `fixtures/` directory.
///
/// Unlike [`input_path`], this does not load profile fonts — fixtures are text,
/// and font loading costs seconds.
pub fn fixture_path(subpath: &str) -> String {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    format!("{}/fixtures/{}", manifest_dir, subpath)
}

/// Read a fixture file as a `String`, panicking with the full path on failure.
pub fn read_fixture(subpath: &str) -> String {
    let path = fixture_path(subpath);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

/// Wrap a bare AEM form `.content.xml` in a minimal in-memory FileVault ZIP so
/// it can go through the real [`crate::aem::parse_aem_zip`] entry point.
///
/// `parse_aem_zip` only requires that some entry under
/// `jcr_root/content/forms/af/` ends in `/.content.xml` and mentions
/// `guideContainer`; it never reads `META-INF`. That makes this the cheapest way
/// to parse a fixture that ships as a bare form XML rather than a full package.
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

/// Parse a fixture's bare form `.content.xml` into an [`crate::aem::AemNode`] tree.
pub fn parse_fixture_form(form_code: &str) -> crate::aem::AemNode {
    let xml = read_fixture(&format!("aem_xsd/{form_code}/source.content.xml"));
    let zip = aem_zip_from_form_xml(form_code, &xml);
    let parsed = crate::aem::parse_aem_zip(&zip)
        .unwrap_or_else(|e| panic!("parse fixture form {form_code}: {e}"));
    parsed.root
}

/// Like [`build_aem_test_output`] but with XSD binding and fragments enabled,
/// i.e. the configuration production actually ships.
pub(super) fn build_aem_test_output_bound(
    pdfs: &[(&str, &str)],
) -> (
    Vec<StructuredNode>,
    crate::aem::AemNode,
    crate::aem::AemConfig,
) {
    use crate::aem::{AemConfig, convert_to_aem};

    let envelopes: Vec<_> = pdfs
        .iter()
        .map(|(file, lang)| {
            crate::run_exhaustive_to_envelope(input_path(file), lang)
                .unwrap_or_else(|e| panic!("Failed to process {file}: {e}"))
        })
        .collect();

    let (ctx, content) = if envelopes.len() == 1 {
        let env = envelopes.into_iter().next().unwrap();
        (env.context, env.content)
    } else {
        let merged = crate::structured::merge_translations(envelopes, None)
            .expect("Failed to merge translations");
        (merged.context, merged.content)
    };

    let (profile, templates, custom_templates) = load_ubs_profile();
    let mut config = AemConfig::from_profile(&profile, templates, custom_templates, &ctx)
        .expect("Failed to create AemConfig");
    let mut xsd_config = load_ubs_xsd_config();
    xsd_config.form_code = Some(config.form_code.clone());
    config.xsd_config = Some(xsd_config);
    config.fragments = load_ubs_fragments();
    config.use_fragments = true;
    config.bind_to_xsd = true;

    let config = crate::resolve_aem_languages(&content, &config);
    let root = convert_to_aem(&content, &config);
    (content, root, config)
}

/// Load the UBS profile's parsed fragment library, as `load_aem_config` does.
pub fn load_ubs_fragments() -> Vec<crate::aem::ParsedFragment> {
    let (profile, _, _) = load_ubs_profile();
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
    crate::profiles::load_aem_fragments("ubs", prefix, &paths).expect("load UBS fragment library")
}

/// Every element path in a schema, in depth-first document order.
///
/// Paths are absolute and rooted at the schema's root element, so they are
/// directly comparable with `bindRef` values.
pub fn xsd_element_paths_in_order(schema: &crate::xsd::XsdSchema) -> Vec<String> {
    fn go(node: &crate::xsd::XsdNode, parent: &str, out: &mut Vec<String>) {
        use crate::xsd::XsdNode;
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

/// Every `bindRef="…"` value in a rendered AEM XML, in document order.
pub fn scrape_bind_refs(xml: &str) -> Vec<String> {
    let needle = "bindRef=\"";
    let mut out = Vec::new();
    let mut cursor = xml;
    while let Some(pos) = cursor.find(needle) {
        let after = &cursor[pos + needle.len()..];
        match after.find('"') {
            Some(end) => {
                out.push(after[..end].to_string());
                cursor = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// The `bind_ref` of any node that can carry one.
pub fn node_bind_ref(node: &crate::aem::AemNode) -> Option<&str> {
    use crate::aem::AemNode;
    match node {
        AemNode::Panel { bind_ref, .. }
        | AemNode::Repeatable { bind_ref, .. }
        | AemNode::TextField { bind_ref, .. }
        | AemNode::NumberField { bind_ref, .. }
        | AemNode::DatePicker { bind_ref, .. }
        | AemNode::Dropdown { bind_ref, .. }
        | AemNode::Checkbox { bind_ref, .. }
        | AemNode::RadioButton { bind_ref, .. }
        | AemNode::Fragment { bind_ref, .. }
        | AemNode::Custom { bind_ref, .. } => bind_ref.as_deref(),
        _ => None,
    }
}

/// Every `fragRef` reachable in an AEM tree.
///
/// Covers all three shapes a fragment can take: an opaque `Fragment` node (the
/// fragment could not be resolved), a `Panel` whose fragment was inlined, and a
/// `Repeatable` built from a repeating fragment panel.
pub fn collect_frag_refs(root: &crate::aem::AemNode) -> Vec<String> {
    use crate::aem::AemNode;
    let mut found = Vec::new();
    walk_aem_nodes(root, &mut |node| match node {
        AemNode::Fragment { frag_ref, .. } => found.push(frag_ref.clone()),
        AemNode::Panel {
            frag_ref: Some(fr), ..
        }
        | AemNode::Repeatable {
            frag_ref: Some(fr), ..
        } => found.push(fr.clone()),
        _ => {}
    });
    found
}

/// Recursively walk a tree of `StructuredNode`s, calling `callback` on every
/// node encountered (depth-first, pre-order).
///
/// The walker handles all node variants that can contain children:
/// `Group`, `Conditional`, `Repeatable`, `GridLayout`, and `Table`.
///
/// # Examples
///
/// Collecting all `FieldNode`s:
/// ```ignore
/// let mut fields = Vec::new();
/// walk_structured_nodes(&nodes, &mut |node| {
///     if let StructuredNode::Field(f) = node {
///         fields.push(f.clone());
///     }
/// });
/// ```
pub fn walk_structured_nodes(nodes: &[StructuredNode], callback: &mut impl FnMut(&StructuredNode)) {
    for node in nodes {
        callback(node);
        match node {
            StructuredNode::Group(group) => {
                walk_structured_nodes(&group.children, callback);
            }
            StructuredNode::Conditional(cond) => {
                walk_structured_nodes(std::slice::from_ref(cond.content.as_ref()), callback);
            }
            StructuredNode::Repeatable(rep) => {
                walk_structured_nodes(std::slice::from_ref(rep.item.as_ref()), callback);
            }
            StructuredNode::GridLayout(grid) => {
                for element in &grid.elements {
                    walk_structured_nodes(std::slice::from_ref(&element.node), callback);
                }
            }
            StructuredNode::Table(table) => {
                if let Some(header) = &table.header {
                    walk_structured_nodes(&header.cells, callback);
                }
                for row in &table.rows {
                    walk_structured_nodes(&row.cells, callback);
                }
            }
            _ => {}
        }
    }
}

// ============================================================================
// Typed collector helpers built on top of `walk_structured_nodes`
// ============================================================================

/// Collect all `FieldNode`s from the tree.
pub fn collect_fields(nodes: &[StructuredNode]) -> Vec<FieldNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Field(f) = node {
            out.push(f.clone());
        }
    });
    out
}

/// Collect all `FieldNode`s whose `input_type` is `Radio`.
pub fn collect_radio_fields(nodes: &[StructuredNode]) -> Vec<FieldNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Field(f) = node {
            if matches!(f.input_type, FieldType::Radio { .. }) {
                out.push(f.clone());
            }
        }
    });
    out
}

/// Collect all `FieldNode`s whose `input_type` is `Textarea`.
pub fn collect_textarea_fields(nodes: &[StructuredNode]) -> Vec<FieldNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Field(f) = node {
            if matches!(f.input_type, FieldType::Textarea { .. }) {
                out.push(f.clone());
            }
        }
    });
    out
}

/// Collect field labels (as plain text) from all `FieldNode`s in the tree.
pub fn collect_field_labels(nodes: &[StructuredNode]) -> Vec<String> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Field(f) = node {
            if let Some(label) = &f.label {
                out.push(label.as_plain_text());
            }
        }
    });
    out
}

/// Collect field labels with trimming — skips fields with empty labels after trim.
pub fn collect_field_labels_trimmed(nodes: &[StructuredNode]) -> Vec<String> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Field(f) = node {
            if let Some(label) = &f.label {
                let text = label.as_plain_text();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
            }
        }
    });
    out
}

/// Collect field names (SOM path strings) from all `FieldNode`s in the tree.
pub fn collect_field_names(nodes: &[StructuredNode]) -> Vec<String> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Field(f) = node {
            out.push(f.som_path_str().to_string());
        }
    });
    out
}

/// Collect all `HeadingNode`s as `(level, text)` pairs.
pub fn collect_headings(nodes: &[StructuredNode]) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Heading(h) = node {
            out.push((h.level.as_u8(), h.content.as_plain_text()));
        }
    });
    out
}

/// Collect all `FootnoteNode`s as `(marker, text)` pairs from the tree.
pub fn collect_footnotes(nodes: &[StructuredNode]) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Footnote(f) = node {
            out.push((f.marker.clone(), f.content.as_plain_text()));
        }
    });
    out
}

/// Collect every screen-only notice's text.
pub fn collect_notices(nodes: &[StructuredNode]) -> Vec<String> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Notice(n) = node {
            out.push(n.content.as_plain_text());
        }
    });
    out
}

/// Collect all `ConditionalNode`s from the tree.
pub fn collect_conditionals(nodes: &[StructuredNode]) -> Vec<ConditionalNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Conditional(c) = node {
            out.push(c.clone());
        }
    });
    out
}

/// Count the number of `Conditional` nodes in the tree.
pub fn count_conditionals(nodes: &[StructuredNode]) -> usize {
    let mut count = 0;
    walk_structured_nodes(nodes, &mut |node| {
        if matches!(node, StructuredNode::Conditional(_)) {
            count += 1;
        }
    });
    count
}

/// Return `true` if any `Paragraph` or `Heading` reachable from `nodes`
/// **without** passing through a `Conditional` node contains `fragment` in its
/// plain text.
///
/// This is used to assert that text blocks which are common to all form states
/// are properly "factored out" of conditionals in the merged output.
pub fn has_text_outside_conditional(nodes: &[StructuredNode], fragment: &str) -> bool {
    for node in nodes {
        match node {
            StructuredNode::Paragraph(p) => {
                if p.content.as_plain_text().contains(fragment) {
                    return true;
                }
            }
            StructuredNode::Heading(h) => {
                if h.content.as_plain_text().contains(fragment) {
                    return true;
                }
            }
            StructuredNode::Group(g) => {
                if has_text_outside_conditional(&g.children, fragment) {
                    return true;
                }
            }
            StructuredNode::Repeatable(r) => {
                if has_text_outside_conditional(std::slice::from_ref(r.item.as_ref()), fragment) {
                    return true;
                }
            }
            StructuredNode::GridLayout(grid) => {
                for element in &grid.elements {
                    if has_text_outside_conditional(std::slice::from_ref(&element.node), fragment) {
                        return true;
                    }
                }
            }
            StructuredNode::Table(table) => {
                if let Some(header) = &table.header {
                    if has_text_outside_conditional(&header.cells, fragment) {
                        return true;
                    }
                }
                for row in &table.rows {
                    if has_text_outside_conditional(&row.cells, fragment) {
                        return true;
                    }
                }
            }
            // Intentionally do NOT recurse into Conditional nodes
            StructuredNode::Conditional(_) => {}
            _ => {}
        }
    }
    false
}

/// Collect all `ListNode`s from the tree.
pub fn collect_lists(nodes: &[StructuredNode]) -> Vec<ListNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::List(l) = node {
            out.push(l.clone());
        }
    });
    out
}

/// Collect all `TableNode`s from the tree.
pub fn collect_tables(nodes: &[StructuredNode]) -> Vec<crate::structured::TableNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Table(t) = node {
            out.push(t.clone());
        }
    });
    out
}

/// Collect all `InlineNode`s from `Paragraph` nodes in the tree.
pub fn collect_inline_nodes(nodes: &[StructuredNode]) -> Vec<InlineNode> {
    let mut out = Vec::new();
    walk_structured_nodes(nodes, &mut |node| {
        if let StructuredNode::Paragraph(p) = node {
            if let Some(inline_text) = p.content.0.values().next() {
                out.extend(inline_text.0.clone());
            }
        }
    });
    out
}

/// Find the `FieldId` of the first field whose SOM path ends with `suffix`.
pub fn find_field_id_by_suffix(nodes: &[StructuredNode], suffix: &str) -> Option<FieldId> {
    let fields = collect_fields(nodes);
    fields
        .iter()
        .find(|f| f.som_path_str().ends_with(suffix))
        .map(|f| f.name.clone())
}

/// Find the first field whose SOM path contains `name`.
pub fn find_field_by_name(nodes: &[StructuredNode], name: &str) -> Option<FieldNode> {
    collect_fields(nodes)
        .into_iter()
        .find(|f| f.som_path_str().contains(name))
}

/// Load the UBS profile (config.toml + XML templates) from `profiles/ubs/aem/`.
///
/// Returns `(AemProfile, templates, custom_templates)` ready for `AemConfig::from_profile`.
pub fn load_ubs_profile() -> (
    crate::aem::AemProfile,
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, String>,
) {
    let dir_path = profiles_path("ubs/aem");
    let dir = std::path::Path::new(&dir_path);
    let toml_str = std::fs::read_to_string(dir.join("config.toml"))
        .expect("Failed to read profiles/ubs/aem/config.toml");
    // Disable custom element rules by default for tests so that existing tests
    // asserting on Fragment/Panel structure are not affected by the custom
    // element replacement pass.
    let mut profile: crate::aem::AemProfile =
        toml::from_str(&toml_str).expect("Failed to parse UBS config.toml");
    profile.custom_elements.clear();

    // Load translations/ directory (per-language TOML files)
    let translations_dir = dir.join("translations");
    if translations_dir.is_dir() {
        for entry in std::fs::read_dir(&translations_dir).expect("Failed to read translations/ dir")
        {
            let entry = entry.expect("Failed to read translations dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Some(lang) = path.file_stem().and_then(|s| s.to_str()) {
                    let content =
                        std::fs::read_to_string(&path).expect("Failed to read translation TOML");
                    crate::profiles::parse_translation_toml(
                        &content,
                        lang,
                        &mut profile.default_translations,
                    )
                    .unwrap_or_else(|e| panic!("Failed to parse translations/{lang}.toml: {e}"));
                }
            }
        }
    }

    let mut templates = std::collections::HashMap::new();
    for entry in std::fs::read_dir(dir).expect("Failed to read profiles/ubs/aem/") {
        let entry = entry.expect("Failed to read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("xml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let content = std::fs::read_to_string(&path).expect("Failed to read template");
                templates.insert(stem.to_string(), content);
            }
        }
    }

    let mut custom_templates = std::collections::HashMap::new();
    let custom_dir = dir.join("custom");
    if custom_dir.is_dir() {
        for entry in std::fs::read_dir(&custom_dir).expect("Failed to read custom/ dir") {
            let entry = entry.expect("Failed to read custom dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("xml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let content =
                        std::fs::read_to_string(&path).expect("Failed to read custom template");
                    custom_templates.insert(stem.to_string(), content);
                }
            }
        }
    }

    (profile, templates, custom_templates)
}

/// Like `load_ubs_profile`, but keeps custom_elements enabled so that tests
/// can verify custom element placement in the AEM tree.
pub fn load_ubs_profile_with_custom_elements() -> (
    crate::aem::AemProfile,
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, String>,
) {
    let (mut profile, templates, custom_templates) = load_ubs_profile();
    // Reload custom_elements from the config (load_ubs_profile clears them).
    let dir_path = profiles_path("ubs/aem");
    let dir = std::path::Path::new(&dir_path);
    let toml_str = std::fs::read_to_string(dir.join("config.toml"))
        .expect("Failed to read profiles/ubs/aem/config.toml");
    let full_profile: crate::aem::AemProfile =
        toml::from_str(&toml_str).expect("Failed to parse UBS config.toml");
    profile.custom_elements = full_profile.custom_elements;
    (profile, templates, custom_templates)
}

/// Build AEM node tree with custom elements enabled.
pub(super) fn build_aem_test_output_with_custom_elements(
    pdfs: &[(&str, &str)],
) -> crate::aem::AemNode {
    use crate::aem::{AemConfig, convert_to_aem};

    let envelopes: Vec<_> = pdfs
        .iter()
        .map(|(file, lang)| {
            crate::run_exhaustive_to_envelope(input_path(file), lang)
                .unwrap_or_else(|e| panic!("Failed to process {file}: {e}"))
        })
        .collect();

    let (ctx, content) = if envelopes.len() == 1 {
        let env = envelopes.into_iter().next().unwrap();
        (env.context, env.content)
    } else {
        let merged = crate::structured::merge_translations(envelopes, None)
            .expect("Failed to merge translations");
        (merged.context, merged.content)
    };

    let (profile, templates, custom_templates) = load_ubs_profile_with_custom_elements();
    let config = AemConfig::from_profile(&profile, templates, custom_templates, &ctx)
        .expect("Failed to create AemConfig");
    let config = crate::resolve_aem_languages(&content, &config);
    convert_to_aem(&content, &config)
}

/// Generate AEM XML from one or more PDF files and validate that it is
/// well-formed XML. Each entry is `(filename, language_code)`.
pub fn assert_aem_xml_valid_for(pdfs: &[(&str, &str)]) {
    let (_, root, config) = build_aem_test_output(pdfs);
    let xml = crate::aem::generate_aem_xml(&root, &config);

    assert_valid_aem_form_xml(&xml);
}

/// Generate an AEM package entirely in memory from one or more PDF files and
/// validate both the form and DAM `.content.xml` entries extracted from the ZIP.
pub fn assert_aem_package_valid_for(pdfs: &[(&str, &str)]) {
    let (content, root, config) = build_aem_test_output(pdfs);
    let zip_bytes = crate::aem::generate_aem_package(&root, &config, &content);
    let reader = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("valid in-memory zip");

    let form_entry = format!(
        "jcr_root/content/forms/af/{}/{}/.content.xml",
        config.form_path,
        config.form_dir()
    );
    let dam_entry = format!(
        "jcr_root/content/dam/formsanddocuments/{}/{}/.content.xml",
        config.form_path,
        config.form_dir()
    );

    let mut form_xml = String::new();
    archive
        .by_name(&form_entry)
        .unwrap_or_else(|_| panic!("package missing form entry: {form_entry}"))
        .read_to_string(&mut form_xml)
        .expect("read form xml from zip");

    let mut dam_xml = String::new();
    archive
        .by_name(&dam_entry)
        .unwrap_or_else(|_| panic!("package missing DAM entry: {dam_entry}"))
        .read_to_string(&mut dam_xml)
        .expect("read dam xml from zip");

    assert_valid_aem_form_xml(&form_xml);
    assert_valid_aem_dam_xml(&dam_xml);
}

/// Assert that the given string is well-formed XML.
///
/// Delegates to [`crate::aem::validate_xml_wellformed`] and panics with the
/// returned message (including a snippet of the surrounding XML) on failure.
pub fn assert_valid_xml(xml: &str) {
    if let Err(e) = crate::aem::validate_xml_wellformed(xml) {
        panic!("{e}");
    }
}

/// Assert that a generated AEM form `.content.xml` is both well-formed and
/// structurally valid for the JCR/CQ/FD/Sling conventions used by this project.
///
/// The validation logic lives in [`crate::aem::xml_validation`] so it can be
/// shared with production callers; this wrapper preserves the panic-on-failure
/// contract the test suite relies on.
pub fn assert_valid_aem_form_xml(xml: &str) {
    assert_valid_xml(xml);
    let violations = crate::aem::xml_validation::validate_aem_form_structure(xml);
    if !violations.is_empty() {
        panic!(
            "AEM form XML failed {} structural validation checks:\n- {}",
            violations.len(),
            violations.join("\n- ")
        );
    }
}

/// Assert that a generated AEM DAM `.content.xml` is both well-formed and
/// structurally valid for the JCR/DAM/Sling conventions used by this project.
pub fn assert_valid_aem_dam_xml(xml: &str) {
    assert_valid_xml(xml);
    let violations = crate::aem::xml_validation::validate_aem_dam_structure(xml);
    if !violations.is_empty() {
        panic!(
            "AEM DAM XML failed {} structural validation checks:\n- {}",
            violations.len(),
            violations.join("\n- ")
        );
    }
}

pub(super) fn build_aem_test_output(
    pdfs: &[(&str, &str)],
) -> (
    Vec<StructuredNode>,
    crate::aem::AemNode,
    crate::aem::AemConfig,
) {
    use crate::aem::{AemConfig, convert_to_aem};

    let envelopes: Vec<_> = pdfs
        .iter()
        .map(|(file, lang)| {
            crate::run_exhaustive_to_envelope(input_path(file), lang)
                .unwrap_or_else(|e| panic!("Failed to process {file}: {e}"))
        })
        .collect();

    let (ctx, content) = if envelopes.len() == 1 {
        let env = envelopes.into_iter().next().unwrap();
        (env.context, env.content)
    } else {
        let merged = crate::structured::merge_translations(envelopes, None)
            .expect("Failed to merge translations");
        (merged.context, merged.content)
    };

    let (profile, templates, custom_templates) = load_ubs_profile();
    let config = AemConfig::from_profile(&profile, templates, custom_templates, &ctx)
        .expect("Failed to create AemConfig");
    let config = crate::resolve_aem_languages(&content, &config);
    let root = convert_to_aem(&content, &config);

    (content, root, config)
}

/// Load the UBS XSD config from `profiles/ubs/xsd/`, including registered types.
pub fn load_ubs_xsd_config() -> crate::xsd::XsdConfig {
    let dir_path = profiles_path("ubs/xsd");
    let dir = std::path::Path::new(&dir_path);
    let config = crate::xsd::load_xsd_config_from_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to load UBS XSD config: {e}"));
    // Mirror `profiles::load_xsd_config`, which indexes every fragment library
    // so a `fragRef` from any of them resolves to its type.
    let fragment_types =
        crate::profiles::load_aem_fragments("ubs", "/content/dam/formsanddocuments/", &[])
            .map(|frags| {
                frags
                    .into_iter()
                    .map(|f| (f.frag_ref, f.xsd_type_name))
                    .collect()
            })
            .unwrap_or_default();
    config.with_fragment_types(fragment_types)
}

/// Recursively walk an AemNode tree, calling `callback` on every node.
pub fn walk_aem_nodes(node: &crate::aem::AemNode, callback: &mut impl FnMut(&crate::aem::AemNode)) {
    callback(node);
    match node {
        crate::aem::AemNode::Root { children, .. }
        | crate::aem::AemNode::Panel { children, .. }
        | crate::aem::AemNode::Repeatable { children, .. } => {
            for child in children {
                walk_aem_nodes(child, callback);
            }
        }
        _ => {}
    }
}

/// Count `AemNode::Fragment` nodes in the tree and collect their details.
pub fn collect_aem_fragment_refs(root: &crate::aem::AemNode) -> Vec<(String, Option<String>)> {
    let mut fragments = Vec::new();
    walk_aem_nodes(root, &mut |node| {
        if let crate::aem::AemNode::Fragment {
            frag_ref, bind_ref, ..
        } = node
        {
            fragments.push((frag_ref.clone(), bind_ref.clone()));
        }
    });
    fragments
}

// ============================================================================
// Redacto helpers
// ============================================================================

/// Build a `RedactoConfig` for tests with fixed identity fields and a fixed
/// `created` timestamp, so only the random UUIDs vary between runs.
///
/// The document carries no page furniture, so the header and footer sections
/// stay empty and the asset rows are exactly the content blocks. Use
/// [`test_redacto_config_with_furniture`] to exercise the furniture sections.
pub fn test_redacto_config(languages: &[&str]) -> crate::redacto::RedactoConfig {
    crate::redacto::RedactoConfig {
        document_id: "test_001".into(),
        title: "TEST_001".into(),
        form_path: "/content/forms/af/redacto-documents/test_001".into(),
        style: "default.css".into(),
        headers: std::collections::BTreeMap::new(),
        footers: std::collections::BTreeMap::new(),
        owner_id: "admin".into(),
        schema: "redacto-document/v2".into(),
        status: crate::redacto::Status::Draft,
        grid_panel_style: "layout-split-block".into(),
        footnote_panel_style: "footnote".into(),
        column_panel_style: "layout-split".into(),
        languages: languages.iter().map(|l| l.to_string()).collect(),
        master_language: languages.first().copied().unwrap_or("en").to_string(),
        created: "2026-01-01 00:00:00.000".into(),
    }
}

/// Like [`test_redacto_config`] but with per-language page header text and
/// footer fields, so the furniture sections are populated.
///
/// `footers` is `(language, fields)` where each field is `(class, value)`,
/// mirroring the profile's `footer_fields` shape once resolved.
pub fn test_redacto_config_with_furniture(
    languages: &[&str],
    headers: &[(&str, &str)],
    footers: &[(&str, &[(&str, &str)])],
) -> crate::redacto::RedactoConfig {
    let header_map: std::collections::BTreeMap<String, String> = headers
        .iter()
        .map(|(lang, text)| (lang.to_string(), text.to_string()))
        .collect();
    let footer_map: std::collections::BTreeMap<String, Vec<crate::redacto::FooterField>> =
        footers
            .iter()
            .map(|(lang, fields)| {
                let fields = fields
                    .iter()
                    .map(|(class, value)| crate::redacto::FooterField {
                        class: class.to_string(),
                        value: value.to_string(),
                    })
                    .collect();
                (lang.to_string(), fields)
            })
            .collect();
    crate::redacto::RedactoConfig {
        headers: header_map,
        footers: footer_map,
        ..test_redacto_config(languages)
    }
}

/// Load `profiles/ubs/redacto/config.toml` from disk and resolve it against
/// `ctx` as the only language variant (mirrors [`load_ubs_profile`]).
pub fn load_ubs_redacto_config(ctx: &crate::Context) -> crate::redacto::RedactoConfig {
    load_ubs_redacto_config_for(ctx, std::slice::from_ref(ctx))
}

/// Like [`load_ubs_redacto_config`] but with an explicit set of language
/// variants, for asserting the per-language page header and footer.
pub fn load_ubs_redacto_config_for(
    master_ctx: &crate::Context,
    language_ctxs: &[crate::Context],
) -> crate::redacto::RedactoConfig {
    let path = profiles_path("ubs/redacto/config.toml");
    let toml_str =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
    let profile: crate::redacto::RedactoProfile =
        toml::from_str(&toml_str).expect("Failed to parse UBS redacto config.toml");
    crate::redacto::RedactoConfig::from_profile(&profile, master_ctx, language_ctxs)
        .expect("Failed to create RedactoConfig")
}

/// Run the pipeline for `pdfs` and produce the Redacto dump with `config`
/// (mirrors [`build_aem_test_output`]).
///
/// The config is passed in rather than loaded from the UBS profile: that
/// profile derives the document identity from XFA variables, which a plain
/// (non-XFA) text PDF does not have.
pub(super) fn build_redacto_test_dump(
    pdfs: &[(&str, &str)],
    config: &crate::redacto::RedactoConfig,
) -> crate::redacto::RedactoDump {
    let envelopes: Vec<_> = pdfs
        .iter()
        .map(|(file, lang)| {
            crate::run_exhaustive_to_envelope(input_path(file), lang)
                .unwrap_or_else(|e| panic!("Failed to process {file}: {e}"))
        })
        .collect();

    let content = if envelopes.len() == 1 {
        envelopes.into_iter().next().unwrap().content
    } else {
        crate::structured::merge_translations(envelopes, None)
            .expect("Failed to merge translations")
            .content
    };

    let config = crate::resolve_redacto_languages(&content, config);
    crate::redacto::generate_redacto_dump(&content, &config)
}

/// All `asset_version.content` strings for one language, in insertion order.
pub fn redacto_contents_for(dump: &crate::redacto::RedactoDump, lang: &str) -> Vec<String> {
    dump.asset_versions
        .iter()
        .filter(|v| v.language == lang)
        .map(|v| v.content.clone())
        .collect()
}

/// The single `documents` row of a dump.
pub fn redacto_configuration(
    dump: &crate::redacto::RedactoDump,
) -> &crate::redacto::RedactoConfiguration {
    assert_eq!(dump.documents.len(), 1, "expected exactly one document row");
    &dump.documents[0].configuration
}

/// Every section of a configuration in page order: first header, header, body,
/// footer.
fn redacto_sections(
    cfg: &crate::redacto::RedactoConfiguration,
) -> [&[crate::redacto::RedactoComponent]; 4] {
    [&cfg.first_header, &cfg.header, &cfg.body, &cfg.footer]
}

/// Flatten the body section into `"assetContainer(n)"` / `"styledPanel(style)"`
/// labels, depth-first, for order assertions.
pub fn flatten_redacto_body(cfg: &crate::redacto::RedactoConfiguration) -> Vec<String> {
    fn walk(components: &[crate::redacto::RedactoComponent], out: &mut Vec<String>) {
        for component in components {
            match component {
                crate::redacto::RedactoComponent::AssetContainer { assets, .. } => {
                    out.push(format!("assetContainer({})", assets.len()));
                }
                crate::redacto::RedactoComponent::StyledPanel {
                    style, components, ..
                } => {
                    out.push(format!("styledPanel({style})"));
                    walk(components, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&cfg.body, &mut out);
    out
}

/// Every asset reference in a configuration, section by section, depth-first.
pub fn redacto_referenced_assets(cfg: &crate::redacto::RedactoConfiguration) -> Vec<String> {
    fn walk(components: &[crate::redacto::RedactoComponent], out: &mut Vec<String>) {
        for component in components {
            match component {
                crate::redacto::RedactoComponent::AssetContainer { assets, .. } => {
                    out.extend(assets.iter().cloned());
                }
                crate::redacto::RedactoComponent::StyledPanel { components, .. } => {
                    walk(components, out)
                }
            }
        }
    }
    let mut out = Vec::new();
    for section in redacto_sections(cfg) {
        walk(section, &mut out);
    }
    out
}

/// Every asset reference in one furniture section.
pub fn redacto_section_assets(components: &[crate::redacto::RedactoComponent]) -> Vec<String> {
    components
        .iter()
        .flat_map(|component| match component {
            crate::redacto::RedactoComponent::AssetContainer { assets, .. } => assets.clone(),
            crate::redacto::RedactoComponent::StyledPanel { components, .. } => {
                redacto_section_assets(components)
            }
        })
        .collect()
}

/// The `asset_version` contents of the asset `reference` points at, keyed by
/// language.
pub fn redacto_versions_of(
    dump: &crate::redacto::RedactoDump,
    reference: &str,
) -> std::collections::BTreeMap<String, String> {
    let asset = dump
        .assets
        .iter()
        .find(|a| {
            crate::redacto::asset_ref(&a.asset_id, crate::redacto::INITIAL_VERSION) == reference
        })
        .unwrap_or_else(|| panic!("no asset row for reference '{reference}'"));
    dump.asset_versions
        .iter()
        .filter(|v| v.asset_fk_id == asset.id)
        .map(|v| (v.language.clone(), v.content.clone()))
        .collect()
}
