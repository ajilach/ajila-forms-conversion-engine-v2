//! Headless conversion-agent engine.
//!
//! This crate holds the framework-agnostic core that drives a form conversion
//! through tools: the [`ConversionAgent`] (its tool catalog and executor over
//! the run's one output document), the edit-history store ([`db`]) and the
//! restore path that reads it back ([`session`]), the check rules and their
//! sandbox ([`rules`]), the connection of the per-profile reference store (`references-mcp`) to the history database ([`references`]), and
//! the vendored u2s tool servers run in-process ([`u2s`]: source-form reads and
//! renders, PDF viewing, and the Docker-hosted AEM verifier).
//!
//! It carries **no UI (Dioxus) and no LLM** dependency, so it can be embedded in
//! the desktop app and the CLI. The LLM agent loop that
//! streams turns and drives these tools lives in the consumer, as does any UI
//! state.

pub mod container_engine;
pub mod conversion;
pub mod coverage;
pub mod db;
mod mcp_reply;
pub mod outputs;
pub mod package_checks;
mod pdfium;
pub mod profiles;
pub mod references;
pub mod review;
pub mod rule_board;
pub mod rules;
pub mod session;
pub mod source;
pub mod u2s;

pub use conversion::{
    AUTHOR_ADDENDUM, Access, Caller, ConversionAgent, JUDGE_PREAMBLE, JudgeView, REVIEWER_ADDENDUM,
    ReplyBlock, ReviewResult, SHARED_PREAMBLE, SYSTEM_PROMPT, ToolReply, ToolSpec, access_of, all_tools,
    catalog, scope, tools_for,
};
pub use rule_board::{RuleKind, RuleState, RuleView};
