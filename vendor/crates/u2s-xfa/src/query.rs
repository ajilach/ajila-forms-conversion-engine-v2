//! Reading the raw XFA document: an outline of its structure and one node by
//! path — both over the **static template tree**, with no scripting and no
//! fonts. This is deliberate: a data-reading tool should not
//! need either, and not needing them is what makes this server cheap and safe
//! to run with no font environment at all.
//!
//! The design principle throughout is the one the render side already
//! established: never dump the whole document. A real form's XFA can run to
//! tens of megabytes; every operation here is windowed, bounded, and honest
//! about what it did not show.
//!
//! Text search used to live here too. It was never XFA-specific — it greps a
//! `&str` — so it moved to [`u2s_core::text::Grep`] when the two render
//! servers needed the same thing over extracted page text.

use serde::{Deserialize, Serialize};

use crate::xfa::{XfaNode, XfaNodeKind};

/// A stable, human-addressable path to a node: dot-joined names from the
/// packet root. Named nodes (`name="..."` in the XFA) use their name; the rest
/// fall back to `tag[index]` among same-tag siblings, so every node has an
/// address even when the XFA author left it anonymous.
pub fn path_of(segments: &[String]) -> String {
    segments.join(".")
}

fn segment_for(node: &XfaNode, index_among_siblings: usize) -> String {
    if let Some(name) = &node.name
        && !name.is_empty()
    {
        return name.clone();
    }
    let tag = tag_name(node);
    format!("{tag}[{index_among_siblings}]")
}

fn tag_name(node: &XfaNode) -> &str {
    match &node.kind {
        XfaNodeKind::Template => "template",
        XfaNodeKind::Subform => "subform",
        XfaNodeKind::Field => "field",
        XfaNodeKind::PageSet => "pageSet",
        XfaNodeKind::PageArea => "pageArea",
        XfaNodeKind::ContentArea => "contentArea",
        XfaNodeKind::Draw => "draw",
        XfaNodeKind::Value => "value",
        XfaNodeKind::Bind => "bind",
        XfaNodeKind::ExclGroup => "exclGroup",
        XfaNodeKind::Element { tag_name, .. } => tag_name,
        XfaNodeKind::Text { .. } => "#text",
    }
}

