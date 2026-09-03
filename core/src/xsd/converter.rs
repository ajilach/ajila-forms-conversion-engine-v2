//! Provisional bind paths derived from the structured tree.
//!
//! These are **not** the `bindRef`s a form ships with — those come from
//! [`super::from_aem`], which walks the finished AEM tree and emits the schema
//! and the bindings together.
//!
//! This module exists only because fragment matching needs bind paths *before*
//! the AEM tree is final: [`crate::aem::convert_to_aem`] matches a panel's
//! children against the fragment library by their bind-path leaf names, and it
//! has to do that while it is still building the tree. So the paths here are
//! computed from the structured tree, used for matching, and then discarded and
//! replaced by the authoritative ones.
//!
//! Headings nest sections (H1..H6), and a section whose children cover one or
//! more registered complex types is matched to them, mirroring the shape the
//! fragment matcher expects.

use crate::structured::{FieldId, GroupNode, HeadingNode, StructuredNode};

use super::{
    XsdConfig, find_matching_types, resolve_element, resolve_section_name_with_heading,
    to_pascal_case,
};

// ============================================================================
// Heading hierarchy
// ============================================================================

/// A section created by splitting nodes at heading boundaries.
///
/// Each section has a heading (except possibly the root-level preamble)
/// and a list of child items: either nested sub-sections or leaf nodes.
#[derive(Debug)]
struct Section {
    /// The heading that starts this section (None for top-level preamble).
    heading: Option<HeadingNode>,
    /// Direct child nodes that are not sub-sections (fields, etc.).
    children: Vec<SectionItem>,
}

/// An item within a section — either a nested sub-section or a leaf node.
#[derive(Debug)]
enum SectionItem {
    SubSection(Section),
    Node(StructuredNode),
}

/// Build a heading-based hierarchy from a flat list of structured nodes.
///
/// Headings split the list into nested sections where heading level determines
/// nesting depth: H1 is the outermost, H6 the innermost. Non-heading nodes
/// become children of the most recently opened section.
fn build_heading_hierarchy(nodes: &[StructuredNode]) -> Vec<Section> {
    let mut stack: Vec<(u8, Section)> = Vec::new();
    let mut result: Vec<Section> = Vec::new();

    // Track the top-level items before any heading
    let mut preamble_items: Vec<SectionItem> = Vec::new();

    for node in nodes {
        match node {
            StructuredNode::Heading(heading) => {
                let level = heading.level.as_u8();

                // Pop sections from the stack that are at the same or deeper level
                while let Some((stack_level, _)) = stack.last() {
                    if *stack_level >= level {
                        let (_, completed) = stack.pop().unwrap();
                        // Add completed section as a sub-section of the new parent
                        if let Some((_, parent)) = stack.last_mut() {
                            parent.children.push(SectionItem::SubSection(completed));
                        } else {
                            result.push(completed);
                        }
                    } else {
                        break;
                    }
                }

                // Push a new section for this heading
                stack.push((
                    level,
                    Section {
                        heading: Some(heading.clone()),
                        children: Vec::new(),
                    },
                ));
            }

            StructuredNode::Group(group) => {
                // Recurse into groups to find headings inside them
                let sub_items = flatten_group_for_hierarchy(group);
                for item in sub_items {
                    match item {
                        FlatItem::Heading(heading) => {
                            let level = heading.level.as_u8();

                            while let Some((stack_level, _)) = stack.last() {
                                if *stack_level >= level {
                                    let (_, completed) = stack.pop().unwrap();
                                    if let Some((_, parent)) = stack.last_mut() {
                                        parent.children.push(SectionItem::SubSection(completed));
                                    } else {
                                        result.push(completed);
                                    }
                                } else {
                                    break;
                                }
                            }

                            stack.push((
                                level,
                                Section {
                                    heading: Some(heading),
                                    children: Vec::new(),
                                },
                            ));
                        }
                        FlatItem::Node(n) => {
                            if let Some((_, section)) = stack.last_mut() {
                                section.children.push(SectionItem::Node(n));
                            } else {
                                preamble_items.push(SectionItem::Node(n));
                            }
                        }
                    }
                }
            }

            other => {
                if let Some((_, section)) = stack.last_mut() {
                    section.children.push(SectionItem::Node(other.clone()));
                } else {
                    preamble_items.push(SectionItem::Node(other.clone()));
                }
            }
        }
    }

    // Drain remaining stack
    while let Some((_, completed)) = stack.pop() {
        if let Some((_, parent)) = stack.last_mut() {
            parent.children.push(SectionItem::SubSection(completed));
        } else {
            result.push(completed);
        }
    }

    // If there were nodes before any heading, wrap them in a preamble section
    if !preamble_items.is_empty() {
        let mut all = vec![Section {
            heading: None,
            children: preamble_items,
        }];
        all.extend(result);
        result = all;
    }

    result
}

