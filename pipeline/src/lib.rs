//! The conversion pipeline: the controller that sequences an
//! [`agent::ConversionAgent`] through its Analyst → Author → Reviewer stages.
//!
//! It sits between `agent` (the tools) and a consumer (the desktop app), and
//! depends on neither a UI framework nor an LLM provider. It does speak rig's
//! message model, which is provider-neutral: naming rig names no vendor, no
//! model and no key. Everything variable
//! reaches it through two traits:
//!
//! * [`TurnProvider`] runs one model turn — the consumer owns the transport,
//!   the credentials and the model choice.
//! * [`RunObserver`] receives progress and answers retry prompts — the consumer
//!   owns how that is displayed and decided.
//!
//! That is what makes the sequencing testable: [`run`] can be driven end to end
//! by a scripted provider and a recording observer, with no network and no
//! desktop runtime.

pub mod describe;
pub mod observer;
pub mod roles;
pub mod run;
pub mod turns;

pub use observer::{AbortFlag, NullObserver, RetryAction, RunEvent, RunObserver};
pub use run::{RunConfig, RunOutcome, RunSeed, run};
pub use turns::{ModelReply, TurnProvider, tool_definitions, tool_result_content, tool_results};
