//! The UBS Redacto output format: the document the Conversion Agent authors
//! (the body and its assets, plus each language's source), and the UBS page
//! furniture and metadata [`encode`] derives around it before the generic
//! `u2s-mapper-redacto` spells it as SQL.

pub mod document;

pub use document::{Error, RedactoSource, UbsRedactoDocument, decode, encode, to_redacto};

/// The JSON Schema of [`UbsRedactoDocument`], which `json_validate` checks a document
/// against.
pub fn document_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(UbsRedactoDocument)).expect("the schema serializes")
}
