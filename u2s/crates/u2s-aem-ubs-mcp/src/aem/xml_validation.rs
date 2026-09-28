//! Validation of generated AEM `.content.xml` documents.
//!
//! These checks verify that a `.content.xml` is well-formed XML and that it
//! satisfies the JCR/CQ/FD/Sling structural conventions used by this project.
//! The logic lives here (rather than in test-only code) so it can be reused
//! both by the test suite (see `tests::helpers::assert_valid_aem_form_xml`)
//! and by production callers such as the MCP agent's package validator.
//!
//! The public entry points return the list of violations rather than panicking,
//! so callers can decide how to surface them.

use std::collections::{BTreeMap, BTreeSet};

/// Validate that `xml` is well-formed and properly escaped.
///
/// Returns `Err` with a human-readable description (including a snippet of the
/// surrounding XML) on the first problem found: a syntax error, an unescaped
/// `&`, or an unescaped `<`/`>` inside an attribute value.
pub fn validate_xml_wellformed(xml: &str) -> Result<(), String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Err(e) => {
                let pos = reader.error_position() as usize;
                let start = pos.saturating_sub(200);
                let end = (pos + 200).min(xml.len());
                let snippet = &xml[start..end];
                return Err(format!(
                    "XML syntax error at byte {pos}: {e}\n\nContext:\n{snippet}"
                ));
            }
            _ => {}
        }
    }

    // quick-xml may silently accept an `&` that is not part of a valid entity
    // reference (`amp;`, `lt;`, `gt;`, `quot;`, `apos;`) or numeric reference.
    check_no_unescaped_ampersands(xml)?;
    // Check for unescaped `<` inside attribute values.
    check_no_unescaped_angle_brackets_in_attributes(xml)?;
    Ok(())
}

