//! The Redacto verification runtime, shared by every Redacto-verifying MCP
//! server (today, `u2s-redacto-ubs-verify-mcp` alone) -- built on
//! `u2s-verify-core`, the format-agnostic Docker/HTTP library.
//!
//! **Why this crate is much smaller than `u2s-aem-verify-core`.** A Redacto
//! document is text-only: the reference converter skips input fields with a
//! warning ("the Redacto target supports text-only documents"). There is no
//! form to walk, no control to set, no submit -- so there is no browser
//! driver, no interactive tool surface, and no per-profile `FormDriver`.
//! A dump is verified by importing it into a Redacto platform this crate
//! boots per `session_id`, and rendering it there.
//!
//! - [`dump_check`] -- offline structural validation (decode-or-error).
//! - [`session`] -- booting, reusing, and tearing down a session's platform.
//! - [`platform`] -- replacing a document's rows in the platform database.
//! - [`profile`] -- the tenant seam, [`profile::RenderProfile`].
//! - [`flow`] -- the whole `verify_run` flow: dump check, import, render.

pub mod dump_check;
pub mod flow;
pub mod platform;
pub mod profile;
pub mod session;

pub use profile::RenderProfile;
