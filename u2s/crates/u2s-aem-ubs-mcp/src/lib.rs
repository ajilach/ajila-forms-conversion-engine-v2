//! The UBS AEM output format: the authoring model the Conversion Agent edits
//! (`aem::AemNodeTranslated`), its lowering and the UBS templates, the
//! FileVault package writer, the XSD, and the reader for real UBS packages.
//!
//! Moved from `ajilach/ajila-forms-conversion-engine`'s deterministic engine,
//! where these templates were refined against the deployed UBS corpus. The
//! generic `u2s-mapper-aem` stays free of UBS behaviour; converging this
//! writer onto it is future work, held to `tests/fixtures/golden`.

pub mod aem;
pub mod context;
pub mod document;
pub mod profiles;
pub mod template;
mod util;
pub mod value;
pub mod xsd;

pub use document::{UbsAemBuild, UbsAemDocument, decode, encode};

/// Errors from building a UBS AEM configuration or rendering its templates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The AEM configuration could not be built (for example a missing form variable).
    #[error("AEM config error: {0}")]
    AemConfig(String),
    /// A profile template string could not be rendered.
    #[error("Template error: {0}")]
    Template(String),
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
