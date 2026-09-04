//! The host side of a conversion run, shared by everything that drives one.
//!
//! `pipeline` sequences the roles but names no provider and no UI. This crate is
//! the other half: the rig client factory that resolves an endpoint to a model
//! ([`client`]), the streamed model call itself ([`stream`]), the context budget
//! that keeps a request inside the window ([`context`]) and the model limits it
//! reads ([`models`]), the operator settings that configure them ([`settings`]),
//! the [`pipeline::TurnProvider`] binding it together ([`turns`]), and the run
//! entry points ([`run`]) that build the agent, open an edit-history session and
//! record the result.
//!
//! So the desktop app and the CLI start a run through the same code and differ
//! only in how they report it and where they put the artefacts.

pub mod aem_lock;
pub mod artifacts;
pub mod client;
pub mod context;
pub mod models;
pub mod provider;
pub mod ratelimit;
pub mod run;
pub mod stream;
pub mod settings;
pub mod turns;

pub use artifacts::{Artifact, artifact_filename};
pub use provider::{LlmEndpoint, Provider};
pub use run::{Completed, RunOptions, run_feedback, run_fresh};
pub use settings::AppSettings;
pub use turns::{ConfiguredTurns, TurnPlan};
