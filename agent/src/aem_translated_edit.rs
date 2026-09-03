//! Path-addressed, structure-aware editing of the working `AemNodeTranslated`
//! tree (the multilingual AEM tree the agent authors directly).
//!
//! Mirrors `structured_edit.rs`, but the AEM tree is simpler: only `Root`,
//! `Panel` and `Repeatable` carry children, so a path is just a `/`-separated
//! list of child indices from the root node.
//!
//! ## Path syntax
//!   - `""` (empty) addresses the root node itself.
//!   - `2` is the root's child at index 2.
//!   - `2/0/3` descends child→child→child.

use blueprint::{AemI18nText, AemNodeTranslated};

pub use crate::tree_edit::{InsertPos, parse_insert_pos};

fn node_type(node: &AemNodeTranslated) -> &'static str {
    use AemNodeTranslated::*;
    match node {
        Root { .. } => "Root",
        Panel { .. } => "Panel",
        TextField { .. } => "TextField",
        NumberField { .. } => "NumberField",
        DatePicker { .. } => "DatePicker",
        Dropdown { .. } => "Dropdown",
        Checkbox { .. } => "Checkbox",
        RadioButton { .. } => "RadioButton",
        TextDraw { .. } => "TextDraw",
        MessageBox { .. } => "MessageBox",
        TitleDraw { .. } => "TitleDraw",
        HtmlDisplayer { .. } => "HtmlDisplayer",
        Repeatable { .. } => "Repeatable",
        Fragment { .. } => "Fragment",
        Preface { .. } => "Preface",
        Appendix { .. } => "Appendix",
        FootnotePlaceholder { .. } => "FootnotePlaceholder",
        Custom { .. } => "Custom",
    }
}

fn children_mut(node: &mut AemNodeTranslated) -> Option<&mut Vec<AemNodeTranslated>> {
    use AemNodeTranslated::*;
    match node {
        Root { children, .. } | Panel { children, .. } | Repeatable { children, .. } => {
            Some(children)
        }
        _ => None,
    }
}

fn children_ref(node: &AemNodeTranslated) -> Option<&[AemNodeTranslated]> {
    use AemNodeTranslated::*;
    match node {
        Root { children, .. } | Panel { children, .. } | Repeatable { children, .. } => {
            Some(children)
        }
        _ => None,
    }
}

/// The node's primary user-visible text field (title/label/content), if any.
fn primary_text(node: &AemNodeTranslated) -> Option<&AemI18nText> {
    use AemNodeTranslated::*;
    match node {
        Root { title, .. } | Panel { title, .. } | Repeatable { title, .. } => Some(title),
        TextField { label, .. }
        | NumberField { label, .. }
        | DatePicker { label, .. }
        | Dropdown { label, .. }
        | Checkbox { label, .. }
        | RadioButton { label, .. }
        | Custom { label, .. } => Some(label),
        TextDraw { content, .. } | TitleDraw { content, .. } | HtmlDisplayer { content, .. } => {
            Some(content)
        }
        _ => None,
    }
}

fn split_path(path: &str) -> Vec<&str> {
    let p = path.trim().trim_matches('/');
    if p.is_empty() || p == "root" {
        return Vec::new();
    }
    p.split('/').filter(|s| !s.is_empty()).collect()
}

/// Resolve a path to a mutable reference to the addressed node.
pub fn resolve_mut<'a>(
    root: &'a mut AemNodeTranslated,
    path: &str,
) -> Result<&'a mut AemNodeTranslated, String> {
    let mut node = root;
    for seg in split_path(path) {
        let i: usize = seg
            .parse()
            .map_err(|_| format!("path segment '{seg}' is not an index"))?;
        let ty = node_type(node);
        let kids =
            children_mut(node).ok_or_else(|| format!("a {ty} node has no children to index"))?;
        let n = kids.len();
        node = kids
            .get_mut(i)
            .ok_or_else(|| format!("no child at index {i} (a {ty} has {n})"))?;
    }
    Ok(node)
}

// ── Edit operations ──────────────────────────────────────────────────────────