/// Validate a generated AEM form `.content.xml`: well-formedness plus the
/// JCR/CQ/FD/Sling structural conventions used by this project.
///
/// Returns `Ok(())` when valid, or `Err` with the list of violations. A
/// well-formedness failure is returned as a single-element list.
pub fn validate_aem_form_xml(xml: &str) -> Result<(), Vec<String>> {
    if let Err(e) = validate_xml_wellformed(xml) {
        return Err(vec![e]);
    }
    let violations = validate_aem_form_structure(xml);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Validate a generated AEM DAM `.content.xml`: well-formedness plus the
/// JCR/DAM/Sling structural conventions used by this project.
pub fn validate_aem_dam_xml(xml: &str) -> Result<(), Vec<String>> {
    if let Err(e) = validate_xml_wellformed(xml) {
        return Err(vec![e]);
    }
    let violations = validate_aem_dam_structure(xml);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Run the form structural checks. Assumes `xml` is already well-formed.
pub fn validate_aem_form_structure(xml: &str) -> Vec<String> {
    let doc = parse_xml_document(xml);
    let mut violations = Vec::new();

    validate_namespaces(&doc, &mut violations);
    validate_form_structure(&doc, &mut violations);
    validate_jcr_primary_types(&doc, XmlKind::Form, &mut violations);
    validate_sling_resource_types(&doc, &mut violations);
    validate_fd_contract(&doc, &mut violations);
    validate_script_escaping(&doc, &mut violations);
    validate_typed_values(&doc, &mut violations);
    validate_guide_node_classes(&doc, &mut violations);
    validate_repeatables(&doc, &mut violations);

    violations
}

/// Run the DAM structural checks. Assumes `xml` is already well-formed.
pub fn validate_aem_dam_structure(xml: &str) -> Vec<String> {
    let doc = parse_xml_document(xml);
    let mut violations = Vec::new();

    validate_namespaces(&doc, &mut violations);
    validate_dam_structure(&doc, &mut violations);
    validate_jcr_primary_types(&doc, XmlKind::Dam, &mut violations);
    validate_sling_resource_types(&doc, &mut violations);
    validate_typed_values(&doc, &mut violations);

    violations
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XmlKind {
    Form,
    Dam,
}

#[derive(Debug)]
pub(crate) struct ParsedXmlDocument {
    pub(crate) nodes: Vec<ParsedXmlNode>,
}

#[derive(Debug)]
pub(crate) struct ParsedXmlNode {
    pub(crate) tag: String,
    pub(crate) path: String,
    pub(crate) attributes: BTreeMap<String, String>,
    pub(crate) parent: Option<usize>,
    pub(crate) children: Vec<usize>,
}

pub(crate) fn parse_xml_document(xml: &str) -> ParsedXmlDocument {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;

    let mut nodes = Vec::<ParsedXmlNode>::new();
    let mut stack = Vec::<usize>::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let node_index = push_xml_node(&reader, &mut nodes, stack.last().copied(), event);
                stack.push(node_index);
            }
            Ok(Event::Empty(event)) => {
                push_xml_node(&reader, &mut nodes, stack.last().copied(), event);
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => panic!("Failed to parse XML for structural validation: {e}"),
            _ => {}
        }
    }

    ParsedXmlDocument { nodes }
}

fn push_xml_node(
    reader: &quick_xml::Reader<&[u8]>,
    nodes: &mut Vec<ParsedXmlNode>,
    parent: Option<usize>,
    event: quick_xml::events::BytesStart<'_>,
) -> usize {
    let tag = String::from_utf8_lossy(event.name().as_ref()).into_owned();
    let mut attributes = BTreeMap::new();
    for attr in event.attributes().with_checks(false) {
        let attr = attr.unwrap_or_else(|e| panic!("Invalid XML attribute: {e}"));
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr
            .decode_and_unescape_value(reader.decoder())
            .unwrap_or_else(|e| panic!("Failed to decode XML attribute value for {key}: {e}"))
            .into_owned();
        attributes.insert(key, value);
    }

    let path = match parent {
        Some(parent_index) => format!(
            "{}/{}",
            nodes[parent_index].path,
            node_descriptor(&tag, &attributes)
        ),
        None => node_descriptor(&tag, &attributes),
    };

    let index = nodes.len();
    nodes.push(ParsedXmlNode {
        tag,
        path,
        attributes,
        parent,
        children: Vec::new(),
    });

    if let Some(parent_index) = parent {
        nodes[parent_index].children.push(index);
    }

    index
}

fn node_descriptor(tag: &str, attributes: &BTreeMap<String, String>) -> String {
    if let Some(name) = attributes.get("name") {
        format!("{tag}[@name='{name}']")
    } else {
        tag.to_string()
    }
}

/// Return every element (as a path) that declares a duplicate attribute key.
/// quick-xml keeps only the last value for a duplicated key, so this re-scans
/// the raw start tags rather than the parsed BTreeMap.
pub fn duplicate_attribute_elements(xml: &str) -> Vec<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut offenders = Vec::new();
    loop {
        let event = match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => e,
            Ok(Event::Eof) => break,
            Err(e) => panic!("Failed to parse XML for duplicate-attribute scan: {e}"),
            _ => continue,
        };
        let tag = String::from_utf8_lossy(event.name().as_ref()).into_owned();
        let mut seen = BTreeSet::new();
        for attr in event.attributes().with_checks(false) {
            let Ok(attr) = attr else { continue };
            let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
            if !seen.insert(key.clone()) {
                offenders.push(format!("<{tag}> has duplicate attribute {key}"));
            }
        }
    }
    offenders
}

fn validate_namespaces(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    let Some(root) = doc.nodes.first() else {
        violations.push("XML document is empty".into());
        return;
    };

    let declared = root
        .attributes
        .iter()
        .filter_map(|(key, value)| key.strip_prefix("xmlns:").map(|prefix| (prefix, value)))
        .collect::<BTreeMap<_, _>>();

    let mut used_prefixes = BTreeSet::new();
    for node in &doc.nodes {
        if let Some(prefix) = xml_prefix(&node.tag) {
            used_prefixes.insert(prefix.to_string());
        }
        for key in node.attributes.keys() {
            if key.starts_with("xmlns:") {
                continue;
            }
            if let Some(prefix) = xml_prefix(key) {
                used_prefixes.insert(prefix.to_string());
            }
        }
    }

    for prefix in used_prefixes {
        if !declared.contains_key(prefix.as_str()) {
            violations.push(format!("root node is missing xmlns:{prefix} declaration"));
        }
    }

    for (prefix, uri) in canonical_namespaces() {
        if let Some(actual) = declared.get(prefix)
            && *actual != uri
        {
            violations.push(format!("xmlns:{prefix} must be '{uri}', got '{actual}'"));
        }
    }
}

