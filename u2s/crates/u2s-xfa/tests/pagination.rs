//! Pages: whole ones, with their master page on each.
//!
//! The layout is a stack of whole pages, so these assert the two things that
//! makes true -- every page is `page.height` tall, and the master page's
//! content is on every page, chosen by `pagePosition` -- plus the page-
//! dependent scripts that can only be answered once the page count is known.

use u2s_xfa::{Flattened, FlattenedNode, FlattenedNodeKind, XfaNode, fonts, prepare_default};

/// Fonts are committed (see `crates/u2s-xfa/src/fonts.rs`
/// `test_support::font_dir`); a machine missing them has a broken checkout,
/// not a reason to skip layout assertions.
macro_rules! require_fonts {
    () => {
        assert!(
            fonts::test_support::ensure_registered(),
            "no fonts registered — see crates/u2s-xfa/src/fonts.rs test_support::font_dir"
        );
    };
}

/// A4 in points, as `<medium stock="a4">` resolves to.
const PAGE_H: f64 = 841.8897637795276;
const PAGE_W: f64 = 595.2755905511812;
/// The contentArea of the templates below: y=30mm, h=255mm.
const CONTENT_TOP: f64 = 85.03937007874016;
const CONTENT_BOTTOM: f64 = CONTENT_TOP + 722.8346456692913;

fn draw(name: &str, text: &str, h: u32) -> String {
    format!(
        r#"<draw name="{name}" w="400pt" h="{h}pt"><font typeface="Helvetica" size="11pt"/><value><text>{text}</text></value></draw>"#
    )
}

/// A template shaped like the UBS masters: a default pageArea carrying a header
/// and a footer, and a `pagePosition="last"` one that adds a closing note.
///
/// `master_extra` goes into *both* pageAreas, the way a shared footer fragment
/// does in the real forms -- so the same named fields exist under two masters,
/// which is exactly the case per-page evaluation has to keep straight.
fn template(body: &str, master_extra: &str) -> String {
    template_with_masters(body, master_extra, master_extra)
}

/// As [`template`], but the two pageAreas' extra content can differ -- the
/// UBS forms' actual shape when a shared fragment (e.g. `Footer_Line`)
/// carries a *different* script on the last page than on the others.
fn template_with_masters(body: &str, mp_extra: &str, mp_last_extra: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs">
<pageArea name="MP">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="Header" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>HEADER EVERY PAGE</text></value></draw>
  <draw name="Footer" x="17.5mm" y="286mm" w="400pt" h="12pt"><font typeface="Helvetica" size="8pt"/><value><text>FOOTER DEFAULT</text></value></draw>
  {mp_extra}
</pageArea>
<pageArea name="MP_Last" pagePosition="last">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="Header" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>HEADER EVERY PAGE</text></value></draw>
  <draw name="LastNote" x="17.5mm" y="274mm" w="400pt" h="12pt"><font typeface="Helvetica" size="8pt"/><value><text>LAST PAGE ONLY</text></value></draw>
  <draw name="Footer" x="17.5mm" y="286mm" w="400pt" h="12pt"><font typeface="Helvetica" size="8pt"/><value><text>FOOTER DEFAULT</text></value></draw>
  {mp_last_extra}
</pageArea>
</pageSet>
<subform name="Body" layout="tb" w="540pt">{body}</subform></subform></template></xdp:xdp>"#
    )
}

fn flatten(xdp: &str) -> Flattened {
    let nodes = XfaNode::parse(xdp.as_bytes()).expect("parse");
    prepare_default(&nodes).expect("prepare").flattened
}

/// Every leaf's text and the page it landed on.
fn texts_by_page(flat: &Flattened) -> Vec<Vec<String>> {
    let mut pages = vec![Vec::new(); flat.page.page_count.max(1) as usize];
    for node in flat.iter_nodes() {
        let y: f64 = node.y.to_string().parse().expect("y");
        let page = (y / PAGE_H).floor() as usize;
        let Some(bucket) = pages.get_mut(page) else {
            continue;
        };
        let content = match &node.kind {
            FlattenedNodeKind::Text { content, .. } => content.trim(),
            FlattenedNodeKind::Field { value, .. } => value.trim(),
            FlattenedNodeKind::Image(_) => "",
        };
        if !content.is_empty() {
            bucket.push(content.to_string());
        }
    }
    pages
}

fn multi_page_form() -> Flattened {
    // 75 rows of 24pt is a shade over 2.5 content areas, so the form is three
    // pages with a short last one.
    let body: String = (1..=75)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    flatten(&template(&body, ""))
}

#[test]
fn a_multi_page_form_is_a_stack_of_whole_pages() {
    require_fonts!();
    let flat = multi_page_form();

    assert_eq!(flat.page.page_count, 3, "75 rows of 24pt is three A4 pages");
    assert!(
        (flat.page.height.to_string().parse::<f64>().unwrap() - PAGE_H).abs() < 0.01,
        "the page size comes from <medium stock=\"a4\">, got {}",
        flat.page.height
    );
    assert!((flat.page.width.to_string().parse::<f64>().unwrap() - PAGE_W).abs() < 0.01);

    // The column is whole pages, including the last one: a short final page
    // would crop the page furniture off the foot of it.
    let column: f64 = flat.page.column_height().to_string().parse().unwrap();
    assert!((column - 3.0 * PAGE_H).abs() < 0.01, "column is {column}");

    // The breaks are the page edges, in order.
    let breaks: Vec<f64> = flat
        .page
        .page_breaks
        .iter()
        .map(|b| b.to_string().parse().unwrap())
        .collect();
    assert_eq!(breaks.len(), 2);
    assert!((breaks[0] - PAGE_H).abs() < 0.01);
    assert!((breaks[1] - 2.0 * PAGE_H).abs() < 0.01);
}

