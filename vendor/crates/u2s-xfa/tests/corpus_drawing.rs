//! Shapes on real UBS forms: the rules and boxes their templates draw with
//! `<line>` and `<rectangle>` (XFA 3.3 Chapter 17) are on the page.

// Shared test helpers; this file uses only some of them.
#[allow(dead_code)]
mod support;

use support::dark_pixels_in;
use u2s_xfa::{Flattened, FlattenedNode, FlattenedNodeKind, XfaNode, fonts, prepare_default};

fn layout(form: &str) -> Flattened {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    let bytes = std::fs::read(u2s_test_assets::corpus_form(form)).expect("read");
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    let nodes = XfaNode::parse(&xfa).expect("parse");
    prepare_default(&nodes).expect("prepare").flattened
}

/// Every shape node named `name`: its SOM path is the name, or ends in
/// `.name` (master-page content carries the bare name).
fn shapes_named<'a>(flat: &'a Flattened, name: &str) -> Vec<&'a FlattenedNode> {
    let suffix = format!(".{name}");
    flat.iter_nodes()
        .filter(|n| matches!(n.kind, FlattenedNodeKind::Shape(_)))
        .filter(|n| {
            n.som_path()
                .is_some_and(|p| p.as_str() == name || p.as_str().ends_with(&suffix))
        })
        .collect()
}

fn f(n: u2s_xfa::Num) -> f64 {
    n.to_string().parse().expect("number")
}

/// ACAV_001_DE separates its signature section with `Linie_Section`, a
/// zero-height `<line>` 174mm wide and 0.375mm thick.
#[test]
fn the_acav_section_rule_is_drawn_across_its_width() {
    let flat = layout("ACAV_001_DE.pdf");
    let rules = shapes_named(&flat, "Linie_Section");
    assert!(!rules.is_empty(), "Linie_Section is laid out as a shape");
    let img = flat.render_to_image_buffer_plain(1.0).expect("render");
    for rule in rules {
        let (x, y, w) = (f(rule.x), f(rule.y), f(rule.width));
        let dark = dark_pixels_in(&img, x + 1.0, y - 2.0, w - 2.0, 4.0);
        assert!(
            dark as f64 >= 0.95 * (w - 2.0),
            "the rule at {x},{y} has {dark} dark pixels over {w}pt"
        );
    }
}

/// AAAB_019_DE draws its table rules as `<line>`s whose height Designer
/// wrote as `h="=0mm"`.
#[test]
fn the_aaab_table_rules_with_a_locked_height_are_drawn() {
    let flat = layout("AAAB_019_DE.pdf");
    let rules = shapes_named(&flat, "Line1");
    assert!(!rules.is_empty(), "Line1 is laid out as a shape");
    let img = flat.render_to_image_buffer_plain(1.0).expect("render");
    for rule in rules {
        assert_eq!(f(rule.height), 0.0, "=0mm is a zero height");
        let (x, y, w) = (f(rule.x), f(rule.y), f(rule.width));
        let dark = dark_pixels_in(&img, x + 1.0, y - 2.0, w - 2.0, 4.0);
        assert!(
            dark as f64 >= 0.95 * (w - 2.0),
            "the rule at {x},{y} has {dark} dark pixels over {w}pt"
        );
    }
}

/// AACJ_019_DE underlines its link `www.ubs.com/aei` with a
/// `<span style="text-decoration:underline">`; the run carries it to the
/// renderer, and the text around it does not.
#[test]
fn the_aacj_link_is_underlined_and_its_sentence_is_not() {
    let flat = layout("AACJ_019_DE.pdf");
    let runs: Vec<_> = flat
        .iter_nodes()
        .filter_map(|n| n.rich_text())
        .flat_map(|rt| rt.paragraphs.iter())
        .flat_map(|p| p.runs.iter())
        .collect();
    let link: Vec<_> = runs.iter().filter(|r| r.text.contains("www.ubs.com/aei")).collect();
    assert!(!link.is_empty(), "the link is laid out");
    assert!(link.iter().all(|r| r.underline && !r.line_through), "{link:?}");
    assert!(
        runs.iter()
            .any(|r| r.text.contains("entnehmen") && !r.underline),
        "the rest of the sentence is not underlined"
    );
}

/// The UBS add buttons are filled with a top-to-bottom green gradient and
/// rounded corners (hidden, but they still shape the button).
#[test]
fn the_aaaa_add_button_has_a_gradient_fill_and_round_corners() {
    let bytes = std::fs::read(u2s_test_assets::corpus_form("AAAA_019_DE.pdf")).expect("read");
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    let nodes = XfaNode::parse(&xfa).expect("parse");
    fn find_add_button(nodes: &[XfaNode]) -> Option<&XfaNode> {
        nodes.iter().find_map(|n| {
            let is_add = matches!(n.kind, u2s_xfa::XfaNodeKind::Field)
                && n.border
                    .as_ref()
                    .and_then(|b| b.fill.as_ref())
                    .is_some_and(|f| {
                        matches!(f.kind, u2s_xfa::xfa::FillKind::Linear(_)) && f.color == Some((154, 170, 113))
                    });
            if is_add { Some(n) } else { find_add_button(&n.children) }
        })
    }
    let button = find_add_button(&nodes).expect("an add button with a gradient fill");
    let border = button.border.as_ref().expect("border");
    let u2s_xfa::xfa::FillKind::Linear(linear) = border.fill.as_ref().expect("fill").kind else {
        panic!("a linear fill");
    };
    assert_eq!(linear.direction, u2s_xfa::xfa::GradientDirection::ToBottom);
    assert_eq!(linear.end, (120, 141, 65));
    let corner = border.get_corner(0).expect("corner");
    assert_eq!(corner.join, u2s_xfa::xfa::JoinStyle::Round);
}

/// AAGS_019_DE labels its button with a caption, "Populate data"; the label
/// is laid out as text on the button.
#[test]
fn the_aags_button_caption_is_on_the_page() {
    let flat = layout("AAGS_019_DE.pdf");
    let caption = flat.iter_nodes().find(|n| {
        matches!(&n.kind, FlattenedNodeKind::Text { content, .. } if content == "Populate data")
    });
    assert!(caption.is_some(), "the button caption is laid out");
}
