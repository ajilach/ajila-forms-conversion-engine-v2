//! What a single script evaluation is allowed to cost.
//!
//! boa gives two of these for free (loop iterations and call recursion — see
//! [`crate::sandbox`]); the third, wall-clock time, it gives no hook for at
//! all. `check::run_check` therefore checks the wall clock only *after* a
//! script returns, which means this crate on its own cannot interrupt a
//! script that is already running long, and has no heap cap to offer
//! either.
//!
//! Neither gap is left open in practice, and neither is closed here:
//! `u2s-rules-host` runs every script in a worker process with an OS-level
//! memory and CPU ceiling and a deadline it enforces by killing the child.
//! The budget below is what that host passes down, so these numbers are
//! still the ones that decide a script's fate -- they are simply enforced
//! from outside, by something that can act on them.

use std::time::Duration;

/// Limits for one `check(output, ctx)` evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptBudget {
    /// Total loop-body executions across every loop in the script, enforced
    /// by boa's VM itself (`RuntimeLimits::set_loop_iteration_limit`) —
    /// `while (true) {}` throws rather than hanging the process.
    pub loop_iterations: u64,
    /// Function-call nesting depth (`RuntimeLimits::set_recursion_limit`).
    pub recursion_limit: usize,
    /// Native stack budget in bytes (`RuntimeLimits::set_stack_size_limit`).
    pub stack_size_bytes: usize,
    /// Wall-clock ceiling for one script. Checked before a script starts and
    /// after it returns, never during — boa has no interrupt hook, so a
    /// script that evades the two limits above can still run past this.
    pub wall_clock: Duration,
}

impl Default for ScriptBudget {
    /// Generous enough for a real tree walk over a few thousand nodes,
    /// tight enough that a runaway script fails fast rather than degrading
    /// the corpus check it is part of.
    fn default() -> Self {
        Self {
            loop_iterations: 1_000_000,
            recursion_limit: 512,
            stack_size_bytes: 10 * 1024 * 1024,
            wall_clock: Duration::from_secs(2),
        }
    }
}
