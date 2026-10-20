//! Pages: whole ones, with their master page on each.
//!
//! The layout is a stack of whole pages, so these assert the two things that
//! makes true -- every page is `page.height` tall, and the master page's
//! content is on every page, chosen by `pagePosition` -- plus the page-
//! dependent scripts that can only be answered once the page count is known.

mod support;

use support::dark_pixels;
use u2s_xfa::{Flattened, FlattenedNode, FlattenedNodeKind, XfaNode, fonts, prepare_default};

/// Fonts are committed (see `u2s_test_assets::font_dir`); a machine missing
/// them has a broken checkout, not a reason to skip layout assertions.
macro_rules! require_fonts {
    () => {
        fonts::register_dir_once(u2s_test_assets::font_dir(), None)
            .expect("register the test fonts");
    };
}

/// A4 in points, as `<medium stock="a4">` resolves to.
const PAGE_H: f64 = 841.8897637795276;
const PAGE_W: f64 = 595.2755905511812;
/// The contentArea of the templates below: y=30mm, h=255mm.
const CONTENT_TOP: f64 = 85.03937007874016;
/// The contentArea's left edge: x=17.5mm.
const CONTENT_LEFT: f64 = 49.60629921259843;
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
<subform name="form1" layout="tb"><pageSet name="MPs" relation="duplexPaginated">
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
            FlattenedNodeKind::Image(_) | FlattenedNodeKind::Shape(_) => "",
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

// ── shapes: <line>, <rectangle>, <arc> (XFA 3.3 Chapter 17) ─────────────────
//
// Each is a master-page draw at 17.5mm, 20mm (about 49.6pt, 56.7pt), below
// the master's own header and above the body's first row (at 85pt), so the
// ink measured is the shape's alone.

fn shape_draw(w: &str, h: &str, value: &str) -> String {
    format!(
        r#"<draw name="Shape" x="17.5mm" y="20mm" w="{w}" h="{h}"><value>{value}</value></draw>"#
    )
}

/// A zero-height `<line>` is a horizontal rule across its box; Designer
/// writes its height as `=0mm`.
#[test]
fn a_zero_height_line_is_drawn_as_a_rule() {
    require_fonts!();
    let master = shape_draw("200pt", "=0mm", r#"<line><edge thickness="1pt"/></line>"#);
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));

    let rule = dark_pixels(&flat, 50.0, 55.0, 199.0, 4.0);
    assert!(rule >= 199, "the rule has only {rule} dark pixels");
    assert_eq!(dark_pixels(&flat, 50.0, 60.0, 199.0, 6.0), 0, "nothing below it");
}

/// `slope="/"` runs from the bottom-left corner to the top-right one.
#[test]
fn a_rising_line_runs_bottom_left_to_top_right() {
    require_fonts!();
    let master = shape_draw("40pt", "40pt", r#"<line slope="/"><edge thickness="1pt"/></line>"#);
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));

    assert!(dark_pixels(&flat, 49.0, 92.0, 4.0, 4.0) > 0, "bottom-left");
    assert!(dark_pixels(&flat, 86.0, 56.0, 4.0, 4.0) > 0, "top-right");
    assert_eq!(dark_pixels(&flat, 49.0, 56.0, 4.0, 4.0), 0, "not top-left");
}

/// A `<rectangle>` with a visible edge is a box. Its corner has a radius
/// but the default `join="square"`, so the corner stays square: the radius
/// only rounds a corner whose join is round (XFA 3.3 "The corner element").
#[test]
fn a_rectangle_is_drawn_with_square_corners() {
    require_fonts!();
    let master = shape_draw(
        "100pt",
        "20pt",
        r#"<rectangle><corner thickness="1pt" radius="5mm"/><edge thickness="1pt"/></rectangle>"#,
    );
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));

    assert!(dark_pixels(&flat, 90.0, 55.0, 20.0, 4.0) > 0, "top edge");
    assert!(dark_pixels(&flat, 90.0, 74.0, 20.0, 4.0) > 0, "bottom edge");
    assert!(dark_pixels(&flat, 48.0, 62.0, 4.0, 8.0) > 0, "left edge");
    assert!(dark_pixels(&flat, 148.0, 62.0, 4.0, 8.0) > 0, "right edge");
    assert!(dark_pixels(&flat, 48.0, 55.0, 4.0, 4.0) > 0, "square top-left corner");
    assert_eq!(dark_pixels(&flat, 60.0, 61.0, 80.0, 11.0), 0, "an empty inside");
}

/// The UBS spacer: a rectangle whose edges and corners are all hidden draws
/// nothing at all.
#[test]
fn a_rectangle_with_hidden_edges_draws_nothing() {
    require_fonts!();
    let master = shape_draw(
        "100pt",
        "20pt",
        r#"<rectangle><corner radius="5.08mm" presence="hidden"/><edge presence="hidden"/></rectangle>"#,
    );
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));
    assert_eq!(dark_pixels(&flat, 45.0, 52.0, 110.0, 30.0), 0);
}

/// A filled circular `<arc>` is a disc centred in its box.
#[test]
fn a_filled_circular_arc_is_a_disc() {
    require_fonts!();
    let master = shape_draw(
        "80pt",
        "40pt",
        r#"<arc circular="1"><fill><color value="0,0,0"/></fill></arc>"#,
    );
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));

    assert!(dark_pixels(&flat, 87.0, 74.0, 4.0, 4.0) >= 16, "the centre is filled");
    assert_eq!(dark_pixels(&flat, 52.0, 74.0, 4.0, 4.0), 0, "the box beside the circle is not");
}

// ── text decoration (XFA 3.3 "Underline and Strikethrough") ──────────────────

