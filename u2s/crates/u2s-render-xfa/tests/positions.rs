//! `xfa_controls`' `positions`: reading order, page bounds, and the round
//! trip into `render_region` — against the generated `choices.xfa.pdf`
//! fixture, so this needs no corpus, only the committed fallback font.

// `support` is shared test scaffolding with more fixtures than this file
// uses; only `choices()` applies here.
#[allow(dead_code)]
mod support;

use u2s_render_xfa::states::{Control, StateSpec, controls};
use u2s_render_xfa::{ImageFormat, Limits, Renderer, Target, fonts, rect_of};

fn require_fonts() {
    assert!(
        fonts::test_support::ensure_registered(),
        "no fonts registered — see crates/u2s-xfa/src/fonts.rs test_support::font_dir"
    );
}

fn find<'a>(controls: &'a [Control], field: &str) -> &'a Control {
    controls
        .iter()
        .find(|c| c.field == field)
        .unwrap_or_else(|| panic!("{field} must be listed"))
}

/// `RB_1`, `RB_2` and `CB_Ok` stack top to bottom in that order (`RB_Anrede`
/// is `layout="tb"`, and `CB_Ok` follows the exclGroup in `Body`, itself
/// `layout="tb"`) — the ground truth this asserts against the actual XDP in
/// `tests/support/mod.rs::choices`.
#[test]
fn visible_controls_report_one_position_each_in_reading_order() {
    let bytes = std::fs::read(support::choices()).expect("read fixture");
    let xfa = u2s_render_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("has xfa");
    let nodes = u2s_render_xfa::XfaNode::parse(&xfa).expect("parse");
    let c = controls(&nodes).expect("controls");

    let rb1 = find(&c.controls, "form1.Body.RB_Anrede.RB_1");
    let rb2 = find(&c.controls, "form1.Body.RB_Anrede.RB_2");
    let cb_ok = find(&c.controls, "form1.Body.CB_Ok");

    for ctrl in [rb1, rb2, cb_ok] {
        assert_eq!(
            ctrl.positions.len(),
            1,
            "{} should have exactly one position, got {:?}",
            ctrl.field,
            ctrl.positions
        );
        let p = ctrl.positions[0];
        assert_eq!(p.page, 1);
        assert!((p.height - 20.0).abs() < 0.5, "{}: {p:?}", ctrl.field);
        assert!(
            p.x >= 0.0 && p.y >= 0.0 && p.x + p.width <= 612.0 && p.y + p.height <= 792.0,
            "{} is outside its page: {p:?}",
            ctrl.field
        );
    }

    assert!(
        rb1.positions[0].y < rb2.positions[0].y,
        "RB_1 {:?} should be above RB_2 {:?}",
        rb1.positions[0],
        rb2.positions[0]
    );
    assert!(
        rb2.positions[0].y < cb_ok.positions[0].y,
        "RB_2 {:?} should be above CB_Ok {:?}",
        rb2.positions[0],
        cb_ok.positions[0]
    );
}

/// The claim that makes `positions` useful rather than merely present: the
/// rect it reports is the same convention `render_region`'s `rect_pt` takes,
/// so an agent can feed one straight into the other. A coordinate-convention
/// mismatch (a flipped axis, a page-relative-vs-column-relative Y) would
/// fail loudly here instead of silently cropping the wrong spot forever.
#[test]
fn a_reported_position_round_trips_through_render_region() {
    require_fonts();
    let form = support::choices();
    let r = Renderer::new(Limits::default());
    let target = Target::doc(&form, &StateSpec::default());

    let c = r.controls(&target).expect("controls");
    let cb_ok = find(&c.controls, "form1.Body.CB_Ok");
    let pos = cb_ok.positions[0];

    let region = r
        .render_region(&target, pos.page, rect_of(&pos), Some(72.0), ImageFormat::Png)
        .expect("render_region must accept a position straight from xfa_controls");

    assert!(
        (region.width_px as f32 - pos.width).abs() <= 2.0,
        "got {}px for a {}pt-wide box",
        region.width_px,
        pos.width
    );
    assert!(
        (region.height_px as f32 - pos.height).abs() <= 2.0,
        "got {}px for a {}pt-tall box",
        region.height_px,
        pos.height
    );

    let rect = rect_of(&pos);
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (pos.x, pos.y, pos.width, pos.height),
        "rect_of must not lose or reorder any field"
    );
}
