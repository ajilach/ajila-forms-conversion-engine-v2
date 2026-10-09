//! The run's structured trace: what every stage, attempt, model turn and tool
//! call did, with the durations the controller measured and the full content
//! it saw.
//!
//! [`crate::RunEvent`] is the progress feed a UI renders — deliberately terse
//! (a tool call is a name and a 120-character summary). This is the other
//! feed: everything needed to analyse a run after the fact — where the time
//! went, which calls repeated, which errors recurred, what the Reviewer found
//! — reported through [`crate::RunObserver::trace`], whose default does
//! nothing, so an observer that does not record (the app's UI state, a test
//! recorder, [`crate::NullObserver`]) never has to know this exists.
//!
//! Events carry no wall-clock time of their own: the recorder stamps each one
//! on arrival, which is the same instant to well within a millisecond, and a
//! `pipeline` that never reads the clock stays trivially testable. Durations,
//! which a recorder could only reconstruct by pairing events, are measured
//! here with a monotonic clock and carried on the finishing event.

use serde::Serialize;

use crate::observer::Spend;

/// One tool call as the model asked for it, arguments in full.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TracedToolCall {
    pub call_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// Token use of a single model turn — the per-turn counterpart of the
/// cumulative [`Spend`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

impl TurnUsage {
    pub fn from_usage(usage: &rig_core::completion::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        }
    }

    /// Everything the request occupied in the context window — cached or not,
    /// the tokens were in the prompt.
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens + self.cached_input_tokens + self.cache_write_tokens
    }
}

/// How a stage came to an end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageEnd {
    /// The model gave its final answer on its own.
    Finished,
    /// A terminal tool recorded the stage's result and ended it: the
    /// Reviewer's `submit_review`, and also the Author's `finish_authoring`
    /// (the name predates it, and is kept for the files already recorded).
    /// The run recorder tells the two apart by the call that ended the stage.
    ReviewSubmitted,
    /// The stuck watch ended it: the watched tool kept returning the same result.
    Stuck,
    /// It used its whole turn budget without finishing.
    TurnBudgetExhausted,
    /// The operator (or Ctrl-C) stopped the run.
    Aborted,
    /// A request failed and the operator gave up at the retry prompt.
    GaveUp,
    /// Any other error the stage ended on; the text is the error.
    Error(String),
}

impl StageEnd {
    /// A short phrase for reports.
    pub fn describe(&self) -> String {
        match self {
            Self::Finished => "finished on its own".into(),
            Self::ReviewSubmitted => "ended by its terminal tool".into(),
            Self::Stuck => "stopped by the stuck watch".into(),
            Self::TurnBudgetExhausted => "ran out of turns".into(),
            Self::Aborted => "aborted".into(),
            Self::GaveUp => "gave up after a failed request".into(),
            Self::Error(e) => format!("ended on an error: {e}"),
        }
    }
}

/// Something the controller did to steer a stage, rather than the model doing
/// work — the events that show where a run fought itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    /// A turn hit the output-token cap and the model was asked to continue
    /// incrementally.
    OutputCapNudge,
    /// The stuck watch saw the same result too many times and ended the stage.
    StuckStop,
    /// The stage used its whole turn budget.
    TurnBudgetExhausted,
    /// A request failed transiently and is retried automatically after a wait.
    TransientRetry,
    /// A request failed and the run paused for the operator's decision.
    OperatorPrompt,
    /// The operator chose to retry.
    OperatorRetried,
    /// The operator chose to give up.
    OperatorCancelled,
    /// The model called a tool that does not exist or is not offered here.
    InvalidToolCall,
    /// The context budget could not shape a turn's history; it went unshaped.
    ContextBudgetFailed,
    /// The stop control was used.
    Aborted,
    /// The stage ended on an error that is neither of the above.
    StageError,
}

impl ControlKind {
    /// A short, stable label for tables.
    pub fn label(self) -> &'static str {
        match self {
            Self::OutputCapNudge => "output-cap nudge",
            Self::StuckStop => "stuck-watch stop",
            Self::TurnBudgetExhausted => "turn budget exhausted",
            Self::TransientRetry => "automatic retry",
            Self::OperatorPrompt => "paused for operator",
            Self::OperatorRetried => "operator retried",
            Self::OperatorCancelled => "operator gave up",
            Self::InvalidToolCall => "invalid tool call",
            Self::ContextBudgetFailed => "context budget failed",
            Self::Aborted => "aborted",
            Self::StageError => "stage error",
        }
    }
}

