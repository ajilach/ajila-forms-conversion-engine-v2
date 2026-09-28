//! The AEM verification runtime shared by every AEM-verifying MCP server:
//! reuse (or boot) a profile's persistent AEM + Chromium session, install a
//! FileVault package, walk a (possibly wizard-shaped) Adaptive Form,
//! optionally submit it, and capture whatever the submit produced.
//!
//! **Format-specific behaviour is a [`driver::FormDriver`], not a branch in
//! here.** How a form's URL query is built (a generic form needs none; a
//! UBS form needs `mandator`/`afAcceptLang`), how the wizard's terminal
//! panel is recognised (a visible submit button; or, for UBS, a summary
//! panel with no submit button at all), and how submit is actually
//! triggered (`guideBridge.submit()`; or UBS's own
//! `window.forms.ubs.navigation.submit(...)`) are all supplied by the
//! binary on top through that trait. This crate names AEM and FileVault
//! vocabulary, but never a specific customer's platform -- that split is
//! enforced by `tests/no_customer_terms.rs` -- the same "library has the
//! logic, binaries are thin" doctrine `u2s-mapper-aem` already gives the
//! two AEM *encoder* binaries (`u2s-aem-mcp`, `u2s-aem-ubs-mcp`), applied
//! here to the two AEM *verifier* binaries (`u2s-aem-verify-mcp`,
//! `u2s-aem-ubs-verify-mcp`).
//!
//! - [`aem_client`] -- the CRX Package Manager HTTP client.
//! - [`profile`] -- the one AEM profile a process serves, read from
//!   `U2S_AEM_VERIFY_*` environment.
//! - [`package_check`] -- offline FileVault package validation.
//! - [`session`] -- the persistent per-`session_id` AEM+Chromium sessions.
//! - [`driver`] -- the [`driver::FormDriver`] seam and the generic
//!   implementation every non-format-specific binary uses.
//! - [`flow`] -- the verification flow itself: install, render, walk,
//!   submit, capture.
//! - [`interactive`] -- the one-step-at-a-time control tools
//!   (`verify_open`/`verify_controls`/`verify_set`/...) built on the same
//!   `flow` primitives `verify_run` uses.
//! - [`warm`] -- the `warm` CLI subcommand's image-commit logic.
//! - [`server`] -- the rmcp `ServerHandler` glue shared by every binary,
//!   parameterised by a [`server::ServerConfig`].

pub mod aem_client;
pub mod driver;
pub mod flow;
pub mod interactive;
pub mod package_check;
pub mod profile;
pub mod server;
pub mod session;
pub mod specs_shared;
pub mod warm;

/// Prefix every log line this crate's own modules emit -- one shared
/// string rather than each binary's own name, since a log line from this
/// crate is about the shared runtime (session boot, wizard walk, warm)
/// regardless of which thin binary is hosting it right now.
pub const LOG_PREFIX: &str = "u2s-aem-verify-core";
