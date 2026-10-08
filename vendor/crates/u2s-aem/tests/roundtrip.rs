//! The sample fixture parses, serializes back to the same value, and passes
//! semantic validation. This is the "every node variant and every enum
//! value" round trip the plan promises.

mod support;

use u2s_aem::AemForm;

#[test]
fn sample_form_parses_and_round_trips() {
    let json = support::sample_form_json();
    let form = AemForm::from_json(&json).expect("sample form parses");

    let reserialized = serde_json::to_value(&form).expect("form serializes");
    let reparsed: AemForm = serde_json::from_value(reserialized).expect("reserialized form parses");

    assert_eq!(form, reparsed);
}

#[test]
fn sample_form_validates() {
    let json = support::sample_form_json();
    let form = AemForm::from_json(&json).expect("sample form parses");
    form.validate().expect("sample form is semantically valid");
}
