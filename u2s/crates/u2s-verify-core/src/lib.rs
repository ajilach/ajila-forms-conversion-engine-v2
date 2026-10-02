//! Format-agnostic verification runtime, shared by every format's verifier
//! server. Nothing in this crate names AEM, a FileVault package, or any
//! other output-format vocabulary -- that belongs to the format-specific
//! crate built on top (`u2s-aem-verify-core` today).
//!
//! - [`docker`] -- container and network lifecycle over `bollard`, plus
//!   `wait_for_http`, a generic readiness poll.
//! - [`browser`] -- a CDP driver over `chromiumoxide`, connecting to an
//!   already-running Chromium rather than launching one.
//! - [`session`] -- the per-`session_id` pool of booted sessions, how this
//!   process reaches their containers, and their startup and idle cleanup.
//! - [`http`] -- a one-shot reachability probe for a dependency this
//!   crate's callers do not themselves start or stop.
//! - [`types`] -- the report shape `verify_run` returns, mirroring
//!   `u2s_mcp::manifest::VerifyCapability::Run`'s documented contract
//!   field for field.

pub mod browser;
pub mod docker;
pub mod http;
pub mod session;
pub mod types;
