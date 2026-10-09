//! Wires the desktop app to the conversion run in the `runner` crate.
//!
//! Everything here is app-side glue: implementing [`pipeline::RunObserver`]
//! for the desktop app, carrying what it reports to the UI thread, and projecting
//! the finished [`runner::Completed`] onto [`ProcessingState`].
//!
//! Building the agent, opening the edit-history session and driving the
//! controller live in [`runner::run`] — shared with the CLI, so both start a run
//! the same way. The stage sequencing itself — Author → (Reviewer →
//! Author-fix)* — lives in `pipeline::run`, where it can be tested without a
//! desktop runtime.
//!
//! The run and the UI never share a lock. The run, on a worker thread, sends
//! [`UiUpdate`]s down an unbounded channel and carries on; [`show_progress`], on
//! the UI thread, applies them to the tab's [`RunState`]. The only thing the
//! run reads back is the Retry / Give up answer, through its own small cell
//! ([`RetryAnswer`]).

use dioxus::prelude::*;
use tokio::sync::mpsc;

use pipeline::{AbortFlag, RetryAction, RunEvent, RunObserver, RunSeed};
use runner::{LlmEndpoint, TurnPlan};

use crate::models::{
    AgentStep, AgentStepKind, AgentStepStatus, ProcessingState, ProcessingStep, RetryAnswer,
    RunState, StageInfo,
};

/// The choices the user made before starting a run.
pub struct RunConfig {
    pub profile: Option<String>,
    pub settings: crate::settings::AppSettings,
    /// Set by the Abort button to stop this run at its next checkpoint.
    pub abort: AbortFlag,
    /// Where the Retry / Give up buttons leave their answer for a paused run.
    pub retry: RetryAnswer,
}

impl RunConfig {
    fn into_parts(self) -> (runner::RunOptions, RetryAnswer) {
        let opts = runner::RunOptions {
            profile: self.profile,
            settings: self.settings,
            abort: self.abort,
        };
        (opts, self.retry)
    }
}

// ── The progress seam: run → UI over a channel ───────────────────────────────

/// One change the run asks the UI to make to its [`ProcessingState`].
pub enum UiUpdate {
    /// A progress event, as the controller reported it.
    Event(RunEvent),
    /// The model's context window, the denominator of the fill indicator.
    ContextWindow(usize),
    /// A request failed and the run is paused, waiting for Retry or Give up.
    RetryPrompt(String),
    /// The paused run picked up the user's answer.
    RetryResolved(RetryAction),
    /// The run is over. Always the last update a run sends. Boxed: it is sent
    /// once, and would otherwise size every other update to fit it.
    Finished(Box<Result<runner::Completed, String>>),
}

/// The run's end of the progress channel.
pub type ProgressSender = mpsc::UnboundedSender<UiUpdate>;
/// The UI's end of the progress channel, drained by [`show_progress`].
pub type ProgressReceiver = mpsc::UnboundedReceiver<UiUpdate>;

/// A fresh progress channel for one run.
///
/// Unbounded on purpose: a bounded channel would make the run wait whenever the
/// UI falls behind, which is the coupling this seam exists to remove. What
/// queues while the UI is busy is small — step labels and counters — and the UI
/// drains all of it at its next chance.
pub fn progress_channel() -> (ProgressSender, ProgressReceiver) {
    mpsc::unbounded_channel()
}

/// How a run's progress ended, as [`show_progress`] saw it.
#[derive(Debug, PartialEq)]
pub enum Settled {
    /// The run reported its result. Carries the edit-history session it
    /// recorded into, when the result adopts one.
    Finished(Option<String>),
    /// The run's task ended without reporting a result — only a panic does.
    Vanished,
}

