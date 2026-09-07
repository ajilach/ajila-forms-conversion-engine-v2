//! Hooks that reproduce, on rig's `Agent::runner`, the behaviours the
//! hand-rolled stage loop used to implement directly by driving `AgentRun`
//! itself: abort, tool observation, the stuck watchdog, `submit_review`
//! ending the stage, and the output-cap nudge.
//!
//! One hook holds every one of a stage's per-run concerns, rather than one
//! hook per concern: rig composes multiple hooks' *results* for a given
//! event, not their *state*, so splitting this up would still need the stuck
//! watch and the nudge counter threaded between the pieces by hand — the
//! composition would buy nothing a single struct's fields do not already
//! give for free.
//!
//! Built and tested against a real `Agent::runner` independently first
//! (rig's own `MockCompletionModel`, no network) before `run_stage` was wired
//! to construct one of these per stage — see `crate::run::run_stage`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rig_agent::agent::hook::{
    AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext, ModelTurnAction,
    ModelTurnFinished, ObservationAction, TextDelta, ToolCall, ToolCallAction, ToolCallDelta,
    ToolResultAction, ToolResultEvent,
};
use rig_agent::tool::ToolOutput;
use rig_core::completion::{FinishReason, Usage};
use rig_core::message::{AssistantContent, DocumentSourceKind, ToolResultContent};

use crate::observer::{AbortFlag, RunEvent, SharedObserver, Spend};
use crate::roles::{Role, MAX_MAX_TOKEN_NUDGES};
use crate::run::{summarize_input, LARGE_TOOL_REPLY_WARN_CHARS};

/// Prices one turn's usage in USD, when the model has a published rate.
///
/// A closure rather than a model id: `pipeline` carries no model tables (see
/// the crate's own turn-provider docs), so pricing stays the caller's
/// business — `runner`, which already knows the model id, supplies this as
/// `|usage| crate::pricing::cost_usd(&model_id, usage)`. Tests that do not
/// care about spend pass a closure that always returns `None`.
pub type PriceFn = Arc<dyn Fn(&Usage) -> Option<f64> + Send + Sync>;

/// The stage-scoped hook: one instance per `run_stage` call, added once to
/// that stage's `AgentRunner`.
///
/// Fields needing interior mutability (the stuck watch, the spend total, the
/// nudge counter) use plain `std::sync` primitives: every hook method here
/// does one quick, synchronous update and never awaits while holding one, and
/// `AgentHook`'s methods take `&self` — rig may hold one hook instance across
/// a whole run, shared with nothing else, but still through a shared
/// reference, not an owned one.
pub(crate) struct StageHook {
    role: &'static Role,
    abort: AbortFlag,
    obs: SharedObserver,
    price: PriceFn,
    stuck: Mutex<StuckWatch>,
    spend: Mutex<Spend>,
    consecutive_max_tokens: AtomicUsize,
    /// The full outgoing request — history plus the prompt about to be sent —
    /// captured on every attempted turn, successful or not.
    ///
    /// This is what makes retry-by-restart possible without hand-reassembling
    /// messages from stream deltas: a request that then fails still updated
    /// this *before* the send, so it is exactly "history plus the turn that
    /// failed" — precisely what re-sending the failed turn means. A
    /// `PromptCancelled`/`MaxTurnsError` stop hands the driver its own
    /// `chat_history` directly; this field exists for the one case that
    /// carries none at all, a bare `CompletionError`.
    last_attempt: Mutex<Vec<rig_core::message::Message>>,
    /// Model calls that actually completed in this attempt — as opposed to
    /// [`Self::last_attempt`]'s every *attempted* call, this counts only the
    /// ones that reached [`AgentHook::on_model_turn_finished`]. What the stage
    /// driver reduces `role.max_iterations` by before a restart, so an
    /// automatically-retried network failure does not eat into the stage's
    /// real turn budget the way a completed turn does.
    completed_turns: AtomicUsize,
    /// The latest non-empty assistant text seen — this attempt's answer to
    /// `run_stage`'s "last non-tool assistant message". A stage that ends via
    /// a hook stop or a bare `CompletionError` has no `PromptResponse` to read
    /// this from at all, so it is captured live instead, turn by turn.
    final_text: Mutex<String>,
}

