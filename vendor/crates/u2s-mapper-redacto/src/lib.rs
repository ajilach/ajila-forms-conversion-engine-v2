//! The Redacto encoder: a mechanical mapper from a validated
//! [`u2s_redacto::model::ValidDocument`] to the platform's own
//! transactional `INSERT` script over the `app_redacto` schema.
//!
//! # Why this crate carries no business logic
//!
//! The reference implementation this workspace draws on
//! (`~/Documents/ajila-forms-conversion-engine`, `core/src/redacto/`) needed
//! real business logic here -- run-accumulation across structured blocks,
//! `GridLayout`/`Group{column_flow}` detection, adjacent-column-panel
//! fusion, footnote panel extraction, a UBS-specific footer-field
//! template -- because its input was a generic `StructuredNode[]` tree that
//! had to be *translated* into Redacto's own composition vocabulary. u2s's
//! Conversion Agent has no such gap: it edits `output_json` directly against
//! whatever schema is bound to a dataset, so it produces
//! `RedactoDocument`-shaped JSON directly, in the platform's own vocabulary
//! (assets, `assetContainer`, `styledPanel`) from the start.
//!
//! That leaves this crate exactly one job: given a document that is already
//! correct (`ValidDocument` is constructible only via
//! `RedactoDocument::validate`), spell it as rows and SQL. Every function
//! here answers "how is this already-decided document spelled", never
//! "should this asset exist" or "which panel style fits this section" --
//! see [`u2s_redacto`]'s own model for where those judgment calls now live
//! (the agent, checked by rules).
//!
//! # Modules
//!
//! - [`ids`] -- deterministic UUIDv5 minting, so `encode` is a pure function
//!   of its input rather than the reference converter's own
//!   `Uuid::new_v4()` + `now()`.
//! - [`rows`] -- the six typed row structs, the three encoder-only enum
//!   domains (`owner_type`/`ownership_type`/`object_type`), and [`rows::build`],
//!   the one function that turns a document into rows.
//! - [`config`] -- the `redacto-document/v2` configuration JSON, both
//!   directions (`build_configuration` for encode, parsed directly by
//!   [`decode`] for the inverse).
//! - [`sql`] -- `INSERT` emission and the platform's own quoting rule,
//!   ported from the reference converter's `redacto/sql.rs`.
//! - [`decode`] -- the exact inverse of [`encode`]: a real, delivered dump
//!   in, a `ValidDocument` out. See that module's own doc for the two named,
//!   accepted gaps (`master_language`'s absence from the row model, and a
//!   `-lang-`-qualified asset reference).

pub mod config;
pub mod decode;
pub mod ids;
pub mod rows;
pub mod sql;

use u2s_redacto::model::ValidDocument;

/// The encoded dump's bytes, ready to hand to a blob store -- the same
/// shape every other MCP server in this workspace already returns a large
/// payload as.
pub struct EncodedDump {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("could not build the row set: {0}")]
    Rows(#[from] rows::BuildError),
}

/// The one entry point: a validated document in, a transactional `INSERT`
/// script out. Everything this function calls is mechanical -- see the
/// module doc for what that means and why.
pub fn encode(doc: &ValidDocument) -> Result<EncodedDump, EncodeError> {
    let dump = rows::build(doc)?;
    let bytes = sql::to_sql(&dump).into_bytes();
    Ok(EncodedDump {
        bytes,
        media_type: "application/sql",
    })
}
