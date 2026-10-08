//! `u2s-aem`: the generic AEM Adaptive Forms output model.
//!
//! One structure, [`model::AemForm`], is the entire intermediate output JSON
//! the Conversion Agent edits. [`schema()`] generates its JSON Schema
//! directly from that structure via `schemars`, so the schema cannot drift
//! from the type — there is nothing else to keep in sync.
//!
//! JCR content-XML emission — the FileVault package — is an encoder
//! concern, ported in a later phase, and is not part of this crate.

pub mod model;
pub mod schema;

pub use model::{AemForm, ValidForm, Violation};
pub use schema::schema;
