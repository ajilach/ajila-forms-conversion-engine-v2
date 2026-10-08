//! Wires the desktop app to the conversion run in the `runner` crate.
//!
//! Everything here is app-side glue: implementing [`pipeline::RunObserver`]
//! against the Dioxus signal the UI renders, and projecting the finished
//! [`runner::Completed`] onto [`ProcessingState`].
//!
//! Building the agent, opening the edit-history session and driving the
//! controller live in [`runner::run`] — shared with the CLI, so both start a run
//! the same way. The stage sequencing itself — Author → (Reviewer →
//! Author-fix)* — lives in `pipeline::run`, where it can be tested without a
//! desktop runtime.

use dioxus::prelude::*;

use pipeline::{AbortFlag, RetryAction, RunEvent, RunObserver, RunSeed};
use runner::{LlmEndpoint, TurnPlan};

use crate::models::{
    AgentStep, AgentStepKind, AgentStepStatus, ProcessingState, ProcessingStep, RunState, StageInfo,
};

/// The choices the user made before starting a run.
pub struct RunConfig {
    pub profile: Option<String>,
    pub target: agent::OutputTarget,
    pub settings: crate::settings::AppSettings,
    /// Set by the Abort button to stop this run at its next checkpoint.
    pub abort: AbortFlag,
}

impl RunConfig {
    fn into_options(self) -> runner::RunOptions {
        runner::RunOptions {
            profile: self.profile,
            target: self.target,
            settings: self.settings,
            abort: self.abort,
        }
    }
}

// ── The progress seam: ProcessingState behind pipeline::RunObserver ──────────

/// Publishes the controller's progress into the signal the UI renders, and reads
/// the Retry button's answer back out.
struct DioxusObserver {
    state: RunState,
}

impl DioxusObserver {
    fn push(&mut self, step: AgentStep) {
        self.state.write().agent_steps.push(step);
    }

    fn thought(&mut self, label: impl Into<String>) {
        self.push(AgentStep {
            id: String::new(),
            kind: AgentStepKind::Thought,
            label: label.into(),
            detail: String::new(),
            status: AgentStepStatus::Done,
        });
    }
}

impl RunObserver for DioxusObserver {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::Stage { role, doing } => {
                self.thought(format!("── {role} — {doing} ──"));
                self.state.write().stage = Some(StageInfo { role: role.to_string(), doing });
            }
            RunEvent::Thought(text) => self.thought(text),
            RunEvent::ToolStarted {
                id,
                name,
                input_summary,
            } => self.push(AgentStep {
                id,
                kind: AgentStepKind::Tool,
                label: name,
                detail: input_summary,
                status: AgentStepStatus::Running,
            }),
            RunEvent::ToolFinished { id, ok, .. } => {
                let mut s = self.state.write();
                if let Some(step) = s.agent_steps.iter_mut().rev().find(|s| s.id == id) {
                    step.status = ok.into();
                }
            }
            RunEvent::Warning(w) => self.state.write().warnings.push(w),
            RunEvent::ContextUsed(tokens) => self.state.write().context_used_tokens = tokens,
            RunEvent::Spend(spend) => self.state.write().spend = Some(spend),
            RunEvent::Rules(rules) => self.state.write().rules = rules,
            RunEvent::Judging { rule_id, running } => {
                let mut s = self.state.write();
                if running {
                    s.judging.insert(rule_id);
                } else {
                    s.judging.remove(&rule_id);
                }
            }
            // Emitted at every abort checkpoint, so record it only once.
            RunEvent::Aborted => {
                let first = !std::mem::replace(&mut self.state.write().aborted, true);
                if first {
                    self.thought("Run aborted by the user.");
                }
            }
        }
    }

    fn retry_prompt(&mut self, role: &str, error: &str) {
        let mut s = self.state.write();
        s.error = Some(format!("Agent failed ({role}): {error}"));
        s.retry_pending = true;
        s.retry_action = None;
    }

    fn poll_retry(&mut self) -> Option<RetryAction> {
        self.state.read().retry_action
    }

    fn retry_resolved(&mut self, action: RetryAction) {
        let mut s = self.state.write();
        s.retry_pending = false;
        s.retry_action = None;
        if action == RetryAction::Retry {
            s.error = None;
        }
    }
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Run the autonomous conversion pipeline end-to-end on a fresh upload.
///
/// Returns the edit-history session the run recorded into, for the caller to
/// keep against whichever conversion it started. Returned rather than written
/// through a signal so this stays independent of how the app organises its runs.
pub async fn run_agent(
    files: Vec<(String, Vec<u8>)>,
    config: RunConfig,
    session_label: String,
    processing_state: RunState,
) -> Option<String> {
    let opts = config.into_options();
    let observer = pipeline::SharedObserver::new(announce(&opts, processing_state));
    let completed = runner::run_fresh(files, &opts, &session_label, &observer).await;
    publish(completed, opts.target, processing_state)
}

