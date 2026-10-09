//! Field access through the renderer: every field is listed with its
//! effective access (XFA 3.3 §17, inherited per §2), a session reports which
//! fields the form's scripts locked or unlocked, and a field that is not
//! open is refused the way a person filling the form is refused. Against the
//! generated `access.xfa.pdf` fixture, which mirrors UBS's AAGS form.

#[allow(dead_code)]
mod support;

use u2s_render_xfa::states::{Control, Controls, SelectionSpec, StateSpec};
use u2s_render_xfa::{AccessChange, Limits, RenderError, Renderer, Target, fonts};
use u2s_xfa::flattened::FieldAccess;

const SHEET: &str = "form1.Body.Sheet";
const RB_1: &str = "form1.Body.RB_Sheet.RB_1";
const RB_2: &str = "form1.Body.RB_Sheet.RB_2";

fn renderer() -> Renderer {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    Renderer::new(Limits::default())
}

fn find<'a>(controls: &'a Controls, field: &str) -> &'a Control {
    controls
        .controls
        .iter()
        .find(|c| c.field == field)
        .unwrap_or_else(|| panic!("{field} is not listed"))
}

fn sheet_locked() -> AccessChange {
    AccessChange {
        field: SHEET.into(),
        from: FieldAccess::Open,
        to: FieldAccess::Protected,
    }
}

#[test]
fn every_field_is_listed_with_its_effective_access_at_open() {
    let r = renderer();
    let c = r
        .controls(&Target::doc(support::access(), &StateSpec::default()))
        .expect("controls");

    let expected = [
        (SHEET, FieldAccess::Open, None),
        ("form1.Body.Locked.Inner", FieldAccess::Protected, Some("form1.Body.Locked")),
        ("form1.Body.Fixed", FieldAccess::ReadOnly, None),
        ("form1.Body.InitLocked", FieldAccess::Protected, None),
        ("form1.Body.BadAccess", FieldAccess::Open, None),
        ("form1.Body.LockedButton", FieldAccess::Protected, None),
        (RB_1, FieldAccess::Open, None),
    ];
    for (field, access, from) in expected {
        let control = find(&c, field);
        assert_eq!(
            (control.access, control.access_from.as_deref()),
            (access, from),
            "{field}"
        );
    }
}

#[test]
fn a_session_reports_the_lock_a_script_sets_and_refuses_the_locked_field() {
    let r = renderer();
    let (session, revision, _) = r.open(support::access()).expect("open");

    let selected = r.set(&session, revision, RB_1, "1").expect("select RB_1");
    assert_eq!(selected.access_changes, vec![sheet_locked()]);
    assert!(
        selected
            .side_effects
            .iter()
            .any(|c| c.field == SHEET && c.to == "1"),
        "the script prefills the field it locks: {:?}",
        selected.side_effects
    );

    let field = r
        .field(&Target::view(&session, selected.revision), SHEET)
        .expect("field")
        .expect("Sheet exists");
    assert_eq!((field.value.as_deref(), field.access), (Some("1"), FieldAccess::Protected));

    match r.set(&session, selected.revision, SHEET, "7") {
        Err(RenderError::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("protected"), "{detail}");
        }
        other => panic!("setting a protected field must be an argument error, got {other:?}"),
    }

    let unlocked = r.set(&session, selected.revision, RB_2, "2").expect("select RB_2");
    assert_eq!(
        unlocked.access_changes,
        vec![AccessChange {
            field: SHEET.into(),
            from: FieldAccess::Protected,
            to: FieldAccess::Open,
        }]
    );
    r.set(&session, unlocked.revision, SHEET, "7")
        .expect("open again, so it can be set");
    r.close(&session).expect("close");
}

#[test]
fn a_locked_button_is_refused_and_nothing_runs() {
    let r = renderer();
    let (session, revision, _) = r.open(support::access()).expect("open");
    match r.click(&session, revision, "form1.Body.LockedButton") {
        Err(RenderError::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("protected"), "{detail}");
        }
        other => panic!("pressing a protected button must be refused, got {other:?}"),
    }
    // A refusal is not an interaction: the revision did not move.
    let field = r
        .field(&Target::view(&session, revision), SHEET)
        .expect("same revision still current")
        .expect("Sheet exists");
    assert_eq!(field.value.as_deref(), Some(""), "the click script did not run");
    r.close(&session).expect("close");
}

#[test]
fn a_reset_reports_the_locks_it_undoes() {
    let r = renderer();
    let (session, revision, _) = r.open(support::access()).expect("open");
    let selected = r.set(&session, revision, RB_1, "1").expect("select RB_1");
    let reset = r.reset(&session, selected.revision).expect("reset");
    assert_eq!(
        reset.access_changes,
        vec![AccessChange {
            field: SHEET.into(),
            from: FieldAccess::Protected,
            to: FieldAccess::Open,
        }]
    );
    r.close(&session).expect("close");
}

#[test]
fn a_one_shot_state_reads_the_same_access_as_a_session() {
    let r = renderer();
    let spec = StateSpec::selections(vec![SelectionSpec {
        field: RB_1.into(),
        value: "1".into(),
    }]);
    let field = r
        .field(&Target::doc(support::access(), &spec), SHEET)
        .expect("field")
        .expect("Sheet exists");
    assert_eq!(field.access, FieldAccess::Protected);
    assert!(
        r.field(&Target::doc(support::access(), &spec), "form1.Body.Nope")
            .expect("lookup")
            .is_none()
    );
}
