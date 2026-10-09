//! Repeatable sections through the renderer: buttons are listed and pressed,
//! instances appear and disappear with their own paths and positions, and a
//! one-shot render reaches the same state through ordered steps. Against the
//! generated `repeat.xfa.pdf` / `repeat_open.xfa.pdf` fixtures.

#[allow(dead_code)]
mod support;

use u2s_render_xfa::states::{
    ClickEffect, Control, ControlKind, Controls, SelectionSpec, StateSpec, Step,
};
use u2s_render_xfa::{
    FieldChange, ImageFormat, Interaction, InteractionKind, Limits, Renderer, Target, fonts,
};

fn require_fonts() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
}

fn renderer() -> Renderer {
    require_fonts();
    Renderer::new(Limits::default())
}

fn find<'a>(controls: &'a Controls, field: &str) -> Option<&'a Control> {
    controls.controls.iter().find(|c| c.field == field)
}

fn effect<'a>(i: &'a Interaction, field: &str) -> Option<&'a FieldChange> {
    i.side_effects.iter().find(|c| c.field == field)
}

const ADD: &str = "form1.Body.Add";

#[test]
fn the_add_button_is_listed_as_a_button_that_changes_instances() {
    let r = renderer();
    let c = r
        .controls(&Target::doc(support::repeat(), &StateSpec::default()))
        .expect("controls");

    let add = find(&c, ADD).expect("Add is listed");
    assert_eq!(add.kind, ControlKind::Button);
    assert_eq!(add.click, Some(ClickEffect::Instances));
    assert!(add.options.is_empty());
    assert_eq!(add.positions.len(), 1, "{:?}", add.positions);

    assert_eq!(
        find(&c, "form1.Body.Noop").and_then(|n| n.click),
        Some(ClickEffect::Script)
    );
    assert!(find(&c, "form1.Body.Row.Remove").is_some());
    assert!(
        find(&c, "form1.Body.Row[1].Remove").is_none(),
        "one row at open"
    );
    assert_eq!(
        c.space_size, 1,
        "buttons are not dimensions of the state space"
    );
}

#[test]
fn clicking_add_creates_a_second_row_with_its_own_paths_and_positions() {
    let r = renderer();
    let (session, revision, _) = r.open(support::repeat()).expect("open");
    let i = r.click(&session, revision, ADD).expect("click");

    assert_eq!(i.revision, 1);
    assert_eq!(i.kind, InteractionKind::Click);
    assert!(i.instances_changed);
    assert!(
        i.appeared.contains(&"form1.Body.Row[1].Amount".to_string()),
        "{:?}",
        i.appeared
    );
    assert!(i.appeared.contains(&"form1.Body.Row[1].Remove".to_string()));
    assert_eq!(
        effect(&i, "form1.Body.Count").map(|c| (c.from.as_str(), c.to.as_str())),
        Some(("1", "2"))
    );

    let c = r.controls(&Target::view(&session, 1)).expect("controls");
    let first = &find(&c, "form1.Body.Row.Remove").expect("row 0").positions;
    let second = &find(&c, "form1.Body.Row[1].Remove")
        .expect("row 1")
        .positions;
    assert_eq!((first.len(), second.len()), (1, 1));
    assert_eq!(first[0].page, second[0].page);
    assert!(
        second[0].y > first[0].y,
        "the new row is below: {first:?} {second:?}"
    );
}

#[test]
fn values_in_each_row_are_addressed_by_index_and_summed() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat()).expect("open");
    r.click(&session, 0, ADD).expect("add");
    let i = r
        .set(&session, 1, "form1.Body.Row[1].Amount", "5")
        .expect("set row 1");
    assert_eq!(
        effect(&i, "form1.Body.Total").map(|c| c.to.as_str()),
        Some("5")
    );

    let i = r
        .set(&session, 2, "form1.Body.Row.Amount", "2")
        .expect("set row 0");
    assert_eq!(
        effect(&i, "form1.Body.Total").map(|c| (c.from.as_str(), c.to.as_str())),
        Some(("5", "7"))
    );
    assert!(
        i.side_effects.iter().all(|c| c.field.contains('.')),
        "no bare-name alias is reported: {:?}",
        i.side_effects
    );
}

#[test]
fn clicking_a_rows_own_remove_button_takes_that_row_away() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat()).expect("open");
    r.click(&session, 0, ADD).expect("add");
    r.set(&session, 1, "form1.Body.Row[1].Amount", "5")
        .expect("set");
    r.set(&session, 2, "form1.Body.Row.Amount", "2")
        .expect("set");

    let i = r
        .click(&session, 3, "form1.Body.Row[1].Remove")
        .expect("remove");
    assert!(i.instances_changed);
    assert_eq!(
        i.disappeared,
        ["form1.Body.Row[1].Amount", "form1.Body.Row[1].Remove"]
    );
    assert_eq!(
        effect(&i, "form1.Body.Total").map(|c| c.to.as_str()),
        Some("2")
    );
    assert_eq!(
        effect(&i, "form1.Body.Count").map(|c| c.to.as_str()),
        Some("1")
    );
}

