//! The working output document: the JSON tree the Conversion Agent edits,
//! never generated whole. A real form's output can run to hundreds of
//! kilobytes, so every operation here is windowed, bounded, and honest about
//! what it did not show — outline first, then a targeted `get`, then a
//! surgical `patch`, the same discipline `u2s-xfa-mcp`'s query tools already
//! established for reading raw XFA (see that crate's own module docs).
//!
//! Pure and format-agnostic on purpose: **no I/O, no schema knowledge**.
//! Validating a patched document against an output format's JSON Schema is
//! the `json_validate` tool in `u2s-agent`, which composes this crate's
//! [`Document`] with `u2s_engine::validate` — deliberately not built here,
//! so this crate never has to know what "valid" means for any given format
//! and the workspace keeps exactly one validator.
//!
//! - [`outline`] — one line per node: path, type, excerpt, flags.
//! - [`get`] — a depth-capped, character-windowed read of one subtree.
//! - [`search`] — pointers and counts, never bulk values.
//! - [`patch::apply`] — RFC 6902, atomic, revision-checked.
//!
//! JSON Pointer parsing, resolution and construction all go through
//! [`jsonptr`] and patch application through [`json_patch`] — both
//! re-exported so a caller building tool-call arguments never needs its own
//! direct dependency on either to name their types.

pub mod document;
pub mod error;
pub mod get;
pub mod outline;
pub mod patch;
pub mod search;

pub use document::{Document, Revision};
pub use error::JsonDocError;
pub use get::{GetResult, get};
pub use json_patch;
pub use jsonptr;
pub use outline::{Flag, NodeKind, Outline, OutlineEntry, outline};
pub use patch::{PatchError, apply as patch_apply};
pub use search::{MatchedIn, SearchMatch, SearchResult, search};
