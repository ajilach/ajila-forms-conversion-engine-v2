//! AEM FileVault ZIP package writer.
//!
//! Wraps a generated AEM Forms `.content.xml` in a complete Apache Jackrabbit
//! FileVault content package (ZIP) that can be uploaded directly to an AEM
//! instance via the Package Manager.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Write};

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, Event};
use uuid::Uuid;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::{AemConfig, AemNode};
use crate::aem::generate_aem_xml;
use crate::aem::template;
use crate::aem::xml_writer::reformat_attributes;

// ============================================================================
// Public API
// ============================================================================

/// The XSD for a finished AEM tree, or `None` when the profile does not bind to
/// one.
///
/// Derived from the node tree, so the schema always matches the `bindRef`s the
/// same walk assigns.
fn xsd_for_tree(root: &AemNode, config: &AemConfig) -> Option<String> {
    if !config.bind_to_xsd || config.xsd_path.is_none() {
        return None;
    }
    let xsd_config = config.xsd_config.as_ref()?;
    Some(crate::xsd::generate_xsd_string_from_aem(
        root,
        xsd_config,
        &config.fragments,
    ))
}

/// Generate a package from an [`AemNode`] tree with no form-content
/// translations: the tree carries only master-language strings, so only the
/// profile's `default_translations` are emitted.
pub fn generate_aem_package_from_node(root: &AemNode, config: &AemConfig) -> Vec<u8> {
    let xsd = xsd_for_tree(root, config);
    assemble_package(root, config, I18nDictionary::new(), xsd, None)
}

/// Like [`generate_aem_package_from_node`] but with an explicit form-content
/// translation dictionary (master-text → { lang → translation }), e.g. the
/// per-language labels edited in the AEM editor.
pub fn generate_aem_package_from_node_with_translations(
    root: &AemNode,
    config: &AemConfig,
    translations: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
) -> Vec<u8> {
    let xsd = xsd_for_tree(root, config);
    assemble_package(root, config, translations, xsd, None)
}

/// Like [`generate_aem_package_from_node_with_translations`] but uses a
/// pre-generated `.content.xml` string verbatim instead of rendering it from
/// the `AemNode` tree.
///
/// Used when the form XML has been hand-edited (expert mode): the supplied
/// `form_xml` becomes the package's `.content.xml`, while everything else
/// (XSD, translations, DAM metadata) is still derived from the node tree.
pub fn generate_aem_package_from_node_with_xml(
    root: &AemNode,
    config: &AemConfig,
    translations: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    form_xml: String,
) -> Vec<u8> {
    let xsd = xsd_for_tree(root, config);
    assemble_package(root, config, translations, xsd, Some(form_xml))
}

/// Like [`generate_aem_package_from_node_with_translations`] but re-emits each
/// node's captured fidelity [`Passthrough`](super::Passthrough) (raw attributes +
/// unmodeled child elements), keyed by node uuid, so saving a working tree that
/// was loaded from an existing package preserves what the typed model doesn't
/// represent. `passthrough` is empty for from-XFA trees (output identical).
pub fn generate_aem_package_from_node_with_passthrough(
    root: &AemNode,
    config: &AemConfig,
    translations: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    passthrough: &std::collections::HashMap<uuid::Uuid, super::Passthrough>,
) -> Vec<u8> {
    let form_xml =
        crate::aem::xml_writer::generate_aem_xml_with_passthrough(root, config, passthrough);
    let xsd = xsd_for_tree(root, config);
    assemble_package(root, config, translations, xsd, Some(form_xml))
}

