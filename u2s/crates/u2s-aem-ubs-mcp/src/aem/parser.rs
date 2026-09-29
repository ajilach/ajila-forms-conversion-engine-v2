//! AEM ZIP Package Parser
//!
//! Parses AEM Adaptive Forms content packages (ZIP files) into the `AemNode`
//! tree representation. Handles:
//! - ZIP extraction and form content discovery
//! - JCR XML parsing (`.content.xml` files)
//! - Fragment resolution (inlining `fragRef` content from the ZIP)
//! - Translation extraction (Sling i18n dictionaries)
//! - Script extraction (`fd:scripts` and `fd:rules` JSON attributes)

use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use uuid::Uuid;

use super::{
    AemAttrs, AemI18nText, AemNode, AemOption, OptionAlignment, Passthrough, TextFieldKind,
};

// ============================================================================
// Public types
// ============================================================================

/// A parsed AEM script event extracted from `fd:scripts` or `fd:rules` JSON.
#[derive(Debug, Clone)]
pub struct AemScript {
    /// The event type (e.g. "Initialize", "Value Commit", "Click", "Calculate").
    pub event: String,
    /// The JavaScript source code.
    pub content: String,
    /// The field path this script is associated with.
    pub field: String,
    /// Whether the script is enabled.
    pub enabled: bool,
}

/// Translation data extracted from Sling i18n dictionaries.
///
/// Maps `sling:key` → `HashMap<language, sling:message>`.
#[derive(Debug, Clone, Default)]
pub struct TranslationData {
    /// Maps dictionary key → (language → translated message).
    pub entries: HashMap<String, HashMap<String, String>>,
}

/// A parsed visibility condition extracted from `fd:rules` `fd:visible` JSON.
///
/// Maps a target panel to its trigger field and the value that makes it visible.
#[derive(Debug, Clone)]
pub struct VisibilityCondition {
    /// The trigger field name (e.g. `"RB_GroupTipo"`).
    pub trigger_field: String,
    /// The value that makes the target panel visible.
    pub trigger_value: String,
}

/// Result of parsing an AEM ZIP package.
#[derive(Debug, Clone)]
pub struct ParsedAemPackage {
    /// The parsed AEM node tree.
    pub root: AemNode,
    /// Translation data from Sling dictionaries.
    pub translations: TranslationData,
    /// All scripts extracted from the form, keyed by component name.
    pub scripts: HashMap<String, Vec<AemScript>>,
    /// The detected master language.
    pub language: String,
    /// Detected form title.
    pub form_title: String,
    /// The `formrange_*` attributes of the form's metadata draw
    /// (`metadataTextDraw`) and its entities, first occurrence winning: the
    /// form's code, entity, languages, version and release date, as the
    /// profile wrote them from the source's XFA variables.
    pub metadata: BTreeMap<String, String>,
    /// Visibility conditions: panel name → condition that shows it.
    pub visibility_conditions: HashMap<String, VisibilityCondition>,
    /// Per-node fidelity passthrough (raw attrs + unmodeled children), keyed by
    /// the node uuid. Consumed by `aem_to_translated` to populate each
    /// `AemNodeTranslated` node's `passthrough`, so a load→save round-trip keeps
    /// every attribute the typed model doesn't represent. See [`Passthrough`].
    pub raw_by_uuid: HashMap<Uuid, Passthrough>,
    /// The sub-panels each fragment's Initialize rule hides
    /// (`hideAFHideDor(this.X)`), keyed by the fragment's uuid. The writer
    /// regenerates a fragment's scripts, so this is what of them a document keeps.
    pub fragment_init_hide: HashMap<Uuid, Vec<super::ComponentName>>,
}

// ============================================================================
// AEM ZIP detection
// ============================================================================

/// Check if the given bytes represent an AEM content package ZIP.
///
/// Returns `true` if the ZIP contains both `META-INF/vault/` (or similar
/// vault metadata) and `jcr_root/content/forms/af/` entries.
pub fn detect_aem_zip(bytes: &[u8]) -> bool {
    let cursor = std::io::Cursor::new(bytes);
    let Ok(mut archive) = zip::ZipArchive::new(cursor) else {
        return false;
    };

    let mut has_meta_inf = false;
    let mut has_forms_af = false;

    for i in 0..archive.len() {
        let Ok(file) = archive.by_index(i) else {
            continue;
        };
        let name = file.name().to_string();
        if name.starts_with("META-INF/") {
            has_meta_inf = true;
        }
        if name.contains("jcr_root/content/forms/af/")
            || name.contains("jcr_root/content/dam/formsanddocuments/")
        {
            has_forms_af = true;
        }
        if has_meta_inf && has_forms_af {
            return true;
        }
    }

    false
}

// ============================================================================
// ZIP parsing
// ============================================================================

/// Parse an AEM ZIP package into a `ParsedAemPackage`.
pub fn parse_aem_zip(bytes: &[u8]) -> Result<ParsedAemPackage, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("Invalid ZIP: {e}"))?;

    // Collect all file contents from the ZIP into memory for random access
    let mut zip_files: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("ZIP entry error: {e}"))?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|e| format!("Read error for {name}: {e}"))?;
        zip_files.insert(name, contents);
    }

    // Find the main form .content.xml
    let form_xml_path = find_form_content_xml(&zip_files)?;
    let form_xml = zip_files
        .get(&form_xml_path)
        .ok_or_else(|| format!("Form XML not found at {form_xml_path}"))?;

    // Parse the main form XML
    let xml_str = String::from_utf8_lossy(form_xml);
    let mut parse_ctx = ParseContext::new(&zip_files);
    let (mut root, form_title, language) = parse_form_xml(&xml_str, &mut parse_ctx)?;
    attach_show_conditions(&mut root, &parse_ctx.show_triggers);
    let metadata = form_metadata(&parse_jcr_xml(&xml_str)?);

    // Extract translations from dictionary files in the ZIP
    let mut translations = extract_translations(&zip_files);

    // Merge translations collected from resolved fragments
    for (key, lang_map) in parse_ctx.translations.entries {
        translations
            .entries
            .entry(key)
            .or_default()
            .extend(lang_map);
    }

    // Resolve visibility condition trigger values to option labels where possible.
    // E.g. if RB_GroupTipo has options [("1","Individual"),("2","Legal Entity")],
    // a condition with trigger_value="2" becomes trigger_value="Legal Entity".
    for cond in parse_ctx.visibility_conditions.values_mut() {
        if let Some(labels) = parse_ctx.option_labels.get(&cond.trigger_field)
            && let Some(label) = labels.get(&cond.trigger_value)
        {
            cond.trigger_value = label.clone();
        }
    }

    Ok(ParsedAemPackage {
        root,
        translations,
        scripts: parse_ctx.scripts,
        language,
        form_title,
        metadata,
        visibility_conditions: parse_ctx.visibility_conditions,
        raw_by_uuid: parse_ctx.raw_by_uuid,
        fragment_init_hide: parse_ctx.fragment_init_hide,
    })
}

/// Find the main form `.content.xml` inside the ZIP.
///
/// Looks for `.content.xml` files under `jcr_root/content/forms/af/` that
/// contain `guideContainer` (the root of an Adaptive Form).
fn find_form_content_xml(zip_files: &HashMap<String, Vec<u8>>) -> Result<String, String> {
    let mut candidates: Vec<String> = zip_files
        .keys()
        .filter(|path| {
            path.contains("jcr_root/content/forms/af/")
                && path.ends_with("/.content.xml")
                && !path.contains("/_jcr_content/")
                && !path.contains("/assets/")
                && !path.contains("/dictionary/")
        })
        .cloned()
        .collect();

    // Sort by depth (prefer shallower paths) then alphabetically
    candidates.sort_by(|a, b| {
        let depth_a = a.matches('/').count();
        let depth_b = b.matches('/').count();
        depth_a.cmp(&depth_b).then(a.cmp(b))
    });

    // Check each candidate for guideContainer content
    for path in &candidates {
        if let Some(content) = zip_files.get(path) {
            let xml_str = String::from_utf8_lossy(content);
            if xml_str.contains("guideContainer") || xml_str.contains("fd/af/components") {
                return Ok(path.clone());
            }
        }
    }

    // Fallback: also check .content-finished.xml files
    let mut finished_candidates: Vec<String> = zip_files
        .keys()
        .filter(|path| {
            path.contains("jcr_root/content/forms/af/") && path.ends_with("/.content-finished.xml")
        })
        .cloned()
        .collect();
    finished_candidates.sort_by(|a, b| {
        let depth_a = a.matches('/').count();
        let depth_b = b.matches('/').count();
        depth_a.cmp(&depth_b).then(a.cmp(b))
    });

    for path in &finished_candidates {
        if let Some(content) = zip_files.get(path) {
            let xml_str = String::from_utf8_lossy(content);
            if xml_str.contains("guideContainer") || xml_str.contains("fd/af/components") {
                return Ok(path.clone());
            }
        }
    }

    Err("No Adaptive Form .content.xml found in ZIP".into())
}