impl StageHook {
    /// `starting_spend` carries the total from any earlier attempt this
    /// restarts — a fresh hook's own `Spend` would otherwise reset to zero on
    /// every restart, silently undercounting whatever the interrupted attempt
    /// already billed before it failed.
    pub(crate) fn new(
        role: &'static Role,
        abort: AbortFlag,
        obs: SharedObserver,
        price: PriceFn,
        starting_spend: Spend,
    ) -> Self {
        Self {
            stuck: Mutex::new(StuckWatch::new(role.stuck_tool)),
            role,
            abort,
            obs,
            price,
            spend: Mutex::new(starting_spend),
            consecutive_max_tokens: AtomicUsize::new(0),
            last_attempt: Mutex::new(Vec::new()),
            completed_turns: AtomicUsize::new(0),
            final_text: Mutex::new(String::new()),
        }
    }

    /// Stop the run the moment the flag is set, without waiting for the
    /// hook's caller to notice the return value — checked from both delta
    /// hooks so an abort during a tool-call argument stream stops just as
    /// fast as one during an ordinary text turn.
    fn stop_if_aborted(&self) -> ObservationAction {
        if self.abort.is_aborted() {
            ObservationAction::stop("aborted")
        } else {
            ObservationAction::continue_run()
        }
    }