/// Items produced when flattening a GroupNode for hierarchy building.
enum FlatItem {
    Heading(HeadingNode),
    Node(StructuredNode),
}

/// Recursively flatten a GroupNode, extracting headings and other nodes.
fn flatten_group_for_hierarchy(group: &GroupNode) -> Vec<FlatItem> {
    let mut items = Vec::new();
    for child in &group.children {
        match child {
            StructuredNode::Heading(h) => items.push(FlatItem::Heading(h.clone())),
            StructuredNode::Group(g) => {
                items.extend(flatten_group_for_hierarchy(g));
            }
            other => items.push(FlatItem::Node(other.clone())),
        }
    }
    items
}

// ============================================================================
// Section text extraction (for keyword/regex matching)
// ============================================================================

/// Collect all plain text from a section (heading + all children) into a
/// single string suitable for keyword/regex matching.
fn section_full_text(section: &Section, config: &XsdConfig) -> String {
    let mut parts = Vec::new();
    if let Some(heading) = &section.heading {
        parts.push(config.label_text(&heading.content));
    }
    for item in &section.children {
        collect_item_text(item, config, &mut parts);
    }
    parts.join(" ")
}

/// Recursively collect text from a SectionItem.
fn collect_item_text(item: &SectionItem, config: &XsdConfig, parts: &mut Vec<String>) {
    match item {
        SectionItem::SubSection(sub) => {
            if let Some(heading) = &sub.heading {
                parts.push(config.label_text(&heading.content));
            }
            for child in &sub.children {
                collect_item_text(child, config, parts);
            }
        }
        SectionItem::Node(node) => {
            collect_node_text(node, config, parts);
        }
    }
}

/// Recursively collect text from a StructuredNode.
fn collect_node_text(node: &StructuredNode, config: &XsdConfig, parts: &mut Vec<String>) {
    match node {
        // Raw markup carries no label the schema could name.
        StructuredNode::Html(_) => {}
        StructuredNode::Heading(h) => {
            parts.push(config.label_text(&h.content));
        }
        StructuredNode::Paragraph(p) => {
            parts.push(config.label_text(&p.content));
        }
        StructuredNode::Field(f) => {
            if let Some(label) = &f.label {
                parts.push(config.label_text(label));
            }
        }
        StructuredNode::Group(g) => {
            for child in &g.children {
                collect_node_text(child, config, parts);
            }
        }
        StructuredNode::Repeatable(r) => {
            collect_node_text(&r.item, config, parts);
        }
        StructuredNode::Conditional(c) => {
            collect_node_text(&c.content, config, parts);
        }
        StructuredNode::GridLayout(grid) => {
            for elem in &grid.elements {
                collect_node_text(&elem.node, config, parts);
            }
        }
        StructuredNode::List(list) => {
            for item in &list.items {
                parts.push(config.label_text(&item.content));
            }
        }
        StructuredNode::Table(table) => {
            if let Some(caption) = &table.caption {
                parts.push(config.label_text(caption));
            }
        }
        StructuredNode::Footnote(n) => {
            parts.push(config.label_text(&n.content));
        }
        StructuredNode::Image(_) | StructuredNode::Notice(_) | StructuredNode::Empty => {}
    }
}

// ============================================================================
// Section → XsdNode building
// ============================================================================

/// Collect resolved (name, type) pairs for the direct children of a section.
///
/// For fields, resolves against `[elements]` config. Unmatched items use
/// snake_case from the label with `xs:string` as the default type.
fn collect_child_name_type_pairs(section: &Section, config: &XsdConfig) -> Vec<(String, String)> {
    let mut pairs = Vec::new();

    for item in &section.children {
        match item {
            SectionItem::SubSection(_) => {
                // Sub-sections are headings, not simple elements — skip for matching
            }
            SectionItem::Node(node) => {
                collect_node_name_type_pairs(node, config, &mut pairs);
            }
        }
    }

    pairs
}

