//! The UBS AEM output format: the authoring model the Conversion Agent edits
//! (`aem::AemNodeTranslated`), its lowering onto the generic AEM model with
//! the UBS components, fragments and scripts, the FileVault package writer,
//! the XSD, and the reader for real UBS packages.
//!
//! The generic `u2s-mapper-aem` encodes the lowered form and stays free of UBS
//! behaviour; what UBS adds is Rust here (`aem::lower`, `aem::scripts`), held
//! to the retired engine's output by `tests/fixtures/golden`.

pub mod aem;
pub mod context;
pub mod document;
pub mod profiles;
mod util;
pub mod value;
pub mod xsd;

pub use document::{UbsAemBuild, UbsAemDocument, decode, encode};

/// Errors from building a UBS AEM configuration or writing its package.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The AEM configuration could not be built (for example a missing form variable).
    #[error("AEM config error: {0}")]
    AemConfig(String),
    /// The profile could not be loaded.
    #[error("Profile error: {0}")]
    Profile(String),
    /// A document breaks an invariant the writer relies on.
    #[error("Invalid document: {0}")]
    InvalidDocument(String),
    /// A package could not be read.
    #[error("Package error: {0}")]
    Package(String),
}

/// The JSON Schema of [`UbsAemDocument`], which `json_validate` checks a document
/// against.
pub fn document_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(UbsAemDocument)).expect("the schema serializes")
}