// ============================================================================
// XML parsing context
// ============================================================================

/// Context carried through the XML parsing process.
struct ParseContext<'a> {
    /// All files in the ZIP for fragment resolution.
    zip_files: &'a HashMap<String, Vec<u8>>,
    /// Counter for generating deterministic UUIDs.
    counter: u32,
    /// Collected scripts keyed by component name.
    scripts: HashMap<String, Vec<AemScript>>,
    /// Stack of fragment paths being resolved (cycle detection).
    fragment_stack: Vec<String>,
    /// Translation data accumulated from ZIP and resolved fragments.
    translations: TranslationData,
    /// Visibility conditions parsed from fd:visible rules: panel name → condition.
    visibility_conditions: HashMap<String, VisibilityCondition>,
    /// Option value→label mappings for radio buttons and dropdowns: field name → (value → label).
    option_labels: HashMap<String, HashMap<String, String>>,
    /// Fragment library paths whose dictionaries have already been loaded.
    loaded_library_dicts: std::collections::HashSet<String>,
    /// Per-node fidelity passthrough keyed by node uuid (see [`Passthrough`]).
    raw_by_uuid: HashMap<Uuid, Passthrough>,
    /// Every uuid handed out so far, so a recovered one is never handed out twice.
    issued: std::collections::HashSet<Uuid>,
    /// The conditional panels `conditional.xml` wrote, in document order, each
    /// with the (trigger field, value) pairs its show rule tests.
    show_triggers: Vec<(String, Vec<(String, String)>)>,
    /// See [`ParsedAemPackage::fragment_init_hide`].
    fragment_init_hide: HashMap<Uuid, Vec<super::ComponentName>>,
}

impl<'a> ParseContext<'a> {
    fn new(zip_files: &'a HashMap<String, Vec<u8>>) -> Self {
        Self {
            zip_files,
            counter: 0,
            scripts: HashMap::new(),
            fragment_stack: Vec::new(),
            translations: TranslationData::default(),
            visibility_conditions: HashMap::new(),
            option_labels: HashMap::new(),
            loaded_library_dicts: std::collections::HashSet::new(),
            raw_by_uuid: HashMap::new(),
            issued: std::collections::HashSet::new(),
            show_triggers: Vec::new(),
            fragment_init_hide: HashMap::new(),
        }
    }

    /// Record the fidelity passthrough for a converted node, keyed by its uuid:
    /// every attribute NOT in `consumed` (the names the converter reads into
    /// typed fields) plus every child element NOT in `skip_children` (which the
    /// writer regenerates). See [`Passthrough`].
    fn record_passthrough(
        &mut self,
        uuid: Uuid,
        node: &JcrNode,
        consumed: &[&str],
        skip_children: &[&str],
    ) {
        let raw_attributes = node
            .attributes
            .iter()
            .filter(|(k, _)| !consumed.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let raw_children = node
            .children
            .iter()
            .filter(|c| !skip_children.contains(&c.tag_name.as_str()) && !is_empty_placeholder(c))
            .map(serialize_jcr_node)
            .collect();
        self.raw_by_uuid.insert(
            uuid,
            Passthrough {
                raw_attributes,
                raw_children,
            },
        );
    }

    /// The node's uuid: the one the writer put in its tag (`panel_<uuid>`), so
    /// a package this crate wrote loads back with its own node identities, or
    /// a fresh one for a tag without it. A uuid already handed out gets a fresh
    /// one too: AEM's copy and paste duplicates a tag in another branch, and
    /// the passthrough is keyed by uuid.
    fn uuid_for(&mut self, node: &JcrNode, seed: &str) -> Uuid {
        let written = node
            .tag_name
            .rsplit_once('_')
            .map(|(_, suffix)| suffix)
            .filter(|suffix| suffix.len() == 32)
            .and_then(|suffix| Uuid::try_parse(suffix).ok());
        let uuid = match written {
            Some(uuid) if !self.issued.contains(&uuid) => uuid,
            _ => self.next_uuid(seed),
        };
        self.issued.insert(uuid);
        uuid
    }

    fn next_uuid(&mut self, seed: &str) -> Uuid {
        self.counter += 1;
        let input = format!("{seed}_{}", self.counter);
        Uuid::new_v5(
            &Uuid::from_bytes([
                0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
                0x30, 0xc8,
            ]),
            input.as_bytes(),
        )
    }
}

// ============================================================================
// JCR XML parsing
// ============================================================================

/// Parse the main form XML into an AemNode tree.
fn parse_form_xml(xml: &str, ctx: &mut ParseContext) -> Result<(AemNode, String, String), String> {
    // Use a SAX-like approach: build a tree of JCR nodes first, then convert to AemNode.
    let jcr_tree = parse_jcr_xml(xml)?;

    // Find the guideContainer node
    let guide_container = find_node_by_resource_type(&jcr_tree, "fd/af/components/guideContainer")
        .ok_or("No guideContainer found in form XML")?;

    // The form title is the rich-text `formTitle` draw; `jcr:title` is the
    // form code (`root.xml`). Older packages without the draw fall back to it.
    let form_title = find_node_by_name(&jcr_tree, "formTitle")
        .and_then(|draw| draw.attr("_value"))
        .map(|value| {
            value
                .trim()
                .strip_prefix("<p>")
                .and_then(|v| v.strip_suffix("</p>"))
                .unwrap_or(value)
                .to_string()
        })
        .or_else(|| {
            jcr_tree
                .attributes
                .iter()
                .find(|(k, _)| k == "jcr:title")
                .map(|(_, v)| v.clone())
                .or_else(|| {
                    jcr_tree.children.iter().find_map(|child| {
                        if child.tag_name == "jcr:content" {
                            child
                                .attributes
                                .iter()
                                .find(|(k, _)| k == "jcr:title")
                                .map(|(_, v)| v.clone())
                        } else {
                            None
                        }
                    })
                })
        })
        .unwrap_or_else(|| "Untitled".into());

    // Extract language
    let language = jcr_tree
        .children
        .iter()
        .find_map(|child| {
            if child.tag_name == "jcr:content" {
                child
                    .attributes
                    .iter()
                    .find(|(k, _)| k == "jcr:language")
                    .map(|(_, v)| v.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "en".into());

    // Find rootPanel inside guideContainer
    let root_panel = guide_container
        .children
        .iter()
        .find(|c| c.tag_name == "rootPanel")
        .ok_or("No rootPanel found in guideContainer")?;

    // Convert rootPanel to AemNode tree
    let children = convert_items_to_aem_nodes(root_panel, ctx)?;

    let root = AemNode::Root {
        title: form_title.clone(),
        children,
    };

    Ok((root, form_title, language))
}

// ============================================================================
// JCR XML tree
// ============================================================================

/// A parsed JCR XML node.
#[derive(Debug, Clone)]
struct JcrNode {
    tag_name: String,
    attributes: Vec<(String, String)>,
    children: Vec<JcrNode>,
}

impl JcrNode {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn resource_type(&self) -> Option<&str> {
        self.attr("sling:resourceType")
    }

    /// Get the `name` attribute (AEM component name used in scripts).
    fn component_name(&self) -> Option<&str> {
        self.attr("name")
    }
}

/// Child element tags the writer regenerates from typed fields, so they are
/// excluded from a node's `raw_children` passthrough (else they'd be emitted
/// twice): the fields' own `items`, the grid `layout`, and `cq:responsive`
/// (reproduced from `colspan`).
const REGENERATED_CHILD_TAGS: &[&str] = &["items", "layout", "cq:responsive"];

/// An empty `fd:rules` or `fd:scripts`: a placeholder the templates write
/// themselves wherever a node has none of its own, carrying nothing. Kept as
/// passthrough it would stop the template from writing its own and come back
/// in a different place.
fn is_empty_placeholder(node: &JcrNode) -> bool {
    matches!(node.tag_name.as_str(), "fd:rules" | "fd:scripts")
        && node.children.is_empty()
        && node.attributes.iter().all(|(k, _)| k == "jcr:primaryType")
}

/// Attributes the repeating-panel archetype owns on the inner repeating panel.
///
/// `profiles/<name>/aem/repeatable.xml` writes each of these unconditionally
/// (`addButton` / `removeButton` only when the panel is not a signature twin),
/// so a loaded value is regenerated on the way back out and must not be kept as
/// unmodeled passthrough. See `PROBLEM-repeating-panel` in the feedback repo.
const REPEATING_PANEL_ARCHETYPE_ATTRS: &[&str] = &[
    "accessibilityLabel",
    "addButton",
    "ajilaPanelSubject",
    "removeButton",
    "summaryHeadingLevel",
];

/// Re-serialize a [`JcrNode`] subtree to XML (the inverse of [`parse_jcr_xml`]).
/// Attribute values are re-escaped (they were XML-decoded on parse); empty vs
/// start/end element form is preserved. AEM `.content.xml` carries no element
/// text, so only attributes + children are emitted.
fn serialize_jcr_node(node: &JcrNode) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = write!(s, "<{}", node.tag_name);
    for (k, v) in &node.attributes {
        let _ = write!(s, " {}=\"{}\"", k, quick_xml::escape::escape(v));
    }
    if node.children.is_empty() {
        s.push_str("/>");
    } else {
        s.push('>');
        for c in &node.children {
            s.push_str(&serialize_jcr_node(c));
        }
        let _ = write!(s, "</{}>", node.tag_name);
    }
    s
}

/// Adaptive-form column span, read from the node's `cq:responsive/default@width`
/// (the AEM 12-column grid), defaulting to full width (12).
fn parse_colspan(node: &JcrNode) -> u32 {
    node.children
        .iter()
        .find(|c| c.tag_name == "cq:responsive")
        .and_then(|r| r.children.iter().find(|c| c.tag_name == "default"))
        .and_then(|d| d.attr("width"))
        .and_then(|w| w.parse().ok())
        .unwrap_or(12)
}

/// Document-of-Record column span (`@dorColspan`), if set.
fn parse_dor_colspan(node: &JcrNode) -> Option<u32> {
    node.attr("dorColspan").and_then(|v| v.parse().ok())
}

/// Parse JCR XML into a tree of JcrNode.
fn parse_jcr_xml(xml: &str) -> Result<JcrNode, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut stack: Vec<JcrNode> = Vec::new();
    let mut root: Option<JcrNode> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let mut attributes = Vec::new();
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                    let value = attr.unescape_value().unwrap_or_default().to_string();
                    attributes.push((key, value));
                }
                stack.push(JcrNode {
                    tag_name,
                    attributes,
                    children: Vec::new(),
                });
            }
            Ok(Event::Empty(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let mut attributes = Vec::new();
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                    let value = attr.unescape_value().unwrap_or_default().to_string();
                    attributes.push((key, value));
                }
                let node = JcrNode {
                    tag_name,
                    attributes,
                    children: Vec::new(),
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else {
                    root = Some(node);
                }
            }
            Ok(Event::End(_)) => {
                let node = stack.pop().ok_or("Mismatched XML end tag")?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else {
                    root = Some(node);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {} // skip text, comments, etc.
            Err(e) => return Err(format!("XML parse error: {e}")),
        }
    }

    root.ok_or_else(|| "Empty XML document".into())
}

/// Find a node by its `sling:resourceType` (searches recursively).
/// The first node, depth first, whose component `name` is `name`.
fn find_node_by_name<'a>(node: &'a JcrNode, name: &str) -> Option<&'a JcrNode> {
    if node.attr("name") == Some(name) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_node_by_name(child, name))
}