/// Collect (name, type) pairs from a single node.
fn collect_node_name_type_pairs(
    node: &StructuredNode,
    config: &XsdConfig,
    pairs: &mut Vec<(String, String)>,
) {
    match node {
        StructuredNode::Field(field) => {
            let label = field
                .label
                .as_ref()
                .map(|l| config.label_text(l))
                .unwrap_or_default();
            let (name, type_ref) = match resolve_element(&label, &config.profile) {
                Some(res) => (res.name, res.type_ref),
                None => (to_pascal_case(&label), "xs:string".to_string()),
            };
            pairs.push((name, type_ref));
        }
        StructuredNode::Repeatable(rep) => {
            collect_node_name_type_pairs(&rep.item, config, pairs);
        }
        StructuredNode::Group(group) => {
            for child in &group.children {
                collect_node_name_type_pairs(child, config, pairs);
            }
        }
        StructuredNode::GridLayout(grid) => {
            for elem in &grid.elements {
                collect_node_name_type_pairs(&elem.node, config, pairs);
            }
        }
        _ => {}
    }
}

// ============================================================================
// BindRef computation (provisional; input to fragment matching)
// ============================================================================

/// Maps produced by [`compute_bind_refs`] and consumed by the AEM converter
/// to attach `bindRef` attributes to form components.
pub struct BindRefMaps {
    /// Maps a [`FieldId`] to its absolute XSD path (e.g. `/form/section/field_name`).
    pub fields: std::collections::HashMap<FieldId, String>,
    /// Maps a heading's plain-text label to the XSD path of the corresponding
    /// complex type element (e.g. `"Personal Data"` → `/form/personal_data`).
    /// Used to look up the bind path for AEM section panels.
    pub sections: std::collections::HashMap<String, String>,
}

/// Compute provisional bind-ref paths for all fields and heading sections.
///
/// These feed fragment matching in [`crate::aem::convert_to_aem`] and are then
/// discarded — the paths a form ships with come from
/// [`crate::xsd::generate_xsd_from_aem`], which walks the finished AEM tree.
pub fn compute_bind_refs(nodes: &[StructuredNode], config: &super::XsdConfig) -> BindRefMaps {
    let mut maps = BindRefMaps {
        fields: std::collections::HashMap::new(),
        sections: std::collections::HashMap::new(),
    };
    // Root all bind-ref paths at the fragment bind-ref prefix so the actual
    // field/section bindRefs share the same root as fragment bindRefs.
    // When `fragmentBindRefPrefix` is unset it falls back to the XSD root
    // element name, so the paths still line up with the generated schema.
    let root_path = format!("/{}", config.fragment_bind_ref_prefix());
    let sections = build_heading_hierarchy(nodes);
    for section in &sections {
        collect_section_bind_refs(section, &root_path, config, &mut maps);
    }
    maps
}

/// Recursively accumulate bind-ref paths for a single [`Section`].
fn collect_section_bind_refs(
    section: &Section,
    parent_path: &str,
    config: &super::XsdConfig,
    maps: &mut BindRefMaps,
) {
    let current_path = match &section.heading {
        Some(heading) => {
            let label = config.label_text(&heading.content);
            let heading_text = config.label_text(&heading.content);
            let name = resolve_section_name_with_heading(
                &section_full_text(section, config),
                Some(&heading_text),
                &config.profile,
            )
            .unwrap_or_else(|| to_pascal_case(&label));
            let path = format!("{}/{}", parent_path, name);
            // Use or_insert so that the first (shallowest) heading with a
            // given label wins.  The AEM converter only creates panels for
            // top-level H2 headings, so deeper sub-headings with the same
            // text must not overwrite the earlier entry.
            maps.sections
                .entry(label.trim().to_string())
                .or_insert(path.clone());
            path
        }
        None => parent_path.to_string(),
    };

    // Mirror the type-matching logic from build_section: when a section is
    // matched to multiple disjoint types, the XSD generator wraps each type's
    // fields under a wrapper element, adding an extra path level.
    let wrapper_paths = if section.heading.is_some() {
        let child_pairs = collect_child_name_type_pairs(section, config);
        let matched = find_matching_types(&child_pairs, &config.registered_types);
        if matched.len() > 1 {
            Some(build_multi_type_wrapper_paths(
                &matched,
                config,
                &current_path,
            ))
        } else {
            None
        }
    } else {
        None
    };

    collect_section_items_bind_refs(
        &section.children,
        &current_path,
        config,
        maps,
        wrapper_paths.as_ref(),
    );
}

