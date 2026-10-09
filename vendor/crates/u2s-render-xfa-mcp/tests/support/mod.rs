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
    u2s_xfa::fonts::register_dir_once(u2s_test_assets::font_dir(), None)
        .expect("register the test fonts");
}

/// Every checkbox/radio/dropdown/button SOM path the *template* declares that
/// a person can reach, walked directly over the parsed `XfaNode` tree rather
/// than through `u2s_xfa::exhaustive`'s field collector — the whole point of
/// an oracle is that it not share the logic it is checking. A field whose own
/// `access` is protected, readOnly or nonInteractive is left out (XFA 3.3
/// §17: a person cannot touch it), since the tests use this set as a lower
/// bound on what `xfa_controls` must report. A field reachable only because
/// of its parent exclGroup's access is not modelled here; such a field would
/// be reported here and missing there, which the test would flag.
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

    let blocked = matches!(
        node.attributes.get("access").map(String::as_str),
        Some("protected" | "readOnly" | "nonInteractive")
    );
    if matches!(node.kind, XfaNodeKind::Field) && classify(node).is_some() && !blocked {
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
    Button,
}

/// A field's control kind from its own `<ui>` child, or `None` for a field
/// that is not one of the interactive kinds this suite cares about (a plain
/// text field, for instance).
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
        "button" => Some(OracleKind::Button),
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

/// Every control `xfa_controls` lists for `args`, walking its `next_offset`
/// cursor to the end: a corpus form can have more fields than one window
/// holds.
pub async fn all_controls(
    c: &u2s_render_test_harness::Client,
    args: serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    loop {
        let mut window_args = args.clone();
        window_args["offset"] = offset.into();
        window_args["limit"] = 500.into();
        let result = u2s_render_test_harness::call(c, "xfa_controls", window_args).await;
        let window = u2s_render_test_harness::structured(&result);
        out.extend(window["controls"].as_array().expect("controls").iter().cloned());
        match window["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset, "next_offset must advance: {next} after {offset}");
                offset = next;
            }
            None => {
                assert_eq!(
                    out.len() as u64,
                    window["total"].as_u64().expect("total"),
                    "the windows must add up to the total"
                );
                return out;
            }
        }
    }
}
