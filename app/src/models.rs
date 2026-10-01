//! The state the agent run publishes to the UI.

use std::sync::{Arc, Mutex, PoisonError};

use dioxus::prelude::{ReadSignal, Signal};

// The run's cancellation flag and retry verdict are the controller's vocabulary,
// not the UI's — they live in `pipeline` and are re-exported here so components
// keep one import path for everything they render.
pub use pipeline::{AbortFlag, RetryAction};

/// A handle on one run's state, held by the tab and by the components that
/// render it.
///
/// Only the UI thread touches it. The run is driven on a worker thread, but it
/// reports through [`crate::agent_runner::ProgressSender`] and the tab's progress
/// task applies what arrives (see [`crate::agent_runner::show_progress`]). Two
/// things depend on keeping it that way:
///
/// - a run never waits for the UI: a slow or stuck render cannot stall a
///   conversion, because sending progress never blocks;
/// - a write cannot land between two reads of one render. This used to be a
///   sync signal, whose storage is a reader–writer lock that stops handing out
///   reads once a writer is waiting: a render that read the state twice while
///   the run's worker wrote it froze both threads for good — the freeze that
///   stopped long runs around the half-hour mark.
///
/// A plain, thread-local signal on purpose: the compiler refuses to move it
/// into a worker's task, so the state cannot quietly be handed back to the run.
pub type RunState = Signal<ProcessingState>;

/// A read-only view of [`RunState`], for the components that only render it.
pub type RunStateRead = ReadSignal<ProcessingState>;

/// The Retry / Give up answer to a paused run, handed from the button to the
/// run's thread.
///
/// Kept apart from [`RunState`] for the same reason progress travels over a
/// channel: the run polls this while paused, and must never have to reach into
/// the UI's state to do so. The mutex is held only for a set or a take, never across
/// anything else.
#[derive(Clone, Debug, Default)]
pub struct RetryAnswer(Arc<Mutex<Option<RetryAction>>>);

impl RetryAnswer {
    /// Record the user's answer for the paused run to pick up.
    pub fn answer(&self, action: RetryAction) {
        *self.cell() = Some(action);
    }

    /// The answer, if one has been given, consuming it.
    pub fn take(&self) -> Option<RetryAction> {
        self.cell().take()
    }

    /// Forget any answer, so a new prompt does not inherit an old click.
    pub fn clear(&self) {
        *self.cell() = None;
    }

    fn cell(&self) -> std::sync::MutexGuard<'_, Option<RetryAction>> {
        // A panic while holding this lock leaves only an `Option` behind,
        // which is always valid.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Two handles are the same answer when they share one cell, as with
/// [`AbortFlag`].
impl PartialEq for RetryAnswer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessingStep {
    #[default]
    Idle,
    /// The agent run is under way.
    Running,
    Complete,
}

/// Kind of an agent activity step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStepKind {
    /// The model's visible text for a turn.
    Thought,
    /// A tool call.
    Tool,
}

/// Status of an agent activity step (drives the spinner / checkmark).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStepStatus {
    Running,
    Done,
    Error,
}

impl AgentStepStatus {
    /// The glyph that stands for this status wherever a step is rendered — the
    /// timeline dots and the Markdown transcript alike, so the three views can
    /// never drift apart.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Running => "…",
            Self::Done => "✓",
            Self::Error => "✗",
        }
    }

    /// Modifier class for the timeline dot that carries this status.
    pub fn dot_class(self) -> &'static str {
        match self {
            Self::Running => "run",
            Self::Done => "ok",
            Self::Error => "err",
        }
    }
}

impl From<bool> for AgentStepStatus {
    /// A finished tool call is `Done` when it succeeded and `Error` when it did
    /// not; the agent loop reports exactly that boolean.
    fn from(ok: bool) -> Self {
        if ok { Self::Done } else { Self::Error }
    }
}

/// One entry in the Agent Processing activity panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStep {
    /// Tool-call id (for matching start→finish); empty for thoughts.
    pub id: String,
    pub kind: AgentStepKind,
    /// Tool name, or the thought text.
    pub label: String,
    /// Short input summary for tool steps.
    pub detail: String,
    pub status: AgentStepStatus,
}

// Only `PartialEq`: the run's spend carries a currency amount, and a float has
// no total equality.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessingState {
    pub step: ProcessingStep,
    /// What the finished run produced. Recorded here rather than read from the
    /// upload selector so the result panel always describes the run that
    /// actually happened.
    pub target: agent::OutputTarget,
    pub form_code: Option<String>,
    pub aem_package: Option<Vec<u8>>,
    /// The AEM package built with `bind_to_xsd` on, offered as its own download.
    pub aem_package_bound: Option<Vec<u8>>,
    pub xsd_schema: Option<String>,
    /// PostgreSQL dump for the Redacto platform (text-only documents).
    pub redacto_sql: Option<String>,
    pub error: Option<String>,
    /// The user stopped the run. Terminal like [`Self::error`], but not a
    /// failure — the box says so rather than reporting an error nobody hit.
    pub aborted: bool,
    /// `true` while an agent run is paused on a failed API turn, waiting for the
    /// user to press Retry (or give up). The run's future is still alive — the
    /// agent, its working tree and the stage history are all held in memory — so
    /// a retry resumes at the failed turn instead of restarting the run.
    pub retry_pending: bool,
    pub warnings: Vec<String>,
    /// Live activity log for the Agent Processing run (thoughts + tool calls).
    pub agent_steps: Vec<AgentStep>,
    /// Wall-clock duration of the most recent agent run, in seconds. Shown
    /// next to "Finished" on the agent "done" screen.
    pub elapsed_secs: Option<u64>,
    /// Latest real prompt-token count sent to the model this run (from the API's
    /// reported usage), for the context-window fill indicator. 0 before the first
    /// turn reports usage.
    pub context_used_tokens: usize,
    /// What the run has spent so far. `None` before the first turn reports
    /// usage, and it stays `None` for a run that never reached the model.
    pub spend: Option<pipeline::Spend>,
    /// The model's context window in tokens — the denominator of the fill
    /// indicator. 0 until the agent run sets it.
    pub context_window: usize,
}