/// Set one field of the node at `path` (e.g. `label`, `title`, `content`,
/// `options`, `visible`, `mandatory`, `colspan`). `value` is the raw JSON for
/// that field — text fields are per-language maps like `{"de":"…","en":"…"}`.
/// Validated by round-trip, so a bad value is rejected and the tree unchanged.
pub fn set_field(
    root: &mut AemNodeTranslated,
    path: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<String, String> {
    if field == "type" {
        return Err(
            "cannot change a node's `type` with set_aem_translated_field; use replace_aem_translated_node"
                .into(),
        );
    }
    let node = resolve_mut(root, path)?;
    crate::tree_edit::set_field_by_roundtrip(node, field, value)?;
    let ty = node_type(node);
    Ok(format!("OK — set `{field}` on {} ({ty}).", show(path)))
}

/// Replace the whole node at `path` with `node_json`.
pub fn replace_node(
    root: &mut AemNodeTranslated,
    path: &str,
    node_json: serde_json::Value,
) -> Result<String, String> {
    let new_node: AemNodeTranslated =
        serde_json::from_value(node_json).map_err(|e| format!("invalid AemNodeTranslated: {e}"))?;
    let node = resolve_mut(root, path)?;
    let ty = node_type(&new_node);
    *node = new_node;
    Ok(format!("OK — replaced {} (now a {ty} node).", show(path)))
}

/// Remove the node at `path` from its parent's child list. The root cannot be removed.
pub fn remove_node(root: &mut AemNodeTranslated, path: &str) -> Result<String, String> {
    let segs = split_path(path);
    let Some((last, prefix)) = segs.split_last() else {
        return Err("cannot remove the root node".into());
    };
    let idx: usize = last
        .parse()
        .map_err(|_| format!("path segment '{last}' is not an index"))?;
    let parent = resolve_mut(root, &prefix.join("/"))?;
    let ty = node_type(parent);
    let kids =
        children_mut(parent).ok_or_else(|| format!("a {ty} node has no children to remove from"))?;
    if idx >= kids.len() {
        return Err(format!("no child at index {idx} to remove"));
    }
    kids.remove(idx);
    Ok(format!("OK — removed {}.", show(path)))
}

/// Insert `node_json` into the child list of the container at `parent_path`
/// (empty/`root` = the root node; otherwise a Panel or Repeatable).
pub fn insert_node(
    root: &mut AemNodeTranslated,
    parent_path: &str,
    node_json: serde_json::Value,
    pos: InsertPos,
) -> Result<String, String> {
    let new_node: AemNodeTranslated =
        serde_json::from_value(node_json).map_err(|e| format!("invalid AemNodeTranslated: {e}"))?;
    let ty = node_type(&new_node);
    let parent = resolve_mut(root, parent_path)?;
    let pty = node_type(parent);
    let kids = children_mut(parent)
        .ok_or_else(|| format!("a {pty} node cannot hold children; insert into a Panel/Repeatable/Root"))?;
    let at = crate::tree_edit::insert_index(&pos, kids.len());
    kids.insert(at, new_node);
    Ok(format!("OK — inserted a {ty} node into '{}' at index {at}.", show(parent_path)))
}

// ── Outline ──────────────────────────────────────────────────────────────────

/// One line per node: `<path>  <Type>  [langs] "excerpt"  <flags>`. Flags mark a
/// text-bearing node whose text is empty (`⚠ empty`) or present in only one
/// language (`⚠ 1 lang` — likely a missing translation).
pub fn outline(root: &AemNodeTranslated) -> String {
    let mut out = String::new();
    walk(root, "", &mut out);
    out
}

fn walk(node: &AemNodeTranslated, path: &str, out: &mut String) {
    out.push_str(&show(path));
    out.push_str("  ");
    out.push_str(node_type(node));
    if let Some(text) = primary_text(node) {
        let langs: Vec<&str> = text.languages().collect();
        out.push_str(&format!("  [{}]", langs.join(",")));
        let excerpt = text
            .0
            .values()
            .find(|s| !s.is_empty())
            .map(|s| crate::tree_edit::excerpt(s))
            .unwrap_or_default();
        if !excerpt.is_empty() {
            out.push_str(&format!(" \"{excerpt}\""));
        }
        if langs.is_empty() || text.0.values().all(|s| s.trim().is_empty()) {
            out.push_str("  ⚠ empty");
        } else if langs.len() == 1 {
            out.push_str("  ⚠ 1 lang");
        }
    }
    out.push('\n');
    if let Some(children) = children_ref(node) {
        for (i, c) in children.iter().enumerate() {
            let child_path = if path.is_empty() {
                i.to_string()
            } else {
                format!("{path}/{i}")
            };
            walk(c, &child_path, out);
        }
    }
}

fn show(path: &str) -> String {
    let p = path.trim().trim_matches('/');
    if p.is_empty() || p == "root" {
        "root".to_string()
    } else {
        p.to_string()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NIL: &str = "00000000-0000-0000-0000-000000000000";

    fn sample() -> AemNodeTranslated {
        serde_json::from_value(json!({
            "type": "Root",
            "title": {"de": "Formular", "en": "Form"},
            "children": [{
                "type": "Panel",
                "uuid": NIL, "name": "panel",
                "title": {"de": "Abschnitt", "en": "Section"},
                "children": [{
                    "type": "TextField",
                    "uuid": NIL, "name": "f1",
                    "label": {"de": "Nachname"},
                    "mandatory": false, "visible": true, "max_chars": null,
                    "colspan": 6, "dor_colspan": null, "bind_ref": null
                }],
                "is_page": true, "dor_exclude": false, "visible": true,
                "is_conditional": false, "dor_num_cols": null, "colspan": 12,
                "dor_colspan": null, "bind_ref": null
            }]
        }))
        .unwrap()
    }

    #[test]
    fn outline_flags_text() {
        let out = outline(&sample());
        assert!(out.contains("root  Root"), "{out}");
        assert!(out.contains("0  Panel"), "{out}");
        assert!(out.contains("0/0  TextField"), "{out}");
        assert!(out.contains("⚠ 1 lang"), "single-language field should flag: {out}");
    }

    #[test]
    fn resolves_and_sets_field() {
        let mut tree = sample();
        assert_eq!(node_type(resolve_mut(&mut tree, "0/0").unwrap()), "TextField");
        // Add the English label.
        set_field(&mut tree, "0/0", "label", json!({"de": "Nachname", "en": "Last name"})).unwrap();
        let node = resolve_mut(&mut tree, "0/0").unwrap();
        let v = serde_json::to_value(&*node).unwrap();
        assert_eq!(v["label"]["en"], "Last name");
        // Invalid value rejected, tree unchanged.
        let before = serde_json::to_value(&tree).unwrap();
        assert!(set_field(&mut tree, "0/0", "mandatory", json!("nope")).is_err());
        assert_eq!(serde_json::to_value(&tree).unwrap(), before);
        // type cannot be changed this way.
        assert!(set_field(&mut tree, "0/0", "type", json!("Panel")).is_err());
    }

    #[test]
    fn insert_and_remove() {
        let mut tree = sample();
        let draw = json!({
            "type": "TextDraw", "uuid": NIL, "name": "d",
            "content": {"de": "Hinweis"}, "dor_exclude": false,
            "colspan": 12, "dor_colspan": null
        });
        insert_node(&mut tree, "0", draw, InsertPos::First).unwrap();
        assert_eq!(node_type(resolve_mut(&mut tree, "0/0").unwrap()), "TextDraw");
        remove_node(&mut tree, "0/0").unwrap();
        assert_eq!(node_type(resolve_mut(&mut tree, "0/0").unwrap()), "TextField");
        // Root cannot be removed.
        assert!(remove_node(&mut tree, "").is_err());
        // Leaf nodes cannot hold children.
        assert!(insert_node(&mut tree, "0/0", json!({"type":"Preface","uuid":NIL,"name":"p"}), InsertPos::Last).is_err());
    }

    #[test]
    fn replace_changes_type() {
        let mut tree = sample();
        replace_node(
            &mut tree,
            "0/0",
            json!({"type":"Preface","uuid":NIL,"name":"p"}),
        )
        .unwrap();
        assert_eq!(node_type(resolve_mut(&mut tree, "0/0").unwrap()), "Preface");
    }
}
