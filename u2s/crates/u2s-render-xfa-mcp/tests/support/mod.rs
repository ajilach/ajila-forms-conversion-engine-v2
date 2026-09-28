//! Shared scaffolding for the corpus-driven tests: an independent "oracle"
//! over the vendored UBS forms' raw XFA template, and the font/server
//! bootstrap every corpus test needs.

use std::collections::BTreeSet;
use std::path::Path;

use u2s_xfa::{XfaNode, XfaNodeKind};

/// Fonts are committed (`vendor/fonts/`, and the licensed
/// `vendor/fonts/ubs-frutiger/` for these forms specifically) — absence means
/// a broken checkout, not a machine this suite skips on.
pub fn require_fonts() {
    assert!(
        u2s_xfa::fonts::test_support::ensure_registered(),
        "no fonts registered — see crates/u2s-xfa/src/fonts.rs test_support::font_dir"
    );
}

/// Every checkbox/radio/dropdown SOM path the *template* declares, walked
/// directly over the parsed `XfaNode` tree rather than through
/// `u2s_xfa::exhaustive`'s field collector — the whole point of an oracle is
/// that it not share the logic it is checking. Deliberately ignores `access`
/// (protected/readOnly/nonInteractive): that only ever *shrinks* the set the
/// render server must report, so comparing against this unfiltered oracle as
/// a lower bound would be unsound; comparing against it as an upper bound
/// (`xfa_controls` must not report anything this oracle does not know at
/// all) is still exactly right, and is what these tests use it for.
///
/// Path convention (dot-joined names of every *named* ancestor, skipping
/// anonymous ones) matches `u2s_xfa::exhaustive`'s own — verified against the
/// vendored corpus, not assumed.
pub fn oracle_controls(doc_path: &Path) -> BTreeSet<String> {
    let bytes = std::fs::read(doc_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", doc_path.display()));
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract xfa")
        .expect("this fixture has an XFA packet");
    let nodes = XfaNode::parse(&xfa).expect("parse xfa");

    let mut out = BTreeSet::new();
    for root in &nodes {
        walk(root, "", &mut out);
    }
    out
}

fn walk(node: &XfaNode, parent_path: &str, out: &mut BTreeSet<String>) {
    let path = match &node.name {
        Some(name) if parent_path.is_empty() => name.clone(),
        Some(name) => format!("{parent_path}.{name}"),
        None => parent_path.to_string(),
    };

    if matches!(node.kind, XfaNodeKind::Field) && classify(node).is_some() {
        out.insert(path.clone());
    }

    for child in &node.children {
        walk(child, &path, out);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleKind {
    Radio,
    Checkbox,
    Dropdown,
}

/// A field's control kind from its own `<ui>` child, or `None` for a field
/// that is not one of the three interactive kinds this suite cares about
/// (a plain text field, for instance).
pub fn classify(field: &XfaNode) -> Option<OracleKind> {
    let ui = field.children.iter().find(|c| is_element(c, "ui"))?;
    let widget = ui.children.first()?;
    match tag_name(widget)? {
        "checkButton" => {
            let shape = widget
                .attributes
                .get("shape")
                .map(String::as_str)
                .unwrap_or("square");
            Some(if shape == "round" {
                OracleKind::Radio
            } else {
                OracleKind::Checkbox
            })
        }
        "choiceList" => Some(OracleKind::Dropdown),
        _ => None,
    }
}

fn tag_name(node: &XfaNode) -> Option<&str> {
    match &node.kind {
        XfaNodeKind::Element { tag_name, .. } => Some(tag_name.as_str()),
        _ => None,
    }
}

fn is_element(node: &XfaNode, tag: &str) -> bool {
    tag_name(node) == Some(tag)
}
