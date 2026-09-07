//! The host side of a conversion run, shared by everything that drives one.
//!
//! `pipeline` sequences the roles but names no provider and no UI. This crate is
//! the other half: the rig client factory that resolves an endpoint to a model
//! ([`client`]), the per-endpoint request gate ([`ratelimit`]) and the model
//! limits it reads ([`models`]), the operator settings that configure them
//! ([`settings`]), [`turns::TurnPlan`] resolving an endpoint to what
//! `pipeline` needs to drive a stage, and the run entry points ([`run`]) that
//! build the agent, open an edit-history session and record the result.
//!
//! So the desktop app and the CLI start a run through the same code and differ
//! only in how they report it and where they put the artefacts.
//!
//! The context budget this crate used to enforce by hand — evicting messages
//! to keep a request inside the window — is gone: `pipeline` now runs each
//! stage on rig's own `Agent::runner`, and the single oversized reply that
//! used to blow the window is capped at the source instead (see `agent`'s tool
//! executors). A rig-native memory/compaction policy taking over per-turn
//! shaping is tracked separately, not yet built.

pub mod aem_lock;
pub mod artifacts;
pub mod client;
pub mod models;
pub mod pricing;
pub mod provider;
pub mod ratelimit;
pub mod run;
pub mod settings;
pub mod turns;

pub use artifacts::{Artifact, artifact_filename};
pub use provider::{LlmEndpoint, Provider};
pub use run::{Completed, RunOptions, resume, run_fresh};
pub use settings::AppSettings;
pub use turns::TurnPlan;
