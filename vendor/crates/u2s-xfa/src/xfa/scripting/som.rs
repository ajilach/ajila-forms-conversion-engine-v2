//! SOM (Scripting Object Model) Path and Resolution
//!
//! This module implements SOM path handling per XFA 3.3 spec Chapter 3 (pages 86-120).
//!
//! The resolver here answers the host's absolute paths
//! (`Form.Page.Section.Field`, `Form.Row[2].F`, `Form.Row[*].F`). A script's
//! unqualified and relative references are resolved in JavaScript by the
//! scope walk in `js_helpers` (`XFA_SOM_WALKER`).

use serde::Serialize;

use crate::xfa::{XfaNode, XfaNodeKind};
use std::collections::HashMap;

/// A wrapper for SOM (Scripting Object Model) path expressions.
///
/// SOM paths uniquely identify nodes in the XFA tree hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SomPath(String);

impl SomPath {
    /// Create a new SomPath from a path string
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// Get the full path as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The bare name of the last node in the path, without its instance
    /// index: `Row` for both `A.Row` and `A.Row[2]`.
    pub fn name(&self) -> &str {
        split_segment(self.leaf()).0
    }

    /// The last segment as written, instance index included: `Row[2]`.
    pub fn leaf(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(&self.0)
    }

    /// The instance index of the last node: 0 for `A.Row`, 2 for `A.Row[2]`.
    pub fn index(&self) -> usize {
        split_segment(self.leaf()).1
    }

    /// Each segment as `(name, index)`, root first.
    pub fn segments(&self) -> impl Iterator<Item = (&str, usize)> {
        self.0.split('.').map(split_segment)
    }

    /// The child of this path named `name` at instance `index`.
    pub fn child_indexed(&self, name: &str, index: usize) -> SomPath {
        SomPath::new(child_som_path(&self.0, name, index))
    }

    /// True when no segment names an instance after the first: the path
    /// the node would have if nothing in the form were repeated.
    pub fn is_first_instance(&self) -> bool {
        self.segments().all(|(_, index)| index == 0)
    }

    /// The template path this instance path was made from: every instance
    /// index dropped, so `A.Row[2].F` and `A.Row.F` share `A.Row.F`.
    pub fn index_free(&self) -> SomPath {
        SomPath::new(
            self.0
                .split('.')
                .map(|seg| split_segment(seg).0)
                .collect::<Vec<_>>()
                .join("."),
        )
    }

    /// Get the path components
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }

    /// Get the parent path, if any
    pub fn parent(&self) -> Option<SomPath> {
        self.0
            .rsplit_once('.')
            .map(|(parent, _)| SomPath::new(parent))
    }

    /// Create a child path by appending a name
    pub fn child(&self, name: &str) -> SomPath {
        SomPath::new(format!("{}.{}", self.0, name))
    }

    /// Check if this path starts with another path (is a descendant)
    pub fn starts_with(&self, other: &SomPath) -> bool {
        self.0.starts_with(&other.0)
            && (self.0.len() == other.0.len()
                || self.0.as_bytes().get(other.0.len()) == Some(&b'.'))
    }

    /// Consume and return the inner String
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for SomPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for SomPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for SomPath {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for SomPath {
    fn from(s: String) -> Self {
        SomPath(s)
    }
}

impl From<&str> for SomPath {
    fn from(s: &str) -> Self {
        SomPath(s.to_string())
    }
}

impl std::ops::Deref for SomPath {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// =============================================================================
// Instance-indexed path segments (XFA 3.3 §3 "Referencing Objects by Index")
// =============================================================================
//
// A canonical SOM path names every node by `name` plus its index among the
// same-named siblings under one parent. Index 0 is written without brackets,
// so a form with no repeated siblings has exactly the paths it always had.
// Every path builder in this crate goes through `child_som_path` and
// `sibling_indices`, so no two builders can spell an instance differently.

/// The index part of one SOM *expression* segment: a concrete instance, or
/// `[*]` for every instance. Canonical paths only ever carry `At`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentIndex {
    At(usize),
    All,
}

/// Parse one expression segment: `Row` is `(Row, At(0))`, `Row[2]` is
/// `(Row, At(2))`, `Row[*]` is `(Row, All)`. `None` for a malformed index,
/// such as `Row[x]` or an unclosed bracket.
pub fn parse_segment(segment: &str) -> Option<(&str, SegmentIndex)> {
    let Some(open) = segment.find('[') else {
        return Some((segment, SegmentIndex::At(0)));
    };
    let inner = segment[open + 1..].strip_suffix(']')?;
    let name = &segment[..open];
    match inner.trim() {
        "*" => Some((name, SegmentIndex::All)),
        n => n.parse().ok().map(|i| (name, SegmentIndex::At(i))),
    }
}

/// Split one canonical path segment into `(name, index)`.
///
/// Canonical paths are built only by [`child_som_path`], so a segment that is
/// not `name` or `name[n]` is a construction bug, not input to be tolerated.
pub fn split_segment(segment: &str) -> (&str, usize) {
    match parse_segment(segment) {
        Some((name, SegmentIndex::At(i))) => (name, i),
        _ => panic!("not a canonical SOM path segment: {segment:?}"),
    }
}

/// One canonical segment: `name` for index 0, `name[index]` otherwise.
pub fn segment(name: &str, index: usize) -> String {
    if index == 0 {
        name.to_string()
    } else {
        format!("{name}[{index}]")
    }
}

/// The canonical path of the child `name` at instance `index` under `parent`
/// (an empty `parent` is the root).
pub fn child_som_path(parent: &str, name: &str, index: usize) -> String {
    let seg = segment(name, index);
    if parent.is_empty() {
        seg
    } else {
        format!("{parent}.{seg}")
    }
}

/// Each node's index among the same-named nodes of this one sibling list.
/// Unnamed nodes get 0; they never appear in a path.
pub fn sibling_indices(nodes: &[XfaNode]) -> Vec<usize> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    nodes
        .iter()
        .map(|n| match n.name.as_deref() {
            Some(name) => {
                let count = seen.entry(name).or_insert(0);
                let index = *count;
                *count += 1;
                index
            }
            None => 0,
        })
        .collect()
}

