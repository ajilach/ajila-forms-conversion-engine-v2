//! Package -> JSON decode: the inverse of this crate's `encode`.
//!
//! [`decode`] is the entry point, mirroring [`crate::encode`]: package
//! bytes in, a validated [`u2s_aem::model::ValidForm`] out. See
//! [`form`]'s own module doc for the scope of what this pass recognises
//! today (closing the loop on this crate's own `encode` output, not yet
//! full real-package fidelity).

pub mod form;
pub mod i18n;
pub mod zip;

use std::collections::BTreeMap;

use u2s_aem::model::{
    AemForm, DataModel, DorMode, FormMetadata, FormName, Language, Passthrough, RawJcrNode,
    ResourceType, ValidForm, XmlName,
};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Zip(#[from] zip::ZipOpenError),
    #[error(transparent)]
    Root(#[from] zip::RootError),
    #[error(transparent)]
    Entry(#[from] zip::EntryError),
    #[error(transparent)]
    Dictionary(#[from] i18n::DictionaryError),
    #[error(transparent)]
    Form(#[from] form::DecodeError),
    #[error("{0}")]
    Invalid(String),
    #[error("the decoded document does not satisfy AemForm::validate: {0:?}")]
    SemanticallyInvalid(Vec<u2s_aem::model::Violation>),
}

/// Decodes a FileVault content package back into a validated
/// [`u2s_aem::model::AemForm`] -- see this module's own doc for scope.
pub fn decode(bytes: &[u8]) -> Result<ValidForm, DecodeError> {
    let zip_files = zip::open_zip(bytes)?;
    let roots = zip::locate_roots(&zip_files)?;
    let form_tree = zip::parse_entry(&zip_files, &roots.form_content_xml_path)?;

    // `jcr:language`, a standard JCR/CQ property (confirmed against the
    // real fixture, on the page's own `jcr:content` node, the same
    // `mix:language` concept every dictionary file's own `jcr:language`
    // already uses) -- not an attribute this crate invented.
    let page_content = form_tree.child("jcr:content").ok_or_else(|| {
        DecodeError::Invalid("the form page carries no jcr:content".to_owned())
    })?;
    let master_raw = page_content.attr("jcr:language").ok_or_else(|| {
        DecodeError::Invalid("jcr:content carries no jcr:language".to_owned())
    })?;
    let master =
        Language::try_from(master_raw).map_err(|e| DecodeError::Invalid(format!("invalid jcr:language: {e}")))?;

    let guide_container = find_guide_container(&form_tree)
        .ok_or_else(|| DecodeError::Invalid("no guideContainer found in the form page".to_owned()))?;

    // `guideContainer`'s own attributes: the ones this crate's writer
    // always sets (consumed below, one at a time) leave `chrome` holding
    // exactly the real per-deployment remainder (`actionType`,
    // `dorTemplateRef`, `themeRef`, ...) -- see `FormMetadata::chrome`'s
    // own doc.
    let mut chrome_attrs: BTreeMap<String, String> = guide_container.attributes.iter().cloned().collect();
    chrome_attrs.remove("jcr:primaryType");
    chrome_attrs.remove("sling:resourceType");
    chrome_attrs.remove("guideNodeClass");
    chrome_attrs.remove("fd:version");
    let dor_raw = chrome_attrs.remove("dorType");

    let dor = match dor_raw.as_deref() {
        Some("generate") => DorMode::Generate,
        _ => DorMode::None,
    };

    let dictionary_dir = format!(
        "jcr_root/content/forms/af/{}/_jcr_content/guideContainer/assets/dictionary",
        relative_form_path(&roots)
    );
    let dictionaries = i18n::read_dictionaries(&zip_files, &dictionary_dir)?;
    let languages: std::collections::BTreeSet<Language> = dictionaries.keys().cloned().collect();

    let form_name = FormName::try_from(roots.form_name.clone())
        .map_err(|e| DecodeError::Invalid(format!("invalid form_name: {e}")))?;
    let folder_path = roots
        .folder_path
        .iter()
        .map(|s| XmlName::try_from(s.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| DecodeError::Invalid(format!("invalid folder_path segment: {e}")))?;

    let data_model = decode_data_model(&zip_files, &roots)?;

    let ctx = form::DecodeCtx { master: &master, dictionaries: &dictionaries };

    // The form page's own `jcr:content` carries the display title as a
    // standard CQ page property (`jcr:title="AABF"` in the real fixture) --
    // the same node `jcr:language` already came from. `write_form_xml`
    // always writes *some* `jcr:title` (`display_title` falls back to
    // `form_name` when `metadata.title` is `None`), so reading it back
    // unconditionally would turn every implicit, re-derivable `None` into
    // an explicit `Some` -- the same reasoning `clear_derivable_bind_refs`
    // already applies to `bind_ref`. An inline value exactly matching
    // `form_name` is therefore treated as the fallback, not a carried
    // title: re-encoding `None` reproduces the identical XML either way,
    // so nothing is lost by not carrying it explicitly.
    let title = page_content
        .attr("jcr:title")
        .filter(|value| *value != roots.form_name)
        .map(|value| ctx.resolve_i18n_text("/metadata/title", value))
        .transpose()?;

    let root_panel = guide_container
        .child("rootPanel")
        .ok_or_else(|| DecodeError::Invalid("guideContainer carries no rootPanel".to_owned()))?;

    // `rootPanel`'s own separate `layout` child -- present only when the
    // source has one (see `FormMetadata::root_panel_layout`'s own doc).
    // Not `mechanical`, so read back rather than assumed.
    let root_panel_layout = root_panel
        .child("layout")
        .and_then(|l| l.resource_type())
        .map(ResourceType::try_from)
        .transpose()
        .map_err(|e| DecodeError::Invalid(format!("invalid rootPanel layout resource type: {e}")))?;

    let items = root_panel
        .child("items")
        .ok_or_else(|| DecodeError::Invalid("rootPanel carries no items".to_owned()))?;

    // Not every child of `rootPanel/items` is a real wizard step: the real
    // fixture's own first one (`fragment_formmetadata`) is a hidden
    // (`visible="false"`), childless `fragRef` metadata carrier, not
    // navigable content -- `Page` models a wizard step specifically (see
    // its own doc), and "a page must have at least one child"
    // (`AemForm::validate`) is a real, meaningful check for one, so the
    // fix belongs here, not in weakening that check: a metadata sibling
    // like this is carried in `chrome.raw_children` instead of becoming a
    // (permanently invalid) empty `Page`.
    let mut pages = Vec::new();
    let mut extra_chrome_children = Vec::new();
    for page_node in &items.children {
        if is_non_page_metadata_sibling(page_node) {
            extra_chrome_children.push(form::to_raw_node(page_node));
            continue;
        }
        let page_path = format!("/pages/{}", pages.len());
        pages.push(form::decode_page(&page_path, page_node, &ctx)?);
    }

    let mut toolbar = Vec::new();
    if let Some(toolbar_node) = root_panel.child("toolbar") {
        // `toolbar`'s own `items` wrapper carries the actions; its sibling
        // `layout` is mechanical (see `write_toolbar`'s own doc) and
        // needs no decode.
        let toolbar_items = toolbar_node.child("items").ok_or_else(|| {
            DecodeError::Invalid("toolbar carries no items".to_owned())
        })?;
        for (index, action_node) in toolbar_items.children.iter().enumerate() {
            toolbar.push(form::decode_node(&format!("/metadata/toolbar/{index}"), action_node, &ctx)?);
        }
    }

    // `guideContainer`'s own remainder: every child beyond `rootPanel`
    // (`autoSaveInfo`, `signerInfo`, `view`, ...) and its own mechanical
    // `layout` (`defaultGuideLayout`, discarded the same way
    // `jcr:primaryType` is -- the writer always re-adds it).
    let mut chrome_children: Vec<RawJcrNode> = guide_container
        .children
        .iter()
        .filter(|c| c.tag_name != "rootPanel" && c.tag_name != "layout")
        .map(form::to_raw_node)
        .collect();
    chrome_children.extend(extra_chrome_children);

    let dam_chrome = decode_dam_chrome(&zip_files, &roots)?;

    let metadata = FormMetadata {
        form_name,
        title,
        master_language: master,
        languages,
        dor,
        data_model,
        toolbar,
        folder_path,
        root_panel_layout,
        chrome: Passthrough { raw_attributes: chrome_attrs, raw_children: chrome_children },
        dam_chrome,
    };

    let mut form = AemForm { metadata, pages };
    deduplicate_component_names(&mut form);
    clear_derivable_bind_refs(&mut form)?;
    form.validate().map_err(DecodeError::SemanticallyInvalid)
}

/// `AemForm::validate`'s own uniqueness check on `Common.name` is a real
/// invariant for agent-authored content -- it is what lets a `bindRef`
/// derivation and a rule script address a node by name unambiguously. A
/// real, human-authored package does not always satisfy it: the committed
/// fixture's own `AF_AABF.zip` contains a literal `panel`/`panel_copy`
/// pair (the second one's own name suggesting a copy-paste that never
/// renamed its internal components) whose descendants collide on names
/// like `RCP_d9cda7db`.
///
/// Rather than fail the whole decode over a naming collision in the
/// source (or weaken `AemForm::validate`'s own invariant, which the rest
/// of this system relies on staying strict for agent-authored documents),
/// every name beyond the first occurrence is disambiguated with a
/// deterministic numeric suffix before validation ever sees it. This is a
/// stated, accepted loss of exact fidelity for a document with this
/// specific defect -- re-encoding will not reproduce the original
/// duplicate name -- traded for "decode succeeds and the rest of the
/// document is intact" over "decode fails entirely". Walks the tree in
/// the same order `AemForm::validate` does (pages, then depth-first
/// children), so the *first* occurrence — the one most likely to be the
/// "real" one if only one is ever actually live — keeps its original
/// name.
fn deduplicate_component_names(form: &mut AemForm) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for page in &mut form.pages {
        dedup_names_in(&mut page.children, &mut seen);
    }
    for action in &mut form.metadata.toolbar {
        dedup_one_name(action, &mut seen);
        if let Some(children) = node_children_mut(action) {
            dedup_names_in(children, &mut seen);
        }
    }
}

fn dedup_names_in(nodes: &mut [u2s_aem::model::Node], seen: &mut std::collections::HashSet<String>) {
    for node in nodes.iter_mut() {
        dedup_one_name(node, seen);
        if let Some(children) = node_children_mut(node) {
            dedup_names_in(children, seen);
        }
    }
}

fn dedup_one_name(node: &mut u2s_aem::model::Node, seen: &mut std::collections::HashSet<String>) {
    let common = node.common_mut();
    let original = common.name.as_str().to_owned();
    if seen.insert(original.clone()) {
        return;
    }
    let mut suffix = 2u32;
    let unique = loop {
        let candidate = format!("{original}_dup{suffix}");
        if candidate.len() <= 64 && seen.insert(candidate.clone()) {
            break candidate;
        }
        suffix += 1;
    };
    if let Ok(name) = u2s_aem::model::ComponentName::try_from(unique) {
        common.name = name;
    }
}

/// A green-field form the Conversion Agent authors never sets
/// `Common.bind_ref` explicitly under
/// [`DataModel::XmlSchema`](u2s_aem::model::DataModel::XmlSchema) -- the
/// mapper derives one mechanically from tree position
/// (`crate::xsd::generate`) and the writer always emits *some* `bindRef`
/// value regardless of whether it came from `bind_ref` or that
/// derivation. Reading it straight back as a *carried* value would make
/// every round trip through this decoder turn an implicit, re-derivable
/// `None` into an explicit `Some`, which is not what "lossless" means
/// here -- the document is not meant to record a fact that is already a
/// pure function of its own structure. So: after decoding, clear
/// `bind_ref` on any node whose decoded value exactly matches what
/// `crate::xsd::generate` would derive for it, leaving `Some(...)` only
/// where the source's own value genuinely disagrees with that derivation
/// -- a real fixture's own `formmodel="none"` stray `bindRef`s, say, or a
/// value pinned by an explicit third argument to `xsd::generate` in a
/// future revision. Costs one extra `validate()`/`xsd::generate()` pass,
/// only under `DataModel::XmlSchema`.
fn clear_derivable_bind_refs(form: &mut AemForm) -> Result<(), DecodeError> {
    if !matches!(form.metadata.data_model, DataModel::XmlSchema { .. }) {
        return Ok(());
    }
    let for_derivation = form.clone().validate().map_err(DecodeError::SemanticallyInvalid)?;
    let xsd = crate::xsd::generate(&for_derivation);
    for page in &mut form.pages {
        clear_bind_refs_in(&mut page.children, &xsd.bind_refs);
    }
    Ok(())
}

fn clear_bind_refs_in(
    nodes: &mut [u2s_aem::model::Node],
    derived: &std::collections::HashMap<u2s_aem::model::ComponentName, String>,
) {
    for node in nodes {
        if let Some(children) = node_children_mut(node) {
            clear_bind_refs_in(children, derived);
        }
        let common = node.common_mut();
        if common.bind_ref.as_deref() == derived.get(&common.name).map(String::as_str) {
            common.bind_ref = None;
        }
    }
}

fn node_children_mut(node: &mut u2s_aem::model::Node) -> Option<&mut Vec<u2s_aem::model::Node>> {
    match node {
        u2s_aem::model::Node::Component { children, .. } => Some(children),
        _ => None,
    }
}

/// A structural heuristic (checked before attempting a full `decode_page`,
/// which would otherwise reject this shape via `AemForm::validate`'s own
/// "a page must have at least one child" check): a hidden, childless,
/// `fragRef`-carrying sibling of the real wizard steps under `rootPanel/
/// items` -- the exact shape the real fixture's own `fragment_formmetadata`
/// has. Matched narrowly (all three conditions, not just one) so an
/// ordinary hidden-but-content-bearing page (a legitimate agent-authored
/// use of `visible=false`, say a conditionally-shown step) is never
/// mistaken for metadata.
fn is_non_page_metadata_sibling(node: &crate::jcr::tree::JcrNode) -> bool {
    let hidden = node.attr("visible").is_some_and(|v| v.contains("false"));
    let has_frag_ref = node.attr("fragRef").is_some();
    let items_empty = node
        .child("items")
        .map(|items| items.children.is_empty())
        .unwrap_or(true);
    hidden && has_frag_ref && items_empty
}

fn find_guide_container(root: &crate::jcr::tree::JcrNode) -> Option<&crate::jcr::tree::JcrNode> {
    if root.tag_name == "guideContainer" {
        return Some(root);
    }
    for child in &root.children {
        if let Some(found) = find_guide_container(child) {
            return Some(found);
        }
    }
    None
}

/// `data_model` has no dedicated attribute anywhere in a written package;
/// its only observable trace is whether `schema.xsd` exists at all
/// (`crate::package::assemble` writes it conditionally on
/// `xsd_xml: Option<&str>`, which `crate::xsd::generate` only produces for
/// [`DataModel::XmlSchema`]). A minimal, mechanical read of just the root
/// element's own name -- not a full XSD parser, matching the low-fidelity,
/// mechanical nature of this crate's own XSD generation.
/// The `folder_path` + `form_name` relative path, joined -- must match
/// `crate::package::Paths::new`'s own construction exactly. The one place
/// this is assembled on the decode side, shared by every caller that needs
/// a package path under a form's own subtree.
fn relative_form_path(roots: &zip::FormRoots) -> String {
    let mut relative = roots.folder_path.iter().map(|s| format!("{s}/")).collect::<String>();
    relative.push_str(&roots.form_name);
    relative
}

/// The DAM asset's own `<metadata>` node's remainder, plus its
/// `jcr:content`'s own sibling children (`<dictionary>`, if present) --
/// see `FormMetadata::dam_chrome`'s own doc. `Default` (empty) when the
/// package carries no DAM asset at all, or that asset carries no
/// `<metadata>` node -- both real for a package this crate's own encoder
/// wrote before this field existed.
///
/// The real fixture's own `<dictionary>` child under the DAM asset lists a
/// *different*, often smaller, set of languages than the guideContainer's
/// own dictionary directory actually ships -- a genuine authoring-tool
/// inconsistency in the source content, not a decode defect (measured
/// directly against the fixture). Carrying it verbatim, rather than trying
/// to regenerate it from `metadata.languages`, is the only way to
/// reproduce that inconsistency rather than silently correcting it.
fn decode_dam_chrome(
    zip_files: &std::collections::HashMap<String, Vec<u8>>,
    roots: &zip::FormRoots,
) -> Result<Passthrough, DecodeError> {
    let Some(path) = &roots.dam_content_xml_path else {
        return Ok(Passthrough::default());
    };
    let Some(bytes) = zip_files.get(path) else {
        return Ok(Passthrough::default());
    };
    let xml = String::from_utf8_lossy(bytes);
    let dam_root = crate::jcr::tree::parse_jcr_xml(&xml)
        .map_err(|e| DecodeError::Invalid(format!("malformed DAM asset .content.xml: {e}")))?;
    let Some(jcr_content) = dam_root.child("jcr:content") else {
        return Ok(Passthrough::default());
    };

    let mut raw_attributes = std::collections::BTreeMap::new();
    if let Some(metadata_node) = jcr_content.child("metadata") {
        raw_attributes = metadata_node.attributes.iter().cloned().collect();
        // `write_dam_xml`'s own mechanical attributes -- always re-derived
        // from `AemForm` itself, never carried.
        for mechanical in [
            "jcr:primaryType",
            "fd:version",
            "allowedRenderFormat",
            "dorType",
            "formmodel",
            "hasCustomThumbnail",
            "title",
        ] {
            raw_attributes.remove(mechanical);
        }
    }

    let raw_children = jcr_content
        .children
        .iter()
        .filter(|c| c.tag_name != "metadata")
        .map(form::to_raw_node)
        .collect();

    Ok(Passthrough { raw_attributes, raw_children })
}

fn decode_data_model(
    zip_files: &std::collections::HashMap<String, Vec<u8>>,
    roots: &zip::FormRoots,
) -> Result<DataModel, DecodeError> {
    let xsd_path = format!(
        "jcr_root/content/forms/af/{}/schema.xsd",
        relative_form_path(roots)
    );
    let Some(bytes) = zip_files.get(&xsd_path) else {
        return Ok(DataModel::Unbound);
    };
    let xml = String::from_utf8_lossy(bytes);
    let needle = "<xs:element name=\"";
    let start = xml.find(needle).ok_or_else(|| {
        DecodeError::Invalid("schema.xsd carries no root xs:element".to_owned())
    })? + needle.len();
    let end = xml[start..]
        .find('"')
        .ok_or_else(|| DecodeError::Invalid("malformed schema.xsd root element".to_owned()))?;
    let root_element = XmlName::try_from(&xml[start..start + end])
        .map_err(|e| DecodeError::Invalid(format!("invalid XSD root element name: {e}")))?;
    Ok(DataModel::XmlSchema { root_element })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use u2s_aem::model::{I18nText, JcrValue, Node};

    /// `encode` a form, `decode` the package it produces, and assert the
    /// result is exactly the form we started with -- the closed-loop
    /// round trip this module's own doc describes. This is the actual
    /// deliverable proving `decode`/`encode` are real inverses of each
    /// other for the shape this pass recognises, not an aspiration.
    fn roundtrip(form: ValidForm) -> AemForm {
        let encoded = crate::encode(&form).expect("encode succeeds");
        let decoded = decode(&encoded.bytes).expect("decode succeeds");
        decoded.into_form()
    }

    #[test]
    fn a_single_text_field_round_trips() {
        let form = build_form(
            xml_schema("Root"),
            vec![a_page_named("Page1", vec![a_text_field("Name")])],
        );
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    /// `write_form_xml` always writes *some* `jcr:title` on the page
    /// (falling back to `form_name` when `metadata.title` is `None`), so a
    /// genuinely authored title -- one that differs from `form_name` --
    /// must still be told apart from that fallback on decode. Verified
    /// directly against the real fixture (`jcr:title="AABF"` vs
    /// `form_name="AF_AABF"`), reproduced here as a green-field case.
    #[test]
    fn a_title_that_differs_from_the_form_name_round_trips() {
        let mut form = build_form(
            xml_schema("Root"),
            vec![a_page_named("Page1", vec![a_text_field("Name")])],
        );
        let mut inner = form.into_form();
        inner.metadata.title = Some(I18nText::from_entries([(lang("en"), plain("Display Title"))]));
        form = inner.validate().expect("still valid");
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_multilingual_field_round_trips_every_language() {
        let mut field = a_text_field("Name");
        if let Node::TextField { field: fc, .. } = &mut field {
            fc.label = I18nText::from_entries([
                (lang("en"), plain("Name")),
                (lang("de"), plain("Vorname")),
                (lang("fr"), plain("Prenom")),
            ]);
        }
        let page = a_page_named("Page1", vec![field]);
        let metadata_languages: std::collections::BTreeSet<Language> =
            [lang("en"), lang("de"), lang("fr")].into_iter().collect();
        let form = AemForm {
            metadata: FormMetadata {
                form_name: u2s_aem::model::FormName::try_from("TestForm").unwrap(),
                title: None,
                master_language: lang("en"),
                languages: metadata_languages,
                dor: DorMode::None,
                data_model: xml_schema("Root"),
                toolbar: Vec::new(),
                folder_path: Vec::new(),
                root_panel_layout: None,
                chrome: Default::default(),
                dam_chrome: Default::default(),
            },
            pages: vec![page],
        }
        .validate()
        .expect("valid");
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    #[test]
    fn every_typed_leaf_kind_round_trips_in_one_form() {
        let page = a_page_named(
            "Page1",
            vec![
                a_text_field("AText"),
                a_dropdown("ADropdown", &[("a", "Option A"), ("b", "Option B")]),
                a_static_text("AStatic"),
            ],
        );
        let form = build_form(xml_schema("Root"), vec![page]);
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    #[test]
    fn nested_components_and_a_toolbar_round_trip() {
        let inner = a_component("Inner", "fd/af/components/panel", vec![a_text_field("Leaf")]);
        let outer = a_component("Outer", "fd/af/components/panel", vec![inner]);
        let page = a_page_named("Page1", vec![outer]);
        let form = build_form_with_toolbar(
            xml_schema("Root"),
            vec![page],
            vec![
                a_component("prev", "fd/af/components/actions/previtemnav", Vec::new()),
                a_component("next", "fd/af/components/actions/nextitemnav", Vec::new()),
            ],
        );
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    #[test]
    fn an_unbound_form_round_trips_without_an_xsd() {
        let form = build_form(
            DataModel::Unbound,
            vec![a_page_named("Page1", vec![a_static_text("Intro")])],
        );
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_component_with_a_translatable_property_round_trips() {
        let mut panel = a_panel_with_title("Section", "Section Title", vec![a_text_field("Field1")]);
        if let Node::Component { properties, .. } = &mut panel {
            properties.insert(
                u2s_aem::model::JcrName::try_from("jcr:title").unwrap(),
                JcrValue::Text(I18nText::from_entries([
                    (lang("en"), plain("Section Title")),
                    (lang("de"), plain("Abschnittstitel")),
                ])),
            );
        }
        let page = a_page_named("Page1", vec![panel]);
        let metadata_languages: std::collections::BTreeSet<Language> =
            [lang("en"), lang("de")].into_iter().collect();
        let form = AemForm {
            metadata: FormMetadata {
                form_name: u2s_aem::model::FormName::try_from("TestForm").unwrap(),
                title: None,
                master_language: lang("en"),
                languages: metadata_languages,
                dor: DorMode::None,
                data_model: DataModel::Unbound,
                toolbar: Vec::new(),
                folder_path: Vec::new(),
                root_panel_layout: None,
                chrome: Default::default(),
                dam_chrome: Default::default(),
            },
            pages: vec![page],
        }
        .validate()
        .expect("valid");
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    /// Exercises `FormMetadata::folder_path`, `root_panel_layout` and
    /// `chrome` together -- the fields added to close the gap between the
    /// closed-loop round trip and a real, nested package like the
    /// committed fixture's own
    /// `afforms_germany_all/af_aa/AF_AABF`.
    #[test]
    fn nested_folder_path_root_panel_layout_and_chrome_round_trip() {
        let mut chrome_attrs = std::collections::BTreeMap::new();
        chrome_attrs.insert("themeRef".to_owned(), "/content/dam/formsanddocuments-themes/example/standard-theme".to_owned());
        chrome_attrs.insert("redirect".to_owned(), "/content/forms/af/confirm".to_owned());
        let chrome = u2s_aem::model::Passthrough {
            raw_attributes: chrome_attrs,
            raw_children: vec![u2s_aem::model::RawJcrNode {
                tag_name: "autoSaveInfo".to_owned(),
                attributes: [("jcr:primaryType".to_owned(), "nt:unstructured".to_owned())]
                    .into_iter()
                    .collect(),
                children: Vec::new(),
            }],
        };
        let page = a_page_named("Page1", vec![a_text_field("Name")]);
        let form = AemForm {
            metadata: FormMetadata {
                form_name: u2s_aem::model::FormName::try_from("AF_AABF").unwrap(),
                title: None,
                master_language: lang("en"),
                languages: [lang("en")].into_iter().collect(),
                dor: DorMode::None,
                data_model: DataModel::Unbound,
                toolbar: Vec::new(),
                folder_path: vec![
                    u2s_aem::model::XmlName::try_from("afforms_germany_all").unwrap(),
                    u2s_aem::model::XmlName::try_from("af_aa").unwrap(),
                ],
                root_panel_layout: Some(
                    ResourceType::try_from("ajila-forms-customers/ajila-forms-ubs/layouts/panel/wizard")
                        .unwrap(),
                ),
                chrome,
                dam_chrome: Default::default(),
            },
            pages: vec![page],
        }
        .validate()
        .expect("valid");
        let original = form.form().clone();
        let decoded = roundtrip(form);
        assert_eq!(decoded, original);
    }

    /// The actual real-package milestone: decoding
    /// `tests/fixtures/AF_AABF.zip` -- a real, human-authored UBS package,
    /// not this crate's own encoder output -- succeeds and produces a
    /// structurally sensible, `AemForm::validate`-passing document. Not
    /// yet a full round-trip assertion (`canonical.rs` and the real-fixture
    /// `encode(decode(pkg))` comparison are later work -- see the design
    /// plan's own "still needs" list); this is the decode half proven on
    /// its own.
    #[test]
    fn the_real_fixture_package_decodes_successfully() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/AF_AABF.zip"),
        )
        .expect("the committed fixture");
        let form = decode(&bytes).expect("the real fixture decodes");
        assert_eq!(form.form().metadata.form_name.as_str(), "AF_AABF");
        assert_eq!(
            form.form().metadata.folder_path.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["afforms_germany_all", "af_aa"]
        );
        assert!(!form.form().pages.is_empty(), "the real fixture has real wizard steps");
        // Every configured language the real fixture ships a dictionary
        // for -- confirmed against the fixture directly in an earlier
        // session's own measurement.
        for code in ["de", "de-ch", "en", "es", "fr", "it", "sp"] {
            assert!(
                form.form().metadata.languages.contains(&lang(code)),
                "missing language {code}"
            );
        }
    }

    /// Half of the real-package round trip: the document
    /// [`the_real_fixture_package_decodes_successfully`] produces can be
    /// encoded back into a package at all (not yet asserted
    /// canonically/byte-identical to the source -- `canonical.rs` is
    /// later work). A crash or an `EncodeError` here would mean decode
    /// produced a document `encode` cannot actually round-trip, which
    /// would make the decoded document a dead end rather than something
    /// the Conversion Agent could ever edit and re-deliver.
    #[test]
    fn the_decoded_real_fixture_encodes_back_into_a_package() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/AF_AABF.zip"),
        )
        .expect("the committed fixture");
        let form = decode(&bytes).expect("the real fixture decodes");
        let encoded = crate::encode(&form).expect("the decoded document encodes back");
        assert!(!encoded.bytes.is_empty());
        // The re-encoded bytes must themselves be a valid ZIP a decode
        // pass can open -- proves the round trip is not merely "produces
        // *some* bytes" but "produces a package shaped like one this
        // crate's own decoder can still make sense of".
        let files = zip::open_zip(&encoded.bytes).expect("re-encoded bytes are a valid ZIP");
        assert!(!files.is_empty());
    }
}