/// Apply the run's updates to the tab's state until the run reports its
/// result.
///
/// Runs on the UI thread. It returns in the same step that applies the result,
/// so the caller's bookkeeping happens before the user can click anything on
/// the finished box — a Continue pressed in between would otherwise reset the
/// state the bookkeeping is about to read. Whatever has queued up is applied
/// under one write, so a burst of events costs one render rather than one each.
pub async fn show_progress(updates: ProgressReceiver, state: RunState) -> Settled {
    // `write_unchecked` only because a closure cannot lend out a borrow of what
    // it captured; each guard is dropped before the loop awaits again.
    pump(updates, move || state.write_unchecked()).await
}

/// The loop behind [`show_progress`], over any way of writing the state, so it
/// can be tested without a desktop runtime.
async fn pump<W>(mut updates: ProgressReceiver, mut write: impl FnMut() -> W) -> Settled
where
    W: std::ops::DerefMut<Target = ProcessingState>,
{
    while let Some(first) = updates.recv().await {
        let mut state = write();
        let mut update = Some(first);
        while let Some(current) = update {
            if matches!(current, UiUpdate::Finished(_)) {
                return Settled::Finished(apply_update(&mut state, current));
            }
            apply_update(&mut state, current);
            update = updates.try_recv().ok();
        }
    }
    Settled::Vanished
}

/// Fold one update into the state the box renders, returning the run's session
/// when the update is the one that finishes it.
///
/// Pure, so the whole seam can be tested without a desktop runtime.
fn apply_update(state: &mut ProcessingState, update: UiUpdate) -> Option<String> {
    match update {
        UiUpdate::Event(event) => {
            apply_event(state, event);
            None
        }
        UiUpdate::ContextWindow(tokens) => {
            state.context_window = tokens;
            None
        }
        UiUpdate::RetryPrompt(error) => {
            state.error = Some(error);
            state.retry_pending = true;
            None
        }
        UiUpdate::RetryResolved(action) => {
            state.retry_pending = false;
            if action == RetryAction::Retry {
                state.error = None;
            }
            None
        }
        UiUpdate::Finished(completed) => apply_completed(state, *completed),
    }
}

fn apply_event(state: &mut ProcessingState, event: RunEvent) {
    match event {
        RunEvent::Stage { role, doing } => {
            push_thought(state, format!("── {role} — {doing} ──"));
            state.stage = Some(StageInfo { role: role.to_string(), doing });
        }
        RunEvent::Thought(text) => push_thought(state, text),
        RunEvent::ToolStarted {
            id,
            name,
            input_summary,
        } => state.agent_steps.push(AgentStep {
            id,
            kind: AgentStepKind::Tool,
            label: name,
            detail: input_summary,
            status: AgentStepStatus::Running,
        }),
        RunEvent::ToolFinished { id, ok, .. } => {
            if let Some(step) = state.agent_steps.iter_mut().rev().find(|s| s.id == id) {
                step.status = ok.into();
            }
        }
        RunEvent::Warning(w) => state.warnings.push(w),
        RunEvent::ContextUsed(tokens) => state.context_used_tokens = tokens,
        RunEvent::Spend(spend) => state.spend = Some(spend),
        RunEvent::Rules(rules) => state.rules = rules,
        RunEvent::Judging { rule_id, running } => {
            if running {
                state.judging.insert(rule_id);
            } else {
                state.judging.remove(&rule_id);
            }
        }
        // Emitted at every abort checkpoint, so record it only once.
        RunEvent::Aborted => {
            if !state.aborted {
                state.aborted = true;
                push_thought(state, "Run aborted by the user.");
            }
        }
    }
}

fn push_thought(state: &mut ProcessingState, label: impl Into<String>) {
    state.agent_steps.push(AgentStep {
        id: String::new(),
        kind: AgentStepKind::Thought,
        label: label.into(),
        detail: String::new(),
        status: AgentStepStatus::Done,
    });
}

/// The run's side of the seam: forwards what the controller reports and reads
/// the Retry button's answer back out, without ever touching the UI's state.
struct DioxusObserver {
    progress: ProgressSender,
    retry: RetryAnswer,
}

