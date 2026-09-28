//! Path-addressed, structure-aware editing of the working structured tree.
//!
//! The structured-tree counterpart to [`crate::aem_translated_edit`]: it lets
//! the agent refine a *seeded* tree node-by-node instead of re-emitting the
//! whole tree through `set_structured` (which is expensive and risks silently
//! dropping nodes or a language). See `conversion.rs` for the tool wiring.
//!
//! ## Path syntax
//!
//! A node path is a `/`-separated walk from the top-level node list:
//!   - the first segment is a top-level index, e.g. `3`;
//!   - then one of the following container steps, repeated:
//!       * `children/<i>`        — into a Group's children
//!       * `rows/<r>/cells/<c>`  — into a Table body cell
//!       * `header/cells/<c>`    — into a Table header cell
//!       * `elements/<i>`        — into a GridLayout element's node
//!       * `item`                — into a Repeatable's item
//!       * `content`             — into a Conditional's content
//!
//! Example: `0/children/2`, `5/rows/0/cells/1`, `2/elements/0/content`.

use blueprint::StructuredNode;

pub use crate::tree_edit::{InsertPos, parse_insert_pos};

fn split_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

fn parse_idx(seg: &str, container: &str) -> Result<usize, String> {
    seg.parse::<usize>()
        .map_err(|_| format!("expected an index after '{container}', got '{seg}'"))
}

/// Resolve a path to a mutable reference to the addressed node.
pub fn resolve_mut<'a>(
    roots: &'a mut [StructuredNode],
    path: &str,
) -> Result<&'a mut StructuredNode, String> {
    let segs = split_path(path);
    if segs.is_empty() {
        return Err("empty path; the root is the whole node list, not a single node".into());
    }
    let idx = parse_idx(segs[0], "the top level")?;
    let node = roots
        .get_mut(idx)
        .ok_or_else(|| format!("no top-level node at index {idx}"))?;
    descend_mut(node, &segs[1..])
}

fn descend_mut<'a>(
    node: &'a mut StructuredNode,
    segs: &[&str],
) -> Result<&'a mut StructuredNode, String> {
    let Some((&head, tail)) = segs.split_first() else {
        return Ok(node);
    };
    // Borrowed immutably and released before the mutable match below; reused in
    // the fall-through error so we don't have to re-borrow `node` there.
    let ty = node_type(node);
    // Every matching arm `return`s so its borrow of `node` is confined to that
    // arm (works around the non-Polonius borrow checker); anything that doesn't
    // match falls through to the error.
    match node {
        StructuredNode::Group(g) if head == "children" => {
            let (i, rest) = take_index(tail, "children")?;
            let child = g.children.get_mut(i).ok_or_else(|| oob("children", i))?;
            return descend_mut(child, rest);
        }
        // One arm for the whole Table variant (two arms of the same variant both
        // returning `'a` defeats the borrow checker), branching on `head`.
        StructuredNode::Table(t) => {
            let (cell, rest): (&mut StructuredNode, &[&str]) = match head {
                "rows" => {
                    let (r, rest) = take_index(tail, "rows")?;
                    let row = t.rows.get_mut(r).ok_or_else(|| oob("rows", r))?;
                    let (kw, rest2) = rest
                        .split_first()
                        .ok_or_else(|| "expected 'cells' after rows/<i>".to_string())?;
                    if *kw != "cells" {
                        return Err(format!("expected 'cells' after rows/<i>, got '{kw}'"));
                    }
                    let (c, rest3) = take_index(rest2, "cells")?;
                    (row.cells.get_mut(c).ok_or_else(|| oob("cells", c))?, rest3)
                }
                "header" => {
                    let header = t
                        .header
                        .as_mut()
                        .ok_or_else(|| "table has no header".to_string())?;
                    let (kw, rest) = tail
                        .split_first()
                        .ok_or_else(|| "expected 'cells' after header".to_string())?;
                    if *kw != "cells" {
                        return Err(format!("expected 'cells' after header, got '{kw}'"));
                    }
                    let (c, rest2) = take_index(rest, "cells")?;
                    (
                        header.cells.get_mut(c).ok_or_else(|| oob("header cells", c))?,
                        rest2,
                    )
                }
                _ => return Err(format!("cannot descend into '{head}' from a table node")),
            };
            return descend_mut(cell, rest);
        }
        StructuredNode::GridLayout(grid) if head == "elements" => {
            let (i, rest) = take_index(tail, "elements")?;
            let el = grid.elements.get_mut(i).ok_or_else(|| oob("elements", i))?;
            return descend_mut(&mut el.node, rest);
        }
        StructuredNode::Repeatable(r) if head == "item" => return descend_mut(&mut r.item, tail),
        StructuredNode::Conditional(c) if head == "content" => {
            return descend_mut(&mut c.content, tail);
        }
        _ => {}
    }
    Err(format!("cannot descend into '{head}' from a {ty} node"))
}

