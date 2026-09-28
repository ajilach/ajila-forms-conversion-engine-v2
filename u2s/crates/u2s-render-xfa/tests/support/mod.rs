//! Synthetic XFA fixtures.
//!
//! Hand-authored XDP templates embedded into a PDF via lopdf, so they travel
//! through the real `/XFA` extraction path rather than bypassing it. Building
//! them in code keeps the repo free of customer documents and makes each
//! fixture's *intent* readable — and it means the e2e suite runs on a machine
//! that has neither the UBS corpus nor a licensed font.

use std::path::{Path, PathBuf};

use lopdf::{Document, Object, Stream, dictionary};

pub fn fixture_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/generated");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

pub fn path(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

/// Wrap an XDP template in the minimum PDF that carries it: a one-page
/// document whose AcroForm holds a packetised `/XFA` array. The array form is
/// deliberate — it is what real forms use, and it exercises packet naming.
fn pdf_with_xfa(xdp: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();

    let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );

    // One named packet, exactly as a real form's /XFA array is shaped.
    let template_id = doc.add_object(Stream::new(dictionary! {}, xdp.as_bytes().to_vec()));
    let acroform_id = doc.add_object(dictionary! {
        "XFA" => vec![
            Object::string_literal("template"),
            Object::Reference(template_id),
        ],
    });

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save pdf");
    out
}

fn template(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet><pageArea name="P1" w="612pt" h="792pt">
<contentArea x="36pt" y="36pt" w="540pt" h="720pt"/></pageArea></pageSet>
<subform name="Body" layout="tb" w="540pt">{body}</subform></subform></template></xdp:xdp>"#
    )
}

/// A draw that starts hidden, so a script making it visible is a *visible*
/// change. A control whose selection alters nothing on screen would make the
/// state tests pass vacuously.
fn hidden_draw(name: &str, text: &str, h: u32) -> String {
    format!(
        r#"<draw name="{name}" w="400pt" h="{h}pt" presence="hidden"><font typeface="Helvetica" size="11pt"/><value><text>{text}</text></value></draw>"#
    )
}

fn draw(name: &str, text: &str, h: u32) -> String {
    format!(
        r#"<draw name="{name}" w="400pt" h="{h}pt"><font typeface="Helvetica" size="11pt"/><value><text>{text}</text></value></draw>"#
    )
}

fn write(name: &str, xdp: &str) -> PathBuf {
    let p = path(name);
    if !p.exists() {
        std::fs::write(&p, pdf_with_xfa(xdp)).expect("write fixture");
    }
    p
}

/// One page: a heading and a field.
pub fn minimal() -> PathBuf {
    write(
        "minimal.xfa.pdf",
        &template(&format!(
            "{}{}",
            draw("Title", "Minimal XFA Fixture", 30),
            r#"<field name="FirstName" w="300pt" h="22pt"><ui><textEdit/></ui><value><text>Ada</text></value></field>"#
        )),
    )
}

/// Content taller than the content area, so the engine records real page
/// breaks and the document genuinely has several pages.
pub fn overflow() -> PathBuf {
    let body: String = (1..=60)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    write("overflow.xfa.pdf", &template(&body))
}

/// A radio group and a checkbox that actually *do* something.
///
/// The scripts are not decoration. State exploration only considers controls
/// with interactive scripts — a checkbox nothing reads cannot change the form,
/// so exploring it would multiply the space for no visible difference. A
/// fixture without scripts therefore has an empty state space, which is
/// correct and useless for testing.
pub fn choices() -> PathBuf {
    let body = format!(
        r#"{}<exclGroup name="RB_Anrede" w="400pt" h="44pt" layout="tb">
<field name="RB_1" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>1</text></items>
<event activity="change" name="e1"><script contentType="application/x-javascript">Shown.presence = "visible";</script></event></field>
<field name="RB_2" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>2</text></items>
<event activity="change" name="e2"><script contentType="application/x-javascript">Shown.presence = "hidden";</script></event></field>
</exclGroup>
<field name="CB_Ok" w="200pt" h="20pt"><ui><checkButton/></ui><items><text>on</text><text>off</text></items>
<event activity="change" name="e3"><script contentType="application/x-javascript">Shown.presence = "visible";</script></event></field>
{}"#,
        draw("Title", "Choices", 30),
        hidden_draw("Shown", "Conditional row", 24)
    );
    write("choices.xfa.pdf", &template(&body))
}