#[test]
fn the_occurrence_maximum_is_reported_not_silently_ignored() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat()).expect("open");
    assert!(r.click(&session, 0, ADD).expect("add 2").instances_changed);
    assert!(r.click(&session, 1, ADD).expect("add 3").instances_changed);
    let refused = r.click(&session, 2, ADD).expect("add 4");
    assert!(!refused.instances_changed);
    assert!(refused.appeared.is_empty());
    let why = refused.warning.expect("the refusal is reported");
    assert!(why.contains("max=\"3\""), "{why}");

    let c = r.controls(&Target::view(&session, 3)).expect("controls");
    assert!(find(&c, "form1.Body.Row[2].Remove").is_some());
    assert!(find(&c, "form1.Body.Row[3].Remove").is_none());
}

#[test]
fn reset_restores_the_single_opening_row() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat()).expect("open");
    r.click(&session, 0, ADD).expect("add");
    r.click(&session, 1, ADD).expect("add");
    let i = r.reset(&session, 2).expect("reset");
    assert_eq!(i.kind, InteractionKind::Reset);
    assert!(i.instances_changed);
    assert!(
        i.disappeared
            .contains(&"form1.Body.Row[2].Amount".to_string())
    );
    let c = r.controls(&Target::view(&session, 3)).expect("controls");
    assert!(find(&c, "form1.Body.Row[1].Remove").is_none());
}

#[test]
fn set_on_a_button_and_click_on_a_field_are_refused_with_a_pointer_to_the_other() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat()).expect("open");
    let e = r
        .set(&session, 0, ADD, "x")
        .expect_err("a button has no value");
    assert!(e.to_string().contains("xfa_click"), "{e}");
    let e = r
        .click(&session, 0, "form1.Body.Row.Amount")
        .expect_err("a text field is not pressed");
    assert!(e.to_string().contains("xfa_set"), "{e}");
    let e = r
        .click(&session, 0, "form1.Body.Nope")
        .expect_err("no such field");
    assert!(e.to_string().contains("xfa_controls"), "{e}");
    // None of the refusals moved the session on.
    assert_eq!(r.click(&session, 0, ADD).expect("still at 0").revision, 1);
}

#[test]
fn a_stale_revision_is_refused_for_click() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat()).expect("open");
    r.click(&session, 0, ADD).expect("first");
    let e = r.click(&session, 0, ADD).expect_err("stale");
    assert!(e.to_string().contains("revision 1"), "{e}");
}

#[test]
fn an_open_section_starts_empty_and_renumbers_on_remove() {
    let r = renderer();
    let (session, _, _) = r.open(support::repeat_open()).expect("open");
    let c = r.controls(&Target::view(&session, 0)).expect("controls");
    assert!(find(&c, ADD).is_some());
    assert!(
        find(&c, "form1.Body.Row.Remove").is_none(),
        "no row at open"
    );

    for rev in 0..4 {
        let i = r.click(&session, rev, ADD).expect("add");
        assert!(i.instances_changed && i.warning.is_none(), "{i:?}");
    }
    r.set(&session, 4, "form1.Body.Row[2].Amount", "9")
        .expect("set");

    // Removing the first row moves every later one down an index (XFA 3.3
    // §9): the last path disappears, and row 2's value is now row 1's.
    let i = r
        .click(&session, 5, "form1.Body.Row.Remove")
        .expect("remove first");
    assert_eq!(
        i.disappeared,
        ["form1.Body.Row[3].Amount", "form1.Body.Row[3].Remove"]
    );
    assert_eq!(
        effect(&i, "form1.Body.Row[1].Amount").map(|c| (c.from.as_str(), c.to.as_str())),
        Some(("0", "9"))
    );
}

#[test]
fn a_one_shot_render_reaches_the_same_rows_through_ordered_steps() {
    let r = renderer();
    let click = || Step::Click {
        field: ADD.to_string(),
    };
    let steps = StateSpec {
        steps: vec![
            click(),
            click(),
            Step::Set(SelectionSpec {
                field: "form1.Body.Row[2].Amount".into(),
                value: "9".into(),
            }),
        ],
    };
    let one = StateSpec {
        steps: vec![click()],
    };
    assert_ne!(
        steps.key(),
        one.key(),
        "two presses are a different state from one"
    );

    let c = r
        .controls(&Target::doc(support::repeat(), &steps))
        .expect("controls");
    assert!(find(&c, "form1.Body.Row[2].Remove").is_some(), "three rows");

    let default_page = r
        .render_page(
            &Target::doc(support::repeat(), &StateSpec::default()),
            1,
            Some(72.0),
            None,
            ImageFormat::Png,
        )
        .expect("render default")
        .0;
    let stepped_page = r
        .render_page(
            &Target::doc(support::repeat(), &steps),
            1,
            Some(72.0),
            None,
            ImageFormat::Png,
        )
        .expect("render stepped")
        .0;
    assert_ne!(
        default_page.data, stepped_page.data,
        "three rows draw differently from one"
    );

    let text = r
        .page_text(&Target::doc(support::repeat(), &steps), 1, None, None)
        .expect("text");
    assert!(text.text.contains('9'), "{}", text.text);
}