    /// The history plus prompt of the most recently attempted turn — see
    /// [`Self::last_attempt`]. Read by the stage driver after a bare
    /// `CompletionError` to restart the run from exactly the point it failed.
    pub(crate) fn last_attempt(&self) -> Vec<rig_core::message::Message> {
        self.last_attempt
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// How many turns actually completed in this attempt — see
    /// [`Self::completed_turns`].
    pub(crate) fn completed_turns(&self) -> usize {
        self.completed_turns.load(Ordering::Relaxed)
    }

    /// The latest non-empty assistant text this attempt reported — see
    /// [`Self::final_text`].
    pub(crate) fn final_text(&self) -> String {
        self.final_text.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The cumulative spend so far, including whatever `starting_spend` this
    /// hook was seeded with. What the driver carries into the next hook on a
    /// restart — see [`Self::new`].
    pub(crate) fn spend(&self) -> Spend {
        *self.spend.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl AgentHook for StageHook {
    /// Checked per chunk, not per turn: a stage's turn can run for minutes,
    /// and a stop control that only takes effect at the end of one is not a
    /// stop control. This is the only hook granular enough for that — see
    /// the module docs on why the stage driver's own item-by-item loop
    /// cannot substitute for it.
    async fn on_text_delta(&self, _ctx: &HookContext, _event: TextDelta<'_>) -> ObservationAction {
        self.stop_if_aborted()
    }

    /// The same check, for a turn that is emitting tool-call arguments
    /// instead of text — a long `set_aem_translated` call streams no text at
    /// all, so without this an abort during one would wait for the whole
    /// call to finish streaming before it took effect.
    async fn on_tool_call_delta(
        &self,
        _ctx: &HookContext,
        _event: ToolCallDelta<'_>,
    ) -> ObservationAction {
        self.stop_if_aborted()
    }

    /// Records the exact request about to be sent — see
    /// [`StageHook::last_attempt`] — and otherwise leaves the request
    /// untouched. Fires before every attempted turn, including the driver's
    /// own restarts, so it always reflects the one currently in flight.
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: rig_agent::agent::hook::CompletionCall<'_>,
    ) -> rig_agent::agent::hook::CompletionCallAction {
        let mut attempt = event.history.to_vec();
        attempt.push(event.prompt.clone());
        *self.last_attempt.lock().unwrap_or_else(|p| p.into_inner()) = attempt;
        rig_agent::agent::hook::CompletionCallAction::continue_run()
    }

    /// Reports the call starting, for the activity timeline. Never rewrites
    /// or refuses the call — that policy question belongs to the catalog's
    /// own scoping, not to this hook.
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        let args: serde_json::Value = serde_json::from_str(event.args).unwrap_or_default();
        self.obs.emit(RunEvent::ToolStarted {
            id: event.internal_call_id.to_string(),
            name: event.tool_name.to_string(),
            input_summary: summarize_input(&args),
        });
        ToolCallAction::Run
    }

    /// Reports the call finishing, warns on an oversized text reply, watches
    /// for the stage going in circles, and ends the stage the moment
    /// `submit_review` reports its verdict — mirroring exactly what the
    /// hand-rolled loop did once a tool's result was in hand.
    async fn on_tool_result(&self, _ctx: &HookContext, event: ToolResultEvent<'_>) -> ToolResultAction {
        let output = event.presentation;
        self.obs.emit(RunEvent::ToolFinished {
            id: event.internal_call_id.to_string(),
            ok: event.raw_result.is_success(),
            reply_chars: output_total_chars(output),
        });
        let text_chars = output_text_chars(output);
        if text_chars > LARGE_TOOL_REPLY_WARN_CHARS {
            self.obs.emit(RunEvent::Warning(format!(
                "{} replied with {text_chars} characters of text — unusually large; this is \
                 what tends to blow the context window.",
                event.tool_name
            )));
        }

        // `submit_review` ends the stage once its verdict is recorded — the
        // stop is not a failure, so the driver reads this sentinel reason as
        // a normal end rather than an error.
        if event.tool_name == "submit_review" {
            return ToolResultAction::stop(SUBMIT_REVIEW_SENTINEL);
        }

        let mut stuck = self.stuck.lock().unwrap_or_else(|p| p.into_inner());
        if stuck.observe(event.tool_name, &output.render()) {
            return ToolResultAction::stop(STUCK_SENTINEL);
        }

        ToolResultAction::Keep
    }

    /// Reports usage/spend/context-fill for the completed turn, and nudges a
    /// turn cut off at the output-token cap to continue incrementally instead
    /// of ending the stage — mirroring exactly what the hand-rolled loop did
    /// once a turn was in hand.
    async fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        event: ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        self.completed_turns.fetch_add(1, Ordering::Relaxed);

        // The stage's visible text is reported live, turn by turn — not only
        // on the stage's last one — so the Analyst's step-by-step reasoning
        // shows up in the timeline as it happens, the way the hand-rolled
        // loop's own per-turn `RunEvent::Thought` did.
        let text: String = event
            .content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        let text = text.trim().to_string();
        if !text.is_empty() {
            *self.final_text.lock().unwrap_or_else(|p| p.into_inner()) = text.clone();
            self.obs.emit(RunEvent::Thought(text));
        }

        // What the API billed for the prompt is the only ground truth for how
        // full the window was; caching lowers the cost but not the occupancy,
        // so the cached buckets count too.
        let prompt_tokens = event.usage.input_tokens
            + event.usage.cached_input_tokens
            + event.usage.cache_creation_input_tokens;
        if prompt_tokens > 0 {
            self.obs.emit(RunEvent::ContextUsed(prompt_tokens as usize));
        }
        let cost = (self.price)(&event.usage);
        let spend = {
            let mut spend = self.spend.lock().unwrap_or_else(|p| p.into_inner());
            spend.add(&event.usage, cost);
            *spend
        };
        self.obs.emit(RunEvent::Spend(spend));

        let had_tool_calls = event
            .content
            .iter()
            .any(|c| matches!(c, AssistantContent::ToolCall(_)));

        // A turn cut off at the output-token cap didn't decide to stop —
        // nudge toward incremental authoring rather than ending the stage.
        // Only a tool-free turn can be retried this way; rig refuses to
        // retry one that carried tool calls, which already matches this
        // stage's own rule of only nudging text-only turns.
        if event.finish_reason == Some(&FinishReason::Length) && !had_tool_calls {
            let nudges = self.consecutive_max_tokens.load(Ordering::Relaxed);
            if nudges < MAX_MAX_TOKEN_NUDGES {
                self.consecutive_max_tokens.store(nudges + 1, Ordering::Relaxed);
                self.obs.emit(RunEvent::Thought(
                    "Turn hit the output-token limit — asking the agent to build the result \
                     incrementally instead of in one call."
                        .into(),
                ));
                return ModelTurnAction::retry_with_feedback(self.role.max_tokens_nudge);
            }
        }
        if had_tool_calls {
            self.consecutive_max_tokens.store(0, Ordering::Relaxed);
        }
        ModelTurnAction::continue_run()
    }

    /// A tool call the model invented, or reached for outside this stage's
    /// scope, is answered the way the hand-rolled loop always did: told, in
    /// its own transcript, that the call did not happen and why — not failed
    /// outright, which would end an otherwise-healthy stage over one ordinary
    /// model mistake.
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        let reason = format!(
            "Unknown tool: {}. It is not available to the {} at this stage. Use one of the \
             tools you were given.",
            event.tool_name, self.role.name
        );
        Some(InvalidToolCallAction::skip(reason))
    }
}