/// Assemble the FileVault ZIP from a node tree plus pre-computed
/// form-content translations and optional XSD content.
fn assemble_package(
    root: &AemNode,
    config: &AemConfig,
    translations: I18nDictionary,
    xsd_content: Option<String>,
    custom_form_xml: Option<String>,
) -> Vec<u8> {
    let form_xml = custom_form_xml.unwrap_or_else(|| generate_aem_xml(root, config));
    let dam_xml = generate_dam_xml(config);

    let package_name = config.form_code.clone();

    let form_dir = config.form_dir();
    let form_jcr_path = format!("/content/forms/af/{}/{}", config.form_path, form_dir);
    let dam_jcr_path = format!(
        "/content/dam/formsanddocuments/{}/{}",
        config.form_path, form_dir
    );

    let mut filter_roots = vec![form_jcr_path.clone(), dam_jcr_path.clone()];

    // When binding to XSD, include the XSD path as a filter root so that CRX
    // actually installs the file (content outside filter roots is ignored).
    if config.bind_to_xsd
        && let Some(xsd_jcr_path) = config.xsd_ref()
    {
        filter_roots.push(xsd_jcr_path);
    }

    let buf = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut written: HashSet<String> = HashSet::new();

    // ── META-INF ────────────────────────────────────────────────────────
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "META-INF/MANIFEST.MF",
        &generate_manifest(&package_name, &filter_roots),
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "META-INF/vault/config.xml",
        VAULT_CONFIG,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "META-INF/vault/nodetypes.cnd",
        NODETYPES_CND,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "META-INF/vault/filter.xml",
        &generate_filter_xml(&filter_roots),
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "META-INF/vault/properties.xml",
        &generate_properties_xml(&package_name, &config.author),
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "META-INF/vault/definition/.content.xml",
        &generate_definition_xml(&package_name, &config.author, &filter_roots),
    );

    // ── jcr_root boilerplate ────────────────────────────────────────────
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/.content.xml",
        JCR_ROOT_XML,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/.content.xml",
        CONTENT_XML,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/forms/.content.xml",
        FORMS_XML,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/forms/af/.content.xml",
        AF_XML,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/dam/.content.xml",
        DAM_XML,
    );
    write_entry(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/dam/formsanddocuments/.content.xml",
        FORMSANDDOCUMENTS_XML,
    );

    // ── Intermediate folder .content.xml files ──────────────────────────
    let path_segments: Vec<&str> = config.form_path.split('/').collect();
    // content/forms/af/<seg1>/<seg2>/.../<form_code>
    write_intermediate_folders(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/forms/af",
        &path_segments,
        false,
    );
    // content/dam/formsanddocuments/<seg1>/<seg2>/.../<form_code>
    write_intermediate_folders(
        &mut zip,
        &opts,
        &mut written,
        "jcr_root/content/dam/formsanddocuments",
        &path_segments,
        true,
    );

    // ── Form content .content.xml ───────────────────────────────────────
    let form_content_path = format!(
        "jcr_root/content/forms/af/{}/{}/.content.xml",
        config.form_path, form_dir
    );
    write_entry(&mut zip, &opts, &mut written, &form_content_path, &form_xml);

    // ── DAM asset .content.xml ──────────────────────────────────────────
    let dam_content_path = format!(
        "jcr_root/content/dam/formsanddocuments/{}/{}/.content.xml",
        config.form_path, form_dir
    );
    write_entry(&mut zip, &opts, &mut written, &dam_content_path, &dam_xml);

    // ── Translation dictionaries ────────────────────────────────────────
    let mut translations = translations;

    // Merge default translations from the profile (toolbar buttons, messages, etc.).
    // Form-content translations take precedence over defaults — but per language,
    // not per key: the content only carries the languages the form ships, so a
    // wholesale win left the profile's other locales without the key at all. A
    // form whose own label happens to read "Company" would knock `Company` out of
    // the French and Italian dictionaries the profile itself emits.
    for (key, lang_map) in &config.default_translations {
        let entry = translations.entry(key.clone()).or_default();
        for (lang, text) in lang_map {
            entry.entry(lang.clone()).or_insert_with(|| text.clone());
        }
    }

    // Fold every synonym code onto the language it is a synonym of, so content
    // detected as `es` and defaults configured under `sp` end up in one bucket
    // rather than in two half-filled dictionaries. Without this the synonym file
    // is written from whichever half was reached first and the other half is
    // silently dropped: the file exists, so nothing looks missing.
    let translations: I18nDictionary = translations
        .into_iter()
        .map(|(key, lang_map)| {
            let mut folded: HashMap<String, String> = HashMap::new();
            for (lang, text) in lang_map {
                let canonical = config.canonical_language(&lang);
                // A value already filed under the primary code wins: it is the
                // one the profile itself asked for.
                let already_primary = folded.contains_key(&canonical) && canonical != lang;
                if !already_primary {
                    folded.insert(canonical, text);
                }
            }
            (key, folded)
        })
        .collect();

    // Every Add button reads "<subject>" through one of the profile's patterns.
    // The subject is a title the dictionary may translate, but the composite is a
    // string of its own, so it needs its own entry or it stays in the master
    // language on every other locale's screen. A subject with no entry for a
    // language reads the same there (lowering records only real translations),
    // and still takes that language's word order. The dictionary is folded onto
    // canonical codes by now, so the languages are too.
    let mut translations = translations;
    let master = config.canonical_language(&config.master_language);
    for subject in crate::aem::xml_writer::collect_add_subjects(root).values() {
        let Some(master_label) = config.add_label(&master, subject) else {
            continue;
        };
        let subject_translations = translations.get(subject);
        let per_language: HashMap<String, String> = config
            .canonical_languages()
            .iter()
            .filter(|lang| **lang != master)
            .filter_map(|lang| {
                let translated_subject = subject_translations
                    .and_then(|by_lang| by_lang.get(lang))
                    .unwrap_or(subject);
                config
                    .add_label(lang, translated_subject)
                    .filter(|label| *label != master_label)
                    .map(|label| (lang.clone(), label))
            })
            .collect();
        if !per_language.is_empty() {
            // A form-content key of the same text wins: it was authored, this is
            // derived.
            translations.entry(master_label).or_insert(per_language);
        }
    }

    if !translations.is_empty() {
        let dict_base = format!(
            "jcr_root/content/forms/af/{}/{}/_jcr_content/guideContainer/assets/dictionary",
            config.form_path, form_dir
        );
        let basename = format!(
            "/content/forms/af/{}/{}/jcr:content/guideContainer/assets/dictionary",
            config.form_path, form_dir
        );

        // Collect all languages that have translations
        let mut languages = BTreeSet::<String>::new();
        for lang_map in translations.values() {
            languages.extend(lang_map.keys().cloned());
        }

        for lang in &languages {
            let entries: Vec<(String, String)> = translations
                .iter()
                .filter_map(|(master_text, lang_map)| {
                    lang_map
                        .get(lang.as_str())
                        .map(|translated| (master_text.clone(), translated.clone()))
                })
                .collect();

            if !entries.is_empty() {
                let dict_xml = generate_dictionary_xml(lang, &entries, &basename);
                let dict_path = format!("{}/{}.xml", dict_base, lang);
                write_entry(&mut zip, &opts, &mut written, &dict_path, &dict_xml);

                // Generate dictionary files for language synonyms with the same translations
                if let Some(synonyms) = config.language_synonyms.get(lang) {
                    for synonym in synonyms {
                        let syn_xml = generate_dictionary_xml(synonym, &entries, &basename);
                        let syn_path = format!("{}/{}.xml", dict_base, synonym);
                        write_entry(&mut zip, &opts, &mut written, &syn_path, &syn_xml);
                    }
                }
            }
        }
    }

    // ── XSD schema (when bind_to_xsd = true and xsd_path is set) ──────
    if config.bind_to_xsd
        && config.xsd_path.is_some()
        && let Some(xsd_content) = xsd_content.as_deref()
    {
        let xsd_zip_path = config.xsd_zip_path().unwrap();

        // Write intermediate .content.xml files for the XSD directory
        // segments that lie between the DAM base and the XSD file.
        let xsd_ref = config.xsd_ref().unwrap();
        let xsd_ref_trimmed = xsd_ref.trim_start_matches('/');
        let dam_base = "content/dam/formsanddocuments/";
        if let Some(rest) = xsd_ref_trimmed.strip_prefix(dam_base) {
            // rest = "afforms_xsd/AFForms/AF_TEST.xsd" → parent segments = ["afforms_xsd", "AFForms"]
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() > 1 {
                let dir_segments = &parts[..parts.len() - 1];
                write_intermediate_folders(
                    &mut zip,
                    &opts,
                    &mut written,
                    "jcr_root/content/dam/formsanddocuments",
                    dir_segments,
                    true,
                );
            }
        }

        write_entry(&mut zip, &opts, &mut written, &xsd_zip_path, xsd_content);
    }

    zip.finish().expect("finalize zip").into_inner()
}

// ============================================================================
// Helpers
// ============================================================================

fn write_entry(
    zip: &mut ZipWriter<Cursor<Vec<u8>>>,
    opts: &SimpleFileOptions,
    written: &mut HashSet<String>,
    path: &str,
    content: &str,
) {
    if !written.insert(path.to_string()) {
        return;
    }
    zip.start_file(path, *opts).expect("zip start_file");
    zip.write_all(content.as_bytes()).expect("zip write");
}

/// Write intermediate folder `.content.xml` files for each segment.
/// DAM folders use `sling:Folder` with `lcFolder`/`type` attributes;
/// forms folders use `sling:OrderedFolder`.
fn write_intermediate_folders(
    zip: &mut ZipWriter<Cursor<Vec<u8>>>,
    opts: &SimpleFileOptions,
    written: &mut HashSet<String>,
    base: &str,
    segments: &[&str],
    is_dam: bool,
) {
    let mut current = base.to_string();
    for seg in segments {
        current = format!("{}/{}", current, seg);
        let path = format!("{}/.content.xml", current);
        if is_dam {
            write_entry(zip, opts, written, &path, DAM_FOLDER_XML);
        } else {
            write_entry(zip, opts, written, &path, ORDERED_FOLDER_XML);
        }
    }
}

// ============================================================================
// DAM Asset XML generation
// ============================================================================