/// The failure this is really about: body content running into the margins,
/// where it collides with the header and footer drawn there.
#[test]
fn body_content_stays_inside_the_content_area_of_its_page() {
    require_fonts!();
    let flat = multi_page_form();

    for node in flat.iter_nodes() {
        let FlattenedNodeKind::Text { content, .. } = &node.kind else {
            continue;
        };
        if !content.starts_with("Row ") {
            continue;
        }
        let y: f64 = node.y.to_string().parse().unwrap();
        let h: f64 = node.height.to_string().parse().unwrap();
        let page = (y / PAGE_H).floor();
        let top = y - page * PAGE_H;
        assert!(
            top >= CONTENT_TOP - 0.5,
            "{content:?} starts at {top} on its page, above the content area"
        );
        assert!(
            top + h <= CONTENT_BOTTOM + 0.5,
            "{content:?} ends at {} on its page, below the content area",
            top + h
        );
    }
}

#[test]
fn every_row_appears_on_exactly_one_page() {
    require_fonts!();
    let flat = multi_page_form();
    let pages = texts_by_page(&flat);

    for i in 1..=75 {
        let row = format!("Row {i}");
        let on: Vec<usize> = pages
            .iter()
            .enumerate()
            .filter(|(_, texts)| texts.iter().any(|t| t == &row))
            .map(|(p, _)| p)
            .collect();
        assert_eq!(
            on.len(),
            1,
            "{row} appears on pages {on:?}, want exactly one"
        );
    }
    assert!(pages[0].iter().any(|t| t == "Row 1"));
    assert!(pages[2].iter().any(|t| t == "Row 75"));
}

#[test]
fn the_master_page_is_drawn_on_every_page() {
    require_fonts!();
    let flat = multi_page_form();
    let pages = texts_by_page(&flat);

    for (i, texts) in pages.iter().enumerate() {
        assert!(
            texts.iter().any(|t| t == "HEADER EVERY PAGE"),
            "page {} has no header: {texts:?}",
            i + 1
        );
        assert!(
            texts.iter().any(|t| t == "FOOTER DEFAULT"),
            "page {} has no footer",
            i + 1
        );
    }
}

/// `pagePosition="last"` picks a different master for the closing page, which
/// is where the legal footnote of the UBS forms lives.
#[test]
fn the_last_page_master_is_used_for_the_last_page_only() {
    require_fonts!();
    let pages = texts_by_page(&multi_page_form());

    let on: Vec<usize> = pages
        .iter()
        .enumerate()
        .filter(|(_, t)| t.iter().any(|t| t == "LAST PAGE ONLY"))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        on,
        vec![2],
        "the closing note belongs to the last page alone"
    );
}

/// A one-page document is its own last page, so it gets the closing master.
#[test]
fn a_single_page_document_uses_the_closing_master() {
    require_fonts!();
    let flat = flatten(&template(&draw("Only", "One row", 24), ""));
    assert_eq!(flat.page.page_count, 1);

    let pages = texts_by_page(&flat);
    assert!(pages[0].iter().any(|t| t == "LAST PAGE ONLY"));
    assert!(pages[0].iter().any(|t| t == "HEADER EVERY PAGE"));
}

/// `xfa.layout.page()` and `pageCount()` cannot be answered until the body has
/// been laid out, so they are evaluated once per page afterwards -- which is
/// the only way a footer can say "2/3".
#[test]
fn page_dependent_master_scripts_are_evaluated_per_page() {
    require_fonts!();
    let body: String = (1..=75)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let master = r#"<field name="Pagination" x="150mm" y="286mm" w="40mm" h="12pt"><font typeface="Helvetica" size="8pt"/><ui><textEdit/></ui>
<event name="e" activity="initialize"><script contentType="application/x-javascript">
if (!this.rawValue) this.rawValue = "Page " + xfa.layout.page(this) + "/" + xfa.layout.pageCount();
</script></event></field>
<subform name="FirstOnly" x="17.5mm" y="20mm" w="400pt">
<event name="e2" activity="initialize"><script contentType="application/x-javascript">
this.presence = MP.index &lt; 1 ? "visible" : "hidden";
</script></event>
<draw name="FirstNote" w="400pt" h="12pt"><font typeface="Helvetica" size="8pt"/><value><text>FIRST PAGE ONLY</text></value></draw>
</subform>"#;

    let flat = flatten(&template(&body, master));
    assert_eq!(flat.page.page_count, 3);
    let pages = texts_by_page(&flat);

    for (i, texts) in pages.iter().enumerate() {
        let want = format!("Page {}/3", i + 1);
        assert!(
            texts.iter().any(|t| t == &want),
            "page {} should be stamped {want:?}, has {texts:?}",
            i + 1
        );
    }

    // `MP.index` is the page being laid out, so a first-page-only block is on
    // the first page and nowhere else.
    let on: Vec<usize> = pages
        .iter()
        .enumerate()
        .filter(|(_, t)| t.iter().any(|t| t == "FIRST PAGE ONLY"))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(on, vec![0]);
}

