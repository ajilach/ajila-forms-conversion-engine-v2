//! Viewer chrome: what the renderer draws on top of the template when asked
//! to, and nothing of the form itself. The field underline marks where a
//! person may type, so it belongs to open edit widgets only (XFA 3.3 "The ui
//! element"): a button, check button, image or signature takes no typed
//! value and must show only its own template border.

// The shared helpers also offer a whole-column count this file does not need.
#[allow(dead_code)]
mod support;

use u2s_xfa::flattened::RenderOptions;
use u2s_xfa::{Flattened, XfaNode, fonts};

fn ensure_fonts() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
}

/// Four 20pt-tall fields stacked from the content area's top (y = 36pt):
/// a button, a text edit, an image edit and a signature.
fn form() -> Flattened {
    let xdp = r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet><pageArea name="P1" w="612pt" h="792pt">
<contentArea x="36pt" y="36pt" w="540pt" h="720pt"/></pageArea></pageSet>
<subform name="Body" layout="tb" w="540pt">
<field name="Add" w="200pt" h="20pt"><ui><button highlight="none"/></ui></field>
<field name="Name" w="200pt" h="20pt"><ui><textEdit/></ui></field>
<field name="Photo" w="200pt" h="20pt"><ui><imageEdit/></ui></field>
<field name="Sign" w="200pt" h="20pt"><ui><signature/></ui></field>
</subform></subform></template></xdp:xdp>"#;
    let nodes = XfaNode::parse(xdp.as_bytes()).expect("parse");
    Flattened::from_xfa_simple(&nodes).expect("flatten")
}

/// Dark pixels on the bottom row of the `index`-th field.
fn bottom_row_ink(img: &image::RgbaImage, index: u32) -> usize {
    let bottom = 36.0 + 20.0 * f64::from(index + 1) - 1.0;
    support::dark_pixels_in(img, 36.0, bottom, 200.0, 1.0)
}

#[test]
fn only_an_edit_widget_is_underlined() {
    ensure_fonts();
    let flat = form();

    let plain = flat.render_to_image_buffer_plain(1.0).expect("render");
    for index in 0..4 {
        assert_eq!(
            bottom_row_ink(&plain, index),
            0,
            "field {index} draws a line of its own"
        );
    }

    let chrome = flat
        .render_to_image_buffer_with(
            1.0,
            RenderOptions {
                field_underline: true,
            },
        )
        .expect("render");
    assert_eq!(bottom_row_ink(&chrome, 0), 0, "a button is not underlined");
    assert!(
        bottom_row_ink(&chrome, 1) >= 190,
        "the text edit is underlined along its width"
    );
    assert_eq!(
        bottom_row_ink(&chrome, 2),
        0,
        "an image edit is not underlined"
    );
    assert_eq!(
        bottom_row_ink(&chrome, 3),
        0,
        "a signature is not underlined"
    );
}