fn validate_form_structure(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    let Some(root) = doc.nodes.first() else {
        return;
    };
    if root.tag != "jcr:root" {
        violations.push(format!(
            "form root element must be jcr:root, got {}",
            root.tag
        ));
        return;
    }

    let Some(content_index) = find_direct_child(doc, 0, "jcr:content") else {
        violations.push("jcr:root must have a direct jcr:content child".into());
        return;
    };
    let content = &doc.nodes[content_index];

    require_attr_value(content, "jcr:primaryType", "cq:PageContent", violations);
    require_non_empty_attr(content, "sling:resourceType", violations);
    require_non_empty_attr(content, "cq:template", violations);
    if let Some(template) = content.attributes.get("cq:template")
        && !template.starts_with('/')
    {
        violations.push(format!(
            "{} has cq:template that is not an absolute JCR path: {}",
            content.path, template
        ));
    }

    let Some(guide_index) = find_direct_child(doc, content_index, "guideContainer") else {
        violations.push(format!("{} must contain guideContainer", content.path));
        return;
    };
    let guide = &doc.nodes[guide_index];
    require_attr_value(guide, "jcr:primaryType", "nt:unstructured", violations);
    require_attr_value(guide, "guideNodeClass", "guideContainerNode", violations);
    require_non_empty_attr(guide, "sling:resourceType", violations);
    if let Some(resource_type) = guide.attributes.get("sling:resourceType")
        && !resource_type.contains("guideContainer")
    {
        violations.push(format!(
            "{} must use a guideContainer sling:resourceType, got {}",
            guide.path, resource_type
        ));
    }
    require_non_empty_attr(guide, "fd:version", violations);
    require_direct_child(doc, guide_index, "layout", violations);
    let Some(root_panel_index) = find_direct_child(doc, guide_index, "rootPanel") else {
        violations.push(format!("{} must contain rootPanel", guide.path));
        return;
    };

    let root_panel = &doc.nodes[root_panel_index];
    require_attr_value(root_panel, "jcr:primaryType", "nt:unstructured", violations);
    require_attr_value(root_panel, "guideNodeClass", "rootPanelNode", violations);
    require_non_empty_attr(root_panel, "sling:resourceType", violations);
    if let Some(resource_type) = root_panel.attributes.get("sling:resourceType")
        && !resource_type.contains("panel")
        && !resource_type.contains("rootPanel")
    {
        violations.push(format!(
            "{} must use a panel/rootPanel sling:resourceType, got {}",
            root_panel.path, resource_type
        ));
    }
    require_direct_child(doc, root_panel_index, "layout", violations);
    require_direct_child(doc, root_panel_index, "items", violations);
    require_direct_child(doc, root_panel_index, "toolbar", violations);

    validate_responsive_nodes(doc, violations);
}

fn validate_dam_structure(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    let Some(root) = doc.nodes.first() else {
        return;
    };
    if root.tag != "jcr:root" {
        violations.push(format!(
            "DAM root element must be jcr:root, got {}",
            root.tag
        ));
        return;
    }

    let Some(content_index) = find_direct_child(doc, 0, "jcr:content") else {
        violations.push("DAM jcr:root must have a direct jcr:content child".into());
        return;
    };
    let content = &doc.nodes[content_index];
    require_attr_value(content, "jcr:primaryType", "dam:AssetContent", violations);
    require_non_empty_attr(content, "sling:resourceType", violations);

    if find_direct_child(doc, content_index, "metadata").is_none() {
        violations.push(format!("{} must contain metadata child", content.path));
    }
}

fn validate_jcr_primary_types(
    doc: &ParsedXmlDocument,
    kind: XmlKind,
    violations: &mut Vec<String>,
) {
    let Some(root) = doc.nodes.first() else {
        return;
    };
    let expected_root_type = match kind {
        XmlKind::Form => "cq:Page",
        XmlKind::Dam => "dam:Asset",
    };
    require_attr_value(root, "jcr:primaryType", expected_root_type, violations);

    for node in &doc.nodes {
        match node.tag.as_str() {
            "layout" | "items" | "toolbar" | "guideContainer" | "rootPanel" | "cq:responsive"
            | "default" | "fd:rules" | "fd:scripts" | "metadata" => {
                require_attr_value(node, "jcr:primaryType", "nt:unstructured", violations);
            }
            _ => {}
        }

        if node.attributes.contains_key("guideNodeClass") {
            require_attr_value(node, "jcr:primaryType", "nt:unstructured", violations);
        }
    }
}

