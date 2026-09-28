//! State exploration, on demand.
//!
//! The claim being tested is that a form's state space is *addressable* without
//! being enumerable: a caller lists the controls, picks a point, and gets it —
//! no matter how large the product is.

use u2s_xfa::states::{SelectionSpec, StateSpec, controls, materialize};
use u2s_xfa::{XfaNode, corpus, fonts};

/// Panics naming what's missing — the corpus and fallback fonts are both
/// committed, so absence means a broken checkout, not a reason to skip.
fn nodes_of(name: &str) -> Vec<XfaNode> {
    assert!(
        fonts::test_support::ensure_registered(),
        "no fonts registered — see crates/u2s-xfa/src/fonts.rs test_support::font_dir"
    );
    let p = corpus::test_support::form(name);
    let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract xfa")
        .expect("this fixture has an XFA packet");
    XfaNode::parse(&xfa).expect("parse xfa")
}

#[test]
fn controls_describe_the_space_without_exploring_it() {
    let nodes = nodes_of("AAAA_019_DE.pdf");
    let c = controls(&nodes).expect("controls");

    assert!(!c.controls.is_empty(), "this form has interactive controls");
    assert!(c.space_size >= 1);
    for control in &c.controls {
        assert!(!control.field.is_empty(), "every control needs a handle");
        assert!(
            !control.options.is_empty(),
            "control {} offers nothing to select",
            control.field
        );
    }

    // space_size multiplies *dimensions*, not controls: radio buttons sharing
    // an exclGroup are alternatives, so they contribute one factor between
    // them. Multiplying every control would misreport the space on any form
    // with radios.
    let mut dims: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for control in &c.controls {
        let dim = control
            .group
            .clone()
            .unwrap_or_else(|| control.field.clone());
        *dims.entry(dim).or_insert(0) += control.options.len().max(1) as u64;
    }
    let expected = dims.values().fold(1u64, |a, n| a.saturating_mul(*n));
    assert_eq!(c.space_size, expected);

    // And grouping must actually be happening where the form has radios.
    if c.controls.iter().any(|c| c.group.is_some()) {
        let naive: u64 = c
            .controls
            .iter()
            .fold(1u64, |a, c| a.saturating_mul(c.options.len().max(1) as u64));
        assert_ne!(
            c.space_size, naive,
            "a form with radio groups should not report the naive per-control product"
        );
    }
}

#[test]
fn the_default_state_materializes() {
    let nodes = nodes_of("AAAA_019_DE.pdf");
    let state = materialize(&nodes, &StateSpec::default()).expect("default");
    assert_eq!(state.key, "default");
    assert!(state.flattened.node_count() > 0);
}

/// The point of the design: reach a specific state directly, with no walk.
#[test]
fn an_explicit_selection_is_reachable_without_enumerating() {
    let nodes = nodes_of("AAAA_019_DE.pdf");
    let c = controls(&nodes).expect("controls");
    let Some(target) = c
        .controls
        .iter()
        .find(|c| c.options.len() > 1 || c.default.is_some())
    else {
        eprintln!("skipping: this form has no settable control");
        return;
    };

    let spec = StateSpec {
        selections: vec![SelectionSpec {
            field: target.field.clone(),
            value: target.options[0].value.clone(),
        }],
    };
    let state = materialize(&nodes, &spec).expect("materialize");
    assert_ne!(state.key, "default");
    assert!(state.flattened.node_count() > 0);
}

/// Selection order is not part of a state's identity — `[a,b]` and `[b,a]` are
/// the same point in the space, so they must key the same and render the same.
#[test]
fn selection_order_does_not_change_the_state() {
    let nodes = nodes_of("AAAA_019_DE.pdf");
    let c = controls(&nodes).expect("controls");
    if c.controls.len() < 2 {
        eprintln!("skipping: need two controls to permute");
        return;
    }
    let (a, b) = (&c.controls[0], &c.controls[1]);
    let sel = |x: &u2s_xfa::states::Control| SelectionSpec {
        field: x.field.clone(),
        value: x.options[0].value.clone(),
    };

    let forward = StateSpec {
        selections: vec![sel(a), sel(b)],
    };
    let reverse = StateSpec {
        selections: vec![sel(b), sel(a)],
    };
    assert_eq!(forward.key(), reverse.key(), "canonical keys must agree");

    let one = materialize(&nodes, &forward).expect("forward");
    let two = materialize(&nodes, &reverse).expect("reverse");
    let a_img = one
        .flattened
        .render_to_image_buffer_plain(1.0)
        .expect("render")
        .into_raw();
    let b_img = two
        .flattened
        .render_to_image_buffer_plain(1.0)
        .expect("render")
        .into_raw();
    assert_eq!(a_img, b_img, "the same state must render identically");
}

#[test]
fn an_unknown_control_says_where_the_real_names_are() {
    let nodes = nodes_of("AAAA_019_DE.pdf");
    let spec = StateSpec {
        selections: vec![SelectionSpec {
            field: "NoSuchField".into(),
            value: "x".into(),
        }],
    };
    let msg = match materialize(&nodes, &spec) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("an unknown control must be refused, not silently ignored"),
    };
    assert!(msg.contains("NoSuchField"), "{msg}");
    assert!(
        msg.contains("controls()"),
        "must point at the listing: {msg}"
    );
}

/// Every listed control reports whether the form's own scripts react to it,
/// so an agent triaging a large form knows where a change is likely to do
/// something -- without that ever having filtered the listing itself.
#[test]
fn every_control_reports_whether_it_affects_layout() {
    let nodes = nodes_of("AAAA_019_DE.pdf");
    let c = controls(&nodes).expect("controls");
    assert!(!c.controls.is_empty());

    // affects_layout must be a real predicate, not a constant: on a form
    // this large, at least one control's own scripts are read elsewhere, and
    // at least one plausibly is not, or the field is not actually predictive
    // of anything and should not exist.
    let any_true = c.controls.iter().any(|ctrl| ctrl.affects_layout);
    assert!(any_true, "no control was reported as affecting layout");
}
