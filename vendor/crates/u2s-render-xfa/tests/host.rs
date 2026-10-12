//! What a form's scripts say and what fails, through a live session: dialogs
//! are reported with the button they were answered with, a failing script is
//! named in the warning rather than dropped, and the host methods forms rely
//! on (`util.printd`, `xfa.host.resetData`) work. Against the generated
//! `dialogs.xfa.pdf` fixture.

#[allow(dead_code)]
mod support;

use u2s_render_xfa::states::StateSpec;
use u2s_render_xfa::{Limits, Renderer, Target, fonts};

fn renderer() -> Renderer {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    Renderer::new(Limits::default())
}

fn value(r: &Renderer, session: &str, revision: u64, field: &str) -> Option<String> {
    r.field(&Target::view(session, revision), field)
        .expect("field")
        .and_then(|c| c.value)
}

#[test]
fn an_alert_is_reported_with_its_answer_and_the_handler_carries_on() {
    let r = renderer();
    let (session, revision, _) = r.open(support::dialogs()).expect("open");
    let i = r.click(&session, revision, "form1.Body.Warn").expect("click");

    assert_eq!(i.messages.len(), 1, "{:?}", i.messages);
    assert_eq!(i.messages[0].source, "app.alert");
    assert_eq!(i.messages[0].text, "Bitte pruefen");
    assert_eq!(i.messages[0].answer, 1);
    assert_eq!(i.warning, None);
    assert_eq!(
        value(&r, &session, i.revision, "form1.Body.Out").as_deref(),
        Some("after")
    );
}

#[test]
fn a_failing_click_script_is_named_in_the_warning() {
    let r = renderer();
    let (session, revision, _) = r.open(support::dialogs()).expect("open");
    let i = r
        .click(&session, revision, "form1.Body.Broken")
        .expect("a failing script does not fail the click");
    let warning = i.warning.expect("a warning");
    assert!(
        warning.contains("click script on form1.Body.Broken: JavaScript error: ReferenceError"),
        "{warning}"
    );
}

#[test]
fn a_load_time_failure_is_on_the_documents_warning() {
    let r = renderer();
    let (_, _, info) = r.open(support::dialogs()).expect("open");
    let warning = info.warning.expect("a warning");
    assert!(
        warning.contains("initialize script on form1.Body.LoadFails"),
        "{warning}"
    );

    let info = r
        .info(&Target::doc(support::dialogs(), &StateSpec::default()))
        .expect("info");
    let warning = info.warning.expect("the stateless path warns too");
    assert!(
        warning.contains("initialize script on form1.Body.LoadFails"),
        "{warning}"
    );
}

#[test]
fn printd_fills_a_date_at_load() {
    let r = renderer();
    let (session, revision, _) = r.open(support::dialogs()).expect("open");
    assert_eq!(
        value(&r, &session, revision, "form1.Body.Today").as_deref(),
        Some("05.01.2024")
    );
}

#[test]
fn reset_data_restores_a_section_and_reports_the_change() {
    let r = renderer();
    let (session, revision, _) = r.open(support::dialogs()).expect("open");
    let set = r
        .set(&session, revision, "form1.Body.Section.Name", "typed")
        .expect("set");
    let i = r
        .click(&session, set.revision, "form1.Body.Clear")
        .expect("click");
    let change = i
        .side_effects
        .iter()
        .find(|c| c.field == "form1.Body.Section.Name")
        .expect("the reset is a side effect");
    assert_eq!((change.from.as_str(), change.to.as_str()), ("typed", "default"));
}

/// Today's local date as `dd.mm.yyyy`, the way the form's script formats it.
/// Read from the system clock through `date` rather than computed here, so
/// the test does not re-implement the formatting it checks.
fn today() -> String {
    let out = std::process::Command::new("date")
        .arg("+%d.%m.%Y")
        .output()
        .expect("run date");
    String::from_utf8(out.stdout).expect("utf-8").trim().to_string()
}