/// Ink in the four pixel rows right below the lowest inked row of the
/// undecorated text, for a 12pt draw holding `value`: nothing for the
/// text itself, a line for an underline. `decorated` is the same draw with
/// the decoration; both are measured in the band the plain one leaves
/// empty.
fn ink_under_text(plain: (&str, &str), decorated: (&str, &str)) -> (usize, usize) {
    let render = |value: &str, font_attrs: &str| {
        let master = format!(
            r#"<draw name="Deco" x="17.5mm" y="20mm" w="200pt" h="20pt"><font typeface="Helvetica" size="12pt"{font_attrs}/><value>{value}</value></draw>"#
        );
        flatten(&template(&draw("Only", "One row", 24), &master))
            .render_to_image_buffer_plain(1.0)
            .expect("render")
    };
    let plain_img = render(plain.0, plain.1);
    let lowest = (57..80)
        .rev()
        .find(|&row| support::dark_pixels_in(&plain_img, 50.0, row as f64, 60.0, 1.0) > 0)
        .expect("the text is inked") as f64;
    let band = |img: &image::RgbaImage| support::dark_pixels_in(img, 50.0, lowest + 1.0, 60.0, 4.0);
    (band(&plain_img), band(&render(decorated.0, decorated.1)))
}

#[test]
fn a_font_underline_draws_a_line_under_the_text() {
    require_fonts!();
    let (plain, underlined) =
        ink_under_text(("<text>Hello world</text>", ""), ("<text>Hello world</text>", r#" underline="1""#));
    assert_eq!(plain, 0, "no ink under undecorated text");
    assert!(underlined >= 50, "the underline has only {underlined} dark pixels");
}

#[test]
fn a_css_underline_on_a_span_draws_a_line_under_it() {
    require_fonts!();
    let html = |style: &str| {
        format!(
            r#"<exData contentType="text/html"><body xmlns="http://www.w3.org/1999/xhtml"><p><span style="{style}">Hello world</span></p></body></exData>"#
        )
    };
    let (plain, ink) = ink_under_text((&html("letter-spacing:0in"), ""), (&html("text-decoration:underline"), ""));
    assert_eq!(plain, 0);
    assert!(ink >= 50, "the underline has only {ink} dark pixels");
}

// ── fills and corners (XFA 3.3 "The linear element", "The corner element") ──

/// A master draw at 17.5mm, 20mm (about 49.6pt, 56.7pt), 100x24pt, with
/// `border` and no text.
fn bordered_draw(border: &str) -> Flattened {
    let master = format!(
        r#"<draw name="Box" x="17.5mm" y="20mm" w="100pt" h="24pt"><border>{border}</border><value><text></text></value></draw>"#
    );
    flatten(&template(&draw("Only", "One row", 24), &master))
}

#[test]
fn a_linear_fill_runs_from_its_color_to_its_end_color() {
    require_fonts!();
    let flat = bordered_draw(
        r#"<edge presence="hidden"/><fill><color value="255,255,255"/><linear type="toBottom"><color value="0,0,0"/></linear></fill>"#,
    );
    assert_eq!(dark_pixels(&flat, 52.0, 57.0, 90.0, 3.0), 0, "light at the top");
    assert!(dark_pixels(&flat, 52.0, 77.0, 90.0, 3.0) >= 200, "dark at the bottom");
}

#[test]
fn a_round_corner_rounds_the_fill_and_the_edges() {
    require_fonts!();
    let flat = bordered_draw(
        r#"<corner join="round" radius="8pt" thickness="1pt"/><edge thickness="1pt"/><fill><color value="0,0,0"/></fill>"#,
    );
    // The box spans about 49.6..149.6 x 56.7..80.7.
    assert!(dark_pixels(&flat, 95.0, 66.0, 10.0, 5.0) >= 50, "a filled middle");
    // The extreme corner pixels; the arc itself passes a pixel or two inside.
    assert_eq!(dark_pixels(&flat, 50.0, 57.0, 1.0, 1.0), 0, "a cut-out top-left corner");
    assert_eq!(dark_pixels(&flat, 148.0, 79.0, 1.0, 1.0), 0, "a cut-out bottom-right corner");

    let outline = bordered_draw(r#"<corner join="round" radius="8pt" thickness="1pt"/><edge thickness="1pt"/>"#);
    assert!(dark_pixels(&outline, 95.0, 56.0, 10.0, 2.0) > 0, "the top edge");
    assert_eq!(dark_pixels(&outline, 50.0, 57.0, 1.0, 1.0), 0, "no square corner");
    assert!(
        dark_pixels(&outline, 50.0, 57.0, 8.0, 8.0) > 0,
        "the quarter circle bends through the corner region"
    );
}

/// A `<radial>` fill runs from the fill's color at the centre to its own
/// color at the edges (XFA 3.3 "The radial element").
#[test]
fn a_radial_fill_runs_from_the_centre_to_the_edges() {
    require_fonts!();
    let flat = bordered_draw(
        r#"<edge presence="hidden"/><fill><color value="255,255,255"/><radial type="toEdge"><color value="0,0,0"/></radial></fill>"#,
    );
    // The box spans about 49.6..149.6 x 56.7..80.7; its centre is light, its
    // left end dark.
    assert_eq!(dark_pixels(&flat, 97.0, 67.0, 5.0, 3.0), 0, "light at the centre");
    assert!(dark_pixels(&flat, 51.0, 67.0, 3.0, 3.0) >= 6, "dark at the edge");
}

/// A `<pattern>` fill hatches its color over the background at an
/// application-defined interval (XFA 3.3 "The pattern element").
#[test]
fn a_pattern_fill_hatches_the_background() {
    require_fonts!();
    let flat = bordered_draw(
        r#"<edge presence="hidden"/><fill><color value="255,255,255"/><pattern type="horizontal"><color value="0,0,0"/></pattern></fill>"#,
    );
    // Horizontal strokes one pixel wide every six: two in twelve rows, each
    // 60 pixels long.
    let dark = dark_pixels(&flat, 60.0, 60.0, 60.0, 12.0);
    assert!((100..=140).contains(&dark), "{dark} dark pixels in a 60x12 region");
}

/// A `<stipple>` fill scatters its color over `rate` percent of the area
/// (XFA 3.3 "The stipple element").
#[test]
fn a_stipple_fill_covers_its_rate_of_the_area() {
    require_fonts!();
    let flat = bordered_draw(
        r#"<edge presence="hidden"/><fill><color value="255,255,255"/><stipple rate="50"><color value="0,0,0"/></stipple></fill>"#,
    );
    let dark = dark_pixels(&flat, 60.0, 60.0, 60.0, 10.0);
    assert!((200..=400).contains(&dark), "{dark} of 600 pixels are dark at a 50% rate");
}

/// A dashed edge leaves gaps; a solid one does not (XFA 3.3 "The edge
/// element").
#[test]
fn a_dashed_edge_leaves_gaps() {
    require_fonts!();
    let solid = bordered_draw(r#"<edge thickness="2pt"/>"#);
    let dashed = bordered_draw(r#"<edge thickness="2pt" stroke="dashed"/>"#);
    let along_top = |flat: &Flattened| dark_pixels(flat, 60.0, 56.0, 60.0, 3.0);
    let (s, d) = (along_top(&solid), along_top(&dashed));
    assert!(s >= 110, "the solid edge inks its whole length, {s}");
    assert!(d >= 50 && d <= s - 30, "the dashed edge inks about two thirds of it, {d} of {s}");
}

/// An inverted round corner bows into the box: the vertex region stays
/// blank and the arc passes a radius away from it (XFA 3.3 "The corner
/// element", `inverted`).
#[test]
fn an_inverted_corner_is_concave() {
    require_fonts!();
    let flat = bordered_draw(
        r#"<corner join="round" radius="8pt" thickness="1pt" inverted="1"/><edge thickness="1pt"/>"#,
    );
    assert_eq!(dark_pixels(&flat, 50.0, 57.0, 3.0, 3.0), 0, "the vertex region is blank");
    assert!(dark_pixels(&flat, 53.5, 60.5, 5.0, 5.0) > 0, "the arc bows through the box's inside");
    assert!(dark_pixels(&flat, 95.0, 56.0, 10.0, 2.0) > 0, "the top edge");
}

/// An image with `aspect="width"` is scaled so its width is the box's,
/// whatever that does to its height (XFA 3.3 "The aspect property").
#[test]
fn an_image_with_aspect_width_fills_the_box_width() {
    require_fonts!();
    let master = format!(
        r#"<draw name="Logo" x="17.5mm" y="20mm" w="40pt" h="10pt"><ui><imageEdit/></ui><value><image contentType="image/png" aspect="width">{}</image></value></draw>"#,
        black_png_base64()
    );
    let flat = flatten(&template(&draw("Only", "One row", 24), &master));
    // A 2x2 image scaled to 40pt wide is 40pt tall: inked across the whole
    // box width, and below the 10pt box.
    assert!(dark_pixels(&flat, 85.0, 58.0, 4.0, 4.0) > 0, "the box's right end");
    assert!(dark_pixels(&flat, 60.0, 75.0, 4.0, 4.0) > 0, "past the box's height");
    let fit = format!(
        r#"<draw name="Logo" x="17.5mm" y="20mm" w="40pt" h="10pt"><ui><imageEdit/></ui><value><image contentType="image/png">{}</image></value></draw>"#,
        black_png_base64()
    );
    let flat = flatten(&template(&draw("Only", "One row", 24), &fit));
    assert_eq!(dark_pixels(&flat, 85.0, 58.0, 4.0, 4.0), 0, "fit keeps a 10pt square");
}

/// A glyph the text's font lacks is taken from a face that has it (XFA 3.3
/// §28): the corpus fonts have no ballot box, DejaVu Sans does, so the
/// symbol is drawn as a hollow square rather than skipped or boxed.
#[test]
fn a_missing_glyph_is_drawn_from_the_symbol_fallback() {
    require_fonts!();
    let master = r#"<draw name="Box" x="17.5mm" y="20mm" w="60pt" h="24pt"><font typeface="Frutiger 45 Light" size="20pt"/><value><text>☐</text></value></draw>"#;
    let flat = flatten(&template(&draw("Only", "One row", 24), master));
    // The glyph's box: about 14pt wide from x=49.6, within the 20pt line.
    let outline = dark_pixels(&flat, 49.0, 57.0, 18.0, 22.0);
    assert!(outline > 30, "the ballot box is drawn, {outline} dark pixels");
    let centre = dark_pixels(&flat, 55.0, 64.0, 4.0, 4.0);
    assert_eq!(centre, 0, "and it is hollow");
}

/// `underline="2"` draws a double underline where the face puts it (XFA
/// 3.3 "The underline property").
#[test]
fn a_double_underline_is_two_lines() {
    require_fonts!();
    let single = r#"<draw name="U" x="17.5mm" y="20mm" w="100pt" h="24pt"><font typeface="Frutiger 45 Light" size="12pt" underline="1"/><value><text>Underlined words</text></value></draw>"#;
    let double = single.replace(r#"underline="1""#, r#"underline="2""#);
    let one = flatten(&template(&draw("Only", "One row", 24), single));
    let two = flatten(&template(&draw("Only", "One row", 24), &double));
    // Below the baseline, the double underline inks more rows.
    let band = |flat: &Flattened| dark_pixels(flat, 50.0, 66.0, 80.0, 8.0);
    let (a, b) = (band(&one), band(&two));
    assert!(a > 0, "a single underline, {a}");
    assert!(b > a + 40, "a double underline inks a second line, {b} against {a}");
}

// ── captions (XFA 3.3 "The caption element") ─────────────────────────────────

/// A caption with a 60pt reserve on the left is drawn there, and the
/// field's value is drawn after it rather than across it.
#[test]
fn a_left_caption_takes_its_reserve_and_the_value_follows() {
    require_fonts!();
    let field = |caption: &str| {
        format!(
            r#"<field name="F" x="17.5mm" y="20mm" w="200pt" h="20pt" access="readOnly"><font typeface="Helvetica" size="11pt"/><ui><textEdit/></ui>{caption}<value><text>Ada</text></value></field>"#
        )
    };
    let plain = flatten(&template(&draw("Only", "One row", 24), &field("")));
    let captioned = flatten(&template(
        &draw("Only", "One row", 24),
        &field(r#"<caption reserve="60pt"><value><text>Name</text></value></caption>"#),
    ));
    let texts: Vec<String> = captioned
        .iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Text { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(texts.contains(&"Name".to_string()), "{texts:?}");
    // The box starts at 49.6pt. Without a caption "Ada" starts there; with
    // one, "Name" does, and "Ada" starts 60pt later.
    assert!(dark_pixels(&plain, 50.0, 57.0, 20.0, 18.0) > 0, "the value at the left edge");
    assert!(dark_pixels(&captioned, 50.0, 57.0, 20.0, 18.0) > 0, "the caption at the left edge");
    assert_eq!(dark_pixels(&plain, 110.0, 57.0, 25.0, 18.0), 0, "nothing past the value");
    assert!(dark_pixels(&captioned, 110.0, 57.0, 25.0, 18.0) > 0, "the value after the reserve");
}

/// A FormCalc page number on the master page, the way ACAV_001_DE writes
/// it: a field directly in the pageArea computes `$ = $.parent.index + 1`,
/// and its parent is the pageArea, whose index is the page being laid out.
#[test]
fn a_formcalc_page_number_on_the_master_page_counts_the_pages() {
    require_fonts!();
    let rows: String = (1..=75).map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24)).collect();
    let page_no = r#"<field name="PageNo" x="17.5mm" y="290mm" w="60pt" h="10pt"><font typeface="Helvetica" size="8pt"/><ui><textEdit/></ui><calculate><script>$ = $.parent.index + 1</script></calculate></field>"#;
    let flat = flatten(&template(&rows, page_no));
    assert_eq!(flat.page.page_count, 3);
    let pages = texts_by_page(&flat);
    for (i, page) in pages.iter().enumerate() {
        // FormCalc's `+` is numeric; the field shows the bare number.
        let label = (i + 1).to_string();
        assert!(page.contains(&label), "page {} lacks {label}: {page:?}", i + 1);
    }
}

/// A barcode field draws bars, not the digits of its value.
#[test]
fn a_barcode_field_is_drawn_as_bars() {
    require_fonts!();
    // At y=20mm the box clears the master's own header draw.
    let master = r#"<field name="BC" x="17.5mm" y="20mm" w="200pt" h="24pt"><font typeface="Helvetica" size="6pt"/><ui><barcode type="code2Of5Interleaved" textLocation="none"/></ui><value><text>0101665</text></value></field>"#;
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

/// `textLocation` defaults to `below` (XFA 3.3 "The barcode element"): the
/// value is printed under the bars in the field's font, so the foot of the
/// box carries sparse glyph ink where the bars do not reach.
#[test]
fn a_barcode_prints_its_value_below_the_bars_by_default() {
    require_fonts!();
    let master = r#"<field name="BC" x="17.5mm" y="20mm" w="200pt" h="24pt"><font typeface="Helvetica" size="6pt"/><ui><barcode type="code2Of5Interleaved"/></ui><value><text>0101665</text></value></field>"#;
    let flat = flatten(&template(&draw("Only", "One row", 24), master));

    let top = dark_pixels(&flat, 49.0, 58.0, 202.0, 6.0);
    let foot = dark_pixels(&flat, 49.0, 74.0, 202.0, 6.0);
    assert!(top > 0 && foot > 0, "bars at the top ({top}) and a legend at the foot ({foot})");
    assert!(
        (foot as f64) < top as f64 * 0.6,
        "the legend ({foot}) is sparser than the bars ({top})"
    );

    // With `checksum="auto"` the bars carry a check digit the legend does
    // not show unless `printCheckDigit="1"`; either way the bars still fit.
    let checked = master.replace(r#"type="code2Of5Interleaved""#, r#"type="code2Of5Interleaved" checksum="auto" dataLength="30""#);
    let flat = flatten(&template(&draw("Only", "One row", 24), &checked));
    assert!(dark_pixels(&flat, 49.0, 58.0, 202.0, 6.0) > 0);
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
/// in its place reads as a different control. The button is its `size`
/// (10pt by default, XFA 3.3 "The checkButton element"), centered in the
/// field's 20pt box.
#[test]
fn a_round_check_button_is_drawn_as_a_circle() {
    require_fonts!();
    let body = r#"<exclGroup name="G" w="400pt" h="30pt"><field name="RB_1" x="0pt" y="0pt" w="20pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>1</text></items></field></exclGroup>"#;
    let flat = flatten(&template(body, ""));

    // The widget sits at the top left of the content area.
    let (bx, by) = (49.6, CONTENT_TOP);
    // A 10pt circle centered in 20pt leaves a 5pt frame blank all round.
    let frame = dark_pixels(&flat, bx, by, 20.0, 4.0) + dark_pixels(&flat, bx, by, 4.0, 20.0);
    assert_eq!(frame, 0, "outside the 10pt button nothing is drawn");
    // But there is a ring: the middle of its top edge is inked.
    let top_edge = dark_pixels(&flat, bx + 8.0, by + 4.5, 4.0, 2.0);
    assert!(top_edge > 0, "the radio has no ring");
}

/// `<checkButton size="16pt" mark="check">` draws a 16pt square outline
/// and, once on, a check mark inside it (XFA 3.3 "The mark property").
#[test]
fn a_check_button_takes_its_size_and_mark_from_the_template() {
    require_fonts!();
    let body = r#"<field name="CB" x="0pt" y="0pt" w="20pt" h="20pt"><ui><checkButton size="16pt" mark="check"/></ui><value><integer>1</integer></value><items><integer>1</integer><integer>0</integer></items></field>"#;
    let flat = flatten(&template(body, ""));
    let (bx, by) = (49.6, CONTENT_TOP);
    // A 16pt box centered in 20pt: its top edge is 2pt down, inked across.
    assert!(dark_pixels(&flat, bx + 4.0, by + 1.5, 12.0, 2.0) > 0, "the outline's top edge");
    assert_eq!(dark_pixels(&flat, bx, by, 20.0, 1.0), 0, "nothing above the 16pt box");
    // The check mark's long stroke rises through the box's right half.
    assert!(dark_pixels(&flat, bx + 12.0, by + 6.0, 4.0, 4.0) > 0, "the check mark");
    // A cross would ink the top left quarter; a check leaves it blank.
    assert_eq!(dark_pixels(&flat, bx + 5.0, by + 5.0, 3.0, 3.0), 0, "no stroke in the top left");
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
    // sentence onto four. The block's last line carries no line gap (AXTE,
    // XFA 3.3 "Special Lines"): 20% of the 8pt font, 1.6pt, comes off.
    assert!(
        wrapped_h >= 3.0 * 9.0 - 1.6 - 0.1,
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
<subform name="form1" layout="tb"><pageSet name="MPs" relation="duplexPaginated">
<pageSet name="Set1" relation="duplexPaginated">
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
<pageSet name="Set2" relation="duplexPaginated">
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

/// The root subform's children flow in its own layout (XFA 3.3 §8): a
/// second content subform starts a new page only when it asks for one with
/// a `breakBefore` to a page, not merely by being a separate subform. Both
/// land on one page here, which is then the only page, so the designated
/// last page is used for it (§8 "Termination processing").
#[test]
fn a_second_content_subform_without_a_page_break_shares_the_page() {
    require_fonts!();
    let flat = flatten(&two_content_subforms(""));
    assert_eq!(flat.page.page_count, 1, "two short subforms fit one page");

    let pages = texts_by_page(&flat);
    assert!(
        pages[0].iter().any(|t| t == "Page A content") && pages[0].iter().any(|t| t == "Page B content"),
        "both subforms' content is on page 1, got {:?}",
        pages[0]
    );
    assert!(
        pages[0].iter().any(|t| t == "LAST"),
        "the only page is the last page and uses the pagePosition=\"last\" area, got {:?}",
        pages[0]
    );
}

/// With `<breakBefore targetType="pageArea"/>` the second content subform
/// starts a page of its own (XFA 3.3 §7 "Break Conditions"), and the page
/// set's designated last page is the last page overall, whichever subform
/// put content on it.
#[test]
fn a_content_subform_with_a_page_break_starts_a_new_page() {
    require_fonts!();
    let flat = flatten(&two_content_subforms(r#"<breakBefore targetType="pageArea"/>"#));
    assert_eq!(flat.page.page_count, 2, "the break makes the second subform a page");

    let pages = texts_by_page(&flat);
    assert!(
        pages[0].iter().any(|t| t == "ORDINARY") && pages[0].iter().any(|t| t == "Page A content"),
        "page 1 should use the ordinary area, got {:?}",
        pages[0]
    );
    assert!(
        pages[1].iter().any(|t| t == "LAST") && pages[1].iter().any(|t| t == "Page B content"),
        "page 2, the last page overall, should use the pagePosition=\"last\" area, got {:?}",
        pages[1]
    );
}

/// Two content subforms under one page set; `second_break` goes into the
/// second one.
fn two_content_subforms(second_break: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs" relation="duplexPaginated">
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
<subform name="Page_66439" layout="tb" w="540pt">{second_break}{}</subform>
</subform></template></xdp:xdp>"#,
        draw("A", "Page A content", 20),
        draw("B", "Page B content", 20)
    )
}

/// A subform's `<border>` is drawn as a node of its own, under its
/// children, at the subform's true (grown) bounds: a growable subform (no
/// explicit `h`, common for a body wrapper like AAOV's `BankingRelationship`)
/// has height 0 before its children are laid out, and its bottom edge must
/// still land under its last child, not at its top.
#[test]
fn a_growable_subforms_border_is_drawn_at_its_true_bottom() {
    require_fonts!();
    let inner = format!("{}{}", draw("L1", "Line1", 20), draw("L2", "Line2", 20));
    let body = format!(
        r#"<subform name="BorderTest" layout="tb" w="400pt">
<border><edge presence="hidden"/><edge presence="hidden"/><edge thickness="1pt"/><edge presence="hidden"/></border>
{inner}
</subform>"#
    );
    let flat = flatten(&template(&body, ""));

    let (border, bounds) = flat
        .iter_nodes()
        .find_map(|n| match &n.kind {
            FlattenedNodeKind::Shape(u2s_xfa::xfa::shapes::Shape::Rectangle(r)) => Some((r.clone(), n.bounds())),
            _ => None,
        })
        .expect("the subform's border is a rectangle node");
    assert_eq!(border.edges.len(), 4);
    assert_eq!(border.edges[2].presence, "visible");
    let h: f64 = bounds.height.to_string().parse().unwrap();
    assert!(
        (h - 40.0).abs() < 0.5,
        "the border is the subform's true (grown) height, 40pt for two 20pt rows; got {h}pt"
    );
    // Drawn under the children: the rectangle precedes L1 in the output.
    let order: Vec<&str> = flat
        .iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Shape(_) => Some("border"),
            FlattenedNodeKind::Text { source_name, .. } if source_name.as_deref() == Some("L1") => Some("L1"),
            _ => None,
        })
        .collect();
    assert_eq!(order, vec!["border", "L1"]);
    // And it is inked along the subform's bottom edge.
    assert!(dark_pixels(&flat, CONTENT_LEFT + 100.0, CONTENT_TOP + 38.0, 100.0, 5.0) > 50);
}

/// A subform's `<border><fill>` is its background colour (AAOV's "Form
/// configurator" pink panel: every edge hidden, but a visible fill), drawn
/// as the subform's own node over its true (grown) bounds.
#[test]
fn a_growable_subforms_fill_is_drawn_over_its_true_bounds() {
    require_fonts!();
    let inner = format!("{}{}", draw("L1", "Line1", 20), draw("L2", "Line2", 20));
    let body = format!(
        r#"<subform name="FillTest" layout="tb" w="400pt">
<border><edge presence="hidden"/><fill><color value="255,244,244"/></fill></border>
{inner}
</subform>"#
    );
    let flat = flatten(&template(&body, ""));

    let (fill, bounds) = flat
        .iter_nodes()
        .find_map(|n| match &n.kind {
            FlattenedNodeKind::Shape(u2s_xfa::xfa::shapes::Shape::Rectangle(r)) => Some((r.fill.clone(), n.bounds())),
            _ => None,
        })
        .expect("the subform's fill is a rectangle node");
    assert_eq!(fill.and_then(|f| f.color), Some((255, 244, 244)));
    let h: f64 = bounds.height.to_string().parse().unwrap();
    assert!((h - 40.0).abs() < 0.5, "the fill covers the subform's true height; got {h}pt");
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

/// A positioned subform in a flowed container moves whole to the next page
/// rather than splitting between its children (Acrobat's reading of XFA
/// 3.3 §8, `acrobat_compat` item 6): 29 rows of 24pt leave 26.83pt of page
/// 1, enough for P1 alone, but the block goes to page 2 entire.
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
        pages.len() > 1 && ["P1", "P2", "P3"].iter().all(|p| pages[1].iter().any(|t| t == p)),
        "P1, P2 and P3 should all land together on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}

/// A flowed (`layout="tb"`) subform with `<keep intact="none"/>` splits
/// between its children (XFA 3.3 §8 "Splitting a Container Having Child
/// Containers"): P1 fits the 26.83pt left on page 1, P2 and P3 follow on
/// page 2.
#[test]
fn a_flowed_subform_splits_between_its_children() {
    require_fonts!();
    let rows: String = (1..=29)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let flowed = r#"<subform name="Flowed" layout="tb" w="300pt"><keep intact="none"/>
<draw name="P1" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P1</text></value></draw>
<draw name="P2" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P2</text></value></draw>
<draw name="P3" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P3</text></value></draw>
</subform>"#;
    let body = format!("{rows}{flowed}");
    let pages = texts_by_page(&flatten(&template(&body, "")));

    assert!(
        pages[0].iter().any(|t| t == "Row 29") && pages[0].iter().any(|t| t == "P1"),
        "P1 fits the 26.83pt left on page 1, has {:?}",
        pages[0]
    );
    assert!(
        pages.len() > 1
            && pages[1].iter().any(|t| t == "P2")
            && pages[1].iter().any(|t| t == "P3")
            && !pages[1].iter().any(|t| t == "P1"),
        "P2 and P3 land on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}

/// With `<keep intact="contentArea"/>` the same block is unsplittable and
/// moves whole to page 2, whatever the distance.
#[test]
fn a_keep_intact_subform_moves_whole_to_the_next_page() {
    require_fonts!();
    let rows: String = (1..=29)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let boxed = r#"<subform name="Boxed" w="300pt"><keep intact="contentArea"/>
<draw name="P1" y="0pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P1</text></value></draw>
<draw name="P2" y="40pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P2</text></value></draw>
<draw name="P3" y="80pt" w="300pt" h="20pt"><font typeface="Helvetica" size="11pt"/><value><text>P3</text></value></draw>
</subform>"#;
    let body = format!("{rows}{boxed}");
    let pages = texts_by_page(&flatten(&template(&body, "")));

    assert!(
        !pages[0].iter().any(|t| t == "P1" || t == "P2" || t == "P3"),
        "the block moves whole to the next page, page 1 has {:?}",
        pages[0]
    );
    assert!(
        pages.len() > 1 && ["P1", "P2", "P3"].iter().all(|p| pages[1].iter().any(|t| t == p)),
        "P1, P2 and P3 land together on page 2, pages have {:?}",
        pages.iter().map(|p| p.len()).collect::<Vec<_>>()
    );
}

/// A subform with `<keep next="contentArea"/>` immediately followed by an
/// unsplittable (`keep intact`) subform: moving the break out of the
/// unsplittable subform lands it between the two, which the keep-next
/// forbids in turn (XFA 3.3 §8 "Adhesion"), so the keep-next subform moves
/// with it rather than being orphaned on the earlier page.
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
    let boxed = r#"<subform name="Boxed" w="300pt"><keep intact="contentArea"/>
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
    let boxed = r#"<subform name="Boxed" w="300pt"><keep intact="contentArea"/>
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

/// A check box field has a natural width of its own: its button's `size`
/// plus its left and right margins (XFA 3.3 §2 "Natural size of widgets").
/// ACYI_001_EN's confirm box declares `minW="5mm"` and `leftInset="5mm"`,
/// which left it no content width at all, and so no box, before this.
#[test]
fn a_check_box_is_as_wide_as_its_button_and_margins() {
    require_fonts!();
    let flat = flatten(&template(
        r#"<field name="CB" minH="4mm" minW="5mm" x="0mm"><ui><checkButton size="3.8806mm"/></ui><margin leftInset="5mm" rightInset="0mm"/><value><integer>0</integer></value><items><integer>1</integer><integer>0</integer></items></field>"#,
        "",
    ));
    let cb = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Field { name, .. } if name == "CB"))
        .expect("the check box is laid out");
    // Its state comes from its own value against its "on" item (the first),
    // with no exclusion group to derive it from: 0 is off here.
    assert!(
        matches!(&cb.kind, FlattenedNodeKind::Field { is_checked: Some(false), .. }),
        "an unchecked box on its own knows it is off: {:?}",
        cb.kind
    );
    let width: f64 = cb.width.to_string().parse().unwrap();
    let x: f64 = cb.x.to_string().parse().unwrap();
    let mm = 72.0 / 25.4;
    assert!((width - 3.8806 * mm).abs() < 0.05, "the box is the button's size, got {width}");
    assert!(
        (x - (CONTENT_LEFT + 5.0 * mm)).abs() < 0.05,
        "the box sits past the left margin, got {x}"
    );
}

/// The same box with its "on" value draws checked.
#[test]
fn a_check_box_holding_its_on_value_is_checked() {
    require_fonts!();
    let flat = flatten(&template(
        r#"<field name="CB" w="5mm" h="5mm"><ui><checkButton/></ui><value><integer>1</integer></value><items><integer>1</integer><integer>0</integer></items></field>"#,
        "",
    ));
    let cb = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Field { name, .. } if name == "CB"))
        .expect("the check box is laid out");
    assert!(
        matches!(&cb.kind, FlattenedNodeKind::Field { is_checked: Some(true), .. }),
        "{:?}",
        cb.kind
    );
}

/// The contents of an `invisible` container take their space but are not
/// drawn (XFA 3.3 "The presence property"): ACYI_001_EN keeps a hidden
/// option group `invisible`, and its radio buttons must not show as a box.
#[test]
fn the_contents_of_an_invisible_container_take_space_but_are_not_drawn() {
    require_fonts!();
    let flat = flatten(&template(
        &format!(
            r#"<exclGroup name="Opt" presence="invisible" w="100pt" h="30pt"><field name="RB" w="20pt" h="20pt"><ui><checkButton shape="round"/></ui><items><integer>1</integer></items></field></exclGroup>{}"#,
            draw("After", "AFTER", 24)
        ),
        "",
    ));
    assert!(
        !flat
            .iter_nodes()
            .any(|n| matches!(&n.kind, FlattenedNodeKind::Field { name, .. } if name == "RB")),
        "the invisible group's button is not drawn"
    );
    let after = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content.trim() == "AFTER"))
        .expect("the draw after the group");
    let y: f64 = after.y.to_string().parse().unwrap();
    assert!(
        (y - (CONTENT_TOP + 30.0)).abs() < 0.05,
        "the group still takes its 30pt, got {y}"
    );
}

/// A cover-page form (ACYI_001_EN): two content subforms side by side under
/// the root, of which the scripts show one at a time. The hidden one takes
/// no page (XFA 3.3 "The presence property": hidden means no layout).
fn cover_template(cover_presence: &str, main_presence: &str) -> String {
    let main: String = (1..=75)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    template(&main, "").replace(
        r#"<subform name="Body" layout="tb" w="540pt">"#,
        &format!(
            r#"<subform name="Cover" layout="tb" w="540pt" presence="{cover_presence}">{}</subform><subform name="Body" layout="tb" w="540pt" presence="{main_presence}">"#,
            draw("Title", "COVER PAGE", 24)
        ),
    )
}

#[test]
fn a_hidden_content_subform_takes_no_page() {
    require_fonts!();

    let cover_only = flatten(&cover_template("visible", "hidden"));
    assert_eq!(
        cover_only.page.page_count, 1,
        "the hidden form proper takes no page"
    );
    let pages = texts_by_page(&cover_only);
    assert!(pages[0].iter().any(|t| t == "COVER PAGE"));
    assert!(!pages[0].iter().any(|t| t.starts_with("Row ")));

    let main_only = flatten(&cover_template("hidden", "visible"));
    assert_eq!(
        main_only.page.page_count, 3,
        "the hidden cover takes no page either"
    );
    let pages = texts_by_page(&main_only);
    assert!(!pages.iter().flatten().any(|t| t == "COVER PAGE"));
    assert!(pages[0].iter().any(|t| t == "Row 1"));
}

// ── pagination strategies (XFA 3.3 §7 "Break Conditions", §8 "Pagination
// Strategies") ────────────────────────────────────────────────────────────

/// Where a leaf with this text landed: its page (1-based) and page-local box.
fn locate_text(flat: &Flattened, text: &str) -> (u32, u2s_xfa::flattened::Bounds) {
    let node = flat
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content.trim() == text))
        .unwrap_or_else(|| panic!("no leaf with text {text:?}"));
    flat.locate(node).unwrap_or_else(|| panic!("{text:?} is on no page"))
}

/// XFA 3.3 §9 Example 9.22: `startNew="1"` asks for a new page even when
/// the current one is still empty, so a subform at the very start of the
/// form leaves the first page blank (page background only).
#[test]
fn a_start_new_break_at_the_very_start_leaves_the_first_page_blank() {
    require_fonts!();
    let body = format!(
        r#"<subform name="X" layout="tb" w="400pt"><breakBefore targetType="pageArea" startNew="1"/>{}</subform>"#,
        draw("A", "FIRST RECORD", 20)
    );
    let flat = flatten(&template(&body, ""));
    assert_eq!(flat.page.page_count, 2, "a blank first page, then the content");
    let pages = texts_by_page(&flat);
    assert_eq!(
        pages[0],
        vec!["HEADER EVERY PAGE", "FOOTER DEFAULT"],
        "page 1 carries page background only"
    );
    assert!(pages[1].iter().any(|t| t == "FIRST RECORD"));
    assert!(
        pages[1].iter().any(|t| t == "LAST PAGE ONLY"),
        "page 2 is the last page and uses the designated last page, got {:?}",
        pages[1]
    );
}

/// XFA 3.3 §8 Example 8.27: in a duplex page set, a break to an odd page
/// from an odd page inserts a blank filler page for the back of the sheet.
#[test]
fn a_break_to_an_odd_page_from_an_odd_page_inserts_a_blank_page() {
    require_fonts!();
    let body = format!(
        r#"<subform name="X" layout="tb" w="400pt">{}<breakAfter targetType="pageOdd"/></subform>{}"#,
        draw("A", "ON PAGE ONE", 20),
        draw("Z", "ON PAGE THREE", 20)
    );
    let flat = flatten(&template(&body, ""));
    assert_eq!(flat.page.page_count, 3, "page 1, a blank page 2, page 3");
    assert!(flat.page.pages[1].blank, "page 2 is the filler");
    let pages = texts_by_page(&flat);
    assert!(pages[0].iter().any(|t| t == "ON PAGE ONE"));
    assert_eq!(pages[1], vec!["HEADER EVERY PAGE", "FOOTER DEFAULT"], "the filler holds page background only");
    assert!(pages[2].iter().any(|t| t == "ON PAGE THREE"));
}

/// A `breakBefore targetType="contentArea"` moves to the next content area
/// of the current page before starting a new page (XFA 3.3 §7 "Break
/// Conditions"): the second column of a two-column page.
#[test]
fn a_content_area_break_moves_to_the_next_content_area_of_the_page() {
    require_fonts!();
    let xdp = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs" relation="simplexPaginated">
<pageArea name="MP">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea name="Left" x="17.5mm" y="30mm" w="90mm" h="255mm"/>
  <contentArea name="Right" x="110mm" y="30mm" w="90mm" h="255mm"/>
</pageArea>
</pageSet>
<subform name="Body" layout="tb" w="250pt">{}<subform name="Second" layout="tb" w="250pt"><breakBefore targetType="contentArea"/>{}</subform></subform>
</subform></template></xdp:xdp>"#,
        draw("A", "LEFT COLUMN", 20),
        draw("B", "RIGHT COLUMN", 20)
    );
    let flat = flatten(&xdp);
    assert_eq!(flat.page.page_count, 1, "both columns are on one page");
    let (page_a, a) = locate_text(&flat, "LEFT COLUMN");
    let (page_b, b) = locate_text(&flat, "RIGHT COLUMN");
    assert_eq!((page_a, page_b), (1, 1));
    let mm = |v: f64| v * 72.0 / 25.4;
    let f = |n: u2s_xfa::Num| n.to_string().parse::<f64>().unwrap();
    assert!((f(a.x) - mm(17.5)).abs() < 0.1, "A is in the left column at {}", a.x);
    assert!((f(b.x) - mm(110.0)).abs() < 0.1, "B is in the right column at {}", b.x);
    assert!((f(a.y) - f(b.y)).abs() < 0.1, "both columns start at the content area's top");
}

/// XFA 3.3 §8 "Termination processing": when the last page's content does
/// not fit the designated last page, that page is kept and a blank
/// instance of the designated last page follows it.
#[test]
fn a_last_page_whose_content_does_not_fit_the_designated_last_page_gets_a_blank_one() {
    require_fonts!();
    let masters = |body: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs" relation="simplexPaginated">
<pageArea name="MP">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
</pageArea>
<pageArea name="MP_Last" pagePosition="last">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="100mm"/>
  <draw name="LastNote" x="17.5mm" y="274mm" w="400pt" h="12pt"><font typeface="Helvetica" size="8pt"/><value><text>LAST PAGE ONLY</text></value></draw>
</pageArea>
</pageSet>
<subform name="Body" layout="tb" w="540pt">{body}</subform></subform></template></xdp:xdp>"#
        )
    };
    let short: String = (1..=5).map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24)).collect();
    let flat = flatten(&masters(&short));
    assert_eq!(flat.page.page_count, 1, "120pt of rows fit the 100mm last page");
    assert!(texts_by_page(&flat)[0].iter().any(|t| t == "LAST PAGE ONLY"));

    let long: String = (1..=20).map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24)).collect();
    let flat = flatten(&masters(&long));
    assert_eq!(flat.page.page_count, 2, "480pt of rows do not fit the 100mm last page");
    assert!(flat.page.pages[1].blank);
    let pages = texts_by_page(&flat);
    assert!(pages[0].iter().any(|t| t == "Row 20"), "page 1 keeps all its rows");
    assert_eq!(pages[1], vec!["LAST PAGE ONLY"], "page 2 is a blank designated last page");
}

