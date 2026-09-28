//! The Redacto verification runtime, shared by every Redacto-verifying MCP
//! server (today, `u2s-redacto-ubs-verify-mcp` alone) -- built on
//! `u2s-verify-core`, the format-agnostic Docker/HTTP library.
//!
//! **Why this crate is much smaller than `u2s-aem-verify-core`.** A Redacto
//! document is text-only: the reference converter skips input fields with a
//! warning ("the Redacto target supports text-only documents"). There is no
//! form to walk, no control to set, no submit -- so there is no browser
//! driver, no interactive tool surface, and no per-profile `FormDriver`
//! choosing how a submit is triggered. What a [`profile::RenderProfile`]
//! actually varies is much narrower: whether an already-running platform's
//! rendering endpoint is configured at all.
//!
//! **Why this crate boots no custom image.** Every AEM verify profile needs
//! a privately-registry-hosted, pre-baked image (`docker/aem/README.md`).
//! This crate's one container is a public `postgres` image, always
//! available. What it proves with that container -- [`session`]'s own
//! throwaway Postgres import -- needs nothing else: a dump either imports
//! cleanly into the platform's own real schema or it does not, and that
//! question has nothing to do with Sling, AEM, or a browser.
//!
//! - [`dump_check`] -- offline structural validation (decode-or-error).
//! - [`session`] -- the throwaway (session-reused) Postgres import.
//! - [`profile`] -- the tenant seam, [`profile::RenderProfile`].
//! - [`flow`] -- the whole `verify_run` flow: dump check, Postgres import,
//!   and an optional render call against an already-running platform.

pub mod dump_check;
pub mod flow;
pub mod profile;
pub mod session;

pub use profile::RenderProfile;