/// Node information for SOM resolution
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub name: String,
    pub path: SomPath,
    pub parent_path: Option<SomPath>,
    pub index: usize,
    pub class_name: String, // "field", "subform", etc.
}

/// SOM (Scripting Object Model) Resolver
/// Implements resolveNode() and resolveNodes() per XFA 3.3 spec Chapter 3
pub struct SomResolver {
    /// All registered nodes indexed by full path
    nodes: HashMap<SomPath, NodeInfo>,
    /// Parent-child relationships
    children: HashMap<SomPath, Vec<SomPath>>,
    /// Every concrete path, grouped by its index-free template path, in
    /// registration (document) order.
    by_template: HashMap<SomPath, Vec<SomPath>>,
}

impl SomResolver {
    pub fn new() -> Self {
        SomResolver {
            nodes: HashMap::new(),
            children: HashMap::new(),
            by_template: HashMap::new(),
        }
    }

    /// Build a SomResolver from XFA nodes
    pub fn from_nodes(xfa_nodes: &[XfaNode]) -> Self {
        let mut resolver = Self::new();

        fn register_recursive(
            resolver: &mut SomResolver,
            nodes: &[XfaNode],
            parent_path: Option<&SomPath>,
        ) {
            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                if let Some(name) = &node.name {
                    let path = SomPath::new(child_som_path(
                        parent_path.map(SomPath::as_str).unwrap_or(""),
                        name,
                        index,
                    ));

                    let class_name = match &node.kind {
                        XfaNodeKind::Field => "field",
                        XfaNodeKind::Subform => "subform",
                        XfaNodeKind::Draw => "draw",
                        XfaNodeKind::Element { tag_name, .. } => tag_name.as_str(),
                        _ => "node",
                    };

                    resolver.register_node(&path, name, class_name, parent_path);
                    register_recursive(resolver, &node.children, Some(&path));
                } else {
                    // Node without name - recurse with same parent path
                    register_recursive(resolver, &node.children, parent_path);
                }
            }
        }

