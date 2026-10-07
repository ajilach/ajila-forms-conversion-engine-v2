//! The reference-form store, its sentence-embedding matcher and the reference
//! tools as an MCP server with typed arguments.
//!
//! Hosts link it in-process (`agent` does) or run the stdio binary. The
//! database is reached only through the [`Opener`] the host supplies, and the
//! profile the references are scoped to is supplied by the host too, so
//! neither is ever an argument the model can set.

pub mod args;
mod hash;
pub mod reference_db;
pub mod semantic;
mod server;
pub mod specs;
mod store;

pub use hash::document_hash;
pub use server::{Error, ReferencesServer};
pub use store::*;