fn take_index<'b>(segs: &'b [&'b str], container: &str) -> Result<(usize, &'b [&'b str]), String> {
    let (idx_seg, rest) = segs
        .split_first()
        .ok_or_else(|| format!("expected an index after '{container}'"))?;
    Ok((parse_idx(idx_seg, container)?, rest))
}

fn oob(container: &str, i: usize) -> String {
    format!("no '{container}' entry at index {i}")
}

/// Locate the child list and index that the addressed node lives in, for
/// structural insert/remove. Only the list-like containers — the top level and
/// `Group.children` — are supported; table cells and grid elements should be
/// edited with `replace_structured_node` on the table/grid node (or
/// `set_structured` for whole-table restructuring).
fn locate_in_list_mut<'a>(
    roots: &'a mut Vec<StructuredNode>,
    path: &str,
) -> Result<(&'a mut Vec<StructuredNode>, usize), String> {
    let segs = split_path(path);
    match segs.as_slice() {
        [] => Err("empty path".into()),
        [only] => {
            let i = parse_idx(only, "the top level")?;
            Ok((roots, i))
        }
        rest => {
            // Must end in `children/<i>`; the prefix addresses a Group.
            let n = rest.len();
            if rest[n - 2] != "children" {
                return Err(
                    "insert/remove is only supported for top-level nodes and Group children; \
                     for table cells or grid elements use replace_structured_node on the \
                     table/grid node, or set_structured for structural changes"
                        .into(),
                );
            }
            let i = parse_idx(rest[n - 1], "children")?;
            let prefix = rest[..n - 2].join("/");
            let parent = resolve_mut(roots, &prefix)?;
            match parent {
                StructuredNode::Group(g) => Ok((&mut g.children, i)),
                other => Err(format!(
                    "node at '{prefix}' is a {} node, not a Group",
                    node_type(other)
                )),
            }
        }
    }
}

/// Resolve the child list of a *container* node for insertion. `parent_path`
/// empty (or `root`/`/`) means the top level.
fn container_list_mut<'a>(
    roots: &'a mut Vec<StructuredNode>,
    parent_path: &str,
) -> Result<&'a mut Vec<StructuredNode>, String> {
    let trimmed = parent_path.trim_matches('/');
    if trimmed.is_empty() || trimmed == "root" {
        return Ok(roots);
    }
    match resolve_mut(roots, trimmed)? {
        StructuredNode::Group(g) => Ok(&mut g.children),
        other => Err(format!(
            "node at '{parent_path}' is a {} node; can only insert into the top level or a Group",
            node_type(other)
        )),
    }
}

// ── Edit operations ──────────────────────────────────────────────────────────