fn validate_sling_resource_types(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    for node in &doc.nodes {
        if node.attributes.contains_key("guideNodeClass") {
            require_non_empty_attr(node, "sling:resourceType", violations);
        }

        if matches!(node.tag.as_str(), "layout" | "items") && has_container_parent(doc, node) {
            require_non_empty_attr(node, "sling:resourceType", violations);
        }

        if let Some(resource_type) = node.attributes.get("sling:resourceType")
            && resource_type.trim().is_empty()
        {
            violations.push(format!("{} has empty sling:resourceType", node.path));
        }
    }
}

fn validate_fd_contract(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    for node in &doc.nodes {
        if node.tag == "fd:scripts" {
            require_attr_value(node, "jcr:primaryType", "nt:unstructured", violations);
            for key in [
                "fd:click",
                "fd:init",
                "fd:calculate",
                "fd:validate",
                "fd:valueCommit",
                "fd:navigationChange",
            ] {
                if let Some(value) = node.attributes.get(key) {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        violations
                            .push(format!("{} attribute {} must not be empty", node.path, key));
                    }
                    let looks_script_payload = (trimmed.starts_with('{') && trimmed.ends_with('}'))
                        || (trimmed.starts_with('[') && trimmed.ends_with(']'));
                    if !looks_script_payload || !trimmed.contains('{') || !trimmed.contains('}') {
                        violations.push(format!(
                            "{} attribute {} must look like an fd script payload, got {}",
                            node.path, key, value
                        ));
                    }
                }
            }
        }

        if node.tag == "fd:rules" {
            require_attr_value(node, "jcr:primaryType", "nt:unstructured", violations);
        }
    }
}

/// Validate that the script payloads on `fd:scripts` nodes are correctly
/// escaped.
///
/// Script event handlers (`fd:click`, `fd:visible`, `fd:valueCommit`, ...) are
/// stored as a JSON payload inside an XML attribute value, wrapped in two
/// layers of escaping:
///
/// 1. XML entity escaping for `<`, `>`, `&` and `"` (`<` -> `&lt;`,
///    `&&` -> `&amp;&amp;`, JSON delimiters -> `&quot;`, ...). This layer is
///    already reversed by the parser, so [`ParsedXmlNode::attributes`] holds
///    the entity-decoded value.
/// 2. FileVault backslash escaping for the characters that are significant to
///    the `.content.xml` multi-value syntax (commas as `\,`, backslashes as
///    `\\`). JavaScript newlines therefore appear as `\\n`, which decodes to
///    the JSON escape `\n`.
///
/// Reversing the FileVault escaping must yield a well-formed JSON document.
/// A parse failure means the embedded JavaScript was not escaped correctly —
/// e.g. a raw (un-escaped) newline, an unescaped double quote inside a string
/// literal, or a stray `&`/`<` — which would corrupt the form when AEM loads
/// it. The generic XML checks in [`validate_xml_wellformed`] cannot catch
/// these, because the broken payload still sits inside a syntactically valid
/// attribute value.
fn validate_script_escaping(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    for node in &doc.nodes {
        if node.tag != "fd:scripts" {
            continue;
        }
        for (key, value) in &node.attributes {
            // Every `fd:` attribute on an `fd:scripts` node is a script
            // handler; `jcr:primaryType` and friends are left untouched.
            if !key.starts_with("fd:") {
                continue;
            }
            let trimmed = value.trim();
            // Only inspect values that look like a JSON script payload; other
            // scalar `fd:` attributes are validated elsewhere.
            if !(trimmed.starts_with('[') || trimmed.starts_with('{')) {
                continue;
            }
            let unescaped = filevault_unescape(trimmed);
            if let Err(e) = serde_json::from_str::<serde_json::Value>(&unescaped) {
                violations.push(format!(
                    "{} attribute {} is not correctly escaped: script payload does not parse as \
                     JSON after reversing FileVault escaping ({e})",
                    node.path, key
                ));
            }
        }
    }
}