/// Generate the DAM `.content.xml` using a Tera template if available in the
/// profile (`dam.xml`), otherwise fall back to the hard-coded builder.
fn generate_dam_xml(config: &AemConfig) -> String {
    if let Some(dam_template) = config.component_templates.get("dam") {
        let mut ctx = tera::Context::new();
        ctx.insert("xfa", &config.xfa_vars);
        ctx.insert("variables", &config.user_vars);
        ctx.insert("author", &config.author);
        ctx.insert("master_language", &config.master_language);
        // The canonical codes, not the detected ones: a language that reached
        // the tree under a synonym (`es`) must be named on the form under the
        // code the platform files it as (`sp`).
        ctx.insert("languages", &config.canonical_languages().join(","));
        ctx.insert("expanded_languages", &config.expand_languages().join(","));
        ctx.insert("form_code", &config.form_code);
        ctx.insert("bind_to_xsd", &config.bind_to_xsd);
        // Advertise the schema only when the package actually binds to one.
        // `xsd_path` names where the schema *would* live, which is needed to
        // build a bound package on demand; a package built without binding must
        // not claim `formmodel="xsd"` and point at a file it does not contain.
        let xsd_ref = config
            .bind_to_xsd
            .then(|| config.xsd_ref())
            .flatten()
            .unwrap_or_default();
        ctx.insert("xsd_ref", &xsd_ref);

        match template::render_string(dam_template, &ctx) {
            Ok(rendered) => return reformat_attributes(&rendered),
            Err(e) => {
                log::error!("Failed to render dam.xml template: {}", e);
                // fall through to hard-coded generator
            }
        }
    }

    generate_dam_asset_xml(config)
}

/// Generate the `dam:Asset` `.content.xml` for the DAM entry of the form.
fn generate_dam_asset_xml(config: &AemConfig) -> String {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = Writer::new_with_indent(&mut buf, b' ', 4);

        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .unwrap();

        // <jcr:root>
        let mut root = BytesStart::new("jcr:root");
        root.push_attribute(("xmlns:sling", "http://sling.apache.org/jcr/sling/1.0"));
        root.push_attribute(("xmlns:fd", "http://www.adobe.com/aemfd/fd/1.0"));
        root.push_attribute(("xmlns:dam", "http://www.day.com/dam/1.0"));
        root.push_attribute(("xmlns:jcr", "http://www.jcp.org/jcr/1.0"));
        root.push_attribute(("xmlns:nt", "http://www.jcp.org/jcr/nt/1.0"));
        root.push_attribute(("jcr:primaryType", "dam:Asset"));
        w.write_event(Event::Start(root)).unwrap();

        // <jcr:content>
        let mut jcr_content = BytesStart::new("jcr:content");
        jcr_content.push_attribute(("jcr:primaryType", "dam:AssetContent"));
        jcr_content.push_attribute(("sling:resourceType", "fd/fm/af/render"));
        jcr_content.push_attribute(("guide", "1"));
        jcr_content.push_attribute(("type", "guide"));
        w.write_event(Event::Start(jcr_content)).unwrap();

        // <metadata>
        let mut meta = BytesStart::new("metadata");
        meta.push_attribute(("fd:version", "1.1"));
        meta.push_attribute(("jcr:mixinTypes", "[mix:created,mix:lastModified]"));
        meta.push_attribute(("jcr:primaryType", "nt:unstructured"));
        meta.push_attribute(("allowedRenderFormat", "HTML"));
        meta.push_attribute(("author", config.author.as_str()));
        meta.push_attribute(("availableInMobileApp", "{Boolean}false"));
        if !config.dor_template_ref.is_empty() {
            meta.push_attribute(("dorTemplateRef", config.dor_template_ref.as_str()));
        }
        meta.push_attribute(("dorType", config.dor_type.as_str()));
        let has_xsd_path = config.xsd_path.is_some();
        meta.push_attribute(("formmodel", if has_xsd_path { "xsd" } else { "none" }));
        if has_xsd_path {
            let xsd_ref = config.xsd_ref().unwrap();
            meta.push_attribute(("xsdRef", xsd_ref.as_str()));
        }
        meta.push_attribute(("hasCustomThumbnail", "{Boolean}false"));
        if !config.theme_ref.is_empty() {
            meta.push_attribute(("themeRef", config.theme_ref.as_str()));
        }
        meta.push_attribute(("title", config.form_title.as_str()));
        w.write_event(Event::Empty(meta)).unwrap();

        // </jcr:content>
        w.write_event(Event::End(BytesEnd::new("jcr:content")))
            .unwrap();
        // </jcr:root>
        w.write_event(Event::End(BytesEnd::new("jcr:root")))
            .unwrap();
    }

    let raw = String::from_utf8(buf.into_inner()).expect("UTF-8 dam xml");
    reformat_attributes(&raw)
}

// ============================================================================
// META-INF generators
// ============================================================================

fn generate_manifest(package_name: &str, roots: &[String]) -> String {
    // MANIFEST.MF has a 72-byte line limit. We use continuation lines
    // (starting with a single space) for long values.
    let roots_value = roots.join(",");
    let mut manifest = String::new();
    manifest.push_str("Manifest-Version: 1.0\r\n");
    write_manifest_entry(
        &mut manifest,
        "Content-Package-Id",
        &format!("fd/export:{}", package_name),
    );
    write_manifest_entry(&mut manifest, "Content-Package-Roots", &roots_value);
    write_manifest_entry(&mut manifest, "Content-Package-Type", "content");
    manifest.push_str("\r\n");
    manifest
}

/// Write a MANIFEST.MF entry, wrapping at 72 bytes with continuation lines.
fn write_manifest_entry(manifest: &mut String, key: &str, value: &str) {
    let line = format!("{}: {}", key, value);
    let bytes = line.as_bytes();
    if bytes.len() <= 72 {
        manifest.push_str(&line);
        manifest.push_str("\r\n");
    } else {
        // First line: up to 72 bytes
        let first = &bytes[..72];
        manifest.push_str(&String::from_utf8_lossy(first));
        manifest.push_str("\r\n");
        // Continuation lines: space + up to 71 bytes
        let mut pos = 72;
        while pos < bytes.len() {
            let end = (pos + 71).min(bytes.len());
            manifest.push(' ');
            manifest.push_str(&String::from_utf8_lossy(&bytes[pos..end]));
            manifest.push_str("\r\n");
            pos = end;
        }
    }
}

fn generate_filter_xml(roots: &[String]) -> String {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = Writer::new_with_indent(&mut buf, b' ', 4);

        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .unwrap();

        let mut ws = BytesStart::new("workspaceFilter");
        ws.push_attribute(("version", "1.0"));
        w.write_event(Event::Start(ws)).unwrap();

        for root in roots {
            let mut f = BytesStart::new("filter");
            f.push_attribute(("root", root.as_str()));
            w.write_event(Event::Empty(f)).unwrap();
        }

        w.write_event(Event::End(BytesEnd::new("workspaceFilter")))
            .unwrap();
    }
    String::from_utf8(buf.into_inner()).expect("UTF-8 filter xml")
}