/// What a judge decided about its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeVerdict {
    /// The document keeps the rule.
    Positive,
    /// The document breaks the rule.
    Negative,
    /// No verdict: the judge failed, was stopped, ended without one, or the
    /// document changed while it judged.
    Unchecked,
}

impl JudgeVerdict {
    /// A short, stable label for tables.
    pub fn label(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Unchecked => "unchecked",
        }
    }
}

/// One entry of the run's trace. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TraceEvent {
    /// A stage began: its full prompt, and what it may use.
    StageStarted {
        stage: String,
        system_prompt: String,
        seed_message: String,
        max_turns: usize,
        tools_offered: Vec<String>,
    },
    /// A fresh `AgentRunner` started for the stage: the first attempt, or a
    /// restart after a failed request (from `history_messages` of history).
    AttemptStarted {
        stage: String,
        attempt: usize,
        history_messages: usize,
    },
    /// A request is about to be sent. `sent_messages` is what the context
    /// budget left of `history_messages`; `estimated_tokens` is its raw
    /// (uncalibrated) estimate of that request; `shaping_ms` is how long the
    /// shaping took (it can decode images), which is not part of the turn's
    /// model latency.
    TurnStarted {
        stage: String,
        attempt: usize,
        turn: usize,
        history_messages: usize,
        sent_messages: usize,
        estimated_tokens: usize,
        shaping_ms: u64,
    },
    /// A request failed before the model answered — a transport error, an
    /// overloaded or rate-limited API, a rejected request. `latency_ms` is how
    /// long it took to fail; what happens next (an automatic retry, the
    /// operator's decision) follows as a [`TraceEvent::Control`].
    RequestFailed {
        stage: String,
        attempt: usize,
        turn: usize,
        latency_ms: u64,
        error: String,
    },
    /// A model turn completed.
    TurnFinished {
        stage: String,
        attempt: usize,
        turn: usize,
        latency_ms: u64,
        finish_reason: Option<String>,
        usage: TurnUsage,
        cost_usd: Option<f64>,
        /// The turn's visible text.
        text: String,
        /// The turn's readable reasoning, when the provider returned any.
        reasoning: String,
        tool_calls: Vec<TracedToolCall>,
    },
    /// A tool began executing.
    ToolStarted {
        stage: String,
        attempt: usize,
        turn: usize,
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A tool finished. `result` is the full text the model received, with
    /// each image replaced by a one-line placeholder (`image_count` of them,
    /// `image_chars` base64 characters in total).
    ToolFinished {
        stage: String,
        attempt: usize,
        turn: usize,
        call_id: String,
        name: String,
        ok: bool,
        duration_ms: u64,
        result: String,
        result_chars: usize,
        image_count: usize,
        image_chars: usize,
        /// A digest of the full presentation (images included), so identical
        /// results can be recognised without comparing megabytes of text.
        result_hash: String,
    },
    /// The Reviewer's verdict for one review round. `approved` is `None`
    /// when the Reviewer ended without calling `submit_review` (its budget
    /// ran out, or the stuck watch stopped it).
    ReviewVerdict {
        stage: String,
        round: usize,
        approved: Option<bool>,
        report: String,
    },
    /// The controller steered the stage. See [`ControlKind`].
    Control {
        stage: String,
        kind: ControlKind,
        detail: String,
    },
    /// A `rule_check` ran its scripts and is about to hand its judged rules
    /// to judges. `stage` is the judges' stage name; the stage that called
    /// `rule_check` is the one the recorder files the line under. `check`
    /// numbers the checks of the process, so a judge's line finds its check;
    /// `partial` is a check of chosen `rule_ids` rather than of every rule.
    RuleCheckStarted {
        stage: String,
        check: u64,
        partial: bool,
        scripted: usize,
        judged: usize,
        /// The document revision the check runs on.
        revision: u64,
    },
    /// One judge ended: its rule, its own verdict on `revision` (the
    /// document it judged), and what it took. `spend` is the judge's own,
    /// which the calling stage's spend also includes. When the check ends
    /// `outdated`, it reports the rule unchecked whatever this says.
    JudgeFinished {
        stage: String,
        check: u64,
        rule_id: String,
        /// The `rule.toml` id.
        rule_name: String,
        rule_title: String,
        verdict: JudgeVerdict,
        violations: usize,
        /// Why there is no verdict, when there is none.
        unchecked_reason: Option<String>,
        turns: usize,
        duration_ms: u64,
        revision: u64,
        spend: Spend,
    },
    /// A `rule_check`'s judges all ended. `judges_spend` is what they spent
    /// together; `outdated` says the document changed while they judged, so
    /// every judged rule came back unchecked.
    RuleCheckFinished {
        stage: String,
        check: u64,
        duration_ms: u64,
        judges_spend: Spend,
        outdated: bool,
    },
    /// A stage ended. `spend` is this stage's own share, not the run's total:
    /// the judges its `rule_check`s dispatched included.
    StageFinished {
        stage: String,
        ended: StageEnd,
        turns: usize,
        attempts: usize,
        duration_ms: u64,
        spend: Spend,
    },
}

