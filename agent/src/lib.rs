//! Headless conversion-agent engine.
//!
//! This crate holds the framework-agnostic core that drives the form-conversion
//! engine via tools: the [`ConversionAgent`] (its tool catalog and executor),
//! the edit-history store ([`db`]) and the restore path that reads it back
//! ([`session`]), the per-profile reference store ([`references`]), the AEM HTTP
//! client ([`aem_client`]), the Playwright MCP browser client ([`browser`]), and
//! image encoding helpers ([`image_encode`]).
//!
//! It carries **no UI (Dioxus) and no LLM** dependency, so it can be embedded in
//! the desktop app *and* in a standalone MCP server. The LLM agent loop that
//! streams turns and drives these tools lives in the consumer (the app), as does
//! any UI state.

pub mod aem_client;
pub mod aem_translated_edit;
pub mod browser;
pub mod conversion;
pub mod db;
pub mod image_encode;
pub mod outputs;
pub mod references;
pub mod session;
pub mod structured_edit;
pub mod tree_edit;
pub mod u2s;

pub use conversion::{
    ANALYST_ADDENDUM, AUTHOR_ADDENDUM, ConversionAgent, MCP_ADDENDUM, NO_PACKAGE,
    REDACTO_ANALYST_ADDENDUM, REDACTO_AUTHOR_ADDENDUM, REDACTO_REVIEWER_ADDENDUM,
    REDACTO_SHARED_PREAMBLE, REDACTO_SYSTEM_PROMPT, REVIEWER_ADDENDUM, ReplyBlock, ReviewResult,
    SHARED_PREAMBLE, SYSTEM_PROMPT, ToolReply, ToolSpec, aem_connection_from_settings, all_tools,
    catalog, scope, target, tools_for, validate_package_bytes,
};