/// See [`ParsedAemPackage::metadata`].
fn form_metadata(tree: &JcrNode) -> BTreeMap<String, String> {
    fn collect(node: &JcrNode, out: &mut BTreeMap<String, String>) {
        for (key, value) in &node.attributes {
            if key.starts_with("formrange_") {
                out.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        node.children.iter().for_each(|child| collect(child, out));
    }
    let mut out = BTreeMap::new();
    if let Some(draw) = find_node_by_name(tree, "metadataTextDraw") {
        collect(draw, &mut out);
    }
    out
}

fn find_node_by_resource_type<'a>(node: &'a JcrNode, resource_type: &str) -> Option<&'a JcrNode> {
    if node.resource_type() == Some(resource_type) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_by_resource_type(child, resource_type) {
            return Some(found);
        }
    }
    None
}

// ============================================================================
// JcrNode → AemNode conversion
// ============================================================================

/// Convert the `items` children of a panel/rootPanel JcrNode into AemNode children.
fn convert_items_to_aem_nodes(
    panel: &JcrNode,
    ctx: &mut ParseContext,
) -> Result<Vec<AemNode>, String> {
    let mut children = Vec::new();

    // Find the `items` child node
    let items_node = panel.children.iter().find(|c| c.tag_name == "items");

    let items = match items_node {
        Some(items) => &items.children,
        None => &panel.children,
    };

    for child in items {
        // Skip non-component nodes (layout, fd:rules, fd:scripts, etc.)
        if child.tag_name == "layout"
            || child.tag_name.starts_with("fd:")
            || child.tag_name == "toolbar"
            || child.tag_name == "items"
        {
            continue;
        }

        if let Some(node) = convert_jcr_to_aem(child, ctx)? {
            children.push(node);
        }
    }

    // Also extract scripts from the panel's fd:scripts child
    if let Some(comp_name) = panel.component_name() {
        extract_scripts_from_node(panel, comp_name, ctx);
    }

    Ok(children)
}

/// Convert a single JcrNode to an AemNode based on its `sling:resourceType`.
fn convert_jcr_to_aem(node: &JcrNode, ctx: &mut ParseContext) -> Result<Option<AemNode>, String> {
    let resource_type = node.resource_type().unwrap_or("");
    let comp_name = node.component_name().unwrap_or(&node.tag_name);

    // Extract scripts from this node
    extract_scripts_from_node(node, comp_name, ctx);

    // Determine component type from resourceType suffix
    let type_suffix = resource_type.rsplit('/').next().unwrap_or(resource_type);

    match type_suffix {
        // `email`, `telephone` and `textboxMultiline` are single-line/multi-line
        // text inputs in the UBS component set. They carry data, so dropping
        // them loses form fields outright — they map onto TextField like any
        // other text input.
        "textbox" | "guideTextBox" | "textboxMultiline" | "email" | "telephone" => {
            Ok(Some(convert_textbox(node, ctx)))
        }
        "numericbox" | "guideNumericBox" => Ok(Some(convert_numberfield(node, ctx))),
        "datepicker" | "guideDatePicker" => Ok(Some(convert_datepicker(node, ctx))),
        "radiobutton" | "guideRadioButton" => Ok(Some(convert_radiobutton(node, ctx))),
        "checkbox" | "guideCheckBox" => Ok(Some(convert_checkbox(node, ctx))),
        "dropdownlist" | "guideDropDownList" => Ok(Some(convert_dropdown(node, ctx))),
        "textdraw" | "guideTextDraw" => Ok(Some(convert_textdraw(node, ctx))),
        // The UBS message box. Without its own arm it degraded to a textdraw,
        // which loses what makes it a notice: it would come back as ordinary
        // static text and land in the DoR.
        "messagebox" => Ok(Some(convert_messagebox(node, ctx))),
        "titledraw" | "guideTitleDraw" => Ok(Some(convert_titledraw(node, ctx))),
        // The UBS HTML component. It has no `items` child and no `fragRef`, so
        // without this arm it falls into the unknown `_ =>` arm below and the
        // whole block -- a table, a chart, an image -- is dropped from a loaded
        // package without a word.
        "htmlDisplayer" => Ok(Some(convert_html_displayer(node, ctx))),
        "panel" | "guidePanel" | "rootPanel" => convert_panel(node, ctx),
        // Fragment reference
        _ if node.attr("fragRef").is_some() => convert_fragment(node, ctx),
        // Buttons and other controls we skip
        "button" | "guideButton" | "tertiarybutton" | "secondarybutton" | "primarybutton" => {
            Ok(None)
        }
        // Signature, file upload, etc. — treat as textbox
        "signature" | "scribble" | "fileupload" => Ok(Some(convert_textbox(node, ctx))),
        // formtitle, guideheader — skip
        "formtitle" | "guideheader" | "aftemplatedpage" => Ok(None),
        // Unknown — try as panel if it has items children, otherwise skip
        _ => {
            if node.children.iter().any(|c| c.tag_name == "items") {
                convert_panel(node, ctx)
            } else {
                log::debug!(
                    "Skipping unknown AEM component type: {} (tag: {})",
                    resource_type,
                    node.tag_name
                );
                Ok(None)
            }
        }
    }
}

