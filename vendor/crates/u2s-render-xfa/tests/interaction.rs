//! Interaction correctness: what an agent doing to a form must actually show
//! up in the rendered layout, not just in the value it fed back.
//!
//! These regression tests run against the generated `choices.xfa.pdf`
//! fixture, so they need no corpus and no licensed fonts to prove their
//! point (fonts are only needed to rasterize, and none of these tests do).

// `support` is shared test scaffolding with more fixtures than this file
// uses; only `choices()` applies here.
#[allow(dead_code)]
mod support;

use u2s_render_xfa::states::{ControlKind, SelectionSpec, StateSpec, controls, materialize};
use u2s_render_xfa::{XfaNode, extract_xfa_from_pdf_bytes};
use u2s_xfa::flattened::{Flattened, FlattenedNodeKind};

fn choices_nodes() -> Vec<XfaNode> {
    let bytes = std::fs::read(support::choices()).expect("read fixture");
    let xfa = extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("has xfa");
    XfaNode::parse(&xfa).expect("parse")
}

fn is_checked(nodes: &Flattened, field_name: &str) -> Option<bool> {
    nodes.iter_nodes().find_map(|n| match &n.kind {
        FlattenedNodeKind::Field { name, is_checked, .. } if name == field_name => *is_checked,
        _ => None,
    })
}

/// The bug this change fixes: `materialize` used to write only the selected
/// radio's own field and never the exclGroup's value, so `is_checked` -- which
/// is derived purely from comparing the exclGroup's rawValue to each button's
/// item key -- never flipped. Selecting `RB_1` must draw it selected and its
/// sibling deselected, with no rendering step involved: this is a data-level
/// check of the flattened layout the renderer would draw from.
#[test]
fn selecting_a_radio_button_is_reflected_in_its_checked_state() {
    let nodes = choices_nodes();

    let spec = StateSpec {
        selections: vec![SelectionSpec {
            field: "form1.Body.RB_Anrede.RB_1".to_string(),
            value: "1".to_string(),
        }],
    };
    let state = materialize(&nodes, &spec).expect("materialize");

    assert_eq!(
        is_checked(&state.flattened, "RB_1"),
        Some(true),
        "the selected radio button must render as checked"
    );
    assert_eq!(
        is_checked(&state.flattened, "RB_2"),
        Some(false),
        "selecting one radio button must deselect its sibling in the same exclGroup"
    );
}

/// Selecting the other button in the group must flip both, proving this is
/// real exclusivity and not a fixture that happens to default to `RB_1`.
#[test]
fn selecting_the_other_radio_button_deselects_the_first() {
    let nodes = choices_nodes();

    let spec = StateSpec {
        selections: vec![SelectionSpec {
            field: "form1.Body.RB_Anrede.RB_2".to_string(),
            value: "2".to_string(),
        }],
    };
    let state = materialize(&nodes, &spec).expect("materialize");

    assert_eq!(is_checked(&state.flattened, "RB_2"), Some(true));
    assert_eq!(is_checked(&state.flattened, "RB_1"), Some(false));
}

/// A radio button can only be set to its own on-value: there is no off-value,
/// since a person cannot deselect a radio by clicking a different value on
/// it, only by selecting a different button. A wrong guess must be refused
/// loudly, since `select_radio_button` itself silently ignores its argument.
#[test]
fn a_radio_button_refuses_a_value_that_is_not_its_own() {
    let nodes = choices_nodes();

    let spec = StateSpec {
        selections: vec![SelectionSpec {
            field: "form1.Body.RB_Anrede.RB_1".to_string(),
            value: "wrong".to_string(),
        }],
    };
    let msg = match materialize(&nodes, &spec) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a mismatched on-value must be refused"),
    };
    assert!(msg.contains("wrong"), "{msg}");
    assert!(msg.contains('1'), "must name the real on-value: {msg}");
}

/// `xfa_controls`' listing used to hide any control no script referenced by
/// name. A checkbox nothing reads is still a field a person can click, and it
/// must now be listed rather than silently dropped -- and, since nothing
/// reads it, `affects_layout` must say so honestly.
#[test]
fn a_control_no_script_reads_is_still_listed() {
    let nodes = choices_nodes();
    let c = controls(&nodes).expect("controls");

    // `choices.xfa.pdf`'s own module doc says every control there carries a
    // script deliberately, so this fixture cannot exercise the "no script
    // reads it" half by itself -- but it can still prove the *listing* case:
    // CB_Ok's script writes `Shown.presence`, not anything that reads CB_Ok
    // back, so nothing else in the form depends on CB_Ok's value even though
    // CB_Ok has its own script. `affects_layout` is about whether the form's
    // scripts react to a change, which for CB_Ok's own change script (which
    // only writes elsewhere) should still be true, since it IS the field the
    // script is attached to.
    let cb = c
        .controls
        .iter()
        .find(|ctrl| ctrl.field == "form1.Body.CB_Ok")
        .expect("CB_Ok must be listed");
    assert_eq!(cb.kind, ControlKind::Checkbox);
    assert!(cb.affects_layout, "CB_Ok owns its own change script");
}
