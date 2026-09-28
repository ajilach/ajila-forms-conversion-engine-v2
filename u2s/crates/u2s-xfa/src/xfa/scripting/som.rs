//! SOM (Scripting Object Model) Path and Resolution
//!
//! This module implements SOM path handling per XFA 3.3 spec Chapter 3 (pages 86-120).
//!
//! ## SOM Path Expressions
//! - Full path: `UBSForms.Page.FormTitle.STP_RB_Horizontal.RB_Group_Neuanlage.RB_1`
//! - Short path: `Löschung` (matches first node with this name)
//! - Relative: `$.RB_1` (relative to current context)
//! - Descendant: `$data..fieldName` (search all descendants)
//! - Indexed: `Detail[0]`, `Item[*]`

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

    /// Get the last component (node name) of the path
    pub fn name(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(&self.0)
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

    /// Check if this path ends with the given suffix
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.0.ends_with(suffix)
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
    /// Nodes indexed by name (may have duplicates)
    nodes_by_name: HashMap<String, Vec<SomPath>>,
    /// Parent-child relationships
    children: HashMap<SomPath, Vec<SomPath>>,
}

impl SomResolver {
    pub fn new() -> Self {
        SomResolver {
            nodes: HashMap::new(),
            nodes_by_name: HashMap::new(),
            children: HashMap::new(),
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
            for node in nodes {
                if let Some(name) = &node.name {
                    let path = match parent_path {
                        Some(p) => p.child(name),
                        None => SomPath::new(name.clone()),
                    };

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

        let index = self.nodes_by_name.get(name).map(|v| v.len()).unwrap_or(0);

        let info = NodeInfo {
            name: name.to_string(),
            path: path.clone(),
            parent_path: parent_path.cloned(),
            index,
            class_name: class_name.to_string(),
        };

        self.nodes.insert(path.clone(), info);
        self.nodes_by_name
            .entry(name.to_string())
            .or_default()
            .push(path.clone());

        if let Some(parent) = parent_path {
            self.children
                .entry(parent.clone())
                .or_default()
                .push(path.clone());
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

        let node_name = path.name().to_string();
        let index = self
            .nodes_by_name
            .get(&node_name)
            .map(|v| v.len())
            .unwrap_or(0);

        let info = NodeInfo {
            name: node_name.clone(),
            path: path.clone(),
            parent_path: parent.clone(),
            index,
            class_name: "subform".to_string(),
        };

        self.nodes.insert(path.clone(), info);
        self.nodes_by_name
            .entry(node_name)
            .or_default()
            .push(path.clone());

        if let Some(ref parent_path) = parent {
            self.children
                .entry(parent_path.clone())
                .or_default()
                .push(path.clone());
        }
    }

    /// Get the child paths registered under a parent path.
    pub fn get_children(&self, path: &SomPath) -> Option<&Vec<SomPath>> {
        self.children.get(path)
    }

    /// Resolve a SOM expression to a single node path
    /// Per XFA 3.3 spec page 106-107
    pub fn resolve_node(
        &self,
        som_expression: &str,
        context_path: Option<&SomPath>,
    ) -> Option<SomPath> {
        self.resolve_nodes(som_expression, context_path)
            .into_iter()
            .next()
    }

    /// Resolve a SOM expression to multiple node paths
    /// Per XFA 3.3 spec page 106-107
    pub fn resolve_nodes(
        &self,
        som_expression: &str,
        context_path: Option<&SomPath>,
    ) -> Vec<SomPath> {
        let expr = som_expression.trim();

        // Handle shortcuts
        let expr = if let Some(stripped) = expr.strip_prefix("$form.") {
            stripped
        } else if let Some(stripped) = expr.strip_prefix("$data.") {
            stripped
        } else if expr == "$" {
            // $ = current context
            return context_path.cloned().into_iter().collect();
        } else if let Some(relative) = expr.strip_prefix("$.") {
            // $.foo = relative to current context
            if let Some(ctx) = context_path {
                return self.resolve_relative(ctx, relative);
            }
            return Vec::new();
        } else {
            expr
        };

        // Handle descendant accessor (..)
        if expr.contains("..") {
            return self.resolve_descendant(expr);
        }

        // Handle array index notation [n]
        if expr.contains('[') {
            return self.resolve_indexed(expr);
        }

        // Simple path lookup - try direct path first
        let path = SomPath::new(expr);
        if self.nodes.contains_key(&path) {
            return vec![path];
        }

        // Try to match by building path from parts
        let parts: Vec<&str> = expr.split('.').collect();
        self.resolve_path_parts(&parts)
    }

    /// Resolve relative path from context
    fn resolve_relative(&self, context_path: &SomPath, relative: &str) -> Vec<SomPath> {
        let full_path = context_path.child(relative);
        if self.nodes.contains_key(&full_path) {
            vec![full_path]
        } else {
            // Search children of context
            if let Some(children) = self.children.get(context_path) {
                children
                    .iter()
                    .filter(|p| p.ends_with(&format!(".{}", relative)))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            }
        }
    }

    /// Resolve descendant accessor (e.g., "$data..fieldName")
    fn resolve_descendant(&self, expr: &str) -> Vec<SomPath> {
        let parts: Vec<&str> = expr.split("..").collect();
        if parts.len() == 2 {
            let target_name = parts[1];
            // Find all nodes with this name
            self.nodes_by_name
                .get(target_name)
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Resolve indexed expression (e.g., "Detail[0]" or "Item[*]")
    fn resolve_indexed(&self, expr: &str) -> Vec<SomPath> {
        // Parse "Name[index]" pattern
        if let Some(bracket_pos) = expr.find('[') {
            let name = &expr[..bracket_pos];
            let index_part = &expr[bracket_pos + 1..expr.len() - 1];

            if let Some(paths) = self.nodes_by_name.get(name) {
                if index_part == "*" {
                    // Return all instances
                    return paths.clone();
                } else if let Ok(index) = index_part.parse::<usize>() {
                    // Return specific index
                    return paths.get(index).cloned().into_iter().collect();
                }
            }
        }
        Vec::new()
    }

    /// Resolve path parts
    fn resolve_path_parts(&self, parts: &[&str]) -> Vec<SomPath> {
        if parts.is_empty() {
            return Vec::new();
        }

        // Try matching by simple name for single-part paths
        if parts.len() == 1 {
            return self
                .nodes_by_name
                .get(parts[0])
                .cloned()
                .unwrap_or_default();
        }

        // Try building full path
        let full_path = SomPath::new(parts.join("."));
        if self.nodes.contains_key(&full_path) {
            return vec![full_path];
        }

        // Search for partial matches
        self.nodes
            .keys()
            .filter(|p| p.ends_with(&parts.join(".")))
            .cloned()
            .collect()
    }

    /// Get a node by its path
    pub fn get_node(&self, path: &SomPath) -> Option<&NodeInfo> {
        self.nodes.get(path)
    }

    /// Get all paths for a given node name
    pub fn get_paths_by_name(&self, name: &str) -> Option<&Vec<SomPath>> {
        self.nodes_by_name.get(name)
    }

    /// Resolve an unqualified reference using the XFA 3.3 §3 pp.110-114 scope walk.
    ///
    /// The search order is:
    /// 1. Children of the context container
    /// 2. The context container itself and its siblings
    /// 3. Parent of the container and siblings of the parent (uncles)
    /// 4. Grandparent and siblings of the grandparent (great-uncles)
    /// 5. Repeat recursively up to the root
    ///
    /// For multi-part names (e.g., "Sub.Field"), the first part is resolved via
    /// the scope walk, then the remainder is resolved as children relative to it.
    pub fn resolve_unqualified(&self, name: &str, context_path: &SomPath) -> Option<SomPath> {
        // Handle multi-part unqualified expressions (e.g., "Sub.Field")
        if let Some(dot_pos) = name.find('.') {
            let first_part = &name[..dot_pos];
            let remainder = &name[dot_pos + 1..];

            // Resolve the first part via scope walk
            if let Some(first_resolved) = self.resolve_unqualified(first_part, context_path) {
                // Then resolve remainder as a child of that node
                let full = first_resolved.child(remainder);
                if self.nodes.contains_key(&full) {
                    return Some(full);
                }
                // Try recursive resolution from that node
                return self.resolve_unqualified(remainder, &first_resolved);
            }
            return None;
        }

        // Single-part name resolution with scope walk
        // Get all registered paths for this name
        let all_paths = match self.nodes_by_name.get(name) {
            Some(paths) if !paths.is_empty() => paths,
            _ => return None,
        };

        // If only one instance exists, return it immediately
        if all_paths.len() == 1 {
            return Some(all_paths[0].clone());
        }

        // Step 1: Children of the context container
        if let Some(children) = self.children.get(context_path) {
            for child_path in children {
                if child_path.name() == name {
                    return Some(child_path.clone());
                }
            }
        }

        // Step 2-5: Walk up from context, checking siblings at each level
        let mut current = context_path.clone();
        loop {
            if let Some(parent) = current.parent() {
                // Check siblings (children of the parent) for the name
                if let Some(siblings) = self.children.get(&parent) {
                    for sibling_path in siblings {
                        if sibling_path.name() == name {
                            return Some(sibling_path.clone());
                        }
                        // Also check children of siblings (nephews)
                        if let Some(nephew_children) = self.children.get(sibling_path) {
                            for nephew in nephew_children {
                                if nephew.name() == name {
                                    return Some(nephew.clone());
                                }
                            }
                        }
                    }
                }
                // Check the parent itself
                if let Some(info) = self.nodes.get(&parent) {
                    if info.name == name {
                        return Some(parent.clone());
                    }
                }
                current = parent;
            } else {
                // Reached root — check root-level nodes
                for path in all_paths {
                    if !path.as_str().contains('.') {
                        return Some(path.clone());
                    }
                }
                break;
            }
        }

        // Final fallback: return the first registered path
        Some(all_paths[0].clone())
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
    let parts: Vec<&str> = som_path.split('.').collect();
    if parts.is_empty() {
        return None;
    }

    fn walk<'a>(nodes: &'a [XfaNode], parts: &[&str], idx: usize) -> Option<&'a XfaNode> {
        if idx >= parts.len() {
            return None;
        }

        let target_name = parts[idx];

        for node in nodes {
            if node.name.as_deref() == Some(target_name) {
                // Found a named node matching current path component
                if idx == parts.len() - 1 {
                    // This is the final target node
                    return Some(node);
                }
                // Continue to next path component in children
                return walk(&node.children, parts, idx + 1);
            } else if node.name.is_none() {
                // Unnamed container - search inside at SAME path index
                if let Some(result) = walk(&node.children, parts, idx) {
                    return Some(result);
                }
            }
        }

        None
    }

    walk(nodes, &parts, 0)
}

/// Walk an XFA tree following a SOM path with mutable access.
///
/// # Returns
/// A mutable reference to the node if found, or None if the path doesn't match.
pub fn walk_som_path_mut<'a>(nodes: &'a mut [XfaNode], som_path: &str) -> Option<&'a mut XfaNode> {
    let parts: Vec<&str> = som_path.split('.').collect();
    if parts.is_empty() {
        return None;
    }

    fn walk<'a>(nodes: &'a mut [XfaNode], parts: &[&str], idx: usize) -> Option<&'a mut XfaNode> {
        if idx >= parts.len() {
            return None;
        }

        let target_name = parts[idx];

        for node in nodes.iter_mut() {
            if node.name.as_deref() == Some(target_name) {
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
        for node in nodes {
            if let Some(name) = &node.name {
                let current_path = match parent_path {
                    Some(p) => format!("{}.{}", p, name),
                    None => name.clone(),
                };
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