/// Every page has its own size: a designated last page on A5 after A4
/// pages stacks at its own height, and nodes locate on it accordingly.
#[test]
fn pages_of_different_sizes_stack_at_their_own_heights() {
    require_fonts!();
    let rows: String = (1..=35).map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24)).collect();
    let xdp = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet name="MPs" relation="simplexPaginated">
<pageArea name="MP">
  <medium long="297mm" short="210mm" stock="a4"/>
  <contentArea x="17.5mm" y="30mm" w="192mm" h="255mm"/>
</pageArea>
<pageArea name="MP_Last" pagePosition="last">
  <medium long="210mm" short="148mm" stock="a5"/>
  <contentArea x="10mm" y="20mm" w="128mm" h="150mm"/>
</pageArea>
</pageSet>
<subform name="Body" layout="tb" w="540pt">{rows}</subform></subform></template></xdp:xdp>"#
    );
    let flat = flatten(&xdp);
    assert_eq!(flat.page.page_count, 2);
    let a4 = 297.0 * 72.0 / 25.4;
    let a5 = 210.0 * 72.0 / 25.4;
    let f = |n: u2s_xfa::Num| n.to_string().parse::<f64>().unwrap();
    assert!((f(flat.page.pages[0].height) - a4).abs() < 0.01);
    assert!((f(flat.page.pages[1].height) - a5).abs() < 0.01, "page 2 is A5 tall");
    assert!((f(flat.page.pages[1].top) - a4).abs() < 0.01, "page 2 starts where page 1 ends");
    assert!((f(flat.page.column_height()) - (a4 + a5)).abs() < 0.01);
    let (page, bounds) = locate_text(&flat, "Row 31");
    assert_eq!(page, 2);
    assert!((f(bounds.y) - 20.0 * 72.0 / 25.4).abs() < 0.01, "Row 31 starts at the A5 content area's top");
    assert!((f(bounds.x) - 10.0 * 72.0 / 25.4).abs() < 0.01, "and at its left edge");
}