/// Reverse `.content.xml` (FileVault) backslash escaping: a backslash escapes
/// the following character (`\,` -> `,`, `\\` -> `\`).
fn filevault_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn validate_typed_values(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    for node in &doc.nodes {
        for (key, value) in &node.attributes {
            if let Some(suffix) = value.strip_prefix("{Boolean}")
                && suffix != "true"
                && suffix != "false"
            {
                violations.push(format!(
                    "{} attribute {} has invalid boolean typed value {}",
                    node.path, key, value
                ));
            }
            if let Some(suffix) = value.strip_prefix("{Long}")
                && (suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()))
            {
                violations.push(format!(
                    "{} attribute {} has invalid long typed value {}",
                    node.path, key, value
                ));
            }
            if let Some(suffix) = value.strip_prefix("{Date}")
                && !looks_like_iso_datetime(suffix)
            {
                violations.push(format!(
                    "{} attribute {} has invalid date typed value {}",
                    node.path, key, value
                ));
            }
            if value.starts_with('[') && !value.ends_with(']') {
                violations.push(format!(
                    "{} attribute {} has unbalanced array-like value {}",
                    node.path, key, value
                ));
            }
        }

        for key in ["visible", "enabled", "validateOnStepCompletion"] {
            if let Some(value) = node.attributes.get(key) {
                let is_valid = matches!(
                    value.as_str(),
                    "{Boolean}true" | "{Boolean}false" | "true" | "false"
                );
                if !is_valid {
                    violations.push(format!(
                        "{} attribute {} must be a boolean-like value, got {}",
                        node.path, key, value
                    ));
                }
            }
        }

        if let Some(value) = node.attributes.get("mandatory")
            && value != "true"
            && value != "false"
        {
            violations.push(format!(
                "{} attribute mandatory must be 'true' or 'false', got {}",
                node.path, value
            ));
        }
    }
}

fn validate_guide_node_classes(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    let valid_classes = [
        "guideContainerNode",
        "rootPanelNode",
        "guidePanel",
        "guideTextBox",
        "guideTextDraw",
        "guideCheckBox",
        "guideRadioButton",
        "guideDropDownList",
        "guideDatePicker",
        "guideButton",
        "guideNumericBox",
        // The telephone component's own class. An email field is a
        // `guideTextBox` (the corpus is unanimous), so it needs no entry.
        "guideTelephone",
        "guideToolbar",
        "guideFootnotePlaceHolder",
    ];

    for node in &doc.nodes {
        if let Some(class_name) = node.attributes.get("guideNodeClass")
            && !valid_classes.contains(&class_name.as_str())
        {
            violations.push(format!(
                "{} has unknown guideNodeClass {}",
                node.path, class_name
            ));
        }
    }
}

fn validate_repeatables(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    for node in &doc.nodes {
        if node.attributes.contains_key("minOccur") || node.attributes.contains_key("maxOccur") {
            require_attr_value(node, "jcr:primaryType", "nt:unstructured", violations);
            require_non_empty_attr(node, "sling:resourceType", violations);

            let min = node
                .attributes
                .get("minOccur")
                .and_then(|value| value.parse::<u32>().ok());
            let max = node
                .attributes
                .get("maxOccur")
                .and_then(|value| value.parse::<u32>().ok());
            if min.is_none() {
                violations.push(format!("{} has invalid minOccur", node.path));
            }
            if max.is_none() {
                violations.push(format!("{} has invalid maxOccur", node.path));
            }
            if let (Some(min), Some(max)) = (min, max)
                && min > max
            {
                violations.push(format!(
                    "{} has minOccur {} greater than maxOccur {}",
                    node.path, min, max
                ));
            }

            let Some(items_index) = find_direct_child_by_tag(doc, node, "items") else {
                violations.push(format!("{} must contain items child", node.path));
                continue;
            };

            // Add and Remove are required exactly when the panel NAMES them,
            // not unconditionally. A signature twin repeats in step with the
            // party's data panel and is driven by that panel's buttons: it
            // declares neither `addButton` nor `removeButton` and carries
            // neither, because a Remove of its own is what would let the two
            // desync (PROBLEM-repeating-panel section 8). The two halves have to
            // agree in both directions -- a named button that is missing cannot
            // be clicked, and a button nothing names cannot do anything.

            let remove_button = doc.nodes[items_index]
                .children
                .iter()
                .map(|index| &doc.nodes[*index])
                .find(|child| {
                    child
                        .attributes
                        .get("name")
                        .is_some_and(|name| name == "BT_Remove")
                });
            match (remove_button, node.attributes.contains_key("removeButton")) {
                (Some(button), true) => validate_repeatable_button_child(
                    doc,
                    button,
                    "fd:click",
                    "removeInstance",
                    violations,
                ),
                (None, true) => violations.push(format!(
                    "{} names removeButton but its items hold no BT_Remove",
                    node.path
                )),
                (Some(button), false) => violations.push(format!(
                    "{} holds {} but names no removeButton, so nothing drives it",
                    node.path, button.path
                )),
                // A driven twin: no `removeButton` property and no button.
                (None, false) => {}
            }

            let add_button = doc.nodes[node.parent.unwrap_or_default()]
                .children
                .iter()
                .map(|index| &doc.nodes[*index])
                .find(|child| {
                    child
                        .attributes
                        .get("name")
                        .is_some_and(|name| name == "BT_Add")
                });
            match (add_button, node.attributes.contains_key("addButton")) {
                (Some(button), true) => {
                    validate_repeatable_button_child(
                        doc,
                        button,
                        "fd:click",
                        "addInstance",
                        violations,
                    );
                    let Some(scripts) = find_direct_child_by_tag(doc, button, "fd:scripts") else {
                        violations.push(format!("{} must contain fd:scripts child", button.path));
                        continue;
                    };
                    if !doc.nodes[scripts].attributes.contains_key("fd:init") {
                        violations.push(format!(
                            "{} must contain fd:init script",
                            doc.nodes[scripts].path
                        ));
                    }
                }
                (None, true) => violations.push(format!(
                    "{} names addButton but has no BT_Add button sibling",
                    node.path
                )),
                (Some(button), false) => violations.push(format!(
                    "{} has the sibling {} but names no addButton, so nothing drives it",
                    node.path, button.path
                )),
                // A driven twin: no `addButton` property and no button.
                (None, false) => {}
            }
        }
    }
}