/// Set one field of the node at `path` (e.g. `label`, `inputType`, `required`,
/// `content`). `value` is the raw JSON for that field. The result is validated
/// by round-tripping through `StructuredNode`, so a malformed value is rejected
/// and the tree left unchanged.
pub fn set_field(
    roots: &mut [StructuredNode],
    path: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<String, String> {
    if field == "type" {
        return Err(
            "cannot change a node's `type` with set_structured_field; use replace_structured_node"
                .into(),
        );
    }
    let node = resolve_mut(roots, path)?;
    crate::tree_edit::set_field_by_roundtrip(node, field, value)?;
    let ty = node_type(node);
    Ok(format!("OK — set `{field}` on {path} ({ty})."))
}

/// Apply many [`set_field`] edits in one go, all-or-nothing.
///
/// Exists so that adding a language to a document is one call rather than one
/// per text node. Without it the cheapest way to translate a seeded tree is to
/// re-emit the whole thing, which throws away the structure the seed carried —
/// the grouping, the multi-column sections and the heading levels the engine
/// already got right.
///
/// Edits are applied to a copy and swapped in only if every one succeeds, so a
/// single bad value leaves the tree exactly as it was.
pub fn set_fields(
    roots: &mut Vec<StructuredNode>,
    edits: &[(String, String, serde_json::Value)],
) -> Result<String, String> {
    if edits.is_empty() {
        return Err("no edits given".into());
    }
    let mut draft = roots.clone();
    for (index, (path, field, value)) in edits.iter().enumerate() {
        set_field(&mut draft, path, field, value.clone())
            .map_err(|e| format!("edit {index} ({path}, `{field}`) failed: {e}; nothing applied"))?;
    }
    *roots = draft;
    Ok(format!("OK — applied {} edit(s).", edits.len()))
}

/// Replace the whole node at `path` with `node_json`.
pub fn replace_node(
    roots: &mut [StructuredNode],
    path: &str,
    node_json: serde_json::Value,
) -> Result<String, String> {
    let new_node: StructuredNode =
        serde_json::from_value(node_json).map_err(|e| format!("invalid StructuredNode: {e}"))?;
    let node = resolve_mut(roots, path)?;
    let ty = node_type(&new_node);
    *node = new_node;
    Ok(format!("OK — replaced {path} (now a {ty} node)."))
}

/// Remove the node at `path` from its parent list.
pub fn remove_node(roots: &mut Vec<StructuredNode>, path: &str) -> Result<String, String> {
    let (list, i) = locate_in_list_mut(roots, path)?;
    if i >= list.len() {
        return Err(format!("no node at index {i} to remove"));
    }
    list.remove(i);
    Ok(format!("OK — removed {path}."))
}

/// Insert `node_json` into the child list of the container at `parent_path`.
pub fn insert_node(
    roots: &mut Vec<StructuredNode>,
    parent_path: &str,
    node_json: serde_json::Value,
    pos: InsertPos,
) -> Result<String, String> {
    let new_node: StructuredNode =
        serde_json::from_value(node_json).map_err(|e| format!("invalid StructuredNode: {e}"))?;
    let ty = node_type(&new_node);
    let list = container_list_mut(roots, parent_path)?;
    let at = crate::tree_edit::insert_index(&pos, list.len());
    list.insert(at, new_node);
    Ok(format!(
        "OK — inserted a {ty} node into '{}' at index {at}.",
        if parent_path.trim_matches('/').is_empty() {
            "root"
        } else {
            parent_path
        }
    ))
}

// ── Outline / lint ───────────────────────────────────────────────────────────

/// One line per node: `<path>  <type> <summary> <flags>`. Flags: `⚠ label?` /
/// `⚠ text?` mark missing or placeholder text, `⚠ no-options` an empty choice
/// list, and `⚠ unsupported` a node the Redacto converter cannot represent
/// (fields, images, conditionals, repeatables). Flags (`⚠ …`) mark the
/// heuristic-seed artifacts the agent should review: fields with a missing or
/// `UNKNOWN` label, option groups with no options, and empty text nodes.
pub fn outline(roots: &[StructuredNode]) -> String {
    let mut out = String::new();
    for (i, node) in roots.iter().enumerate() {
        walk(node, &i.to_string(), &mut out);
    }
    if out.is_empty() {
        out.push_str("(empty structured tree: author it with set_structured first)");
    }
    out
}

fn walk(node: &StructuredNode, path: &str, out: &mut String) {
    let (summary, flags) = describe(node);
    out.push_str(path);
    out.push_str("  ");
    out.push_str(node_type(node));
    if !summary.is_empty() {
        out.push(' ');
        out.push_str(&summary);
    }
    if !flags.is_empty() {
        out.push_str("  ");
        out.push_str(&flags);
    }
    out.push('\n');

    match node {
        StructuredNode::Group(g) => {
            for (i, c) in g.children.iter().enumerate() {
                walk(c, &format!("{path}/children/{i}"), out);
            }
        }
        StructuredNode::Table(t) => {
            if let Some(h) = &t.header {
                for (c, cell) in h.cells.iter().enumerate() {
                    walk(cell, &format!("{path}/header/cells/{c}"), out);
                }
            }
            for (r, row) in t.rows.iter().enumerate() {
                for (c, cell) in row.cells.iter().enumerate() {
                    walk(cell, &format!("{path}/rows/{r}/cells/{c}"), out);
                }
            }
        }
        StructuredNode::GridLayout(grid) => {
            for (i, el) in grid.elements.iter().enumerate() {
                walk(&el.node, &format!("{path}/elements/{i}"), out);
            }
        }
        StructuredNode::Repeatable(r) => walk(&r.item, &format!("{path}/item"), out),
        StructuredNode::Conditional(c) => walk(&c.content, &format!("{path}/content"), out),
        _ => {}
    }
}

fn looks_unknown(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t.to_ascii_lowercase().contains("unknown")
}


/// Returns `(summary, flags)` for one node.
fn describe(node: &StructuredNode) -> (String, String) {
    use blueprint::FieldType;
    match node {
        StructuredNode::Field(f) => {
            let label = f.label.as_ref().map(|l| l.as_plain_text()).unwrap_or_default();
            let mut flags = String::new();
            if f.label.is_none() || looks_unknown(&label) {
                flags.push_str("⚠ label? ");
            }
            let opts_empty = match &f.input_type {
                FieldType::Radio { options }
                | FieldType::Select { options }
                | FieldType::CheckboxGroup { options } => options.is_empty(),
                _ => false,
            };
            if opts_empty {
                flags.push_str("⚠ no-options ");
            }
            // The Redacto converter has no representation for a fillable
            // field and skips it with a warning, so say so up front rather
            // than leaving it to be discovered in the dump.
            flags.push_str("⚠ unsupported ");
            let kind = field_type_name(&f.input_type);
            let summary = if label.is_empty() {
                format!("· {kind}")
            } else {
                format!("[{}] · {kind}", crate::tree_edit::excerpt(&label))
            };
            (summary, flags.trim_end().to_string())
        }
        StructuredNode::Heading(h) => text_node_desc(&h.content.as_plain_text()),
        StructuredNode::Paragraph(p) => text_node_desc(&p.content.as_plain_text()),
        StructuredNode::Footnote(f) => text_node_desc(&f.content.as_plain_text()),
        StructuredNode::Notice(n) => text_node_desc(&n.content.as_plain_text()),
        StructuredNode::Group(g) => {
            // `column_flow` becomes a multi-column panel in the Redacto output,
            // so it has to be visible (and settable) in the outline.
            let summary = if g.column_flow {
                format!("({} children, column flow)", g.children.len())
            } else {
                format!("({} children)", g.children.len())
            };
            (summary, String::new())
        }
        StructuredNode::Table(t) => {
            let cols = t.rows.first().map(|r| r.cells.len()).unwrap_or(0);
            (format!("({}×{})", t.rows.len(), cols), String::new())
        }
        StructuredNode::GridLayout(g) => (
            format!("({} cols, {} elements)", g.columns, g.elements.len()),
            String::new(),
        ),
        StructuredNode::List(l) => (format!("({} items)", l.items.len()), String::new()),
        // Content the Redacto converter cannot represent: an image is dropped,
        // a conditional is flattened and a repeatable is emitted once.
        StructuredNode::Image(_) | StructuredNode::Conditional(_) | StructuredNode::Repeatable(_) => {
            (String::new(), "⚠ unsupported".to_string())
        }
        // Raw markup only ever arrives here by lifting an AEM HTML component
        // back into the structured tree. Nothing on the Redacto authoring path
        // should hand-write one, so it is flagged rather than offered.
        StructuredNode::Html(_) => (String::new(), "⚠ unsupported".to_string()),
        _ => (String::new(), String::new()),
    }
}

fn text_node_desc(text: &str) -> (String, String) {
    let flags = if looks_unknown(text) {
        "⚠ text?".to_string()
    } else {
        String::new()
    };
    let summary = if text.trim().is_empty() {
        String::new()
    } else {
        format!("[{}]", crate::tree_edit::excerpt(text))
    };
    (summary, flags)
}

fn field_type_name(ft: &blueprint::FieldType) -> &'static str {
    use blueprint::FieldType;
    match ft {
        FieldType::Text { .. } => "text",
        FieldType::Textarea { .. } => "textarea",
        FieldType::Number { .. } => "number",
        FieldType::Date => "date",
        FieldType::Email => "email",
        FieldType::Tel => "tel",
        FieldType::Bool => "bool",
        FieldType::Radio { .. } => "radio",
        FieldType::Select { .. } => "select",
        FieldType::CheckboxGroup { .. } => "checkboxGroup",
    }
}