// ============================================================================
// Component converters
// ============================================================================

/// Which single-line input component a node's `sling:resourceType` names.
///
/// Preserving this on load is what lets a package that already carries
/// `controls/email` / `controls/telephone` nodes survive a load → save
/// round-trip: without it every one of them would be written back out as a plain
/// `controls/textbox`, silently stripping the validation clause and the autofill
/// hint the form was authored with.
fn text_field_kind(node: &JcrNode) -> TextFieldKind {
    let resource_type = node.resource_type().unwrap_or("");
    match resource_type.rsplit('/').next().unwrap_or(resource_type) {
        "email" => TextFieldKind::Email,
        "telephone" => TextFieldKind::Telephone,
        "textboxMultiline" => TextFieldKind::Multiline,
        _ => TextFieldKind::Plain,
    }
}

fn convert_textbox(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let kind = text_field_kind(node);
    let name = node.component_name().unwrap_or("textbox").to_string();
    let label = node.attr("jcr:title").unwrap_or("").to_string();
    let uuid = ctx.uuid_for(node, &name);
    let visible = parse_visible(node);
    let mandatory = parse_bool_attr(node, "mandatory");
    let max_chars = node.attr("maxChars").and_then(|v| v.parse::<usize>().ok());
    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "name",
                "jcr:title",
                "mandatory",
                "maxChars",
                "bindRef",
                "visible",
                "dorColspan",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::TextField {
        attrs: parse_attrs(node),
        uuid,
        name,
        label,
        mandatory,
        visible,
        max_chars,
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
        kind,
    }
}

fn convert_numberfield(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("numericbox").to_string();
    let label = node.attr("jcr:title").unwrap_or("").to_string();
    let uuid = ctx.uuid_for(node, &name);
    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "name",
                "jcr:title",
                "mandatory",
                "bindRef",
                "visible",
                "dorColspan",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::NumberField {
        attrs: parse_attrs(node),
        uuid,
        name,
        label,
        mandatory: parse_bool_attr(node, "mandatory"),
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
    }
}

fn convert_datepicker(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("datepicker").to_string();
    let label = node.attr("jcr:title").unwrap_or("").to_string();
    let uuid = ctx.uuid_for(node, &name);
    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "name",
                "jcr:title",
                "mandatory",
                "bindRef",
                "visible",
                "dorColspan",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::DatePicker {
        attrs: parse_attrs(node),
        uuid,
        name,
        label,
        mandatory: parse_bool_attr(node, "mandatory"),
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
    }
}

fn convert_radiobutton(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("radiobutton").to_string();
    let label = node.attr("jcr:title").unwrap_or("").to_string();
    let uuid = ctx.uuid_for(node, &name);
    let options = parse_options(node);
    let alignment = if node.attr("alignment").or(node.attr("optionLayout")) == Some("horizontal") {
        OptionAlignment::Horizontal
    } else {
        OptionAlignment::Vertical
    };

    // Store option value→label mapping for visibility condition resolution
    let label_map: HashMap<String, String> = options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .collect();
    if !label_map.is_empty() {
        ctx.option_labels.insert(name.clone(), label_map);
    }

    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "name",
                "jcr:title",
                "optionLayout",
                "mandatory",
                "bindRef",
                "visible",
                "dorColspan",
                "enum",
                "enumNames",
                "options",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::RadioButton {
        attrs: parse_attrs(node),
        uuid,
        name,
        label,
        options,
        alignment,
        mandatory: parse_bool_attr(node, "mandatory"),
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
        conditions: Vec::new(),
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
    }
}

fn convert_checkbox(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("checkbox").to_string();
    let label = node.attr("jcr:title").unwrap_or("").to_string();
    let uuid = ctx.uuid_for(node, &name);
    let options = parse_options(node);
    let alignment = if node.attr("alignment").or(node.attr("optionLayout")) == Some("horizontal") {
        OptionAlignment::Horizontal
    } else {
        OptionAlignment::Vertical
    };

    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "name",
                "jcr:title",
                "optionLayout",
                "bindRef",
                "visible",
                "dorColspan",
                "enum",
                "enumNames",
                "options",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::Checkbox {
        attrs: parse_attrs(node),
        uuid,
        name,
        label,
        options,
        alignment,
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
        conditions: Vec::new(),
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
    }
}

fn convert_dropdown(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("dropdownlist").to_string();
    let label = node.attr("jcr:title").unwrap_or("").to_string();
    let uuid = ctx.uuid_for(node, &name);
    let options = parse_options(node);

    // Store option value→label mapping for visibility condition resolution
    let label_map: HashMap<String, String> = options
        .iter()
        .map(|o| (o.value.clone(), o.label.clone()))
        .collect();
    if !label_map.is_empty() {
        ctx.option_labels.insert(name.clone(), label_map);
    }

    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "name",
                "jcr:title",
                "mandatory",
                "bindRef",
                "visible",
                "dorColspan",
                "enum",
                "enumNames",
                "options",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::Dropdown {
        attrs: parse_attrs(node),
        uuid,
        name,
        label,
        options,
        mandatory: parse_bool_attr(node, "mandatory"),
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
        conditions: Vec::new(),
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
    }
}

/// The presentation attributes ([`AemAttrs`]) a loaded node carries.
///
/// One reader for every component type, mirroring `insert_attrs` on the writing
/// side: whatever this consumes here is written back from the typed field, and
/// [`ATTR_NAMES`] names the same set for `record_passthrough` so nothing is
/// emitted twice.
fn parse_attrs(node: &JcrNode) -> AemAttrs {
    AemAttrs {
        dor_exclude: parse_bool_attr(node, "dorExclusion"),
        summary_exclude: parse_bool_attr(node, "summaryExclusion"),
        dor_exclude_title: parse_bool_attr(node, "dorExcludeTitle"),
        always_in_pdf: parse_bool_attr(node, "alwaysInPdf"),
        show_if_hidden: parse_bool_attr(node, "showIfHidden"),
        jump_to_field: parse_bool_attr(node, "jumpToFieldButtonVisible"),
        css: node
            .attr("css")
            .filter(|v| !v.is_empty())
            .map(|s| s.to_string()),
        dor_header_slot: node.attr("dorHeaderSlot").map(|s| s.to_string()),
    }
}

/// The attribute names [`parse_attrs`] consumes into typed fields, listed for
/// `record_passthrough` so they do not also travel as raw passthrough.
const ATTR_NAMES: &[&str] = &[
    "dorExclusion",
    "summaryExclusion",
    "dorExcludeTitle",
    "alwaysInPdf",
    "showIfHidden",
    "jumpToFieldButtonVisible",
    "css",
    "dorHeaderSlot",
];

