//! Layout and rendering with the plain font set alone (`vendor/fonts`, DejaVu
//! Sans): what a machine without the customer's fonts gets. The typeface the
//! template asks for is not there, so every text is a reported substitution
//! drawn in the fallback, and the layout still produces real pages.
//!
//! A separate test binary, because fonts are registered once per process.

mod support;

use support::dark_pixels;
use u2s_xfa::{Flattened, FlattenedNodeKind, XfaNode, fonts, prepare_default};

fn template(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs" relation="simplexPaginated">
<pageArea name="MP">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
</pageArea>
</pageSet>
<subform name="Body" layout="tb" w="540pt">{body}</subform></subform></template></xdp:xdp>"#
    )
}

fn flatten(xdp: &str) -> Flattened {
    let registered = fonts::register_dir_once(u2s_test_assets::fallback_font_dir(), None)
        .expect("register the plain fonts");
    assert!(
        registered.fallback.ends_with("DejaVuSans.ttf"),
        "the plain set's only face is the fallback: {:?}",
        registered.fallback
    );
    let nodes = XfaNode::parse(xdp.as_bytes()).expect("parse");
    prepare_default(&nodes).expect("prepare").flattened
}

/// A typeface the plain set does not have is drawn in the fallback: the
/// text is on the page, sized by the fallback's metrics, and a growable draw
/// is as wide as its text in that face.
#[test]
fn an_unavailable_typeface_is_drawn_in_the_fallback() {
    let body: String = (1..=40)
        .map(|i| {
            format!(
                r#"<draw name="R{i}" w="400pt" h="24pt"><font typeface="Frutiger 45 Light" size="11pt"/><value><text>Row {i}</text></value></draw>"#
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let flat = flatten(&template(&body));
    assert_eq!(flat.page.page_count, 2, "40 rows of 24pt are two pages");
    let rows = flat
        .iter_nodes()
        .filter(|n| matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content.starts_with("Row ")))
        .count();
    assert_eq!(rows, 40);
    assert!(dark_pixels(&flat, 49.0, 85.0, 60.0, 24.0) > 20, "the first row is inked");
}

/// A growable draw in an unavailable typeface takes its width from the
/// fallback face it is drawn in, so its text fits on one line.
#[test]
fn a_growable_draw_is_measured_in_the_face_it_is_drawn_in() {
    let body = r#"<draw name="G"><font typeface="Myriad Pro" size="12pt"/><value><text>Measured in DejaVu Sans</text></value></draw>"#;
    let flat = flatten(&template(body));
    let node = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content.contains("Measured")))
        .expect("the draw");
    let width: f64 = node.width.to_string().parse().unwrap();
    let height: f64 = node.height.to_string().parse().unwrap();
    assert!(width > 100.0 && width < 200.0, "one line of 12pt DejaVu is about 150pt wide, got {width}");
    assert!(height < 20.0, "one line tall, got {height}");
}
