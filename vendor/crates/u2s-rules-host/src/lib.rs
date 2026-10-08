//! Runs generated rule scripts in isolated worker processes.
//!
//! `u2s-rules` is the sandbox: pure, synchronous, and deliberately free of
//! tokio or any notion of a process. This crate is what stands between that
//! sandbox and a server that must survive the scripts it runs.
//!
//! # The gap this closes
//!
//! boa gives no heap cap and no interrupt hook. In-process, that leaves two
//! failure modes with no defence: a script asking for more memory than
//! exists aborts the whole server (Rust aborts on allocation failure, so
//! neither `catch_unwind` nor `spawn_blocking` contains it), and a script
//! stuck in a tight non-looping computation never returns, stranding its
//! thread where `tokio::time::timeout` cannot reach it.
//!
//! Out of process both are ordinary events. The kernel enforces a memory and
//! CPU ceiling on the worker, the host enforces a wall-clock deadline it can
//! actually act on, and either way the death is attributed to the one script
//! that caused it. That is what makes PLAN.md's "an overrunning or throwing
//! script fails **the rule**, not the run" true rather than aspirational.
//!
//! # Why this is its own crate
//!
//! Both script-execution paths must use it, and one of them is `u2s-agent`,
//! which may not depend on `u2s-store` (an architectural rule enforced by
//! `u2s-server`'s `genericity` test). A crate that depends only on
//! `u2s-rules` and tokio is importable from the agent and the server alike.

pub mod protocol;
pub mod runner;
pub mod worker;

pub use protocol::{CheckRequest, CheckResponse, WireBudget, WireVerdict};
pub use runner::{RuleRunner, RunnerError, WORKER_BIN};