/// Two pageAreas can share a fragment name (`Footer_Line`, `Pagination`) with
/// *different* scripts on each -- the UBS forms do this because the closing
/// page's footer computes "N/N" instead of "current/N". Before this fix,
/// `LayoutScripts::evaluate` ran every master event on every page regardless
/// of which pageArea the page actually used, so whichever pageArea's script
/// happened to run last in document order (`MP_Last`, here unguarded on
/// purpose) silently overwrote the other's answer on every page, not just
/// its own.
#[test]
fn master_page_scripts_run_only_for_their_own_page_area() {
    require_fonts!();
    let body: String = (1..=75)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let mp_extra = r#"<field name="Pagination" x="150mm" y="286mm" w="40mm" h="12pt"><font typeface="Helvetica" size="8pt"/><ui><textEdit/></ui>
<event name="e" activity="initialize"><script contentType="application/x-javascript">
if (!this.rawValue) this.rawValue = "P" + (MP.index + 1) + "/" + xfa.layout.pageCount();
</script></event></field>"#;
    let mp_last_extra = r#"<field name="Pagination" x="150mm" y="286mm" w="40mm" h="12pt"><font typeface="Helvetica" size="8pt"/><ui><textEdit/></ui>
<event name="e" activity="initialize"><script contentType="application/x-javascript">
this.rawValue = "LAST " + xfa.layout.pageCount();
</script></event></field>"#;

    let flat = flatten(&template_with_masters(&body, mp_extra, mp_last_extra));
    assert_eq!(flat.page.page_count, 3);
    let pages = texts_by_page(&flat);

    assert!(
        pages[0].iter().any(|t| t == "P1/3"),
        "page 1 should be stamped by MP's own script, has {:?}",
        pages[0]
    );
    assert!(
        pages[1].iter().any(|t| t == "P2/3"),
        "page 2 should be stamped by MP's own script, has {:?}",
        pages[1]
    );
    assert!(
        pages[2].iter().any(|t| t == "LAST 3"),
        "page 3 should be stamped by MP_Last's own script, has {:?}",
        pages[2]
    );
}

/// A script object declared in `<variables>` is, per XFA 3.3 §10, "registered
/// with the subform" that declares it -- so `form1.soAlt.set()` must resolve,
/// and a write it makes to another master-page node
/// (`form1.pageSet.MP_Last.B.presence = "visible"`, reached the way Designer
/// actually emits it) must be visible to that node's own flattening, not just
/// to the object that ran the script. Before this fix `pageSet`/`pageArea`
/// were not addressable this way at all, the script threw, and both `A` and
/// `B` -- positioned on top of each other on purpose, as alternates -- stayed
/// at their default "visible" presence and rendered together.
#[test]
fn a_script_object_hides_master_alternates_through_the_page_set() {
    require_fonts!();
    let mp_last_extra = r#"<subform name="A" x="17.5mm" y="260mm" w="100mm" h="10pt">
<draw name="NoteA" w="100mm" h="10pt"><font typeface="Helvetica" size="8pt"/><value><text>ALT A</text></value></draw>
</subform>
<subform name="B" x="17.5mm" y="260mm" w="100mm" h="10pt" presence="hidden">
<draw name="NoteB" w="100mm" h="10pt"><font typeface="Helvetica" size="8pt"/><value><text>ALT B</text></value></draw>
</subform>
<subform name="Trigger" x="17.5mm" y="272mm" w="100mm" h="10pt">
<event name="e" activity="initialize"><script contentType="application/x-javascript">
var top;
try{ top = UBSForms.form1; } catch(e){ top = form1; }
top.soAlt.set();
</script></event>
</subform>"#;

    let variables = r#"<variables><script name="soAlt" contentType="application/x-javascript">
function set(){
	var mp = form1.pageSet.MP_Last;
	mp.A.presence = "hidden";
	mp.B.presence = "visible";
}
</script></variables>"#;

    let xdp = template_with_masters("", "", mp_last_extra)
        .replacen("<subform name=\"form1\" layout=\"tb\">", &format!("<subform name=\"form1\" layout=\"tb\">{variables}"), 1);

    let flat = flatten(&xdp);
    assert_eq!(flat.page.page_count, 1);
    let pages = texts_by_page(&flat);

    assert!(
        pages[0].iter().any(|t| t == "ALT B"),
        "the script should have turned B on, page has {:?}",
        pages[0]
    );
    assert!(
        !pages[0].iter().any(|t| t == "ALT A"),
        "the script should have turned A off, page has {:?}",
        pages[0]
    );
}

