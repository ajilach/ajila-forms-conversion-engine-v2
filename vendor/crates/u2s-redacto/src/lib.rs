//! `u2s-redacto`: the generic Ajila Redacto document output model.
//!
//! One structure, [`model::RedactoDocument`], is the entire intermediate
//! output JSON the Conversion Agent edits. [`schema()`] generates its JSON
//! Schema directly from that structure via `schemars`, so the schema cannot
//! drift from the type — there is nothing else to keep in sync.
//!
//! The delivered artefact is *not* this JSON: Redacto stores a document as
//! rows in the `app_redacto` Postgres schema, wrapped in one transactional
//! `INSERT` script. Lowering this JSON into that script is an encoder
//! concern (`u2s-mapper-redacto`), not part of this crate.

pub mod model;
pub mod schema;

pub use model::{Asset, Component, DocumentMetadata, RedactoDocument, ValidDocument, Violation};
pub use schema::schema;