/// Carry an existing session on, either applying the user's feedback or simply
/// finishing what the last run left.
///
/// Which of the two is the seed's business — the app decides it once, where the
/// user's typing arrives, rather than carrying an "empty feedback" case down
/// into the run.
pub async fn run_agent_resume(
    seed: RunSeed,
    pdfs: Vec<(String, Vec<u8>)>,
    config: RunConfig,
    structured_session: String,
    processing_state: RunState,
) -> Option<String> {
    let opts = config.into_options();
    let observer = pipeline::SharedObserver::new(announce(&opts, processing_state));
    let completed = runner::resume(seed, pdfs, &opts, structured_session, &observer).await;
    publish(completed, opts.target, processing_state)
}

/// Surface the run's token budget, so a mis-detected context window is visible,
/// and hand back the observer the run will report through.
fn announce(
    opts: &runner::RunOptions,
    mut processing_state: RunState,
) -> DioxusObserver {
    let plan = TurnPlan::for_settings(&opts.settings);
    processing_state.write().context_window = plan.context_window;

    let mut observer = DioxusObserver {
        state: processing_state,
    };
    observer.emit(RunEvent::Thought(plan.describe()));
    observer
}

/// Fold a finished run into the state the box renders, returning the edit-history
/// session the run belongs to once there is one.
///
/// Pure so it can be tested without a desktop runtime: [`publish`] is only the
/// signal plumbing around it.
///
/// Every branch *edits* the state rather than replacing it. The activity
/// timeline is the run's only record of what happened, and a failed run is
/// exactly when the user most needs to read it — so a failure records the error
/// alongside the transcript instead of in place of it.
fn apply_completed(
    state: &mut ProcessingState,
    completed: Result<runner::Completed, String>,
    target: agent::OutputTarget,
) -> Option<String> {
    // However the run ended, no agent works for it any more.
    state.stage = None;
    state.judging.clear();
    let completed = match completed {
        Ok(completed) => completed,
        Err(e) => {
            // The step stays as it was: `screen_for` reads a recorded error on a
            // run that is no longer in flight as a failure, so nothing else has
            // to be set for the box to reach its terminal screen.
            state.error = Some(e);
            return None;
        }
    };

    // Aborted, or the user gave up at a retry prompt. The observer has already
    // recorded why; there is no result to publish. The session is deliberately
    // not adopted — nothing can resume a run that produced no snapshot yet, and
    // the restore work is what makes a stopped run continuable.
    let outcome = completed.outcome?;

    state.warnings.extend(outcome.warnings);
    state.step = ProcessingStep::Complete;
    state.target = target;
    state.xsd_schema = outcome.xsd_schema;
    state.aem_package = outcome.aem_package;
    state.aem_package_bound = outcome.aem_package_bound;
    state.redacto_sql = outcome.redacto_sql;
    state.form_code = outcome.form_code;
    state.rules = outcome.rules;
    state.elapsed_secs = Some(completed.elapsed_secs);

    Some(completed.session_id)
}

/// Project a finished run onto the UI state, handing back its session.
fn publish(
    completed: Result<runner::Completed, String>,
    target: agent::OutputTarget,
    mut processing_state: RunState,
) -> Option<String> {
    apply_completed(&mut processing_state.write(), completed, target)
}