fn generate_properties_xml(package_name: &str, author: &str) -> String {
    let now = crate::util::iso_now();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE properties SYSTEM "http://java.sun.com/dtd/properties.dtd">
<properties>
<comment>FileVault Package Properties</comment>
<entry key="packageType">content</entry>
<entry key="lastWrappedBy">{author}</entry>
<entry key="packageFormatVersion">2</entry>
<entry key="group">fd/export</entry>
<entry key="created">{now}</entry>
<entry key="lastModifiedBy">{author}</entry>
<entry key="buildCount">1</entry>
<entry key="lastWrapped">{now}</entry>
<entry key="version"></entry>
<entry key="dependencies"></entry>
<entry key="createdBy">{author}</entry>
<entry key="name">{package_name}</entry>
<entry key="lastModified">{now}</entry>
</properties>
"#
    )
}

fn generate_definition_xml(package_name: &str, author: &str, roots: &[String]) -> String {
    let now = crate::util::iso_now();
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = Writer::new_with_indent(&mut buf, b' ', 4);

        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .unwrap();

        let mut root_elem = BytesStart::new("jcr:root");
        root_elem.push_attribute(("xmlns:vlt", "http://www.day.com/jcr/vault/1.0"));
        root_elem.push_attribute(("xmlns:jcr", "http://www.jcp.org/jcr/1.0"));
        root_elem.push_attribute(("xmlns:nt", "http://www.jcp.org/jcr/nt/1.0"));
        root_elem.push_attribute(("jcr:primaryType", "vlt:PackageDefinition"));
        root_elem.push_attribute(("buildCount", "1"));
        root_elem.push_attribute(("group", "fd/export"));
        let created = format!("{{Date}}{}", now);
        root_elem.push_attribute(("jcr:created", created.as_str()));
        root_elem.push_attribute(("jcr:createdBy", author));
        root_elem.push_attribute(("jcr:lastModified", created.as_str()));
        root_elem.push_attribute(("jcr:lastModifiedBy", author));
        root_elem.push_attribute(("lastWrapped", created.as_str()));
        root_elem.push_attribute(("lastWrappedBy", author));
        root_elem.push_attribute(("name", package_name));
        root_elem.push_attribute(("version", ""));
        w.write_event(Event::Start(root_elem)).unwrap();

        // <filter>
        let mut filter_elem = BytesStart::new("filter");
        filter_elem.push_attribute(("jcr:primaryType", "nt:unstructured"));
        w.write_event(Event::Start(filter_elem)).unwrap();

        for (i, root) in roots.iter().enumerate() {
            let tag = format!("f{}", i);
            let mut f = BytesStart::new(tag.as_str());
            f.push_attribute(("jcr:primaryType", "nt:unstructured"));
            f.push_attribute(("mode", "replace"));
            f.push_attribute(("root", root.as_str()));
            f.push_attribute(("rules", "[]"));
            w.write_event(Event::Empty(f)).unwrap();
        }

        w.write_event(Event::End(BytesEnd::new("filter"))).unwrap();
        w.write_event(Event::End(BytesEnd::new("jcr:root")))
            .unwrap();
    }
    let raw = String::from_utf8(buf.into_inner()).expect("UTF-8 definition xml");
    reformat_attributes(&raw)
}

// ============================================================================
// Translations
// ============================================================================

/// A map of master-language text → { lang_code → translated_text }.
type I18nDictionary = HashMap<String, HashMap<String, String>>;

// ============================================================================
// Dictionary XML generation
// ============================================================================

/// Generate a Sling dictionary XML file for a single locale.
fn generate_dictionary_xml(locale: &str, entries: &[(String, String)], basename: &str) -> String {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = Writer::new_with_indent(&mut buf, b' ', 4);

        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .unwrap();

        // <jcr:root>
        let mut root = BytesStart::new("jcr:root");
        root.push_attribute(("xmlns:sling", "http://sling.apache.org/jcr/sling/1.0"));
        root.push_attribute(("xmlns:jcr", "http://www.jcp.org/jcr/1.0"));
        root.push_attribute(("xmlns:mix", "http://www.jcp.org/jcr/mix/1.0"));
        root.push_attribute(("xmlns:nt", "http://www.jcp.org/jcr/nt/1.0"));
        root.push_attribute(("jcr:language", locale));
        root.push_attribute(("jcr:mixinTypes", "[mix:language]"));
        root.push_attribute(("jcr:primaryType", "sling:Folder"));
        root.push_attribute(("sling:basename", basename));
        w.write_event(Event::Start(root)).unwrap();

        // Fixed namespace for deterministic UUIDs
        let ns = Uuid::NAMESPACE_URL;

        for (master_text, translated_text) in entries {
            let key = format!("fd_{}", master_text);

            // Deterministic element name from the key
            let uuid = Uuid::new_v5(&ns, key.as_bytes());
            let elem_name = format!("fd_{}", uuid.as_hyphenated());

            let mut entry = BytesStart::new(elem_name.as_str());
            entry.push_attribute(("jcr:mixinTypes", "[sling:Message]"));
            entry.push_attribute(("jcr:primaryType", "nt:folder"));
            entry.push_attribute(("sling:key", key.as_str()));
            entry.push_attribute(("sling:message", translated_text.as_str()));
            w.write_event(Event::Empty(entry)).unwrap();
        }

        w.write_event(Event::End(BytesEnd::new("jcr:root")))
            .unwrap();
    }

    let raw = String::from_utf8(buf.into_inner()).expect("UTF-8 dictionary xml");
    reformat_attributes(&raw)
}

// ============================================================================
// Static boilerplate content
// ============================================================================

const JCR_ROOT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0" xmlns:rep="internal"
    jcr:mixinTypes="[rep:AccessControllable,rep:RepoAccessControllable]"
    jcr:primaryType="rep:root"
    sling:resourceType="sling:redirect"
    sling:target="/index.html"/>
"#;

const CONTENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:cq="http://www.day.com/jcr/cq/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0" xmlns:rep="internal"
    jcr:mixinTypes="[rep:AccessControllable]"
    jcr:primaryType="sling:OrderedFolder">
    <rep:policy/>
    <dam/>
    <forms/>
</jcr:root>
"#;

const FORMS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    jcr:primaryType="sling:OrderedFolder">
    <af/>
</jcr:root>
"#;

const AF_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0" xmlns:rep="internal"
    jcr:mixinTypes="[rep:AccessControllable]"
    jcr:primaryType="sling:Folder"
    hidden="true"/>
"#;

const DAM_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0" xmlns:rep="internal"
    jcr:mixinTypes="[rep:AccessControllable]"
    jcr:primaryType="sling:Folder"/>
"#;

const FORMSANDDOCUMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0" xmlns:rep="internal"
    jcr:mixinTypes="[rep:AccessControllable]"
    jcr:primaryType="sling:Folder"
    hidden="true"/>
"#;

const ORDERED_FOLDER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    jcr:primaryType="sling:OrderedFolder"/>
"#;

const DAM_FOLDER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    jcr:primaryType="sling:Folder"
    lcFolder="{Long}0"
    type="lcFolder"/>
"#;

