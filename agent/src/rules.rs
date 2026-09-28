//! The check rules a run's document is held to, and the sandbox they run in.
//!
//! The AEM rules are the UBS ones compiled into `u2s-aem-ubs-mcp`; the Redacto
//! format has none yet. Every rule runs in a `u2s-rules-worker` process with a
//! memory and time ceiling (see `u2s-rules-host`), so a runaway script fails its
//! own rule rather than the conversion. The worker ships next to every binary
//! that runs conversions, the way `libpdfium` does.

use std::path::PathBuf;

use u2s_doc_tools::native::RuleForCheck;
use u2s_rules_host::runner::RuleRunner;

use crate::OutputTarget;

/// The worker binary's file name.
fn worker_name() -> String {
    format!("u2s-rules-worker{}", std::env::consts::EXE_SUFFIX)
}

/// Where the worker is expected: next to the running executable. A test binary
/// runs from `target/<profile>/deps`, where the worker is one level up.
pub fn worker_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    let mut dir = exe
        .parent()
        .ok_or("this executable has no directory")?
        .to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    Ok(dir.join(worker_name()))
}

/// A runner over the worker next to this executable, or why there is none.
pub fn runner() -> Result<RuleRunner, String> {
    let path = worker_path()?;
    RuleRunner::new(path.clone(), RuleRunner::workers_from_env()).map_err(|e| {
        format!(
            "the rule sandbox cannot start: {e}. Build it with `cargo build -p u2s-rules-host \
             --bin u2s-rules-worker` (with `--release` for release builds); it must sit at {}",
            path.display()
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
    Ok(format!("{} check rules, sandbox at {}", rules.len(), worker_path()?.display()))
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