/// `<keep next="contentArea" intact="contentArea"/>` (XFA 3.3 §8 "Content
/// Splitting") asks that a subform be kept whole *and* in the same content
/// area as the subform right after it. 29 rows of 24pt leave 26.83pt of page
/// 1 free -- enough, alone, for the 20pt "Sig" subform, but not for "Sig"
/// plus the 300pt "Spacer" draw after it. Without the `keep`, "Sig" (an
/// unsplit leaf either way) simply stays on page 1 and "Spacer" moves to page
/// 2. With it, the two are a single unit that does not fit in what is left
/// of page 1, so both move to page 2 together.
#[test]
fn keep_next_moves_a_subform_with_its_successor() {
    require_fonts!();
    let rows: String = (1..=29)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let spacer = draw("Spacer", "SPACER", 300);

    let without_keep = format!(
        r#"{rows}<subform name="Sig" layout="tb" w="400pt"><draw name="SignDraw" w="400pt" h="20pt"><font typeface="Helvetica" size="8pt"/><value><text>SIGN</text></value></draw></subform>{spacer}"#
    );
    let baseline = texts_by_page(&flatten(&template(&without_keep, "")));
    assert!(
        baseline[0].iter().any(|t| t == "SIGN"),
        "sanity check: SIGN should fit on page 1 without a keep constraint, has {:?}",
        baseline[0]
    );

    let with_keep = format!(
        r#"{rows}<subform name="Sig" layout="tb" w="400pt"><keep next="contentArea" intact="contentArea"/><draw name="SignDraw" w="400pt" h="20pt"><font typeface="Helvetica" size="8pt"/><value><text>SIGN</text></value></draw></subform>{spacer}"#
    );
    let pages = texts_by_page(&flatten(&template(&with_keep, "")));
    assert!(
        !pages[0].iter().any(|t| t == "SIGN"),
        "keep next=\"contentArea\" should move SIGN off page 1 along with SPACER, page 1 has {:?}",
        pages[0]
    );
    assert!(
        pages.len() > 1 && pages[1].iter().any(|t| t == "SIGN"),
        "SIGN should land on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
    assert!(
        pages[1].iter().any(|t| t == "SPACER"),
        "SPACER should be on the same page as SIGN, page 2 has {:?}",
        pages[1]
    );
}

/// `breakBefore`'s `targetType` defaults to `auto` -- "layout continues using
/// the current layout container" (XFA 3.3 §7 "Break Conditions") -- so a bare
/// `<breakBefore/>` with no `targetType`, as Designer commonly leaves behind
/// after reordering, forces nothing. Only an explicit `targetType="pageArea"`
/// (or `contentArea`/`pageEven`/`pageOdd`) does.
#[test]
fn breakbefore_only_forces_a_page_start_with_an_explicit_target_type() {
    require_fonts!();
    let rows: String = (1..=29)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let marker_draw = r#"<draw name="M" w="400pt" h="10pt"><font typeface="Helvetica" size="8pt"/><value><text>MARKER</text></value></draw>"#;

    let bare = format!(
        r#"{rows}<subform name="Marker" layout="tb" w="400pt"><breakBefore/>{marker_draw}</subform>"#
    );
    let pages_bare = texts_by_page(&flatten(&template(&bare, "")));
    assert!(
        pages_bare[0].iter().any(|t| t == "MARKER"),
        "a bare breakBefore should not force a page break, page 1 has {:?}",
        pages_bare[0]
    );

    let forced = format!(
        r#"{rows}<subform name="Marker" layout="tb" w="400pt"><breakBefore targetType="pageArea"/>{marker_draw}</subform>"#
    );
    let pages_forced = texts_by_page(&flatten(&template(&forced, "")));
    assert!(
        !pages_forced[0].iter().any(|t| t == "MARKER"),
        "breakBefore targetType=\"pageArea\" should move MARKER off page 1, page 1 has {:?}",
        pages_forced[0]
    );
    assert!(
        pages_forced.len() > 1 && pages_forced[1].iter().any(|t| t == "MARKER"),
        "MARKER should land on page 2, pages have {:?}",
        pages_forced.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}

// ── page furniture: images, barcodes, round radios ──────────────────────────

/// Count the dark pixels in a rectangle of a rendered page, in points.
fn dark_pixels(flat: &Flattened, x: f64, y: f64, w: f64, h: f64) -> usize {
    let img = flat.render_to_image_buffer_plain(1.0).expect("render");
    let (x0, y0) = (x.max(0.0) as u32, y.max(0.0) as u32);
    let (x1, y1) = (
        ((x + w) as u32).min(img.width()),
        ((y + h) as u32).min(img.height()),
    );
    let mut dark = 0;
    for py in y0..y1 {
        for px in x0..x1 {
            let p = img.get_pixel(px, py).0;
            if p[0] < 200 && p[1] < 200 && p[2] < 200 {
                dark += 1;
            }
        }
    }
    dark
}

/// A 2x2 all-black PNG, base64, as a template would carry it.
fn black_png_base64() -> String {
    use base64::Engine as _;
    let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("encode png");
    base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
}

/// A `<draw>` whose value is an `<image>` draws the image. Before this the
/// whole payload was ignored and the UBS logo simply was not on the page.
#[test]
fn an_embedded_image_is_drawn() {
    require_fonts!();
    // At y=20mm the box clears the master's own header draw, so the ink
    // measured below is the image's alone.
    let master = format!(
        r#"<draw name="Logo" x="17.5mm" y="20mm" w="60pt" h="24pt"><ui><imageEdit/></ui><value><image contentType="image/png">{}</image></value></draw>"#,
        black_png_base64()
    );
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));

    // A black square fitted into a 60x24pt box fills a 24x24pt square of it.
    let dark = dark_pixels(&flat, 49.0, 57.0, 62.0, 23.0);
    assert!(dark > 400, "the image box has only {dark} dark pixels");
}

/// A barcode field draws bars, not the digits of its value.
#[test]
fn a_barcode_field_is_drawn_as_bars() {
    require_fonts!();
    // At y=20mm the box clears the master's own header draw.
    let master = r#"<field name="BC" x="17.5mm" y="20mm" w="200pt" h="24pt"><font typeface="Helvetica" size="6pt"/><ui><barcode type="code2Of5Interleaved"/></ui><value><text>0101665</text></value></field>"#;
    let flat = flatten(&template(&draw("Only", "One row", 24), master));

    let dark = dark_pixels(&flat, 49.0, 57.0, 202.0, 23.0);
    assert!(dark > 300, "the barcode box has only {dark} dark pixels");

    // Bars, not glyphs: a barcode's ink is in full-height vertical strokes, so
    // the top and the bottom of the box carry the same amount of it.
    let top = dark_pixels(&flat, 49.0, 58.0, 202.0, 8.0);
    let bottom = dark_pixels(&flat, 49.0, 71.0, 202.0, 8.0);
    assert!(
        top > 0 && bottom > 0 && (top as f64 - bottom as f64).abs() / top as f64 <= 0.1,
        "bars should run the height of the box, got {top} at the top and {bottom} at the foot"
    );
}

/// An unsupported symbology is reported and marked, not silently skipped: a
/// blank space would read as a form with no barcode on it.
#[test]
fn an_unsupported_barcode_draws_a_placeholder() {
    require_fonts!();
    let master = r#"<field name="BC" x="17.5mm" y="20mm" w="200pt" h="24pt"><font typeface="Helvetica" size="6pt"/><ui><barcode type="qrCode"/></ui><value><text>anything</text></value></field>"#;
    let flat = flatten(&template(&draw("Only", "One row", 24), master));

    let dark = dark_pixels(&flat, 49.0, 57.0, 202.0, 23.0);
    assert!(
        dark > 100,
        "an unsupported barcode should still mark its box"
    );
}