const VAULT_CONFIG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<vaultfs version="1.1">
    <aggregates>
        <aggregate type="file" title="File Aggregate"/>
        <aggregate type="filefolder" title="File/Folder Aggregate"/>
        <aggregate type="nodetype" title="Node Type Aggregate" />
        <aggregate type="full" title="Full Coverage Aggregate">
            <matches>
                <include nodeType="rep:AccessControl" respectSupertype="true" />
                <include nodeType="rep:Policy" respectSupertype="true" />
                <include nodeType="cq:Widget" respectSupertype="true" />
                <include nodeType="cq:EditConfig" respectSupertype="true" />
                <include nodeType="cq:WorkflowModel" respectSupertype="true" />
                <include nodeType="vlt:FullCoverage" respectSupertype="true" />
                <include nodeType="mix:language" respectSupertype="true" />
                <include nodeType="sling:OsgiConfig" respectSupertype="true" />
            </matches>
        </aggregate>
        <aggregate type="generic" title="Folder Aggregate">
            <matches>
                <include nodeType="nt:folder" respectSupertype="true" />
            </matches>
            <contains>
                <exclude isNode="true" />
            </contains>
        </aggregate>
        <aggregate type="generic" title="Default Aggregator" isDefault="true">
            <matches>
            </matches>
            <contains>
                <exclude nodeType="nt:hierarchyNode" respectSupertype="true" />
            </contains>
        </aggregate>
    </aggregates>
    <handlers>
        <handler type="folder"/>
        <handler type="file"/>
        <handler type="nodetype"/>
        <handler type="generic"/>
    </handlers>
</vaultfs>
"#;

const NODETYPES_CND: &str = r#"<'sling'='http://sling.apache.org/jcr/sling/1.0'>
<'cq'='http://www.day.com/jcr/cq/1.0'>
<'nt'='http://www.jcp.org/jcr/nt/1.0'>
<'jcr'='http://www.jcp.org/jcr/1.0'>
<'rep'='internal'>
<'dam'='http://www.day.com/dam/1.0'>
<'oak'='http://jackrabbit.apache.org/oak/ns/1.0'>
<'mix'='http://www.jcp.org/jcr/mix/1.0'>
<'fd'='http://www.adobe.com/aemfd/fd/1.0'>

[sling:Resource]
  mixin
  - sling:resourceType (string)

[cq:ClientLibraryFolder] > sling:Folder
  - dependencies (string) multiple
  - categories (string) multiple
  - embed (string) multiple
  - channels (string) multiple

[sling:Folder] > nt:folder
  - * (undefined) multiple
  - * (undefined)
  + * (nt:base) = sling:Folder version

[cq:Page] > nt:hierarchyNode
  orderable primaryitem jcr:content
  + jcr:content (nt:base) = nt:unstructured
  + * (nt:base) = nt:base version

[cq:Taggable]
  mixin
  - cq:tags (string) multiple

[sling:Message]
  mixin
  - sling:key (string)
  - sling:message (undefined)

[sling:OrderedFolder] > sling:Folder
  orderable
  + * (nt:base) = sling:OrderedFolder version

[cq:ReplicationStatus]
  mixin
  - cq:lastReplicatedBy (string) ignore
  - cq:lastPublished (date) ignore
  - cq:lastReplicationStatus (string) ignore
  - cq:lastPublishedBy (string) ignore
  - cq:lastReplicationAction (string) ignore
  - cq:lastReplicated (date) ignore

[rep:RepoAccessControllable]
  mixin
  + rep:repoPolicy (rep:Policy) protected ignore

[dam:Asset] > nt:hierarchyNode
  primaryitem jcr:content
  + jcr:content (dam:AssetContent) = dam:AssetContent
  + * (nt:base) = nt:base version

[dam:AssetContent] > nt:unstructured
  + metadata (nt:unstructured)
  + related (nt:unstructured)
  + renditions (nt:folder)

[oak:Resource] > mix:lastModified, mix:mimeType
  primaryitem jcr:data
  - jcr:data (binary) mandatory

[cq:PageContent] > cq:OwnerTaggable, cq:ReplicationStatus, mix:created, mix:title, nt:unstructured, sling:Resource, sling:VanityPath
  orderable
  - cq:lastModified (date)
  - cq:template (string)
  - pageTitle (string)
  - offTime (date)
  - hideInNav (boolean)
  - cq:lastModifiedBy (string)
  - onTime (date)
  - jcr:language (string)
  - cq:allowedTemplates (string) multiple
  - cq:designPath (string)
  - navTitle (string)

[cq:OwnerTaggable] > cq:Taggable
  mixin

[sling:VanityPath]
  mixin
  - sling:vanityPath (string) multiple
  - sling:redirect (boolean)
  - sling:vanityOrder (long)
  - sling:redirectStatus (long)

[sling:MessageEntry] > nt:hierarchyNode, sling:Message

[fd:xdp]
  mixin
  - fd:trusted (boolean)