/// A one-line excerpt of what a node actually says, for the outline: the
/// value text for a value/draw, the literal content for a raw element,
/// nothing for a pure container. Capped short — this is a summary, not a read.
fn excerpt(node: &XfaNode) -> Option<String> {
    const MAX: usize = 80;
    let text = match &node.kind {
        XfaNodeKind::Element {
            text_content: Some(t),
            ..
        } => Some(t.as_str()),
        XfaNodeKind::Text { content } => Some(content.as_str()),
        _ => None,
    }
    .or_else(|| node.attributes.get("value").map(String::as_str));
    text.map(|t| {
        let t = t.trim();
        if t.chars().count() > MAX {
            format!("{}…", t.chars().take(MAX).collect::<String>())
        } else {
            t.to_string()
        }
    })
    .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlineEntry {
    pub path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    pub child_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outline {
    pub entries: Vec<OutlineEntry>,
    /// True when `max_depth` or `limit` cut the walk short — descend into a
    /// specific path with `node` rather than raising the limit.
    pub truncated: bool,
}

/// Depth- and count-capped tree summary. Every entry is one line: its path,
/// its kind, a short excerpt if it has readable content, and how many
/// children it has (so a caller knows there is more without being shown it).
pub fn outline(roots: &[XfaNode], max_depth: usize, limit: usize) -> Outline {
    let mut entries = Vec::new();
    let mut truncated = false;

    fn walk(
        node: &XfaNode,
        prefix: &[String],
        index: usize,
        depth: usize,
        max_depth: usize,
        limit: usize,
        entries: &mut Vec<OutlineEntry>,
        truncated: &mut bool,
    ) {
        if entries.len() >= limit {
            *truncated = true;
            return;
        }
        let mut path = prefix.to_vec();
        path.push(segment_for(node, index));

        entries.push(OutlineEntry {
            path: path_of(&path),
            kind: tag_name(node).to_string(),
            excerpt: excerpt(node),
            child_count: node.children.len(),
        });

        if depth >= max_depth {
            if !node.children.is_empty() {
                *truncated = true;
            }
            return;
        }
        for (i, child) in node.children.iter().enumerate() {
            if entries.len() >= limit {
                *truncated = true;
                return;
            }
            walk(
                child,
                &path,
                i,
                depth + 1,
                max_depth,
                limit,
                entries,
                truncated,
            );
        }
    }

    for (i, root) in roots.iter().enumerate() {
        if entries.len() >= limit {
            truncated = true;
            break;
        }
        walk(
            root,
            &[],
            i,
            0,
            max_depth,
            limit,
            &mut entries,
            &mut truncated,
        );
    }

    Outline { entries, truncated }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub attributes: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// Immediate children only — descend further with another `node` call
    /// rather than pulling a whole subtree back at once.
    pub children: Vec<OutlineEntry>,
}

/// Resolve a dot-joined path (as returned by `outline`) to one node's own
/// info and the list of its immediate children. Never returns a subtree.
pub fn node_at(roots: &[XfaNode], path: &str) -> Option<NodeInfo> {
    if path.is_empty() {
        return None;
    }
    let segments: Vec<&str> = path.split('.').collect();

    fn find<'a>(nodes: &'a [XfaNode], segments: &[&str]) -> Option<(&'a XfaNode, usize)> {
        let (head, rest) = segments.split_first()?;
        for (i, n) in nodes.iter().enumerate() {
            if segment_for(n, i) == *head {
                if rest.is_empty() {
                    return Some((n, i));
                }
                return find(&n.children, rest);
            }
        }
        None
    }

    let (node, _) = find(roots, &segments)?;
    let children = node
        .children
        .iter()
        .enumerate()
        .map(|(i, c)| OutlineEntry {
            path: format!("{path}.{}", segment_for(c, i)),
            kind: tag_name(c).to_string(),
            excerpt: excerpt(c),
            child_count: c.children.len(),
        })
        .collect();

    Some(NodeInfo {
        path: path.to_string(),
        kind: tag_name(node).to_string(),
        attributes: node.attributes.clone(),
        excerpt: excerpt(node),
        children,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xfa::XfaNode;

    // `XfaNode::parse` needs the real xdp:xdp envelope to recurse past the
    // first level - a bare `<template>` fragment leaves deeper children
    // unpopulated. Single-line (no incidental whitespace-text nodes) so the
    // synthetic tree here matches exactly what these tests assert on.
    fn parse(xml: &str) -> Vec<XfaNode> {
        let wrapped = format!(
            "<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\"><template xmlns=\"http://www.xfa.org/schema/xfa-template/3.3/\">{xml}</template></xdp:xdp>"
        );
        XfaNode::parse(wrapped.as_bytes()).expect("parse")
    }

    fn find_path<'a>(entries: &'a [OutlineEntry], suffix: &str) -> &'a OutlineEntry {
        entries
            .iter()
            .find(|e| e.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no entry ending in {suffix:?} among {entries:?}"))
    }

    #[test]
    fn outline_lists_named_and_anonymous_siblings() {
        let nodes = parse(r#"<subform name="Body"><draw name="Title"/><draw/><draw/></subform>"#);
        let out = outline(&nodes, 10, 100);
        let paths: Vec<&str> = out.entries.iter().map(|e| e.path.as_str()).collect();
        // The wrapper (xdp:xdp, template) is itself anonymous, so this only
        // pins the addressable suffix - not the wrapper's own indexing.
        assert!(paths.iter().any(|p| p.ends_with("Body.Title")));
        // Anonymous siblings get a stable, distinct address.
        assert!(paths.iter().any(|p| p.ends_with("draw[1]")));
        assert!(paths.iter().any(|p| p.ends_with("draw[2]")));
        assert!(!out.truncated);
    }

    #[test]
    fn outline_reports_truncation_honestly() {
        let nodes = parse(r#"<subform name="Body"><draw name="A"/><draw name="B"/></subform>"#);
        let out = outline(&nodes, 10, 2);
        assert!(out.truncated, "a limit of 2 must be reported as a cut walk");
        assert!(out.entries.len() <= 2);
    }

    #[test]
    fn node_at_resolves_a_path_returned_by_outline() {
        let nodes = parse(r#"<subform name="Body"><field name="First"/></subform>"#);
        let out = outline(&nodes, 10, 100);
        let target = find_path(&out.entries, "Body.First");

        let info = node_at(&nodes, &target.path).expect("node must resolve");
        assert_eq!(info.kind, "field");
        assert!(info.children.is_empty());
    }

    #[test]
    fn node_at_returns_only_immediate_children_not_a_subtree() {
        let nodes = parse(
            r#"<subform name="Body"><subform name="Inner"><field name="Deep"/></subform></subform>"#,
        );
        let out = outline(&nodes, 10, 100);
        let body = find_path(&out.entries, "Body");
        let info = node_at(&nodes, &body.path).expect("resolve");
        assert_eq!(info.children.len(), 1);
        assert_eq!(info.children[0].path, format!("{}.Inner", body.path));
        // The grandchild must not appear at this level.
        assert!(info.children[0].child_count >= 1, "the count is reported");
    }
}