type WrapperPaths = std::collections::HashMap<(String, String), String>;

/// For each matched type, compute the wrapper element path that the XSD
/// generator would produce, and map every child element of that type to it.
fn build_multi_type_wrapper_paths(
    matched: &[&super::RegisteredComplexType],
    config: &super::XsdConfig,
    section_path: &str,
) -> WrapperPaths {
    let mut map = WrapperPaths::new();
    for rt in matched {
        let elem_name = config
            .type_to_element_name
            .get(&rt.name)
            .cloned()
            .unwrap_or_else(|| rt.name.trim_end_matches("Type").to_string());
        let wrapper_path = format!("{}/{}", section_path, elem_name);
        for child in &rt.elements {
            map.insert(
                (child.name.clone(), child.type_ref.clone()),
                wrapper_path.clone(),
            );
        }
    }
    map
}

/// Accumulate bind-ref paths for the direct children of a section.
///
/// When `wrappers` is `Some` (multi-type matched section), field paths are
/// routed through the appropriate wrapper element.
fn collect_section_items_bind_refs(
    items: &[SectionItem],
    current_path: &str,
    config: &super::XsdConfig,
    maps: &mut BindRefMaps,
    wrappers: Option<&WrapperPaths>,
) {
    for item in items {
        match item {
            SectionItem::SubSection(sub) => {
                collect_section_bind_refs(sub, current_path, config, maps);
            }
            SectionItem::Node(node) => {
                collect_node_bind_refs(node, current_path, config, maps, wrappers);
            }
        }
    }
}

/// Accumulate bind-ref paths for a single [`StructuredNode`].
fn collect_node_bind_refs(
    node: &StructuredNode,
    current_path: &str,
    config: &super::XsdConfig,
    maps: &mut BindRefMaps,
    wrappers: Option<&WrapperPaths>,
) {
    match node {
        // Presentation only -- it binds to nothing.
        StructuredNode::Html(_) => {}
        StructuredNode::Field(field) => {
            let label = field
                .label
                .as_ref()
                .map(|l| config.label_text(l))
                .unwrap_or_default();
            let (name, type_ref) = match resolve_element(&label, &config.profile) {
                Some(res) => (res.name, res.type_ref),
                None => (to_pascal_case(&label), "xs:string".to_string()),
            };
            if !name.is_empty() && name != "Unknown" {
                let base_path = wrappers
                    .and_then(|wp| wp.get(&(name.clone(), type_ref)))
                    .map(|s| s.as_str())
                    .unwrap_or(current_path);
                let path = format!("{}/{}", base_path, name);
                maps.fields.insert(field.name.clone(), path);
            }
        }
        StructuredNode::Repeatable(rep) => {
            collect_node_bind_refs(&rep.item, current_path, config, maps, wrappers);
        }
        StructuredNode::Group(group) => {
            for child in &group.children {
                collect_node_bind_refs(child, current_path, config, maps, wrappers);
            }
        }
        StructuredNode::Conditional(cond) => {
            collect_conditional_node_bind_refs(&cond.content, current_path, config, maps, wrappers);
        }
        StructuredNode::GridLayout(grid) => {
            for elem in &grid.elements {
                collect_node_bind_refs(&elem.node, current_path, config, maps, wrappers);
            }
        }
        StructuredNode::Heading(_)
        | StructuredNode::Paragraph(_)
        | StructuredNode::Image(_)
        | StructuredNode::Table(_)
        | StructuredNode::List(_)
        | StructuredNode::Footnote(_)
        | StructuredNode::Notice(_)
        | StructuredNode::Empty => {}
    }
}

/// Accumulate bind-ref paths for the content of a conditional branch.
///
/// Groups are transparent, all other nodes delegate to [`collect_node_bind_refs`].
fn collect_conditional_node_bind_refs(
    node: &StructuredNode,
    current_path: &str,
    config: &super::XsdConfig,
    maps: &mut BindRefMaps,
    wrappers: Option<&WrapperPaths>,
) {
    match node {
        StructuredNode::Group(group) => {
            for child in &group.children {
                collect_conditional_node_bind_refs(child, current_path, config, maps, wrappers);
            }
        }
        _ => {
            collect_node_bind_refs(node, current_path, config, maps, wrappers);
        }
    }
}