        register_recursive(&mut resolver, xfa_nodes, None);
        resolver
    }

    /// Register a node in the SOM tree.
    /// Also ensures all ancestor containers in the path are registered
    /// (e.g., for "A.B.C", both "A" and "A.B" are created as synthetic
    /// containers if they don't already exist).
    pub fn register_node(
        &mut self,
        path: &SomPath,
        name: &str,
        class_name: &str,
        parent_path: Option<&SomPath>,
    ) {
        // Ensure all ancestors exist in the tree
        if let Some(parent) = parent_path {
            self.ensure_ancestors(parent);
        }

        let index = path.index();

        let info = NodeInfo {
            name: name.to_string(),
            path: path.clone(),
            parent_path: parent_path.cloned(),
            index,
            class_name: class_name.to_string(),
        };

        self.insert_info(info);

        if let Some(parent) = parent_path {
            let siblings = self.children.entry(parent.clone()).or_default();
            if !siblings.contains(path) {
                siblings.push(path.clone());
            }
        }
    }

    /// Ensure that all ancestor containers in the path hierarchy exist
    /// in the SOM tree. Creates synthetic "subform" nodes for any missing
    /// intermediate segments.
    fn ensure_ancestors(&mut self, path: &SomPath) {
        if self.nodes.contains_key(path) {
            return; // Already registered
        }

        let parent = path.parent();
        if let Some(ref parent_path) = parent {
            self.ensure_ancestors(parent_path);
        }

        let info = NodeInfo {
            name: path.name().to_string(),
            path: path.clone(),
            parent_path: parent.clone(),
            index: path.index(),
            class_name: "subform".to_string(),
        };

        self.insert_info(info);

        if let Some(ref parent_path) = parent {
            self.children
                .entry(parent_path.clone())
                .or_default()
                .push(path.clone());
        }
    }

    /// Record one node under its path and its template path.
    /// Re-registering a path replaces its info without listing it twice.
    fn insert_info(&mut self, info: NodeInfo) {
        let path = info.path.clone();
        if self.nodes.insert(path.clone(), info).is_some() {
            return;
        }
        self.by_template
            .entry(path.index_free())
            .or_default()
            .push(path);
    }

    /// Every concrete instance path of a template path, in document order:
    /// `A.Row.F` expands to `A.Row.F`, `A.Row[1].F`, ... Empty when the
    /// template has no instance at all (a repeatable at count 0).
    pub fn expand_template(&self, template: &SomPath) -> Vec<SomPath> {
        self.by_template
            .get(&template.index_free())
            .cloned()
            .unwrap_or_default()
    }

    /// Get the child paths registered under a parent path.
    pub fn get_children(&self, path: &SomPath) -> Option<&Vec<SomPath>> {
        self.children.get(path)
    }

    /// The node an absolute SOM path names: [`Self::resolve_nodes`] narrowed
    /// to its first answer.
    pub fn resolve_node(&self, som_expression: &str) -> Option<SomPath> {
        self.resolve_nodes(som_expression).into_iter().next()
    }

    /// The nodes an absolute SOM path names (XFA 3.3 §3): a root node first,
    /// then a child of each node selected so far, every segment narrowed by
    /// its index (`Row[2]`; every instance for `Row[*]`; index 0 when none
    /// is written). A `$form.` or `xfa.form.` prefix names the Form DOM
    /// itself. Unqualified references are the scripting engine's business,
    /// resolved by its scope walk; this resolver answers the host, which
    /// addresses nodes by their full path, and nothing else.
    pub fn resolve_nodes(&self, som_expression: &str) -> Vec<SomPath> {
        let expr = som_expression.trim();
        let expr = expr
            .strip_prefix("$form.")
            .or_else(|| expr.strip_prefix("xfa.form."))
            .unwrap_or(expr);
        let path = SomPath::new(expr);
        if self.nodes.contains_key(&path) {
            return vec![path];
        }
        let Some(segments) = expr.split('.').map(parse_segment).collect::<Option<Vec<_>>>() else {
            return Vec::new();
        };
        let Some(((first_name, first_index), rest)) = segments.split_first() else {
            return Vec::new();
        };
        let mut selected = Self::pick(self.roots().filter(|p| p.name() == *first_name), *first_index);
        for (name, index) in rest {
            selected = selected
                .iter()
                .flat_map(|p| self.select_children(p, name, *index))
                .collect();
        }
        selected
    }

    /// The nodes with no parent: the root subform, and anything registered
    /// beside it.
    fn roots(&self) -> impl Iterator<Item = &SomPath> {
        self.nodes
            .values()
            .filter(|info| info.parent_path.is_none())
            .map(|info| &info.path)
    }

    /// The children of `parent` named `name`, narrowed by `index`.
    fn select_children(&self, parent: &SomPath, name: &str, index: SegmentIndex) -> Vec<SomPath> {
        Self::pick(
            self.children
                .get(parent)
                .into_iter()
                .flatten()
                .filter(|c| c.name() == name),
            index,
        )
    }

    fn pick<'a>(paths: impl Iterator<Item = &'a SomPath>, index: SegmentIndex) -> Vec<SomPath> {
        match index {
            SegmentIndex::All => paths.cloned().collect(),
            SegmentIndex::At(i) => paths.filter(|p| p.index() == i).cloned().collect(),
        }
    }

    /// Get a node by its path
    pub fn get_node(&self, path: &SomPath) -> Option<&NodeInfo> {
        self.nodes.get(path)
    }

}