fn validate_responsive_nodes(doc: &ParsedXmlDocument, violations: &mut Vec<String>) {
    for node in &doc.nodes {
        if node.tag == "cq:responsive" {
            let Some(default_index) = find_direct_child_by_tag(doc, node, "default") else {
                violations.push(format!("{} must contain default child", node.path));
                continue;
            };
            let default_node = &doc.nodes[default_index];
            require_non_empty_attr(default_node, "offset", violations);
            require_non_empty_attr(default_node, "width", violations);
        }
    }
}

fn validate_repeatable_button_child(
    doc: &ParsedXmlDocument,
    button: &ParsedXmlNode,
    script_attr: &str,
    needle: &str,
    violations: &mut Vec<String>,
) {
    let Some(scripts_index) = find_direct_child_by_tag(doc, button, "fd:scripts") else {
        violations.push(format!("{} must contain fd:scripts child", button.path));
        return;
    };
    let scripts = &doc.nodes[scripts_index];
    require_attr_value(scripts, "jcr:primaryType", "nt:unstructured", violations);
    let Some(payload) = scripts.attributes.get(script_attr) else {
        violations.push(format!("{} must contain {}", scripts.path, script_attr));
        return;
    };
    if !payload.contains(needle) {
        violations.push(format!(
            "{} {} must reference {}",
            scripts.path, script_attr, needle
        ));
    }
}

fn require_direct_child(
    doc: &ParsedXmlDocument,
    parent_index: usize,
    tag: &str,
    violations: &mut Vec<String>,
) {
    if find_direct_child(doc, parent_index, tag).is_none() {
        violations.push(format!(
            "{} must contain direct child {}",
            doc.nodes[parent_index].path, tag
        ));
    }
}

fn require_non_empty_attr(node: &ParsedXmlNode, attr: &str, violations: &mut Vec<String>) {
    match node.attributes.get(attr) {
        Some(value) if !value.trim().is_empty() => {}
        Some(_) => violations.push(format!("{} has empty {}", node.path, attr)),
        None => violations.push(format!("{} is missing {}", node.path, attr)),
    }
}

fn require_attr_value(
    node: &ParsedXmlNode,
    attr: &str,
    expected: &str,
    violations: &mut Vec<String>,
) {
    match node.attributes.get(attr) {
        Some(value) if value == expected => {}
        Some(value) => violations.push(format!(
            "{} must have {}='{}', got '{}'",
            node.path, attr, expected, value
        )),
        None => violations.push(format!("{} is missing {}", node.path, attr)),
    }
}

fn find_direct_child(doc: &ParsedXmlDocument, parent_index: usize, tag: &str) -> Option<usize> {
    doc.nodes[parent_index]
        .children
        .iter()
        .copied()
        .find(|child| doc.nodes[*child].tag == tag)
}