fn convert_textdraw(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("textdraw").to_string();
    let uuid = ctx.uuid_for(node, &name);
    // For textdraw, content is in _value attribute
    // For messagebox, content is in messageboxBody attribute
    let content = node
        .attr("_value")
        .or_else(|| node.attr("messageboxBody"))
        .unwrap_or("")
        .to_string();

    ctx.record_passthrough(
        uuid,
        node,
        &[
            &["name", "_value", "messageboxBody", "dorColspan", "visible"][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::TextDraw {
        uuid,
        name,
        content,
        attrs: parse_attrs(node),
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
    }
}

fn convert_messagebox(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("messagebox").to_string();
    let uuid = ctx.uuid_for(node, &name);
    let content = node.attr("messageboxBody").unwrap_or("").to_string();

    ctx.record_passthrough(
        uuid,
        node,
        &[
            &["name", "messageboxBody", "dorColspan", "visible"][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::MessageBox {
        uuid,
        name,
        content,
        attrs: parse_attrs(node),
        visible: parse_visible(node),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
    }
}

fn convert_titledraw(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("titledraw").to_string();
    let uuid = ctx.uuid_for(node, &name);
    let content = node.attr("_value").unwrap_or("").to_string();
    let heading_level = node
        .attr("headingLevel")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    ctx.record_passthrough(
        uuid,
        node,
        &[
            &["name", "_value", "headingLevel", "dorColspan", "visible"][..],
            ATTR_NAMES,
        ]
        .concat(),
        REGENERATED_CHILD_TAGS,
    );

    AemNode::TitleDraw {
        attrs: parse_attrs(node),
        visible: parse_visible(node),
        uuid,
        name,
        content,
        heading_level,
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
    }
}

/// Fold an HTML-component locale onto the engine's own language code, and say
/// whether it was already one.
///
/// The engine keys text by language (`en`, `de`, `it`), while the AEM authoring
/// UI writes the regional UBS locales (`en-us`, `it-ch`, `de-ch`) -- the
/// collapse the corpus documents (`specs/feedback/consistent-problems.md`:
/// `de-ch` -> DE, `en-us`/`en-gb` -> EN, `it-ch` -> IT, `fr-ch` -> FR). Without
/// this a hand-authored `en-us` block would come back under a language the
/// writer never asks for, and every locale would then fall back to whichever
/// markup happened to sort first.
///
/// The inverse direction is the profile's `[html_locales]`, because which
/// spelling the deployed component matches on is the component's business; this
/// direction is not, since folding a region away is unambiguous.
fn fold_html_locale(locale: &str) -> (String, bool) {
    let lower = locale.trim().to_ascii_lowercase();
    match lower.split_once('-') {
        Some((base, _)) if !base.is_empty() => (base.to_string(), false),
        _ => (lower, true),
    }
}

/// Read the HTML component's per-locale markup back off its `localeContent`
/// children.
///
/// Item elements are positional (`item0`, `item1`, ...) and each carries a
/// `locale` and an `html`. The locale is folded back onto the engine's own
/// language code where the profile maps one -- but a locale the profile does
/// not name is kept under its own string rather than dropped, so a block a
/// person authored in AEM for a locale this profile has never heard of still
/// survives a load -> save round trip.
fn convert_html_displayer(node: &JcrNode, ctx: &mut ParseContext) -> AemNode {
    let name = node.component_name().unwrap_or("htmldisplayer").to_string();
    let uuid = ctx.uuid_for(node, &name);

    let mut content: BTreeMap<String, String> = BTreeMap::new();
    if let Some(locales) = node.children.iter().find(|c| c.tag_name == "localeContent") {
        for item in &locales.children {
            let Some(locale) = item.attr("locale") else {
                continue;
            };
            let html = item.attr("html").unwrap_or("").to_string();
            let (lang, exact) = fold_html_locale(locale);
            // An exact base code wins over a regional one that folds onto it, so
            // a package carrying both `de` and `de-ch` keeps the `de` markup
            // whichever order the items appear in.
            if exact {
                content.insert(lang, html);
            } else {
                content.entry(lang).or_insert(html);
            }
        }
    }

    ctx.record_passthrough(
        uuid,
        node,
        &[&["name", "dorColspan", "visible"][..], ATTR_NAMES].concat(),
        &[&["localeContent"][..], REGENERATED_CHILD_TAGS].concat(),
    );

    AemNode::HtmlDisplayer {
        attrs: parse_attrs(node),
        visible: parse_visible(node),
        uuid,
        name,
        content: AemI18nText(content),
        colspan: parse_colspan(node),
        dor_colspan: parse_dor_colspan(node),
    }
}

fn convert_panel(node: &JcrNode, ctx: &mut ParseContext) -> Result<Option<AemNode>, String> {
    let name = node.component_name().unwrap_or(&node.tag_name).to_string();
    let title = node.attr("jcr:title").unwrap_or("").to_string();

    // Check for repeatable.
    //
    // AEM writes `maxOccur="-1"` for an unbounded repeat. Parsing that as `u32`
    // fails, so it used to fall back to 0 and the panel was flattened into an
    // ordinary `Panel` with its occurrence silently dropped. Parse as `i64` and
    // map any negative value onto [`AemNode::UNBOUNDED_OCCUR`].
    let min_occur = node
        .attr("minOccur")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let max_occur = match node.attr("maxOccur").and_then(|v| v.parse::<i64>().ok()) {
        Some(v) if v < 0 => AemNode::UNBOUNDED_OCCUR,
        Some(v) => u32::try_from(v).unwrap_or(0),
        None => 0,
    };

    let is_repeatable = max_occur > 1;

    // A plain fragment reference is the fragment's node, uuid and passthrough
    // included, not a panel's.
    if !is_repeatable && node.attr("fragRef").is_some() {
        return convert_fragment(node, ctx);
    }

    let uuid = ctx.uuid_for(node, &name);
    let attrs = parse_attrs(node);
    // A conditional panel's show rule is regenerated from its triggers'
    // conditions, and the panel stays hidden until one of them matches.
    let show_triggers = show_triggers(node);
    let visible = show_triggers.is_none() && parse_visible(node);
    let regenerated_children: Vec<&str> = match show_triggers {
        Some(_) => [REGENERATED_CHILD_TAGS, &["fd:rules", "fd:scripts"]].concat(),
        None => REGENERATED_CHILD_TAGS.to_vec(),
    };

    ctx.record_passthrough(
        uuid,
        node,
        // `dorExclusion` and `dorExcludeTitle` are two different attributes and
        // are kept apart: the first drops the whole node from the DoR, the
        // second only its heading, and a wizard step carries the second by
        // convention. Folding them into one flag (as this did) turned every
        // step into a DoR-excluded node on the way back out.
        &[
            &[
                "name",
                "jcr:title",
                "visible",
                "minOccur",
                "maxOccur",
                "bindRef",
                "dorColspan",
                "fragRef",
            ][..],
            // The repeating-panel archetype. `repeatable.xml` writes all five
            // itself and stamps the rules it generates "Do not edit: will be
            // overwritten", so they are engine-owned, not authored: capturing
            // them as unmodeled passthrough would carry a stale value forward
            // and would make load -> save -> load stop being a fixpoint the
            // moment the template gained an attribute the deployed corpus
            // predates. Only on a repeatable -- an ordinary panel that carries
            // an `accessibilityLabel` authored it, and keeps it.
            if is_repeatable {
                REPEATING_PANEL_ARCHETYPE_ATTRS
            } else {
                &[]
            },
            ATTR_NAMES,
        ]
        .concat(),
        &regenerated_children,
    );

    // Check for page/wizard step (panels with validateOnStepCompletion or
    // that are direct children of a wizard-layout rootPanel)
    let is_page = false; // Determined by parent context, not self attributes

    // Check for fragment reference
    if let Some(frag_ref_for_repeat) = node.attr("fragRef").map(|s| s.to_string()) {
        let fragment_result = convert_fragment(node, ctx)?;
        if let Some(inner) = fragment_result {
            let children = match inner {
                AemNode::Panel { children, .. } => children,
                other => vec![other],
            };
            return Ok(Some(AemNode::Repeatable {
                attrs: attrs.clone(),
                visible,
                uuid,
                name,
                title,
                children,
                min_occur: min_occur.max(1),
                max_occur,
                bind_ref: node.attr("bindRef").map(|s| s.to_string()),
                frag_ref: Some(frag_ref_for_repeat),
            }));
        }
        return Ok(fragment_result);
    }

    // Parse children
    let children = convert_items_to_aem_nodes(node, ctx)?;

    // The DoR's column count, which the templates write as the layout's
    // `dorNumCols` (its `columns` is the screen layout's, always 1).
    let dor_num_cols = node
        .children
        .iter()
        .find(|c| c.tag_name == "layout")
        .and_then(|layout| layout.attr("dorNumCols"))
        .and_then(|v| v.parse().ok());

    if is_repeatable {
        Ok(Some(AemNode::Repeatable {
            attrs,
            visible,
            uuid,
            name,
            title,
            children,
            min_occur: min_occur.max(1),
            max_occur,
            bind_ref: node.attr("bindRef").map(|s| s.to_string()),
            frag_ref: None,
        }))
    } else {
        // A panel hidden by an authored rule rather than `conditional.xml`'s may
        // be conditional too.
        let is_conditional = match show_triggers {
            Some(triggers) => {
                ctx.show_triggers.push((name.clone(), triggers));
                true
            }
            None => !visible,
        };
        Ok(Some(AemNode::Panel {
            uuid,
            name,
            title,
            children,
            is_page,
            attrs,
            visible,
            is_conditional,
            dor_num_cols,
            colspan: parse_colspan(node),
            dor_colspan: parse_dor_colspan(node),
            bind_ref: node.attr("bindRef").map(|s| s.to_string()),
            frag_ref: None,
        }))
    }
}

fn convert_fragment(node: &JcrNode, ctx: &mut ParseContext) -> Result<Option<AemNode>, String> {
    let frag_ref = node.attr("fragRef").unwrap_or("").to_string();
    let name = node.component_name().unwrap_or(&node.tag_name).to_string();
    let uuid = ctx.uuid_for(node, &name);
    ctx.record_passthrough(
        uuid,
        node,
        &[
            &[
                "fragRef",
                "name",
                "jcr:title",
                "visible",
                "bindRef",
                "dorColspan",
            ][..],
            ATTR_NAMES,
        ]
        .concat(),
        // `fragment.xml` always writes a fragment's one `fd:scripts` itself (the
        // profile's Initialize rule for its kind, or an empty one), and JCR holds
        // no second child of that name, so a loaded one is regenerated rather
        // than kept: an authored fragment script gives way to the profile's.
        &[REGENERATED_CHILD_TAGS, &["fd:scripts"]].concat(),
    );
    let hidden = hidden_on_init(node);
    if !hidden.is_empty() {
        ctx.fragment_init_hide.insert(uuid, hidden);
    }

    // Cycle detection
    if ctx.fragment_stack.contains(&frag_ref) {
        log::warn!(
            "Fragment cycle detected: {} → {}",
            ctx.fragment_stack.join(" → "),
            frag_ref
        );
        return Ok(None);
    }

    // Try to resolve the fragment from within the ZIP
    let fragment_xml_path = resolve_fragment_path(&frag_ref, ctx.zip_files);

    // Helper closure to parse fragment XML and produce a Panel node
    let parse_fragment_xml =
        |xml_str: &str, ctx: &mut ParseContext| -> Result<Option<AemNode>, String> {
            ctx.fragment_stack.push(frag_ref.clone());

            let fragment_tree = parse_jcr_xml(xml_str)?;
            let content_node = find_fragment_content(&fragment_tree);

            let children = if let Some(content) = content_node {
                convert_items_to_aem_nodes(content, ctx)?
            } else {
                Vec::new()
            };

            ctx.fragment_stack.pop();

            let title = node.attr("jcr:title").unwrap_or("").to_string();
            Ok(Some(AemNode::Panel {
                uuid,
                name: name.clone(),
                title,
                children,
                is_page: false,
                attrs: parse_attrs(node),
                visible: parse_visible(node),
                is_conditional: false,
                dor_num_cols: None,
                colspan: parse_colspan(node),
                dor_colspan: parse_dor_colspan(node),
                bind_ref: node.attr("bindRef").map(|s| s.to_string()),
                // The fragment's children have been inlined, but the panel
                // remembers where they came from so the XSD walk can emit a
                // single fragment element instead of descending into them.
                frag_ref: Some(frag_ref.clone()),
            }))
        };

    // First: try resolving from the ZIP
    if let Some(xml_path) = fragment_xml_path
        && let Some(xml_bytes) = ctx.zip_files.get(&xml_path)
    {
        let xml_str = String::from_utf8_lossy(xml_bytes);
        return parse_fragment_xml(&xml_str, ctx);
    }

    // Second: try resolving from embedded profiles (on-disk fragments)
    if let Some(xml_str) = crate::profiles::resolve_embedded_fragment_xml(&frag_ref) {
        // Load dictionaries from the specific fragment
        for (lang, dict_xml) in crate::profiles::resolve_embedded_fragment_dictionaries(&frag_ref) {
            parse_sling_dictionary(&dict_xml, &lang, &mut ctx.translations);
        }

        // Also load dictionaries from all sibling fragments in the same library.
        // In AEM, Sling dictionaries are shared across fragments in the same content subtree.
        let library = frag_ref
            .strip_prefix("/content/dam/formsanddocuments/")
            .or_else(|| frag_ref.strip_prefix("/content/forms/af/"))
            .and_then(|r| r.split('/').next())
            .unwrap_or("")
            .to_string();
        if !library.is_empty() && ctx.loaded_library_dicts.insert(library) {
            for (lang, dict_xml) in
                crate::profiles::resolve_embedded_library_dictionaries(&frag_ref)
            {
                parse_sling_dictionary(&dict_xml, &lang, &mut ctx.translations);
            }
        }

        return parse_fragment_xml(&xml_str, ctx);
    }

    // Fragment not found — keep as opaque fragment reference
    log::warn!("Fragment not found: {frag_ref}");
    Ok(Some(AemNode::Fragment {
        attrs: parse_attrs(node),
        visible: true,
        uuid,
        name,
        title: node.attr("jcr:title").unwrap_or_default().to_string(),
        frag_ref,
        bind_ref: node.attr("bindRef").map(|s| s.to_string()),
        init_hide: Vec::new(),
    }))
}

/// Try to resolve a fragRef path to a ZIP file path.
///
/// `fragRef` looks like `/content/dam/formsanddocuments/path/to/fragment` or
/// `/content/forms/af/path/to/fragment`. We need to find the corresponding
/// `.content.xml` in the ZIP.
fn resolve_fragment_path(frag_ref: &str, zip_files: &HashMap<String, Vec<u8>>) -> Option<String> {
    // Try direct mapping: fragRef → jcr_root/{fragRef}/.content.xml
    let clean_ref = frag_ref.trim_start_matches('/');
    let direct_path = format!("jcr_root/{clean_ref}/.content.xml");
    if zip_files.contains_key(&direct_path) {
        return Some(direct_path);
    }

    // Try with guideContainer path
    let guide_path = format!("jcr_root/{clean_ref}/jcr:content/guideContainer/.content.xml");
    if zip_files.contains_key(&guide_path) {
        return Some(guide_path);
    }

    // Search for any .content.xml that contains the fragment's last path segment
    let fragment_name = frag_ref.rsplit('/').next().unwrap_or(frag_ref);
    for path in zip_files.keys() {
        if path.contains(fragment_name) && path.ends_with("/.content.xml") {
            return Some(path.clone());
        }
    }

    None
}

/// Find the content node inside a fragment XML (typically guideContainer > rootPanel
/// or a top-level panel).
fn find_fragment_content(tree: &JcrNode) -> Option<&JcrNode> {
    // First try: guideContainer > rootPanel
    if let Some(gc) = find_node_by_resource_type(tree, "fd/af/components/guideContainer") {
        if let Some(rp) = gc.children.iter().find(|c| c.tag_name == "rootPanel") {
            return Some(rp);
        }
        return Some(gc);
    }

    // Second try: rootPanel directly
    if let Some(rp) = find_node_by_resource_type(tree, "fd/af/components/rootPanel") {
        return Some(rp);
    }

    // Third try: any panel with items
    if tree.children.iter().any(|c| c.tag_name == "items") {
        return Some(tree);
    }

    // Recurse into jcr:content if present
    for child in &tree.children {
        if child.tag_name == "jcr:content" {
            return find_fragment_content(child);
        }
    }

    None
}

// ============================================================================
// Script extraction
// ============================================================================

/// The sub-panels `node`'s Initialize rule hides: each `hideAFHideDor(this.X)`
/// in its `fd:init`, in order.
fn hidden_on_init(node: &JcrNode) -> Vec<super::ComponentName> {
    use regex_lite::Regex;
    let Some(init) = node
        .children
        .iter()
        .find(|c| c.tag_name == "fd:scripts")
        .and_then(|scripts| scripts.attr("fd:init"))
    else {
        return Vec::new();
    };
    let call = Regex::new(r"hideAFHideDor\(this\.([A-Za-z_][A-Za-z0-9_]*)\)").unwrap();
    call.captures_iter(init)
        .filter_map(|c| super::ComponentName::try_from(c[1].to_string()).ok())
        .collect()
}

/// Extract scripts from `fd:scripts` and `fd:rules` child nodes.
fn extract_scripts_from_node(node: &JcrNode, comp_name: &str, ctx: &mut ParseContext) {
    for child in &node.children {
        if child.tag_name == "fd:scripts" {
            // fd:scripts has attributes like fd:init, fd:click, fd:valueCommit, etc.
            // Each is a JSON array of script models.
            for (attr_name, attr_value) in &child.attributes {
                if attr_name.starts_with("fd:") && attr_name != "fd:translationIds" {
                    let event_name = match attr_name.as_str() {
                        "fd:init" => "Initialize",
                        "fd:click" => "Click",
                        "fd:valueCommit" => "Value Commit",
                        "fd:calc" => "Calculate",
                        "fd:visible" => "Visibility",
                        "fd:navigationChange" => "Navigation Change",
                        "fd:validate" => "Validate",
                        _ => attr_name.trim_start_matches("fd:"),
                    };

                    if let Some(scripts) = parse_fd_scripts_json(attr_value, event_name) {
                        ctx.scripts
                            .entry(comp_name.to_string())
                            .or_default()
                            .extend(scripts);
                    }
                }
            }
        }

        if child.tag_name == "fd:rules" {
            // fd:rules can also contain script content in various attributes
            for (attr_name, attr_value) in &child.attributes {
                if attr_name.starts_with("fd:") {
                    if let Some(scripts) = parse_fd_scripts_json(attr_value, "Rule") {
                        ctx.scripts
                            .entry(comp_name.to_string())
                            .or_default()
                            .extend(scripts);
                    }

                    // Parse structured visibility rules from fd:visible
                    if attr_name == "fd:visible" {
                        parse_visibility_rules(attr_value, ctx);
                    }
                }
            }
        }
    }
}

/// Parse visibility rules from a `fd:visible` attribute on `fd:rules`.
///
/// The raw value (after XML unescaping) contains JCR-serialized JSON with
/// structured rule trees. Deeply nested script content makes full JSON
/// parsing unreliable, so we use regex to extract the structured
/// SHOW_EXPRESSION components.
fn parse_visibility_rules(raw: &str, ctx: &mut ParseContext) {
    use regex_lite::Regex;

    // JCR unescaping: \, → , (commas are escaped in JCR).
    // Quotes are already unescaped by quick-xml.
    let cleaned = raw.replace("\\,", ",");

    // Only process rules that contain a structured SHOW_EXPRESSION
    if !cleaned.contains("SHOW_EXPRESSION") {
        return;
    }

    // Extract the target panel name from AFCOMPONENT
    let re_afcomp = Regex::new(r#""AFCOMPONENT".*?"name":"([^"]+)""#).unwrap();
    // Extract the trigger field name from COMPONENT
    let re_comp = Regex::new(r#""COMPONENT".*?"name":"([^"]+)""#).unwrap();
    // Extract the trigger value from STRING_LITERAL
    let re_strlit = Regex::new(r#""STRING_LITERAL".*?"value":"([^"]+)""#).unwrap();

    if let (Some(target), Some(trigger), Some(value)) = (
        re_afcomp.captures(&cleaned),
        re_comp.captures(&cleaned),
        re_strlit.captures(&cleaned),
    ) {
        let target_panel = target.get(1).unwrap().as_str().to_string();
        let trigger_field = trigger.get(1).unwrap().as_str().to_string();
        let trigger_value = value.get(1).unwrap().as_str().to_string();

        ctx.visibility_conditions.insert(
            target_panel,
            VisibilityCondition {
                trigger_field,
                trigger_value,
            },
        );
    }
}

/// The (trigger field, value) pairs of the show rule `conditional.xml` writes,
/// `if (F.value == "v" || G.value == "w") { showAFShowDor ... }`, or `None`
/// when the panel carries no such rule.
fn show_triggers(node: &JcrNode) -> Option<Vec<(String, String)>> {
    use regex_lite::Regex;
    let script = node
        .children
        .iter()
        .find(|c| c.tag_name == "fd:scripts")?
        .attr("fd:visible")?;
    if !script.contains("SHOW_EXPRESSION") || !script.contains("showAFShowDor") {
        return None;
    }
    let content = script.replace("\\,", ",");
    let condition = Regex::new(r#"if \((.*?)\) \{"#).unwrap();
    // The quotes around a value are JSON-escaped, and JCR escapes that
    // backslash again: `\\"` in the attribute as read.
    let test = Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\.value == \\+"([^"\\]*)\\+""#).unwrap();
    let condition = condition.captures(&content)?.get(1)?.as_str().to_string();
    let triggers: Vec<(String, String)> = test
        .captures_iter(&condition)
        .map(|c| (c[1].to_string(), unescape_show_value(&c[2])))
        .collect();
    (!triggers.is_empty()).then_some(triggers)
}

/// A value as `conditional.xml` wrote it into the rule: `{{ t.value | escape }}`.
fn unescape_show_value(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
        .replace("&amp;", "&")
}

/// Give each trigger field the conditions the show rules test it for, in the
/// order the rules list them, so the writer regenerates the same rules.
fn attach_show_conditions(node: &mut AemNode, triggers: &[(String, Vec<(String, String)>)]) {
    match node {
        AemNode::RadioButton {
            name, conditions, ..
        }
        | AemNode::Checkbox {
            name, conditions, ..
        }
        | AemNode::Dropdown {
            name, conditions, ..
        } => {
            for (panel, tests) in triggers {
                for (field, value) in tests {
                    if field == name {
                        conditions.push(super::ConditionRule {
                            target_panel_name: panel.clone(),
                            value: crate::value::InputValue::Text(value.clone()),
                            show: true,
                        });
                    }
                }
            }
        }
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => children
            .iter_mut()
            .for_each(|c| attach_show_conditions(c, triggers)),
        _ => {}
    }
}

/// Parse the JSON content of an `fd:scripts` attribute value.
///
/// The format is a JSON array of objects like:
/// ```json
/// [{"script":{"content":"...", "event":"Initialize", "field":"..."}, "nodeName":"SCRIPTMODEL", "version":1, "enabled":true}]
/// ```
///
/// The JSON is typically XML-escaped with `&quot;` replaced by `"` and `\,` etc.
fn parse_fd_scripts_json(raw: &str, default_event: &str) -> Option<Vec<AemScript>> {
    // The raw value has already been XML-unescaped by quick-xml's unescape_value,
    // but it may still have backslash-escaped commas and quotes from JCR serialization.
    let cleaned = raw.replace("\\,", ",").replace("\\\"", "\"");

    let parsed: serde_json::Value = serde_json::from_str(&cleaned).ok()?;
    let arr = parsed.as_array()?;

    let mut scripts = Vec::new();
    for item in arr {
        let script_obj = item.get("script")?;
        let content = script_obj
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let event = script_obj
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or(default_event);
        let field = script_obj
            .get("field")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let enabled = item
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if !content.is_empty() {
            scripts.push(AemScript {
                event: event.to_string(),
                content: content.to_string(),
                field: field.to_string(),
                enabled,
            });
        }
    }

    Some(scripts)
}

// ============================================================================
// Translation extraction
// ============================================================================

/// Extract Sling i18n translations from dictionary files in the ZIP.
fn extract_translations(zip_files: &HashMap<String, Vec<u8>>) -> TranslationData {
    let mut data = TranslationData::default();

    for (path, content) in zip_files {
        // Dictionary files are typically at paths like:
        // .../dictionary/de.xml, .../dictionary/en-us.xml, .../i18n/de.xml
        if !path.contains("dictionary/") && !path.contains("i18n/") {
            continue;
        }
        if !path.ends_with(".xml") {
            continue;
        }

        // Extract language from filename (e.g., "de.xml" → "de")
        let filename = path.rsplit('/').next().unwrap_or("");
        let lang = filename.trim_end_matches(".xml");
        if lang.is_empty() {
            continue;
        }

        let xml_str = String::from_utf8_lossy(content);
        parse_sling_dictionary(&xml_str, lang, &mut data);
    }

    data
}

/// Parse a Sling i18n dictionary XML file.
///
/// Format:
/// ```xml
/// <jcr:root ...>
///   <entry sling:key="some##key##123" sling:message="Translated text" .../>
/// </jcr:root>
/// ```
fn parse_sling_dictionary(xml: &str, language: &str, data: &mut TranslationData) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e) | Event::Start(ref e)) => {
                let mut sling_key = None;
                let mut sling_message = None;

                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                    let value = attr.unescape_value().unwrap_or_default().to_string();
                    match key.as_str() {
                        "sling:key" => sling_key = Some(value),
                        "sling:message" => sling_message = Some(value),
                        _ => {}
                    }
                }

                if let (Some(key), Some(message)) = (sling_key, sling_message) {
                    data.entries
                        .entry(key)
                        .or_default()
                        .insert(language.to_string(), message);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

// ============================================================================
// Attribute helpers
// ============================================================================

/// Parse the `visible` attribute from a JCR node.
/// Default is `true`. `{Boolean}false` or `false` → `false`.
fn parse_visible(node: &JcrNode) -> bool {
    match node.attr("visible") {
        Some(v) => !v.contains("false"),
        None => true,
    }
}

/// Parse a boolean attribute (like `mandatory`, `dorExclusion`).
fn parse_bool_attr(node: &JcrNode, attr: &str) -> bool {
    match node.attr(attr) {
        Some(v) => v.contains("true"),
        None => false,
    }
}

/// Parse radio/checkbox/dropdown options from a JCR node.
///
/// AEM stores options as pipe-delimited strings in `enum` and `enumNames` attributes,
/// or as child `<items>` elements.
fn parse_options(node: &JcrNode) -> Vec<AemOption> {
    let mut options = Vec::new();

    // Try enum/enumNames format: enum="[val1,val2]" enumNames="[Label 1,Label 2]"
    let enum_values = node.attr("enum").map(parse_jcr_array).unwrap_or_default();
    let enum_names = node
        .attr("enumNames")
        .map(parse_jcr_array)
        .unwrap_or_default();

    if !enum_values.is_empty() {
        for (i, value) in enum_values.iter().enumerate() {
            let label = enum_names.get(i).cloned().unwrap_or_else(|| value.clone());
            options.push(AemOption {
                label,
                value: value.clone(),
            });
        }
        return options;
    }

    // Try options="[1=Individual,2=Legal Entity]" format
    if let Some(opts_str) = node.attr("options") {
        let trimmed = opts_str.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let inner = &trimmed[1..trimmed.len() - 1];
            for entry in split_jcr_list(inner) {
                let entry = entry.trim();
                if let Some((value, label)) = entry.split_once('=') {
                    options.push(AemOption {
                        label: jcr_unescape(label),
                        value: jcr_unescape(value),
                    });
                }
            }
            if !options.is_empty() {
                return options;
            }
        }
    }

    // Try items child elements
    for child in &node.children {
        if child.tag_name == "items" {
            for item in &child.children {
                let value = item.attr("value").unwrap_or("").to_string();
                let label = item
                    .attr("jcr:title")
                    .or_else(|| item.attr("text"))
                    .unwrap_or(&value)
                    .to_string();
                options.push(AemOption { label, value });
            }
        }
    }

    options
}

/// Split a JCR comma-separated string on unescaped commas.
///
/// Backslash-escaped commas (`\,`) are treated as literal commas and do not
/// act as separators.
fn split_jcr_list(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek()
                && (next == ',' || next == '\\')
            {
                current.push(next);
                chars.next();
                continue;
            }
            current.push(ch);
        } else if ch == ',' {
            items.push(current);
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    items.push(current);
    // Filter out empty entries (matches old behaviour for empty brackets)
    items.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// Unescape JCR backslash sequences (`\,` → `,`, `\\` → `\`).
fn jcr_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek()
                && (next == ',' || next == '\\')
            {
                out.push(next);
                chars.next();
                continue;
            }
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse a JCR multi-value array like `"[val1,val2,val3]"` into a Vec<String>.
fn parse_jcr_array(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        split_jcr_list(&trimmed[1..trimmed.len() - 1])
            .into_iter()
            .map(|s| jcr_unescape(s.trim()))
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![trimmed.to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The show rule exactly as `conditional.xml` writes it, as the parser
    /// reads the attribute back.
    #[test]
    fn show_triggers_reads_the_conditional_templates_rule() {
        let xml = r#"<panel_1 name="PN_A"><fd:scripts jcr:primaryType="nt:unstructured" fd:visible="[{&quot;script&quot;:{&quot;field&quot;:&quot;PN_A&quot;\,&quot;event&quot;:&quot;Visibility&quot;\,&quot;model&quot;:{&quot;nodeName&quot;:&quot;SHOW_EXPRESSION&quot;}\,&quot;content&quot;:&quot;if (RB_Order.value == \\&quot;RB_1\\&quot; || CB_Block.value == \\&quot;true\\&quot;) {\\n  window.forms.ubs.showAFShowDor(this);\\n  true;\\n} else {\\n  window.forms.ubs.hideAFHideDor(this);\\n  false;\\n}\\n&quot;}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true}]"/></panel_1>"#;
        let tree = parse_jcr_xml(xml).unwrap();
        assert_eq!(
            show_triggers(&tree),
            Some(vec![
                ("RB_Order".to_string(), "RB_1".to_string()),
                ("CB_Block".to_string(), "true".to_string())
            ])
        );
    }

    /// A regional UBS locale folds onto the engine's language code; a bare
    /// language code is reported as already exact, which is what lets a package
    /// carrying both `de` and `de-ch` keep the `de` markup regardless of the
    /// order the items appear in.
    #[test]
    fn regional_html_locales_fold_onto_the_language_code() {
        assert_eq!(fold_html_locale("en-us"), ("en".to_string(), false));
        assert_eq!(fold_html_locale("it-CH"), ("it".to_string(), false));
        assert_eq!(fold_html_locale("de-ch"), ("de".to_string(), false));
        assert_eq!(fold_html_locale("en"), ("en".to_string(), true));
        assert_eq!(fold_html_locale("  sp "), ("sp".to_string(), true));
    }

    #[test]
    fn test_parse_jcr_array() {
        assert_eq!(parse_jcr_array("[a,b,c]"), vec!["a", "b", "c"]);
        assert_eq!(parse_jcr_array("single"), vec!["single"]);
        assert_eq!(parse_jcr_array("[]"), Vec::<String>::new());
    }

    #[test]
    fn test_parse_fd_scripts_json() {
        let raw = r#"[{"script":{"content":"this.visible = false;","event":"Initialize","field":"guide.rootPanel.myField"},"nodeName":"SCRIPTMODEL","version":1,"enabled":true}]"#;
        let scripts = parse_fd_scripts_json(raw, "Initialize").unwrap();
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].event, "Initialize");
        assert_eq!(scripts[0].content, "this.visible = false;");
        assert!(scripts[0].enabled);
    }

    #[test]
    fn test_detect_aem_zip_false_for_non_zip() {
        assert!(!detect_aem_zip(b"not a zip file"));
    }

    #[test]
    fn test_parse_jcr_array_with_escaped_commas() {
        // A label containing a comma should be kept intact
        assert_eq!(
            parse_jcr_array(r"[Yes\, definitely,No]"),
            vec!["Yes, definitely", "No"]
        );
    }

    #[test]
    fn test_split_jcr_list_basic() {
        assert_eq!(split_jcr_list("a,b,c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_split_jcr_list_escaped_comma() {
        assert_eq!(
            split_jcr_list(r"1=Yes\, definitely,2=No\, thanks"),
            vec!["1=Yes, definitely", "2=No, thanks"]
        );
    }

    #[test]
    fn test_split_jcr_list_escaped_backslash() {
        // A literal backslash followed by a comma that is NOT an escape
        assert_eq!(split_jcr_list(r"a\\,b"), vec![r"a\", "b"]);
    }

    #[test]
    fn test_jcr_unescape() {
        assert_eq!(jcr_unescape(r"Yes\, definitely"), "Yes, definitely");
        assert_eq!(jcr_unescape(r"back\\slash"), "back\\slash");
        assert_eq!(jcr_unescape("plain"), "plain");
    }
}