impl Default for SomResolver {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// XFA Tree Walking Utilities
// =============================================================================

/// Walk an XFA tree following a SOM path, calling a visitor on the final node.
///
/// This is a generic utility to eliminate the repeated path-walking code patterns
/// found throughout the codebase. It handles unnamed containers transparently.
///
/// # Arguments
/// * `nodes` - The XFA node slice to search
/// * `som_path` - The SOM path to follow
/// * `visitor` - Callback invoked when the target node is found
///
/// # Returns
/// A reference to the node if found, or None if the path doesn't match.
pub fn walk_som_path<'a>(nodes: &'a [XfaNode], som_path: &str) -> Option<&'a XfaNode> {
    walk_som_route(nodes, som_path).and_then(|route| route.last().copied())
}

/// As [`walk_som_path`], but returning every node passed through on the way,
/// outermost first and the target last -- unnamed containers included, since
/// they are real containers in the Form DOM even though a SOM path skips
/// them. What a property inherited from enclosing containers (XFA 3.3 §2
/// "Access Restrictions") has to be read from.
pub fn walk_som_route<'a>(nodes: &'a [XfaNode], som_path: &str) -> Option<Vec<&'a XfaNode>> {
    let parts = concrete_segments(som_path)?;

    fn walk<'a>(
        nodes: &'a [XfaNode],
        parts: &[(&str, usize)],
        idx: usize,
        route: &mut Vec<&'a XfaNode>,
    ) -> bool {
        let Some(&(target_name, target_index)) = parts.get(idx) else {
            return false;
        };

        for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
            match node.name.as_deref() {
                Some(name) if name == target_name && index == target_index => {
                    route.push(node);
                    if idx == parts.len() - 1 || walk(&node.children, parts, idx + 1, route) {
                        return true;
                    }
                    route.pop();
                    return false;
                }
                // Unnamed container - search inside at SAME path index
                None => {
                    route.push(node);
                    if walk(&node.children, parts, idx, route) {
                        return true;
                    }
                    route.pop();
                }
                Some(_) => {}
            }
        }

        false
    }

    let mut route = Vec::new();
    walk(nodes, &parts, 0, &mut route).then_some(route)
}

/// Walk an XFA tree following a SOM path with mutable access.
///
/// # Returns
/// A mutable reference to the node if found, or None if the path doesn't match.
pub fn walk_som_path_mut<'a>(nodes: &'a mut [XfaNode], som_path: &str) -> Option<&'a mut XfaNode> {
    let parts = concrete_segments(som_path)?;

    fn walk<'a>(
        nodes: &'a mut [XfaNode],
        parts: &[(&str, usize)],
        idx: usize,
    ) -> Option<&'a mut XfaNode> {
        let (target_name, target_index) = *parts.get(idx)?;
        let indices = sibling_indices(nodes);

        for (node, index) in nodes.iter_mut().zip(indices) {
            if node.name.as_deref() == Some(target_name) {
                if index != target_index {
                    continue;
                }
                if idx == parts.len() - 1 {
                    return Some(node);
                }
                return walk(&mut node.children, parts, idx + 1);
            } else if node.name.is_none()
                && let Some(result) = walk(&mut node.children, parts, idx)
            {
                return Some(result);
            }
        }

        None
    }

    walk(nodes, &parts, 0)
}

/// A path's segments as `(name, index)`, or `None` when it is empty or uses
/// `[*]` or a malformed index -- a walk follows exactly one node.
pub(crate) fn concrete_segments(som_path: &str) -> Option<Vec<(&str, usize)>> {
    if som_path.is_empty() {
        return None;
    }
    som_path
        .split('.')
        .map(|seg| match parse_segment(seg)? {
            (name, SegmentIndex::At(i)) => Some((name, i)),
            (_, SegmentIndex::All) => None,
        })
        .collect()
}

/// Walk an XFA tree, tracking the current path and calling a visitor for each named node.
///
/// # Arguments
/// * `nodes` - The XFA node slice to traverse
/// * `visitor` - Callback invoked for each named node with (node, full_path, parent_path)
pub fn traverse_xfa_tree<F>(nodes: &[XfaNode], mut visitor: F)
where
    F: FnMut(&XfaNode, &str, Option<&str>),
{
    fn traverse<F>(nodes: &[XfaNode], parent_path: Option<&str>, visitor: &mut F)
    where
        F: FnMut(&XfaNode, &str, Option<&str>),
    {
        for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
            if let Some(name) = &node.name {
                let current_path = child_som_path(parent_path.unwrap_or(""), name, index);
                visitor(node, &current_path, parent_path);
                traverse(&node.children, Some(&current_path), visitor);
            } else {
                // Unnamed node - continue traversal with same parent path
                traverse(&node.children, parent_path, visitor);
            }
        }
    }

    traverse(nodes, None, &mut visitor);
}
