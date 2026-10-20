//! Prototypes and document variables (XFA 3.3 §6 "Prototypes", "The
//! variables element") are template definitions, not form content: an
//! element under `<proto>` "plays no part in the form except when it is
//! referenced by a use or usehref attribute", and a variable is boilerplate
//! a script inserts elsewhere. Neither is laid out or drawn where it is
//! declared. Designer writes a `designer__defaultHyphenation` field into a
//! `<proto>` of the root subform; laid out, it struck a full-width line
//! through the form's title.

use u2s_xfa::{Flattened, FlattenedNodeKind, XfaNode, fonts};

fn ensure_fonts() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
}

/// A root subform holding `definitions` beside a body with one visible draw.
fn form(definitions: &str) -> Flattened {
    let xdp = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet><pageArea name="P1" w="612pt" h="792pt">
<contentArea x="36pt" y="36pt" w="540pt" h="720pt"/></pageArea></pageSet>
{definitions}
<subform name="Body" layout="tb" w="540pt">
<draw name="Visible" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>Visible</text></value></draw>
</subform></subform></template></xdp:xdp>"#
    );
    let nodes = XfaNode::parse(xdp.as_bytes()).expect("parse");
    Flattened::from_xfa_simple(&nodes).expect("flatten")
}

fn field_names(flat: &Flattened) -> Vec<String> {
    flat.iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Field { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn texts(flat: &Flattened) -> Vec<(String, String)> {
    flat.iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Text { content, .. } => Some((content.clone(), n.y.to_string())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_field_in_a_proto_is_neither_laid_out_nor_drawn() {
    ensure_fonts();
    let flat = form(
        r#"<proto><field name="designer__defaultHyphenation" w="540pt" h="20pt"><ui><textEdit/></ui>
<border><edge/></border><para><hyphenation hyphenate="1"/></para></field></proto>"#,
    );

    assert!(
        field_names(&flat).is_empty(),
        "the prototype field was laid out: {:?}",
        field_names(&flat)
    );
    // The body starts at the content area's top: the prototype took no room.
    assert_eq!(
        texts(&flat),
        vec![("Visible".to_string(), "36".to_string())]
    );
}

#[test]
fn a_document_variable_is_not_drawn_where_it_is_declared() {
    ensure_fonts();
    let flat = form(r#"<variables><text name="Terms">Terms and conditions</text></variables>"#);
    assert_eq!(
        texts(&flat),
        vec![("Visible".to_string(), "36".to_string())]
    );
}
