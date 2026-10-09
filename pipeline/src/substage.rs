//! How a stage runs the sub-stages it dispatches: the judges of its
//! `rule_check` ([`crate::judge`]). A sub-stage is a stage of its own on the
//! run's model and the run's one agent. It is one-shot (no stored conversation), it never
//! prompts the operator (one that fails gives up, and the dispatching tool
//! says so), and its spend is folded into the stage that dispatched it.

use std::sync::{Arc, Mutex};

use agent::Caller;
use rig_agent::agent::model::ModelHandle;

use crate::hooks::PriceFn;
use crate::memory::ContextBudget;
use crate::observer::{AbortFlag, RetryAction, RunEvent, RunObserver, SharedObserver, Spend};
use crate::roles::Role;
use crate::run::run_stage_as;
use crate::tools::SharedAgent;
use crate::trace::TraceEvent;

/// A model a stage runs on, with what goes with it: its price, its output cap
/// and the context budget sized from its window. The run's own model is one;
/// a role can be given another (see [`crate::RunConfig::reviewer_model`] and
/// [`crate::RunConfig::judge_model`]).
#[derive(Clone)]
pub struct StageModel {
    pub model: ModelHandle,
    pub price: PriceFn,
    pub max_tokens: u32,
    pub context_budget: Arc<dyn ContextBudget>,
}

/// What a sub-stage needs from the stage that dispatches it.
#[derive(Clone)]
pub(crate) struct SubStageContext {
    pub(crate) model: ModelHandle,
    pub(crate) price: PriceFn,
    pub(crate) max_tokens: u32,
    pub(crate) context_budget: Arc<dyn ContextBudget>,
    pub(crate) abort: AbortFlag,
    pub(crate) obs: SharedObserver,
    /// What the sub-stages spent, which the dispatching stage folds into the
    /// run's total when it ends. Shared because sub-stages run concurrently.
    pub(crate) spend: Arc<Mutex<Spend>>,
}

impl SubStageContext {
    /// A context whose sub-stages have spent nothing yet.
    pub(crate) fn new(
        model: ModelHandle,
        price: PriceFn,
        max_tokens: u32,
        context_budget: Arc<dyn ContextBudget>,
        abort: AbortFlag,
        obs: SharedObserver,
    ) -> Self {
        let spend = Arc::new(Mutex::new(Spend::default()));
        Self { model, price, max_tokens, context_budget, abort, obs, spend }
    }

    /// A context whose sub-stages run on `judge` rather than the stage's own
    /// model when the run gives judges a model of their own.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_stage(
        model: ModelHandle,
        price: PriceFn,
        max_tokens: u32,
        context_budget: Arc<dyn ContextBudget>,
        judge: Option<&StageModel>,
        abort: AbortFlag,
        obs: SharedObserver,
    ) -> Self {
        match judge {
            Some(j) => Self::new(j.model.clone(), j.price.clone(), j.max_tokens, j.context_budget.clone(), abort, obs),
            None => Self::new(model, price, max_tokens, context_budget, abort, obs),
        }
    }

    /// Folds what the sub-stages spent into the run's `total`, and reports the
    /// new total when they spent anything.
    pub(crate) fn fold_spend(&self, total: &mut Spend) {
        let spent = *self.spend.lock().unwrap_or_else(|p| p.into_inner());
        if spent.input_tokens + spent.output_tokens > 0 {
            total.merge(&spent);
            self.obs.emit(RunEvent::Spend(*total));
        }
    }
}

/// How one sub-stage ended, for the caller to read its result against.
pub(crate) struct SubStageEnd {
    /// The stage's final text: `None` when it was stopped before it ended.
    ended: Option<String>,
    /// The error it gave up on, when it failed.
    failure: Option<String>,
    /// The model turns it took.
    pub(crate) turns: usize,
    /// What it spent on its own (already folded into the context's spend).
    pub(crate) spend: Spend,
}

impl SubStageEnd {
    /// Why a `who` that left no `result` has none: it failed, was stopped,
    /// or ended without one.
    pub(crate) fn why_no(&self, who: &str, result: &str) -> String {
        match (&self.failure, &self.ended) {
            (Some(error), _) => format!("the {who} failed: {error}"),
            (None, None) => format!("the {who} was stopped before it gave {result}"),
            (None, Some(_)) => format!("the {who} ended without {result}"),
        }
    }
}