/// `<checkButton shape="round">` is a radio, and a radio is a circle. A square
/// in its place reads as a different control.
#[test]
fn a_round_check_button_is_drawn_as_a_circle() {
    require_fonts!();
    let body = r#"<exclGroup name="G" w="400pt" h="30pt"><field name="RB_1" x="0pt" y="0pt" w="20pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>1</text></items></field></exclGroup>"#;
    let flat = flatten(&template(body, ""));

    // The widget sits at the top left of the content area.
    let (bx, by) = (49.6, CONTENT_TOP);
    // A circle leaves its bounding box's corners white; a square outline does not.
    let corner = dark_pixels(&flat, bx, by, 4.0, 4.0);
    assert_eq!(corner, 0, "the corner of a round radio should be blank");
    // But there is a ring: the middle of the top edge is inked.
    let top_edge = dark_pixels(&flat, bx + 7.0, by + 1.0, 6.0, 3.0);
    assert!(top_edge > 0, "the radio has no ring");
}

/// A `<draw>` with no width is sized to its text -- but its margin insets are
/// taken off again when the text is laid out inside it, so they have to be part
/// of that width. Without this the radio labels of the UBS forms (5mm and 3mm
/// insets around a three-letter word) got a zero-wide text box and vanished.
#[test]
fn a_growable_draw_is_wide_enough_for_its_text_and_its_insets() {
    require_fonts!();
    let body = r#"<draw name="Label"><font typeface="Helvetica" size="8pt"/><margin leftInset="5mm" rightInset="3mm"/><value><text>USD</text></value></draw>"#;
    let flat = flatten(&template(body, ""));

    let label = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content == "USD"))
        .expect("the label should be on the page");

    let width: f64 = label.width.to_string().parse().unwrap();
    assert!(
        width > 0.0,
        "the label's text box is {width}pt wide, so nothing can be drawn in it"
    );
}

/// A growable rich-text draw's natural height must actually hold all of its
/// wrapped paragraphs -- not just one line's worth of the first paragraph.
/// This is the UBS "Verlangte Sicherheiten" row: a bold one-line heading
/// followed by a sentence that wraps across several lines once the draw's
/// own right inset narrows it. Before this fix, natural height was measured
/// at the draw's outer width (ignoring the inset, so fewer lines wrapped
/// than were actually drawn) and summed each paragraph's line gap instead of
/// removing it once for the whole block -- undercounting by nearly a full
/// line. The next draw in the `tb` flow would then start inside this one's
/// own wrapped text.
#[test]
fn a_growable_rich_text_draw_is_as_tall_as_its_paragraphs() {
    require_fonts!();
    let body = r#"<draw name="Label" w="60mm"><font typeface="Helvetica" size="8pt"/><margin rightInset="5mm"/><para lineHeight="9pt"/><value><exData contentType="text/html"><body xmlns:xfa="http://www.xfa.org/schema/xfa-data/1.0/" xmlns="http://www.w3.org/1999/xhtml"><p style="font-weight:bold">Verlangte Sicherheiten</p><p>Beschreibung der von Ihnen im Zusammenhang mit dem Kreditvertrag zu stellenden Sicherheiten</p></body></exData></value></draw><draw name="After" w="400pt" h="12pt"><font typeface="Helvetica" size="8pt"/><value><text>AFTER</text></value></draw>"#;
    let flat = flatten(&template(body, ""));

    let heading = flat
        .iter_nodes()
        .find(|n| {
            matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content == "Verlangte Sicherheiten")
        })
        .expect("the heading paragraph should be on the page");
    let wrapped = flat
        .iter_nodes()
        .find(|n| {
            matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content.starts_with("Beschreibung der von Ihnen"))
        })
        .expect("the wrapped body paragraph should be on the page");
    let after = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content == "AFTER"))
        .expect("the following draw should be on the page");

    let heading_y: f64 = heading.y.to_string().parse().unwrap();
    let heading_h: f64 = heading.height.to_string().parse().unwrap();
    let wrapped_h: f64 = wrapped.height.to_string().parse().unwrap();
    let after_y: f64 = after.y.to_string().parse().unwrap();

    // The one-line bold heading is exactly one 9pt line.
    assert!(
        (heading_h - 9.0).abs() < 0.5,
        "the single-line heading should be one 9pt line, got {heading_h}pt"
    );
    // Narrowed by the 5mm right inset (55mm of content width), the sentence
    // must wrap onto at least three lines of 9pt -- Acrobat wraps this exact
    // sentence onto four.
    assert!(
        wrapped_h >= 3.0 * 9.0 - 1.0,
        "the body paragraph should wrap onto at least three 9pt lines, got {wrapped_h}pt tall"
    );

    // The draw's total natural height (used to advance the `tb` flow cursor)
    // may fall one line gap short of the sum of its paragraph boxes -- AXTE
    // removes the trailing line gap after a block's last line, which is
    // whitespace, not text -- but not by more than that.
    let content_bottom = heading_y + heading_h + wrapped_h;
    assert!(
        after_y >= content_bottom - 3.0,
        "the next draw starts at {after_y}pt, inside this draw's own content which runs to {content_bottom}pt"
    );
}