fn node_type(node: &StructuredNode) -> &'static str {
    match node {
        StructuredNode::Heading(_) => "heading",
        StructuredNode::Paragraph(_) => "paragraph",
        StructuredNode::Image(_) => "image",
        StructuredNode::Table(_) => "table",
        StructuredNode::Field(_) => "field",
        StructuredNode::Repeatable(_) => "repeatable",
        StructuredNode::Group(_) => "group",
        StructuredNode::Conditional(_) => "conditional",
        StructuredNode::Empty => "empty",
        StructuredNode::GridLayout(_) => "gridLayout",
        StructuredNode::List(_) => "list",
        StructuredNode::Footnote(_) => "footnote",
        StructuredNode::Html(_) => "html",
        StructuredNode::Notice(_) => "notice",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // A group with a heuristic-seed field (no label, empty select options) and a
    // trailing empty node.
    fn sample() -> Vec<StructuredNode> {
        serde_json::from_value(json!([{
            "type": "group",
            "children": [
                {
                    "type": "field",
                    "name": "00000000-0000-0000-0000-000000000001",
                    "label": null,
                    "inputType": { "type": "select", "options": [] },
                    "value": null,
                    "placeholder": null,
                    "required": false
                },
                { "type": "empty" }
            ]
        }]))
        .unwrap()
    }

    #[test]
    fn outline_flags_seed_artifacts() {
        let out = outline(&sample());
        assert!(out.contains("0  group"), "{out}");
        assert!(out.contains("0/children/0  field"), "{out}");
        assert!(out.contains("⚠ label?"), "{out}");
        assert!(out.contains("⚠ no-options"), "{out}");
        assert!(out.contains("0/children/1  empty"), "{out}");
    }

    /// The Redacto converter silently skips these node kinds (with a warning
    /// buried in the dump), so the outline has to call them out while the
    /// document can still be fixed.
    #[test]
    fn outline_flags_nodes_the_redacto_target_cannot_represent() {
        let tree = vec![
            StructuredNode::Image(blueprint::structured::ImageNode {
                content: Vec::new(),
                alt_text: Some("Logo".into()),
            }),
            StructuredNode::Paragraph(blueprint::structured::ParagraphNode {
                content: blueprint::structured::TranslatedText::plain("Body"),
                som_path: None,
                source_name: None,
            }),
        ];

        let out = outline(&tree);

        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("⚠ unsupported"), "{out}");
        assert!(
            !lines[1].contains("⚠"),
            "a plain paragraph is fully supported: {out}"
        );
    }

    /// `column_flow` drives the multi-column panel in the Redacto output, so it
    /// must be visible in the outline — a group carrying it looks identical to
    /// a plain one otherwise.
    #[test]
    fn outline_shows_the_column_flow_of_a_group() {
        use blueprint::structured::GroupNode;

        let plain = outline(&[StructuredNode::Group(GroupNode::new(Vec::new()))]);
        let columns = outline(&[StructuredNode::Group(GroupNode::columns(Vec::new()))]);

        assert!(!plain.contains("column flow"), "{plain}");
        assert!(columns.contains("column flow"), "{columns}");
    }

    /// Adding a language must not cost one call per node — that price is what
    /// drove the whole-tree rewrite that flattened the document's structure.
    #[test]
    fn set_fields_applies_every_edit_in_one_call() {
        let mut tree = sample();

        set_fields(
            &mut tree,
            &[
                ("0/children/0".into(), "required".into(), json!(true)),
                ("0".into(), "columnFlow".into(), json!(true)),
            ],
        )
        .unwrap();

        let out = outline(&tree);
        assert!(out.contains("column flow"), "{out}");
        let node = resolve_mut(&mut tree, "0/children/0").unwrap();
        assert!(serde_json::to_value(&*node).unwrap()["required"].as_bool() == Some(true));
    }

    /// All-or-nothing, so one bad value cannot leave a half-translated tree.
    #[test]
    fn set_fields_applies_nothing_when_one_edit_is_invalid() {
        let mut tree = sample();
        let before = serde_json::to_value(&tree).unwrap();

        let err = set_fields(
            &mut tree,
            &[
                ("0".into(), "columnFlow".into(), json!(true)),
                ("0".into(), "columnFlow".into(), json!("not a bool")),
            ],
        )
        .unwrap_err();

        assert!(err.contains("edit 1"), "the failing edit must be named: {err}");
        assert!(err.contains("nothing applied"), "{err}");
        assert_eq!(
            serde_json::to_value(&tree).unwrap(),
            before,
            "a rejected batch must leave the tree untouched"
        );
    }

    #[test]
    fn resolve_descends_into_group() {
        let mut tree = sample();
        let node = resolve_mut(&mut tree, "0/children/0").unwrap();
        assert_eq!(node_type(node), "field");
        assert!(resolve_mut(&mut tree, "0/children/9").is_err());
        assert!(resolve_mut(&mut tree, "5").is_err());
    }

    #[test]
    fn set_field_validates_and_applies() {
        let mut tree = sample();
        // Valid: flip `required`.
        set_field(&mut tree, "0/children/0", "required", json!(true)).unwrap();
        let node = resolve_mut(&mut tree, "0/children/0").unwrap();
        let v = serde_json::to_value(&*node).unwrap();
        assert_eq!(v["required"], json!(true));

        // Invalid inputType is rejected and leaves the tree unchanged.
        let before = serde_json::to_value(&tree).unwrap();
        assert!(set_field(&mut tree, "0/children/0", "inputType", json!({"type": "bogus"})).is_err());
        assert_eq!(serde_json::to_value(&tree).unwrap(), before);

        // `type` cannot be changed this way.
        assert!(set_field(&mut tree, "0/children/0", "type", json!("empty")).is_err());
    }

    #[test]
    fn insert_and_remove_in_group() {
        let mut tree = sample();
        insert_node(&mut tree, "0", json!({"type": "empty"}), InsertPos::First).unwrap();
        let n = match &tree[0] {
            StructuredNode::Group(g) => g.children.len(),
            _ => panic!("expected group"),
        };
        assert_eq!(n, 3);
        assert_eq!(node_type(resolve_mut(&mut tree, "0/children/0").unwrap()), "empty");

        remove_node(&mut tree, "0/children/0").unwrap();
        let n = match &tree[0] {
            StructuredNode::Group(g) => g.children.len(),
            _ => panic!(),
        };
        assert_eq!(n, 2);

        // Table cells aren't list-removable.
        let mut t2: Vec<StructuredNode> =
            serde_json::from_value(json!([{"type":"table","header":null,"rows":[],"caption":null}]))
                .unwrap();
        assert!(remove_node(&mut t2, "0/rows/0/cells/0").is_err());
    }

    #[test]
    fn replace_node_swaps_type() {
        let mut tree = sample();
        replace_node(&mut tree, "0/children/0", json!({"type": "empty"})).unwrap();
        assert_eq!(node_type(resolve_mut(&mut tree, "0/children/0").unwrap()), "empty");
    }
}
