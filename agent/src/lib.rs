//! Headless conversion-agent engine.
//!
//! This crate holds the framework-agnostic core that drives a form conversion
//! through tools: the [`ConversionAgent`] (its tool catalog and executor over
//! the run's one output document), the edit-history store ([`db`]) and the
//! restore path that reads it back ([`session`]), the check rules and their
//! sandbox ([`rules`]), the connection of the per-profile reference store (`references-mcp`) to the history database ([`references`]), and
//! the vendored u2s tool servers run in-process ([`u2s`]: source-form reads and
//! renders, PDF viewing, and the Docker-hosted AEM and Redacto verifiers).
//!
//! It carries **no UI (Dioxus) and no LLM** dependency, so it can be embedded in
//! the desktop app *and* in a standalone MCP server. The LLM agent loop that
//! streams turns and drives these tools lives in the consumer, as does any UI
//! state.

pub mod conversion;
pub mod db;
mod mcp_reply;
mod output_target;
pub mod outputs;
pub mod package_checks;
pub mod profiles;
pub mod references;
pub mod rules;
pub mod session;
pub mod source;
pub mod u2s;

pub use conversion::{
    AUTHOR_ADDENDUM, Access, ConversionAgent, MCP_ADDENDUM, NO_PACKAGE, REDACTO_AUTHOR_ADDENDUM,
    REDACTO_MCP_ADDENDUM, REDACTO_REVIEWER_ADDENDUM,
    REDACTO_SHARED_PREAMBLE, REDACTO_SYSTEM_PROMPT, REVIEWER_ADDENDUM, ReplyBlock, ReviewResult,
    SHARED_PREAMBLE, SYSTEM_PROMPT, ToolReply, ToolSpec, access_of, all_tools, catalog, scope,
    target, tools_for, validate_package_bytes,
};
pub use output_target::OutputTarget;