/// Master-page content is normally addressed by absolute x/y ("positioned
/// layout"), but a subform can still declare `layout="lr-tb"` for its own
/// children -- the header of every UBS form does exactly this to place a
/// caption draw and a value draw side by side, wrapping to a second line
/// once the row is full. Before this fix, `flatten_single_node` (used only
/// for pageArea content) ignored a node's own `layout` attribute entirely
/// and placed every child at its raw, mostly-absent x/y -- so the caption
/// and the value landed on top of each other at (0, 0) relative to the
/// subform, on every single page.
#[test]
fn master_page_subform_with_lr_tb_layout_wraps_its_children() {
    require_fonts!();
    fn mm(v: f64) -> f64 {
        v * 72.0 / 25.4
    }

    let header = r#"<subform name="BankingRelationship" layout="lr-tb" w="55mm" x="20mm" y="20mm">
<draw w="55mm" name="Cap" h="3.2mm"><font typeface="Helvetica" size="8pt"/><value><text>Bankbeziehung</text></value></draw>
<draw w="8.5mm" name="Num" h="4mm"><font typeface="Helvetica" size="8pt"/><value><text>0319</text></value></draw>
<draw y="3mm" w="5.099mm" name="Dash" h="4mm"><font typeface="Helvetica" size="8pt"/><value><text> 00</text></value></draw>
</subform>"#;
    let flat = flatten(&template("", header));

    let find = |name: &str| -> &FlattenedNode {
        flat.iter_nodes()
            .find(|n| match &n.kind {
                FlattenedNodeKind::Text { source_name, .. } => {
                    source_name.as_deref() == Some(name)
                }
                _ => false,
            })
            .unwrap_or_else(|| panic!("draw {name:?} not on the page"))
    };

    let cap = find("Cap");
    let num = find("Num");
    let dash = find("Dash");

    let cap_x: f64 = cap.x.to_string().parse().unwrap();
    let cap_y: f64 = cap.y.to_string().parse().unwrap();
    let num_x: f64 = num.x.to_string().parse().unwrap();
    let num_y: f64 = num.y.to_string().parse().unwrap();
    let dash_x: f64 = dash.x.to_string().parse().unwrap();
    let dash_y: f64 = dash.y.to_string().parse().unwrap();

    // "Cap" (w=55mm) fills the whole row by itself, at the subform's own
    // position -- an lr-tb child's placement comes from the flow, not from
    // its own (here absent, i.e. zero) x/y attributes.
    assert!((cap_x - mm(20.0)).abs() < 0.5, "Cap.x = {cap_x}");
    assert!((cap_y - mm(20.0)).abs() < 0.5, "Cap.y = {cap_y}");

    // "Num" cannot fit next to "Cap" (55mm + 8.5mm > the 55mm row width), so
    // it wraps to a new row directly below "Cap" -- this is why Acrobat
    // shows "Bankbeziehung" on one line and "0319 00" on the next.
    assert!(
        (num_x - mm(20.0)).abs() < 0.5,
        "Num should start a new row at the subform's left edge, got x={num_x}"
    );
    assert!(
        (num_y - (mm(20.0) + mm(3.2))).abs() < 0.5,
        "Num should be directly below Cap (height 3.2mm), got y={num_y}"
    );

    // "Dash" declares `y="3mm"`, but lr-tb ignores a child's own x/y: it
    // fits next to "Num" (8.5mm + 5.099mm < 55mm), on the same row.
    assert!(
        (dash_x - (mm(20.0) + mm(8.5))).abs() < 0.5,
        "Dash's authored y=3mm must be ignored by lr-tb; it should sit right of Num, got x={dash_x}"
    );
    assert!(
        (dash_y - num_y).abs() < 0.5,
        "Dash should be on Num's row, not shifted by its own y=3mm; got y={dash_y}, Num.y={num_y}"
    );
}

/// A document can have more than one `pageSet` (AAOV: `MP1Set` for its first
/// two pages, `MP2Set` for its third), and a content subform's own
/// `<breakBefore targetType="pageArea" target="...">` says which one its own
/// pages use (XFA 3.3 §7) -- not document order, and not "every pageArea in
/// the document" (which is what a single shared pageSet degrades to, and
/// what this used to be treated as unconditionally). Two content subforms
/// here: the first overflows onto a second page within `Set1` (so its
/// `pagePosition="last"` area is used only for that second page, not for
/// the first), the second is one page long and targets `Set2` -- a wholly
/// different pageSet -- and must not pick up any of `Set1`'s master content.
#[test]
fn a_content_subform_can_target_a_different_page_set_than_the_one_before_it() {
    require_fonts!();
    let rows: String = (1..=35)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let xdp = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs">
<pageSet name="Set1">
<pageArea name="A1">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="H1" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>SET1_A1</text></value></draw>
</pageArea>
<pageArea name="A1Last" pagePosition="last">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="H1L" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>SET1_LAST</text></value></draw>
</pageArea>
</pageSet>
<pageSet name="Set2">
<pageArea name="A2">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="H2" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>SET2_A2</text></value></draw>
</pageArea>
</pageSet>
</pageSet>
<subform name="Page1" layout="tb" w="540pt">
<breakBefore targetType="pageArea" target="Set1.A1"/>
{rows}
</subform>
<subform name="Page2" layout="tb" w="540pt">
<breakBefore targetType="pageArea" target="Set2.A2"/>
<draw name="P2" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>PAGE2 CONTENT</text></value></draw>
</subform>
</subform></template></xdp:xdp>"#
    );

    let flat = flatten(&xdp);
    assert_eq!(
        flat.page.page_count, 3,
        "35 rows overflow Set1 onto a second page, plus one page for Page2"
    );

    let pages = texts_by_page(&flat);
    assert!(
        pages[0].iter().any(|t| t == "SET1_A1"),
        "page 1 should use Set1's ordinary area, got {:?}",
        pages[0]
    );
    assert!(
        pages[1].iter().any(|t| t == "SET1_LAST"),
        "page 2 is the last page of Page1's own run within Set1, so it should use \
         Set1's pagePosition=\"last\" area, got {:?}",
        pages[1]
    );
    assert!(
        pages[2].iter().any(|t| t == "SET2_A2"),
        "page 3 (Page2's own, targeting Set2) should use Set2's area, got {:?}",
        pages[2]
    );
    assert!(
        !pages[2].iter().any(|t| t.starts_with("SET1")),
        "page 3 must not pick up any of Set1's master content, got {:?}",
        pages[2]
    );
}