/// A cheap handle onto one `StageHook`, forwarding every method to it.
///
/// `AgentRunner::add_hook` takes its argument by value, so attaching a
/// `StageHook` directly would give up the only handle to it — and the stage
/// driver needs to read `last_attempt`/`completed_turns`/`final_text` back
/// out once the stream ends. `Arc<StageHook>` itself cannot implement a
/// foreign trait here (Rust's orphan rules: neither `Arc` nor `AgentHook` is
/// local to this crate), so this newtype is the local type the impl attaches
/// to; cloning it is just cloning the `Arc`.
#[derive(Clone)]
pub(crate) struct SharedHook(std::sync::Arc<StageHook>);

impl SharedHook {
    pub(crate) fn new(hook: StageHook) -> Self {
        Self(std::sync::Arc::new(hook))
    }
}

impl std::ops::Deref for SharedHook {
    type Target = StageHook;
    fn deref(&self) -> &StageHook {
        &self.0
    }
}

impl AgentHook for SharedHook {
    async fn on_text_delta(&self, ctx: &HookContext, event: TextDelta<'_>) -> ObservationAction {
        self.0.on_text_delta(ctx, event).await
    }

    async fn on_tool_call_delta(
        &self,
        ctx: &HookContext,
        event: ToolCallDelta<'_>,
    ) -> ObservationAction {
        self.0.on_tool_call_delta(ctx, event).await
    }

    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        event: rig_agent::agent::hook::CompletionCall<'_>,
    ) -> rig_agent::agent::hook::CompletionCallAction {
        self.0.on_completion_call(ctx, event).await
    }

    async fn on_tool_call(&self, ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        self.0.on_tool_call(ctx, event).await
    }

    async fn on_tool_result(&self, ctx: &HookContext, event: ToolResultEvent<'_>) -> ToolResultAction {
        self.0.on_tool_result(ctx, event).await
    }

    async fn on_model_turn_finished(
        &self,
        ctx: &HookContext,
        event: ModelTurnFinished<'_>,
    ) -> ModelTurnAction {
        self.0.on_model_turn_finished(ctx, event).await
    }

    async fn on_invalid_tool_call(
        &self,
        ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        self.0.on_invalid_tool_call(ctx, event).await
    }
}

/// [`ToolResultAction::Stop`] reasons the stage driver reads back as a normal
/// end rather than a failure — both are how the hand-rolled loop's `break`
/// used to end a stage, and a stop is the only way rig can end one at all
/// (see the module docs on `PromptError::PromptCancelled`).
pub(crate) const SUBMIT_REVIEW_SENTINEL: &str = "submit_review recorded a verdict";
pub(crate) const STUCK_SENTINEL: &str = "stuck: repeated identical result";

