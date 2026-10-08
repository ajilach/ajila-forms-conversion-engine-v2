//! The sample fixture parses, serializes back to the same value, and passes
//! semantic validation.

mod support;

use u2s_redacto::RedactoDocument;

#[test]
fn sample_document_parses_and_round_trips() {
    let json = support::sample_document_json();
    let doc = RedactoDocument::from_json(&json).expect("sample document parses");

    let reserialized = serde_json::to_value(&doc).expect("document serializes");
    let reparsed: RedactoDocument =
        serde_json::from_value(reserialized).expect("reserialized document parses");

    assert_eq!(doc, reparsed);
}

#[test]
fn sample_document_validates() {
    let json = support::sample_document_json();
    let doc = RedactoDocument::from_json(&json).expect("sample document parses");
    doc.validate().expect("sample document is semantically valid");
}