"#;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xsd::{XsdConfig, XsdProfile};
    use std::io::Read;

    #[test]
    fn dam_asset_xml_has_correct_resource_type() {
        let mut config = AemConfig::test_default("TEST_FORM");
        config.form_title = "TEST_FORM".into();
        let xml = generate_dam_asset_xml(&config);
        assert!(
            xml.contains("jcr:primaryType=\"dam:Asset\""),
            "root must be dam:Asset"
        );
        assert!(
            xml.contains("jcr:primaryType=\"dam:AssetContent\""),
            "jcr:content must be dam:AssetContent"
        );
        assert!(
            xml.contains("sling:resourceType=\"fd/fm/af/render\""),
            "jcr:content must have sling:resourceType=fd/fm/af/render"
        );
        assert!(xml.contains("guide=\"1\""));
        assert!(xml.contains("type=\"guide\""));
    }

    #[test]
    fn dam_intermediate_folders_use_sling_folder() {
        assert!(
            DAM_FOLDER_XML.contains("sling:Folder"),
            "DAM folders must use sling:Folder"
        );
        assert!(
            DAM_FOLDER_XML.contains("lcFolder"),
            "DAM folders must have lcFolder attribute"
        );
        assert!(
            !DAM_FOLDER_XML.contains("OrderedFolder"),
            "DAM folders must NOT use sling:OrderedFolder"
        );
    }

    #[test]
    fn package_contains_dam_and_form_content() {
        let config = AemConfig::test_default("TEST");
        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };
        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        let mut found_form = false;
        let mut found_dam = false;
        let mut found_dam_folder = false;

        for i in 0..archive.len() {
            let entry = archive.by_index(i).unwrap();
            let name = entry.name().to_string();
            if name.contains("content/forms/af/") && name.ends_with("TEST/.content.xml") {
                found_form = true;
            }
            if name.contains("content/dam/formsanddocuments/")
                && name.ends_with("TEST/.content.xml")
            {
                found_dam = true;
            }
            if name.contains("content/dam/formsanddocuments/test/.content.xml") {
                found_dam_folder = true;
            }
        }

        assert!(found_form, "package must contain form .content.xml");
        assert!(found_dam, "package must contain DAM .content.xml");
        assert!(
            found_dam_folder,
            "package must contain DAM intermediate folder"
        );

        // Verify DAM intermediate folder uses sling:Folder
        let mut dam_folder = archive
            .by_name("jcr_root/content/dam/formsanddocuments/test/.content.xml")
            .expect("DAM folder entry");
        let mut dam_folder_xml = String::new();
        dam_folder.read_to_string(&mut dam_folder_xml).unwrap();
        assert!(
            dam_folder_xml.contains("sling:Folder"),
            "DAM folder must be sling:Folder, got: {}",
            dam_folder_xml
        );
        assert!(
            dam_folder_xml.contains("lcFolder"),
            "DAM folder must have lcFolder"
        );
    }

    #[test]
    fn custom_form_xml_is_used_verbatim() {
        let config = AemConfig::test_default("TEST");
        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };
        let sentinel = "<!-- HAND EDITED SENTINEL 12345 -->";
        let custom = format!("<?xml version=\"1.0\"?>\n{sentinel}\n");

        let zip_bytes = generate_aem_package_from_node_with_xml(
            &root,
            &config,
            std::collections::HashMap::new(),
            custom.clone(),
        );
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        let form_path = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .find(|n| n.contains("content/forms/af/") && n.ends_with("TEST/.content.xml"))
            .expect("form .content.xml entry");
        let mut form = archive.by_name(&form_path).unwrap();
        let mut form_xml = String::new();
        form.read_to_string(&mut form_xml).unwrap();
        assert_eq!(
            form_xml, custom,
            "injected form XML must be used verbatim, got: {form_xml}"
        );
    }

    #[test]
    fn package_uses_configured_xsd_zip_path() {
        let mut config = AemConfig::test_default("TEST");
        config.bind_to_xsd = true;
        config.xsd_config = Some(XsdConfig::from_profile(XsdProfile::default()));
        config.xsd_path =
            Some("/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd".into());

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };

        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        let mut names = Vec::new();
        for i in 0..archive.len() {
            let entry = archive.by_index(i).expect("zip entry by index");
            names.push(entry.name().to_string());
        }

        let expected = "jcr_root/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd";
        let legacy = "jcr_root/content/dam/formsanddocuments/test/path/AF_TEST/schema.xsd";

        assert!(
            names.iter().any(|n| n == expected),
            "package must contain configured xsd path '{}'. Entries: {:?}",
            expected,
            names
        );
        assert!(
            names.iter().all(|n| n != legacy),
            "package must not contain legacy xsd path '{}'. Entries: {:?}",
            legacy,
            names
        );
    }

    #[test]
    fn dam_asset_xml_uses_configured_xsd_ref() {
        let mut config = AemConfig::test_default("TEST_FORM");
        config.bind_to_xsd = true;
        config.xsd_path =
            Some("/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST_FORM.xsd".into());

        let xml = generate_dam_asset_xml(&config);

        assert!(
            xml.contains("formmodel=\"xsd\""),
            "DAM metadata should use xsd model when xsd_path is set"
        );
        assert!(
            xml.contains(
                "xsdRef=\"/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST_FORM.xsd\""
            ),
            "DAM metadata should reference configured xsd path, got: {}",
            xml
        );
    }

    #[test]
    fn dam_template_receives_resolved_xsd_ref() {
        let mut config = AemConfig::test_default("TEST");
        config.bind_to_xsd = true;
        config.xsd_path =
            Some("/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd".into());
        config.component_templates.insert(
            "dam".into(),
            "<jcr:root><jcr:content><metadata {% if bind_to_xsd %}xsdRef=\"{{ xsd_ref }}\"{% endif %}/></jcr:content></jcr:root>".into(),
        );

        let xml = generate_dam_xml(&config);

        assert!(
            xml.contains(
                "xsdRef=\"/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd\""
            ),
            "DAM template rendering should use resolved xsd_ref, got: {}",
            xml
        );
    }

    #[test]
    fn xsd_path_without_leading_slash_is_normalized() {
        let mut config = AemConfig::test_default("TEST");
        config.bind_to_xsd = true;
        config.xsd_config = Some(XsdConfig::from_profile(XsdProfile::default()));
        config.xsd_path =
            Some("content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd".into());

        let xml = generate_dam_asset_xml(&config);
        assert!(
            xml.contains(
                "xsdRef=\"/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd\""
            ),
            "xsdRef should be normalized with leading slash, got: {}",
            xml
        );

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };
        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        archive
            .by_name("jcr_root/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd")
            .expect("normalized xsd path should be present in zip");
    }

    #[test]
    fn xsd_filter_root_included_in_package() {
        use std::io::Read;

        let mut config = AemConfig::test_default("TEST");
        config.bind_to_xsd = true;
        config.xsd_config = Some(XsdConfig::from_profile(XsdProfile::default()));
        config.xsd_path =
            Some("/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd".into());

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };

        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        // filter.xml must include the XSD path as a root
        let mut filter_xml = String::new();
        archive
            .by_name("META-INF/vault/filter.xml")
            .expect("filter.xml")
            .read_to_string(&mut filter_xml)
            .unwrap();
        assert!(
            filter_xml.contains(
                "root=\"/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_TEST.xsd\""
            ),
            "filter.xml must include xsd path as filter root, got: {}",
            filter_xml
        );

        // Intermediate .content.xml files for XSD directories must exist
        archive
            .by_name("jcr_root/content/dam/formsanddocuments/afforms_xsd/.content.xml")
            .expect("afforms_xsd intermediate folder must exist");
        archive
            .by_name("jcr_root/content/dam/formsanddocuments/afforms_xsd/AFForms/.content.xml")
            .expect("AFForms intermediate folder must exist");
    }

    #[test]
    fn bind_to_xsd_without_xsd_path_omits_xsd_from_package() {
        use std::io::Read;

        let mut config = AemConfig::test_default("TEST");
        config.bind_to_xsd = true;
        config.xsd_config = Some(XsdConfig::from_profile(XsdProfile::default()));
        config.xsd_path = None; // no xsd_path

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };

        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        // No XSD file should be in the package
        let mut names = Vec::new();
        for i in 0..archive.len() {
            let entry = archive.by_index(i).expect("zip entry");
            names.push(entry.name().to_string());
        }
        assert!(
            !names.iter().any(|n| n.ends_with(".xsd")),
            "package must NOT contain any XSD file when xsd_path is empty. Entries: {:?}",
            names
        );

        // filter.xml must NOT contain an XSD filter root
        let mut filter_xml = String::new();
        archive
            .by_name("META-INF/vault/filter.xml")
            .expect("filter.xml")
            .read_to_string(&mut filter_xml)
            .unwrap();
        assert!(
            !filter_xml.contains("afforms_xsd"),
            "filter.xml must NOT reference xsd path when xsd_path is empty, got: {}",
            filter_xml
        );

        // DAM metadata must use formmodel="none" and no xsdRef
        let dam_xml = generate_dam_asset_xml(&config);
        assert!(
            dam_xml.contains("formmodel=\"none\""),
            "DAM metadata should use formmodel=none when xsd_path is empty, got: {}",
            dam_xml
        );
        assert!(
            !dam_xml.contains("xsdRef"),
            "DAM metadata should NOT include xsdRef when xsd_path is empty, got: {}",
            dam_xml
        );
    }

    #[test]
    fn default_translations_appear_in_package_dictionary() {
        use std::io::Read;

        let mut config = AemConfig::test_default("TEST");
        config.languages = vec!["en".into(), "de".into(), "fr".into()];
        config.master_language = "en".into();
        config.default_translations = {
            let mut map = HashMap::new();
            map.insert("Back".into(), {
                let mut lm = HashMap::new();
                lm.insert("de".into(), "Zurück".into());
                lm.insert("fr".into(), "Retour".into());
                lm
            });
            map.insert("Submit".into(), {
                let mut lm = HashMap::new();
                lm.insert("de".into(), "Absenden".into());
                lm.insert("fr".into(), "Soumettre".into());
                lm
            });
            map
        };

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };

        // No form content — only default translations should appear
        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        let dict_base = format!(
            "jcr_root/content/forms/af/{}/AF_TEST/_jcr_content/guideContainer/assets/dictionary",
            config.form_path
        );

        // German dictionary must exist and contain toolbar translations
        let de_path = format!("{}/de.xml", dict_base);
        let mut de_xml = String::new();
        archive
            .by_name(&de_path)
            .unwrap_or_else(|_| panic!("German dictionary must exist at {}", de_path))
            .read_to_string(&mut de_xml)
            .unwrap();
        assert!(
            de_xml.contains("sling:key=\"fd_Back\""),
            "German dictionary must contain 'Back' key, got: {}",
            de_xml
        );
        assert!(
            de_xml.contains("sling:message=\"Zurück\""),
            "German dictionary must contain 'Zurück' translation, got: {}",
            de_xml
        );
        assert!(
            de_xml.contains("sling:key=\"fd_Submit\""),
            "German dictionary must contain 'Submit' key, got: {}",
            de_xml
        );

        // French dictionary must also exist
        let fr_path = format!("{}/fr.xml", dict_base);
        let mut fr_xml = String::new();
        archive
            .by_name(&fr_path)
            .unwrap_or_else(|_| panic!("French dictionary must exist at {}", fr_path))
            .read_to_string(&mut fr_xml)
            .unwrap();
        assert!(
            fr_xml.contains("sling:message=\"Retour\""),
            "French dictionary must contain 'Retour' translation, got: {}",
            fr_xml
        );
    }

    /// A profile default must reach every locale the profile has one for, even
    /// when the form authored the same text in some of its own languages.
    ///
    /// The content used to win per key rather than per language: a form issued in
    /// DE/EN/SP whose own label reads "Company" carries translations for those
    /// languages only, and that entry then shadowed the profile's French and
    /// Italian ones — so `Company` was missing from the fr and it dictionaries
    /// the profile itself emits (feedback PROBLEM-default-translations).
    #[test]
    fn a_profile_default_fills_the_locales_the_form_does_not_translate() {
        use std::io::Read;

        let mut config = AemConfig::test_default("TEST");
        config.languages = vec!["en".into(), "de".into(), "fr".into()];
        config.master_language = "en".into();
        config.default_translations = {
            let mut map = HashMap::new();
            map.insert("Company".into(), {
                let mut lm = HashMap::new();
                lm.insert("de".into(), "Firma".into());
                lm.insert("fr".into(), "Société".into());
                lm
            });
            map
        };

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };
        // The form authored the same master text, but only in its own languages.
        let mut content: I18nDictionary = HashMap::new();
        content.insert("Company".into(), {
            let mut lm = HashMap::new();
            lm.insert("de".into(), "Firma AG".into());
            lm
        });

        let zip_bytes = generate_aem_package_from_node_with_translations(&root, &config, content);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("valid zip");
        let dict_base = format!(
            "jcr_root/content/forms/af/{}/AF_TEST/_jcr_content/guideContainer/assets/dictionary",
            config.form_path
        );
        let read = |archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, locale: &str| {
            let path = format!("{}/{}.xml", dict_base, locale);
            let mut xml = String::new();
            archive
                .by_name(&path)
                .unwrap_or_else(|_| panic!("{} dictionary must exist at {}", locale, path))
                .read_to_string(&mut xml)
                .unwrap();
            xml
        };

        // The profile fills the language the form left out …
        assert!(
            read(&mut archive, "fr").contains("sling:message=\"Société\""),
            "the French default must survive the form's own entry"
        );
        // … and the form still wins for the language it did translate.
        assert!(
            read(&mut archive, "de").contains("sling:message=\"Firma AG\""),
            "the form's own translation must take precedence"
        );
    }

    /// A bilingual (en master, de) config with the profile's Add patterns, and a
    /// repeatable whose Add button takes its subject from the panel title "Client".
    fn add_button_fixture() -> (AemConfig, AemNode) {
        let mut config = AemConfig::test_default("TEST");
        config.languages = vec!["en".into(), "de".into()];
        config.master_language = "en".into();
        config
            .add_label_patterns
            .insert("en".into(), "Add {subject}".into());
        config
            .add_label_patterns
            .insert("de".into(), "{subject} hinzufügen".into());

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![AemNode::Panel {
                uuid: Uuid::from_u128(1),
                name: "PN_Outer".into(),
                title: "Client".into(),
                children: vec![AemNode::Repeatable {
                    attrs: crate::aem::AemAttrs::default(),
                    visible: true,
                    uuid: Uuid::from_u128(2),
                    name: "RCP_1".into(),
                    title: String::new(),
                    children: vec![],
                    min_occur: 1,
                    max_occur: 5,
                    bind_ref: None,
                    frag_ref: None,
                }],
                is_page: false,
                attrs: crate::aem::AemAttrs::default(),
                visible: true,
                is_conditional: false,
                dor_num_cols: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                frag_ref: None,
            }],
        };
        (config, root)
    }

    fn dictionary_xml(zip_bytes: Vec<u8>, config: &AemConfig, locale: &str) -> String {
        use std::io::Read;

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("valid zip");
        let path = format!(
            "jcr_root/content/forms/af/{}/AF_TEST/_jcr_content/guideContainer/assets/dictionary/{locale}.xml",
            config.form_path
        );
        let mut xml = String::new();
        archive
            .by_name(&path)
            .unwrap_or_else(|_| panic!("the {locale} dictionary must exist at {}", path))
            .read_to_string(&mut xml)
            .unwrap();
        xml
    }

    /// The Add-button label is a string the form did not author, so it needs its
    /// own dictionary entry per language, built from the subject's translation and
    /// the profile's word order for that language.
    ///
    /// Without it the button reads the master language on every screen: German
    /// readers of a German form would see "Add client".
    #[test]
    fn the_add_button_label_is_translated_from_the_subject() {
        let (config, root) = add_button_fixture();
        // The subject's own translation, as lowering produces it.
        let mut content: I18nDictionary = HashMap::new();
        content.insert("Client".into(), {
            let mut lm = HashMap::new();
            lm.insert("de".into(), "Kunde".into());
            lm
        });

        let de_xml = dictionary_xml(
            generate_aem_package_from_node_with_translations(&root, &config, content),
            &config,
            "de",
        );
        assert!(
            de_xml.contains("sling:message=\"Kunde hinzufügen\""),
            "the German dictionary must translate the Add label in German word order, got: {}",
            de_xml
        );
    }

    /// A subject that reads the same in every language (a product name, say) has
    /// no dictionary entry, since lowering records only real translations. Its
    /// Add button still needs the German word order.
    #[test]
    fn the_add_button_label_is_translated_when_the_subject_is_not() {
        let (config, root) = add_button_fixture();

        let de_xml = dictionary_xml(
            generate_aem_package_from_node_with_translations(&root, &config, HashMap::new()),
            &config,
            "de",
        );
        assert!(
            de_xml.contains("sling:message=\"Client hinzufügen\""),
            "an untranslated subject must still get the German Add label, got: {}",
            de_xml
        );
    }

    /// A form detected as `es` keeps its Add label with the rest of its Spanish
    /// dictionary. The label is derived after the synonym codes are folded onto
    /// the profile's own (`sp`), so filing it under `es` would start a second,
    /// one-entry Spanish dictionary that takes the synonym file's place.
    #[test]
    fn an_add_label_in_a_synonym_locale_joins_its_dictionary() {
        let (mut config, root) = add_button_fixture();
        config.languages = vec!["en".into(), "es".into()];
        config.add_label_patterns.clear();
        config
            .add_label_patterns
            .insert("en".into(), "Add {subject}".into());
        config
            .add_label_patterns
            .insert("sp".into(), "Añadir {subject}".into());
        config
            .language_synonyms
            .insert("sp".into(), vec!["es".into()]);
        config.default_translations.insert(
            "Back".into(),
            HashMap::from([("sp".to_string(), "Atrás".to_string())]),
        );

        let package =
            generate_aem_package_from_node_with_translations(&root, &config, HashMap::new());
        for locale in ["sp", "es"] {
            let xml = dictionary_xml(package.clone(), &config, locale);
            assert!(
                xml.contains("sling:message=\"Atrás\"")
                    && xml.contains("sling:message=\"Añadir Client\""),
                "{locale} must carry both the default and the Add label, got: {xml}"
            );
        }
    }

    /// A language the document was detected under (`es`) and the same language as
    /// the profile files it (`sp`) must land in one dictionary, holding both the
    /// form's own translations and the profile's defaults.
    ///
    /// They did not. Detection yields ISO 639-1, the profile keys its defaults
    /// `sp` and declares `es` its synonym, and the writer bucketed by the raw
    /// code: `sp.xml` got the defaults, `es.xml` got the content, and the synonym
    /// pass could not repair `es.xml` because a file had already been written
    /// there. Nothing looked missing -- both files existed -- but every default
    /// UI string was absent from the one AEM actually serves (feedback
    /// PROBLEM-default-translations).
    #[test]
    fn a_synonym_locale_gets_both_the_content_and_the_default_translations() {
        use std::io::Read;

        let mut config = AemConfig::test_default("TEST");
        // As a Spanish source arrives from language detection.
        config.languages = vec!["en".into(), "es".into()];
        config.master_language = "en".into();
        config
            .language_synonyms
            .insert("sp".into(), vec!["es".into()]);
        // As the profile ships them.
        config.default_translations = {
            let mut map = HashMap::new();
            map.insert("Back".into(), {
                let mut lm = HashMap::new();
                lm.insert("sp".into(), "Atrás".into());
                lm
            });
            map
        };

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };
        // Form content translated into the detected code.
        let mut content: I18nDictionary = HashMap::new();
        content.insert("Client".into(), {
            let mut lm = HashMap::new();
            lm.insert("es".into(), "Cliente".into());
            lm
        });

        let zip_bytes = generate_aem_package_from_node_with_translations(&root, &config, content);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("valid zip");
        let dict_base = format!(
            "jcr_root/content/forms/af/{}/AF_TEST/_jcr_content/guideContainer/assets/dictionary",
            config.form_path
        );

        // Both the primary and the synonym file must be complete: AEM resolves
        // one of them and the profile does not say which.
        for locale in ["sp", "es"] {
            let path = format!("{}/{}.xml", dict_base, locale);
            let mut xml = String::new();
            archive
                .by_name(&path)
                .unwrap_or_else(|_| panic!("{} dictionary must exist at {}", locale, path))
                .read_to_string(&mut xml)
                .unwrap();
            assert!(
                xml.contains("sling:message=\"Atrás\""),
                "{} must carry the profile default, got: {}",
                locale,
                xml
            );
            assert!(
                xml.contains("sling:message=\"Cliente\""),
                "{} must carry the form content, got: {}",
                locale,
                xml
            );
        }
    }

    #[test]
    fn default_translations_generate_synonym_dictionaries() {
        use std::io::Read;

        let mut config = AemConfig::test_default("TEST");
        config.languages = vec!["en".into(), "de".into()];
        config.master_language = "en".into();
        // de → ["de-ch"] synonym is already set in test_default
        config.default_translations = {
            let mut map = HashMap::new();
            map.insert("Next".into(), {
                let mut lm = HashMap::new();
                lm.insert("de".into(), "Weiter".into());
                lm
            });
            map
        };

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };

        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("valid zip");

        let dict_base = format!(
            "jcr_root/content/forms/af/{}/AF_TEST/_jcr_content/guideContainer/assets/dictionary",
            config.form_path
        );

        // de-ch synonym dictionary must also be generated
        let de_ch_path = format!("{}/de-ch.xml", dict_base);
        let mut de_ch_xml = String::new();
        archive
            .by_name(&de_ch_path)
            .unwrap_or_else(|_| panic!("de-ch synonym dictionary must exist at {}", de_ch_path))
            .read_to_string(&mut de_ch_xml)
            .unwrap();
        assert!(
            de_ch_xml.contains("sling:message=\"Weiter\""),
            "de-ch synonym dictionary must contain the same translations as de, got: {}",
            de_ch_xml
        );
    }

    /// Regression test: when the XSD path shares a common prefix with the form
    /// path, `write_intermediate_folders` would previously try to add the same
    /// ZIP entry twice, causing an `InvalidArchive("Duplicate filename")` panic.
    #[test]
    fn package_no_duplicate_filenames_when_xsd_shares_form_path_prefix() {
        let mut config = AemConfig::test_default("TEST");
        config.bind_to_xsd = true;
        config.xsd_config = Some(XsdConfig::from_profile(XsdProfile::default()));
        // XSD path under the same "test/path" prefix as form_path
        config.xsd_path =
            Some("/content/dam/formsanddocuments/test/path/AF_TEST/schema.xsd".into());

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };

        // Must not panic
        let zip_bytes = generate_aem_package_from_node(&root, &config);
        let reader = std::io::Cursor::new(zip_bytes);
        let archive = zip::ZipArchive::new(reader).expect("valid zip");

        // Verify no duplicate names exist
        let mut names = std::collections::HashSet::new();
        for i in 0..archive.len() {
            let entry = archive.file_names().nth(i).unwrap().to_string();
            assert!(
                names.insert(entry.clone()),
                "duplicate ZIP entry found: {}",
                entry
            );
        }
    }
}