/// Characters of *text* content in a tool's presentation — the [`ToolOutput`]
/// analogue of `pipeline::run::text_reply_chars`, for the shape hooks see
/// rather than the agent's own `ToolReply`.
///
/// An image's base64 payload is excluded on purpose: its real cost is the
/// vision encoder's, already bounded by the page-render clamp regardless of
/// how long the base64 string is, unlike the same bytes tokenized as text.
fn output_text_chars(output: &ToolOutput) -> usize {
    output
        .as_content()
        .iter()
        .map(|c| match c {
            ToolResultContent::Text(t) => t.text.len(),
            ToolResultContent::Json { value } => value.to_string().len(),
            ToolResultContent::Image(_) => 0,
        })
        .sum()
}

/// Characters in a tool's presentation — text plus every image's base64
/// payload — the [`ToolOutput`] analogue of `pipeline::run::reply_size_chars`.
fn output_total_chars(output: &ToolOutput) -> usize {
    output
        .as_content()
        .iter()
        .map(|c| match c {
            ToolResultContent::Text(t) => t.text.len(),
            ToolResultContent::Json { value } => value.to_string().len(),
            ToolResultContent::Image(image) => match &image.data {
                DocumentSourceKind::Base64(data) => data.len(),
                DocumentSourceKind::Url(_)
                | DocumentSourceKind::FileId(_)
                | DocumentSourceKind::Raw(_)
                | DocumentSourceKind::String(_)
                | DocumentSourceKind::Unknown => 0,
            },
        })
        .sum()
}

/// Watches one tool for "same answer, again and again" — the [`ToolOutput`]
/// analogue of `pipeline::run::StuckWatch`, hashing a rendered string instead
/// of matching over `ToolReply`'s variants, since a hook only ever sees the
/// presentation rig already converted a reply into.
struct StuckWatch {
    tool: Option<&'static str>,
    last: Option<u64>,
    repeats: usize,
}

impl StuckWatch {
    fn new(tool: Option<&'static str>) -> Self {
        Self {
            tool,
            last: None,
            repeats: 0,
        }
    }

