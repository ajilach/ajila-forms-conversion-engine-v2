//! The conversion pipeline: the controller that sequences an
//! [`agent::ConversionAgent`] through its Author → Reviewer stages.
//!
//! It sits between `agent` (the tools) and a consumer (the desktop app), and
//! depends on neither a UI framework nor an LLM provider directly. It does
//! speak rig's message and agent model: each stage runs as a real
//! [`rig_agent::agent::Agent`], driven through [`rig_agent::agent::AgentRunner`],
//! with the choice of model and its pricing supplied by the caller through
//! [`RunConfig`] rather than owned here. Everything else variable reaches it
//! through one seam:
//!
//! * [`SharedObserver`] receives progress and answers retry prompts — the
//!   consumer owns how that is displayed and decided.
//!
//! That is what makes the sequencing testable: [`run`] can be driven end to end
//! by rig's own [`rig_core::test_utils::MockCompletionModel`] and a recording
//! observer, with no network and no desktop runtime.

pub mod describe;
pub mod hooks;
mod judge;
pub mod memory;
pub mod observer;
pub mod roles;
pub mod run;
mod substage;
pub mod tools;
pub mod turns;

pub use hooks::PriceFn;
pub use memory::ContextBudget;
pub use observer::{AbortFlag, NullObserver, RetryAction, RunEvent, RunObserver, SharedObserver, Spend};
pub use run::{RunConfig, RunOutcome, RunSeed, describe_completion_error, is_transient_error, run};
pub use tools::SharedAgent;
