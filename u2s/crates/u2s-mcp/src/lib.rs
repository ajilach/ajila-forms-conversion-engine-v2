//! The extension mechanism (PLAN.md): everything pluggable — a renderer, a
//! data reader, an output-format encoder — is an MCP server, and this crate
//! is the client side of that convention. It is a **consumer** of a contract
//! the three real servers in this workspace already implement
//! (`u2s-xfa-mcp`, `u2s-render-pdf-mcp`, `u2s-render-xfa-mcp`), not the
//! author of a new one — see each module's docs for what already existed
//! versus what this crate adds.
//!
//! - [`manifest`] — parsing and validating a server's `u2s://manifest`.
//! - [`resolution`] — pure: for a `(role, FormatScope)`, which tool wins.
//! - [`registration`] — the `U2S_MCP_BIN_DIR` containment check.
//! - [`pool`] — the lazy, per-server-concurrency-limited client pool.
//! - [`conformance`] — the suite a third party runs before registering.
//! - [`session`] — the optional, additive contract for a server that lets a
//!   caller open a document once and address it by handle afterwards; see
//!   [`manifest::SessionSupport`] for how a server declares it.
//!
//! Deliberately not here: persistence. `u2s-mcp` has no opinion on where a
//! registered server's row lives; the app (`u2s-server`, wired through
//! `u2s-store`, landing with step 4 of `plan-the-agent-engine.md`) owns that.
//! A [`resolution::ToolCandidate`] identifies a server by a plain `String`
//! id for exactly this reason. [`session::SessionStore`] is a narrower,
//! deliberate exception: not persistence across restarts, but in-process
//! state a single server holds for the lifetime of one agent's session.

pub mod conformance;
pub mod manifest;
pub mod pool;
pub mod registration;
pub mod resolution;
pub mod session;

pub use manifest::{
    FormatId, FormatModule, ManifestError, SUPPORTED_CONTRACT_MAJOR, ServerIdentity,
    ServerManifest, SessionSupport, TestVector, ToolManifest, ToolRole,
};
pub use pool::{
    AdvertisedTool, CallToolResult, InlineImage, McpClient, McpClientPool, PoolError, ResultBlock,
    Transport, classify_block, inline_image,
};
pub use registration::{RegistrationError, validate_stdio_command, validate_stdio_env};
pub use resolution::{ResolutionOutcome, ResolutionReason, ToolCandidate, resolve};
pub use session::{SessionError, SessionStore, Target};