/// ACAV_001_DE stamps the current date into its footer (`CD`) with
/// `util.printd("dd.mm.yyyy", new Date())` when it loads; without `util` the
/// script threw and the footer stayed empty.
#[test]
fn the_acav_footer_shows_the_current_date() {
    let r = renderer();
    let form = u2s_test_assets::corpus_form("ACAV_001_DE.pdf");
    let text = r
        .page_text(&Target::doc(&form, &StateSpec::default()), 1, None, Some(100_000))
        .expect("page text");
    let today = today();
    assert!(text.text.contains(&today), "{today} not in {:?}", text.text);
}

/// ACAV_001_DE's address field tells the person it is mandatory when it is
/// left empty: the exit script raises an alert, which must be reported.
#[test]
fn leaving_the_acav_address_empty_raises_its_mandatory_alert() {
    let r = renderer();
    let form = u2s_test_assets::corpus_form("ACAV_001_DE.pdf");
    let (session, revision, _) = r.open(&form).expect("open");
    let i = r
        .set(&session, revision, "Form_61189.Page1.Form_Title.STP.S1.Address_1", "")
        .expect("set");
    assert!(
        i.messages
            .iter()
            .any(|m| m.source == "app.alert" && m.text.contains("Mandatory")),
        "{:?}",
        i.messages
    );
}

/// ACAV_001_DE's signature date shows and reads `date{DD.MM.YYYY}`: a date
/// typed that way is stored canonically, the page shows it in the form's
/// format, and the control says which format that is.
#[test]
fn the_acav_signature_date_is_typed_stored_and_shown_through_its_picture() {
    let r = renderer();
    let form = u2s_test_assets::corpus_form("ACAV_001_DE.pdf");
    let (session, revision, _) = r.open(&form).expect("open");
    let controls = r.controls(&Target::view(&session, revision)).expect("controls");
    let date = controls
        .controls
        .iter()
        .find(|c| c.field.ends_with(".SGN_Date1"))
        .expect("SGN_Date1 is listed");
    assert_eq!(date.format.as_deref(), Some("date{DD.MM.YYYY}"));
    assert_eq!(date.tooltip.as_deref(), Some("Date"));
    let field = date.field.clone();
    let page = date.positions.first().expect("drawn").page;

    let i = r.set(&session, revision, &field, "05.01.2024").expect("set");
    let after = r.field(&Target::view(&session, i.revision), &field).expect("field").expect("listed");
    assert_eq!(after.value.as_deref(), Some("2024-01-05"), "stored canonically");
    assert_eq!(after.shown.as_deref(), Some("05.01.2024"));
    let text = r
        .page_text(&Target::view(&session, i.revision), page, None, Some(100_000))
        .expect("page text");
    assert!(text.text.contains("05.01.2024"), "{:?}", text.text);
    assert!(!text.text.contains("2024-01-05"), "the page shows the format, not the raw value");
}

/// AAAA_019_DE's additional-address radios deselect on a second press,
/// through their own `mouseDown`/`mouseUp` scripts.
#[test]
fn a_second_press_on_an_aaaa_radio_deselects_its_group() {
    let r = renderer();
    let form = u2s_test_assets::corpus_form("AAAA_019_DE.pdf");
    let (session, revision, _) = r.open(&form).expect("open");
    let controls = r.controls(&Target::view(&session, revision)).expect("controls");
    let radio = controls
        .controls
        .iter()
        .find(|c| {
            c.field.contains("RB_Group_Additional_Address.")
                && c.access == u2s_xfa::flattened::FieldAccess::Open
        })
        .expect("an additional-address radio")
        .field
        .clone();
    let first = r.click(&session, revision, &radio).expect("press");
    let selected = r.field(&Target::view(&session, first.revision), &radio).expect("field").expect("listed");
    assert!(selected.value.as_deref().is_some_and(|v| !v.is_empty()), "{selected:?}");

    let second = r.click(&session, first.revision, &radio).expect("press again");
    assert!(second.values_changed);
    let cleared = r.field(&Target::view(&session, second.revision), &radio).expect("field").expect("listed");
    assert!(cleared.value.as_deref().unwrap_or("").is_empty(), "deselected: {cleared:?}");
}