    /// Record one tool result's rendering; `true` once the watched tool has
    /// produced the same rendering [`crate::roles::MAX_VALIDATE_REPEATS`]
    /// times running. Any other tool means the stage is making progress and
    /// resets the count.
    fn observe(&mut self, name: &str, rendered: &str) -> bool {
        if self.tool != Some(name) {
            self.last = None;
            self.repeats = 0;
            return false;
        }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        rendered.hash(&mut hasher);
        let digest = hasher.finish();

        if self.last == Some(digest) {
            self.repeats += 1;
        } else {
            self.last = Some(digest);
            self.repeats = 1;
        }
        self.repeats >= crate::roles::MAX_VALIDATE_REPEATS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use rig_agent::agent::{Agent, AgentBuilder};
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    fn no_price() -> PriceFn {
        Arc::new(|_| None)
    }

    /// A role with a chosen `stuck_tool`, for the stuck-watch test — the
    /// built-in roles' own stuck tools (`validate_aem_package`,
    /// `build_redacto_dump`) are not reachable from a bare agent with no
    /// sources.
    const STUCK_ON_GET_SOURCE_INFO: Role = Role {
        name: "Test",
        scope: agent::scope::AEM_ANALYST,
        max_iterations: 10,
        stuck_tool: Some("get_source_info"),
        stuck_activity: "testing",
        max_tokens_nudge: "nudge incrementally",
    };

    fn agent_with(model: MockCompletionModel) -> Agent {
        AgentBuilder::new(model).build()
    }

    fn agent_with_tools(
        model: MockCompletionModel,
        tools: Vec<rig_agent::tool::DynamicTool>,
    ) -> Agent {
        let mut iter = tools.into_iter();
        match iter.next() {
            None => AgentBuilder::new(model).build(),
            Some(first) => {
                let mut builder = AgentBuilder::new(model).dynamic_tool(first);
                for tool in iter {
                    builder = builder.dynamic_tool(tool);
                }
                builder.build()
            }
        }
    }

    fn bare_shared_agent() -> crate::tools::SharedAgent {
        std::sync::Arc::new(tokio::sync::Mutex::new(agent::ConversionAgent::new(
            None,
            Vec::new(),
            None,
            "test-hooks".into(),
            blueprint::OutputTarget::Redacto,
        )))
    }

    /// Every `RunEvent` a test's `SharedObserver` recorded, for asserting on
    /// what a hook actually reported.
    fn recorder() -> (SharedObserver, std::sync::Arc<Mutex<Vec<RunEvent>>>) {
        struct Logging(std::sync::Arc<Mutex<Vec<RunEvent>>>);
        impl crate::observer::RunObserver for Logging {
            fn emit(&mut self, event: RunEvent) {
                self.0.lock().unwrap().push(event);
            }
            fn retry_prompt(&mut self, _role: &str, _error: &str) {}
            fn poll_retry(&mut self) -> Option<crate::observer::RetryAction> {
                None
            }
            fn retry_resolved(&mut self, _action: crate::observer::RetryAction) {}
        }
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        (SharedObserver::new(Logging(log.clone())), log)
    }

    /// Drive a stream to completion (or a stop), discarding items — tests
    /// that only care about what the hook reported on the side, not the
    /// stream's own outcome.
    async fn drain(mut stream: rig_agent::agent::StreamingResult) {
        while stream.next().await.is_some() {}
    }

    /// An abort mid-stream must stop the run before the scripted turn even
    /// finishes — proven by scripting a turn the run would otherwise
    /// complete normally, aborting before driving it, and confirming the
    /// stream ends in an error rather than a final response.
    #[tokio::test]
    async fn an_aborted_run_stops_mid_stream_rather_than_finishing_the_turn() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("hello"),
            MockStreamEvent::final_response(rig_core::completion::Usage::new()),
        ]]);
        let agent = agent_with(model);
        let abort = AbortFlag::default();
        abort.abort();

        let obs = SharedObserver::new(crate::observer::NullObserver);
        let hook = StageHook::new(&crate::roles::ANALYST, abort, obs, no_price(), Spend::default());

        let mut stream = agent.runner("go").add_hook(hook).max_turns(5).stream().await;
        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            if item.is_err() {
                saw_error = true;
                break;
            }
        }
        assert!(saw_error, "an aborted run must not reach a final response");
    }

    /// The mirror case: an untouched flag must not interfere with an
    /// otherwise-normal run reaching its final response.
    #[tokio::test]
    async fn an_unaborted_run_reaches_its_final_response() {
        use rig_agent::agent::MultiTurnStreamItem;

        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("hello"),
            MockStreamEvent::final_response(rig_core::completion::Usage::new()),
        ]]);
        let agent = agent_with(model);
        let hook = StageHook::new(
            &crate::roles::ANALYST,
            AbortFlag::default(),
            SharedObserver::new(crate::observer::NullObserver),
            no_price(),
            Spend::default(),
        );

        let mut stream = agent.runner("go").add_hook(hook).max_turns(5).stream().await;
        let mut reached_final = false;
        while let Some(item) = stream.next().await {
            let item = item.expect("an unaborted run must not error");
            if matches!(item, MultiTurnStreamItem::FinalResponse(_)) {
                reached_final = true;
            }
        }
        assert!(reached_final);
    }

    /// A tool call reports both its start and its finish, on the real agent —
    /// this is what feeds the app's activity timeline.
    #[tokio::test]
    async fn a_tool_call_reports_start_and_finish() {
        let shared_agent = bare_shared_agent();
        let (obs, log) = recorder();
        let tools = crate::tools::dynamic_tools_for(
            &shared_agent,
            &[serde_json::json!({
                "name": "get_source_info",
                "description": "Info about the source.",
                "input_schema": {"type": "object", "properties": {}},
            })],
            &obs,
        );

        let model = MockCompletionModel::from_stream_turns([
            [
                MockStreamEvent::tool_call("call-1", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ],
            [
                MockStreamEvent::text("done"),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ],
        ]);
        let agent = agent_with_tools(model, tools);
        let hook = StageHook::new(&crate::roles::ANALYST, AbortFlag::default(), obs, no_price(), Spend::default());

        drain(agent.runner("go").add_hook(hook).max_turns(5).stream().await).await;

        let events = log.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RunEvent::ToolStarted { name, .. } if name == "get_source_info")),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RunEvent::ToolFinished { ok: true, .. })),
            "{events:?}"
        );
    }

    /// `submit_review` ends the stage the moment its verdict is recorded —
    /// the model must not get a further turn afterward.
    #[tokio::test]
    async fn submit_review_ends_the_stage() {
        let shared_agent = bare_shared_agent();
        let tools = crate::tools::dynamic_tools_for(
            &shared_agent,
            &[serde_json::json!({
                "name": "submit_review",
                "description": "Record the review verdict.",
                "input_schema": {"type": "object", "properties": {}},
            })],
            &SharedObserver::new(crate::observer::NullObserver),
        );

        let model = MockCompletionModel::from_stream_turns([
            [
                MockStreamEvent::tool_call(
                    "call-1",
                    "submit_review",
                    serde_json::json!({"approved": true, "report": ""}),
                ),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ],
            // A second turn the run must never reach.
            [
                MockStreamEvent::text("should not run"),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ],
        ]);
        let agent = agent_with_tools(model.clone(), tools);
        let hook = StageHook::new(
            &crate::roles::ANALYST,
            AbortFlag::default(),
            SharedObserver::new(crate::observer::NullObserver),
            no_price(),
            Spend::default(),
        );

        drain(agent.runner("go").add_hook(hook).max_turns(5).stream().await).await;

        assert_eq!(
            model.request_count(),
            1,
            "the stage must stop right after submit_review, not take a second turn"
        );
    }

    /// The stuck watch ends the stage once the watched tool repeats its
    /// output — otherwise a stage burns its whole turn budget re-validating
    /// an unchanged tree.
    #[tokio::test]
    async fn the_stuck_watch_ends_a_stage_that_stopped_making_progress() {
        let shared_agent = bare_shared_agent();
        let tools = crate::tools::dynamic_tools_for(
            &shared_agent,
            &[serde_json::json!({
                "name": "get_source_info",
                "description": "Info about the source.",
                "input_schema": {"type": "object", "properties": {}},
            })],
            &SharedObserver::new(crate::observer::NullObserver),
        );

        // The same tool call, repeated: `get_source_info` on an unchanged
        // agent always answers identically, so every call after the first is
        // a repeat of the same rendering.
        let repeat_turn = || {
            vec![
                MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ]
        };
        let model = MockCompletionModel::from_stream_turns(
            std::iter::repeat_with(repeat_turn).take(10),
        );
        let agent = agent_with_tools(model.clone(), tools);
        let hook = StageHook::new(
            &STUCK_ON_GET_SOURCE_INFO,
            AbortFlag::default(),
            SharedObserver::new(crate::observer::NullObserver),
            no_price(),
            Spend::default(),
        );

        drain(
            agent
                .runner("go")
                .add_hook(hook)
                .max_turns(10)
                .stream()
                .await,
        )
        .await;

        assert_eq!(
            model.request_count(),
            crate::roles::MAX_VALIDATE_REPEATS,
            "the stage must stop as soon as the repeat threshold is reached, not run to the \
             turn budget"
        );
    }

    /// A turn's visible text has to reach the timeline as it happens, not only
    /// once the stage ends — the Analyst's step-by-step reasoning is the whole
    /// point of watching a run live, and `PromptResponse::output` only exists
    /// once the run is over (and never at all for a stage that stops early).
    #[tokio::test]
    async fn a_turns_visible_text_reaches_the_timeline_as_a_thought() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("Reading the source form."),
            MockStreamEvent::final_response(rig_core::completion::Usage::new()),
        ]]);
        let agent = agent_with(model);
        let (obs, log) = recorder();
        let hook = StageHook::new(&crate::roles::ANALYST, AbortFlag::default(), obs, no_price(), Spend::default());

        drain(agent.runner("go").add_hook(hook).max_turns(5).stream().await).await;

        let events = log.lock().unwrap();
        assert!(
            events.iter().any(
                |e| matches!(e, RunEvent::Thought(text) if text == "Reading the source form.")
            ),
            "{events:?}"
        );
    }

    /// A completed turn's usage drives both the context-fill gauge and the
    /// cumulative spend line — the two things the app's timeline reads live.
    #[tokio::test]
    async fn a_completed_turn_reports_context_use_and_spend() {
        let mut usage = rig_core::completion::Usage::new();
        usage.input_tokens = 1000;
        usage.output_tokens = 50;
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("hello"),
            MockStreamEvent::final_response(usage),
        ]]);
        let agent = agent_with(model);
        let (obs, log) = recorder();
        let price: PriceFn = Arc::new(|usage| Some(usage.input_tokens as f64 * 0.001));
        let hook = StageHook::new(&crate::roles::ANALYST, AbortFlag::default(), obs, price, Spend::default());

        drain(agent.runner("go").add_hook(hook).max_turns(5).stream().await).await;

        let events = log.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RunEvent::ContextUsed(tokens) if *tokens == 1000)),
            "{events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                RunEvent::Spend(spend) if spend.input_tokens == 1000 && spend.cost_usd == Some(1.0)
            )),
            "{events:?}"
        );
    }

    /// A turn cut off at the output-token cap is nudged to continue
    /// incrementally, up to the configured budget — then the run accepts the
    /// truncated turn rather than nudging forever.
    #[tokio::test]
    async fn a_truncated_turn_is_nudged_up_to_the_budget_then_accepted() {
        use rig_core::completion::FinishReason;
        use rig_core::streaming::StreamFinal;

        let truncated_turn = || {
            vec![
                MockStreamEvent::text("partial…"),
                MockStreamEvent::FinalResponse(
                    StreamFinal::new(rig_core::test_utils::MOCK_PROVIDER, rig_core::completion::Usage::new())
                        .with_finish_reason(FinishReason::Length),
                ),
            ]
        };
        // One more scripted turn than the nudge budget allows, so the test
        // fails loudly (a script-exhaustion panic) if the hook nudges even
        // once past its own limit.
        let model = MockCompletionModel::from_stream_turns(
            std::iter::repeat_with(truncated_turn).take(MAX_MAX_TOKEN_NUDGES + 1),
        );
        let agent = agent_with(model.clone());
        let hook = StageHook::new(
            &crate::roles::ANALYST,
            AbortFlag::default(),
            SharedObserver::new(crate::observer::NullObserver),
            no_price(),
            Spend::default(),
        );

        drain(
            agent
                .runner("go")
                .add_hook(hook)
                .max_turns(MAX_MAX_TOKEN_NUDGES + 2)
                .stream()
                .await,
        )
        .await;

        assert_eq!(
            model.request_count(),
            MAX_MAX_TOKEN_NUDGES + 1,
            "the initial attempt plus every nudge, then the run must accept the result rather \
             than nudging again"
        );
    }

    /// A tool call outside the stage's registered set is skipped with an
    /// explanation, not failed outright — an ordinary model mistake must not
    /// end an otherwise-healthy stage.
    #[tokio::test]
    async fn a_tool_call_outside_the_registered_set_is_skipped_not_fatal() {
        // No tools registered at all, so any tool call the model makes is
        // unknown to this stage.
        let model = MockCompletionModel::from_stream_turns([
            [
                MockStreamEvent::tool_call(
                    "call-1",
                    "not_a_real_tool",
                    serde_json::json!({}),
                ),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ],
            [
                MockStreamEvent::text("recovered"),
                MockStreamEvent::final_response(rig_core::completion::Usage::new()),
            ],
        ]);
        let agent = agent_with(model.clone());
        let hook = StageHook::new(
            &crate::roles::ANALYST,
            AbortFlag::default(),
            SharedObserver::new(crate::observer::NullObserver),
            no_price(),
            Spend::default(),
        );

        drain(agent.runner("go").add_hook(hook).max_turns(5).stream().await).await;

        assert_eq!(
            model.request_count(),
            2,
            "the stage must recover and take its second scripted turn, not end on the \
             unknown tool"
        );
    }
}