fn find_direct_child_by_tag(
    doc: &ParsedXmlDocument,
    parent: &ParsedXmlNode,
    tag: &str,
) -> Option<usize> {
    let parent_index = doc
        .nodes
        .iter()
        .position(|node| std::ptr::eq(node, parent))
        .expect("parent node should exist in parsed document");
    find_direct_child(doc, parent_index, tag)
}

fn has_container_parent(doc: &ParsedXmlDocument, node: &ParsedXmlNode) -> bool {
    node.parent
        .map(|parent_index| {
            doc.nodes[parent_index]
                .attributes
                .contains_key("guideNodeClass")
                || matches!(
                    doc.nodes[parent_index].tag.as_str(),
                    "guideContainer" | "rootPanel"
                )
        })
        .unwrap_or(false)
}

fn xml_prefix(value: &str) -> Option<&str> {
    value.split_once(':').map(|(prefix, _)| prefix)
}

fn canonical_namespaces() -> [(&'static str, &'static str); 6] {
    [
        ("jcr", "http://www.jcp.org/jcr/1.0"),
        ("sling", "http://sling.apache.org/jcr/sling/1.0"),
        ("cq", "http://www.day.com/jcr/cq/1.0"),
        ("nt", "http://www.jcp.org/jcr/nt/1.0"),
        ("fd", "http://www.adobe.com/aemfd/fd/1.0"),
        ("dam", "http://www.day.com/dam/1.0"),
    ]
}

fn looks_like_iso_datetime(value: &str) -> bool {
    value.contains('T')
        && value.chars().take(4).all(|c| c.is_ascii_digit())
        && (value.ends_with('Z') || value.contains('+') || value.rfind('-').is_some())
}

/// Verify that every `&` in the XML is part of a valid entity or character
/// reference. Returns `Err` with context if an unescaped `&` is found.
fn check_no_unescaped_ampersands(xml: &str) -> Result<(), String> {
    let bytes = xml.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'&' {
            continue;
        }
        // Look ahead for `;`
        let rest = &xml[i + 1..];
        if let Some(semi) = rest.find(';') {
            let entity = &rest[..semi];
            // Permit named entities and numeric references
            if matches!(entity, "amp" | "lt" | "gt" | "quot" | "apos") || entity.starts_with('#') {
                continue;
            }
        }
        // Unescaped `&`
        let start = i.saturating_sub(100);
        let end = (i + 100).min(xml.len());
        let snippet = &xml[start..end];
        return Err(format!(
            "Unescaped '&' at byte {i} in XML output.\n\nContext:\n{snippet}"
        ));
    }
    Ok(())
}