/// Describe a reference form so the reference store can match against it.
///
/// The same stage machinery as a conversion, over the same tool catalog — this
/// wrapper only supplies the transport and swallows progress, since the
/// references page reports status itself.
pub async fn describe_reference(
    profile: &str,
    pdfs: Vec<(String, Vec<u8>)>,
    package_zip: Vec<u8>,
    endpoint: LlmEndpoint,
) -> Result<String, String> {
    let resolved = TurnPlan::for_endpoint(endpoint).resolve()?;
    pipeline::describe::describe_reference(
        profile,
        pdfs,
        package_zip,
        &AbortFlag::default(),
        resolved.model,
        resolved.price,
        resolved.max_tokens,
        resolved.context_budget,
        &pipeline::SharedObserver::new(pipeline::NullObserver),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run that already reported some work, as any real failure would have.
    fn run_in_flight() -> ProcessingState {
        ProcessingState {
            step: ProcessingStep::Running,
            target: agent::OutputTarget::Aem,
            agent_steps: vec![AgentStep {
                id: "t1".into(),
                kind: AgentStepKind::Tool,
                label: "pdf_info".into(),
                detail: String::new(),
                status: AgentStepStatus::Done,
            }],
            warnings: vec!["a page had no fields".into()],
            ..ProcessingState::default()
        }
    }

    /// The timeline is the only record of what a run did, and a failure is
    /// exactly when the user needs to read it — so recording the error must not
    /// throw the transcript away with it.
    #[test]
    fn a_failed_run_keeps_its_transcript() {
        let mut state = run_in_flight();

        let session = apply_completed(
            &mut state,
            Err("Agent failed (Author): overloaded".into()),
            agent::OutputTarget::Aem,
        );

        assert_eq!(session, None, "a run that never started records no session");
        assert_eq!(
            state.error.as_deref(),
            Some("Agent failed (Author): overloaded")
        );
        assert_eq!(state.agent_steps.len(), 1, "the transcript has to survive");
        assert_eq!(state.warnings, ["a page had no fields"]);
        assert_eq!(state.target, agent::OutputTarget::Aem);
        assert_ne!(
            state.step,
            ProcessingStep::Complete,
            "a failed run must not report a result"
        );
    }

    /// A finished run shows its final rule check, and no agent at work.
    #[test]
    fn a_finished_run_shows_its_final_rules_and_no_working_agent() {
        let mut state = run_in_flight();
        state.stage = Some(StageInfo { role: "Final check".into(), doing: "checking every rule".into() });
        state.judging.insert("r".into());
        let rules = vec![agent::RuleView {
            rule_id: "r".into(),
            title: "A rule".into(),
            kind: agent::RuleKind::Judge,
            state: agent::RuleState::Pass,
            outdated: false,
        }];
        let outcome = pipeline::RunOutcome {
            document: serde_json::Value::Null,
            aem_package: None,
            aem_package_bound: None,
            xsd_schema: None,
            redacto_sql: None,
            form_code: None,
            warnings: Vec::new(),
            review: None,
            rules: rules.clone(),
        };

        apply_completed(
            &mut state,
            Ok(runner::Completed { session_id: "s-1".into(), outcome: Some(outcome), elapsed_secs: 3 }),
            agent::OutputTarget::Aem,
        );

        assert_eq!(state.rules, rules);
        assert_eq!(state.stage, None);
        assert!(state.judging.is_empty());
    }

    /// A stopped run leaves no result, but what it managed to do still has to be
    /// readable.
    #[test]
    fn an_aborted_run_keeps_its_transcript() {
        let mut state = run_in_flight();
        state.aborted = true;

        let session = apply_completed(
            &mut state,
            Ok(runner::Completed {
                session_id: "s-1".into(),
                outcome: None,
                elapsed_secs: 12,
            }),
            agent::OutputTarget::Aem,
        );

        assert_eq!(session, None);
        assert_eq!(state.agent_steps.len(), 1);
        assert_eq!(state.error, None, "aborting is not a failure");
        assert_ne!(state.step, ProcessingStep::Complete);
    }
}