/// Runs one sub-stage of `role` as `caller`, its observer events labelled
/// with `label`, and folds its spend into `ctx`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sub_stage(
    agent: &SharedAgent,
    ctx: &SubStageContext,
    role: &'static Role,
    system: &str,
    seed: &str,
    caller: &Caller,
    label: String,
) -> SubStageEnd {
    let failure = Arc::new(Mutex::new(None));
    let turns = Arc::new(Mutex::new(0));
    let obs = SharedObserver::new(SubStageObserver {
        inner: ctx.obs.clone(),
        label,
        failure: failure.clone(),
        turns: turns.clone(),
    });
    let mut spend = Spend::default();
    let ended = run_stage_as(
        agent,
        role,
        system,
        seed,
        &ctx.abort,
        ctx.model.clone(),
        ctx.price.clone(),
        ctx.max_tokens,
        ctx.context_budget.clone(),
        &obs,
        &mut spend,
        caller,
        // A judge dispatches no judges of its own.
        None,
    )
    .await;
    ctx.spend.lock().unwrap_or_else(|p| p.into_inner()).merge(&spend);
    let failure = failure.lock().unwrap_or_else(|p| p.into_inner()).take();
    let turns = *turns.lock().unwrap_or_else(|p| p.into_inner());
    SubStageEnd { ended, failure, turns, spend }
}

/// What a sub-stage reports to the run's observer: its tool timeline and its
/// warnings and thoughts, labelled with what it was handed. Not its stage
/// header, its spend (cumulative per stage, so it would read as the run's
/// total; the dispatching stage folds it in instead) or its context fill, and
/// never a retry prompt: a sub-stage that fails permanently gives up. Nor its
/// trace: several run at once inside the dispatching stage's tool call, so
/// their stages would interleave with it; the dispatcher traces each one as a
/// whole instead (see `crate::judge`), from the turns kept here.
struct SubStageObserver {
    inner: SharedObserver,
    label: String,
    /// The error the sub-stage gave up on.
    failure: Arc<Mutex<Option<String>>>,
    /// The turns the sub-stage took, from its own `StageFinished`.
    turns: Arc<Mutex<usize>>,
}

impl RunObserver for SubStageObserver {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::Stage { .. } | RunEvent::Spend(_) | RunEvent::ContextUsed(_) => {}
            RunEvent::Thought(text) => self.inner.emit(RunEvent::Thought(format!("[{}] {text}", self.label))),
            RunEvent::Warning(text) => self.inner.emit(RunEvent::Warning(format!("[{}] {text}", self.label))),
            // Several sub-stages run at once: each call says whose it is.
            RunEvent::ToolStarted { id, name, input_summary } => self.inner.emit(RunEvent::ToolStarted {
                id,
                name,
                input_summary: format!("[{}] {input_summary}", self.label),
            }),
            other => self.inner.emit(other),
        }
    }

    fn retry_prompt(&mut self, _role: &str, error: &str) {
        *self.failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.to_string());
        self.inner.emit(RunEvent::Warning(format!("[{}] gave up after a failed turn: {error}", self.label)));
    }

    fn poll_retry(&mut self) -> Option<RetryAction> {
        Some(RetryAction::Cancel)
    }

    fn retry_resolved(&mut self, _action: RetryAction) {}

    fn trace(&mut self, event: TraceEvent) {
        if let TraceEvent::StageFinished { turns, .. } = event {
            *self.turns.lock().unwrap_or_else(|p| p.into_inner()) = turns;
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use rig_core::test_utils::MockCompletionModel;

    struct NoBudget;
    impl ContextBudget for NoBudget {
        fn policy(&self) -> Arc<dyn rig_memory::MemoryPolicy> {
            Arc::new(rig_memory::NoopMemoryPolicy)
        }
        fn raw_estimate(&self, _history: &[rig_core::message::Message]) -> usize {
            0
        }
        fn record_actual(&self, _raw_estimate: usize, _real_tokens: u64) {}
    }

    /// An agent without sources, holding `rules` as its judged ones.
    pub(crate) fn agent_with_judged(rules: Vec<agent::rules::JudgedRule>) -> SharedAgent {
        let mut agent = agent::ConversionAgent::new(None, Vec::new(), String::new())
            .expect("an agent without sources starts");
        agent.set_judged_rules(rules);
        Arc::new(tokio::sync::Mutex::new(agent))
    }

    /// `model` as a stage model of its own, priced at `per_input_token`.
    pub(crate) fn stage_model(model: MockCompletionModel, per_input_token: f64) -> crate::StageModel {
        crate::StageModel {
            model: ModelHandle::new(model),
            price: Arc::new(move |usage| Some(usage.input_tokens as f64 * per_input_token)),
            max_tokens: 1000,
            context_budget: Arc::new(NoBudget),
        }
    }

    /// A context on `model` whose sub-stages report to nobody.
    pub(crate) fn context(model: MockCompletionModel, abort: AbortFlag) -> SubStageContext {
        SubStageContext {
            model: ModelHandle::new(model),
            price: Arc::new(|usage| Some(usage.input_tokens as f64 * 0.01)),
            max_tokens: 1000,
            context_budget: Arc::new(NoBudget),
            abort,
            obs: SharedObserver::new(crate::observer::NullObserver),
            spend: Arc::default(),
        }
    }
}
