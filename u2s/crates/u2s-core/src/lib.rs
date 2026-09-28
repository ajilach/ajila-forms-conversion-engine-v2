//! Domain identifier types and config-parsing primitives shared across the
//! web-app crates.
//!
//! `u2s-core` exists so `u2s-auth` (grants key on [`DatasetSlug`]) and
//! `u2s-store` (which stores it, `datasets.id` and `datasets.slug`) can
//! name the same types without either depending on the other, and so both
//! can share one `from_env` error-accumulation shape without duplicating
//! it. [`FormatScope`] extends that for the same reason: `u2s-store`'s pull
//! query and every MCP tool's manifest match a run's format against a scope,
//! and [`format::matches`] is the one function both are tested against.
//! Nothing here does I/O, nothing here names AEM or any output format — see
//! PLAN.md's genericity invariant, enforced mechanically by
//! `crates/u2s-server/tests/genericity.rs`.
//!
//! A `ports` module (async traits `u2s-agent` calls and `u2s-store`
//! implements — `ReferenceStore`, `RuleStore`, `TranscriptWriter`,
//! `ToolRegistry`) is planned here but deliberately not yet built: each
//! trait's shape is the DTOs its backing migration defines, and none of
//! those migrations exist yet (they land with steps 4/5/6/9 of
//! `plan-the-agent-engine.md`). Writing the trait ahead of its schema would
//! be exactly the "undocumented mechanism" CLAUDE.md warns against — add a
//! port when its first concrete consumer does.

pub mod agent_role;
pub mod base64;
pub mod config;
mod format;
pub mod ids;
mod label;
pub mod slug;
pub mod text;

pub use agent_role::{AgentRole, AgentRoleError, TurnPhase};
pub use config::{BuildProfile, ConfigError, ConfigProblem, EnvSource, ProcessEnv};
pub use format::{
    FormatId, FormatIdError, FormatScope, matches as format_matches, scopes_could_overlap,
};
pub use ids::{
    DatasetId, InputFileId, InputId, JobId, McpServerId, McpToolId, RuleId, RuleRevisionId, RunId,
};
pub use slug::{DatasetSlug, SlugError};
pub use text::{Windowed, window_chars};
