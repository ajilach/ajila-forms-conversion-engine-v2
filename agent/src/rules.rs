//! The check rules a run's document is held to, and the sandbox they run in.
//!
//! The AEM rules are the UBS ones compiled into `u2s-aem-ubs-mcp`; the Redacto
//! format has none yet. Every rule runs in a worker process with a
//! memory and time ceiling (see `u2s-rules-host`), so a runaway script fails its
//! own rule rather than the conversion. That process is the running executable
//! itself, started with the worker flag (see [`runner`]).

use std::ffi::OsString;
use std::path::PathBuf;

use u2s_doc_tools::native::RuleForCheck;
use u2s_rules_host::runner::RuleRunner;
use u2s_rules_host::worker::WORKER_ARG;

use crate::OutputTarget;

/// The worker binary's file name, which only test binaries use.
fn worker_name() -> String {
    format!("u2s-rules-worker{}", std::env::consts::EXE_SUFFIX)
}

/// What every rule runs in: this executable itself, started with
/// [`WORKER_ARG`], so the app, the CLI and the MCP server ship no second
/// program. Each of them hands control to [`serve_worker_if_invoked`] first
/// thing in `main`.
///
/// A test binary cannot act as the worker (the test harness owns its `main`),
/// so a test runs from `target/<profile>/deps` and uses the built
/// `u2s-rules-worker` one level up instead.
fn worker_command() -> Result<(PathBuf, Vec<OsString>), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    let dir = exe.parent().ok_or("this executable has no directory")?;
    if dir.ends_with("deps") {
        let parent = dir.parent().ok_or("the test binary's directory has no parent")?;
        return Ok((parent.join(worker_name()), Vec::new()));
    }
    Ok((exe, vec![WORKER_ARG.into()]))
}

/// Serves rule-worker requests and exits, when this process was started as
/// the worker; returns otherwise. Call it first thing in `main`, before a
/// runtime, a window or a stdio server exists.
pub fn serve_worker_if_invoked() {
    if u2s_rules_host::worker::invoked_as_worker() {
        u2s_rules_host::worker::run();
        std::process::exit(0);
    }
}

/// A runner over this executable as its own worker, or why there is none.
pub fn runner() -> Result<RuleRunner, String> {
    let (program, args) = worker_command()?;
    RuleRunner::with_args(program.clone(), args, RuleRunner::workers_from_env()).map_err(|e| {
        format!(
            "the rule sandbox cannot start: {e}. A test needs the worker built first: \
             `cargo build -p u2s-rules-host --bin u2s-rules-worker` (with `--release` for \
             release builds), so that it sits at {}",
            program.display()
        )
    })
}

/// The rules `target`'s documents are checked against.
pub fn rules_for(target: OutputTarget) -> Result<Vec<RuleForCheck>, String> {
    match target {
        OutputTarget::Aem => u2s_doc_tools::rules_dir::load_rules(u2s_aem_ubs_mcp::rule_files())
            .map_err(|e| format!("the UBS AEM rules do not load: {e}")),
        OutputTarget::Redacto => Ok(Vec::new()),
    }
}

/// Check that `target`'s rules load and their sandbox starts, before a run
/// spends a token: a document that cannot be held to its rules is not a
/// conversion to start. Reports what it checked.
pub fn readiness(target: OutputTarget) -> Result<String, String> {
    let rules = rules_for(target)?;
    if rules.is_empty() {
        return Ok("no check rules for this format".into());
    }
    runner()?;
    Ok(format!("{} check rules, each run in a sandboxed worker process", rules.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aem_rules_load_and_redacto_has_none() {
        assert!(!rules_for(OutputTarget::Aem).unwrap().is_empty());
        assert!(rules_for(OutputTarget::Redacto).unwrap().is_empty());
    }
}