/// Two content subforms that neither name a pageArea target still share one
/// implicit run spanning every pageArea in the document -- the historical
/// behaviour for a document with a single pageSet (or, like AAGS, several
/// untargeted content subforms each meant to be exactly one page): the
/// first page is "first"/"any", the very last page is "last", regardless of
/// how many subforms contributed pages in between.
#[test]
fn untargeted_content_subforms_still_share_one_document_wide_run() {
    require_fonts!();
    let xdp = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs">
<pageArea name="MP">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="H" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>ORDINARY</text></value></draw>
</pageArea>
<pageArea name="MP_Last" pagePosition="last">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
  <draw name="HL" x="17.5mm" y="10mm" w="400pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>LAST</text></value></draw>
</pageArea>
</pageSet>
<subform name="Page" layout="tb" w="540pt">{}</subform>
<subform name="Page_66439" layout="tb" w="540pt">{}</subform>
</subform></template></xdp:xdp>"#,
        draw("A", "Page A content", 20),
        draw("B", "Page B content", 20)
    );

    let flat = flatten(&xdp);
    assert_eq!(flat.page.page_count, 2, "each untargeted subform is one page");

    let pages = texts_by_page(&flat);
    assert!(
        pages[0].iter().any(|t| t == "ORDINARY"),
        "page 1 should use the ordinary area, got {:?}",
        pages[0]
    );
    assert!(
        pages[1].iter().any(|t| t == "LAST"),
        "page 2, the last page overall, should use the pagePosition=\"last\" area \
         even though it came from a different content subform than page 1, got {:?}",
        pages[1]
    );
}

/// A growable subform (no explicit `h`, common for a body wrapper like AAOV's
/// `BankingRelationship`) is given height 0 by the layout engine *before* its
/// children are laid out -- "containers grow to fit their children". A
/// `<border>`'s edges used to be propagated onto the subform's enclosed
/// leaves using that still-zero height, so a bottom-only edge (a plain
/// underline under a header field) was drawn at the subform's *top* instead
/// of its true bottom.
#[test]
fn a_growable_subforms_border_is_propagated_at_its_true_bottom() {
    require_fonts!();
    let inner = format!("{}{}", draw("L1", "Line1", 20), draw("L2", "Line2", 20));
    let body = format!(
        r#"<subform name="BorderTest" layout="tb" w="400pt">
<border><edge presence="hidden"/><edge presence="hidden"/><edge thickness="1pt"/><edge presence="hidden"/></border>
{inner}
</subform>"#
    );
    let flat = flatten(&template(&body, ""));

    let l1 = flat
        .iter_nodes()
        .find(|n| {
            matches!(&n.kind, FlattenedNodeKind::Text { source_name, .. } if source_name.as_deref() == Some("L1"))
        })
        .expect("L1 draw not found");

    let border = l1
        .style
        .border
        .as_ref()
        .expect("BorderTest's bottom edge should have propagated onto its first leaf");
    let bottom = border.get_edge(2).expect("bottom edge");
    assert_eq!(bottom.presence, "visible");
    let (_, _, _, h) = border
        .render_bounds
        .expect("a propagated edge must carry render_bounds so it draws at the subform's position");
    let h: f64 = h.to_string().parse().unwrap();
    assert!(
        h > 30.0,
        "the propagated border's height must be the subform's true (grown) height, ~40pt \
         for two 20pt rows, not the 0 it reads before its children are laid out; got {h}pt"
    );
}

/// A subform's `<border><fill>` is its background colour (AAOV's "Form
/// configurator" pink panel: every edge hidden, but a visible fill) --
/// propagated the same way a visible edge is, onto the first enclosed leaf,
/// at the subform's true (grown) bounds.
#[test]
fn a_growable_subforms_fill_is_propagated_onto_its_first_leaf() {
    require_fonts!();
    let inner = format!("{}{}", draw("L1", "Line1", 20), draw("L2", "Line2", 20));
    let body = format!(
        r#"<subform name="FillTest" layout="tb" w="400pt">
<border><edge presence="hidden"/><fill><color value="255,244,244"/></fill></border>
{inner}
</subform>"#
    );
    let flat = flatten(&template(&body, ""));

    let l1 = flat
        .iter_nodes()
        .find(|n| {
            matches!(&n.kind, FlattenedNodeKind::Text { source_name, .. } if source_name.as_deref() == Some("L1"))
        })
        .expect("L1 draw not found");

    let border = l1
        .style
        .border
        .as_ref()
        .expect("FillTest's fill should have propagated onto its first leaf");
    let fill = border.fill.as_ref().expect("fill propagated");
    assert_eq!(fill.color, Some((255, 244, 244)));
    let (_, _, _, h) = border
        .render_bounds
        .expect("a propagated fill must carry render_bounds so it paints the subform's box");
    let h: f64 = h.to_string().parse().unwrap();
    assert!(
        h > 30.0,
        "the propagated fill's height must be the subform's true (grown) height, not 0; got {h}pt"
    );
}

/// A draw's own `<border>` draws around its outer (margin-inclusive) box,
/// not its margin-inset content box -- the rule above a UBS section heading
/// (a visible top edge plus `topInset`) must sit clear of the heading text,
/// not flush against its first line. Per XFA 3.3 ch. 8 "Layout for Growable
/// Objects": "borders have no effect upon the nominal extent" of an object.
#[test]
fn a_draws_own_border_is_drawn_at_its_outer_box() {
    require_fonts!();
    let body = r#"<draw name="Heading" w="400pt" minH="30pt">
<font typeface="Helvetica" size="11pt"/>
<value><text>Heading text</text></value>
<margin topInset="10pt" bottomInset="2pt"/>
<border><edge thickness="1pt"/><edge presence="hidden"/><edge presence="hidden"/><edge presence="hidden"/></border>
</draw>"#;
    let flat = flatten(&template(body, ""));

    let heading = flat
        .iter_nodes()
        .find(|n| {
            matches!(&n.kind, FlattenedNodeKind::Text { source_name, .. } if source_name.as_deref() == Some("Heading"))
        })
        .expect("Heading draw not found");
    let content_top: f64 = heading.y.to_string().parse().unwrap();

    let border = heading
        .style
        .border
        .as_ref()
        .expect("Heading's own border should be present");
    let (_, render_top, _, _) = border.render_bounds.expect(
        "a leaf's own border must carry render_bounds so it draws at its outer box, not its content box",
    );
    let render_top: f64 = render_top.to_string().parse().unwrap();

    assert!(
        (content_top - render_top - 10.0).abs() < 0.01,
        "the border should draw 10pt (topInset) above the content top ({content_top}pt), \
         at {render_top}pt, not flush against it (which was the old, wrong, behaviour)"
    );
}