/// Verify that no `<` or `>` appear inside XML attribute values.
/// Returns `Err` with context if a raw angle bracket is found in an attribute.
fn check_no_unescaped_angle_brackets_in_attributes(xml: &str) -> Result<(), String> {
    // Simple state machine: track whether we are inside an opening tag
    // (between `<tagname` and `>`) and inside a quoted attribute value.
    let bytes = xml.as_bytes();
    let mut in_tag = false;
    let mut quote_char: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote_char {
            // Inside a quoted attribute value
            if b == q {
                quote_char = None;
            } else if b == b'<' {
                let start = i.saturating_sub(100);
                let end = (i + 100).min(xml.len());
                let snippet = &xml[start..end];
                return Err(format!(
                    "Unescaped '<' inside XML attribute value at byte {i}.\n\nContext:\n{snippet}",
                ));
            }
            i += 1;
            continue;
        }
        if in_tag {
            if b == b'"' || b == b'\'' {
                quote_char = Some(b);
            } else if b == b'>' || (b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'>') {
                in_tag = false;
            }
        } else if b == b'<' {
            // Detect start of an opening/self-closing tag (skip comments, CDATA, PI)
            if i + 1 < bytes.len() && bytes[i + 1] != b'!' && bytes[i + 1] != b'?' {
                in_tag = true;
            }
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod repeatable_button_tests {
    use super::*;

    /// A repeating panel, with the Add button as its sibling and the Remove
    /// button among its items, the way `repeatable.xml` lays them out.
    /// `add`/`remove` control whether the panel NAMES each button and whether
    /// the button element is there.
    fn form_with_repeatable(add: bool, remove: bool, add_elem: bool, remove_elem: bool) -> String {
        let add_attr = if add { r#"addButton="BT_Add""# } else { "" };
        let remove_attr = if remove {
            r#"removeButton="BT_Remove""#
        } else {
            ""
        };
        let add_button = if add_elem {
            r#"<bt_add jcr:primaryType="nt:unstructured"
                   sling:resourceType="fd/af/components/guidebutton" name="BT_Add">
                   <fd:scripts jcr:primaryType="nt:unstructured"
                       fd:click="window.forms.ubs.addInstance(this);" fd:init="1"/>
               </bt_add>"#
        } else {
            ""
        };
        let remove_button = if remove_elem {
            r#"<bt_remove jcr:primaryType="nt:unstructured"
                   sling:resourceType="fd/af/components/guidebutton" name="BT_Remove">
                   <fd:scripts jcr:primaryType="nt:unstructured"
                       fd:click="window.forms.ubs.removeInstance(this);"/>
               </bt_remove>"#
        } else {
            ""
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
    xmlns:cq="http://www.day.com/jcr/cq/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    xmlns:nt="http://www.jcp.org/jcr/nt/1.0" jcr:primaryType="cq:Page">
    <jcr:content jcr:primaryType="cq:PageContent">
        <guideContainer jcr:primaryType="nt:unstructured"
            sling:resourceType="fd/af/components/guideContainer" name="guide1">
            <rootPanel jcr:primaryType="nt:unstructured"
                sling:resourceType="fd/af/components/rootPanel" name="guideRootPanel">
                <items jcr:primaryType="nt:unstructured">
                    <wrapper jcr:primaryType="nt:unstructured"
                        sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                        guideNodeClass="guidePanel" name="PN_Party">
                        <items jcr:primaryType="nt:unstructured">
                            <repeatableInner jcr:primaryType="nt:unstructured"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                                guideNodeClass="guidePanel" name="RCP_Party_repeat"
                                minOccur="1" maxOccur="4" {add_attr} {remove_attr}>
                                <items jcr:primaryType="nt:unstructured">{remove_button}</items>
                            </repeatableInner>
                            {add_button}
                        </items>
                    </wrapper>
                </items>
            </rootPanel>
        </guideContainer>
    </jcr:content>
</jcr:root>
"#
        )
    }

    fn button_violations(xml: &str) -> Vec<String> {
        let doc = parse_xml_document(xml);
        let mut violations = Vec::new();
        validate_repeatables(&doc, &mut violations);
        violations
    }

    /// An ordinary repeating panel names both buttons and has both.
    #[test]
    fn a_panel_that_names_both_buttons_and_has_them_is_clean() {
        let v = button_violations(&form_with_repeatable(true, true, true, true));
        assert!(v.is_empty(), "expected no violations, got {v:?}");
    }

    /// The regression: a signature twin is driven by its party's data panel, so
    /// it names neither button and carries neither. The validator used to demand
    /// both unconditionally and rejected the engine's own output -- which broke
    /// `passthrough_load_save_load_is_a_fixpoint` on `Germany_AACR.zip` from
    /// commit be89c65 onward.
    #[test]
    fn a_driven_twin_that_names_no_buttons_is_clean() {
        let v = button_violations(&form_with_repeatable(false, false, false, false));
        assert!(
            v.is_empty(),
            "a driven twin has no buttons of its own: {v:?}"
        );
    }

    /// Both halves still have to agree. A named button that is not there cannot
    /// be clicked.
    #[test]
    fn a_named_button_that_is_missing_is_reported() {
        let v = button_violations(&form_with_repeatable(true, true, false, false));
        assert_eq!(v.len(), 2, "one per missing button: {v:?}");
        assert!(v.iter().any(|m| m.contains("names removeButton")), "{v:?}");
        assert!(v.iter().any(|m| m.contains("names addButton")), "{v:?}");
    }

    /// And a button nothing names cannot do anything -- on a twin it is exactly
    /// the Remove that would let the two panels desync.
    #[test]
    fn a_button_the_panel_does_not_name_is_reported() {
        let v = button_violations(&form_with_repeatable(false, false, true, true));
        assert_eq!(v.len(), 2, "one per undriven button: {v:?}");
        assert!(
            v.iter().any(|m| m.contains("names no removeButton")),
            "{v:?}"
        );
        assert!(v.iter().any(|m| m.contains("names no addButton")), "{v:?}");
    }
}