impl DioxusObserver {
    /// Hand an update to the UI. A closed channel means the tab's progress task
    /// is gone (the window closed); the run carries on regardless, and its
    /// recording and edit history still capture everything.
    fn send(&self, update: UiUpdate) {
        let _ = self.progress.send(update);
    }
}

impl RunObserver for DioxusObserver {
    fn emit(&mut self, event: RunEvent) {
        self.send(UiUpdate::Event(event));
    }

    fn retry_prompt(&mut self, role: &str, error: &str) {
        // A click left over from an earlier prompt must not answer this one.
        self.retry.clear();
        self.send(UiUpdate::RetryPrompt(format!(
            "Agent failed ({role}): {error}"
        )));
    }

    fn poll_retry(&mut self) -> Option<RetryAction> {
        self.retry.take()
    }

    fn retry_resolved(&mut self, action: RetryAction) {
        self.send(UiUpdate::RetryResolved(action));
    }
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Run the autonomous conversion pipeline end-to-end on a fresh upload.
///
/// Everything it has to tell the UI — progress, and the result last of all —
/// goes through `progress`; pair it with [`show_progress`] on the UI thread.
pub async fn run_agent(
    files: Vec<(String, Vec<u8>)>,
    config: RunConfig,
    session_label: String,
    progress: ProgressSender,
) {
    let (opts, retry) = config.into_parts();
    let observer = pipeline::SharedObserver::new(announce(&opts, progress.clone(), retry));
    let completed = runner::run_fresh(files, &opts, &session_label, &observer).await;
    finish(&progress, completed);
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
    progress: ProgressSender,
) {
    let (opts, retry) = config.into_parts();
    let observer = pipeline::SharedObserver::new(announce(&opts, progress.clone(), retry));
    let completed = runner::resume(seed, pdfs, &opts, structured_session, &observer).await;
    finish(&progress, completed);
}

/// Surface the run's token budget, so a mis-detected context window is visible,
/// and hand back the observer the run will report through.
fn announce(
    opts: &runner::RunOptions,
    progress: ProgressSender,
    retry: RetryAnswer,
) -> DioxusObserver {
    let plan = TurnPlan::for_settings(&opts.settings);
    let mut observer = DioxusObserver { progress, retry };
    observer.send(UiUpdate::ContextWindow(plan.context_window));
    observer.emit(RunEvent::Thought(plan.describe()));
    observer
}

/// Send the run's result, the last update it makes.
fn finish(progress: &ProgressSender, completed: Result<runner::Completed, String>) {
    let _ = progress.send(UiUpdate::Finished(Box::new(completed)));
}

/// Fold a finished run into the state the box renders, returning the edit-history
/// session the run belongs to once there is one.
///
/// Every branch *edits* the state rather than replacing it. The activity
/// timeline is the run's only record of what happened, and a failed run is
/// exactly when the user most needs to read it — so a failure records the error
/// alongside the transcript instead of in place of it.
fn apply_completed(state: &mut ProcessingState, completed: Result<runner::Completed, String>) -> Option<String> {
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
    state.xsd_schema = outcome.xsd_schema;
    state.aem_package = outcome.aem_package;
    state.aem_package_bound = outcome.aem_package_bound;
    state.form_code = outcome.form_code;
    state.rules = outcome.rules;
    state.elapsed_secs = Some(completed.elapsed_secs);

    Some(completed.session_id)
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

    /// An observer wired to a fresh channel, plus the UI's end of it.
    fn observer() -> (DioxusObserver, ProgressReceiver, RetryAnswer) {
        let (progress, updates) = progress_channel();
        let retry = RetryAnswer::default();
        let observer = DioxusObserver {
            progress,
            retry: retry.clone(),
        };
        (observer, updates, retry)
    }

    /// Everything queued so far, applied as the UI's progress task would.
    fn drain(updates: &mut ProgressReceiver, state: &mut ProcessingState) -> Option<String> {
        let mut session = None;
        while let Ok(update) = updates.try_recv() {
            session = apply_update(state, update).or(session);
        }
        session
    }

    /// What the run reports reaches the state in the order it happened, as it
    /// did when the observer wrote the state directly.
    #[test]
    fn progress_arrives_in_order() {
        let (mut obs, mut updates, _) = observer();
        obs.emit(RunEvent::Stage {
            role: "Author",
            doing: "building".into(),
        });
        obs.emit(RunEvent::ToolStarted {
            id: "c1".into(),
            name: "get_xfa".into(),
            input_summary: "page 1".into(),
        });
        obs.emit(RunEvent::ToolFinished {
            id: "c1".into(),
            ok: false,
            reply_chars: 10,
        });
        obs.emit(RunEvent::ContextUsed(1_234));
        obs.emit(RunEvent::Warning("no footer".into()));
        obs.send(UiUpdate::ContextWindow(200_000));

        let mut state = ProcessingState::default();
        assert_eq!(drain(&mut updates, &mut state), None);

        assert_eq!(state.agent_steps.len(), 2);
        assert_eq!(state.agent_steps[0].label, "── Author — building ──");
        assert_eq!(state.agent_steps[1].label, "get_xfa");
        assert_eq!(state.agent_steps[1].status, AgentStepStatus::Error);
        assert_eq!(state.context_used_tokens, 1_234);
        assert_eq!(state.context_window, 200_000);
        assert_eq!(state.warnings, ["no footer"]);
    }

    /// Abort is emitted at every checkpoint; the box says so once.
    #[test]
    fn an_abort_is_recorded_once() {
        let (mut obs, mut updates, _) = observer();
        obs.emit(RunEvent::Aborted);
        obs.emit(RunEvent::Aborted);

        let mut state = ProcessingState::default();
        drain(&mut updates, &mut state);

        assert!(state.aborted);
        assert_eq!(state.agent_steps.len(), 1);
    }

    /// The point of the channel: reporting progress never waits for the UI.
    /// Nothing drains here — the UI is as stuck as it can be — and the run still
    /// gets through every event; with the UI gone altogether, it carries on too.
    #[test]
    fn reporting_never_waits_for_the_ui() {
        let (mut obs, updates, _) = observer();
        for i in 0..50_000 {
            obs.emit(RunEvent::Thought(format!("turn {i}")));
        }
        assert_eq!(updates.len(), 50_000);

        drop(updates);
        obs.emit(RunEvent::Thought("after the window closed".into()));
        obs.retry_prompt("Author", "overloaded");
        assert_eq!(obs.poll_retry(), None);
    }

    /// The retry handshake no longer goes through the run state: the prompt and
    /// its resolution travel as updates, the answer through its own cell.
    #[test]
    fn a_retry_is_asked_answered_and_resolved() {
        let (mut obs, mut updates, retry) = observer();
        let mut state = ProcessingState::default();

        // A click from an earlier pause must not answer the next one.
        retry.answer(RetryAction::Cancel);
        obs.retry_prompt("Reviewer", "HTTP 529");
        drain(&mut updates, &mut state);
        assert!(state.retry_pending);
        assert_eq!(
            state.error.as_deref(),
            Some("Agent failed (Reviewer): HTTP 529")
        );
        assert_eq!(obs.poll_retry(), None, "a stale answer was carried over");

        retry.answer(RetryAction::Retry);
        assert_eq!(obs.poll_retry(), Some(RetryAction::Retry));
        assert_eq!(obs.poll_retry(), None, "an answer is consumed once");

        obs.retry_resolved(RetryAction::Retry);
        drain(&mut updates, &mut state);
        assert!(!state.retry_pending);
        assert_eq!(state.error, None, "a retry clears the error it answered");
    }

    /// Giving up keeps the error on screen: it is why the run stopped.
    #[test]
    fn giving_up_keeps_the_error() {
        let (mut obs, mut updates, _) = observer();
        let mut state = ProcessingState::default();
        obs.retry_prompt("Author", "HTTP 500");
        obs.retry_resolved(RetryAction::Cancel);
        drain(&mut updates, &mut state);
        assert!(!state.retry_pending);
        assert_eq!(
            state.error.as_deref(),
            Some("Agent failed (Author): HTTP 500")
        );
    }

    /// The result travels as the last update and is folded in like the rest.
    #[test]
    fn the_result_is_applied_after_the_progress() {
        let (mut obs, mut updates, _) = observer();
        obs.emit(RunEvent::Thought("working".into()));
        finish(
            &obs.progress,
            Err("Agent failed (Author): overloaded".into()),
        );
        drop(obs);

        let mut state = ProcessingState::default();
        assert_eq!(drain(&mut updates, &mut state), None);
        assert_eq!(state.agent_steps.len(), 1, "the transcript survives");
        assert_eq!(
            state.error.as_deref(),
            Some("Agent failed (Author): overloaded")
        );
        assert!(updates.is_closed(), "nothing can follow the result");
    }

    /// The UI's loop stops at the result, without waiting for the channel to
    /// close: the bookkeeping that follows must run before anything else can.
    #[tokio::test]
    async fn the_progress_loop_stops_at_the_result() {
        let (mut obs, updates, _) = observer();
        let still_open = obs.progress.clone();
        obs.emit(RunEvent::Thought("one".into()));
        obs.emit(RunEvent::Thought("two".into()));
        finish(
            &obs.progress,
            Err("Agent failed (Author): overloaded".into()),
        );

        let state = std::cell::RefCell::new(ProcessingState::default());
        let settled = pump(updates, || state.borrow_mut()).await;

        assert_eq!(settled, Settled::Finished(None));
        let state = state.into_inner();
        assert_eq!(state.agent_steps.len(), 2);
        assert_eq!(
            state.error.as_deref(),
            Some("Agent failed (Author): overloaded")
        );
        drop(still_open);
    }

    /// A run whose task dies without a result is reported as such, with what it
    /// managed to report still applied.
    #[tokio::test]
    async fn a_run_that_vanishes_is_noticed() {
        let (mut obs, updates, _) = observer();
        obs.emit(RunEvent::Thought("working".into()));
        drop(obs);

        let state = std::cell::RefCell::new(ProcessingState::default());
        let settled = pump(updates, || state.borrow_mut()).await;

        assert_eq!(settled, Settled::Vanished);
        assert_eq!(state.into_inner().agent_steps.len(), 1);
    }

    /// Progress sent while the UI is busy is applied in one write.
    #[tokio::test]
    async fn queued_progress_is_applied_in_one_write() {
        let (mut obs, updates, _) = observer();
        for i in 0..100 {
            obs.emit(RunEvent::Thought(format!("turn {i}")));
        }
        drop(obs);

        let state = std::cell::RefCell::new(ProcessingState::default());
        let writes = std::cell::Cell::new(0);
        pump(updates, || {
            writes.set(writes.get() + 1);
            state.borrow_mut()
        })
        .await;

        assert_eq!(writes.get(), 1);
        assert_eq!(state.into_inner().agent_steps.len(), 100);
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
        );

        assert_eq!(session, None, "a run that never started records no session");
        assert_eq!(
            state.error.as_deref(),
            Some("Agent failed (Author): overloaded")
        );
        assert_eq!(state.agent_steps.len(), 1, "the transcript has to survive");
        assert_eq!(state.warnings, ["a page had no fields"]);
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
            form_code: None,
            warnings: Vec::new(),
            review: None,
            rules: rules.clone(),
        };

        apply_completed(
            &mut state,
            Ok(runner::Completed { session_id: "s-1".into(), outcome: Some(outcome), elapsed_secs: 3 }),
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
        );

        assert_eq!(session, None);
        assert_eq!(state.agent_steps.len(), 1);
        assert_eq!(state.error, None, "aborting is not a failure");
        assert_ne!(state.step, ProcessingStep::Complete);
    }
}