/// A subform whose own `layout` is unspecified (positioned, XFA's default)
/// must move whole to the next page rather than have its own children
/// scattered across a page break -- no viewer we've observed ever tears a
/// positioned subform's contents apart from each other, even though the
/// XFA 3.3 spec's split-consensus algorithm (ch. 8 "Content Splitting")
/// would in principle allow it.
#[test]
fn a_positioned_subform_is_never_split_across_pages() {
    require_fonts!();
    let rows: String = (1..=29)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let boxed = r#"<subform name="Boxed" w="300pt">
<draw name="P1" y="0pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P1</text></value></draw>
<draw name="P2" y="40pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P2</text></value></draw>
<draw name="P3" y="80pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P3</text></value></draw>
</subform>"#;
    let body = format!("{rows}{boxed}");
    let pages = texts_by_page(&flatten(&template(&body, "")));

    assert!(
        pages[0].iter().any(|t| t == "Row 29"),
        "sanity check: the 29 filler rows should still all fit on page 1, has {:?}",
        pages[0]
    );
    assert!(
        !pages[0].iter().any(|t| t == "P1" || t == "P2" || t == "P3"),
        "a positioned subform must move whole to the next page rather than \
         split, page 1 has {:?}",
        pages[0]
    );
    assert!(
        pages.len() > 1
            && pages[1].iter().any(|t| t == "P1")
            && pages[1].iter().any(|t| t == "P2")
            && pages[1].iter().any(|t| t == "P3"),
        "P1, P2 and P3 should all land together on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}

/// A subform with `<keep next="contentArea"/>` immediately followed by an
/// unsplittable positioned subform: snapping the page break out of the
/// positioned subform's own range can land inside the *preceding* subform's
/// keep-next range (it and the positioned subform's first child must stay
/// together), which must itself be honoured -- not just the first range the
/// break happens to hit. Without re-checking after each snap, the keep-next
/// subform would be orphaned on the earlier page while the positioned
/// subform moved whole to the next one.
#[test]
fn keep_next_follows_into_an_unsplittable_successor() {
    require_fonts!();
    let rows: String = (1..=28)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let keep_draw = r#"<subform name="KeepWrap" layout="tb" w="300pt">
<keep next="contentArea"/>
<draw name="KeepDraw" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>KEEP</text></value></draw>
</subform>"#;
    let boxed = r#"<subform name="Boxed" w="300pt">
<draw name="P1" y="0pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P1</text></value></draw>
<draw name="P2" y="40pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P2</text></value></draw>
<draw name="P3" y="80pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P3</text></value></draw>
</subform>"#;
    let body = format!("{rows}{keep_draw}{boxed}");
    let pages = texts_by_page(&flatten(&template(&body, "")));

    assert!(
        pages[0].iter().any(|t| t == "Row 28"),
        "sanity check: the 28 filler rows should still all fit on page 1, has {:?}",
        pages[0]
    );
    assert!(
        !pages[0].iter().any(|t| t == "KEEP"),
        "KEEP has a keep-next constraint into the unsplittable Boxed subform, \
         so it must move with it rather than stay on page 1; page 1 has {:?}",
        pages[0]
    );
    assert!(
        pages.len() > 1
            && pages[1].iter().any(|t| t == "KEEP")
            && pages[1].iter().any(|t| t == "P1")
            && pages[1].iter().any(|t| t == "P2")
            && pages[1].iter().any(|t| t == "P3"),
        "KEEP, P1, P2 and P3 should all land together on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}

/// As above, but the `<keep next="contentArea"/>` sits directly on the draw,
/// with no wrapper subform. This is how the UBS forms author it: a section
/// heading draw bound to the positioned question block that follows it
/// (AAJB_033_IT `Text_B2` -> `STP_Economica`). A `keep` on a leaf must be
/// recorded as a break constraint exactly like one on a subform, or the
/// heading is orphaned at the bottom of the earlier page.
#[test]
fn keep_next_on_a_draw_follows_into_an_unsplittable_successor() {
    require_fonts!();
    let rows: String = (1..=28)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let keep_draw = r#"<draw name="KeepDraw" w="300pt" h="20pt"><keep next="contentArea"/><font typeface="Helvetica" size="11pt"/><value><text>KEEP</text></value></draw>"#;
    let boxed = r#"<subform name="Boxed" w="300pt">
<draw name="P1" y="0pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P1</text></value></draw>
<draw name="P2" y="40pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P2</text></value></draw>
<draw name="P3" y="80pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P3</text></value></draw>
</subform>"#;
    let body = format!("{rows}{keep_draw}{boxed}");
    let pages = texts_by_page(&flatten(&template(&body, "")));

    assert!(
        pages[0].iter().any(|t| t == "Row 28"),
        "sanity check: the 28 filler rows should still all fit on page 1, has {:?}",
        pages[0]
    );
    assert!(
        !pages[0].iter().any(|t| t == "KEEP"),
        "KEEP carries its own keep-next constraint into the unsplittable Boxed \
         subform, so it must move with it rather than stay on page 1; page 1 has {:?}",
        pages[0]
    );
    assert!(
        pages.len() > 1
            && pages[1].iter().any(|t| t == "KEEP")
            && pages[1].iter().any(|t| t == "P1")
            && pages[1].iter().any(|t| t == "P2")
            && pages[1].iter().any(|t| t == "P3"),
        "KEEP, P1, P2 and P3 should all land together on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}