impl TraceEvent {
    /// The stage the event belongs to.
    pub fn stage(&self) -> &str {
        match self {
            Self::StageStarted { stage, .. }
            | Self::AttemptStarted { stage, .. }
            | Self::TurnStarted { stage, .. }
            | Self::TurnFinished { stage, .. }
            | Self::ToolStarted { stage, .. }
            | Self::ToolFinished { stage, .. }
            | Self::RequestFailed { stage, .. }
            | Self::ReviewVerdict { stage, .. }
            | Self::Control { stage, .. }
            | Self::RuleCheckStarted { stage, .. }
            | Self::JudgeFinished { stage, .. }
            | Self::RuleCheckFinished { stage, .. }
            | Self::StageFinished { stage, .. } => stage,
        }
    }
}

/// Milliseconds since `started`, saturating — the one conversion every
/// duration in the trace goes through.
pub fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A stable hex digest of `text` — what [`TraceEvent::ToolFinished::result_hash`]
/// holds. FNV-1a rather than `DefaultHasher`, whose output is explicitly not
/// stable across Rust releases: a digest in a file compared across runs has to
/// mean the same thing next month.
pub fn stable_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The difference between two cumulative spends — one stage's share, given
/// the run's total before and after it, or a stage's own share given its
/// judges' (`before`) and its whole (`after`).
pub fn spend_between(before: &Spend, after: &Spend) -> Spend {
    Spend {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        cached_input_tokens: after
            .cached_input_tokens
            .saturating_sub(before.cached_input_tokens),
        cache_write_tokens: after
            .cache_write_tokens
            .saturating_sub(before.cache_write_tokens),
        reasoning_tokens: after.reasoning_tokens.saturating_sub(before.reasoning_tokens),
        cost_usd: match (before.cost_usd, after.cost_usd) {
            (Some(b), Some(a)) => Some((a - b).max(0.0)),
            (None, Some(a)) => Some(a),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is compared across runs, so it must not depend on the Rust
    /// release or the process — pinned to known FNV-1a values.
    #[test]
    fn the_stable_hash_is_fnv1a() {
        assert_eq!(stable_hash(""), "cbf29ce484222325");
        assert_eq!(stable_hash("a"), "af63dc4c8601ec8c");
        assert_ne!(stable_hash("same"), stable_hash("same "));
    }

    #[test]
    fn a_stages_share_is_the_difference_of_two_totals() {
        let before = Spend {
            input_tokens: 100,
            cost_usd: Some(1.0),
            ..Spend::default()
        };
        let after = Spend {
            input_tokens: 150,
            output_tokens: 7,
            cost_usd: Some(1.5),
            ..Spend::default()
        };
        let share = spend_between(&before, &after);
        assert_eq!(share.input_tokens, 50);
        assert_eq!(share.output_tokens, 7);
        assert!((share.cost_usd.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(spend_between(&Spend::default(), &Spend::default()).cost_usd, None);
    }

    /// The JSON shape is what scripts and the report read; the tag and the
    /// snake_case names are part of that contract.
    #[test]
    fn events_serialize_with_an_event_tag() {
        let event = TraceEvent::Control {
            stage: "Author".into(),
            kind: ControlKind::OutputCapNudge,
            detail: "x".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "control");
        assert_eq!(json["kind"], "output_cap_nudge");
        let end = serde_json::to_value(StageEnd::Error("boom".into())).unwrap();
        assert_eq!(end, serde_json::json!({"error": "boom"}));
    }
}
