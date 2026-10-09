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

/// A button with a caption and one click script.
fn button(name: &str, caption: &str, script: &str) -> String {
    format!(
        r#"<field name="{name}" w="80pt" h="22pt"><ui><button/></ui><caption><value><text>{caption}</text></value></caption>
<event activity="click" name="ev{name}"><script contentType="application/x-javascript">{script}</script></event></field>"#
    )
}

/// The body both repeat fixtures share: a repeatable `Row` (an `Amount` and a
/// `Remove` button that removes its own row), an `Add` button, and two
/// calculated fields that read every row, so a press is visible in values
/// as well as in the layout. The calculations assign `this.rawValue`: the
/// engine does not take a script's last expression as its result.
fn repeat_body(occur: &str, extra: &str) -> String {
    format!(
        r#"{}<subform name="Row" layout="lr-tb" w="540pt"><occur {occur}/>
<field name="Amount" w="120pt" h="22pt"><ui><textEdit/></ui><value><text>0</text></value></field>
{}</subform>
{}
<field name="Total" w="120pt" h="22pt"><ui><textEdit/></ui><calculate><script contentType="application/x-javascript">var s = 0; var a = this.parent.Row ? this.parent.Row.all : []; for (var i = 0; i &lt; a.length; i++) {{ s += Number(a.item(i).Amount.rawValue); }} this.rawValue = String(s);</script></calculate></field>
<field name="Count" w="120pt" h="22pt"><ui><textEdit/></ui><calculate><script contentType="application/x-javascript">this.rawValue = String(_Row.count);</script></calculate></field>
{extra}"#,
        draw("Title", "Repeat", 30),
        button(
            "Remove",
            "Remove",
            "this.parent.instanceManager.removeInstance(this.parent.index);"
        ),
        button("Add", "Add", "_Row.addInstance(1);"),
    )
}

/// A repeatable section of one to three rows (XFA 3.3 §9), with buttons that
/// add and remove rows, plus a button whose script does something else.
pub fn repeat() -> PathBuf {
    let body = repeat_body(
        r#"min="1" max="3" initial="1""#,
        &button("Noop", "Hello", "xfa.host.messageBox(\"hello\");"),
    );
    write("repeat.xfa.pdf", &template(&body))
}

/// The same section with no row at open and no upper limit.
pub fn repeat_open() -> PathBuf {
    write(
        "repeat_open.xfa.pdf",
        &template(&repeat_body(r#"min="0" max="-1" initial="0""#, "")),
    )
}

/// Field access (XFA 3.3 §17), set every way a form sets it.
///
/// - `RB_Sheet` mirrors UBS's AAGS form: its change script prefills `Sheet`
///   and locks it when `RB_1` is selected, and clears and unlocks it for
///   `RB_2`.
/// - `Locked` is a protected subform around an open field, `Inner`, which
///   inherits the lock (XFA 3.3 §2: an object may only tighten what it
///   inherits).
/// - `Fixed` is readOnly in the template.
/// - `InitLocked` locks itself in its initialize script.
/// - `BadAccess` assigns a value that is not an XFA keyword, which must not
///   change its access.
/// - `LockedButton` is a protected button.
pub fn access() -> PathBuf {
    let body = format!(
        r#"{}<exclGroup name="RB_Sheet" w="400pt" h="44pt" layout="tb">
<field name="RB_1" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>1</text></items></field>
<field name="RB_2" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>2</text></items></field>
<event activity="change" name="eSheet"><script contentType="application/x-javascript">if (this.rawValue == "1") {{ Body.Sheet.rawValue = "1"; Body.Sheet.access = "protected"; }} else {{ Body.Sheet.rawValue = ""; Body.Sheet.access = "open"; }}</script></event>
</exclGroup>
<field name="Sheet" w="200pt" h="22pt"><ui><textEdit/></ui></field>
<subform name="Locked" access="protected" layout="tb" w="540pt">
<field name="Inner" w="200pt" h="22pt"><ui><textEdit/></ui></field>
</subform>
<field name="Fixed" access="readOnly" w="200pt" h="22pt"><ui><textEdit/></ui><value><text>fixed</text></value></field>
<field name="InitLocked" w="200pt" h="22pt"><ui><textEdit/></ui>
<event activity="initialize" name="eInit"><script contentType="application/x-javascript">this.access = "protected";</script></event></field>
<field name="BadAccess" w="200pt" h="22pt"><ui><textEdit/></ui>
<event activity="initialize" name="eBad"><script contentType="application/x-javascript">this.access = "locked";</script></event></field>
<field name="LockedButton" access="protected" w="80pt" h="22pt"><ui><button/></ui><caption><value><text>Go</text></value></caption>
<event activity="click" name="eGo"><script contentType="application/x-javascript">Body.Sheet.rawValue = "pressed";</script></event></field>"#,
        draw("Title", "Access", 30),
    );
    write("access.xfa.pdf", &template(&body))
}
