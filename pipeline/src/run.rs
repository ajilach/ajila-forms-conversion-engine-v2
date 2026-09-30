//! The conversion controller: Analyst → Author → (Reviewer → Author-fix)* →
//! finalize, sequenced over one shared [`ConversionAgent`].
//!
//! Each stage runs as a real rig [`Agent`], driven through
//! [`AgentRunner::stream`]: [`crate::hooks::StageHook`] is what reproduces the
//! abort flag, the tool timeline, the stuck watchdog and the output-cap
//! nudge, and this module is what reproduces the operator's retry prompt — by
//! restarting a failed attempt from exactly the point it failed, rather than
//! resuming the same `AgentRun` in place, which rig's own execution model does
//! not expose a seam for (see [`run_stage`]'s docs). Progress and retry
//! decisions still cross one boundary, [`RunObserver`], so the sequencing is
//! drivable from a test with no network and no desktop runtime — now against
//! rig's own [`rig_core::test_utils::MockCompletionModel`] rather than a
//! hand-rolled turn provider.

use std::sync::Arc;

use agent::ToolReply;
use agent::OutputTarget;
use rig_agent::agent::model::ModelHandle;
use rig_agent::agent::{Agent, AgentBuilder, MultiTurnStreamItem, StreamingError};
use rig_agent::completion::PromptError;
use rig_core::completion::CompletionError;
use rig_core::memory::ConversationMemory;
use rig_core::message::{
    AssistantContent, Message, ProviderCallId, ToolCallId, ToolResultContent, UserContent,
};
use rig_memory::{CompactingMemory, MemoryPolicy, TemplateCompactor};

use crate::hooks::{PriceFn, SharedHook, StageHook};
use crate::memory::{self, ContextBudget, SqliteConversationMemory};
use crate::observer::{AbortFlag, RetryAction, RunEvent, SharedObserver, Spend};
use crate::roles::{
    self, MAX_AUTO_RETRIES, MAX_RETRY_BACKOFF_SECS, MAX_VALIDATE_REPEATS, RETRY_POLL_MS, Role,
    RETRY_BACKOFF_SECS,
};
use crate::tools::{self, SharedAgent};
use crate::trace::{self, ControlKind, StageEnd, TraceEvent};

/// The choices that shape a run, independent of who is driving it.
pub struct RunConfig {
    pub profile: Option<String>,
    pub target: OutputTarget,
    /// Set by the caller's stop control to end this run at its next checkpoint.
    pub abort: AbortFlag,
    /// How many Reviewer → Author-fix rounds to allow.
    pub max_review_rounds: usize,
    /// The operator's extra instructions, already composed into a block.
    pub extra_instructions: String,
    /// Extra Author guidance when a template tree was pre-loaded; empty if not.
    pub template_note: &'static str,
    /// The model every stage runs against. `runner` resolves and rate-limits
    /// this; the controller carries no model tables of its own.
    pub model: ModelHandle,
    /// Prices one turn's usage in USD — see [`PriceFn`].
    pub price: PriceFn,
    /// Output-token cap sent with every request — provider knowledge `runner`
    /// resolves per model, same as `model` and `price`.
    pub max_tokens: u32,
    /// Shapes a growing stage's history to fit a budget, and learns from what
    /// the provider actually bills — see [`ContextBudget`]. `runner` builds
    /// this from the same model knowledge as `price`/`max_tokens`.
    pub context_budget: Arc<dyn ContextBudget>,
}

/// What starts a run: a fresh analysis, feedback on the previous result, or
/// carrying on with what a previous run left behind.
///
/// Only [`RunSeed::Fresh`] runs the Analyst. The other two resume a session
/// whose working tree is already seeded, so there is nothing left to analyse:
/// feedback becomes the first pinned review, and a continuation pins nothing at
/// all — the Author simply picks the tree up where it was left.
pub enum RunSeed {
    Fresh,
    Feedback(String),
    Continue,
}

impl RunSeed {
    /// The seed for carrying an existing session on, from whatever the operator
    /// typed into the feedback field.
    ///
    /// Blank feedback is a continuation, not an empty instruction to apply:
    /// pinning `"User feedback to apply to the form:"` with nothing after it
    /// would spend an Author stage on a brief that says nothing. Converting here
    /// rather than at each call site is what makes that unrepresentable — a
    /// `Feedback` seed always carries text.
    pub fn resuming(feedback: &str) -> Self {
        let feedback = feedback.trim();
        if feedback.is_empty() {
            Self::Continue
        } else {
            Self::Feedback(feedback.to_string())
        }
    }

    /// Whether this run starts with the Analyst.
    ///
    /// Only a fresh conversion does. The other two resume a session that already
    /// holds an authored tree, and re-analysing the source would spend a stage's
    /// whole budget producing a plan for work that is already done.
    fn runs_analyst(&self) -> bool {
        matches!(self, Self::Fresh)
    }
}

/// Which message opens the Author stage.
///
/// Pinned review feedback wins over everything: whether it came from the user or
/// from a Reviewer round, there is something concrete to apply. Failing that a
/// continuation finishes the tree it was seeded with, and a fresh run begins
/// from the Analyst's plan.
fn author_seed_for(
    seed: &RunSeed,
    reviews: &[String],
    stages: &roles::TargetRoles,
) -> &'static str {
    if !reviews.is_empty() {
        return stages.author_fix_seed;
    }
    match seed {
        RunSeed::Continue => stages.author_continue_seed,
        RunSeed::Fresh | RunSeed::Feedback(_) => stages.author_seed,
    }
}

/// What a finished run produced.
pub struct RunOutcome {
    /// The run's final document.
    pub document: serde_json::Value,
    pub aem_package: Option<Vec<u8>>,
    /// The same package built with `bind_to_xsd` on: every field carries a
    /// `bindRef` and the schema is bundled. Offered as a separate download.
    pub aem_package_bound: Option<Vec<u8>>,
    pub xsd_schema: Option<String>,
    pub redacto_sql: Option<String>,
    pub form_code: Option<String>,
    /// Notes the run accumulated that did not stop it.
    pub warnings: Vec<String>,
}

/// Sequence the pipeline over `shared_agent`.
///
/// `None` means the run ended before producing anything — the user aborted, or
/// gave up at a retry prompt. The observer has already been told why.
pub async fn run(
    shared_agent: SharedAgent,
    config: RunConfig,
    seed: RunSeed,
    obs: SharedObserver,
) -> Option<RunOutcome> {
    let outcome = run_stages(&shared_agent, &config, seed, &obs).await;
    // Every way out (approved, unapproved, aborted, given up at a retry
    // prompt) tears the verifier down, so no AEM or Postgres container
    // outlives the run.
    if let Err(e) = shared_agent.lock().await.shutdown_verifiers().await {
        obs.emit(RunEvent::Warning(format!(
            "The verification containers could not be removed: {e}"
        )));
    }
    outcome
}

/// The stages themselves; [`run`] wraps this with the teardown that must happen
/// on every exit path.
async fn run_stages(
    shared_agent: &SharedAgent,
    config: &RunConfig,
    seed: RunSeed,
    obs: &SharedObserver,
) -> Option<RunOutcome> {
    let target = config.target;
    let extra = &config.extra_instructions;
    let stages = roles::roles_for(target);
    let mut plan = String::new();
    let mut reviews: Vec<String> = Vec::new();
    // The whole run's running total — every stage folds its own spend into
    // this one accumulator, so the last `RunEvent::Spend` a run emits is the
    // form's full cost, not just its last stage's.
    let mut spend = Spend::default();

    // A feedback run pins the request as the first "review"; a continuation pins
    // nothing at all — the seeded tree is the whole brief.
    if let RunSeed::Feedback(fb) = &seed {
        reviews.push(format!("User feedback to apply to the form:\n{fb}"));
    }

    // ── Stage 1: Analyst → conversion plan ──────────────────────────────────
    if seed.runs_analyst() {
        obs.emit(RunEvent::Stage {
            role: "Analyst",
            doing: "analysing the source and researching precedents".into(),
        });
        plan = run_stage(
            shared_agent,
            stages.analyst,
            &roles::sys_analyst(target, extra),
            "Analyse the source form and produce the detailed CONVERSION PLAN. \
             Your final message is the plan.",
            &config.abort,
            config.model.clone(),
            config.price.clone(),
            config.max_tokens,
            config.context_budget.clone(),
            obs,
            &mut spend,
        )
        .await?; // fatal API error or abort, already surfaced
    }

    // ── Stage 2: Author → build the artefact ────────────────────────────────
    obs.emit(RunEvent::Stage {
        role: "Author",
        doing: stages.author_doing.into(),
    });
    let author_seed = author_seed_for(&seed, &reviews, &stages);
    run_stage(
        shared_agent,
        stages.author,
        &roles::sys_author(target, extra, config.template_note, &plan, &reviews),
        author_seed,
        &config.abort,
        config.model.clone(),
        config.price.clone(),
        config.max_tokens,
        config.context_budget.clone(),
        obs,
        &mut spend,
    )
    .await?;

    // ── Stage 3: Reviewer → (Author fix)* ───────────────────────────────────
    let mut approved = false;
    let mut warnings: Vec<String> = Vec::new();
    for round in 0..config.max_review_rounds {
        obs.emit(RunEvent::Stage {
            role: "Reviewer",
            doing: format!("reviewing (round {})", round + 1),
        });
        run_stage(
            shared_agent,
            stages.reviewer,
            &roles::sys_reviewer(target, extra, &plan, &reviews),
            "Review the built form end to end against the source and the CONVERSION PLAN, \
             then finish by calling submit_review.",
            &config.abort,
            config.model.clone(),
            config.price.clone(),
            config.max_tokens,
            config.context_budget.clone(),
            obs,
            &mut spend,
        )
        .await?;

        let review = shared_agent.lock().await.take_review();
        obs.trace(TraceEvent::ReviewVerdict {
            stage: stages.reviewer.name.to_string(),
            round: round + 1,
            approved: review.as_ref().map(|r| r.approved),
            report: review.as_ref().map(|r| r.report.clone()).unwrap_or_default(),
        });
        match review {
            Some(r) if r.approved => {
                approved = true;
                obs.emit(RunEvent::Thought("Reviewer approved the form.".into()));
                break;
            }
            Some(r) => {
                obs.emit(RunEvent::Thought(format!(
                    "Reviewer requested changes (round {}). Returning to the author.",
                    round + 1
                )));
                reviews.push(r.report);
                obs.emit(RunEvent::Stage {
                    role: "Author",
                    doing: format!("applying review feedback (round {})", round + 1),
                });
                run_stage(
                    shared_agent,
                    stages.author,
                    &roles::sys_author(target, extra, config.template_note, &plan, &reviews),
                    stages.author_fix_seed,
                    &config.abort,
                    config.model.clone(),
                    config.price.clone(),
                    config.max_tokens,
                    config.context_budget.clone(),
                    obs,
                    &mut spend,
                )
                .await?;
            }
            None => {
                // Reviewer ended without a verdict (budget/stuck). Stop the loop.
                let w =
                    "The reviewer ended without a verdict — finalizing with what's built.".to_string();
                obs.emit(RunEvent::Warning(w.clone()));
                warnings.push(w);
                break;
            }
        }
    }

    if !approved {
        let w = "Finalizing without a clean review — some issues may require manual follow-up."
            .to_string();
        obs.emit(RunEvent::Warning(w.clone()));
        warnings.push(w);
    }

    // Building a CRX package is AEM-only; for any other target the dump the
    // Author already validated is the artefact, and calling this would paint a
    // failed build step on an otherwise successful run.
    if target == OutputTarget::Aem {
        tool_step(shared_agent, "finalize-build", "build_aem_package", obs).await;
    }

    Some(finalize(shared_agent, config, warnings).await)
}

/// Assemble the run's artefacts: the build of the final document.
async fn finalize(
    shared_agent: &SharedAgent,
    _config: &RunConfig,
    mut warnings: Vec<String>,
) -> RunOutcome {
    let mut agent = shared_agent.lock().await;
    let agent::outputs::Outputs {
        document,
        package,
        package_bound,
        xsd,
        redacto_sql,
        warnings: build_warnings,
    } = agent::outputs::build(&mut agent);
    warnings.extend(build_warnings);

    RunOutcome {
        document,
        aem_package: package,
        aem_package_bound: package_bound,
        xsd_schema: xsd,
        redacto_sql,
        form_code: agent.form_code(),
        warnings,
    }
}

// ── Turn-level failure recovery ──────────────────────────────────────────────

/// Whether a model-call error looks transient — i.e. worth re-sending the same turn
/// unchanged.
pub fn is_transient_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    const TRANSIENT: &[&str] = &[
        "timed out",
        "timeout",
        "error decoding response body",
        "error reading a body from connection",
        "connection reset",
        "connection closed",
        "connection refused",
        "broken pipe",
        "incomplete message",
        "dns error",
        "os error 50", // network is down
        "os error 51", // network unreachable
        "os error 54", // connection reset by peer
        "os error 64", // host is down
        "os error 65", // no route to host
        "overloaded",
        "rate_limit",
        "rate limit",
        "internal server error",
        "api_error",
    ];
    // The transport appends `[status: N]` verbatim, so this matches a token it
    // controls rather than an error type's prose. Retry what the APIs document
    // as retryable, and nothing else: a 4xx we would only repeat goes straight
    // to the operator.
    const RETRYABLE_STATUSES: &[&str] = &[
        "[status: 429]",
        "[status: 500]",
        "[status: 502]",
        "[status: 503]",
        "[status: 504]",
        "[status: 529]",
    ];

    TRANSIENT
        .iter()
        .chain(RETRYABLE_STATUSES)
        .any(|needle| e.contains(needle))
}

/// How long the provider asked us to wait, if it said.
///
/// The transports append `[retry-after: N]` to the error text when the response
/// carried the header. Honouring it beats guessing: a provider that names a
/// window knows when the quota actually refills, and retrying earlier just burns
/// another rejection.
pub(crate) fn parse_retry_after(err: &str) -> Option<u64> {
    let start = err.rfind("[retry-after: ")? + "[retry-after: ".len();
    let rest = &err[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

/// Spread retries so parallel runs stop colliding.
///
/// Conversions run side by side against one API key, so they tend to hit the
/// same rate limit at the same moment — and with a purely exponential backoff
/// they would then wake at the same moment too, collide again, and keep step
/// with each other. Jitter breaks that lockstep.
///
/// Returns a delay in `[base / 2, base * 3 / 2)`. The seed is the caller's to
/// supply, so the spread is testable rather than genuinely random.
pub(crate) fn jittered_backoff(base_secs: u64, seed: u64) -> u64 {
    // xorshift64*: `pipeline` carries no rng dependency, and the quality bar
    // here is "two runs pick different numbers".
    let mut x = seed | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let spread = base_secs.max(1);
    (base_secs / 2) + (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) % spread
}

/// A seed for [`jittered_backoff`] that differs between runs in one process.
fn jitter_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}

/// Sleep for `total`, waking early if the run is aborted. Returns whether it was
/// aborted.
///
/// A plain sleep would hold an aborted run open for the whole retry backoff.
pub(crate) async fn sleep_unless_aborted(
    total: std::time::Duration,
    abort: &AbortFlag,
) -> bool {
    let tick = std::time::Duration::from_millis(RETRY_POLL_MS);
    let mut slept = std::time::Duration::ZERO;
    while slept < total {
        if abort.is_aborted() {
            return true;
        }
        let step = tick.min(total - slept);
        tokio::time::sleep(step).await;
        slept += step;
    }
    abort.is_aborted()
}

/// Pause a stage on a failed turn and wait for the operator to retry or give up.
/// Keeping the run's future alive is the whole point: the agent, its working
/// tree and this stage's history stay in memory, so a retry re-sends exactly the
/// turn that failed rather than restarting the conversion.
async fn await_user_retry(
    obs: &SharedObserver,
    abort: &AbortFlag,
    role: &str,
    err: &str,
) -> RetryAction {
    obs.retry_prompt(role, err);
    obs.trace(TraceEvent::Control {
        stage: role.to_string(),
        kind: ControlKind::OperatorPrompt,
        detail: err.to_string(),
    });
    // Surface-neutral: the app answers this with a button, a CLI with a retry
    // budget, and the sentence has to read correctly in both.
    obs.emit(RunEvent::Thought(format!(
        "Paused after a failed request ({err}). Waiting for a retry decision."
    )));

    let action = loop {
        if let Some(action) = obs.poll_retry() {
            break action;
        }
        // Abort ends a paused run too, so the stop control works in every state.
        if abort.is_aborted() {
            break RetryAction::Cancel;
        }
        tokio::time::sleep(std::time::Duration::from_millis(RETRY_POLL_MS)).await;
    };

    obs.retry_resolved(action);
    obs.trace(TraceEvent::Control {
        stage: role.to_string(),
        kind: match action {
            RetryAction::Retry => ControlKind::OperatorRetried,
            RetryAction::Cancel => ControlKind::OperatorCancelled,
        },
        detail: String::new(),
    });
    if action == RetryAction::Retry {
        obs.emit(RunEvent::Thought("Retrying the failed request…".into()));
    }
    action
}

/// Run one stage to completion: a fresh [`Agent::runner`] seeded with
/// `seed_user_msg`, the stage's scoped tool subset, and its `system` prompt.
/// Returns the last non-tool assistant message; `None` if the run should stop.
///
/// rig's own execution model owns turn counting, tool-call validation and
/// history threading; [`crate::hooks::StageHook`] is what reproduces the
/// abort flag, the tool timeline, the output-cap nudge and the stuck
/// watchdog. What is left here is what rig has no seam for at all: a
/// transient failure's automatic retry and backoff, the operator's Retry
/// prompt, and — since rig cannot resume a failed `AgentRunner` in place —
/// restarting a fresh one from exactly the point the failed attempt reached,
/// using [`crate::hooks::StageHook::last_attempt`].
///
/// A hook's stop (abort, the stuck watch, `submit_review`) surfaces as
/// `Err(PromptError::PromptCancelled)` rather than a normal finish — rig has
/// no "clean early stop" outcome — so those reasons are read back here as
/// success, not failure; see the sentinels in [`crate::hooks`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_stage(
    shared_agent: &SharedAgent,
    role: &'static Role,
    system: &str,
    seed_user_msg: &str,
    abort: &AbortFlag,
    model: ModelHandle,
    price: PriceFn,
    max_tokens: u32,
    context_budget: Arc<dyn ContextBudget>,
    obs: &SharedObserver,
    // The run's own running total, not this stage's alone — seeded from
    // whatever the earlier stages already spent, and left holding this
    // stage's contribution added in on every return path, so the whole run's
    // cost survives across stages the same way a restart already carries a
    // stage's own spend across retries.
    total_spend: &mut Spend,
) -> Option<String> {
    let started = std::time::Instant::now();
    let spend_before = *total_spend;
    let mut stats = StageTally::default();

    let (specs, session_id) = {
        let agent = shared_agent.lock().await;
        (agent.tools_for_stage(role.scope), agent.session_id().to_string())
    };
    obs.trace(TraceEvent::StageStarted {
        stage: role.name.to_string(),
        system_prompt: system.to_string(),
        seed_message: seed_user_msg.to_string(),
        max_turns: role.max_iterations,
        tools_offered: specs
            .iter()
            .filter_map(|spec| spec.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect(),
    });
    let agent = build_stage_agent(model, max_tokens, shared_agent, &specs);

    let memory = stage_memory(&session_id, role.name, context_budget.policy());
    let loaded: Vec<Message> = match &memory {
        // Repaired as it is loaded: a conversation stored before calls were
        // answered on the way out still holds unanswered ones.
        Some((memory, id)) => match memory.load(id).await {
            Ok(loaded) => answer_unanswered_tool_calls(loaded),
            Err(e) => {
                obs.emit(RunEvent::Warning(format!(
                    "{}: the stage's earlier conversation could not be loaded ({e}); \
                     starting without it.",
                    role.name
                )));
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    // The stage's whole history as far as it got, stored on the way out.
    let mut history: Vec<Message> = loaded.clone();
    let outcome = run_stage_attempts(
        &agent,
        role,
        system,
        seed_user_msg,
        abort,
        &price,
        &context_budget,
        obs,
        total_spend,
        &loaded,
        &mut history,
        &mut stats,
    )
    .await;
    let history = answer_unanswered_tool_calls(history);
    store_stage(memory.as_ref(), &loaded, &history, role, obs).await;
    obs.trace(TraceEvent::StageFinished {
        stage: role.name.to_string(),
        ended: stats.ended.unwrap_or(if outcome.is_some() {
            StageEnd::Finished
        } else {
            StageEnd::Aborted
        }),
        turns: stats.turns,
        attempts: stats.attempts,
        duration_ms: trace::elapsed_ms(started),
        spend: trace::spend_between(&spend_before, total_spend),
    });
    outcome
}

/// What [`run_stage_attempts`] reports back for the stage's trace summary.
#[derive(Default)]
struct StageTally {
    /// How the stage ended; `None` for an ordinary finish.
    ended: Option<StageEnd>,
    attempts: usize,
    turns: usize,
}

impl StageTally {
    fn end(&mut self, ended: StageEnd) {
        self.ended = Some(ended);
    }
}

/// [`run_stage`]'s attempts: the first from the loaded conversation, and each
/// restart from where the failed one left off. Leaves in `history` the
/// stage's whole history as far as it got.
#[allow(clippy::too_many_arguments)]
async fn run_stage_attempts(
    agent: &Agent,
    role: &'static Role,
    system: &str,
    seed_user_msg: &str,
    abort: &AbortFlag,
    price: &PriceFn,
    context_budget: &Arc<dyn ContextBudget>,
    obs: &SharedObserver,
    total_spend: &mut Spend,
    loaded: &[Message],
    history: &mut Vec<Message>,
    stats: &mut StageTally,
) -> Option<String> {
    use futures_util::StreamExt;

    let mut restart_from: Option<Vec<Message>> = None;
    let mut total_completed_turns = 0usize;
    let mut final_text = String::new();
    let mut auto_retries = 0usize;

    loop {
        if abort.is_aborted() {
            obs.emit(RunEvent::Aborted);
            trace_aborted(obs, role, stats);
            return None;
        }
        let remaining = role.max_iterations.saturating_sub(total_completed_turns).max(1);
        // A fresh hook per attempt carries the turn count and spend forward
        // (both threaded in above), but not the output-cap nudge counter or
        // the stuck watch's repeat count — those reset to zero on a restart.
        // Accepted: a transient network failure landing mid-nudge or
        // mid-repeat-sequence gives the stage a few turns' extra leeway
        // rather than losing progress, which is the direction to err in for
        // state that only ever *tightens* a budget, never grows the actual
        // turn/spend totals a restart must not lose.
        stats.attempts += 1;
        let history_messages = restart_from.as_ref().map_or(loaded.len(), Vec::len);
        obs.trace(TraceEvent::AttemptStarted {
            stage: role.name.to_string(),
            attempt: stats.attempts,
            history_messages,
        });
        let hook = SharedHook::new(
            StageHook::new(
                role,
                abort.clone(),
                obs.clone(),
                price.clone(),
                *total_spend,
                context_budget.clone(),
            )
            .with_trace_position(stats.attempts, total_completed_turns),
        );

        // The history this attempt starts from, which its own messages
        // follow; explicit either way, so rig neither loads nor saves.
        let (runner, attempt_history) = match restart_from.take() {
            None => (
                agent.runner(seed_user_msg.to_string()).history(loaded.to_vec()),
                loaded.to_vec(),
            ),
            Some(mut attempt) => {
                // `last_attempt` always captured history plus the prompt about
                // to be sent, so a restart's own prompt is that last message
                // and the rest becomes the history it is resumed with.
                let prompt = attempt
                    .pop()
                    .expect("a restart always captured at least one message");
                (agent.runner(prompt).history(attempt.clone()), attempt)
            }
        };
        let mut stream = runner
            .preamble(system)
            .max_turns(remaining)
            .add_hook(hook.clone())
            .stream()
            .await;

        let mut stream_error: Option<StreamingError> = None;
        let mut finished: Option<Vec<Message>> = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                    finished = Some(response.messages.unwrap_or_default());
                }
                Ok(_) => {}
                Err(e) => {
                    stream_error = Some(e);
                    break;
                }
            }
        }
        drop(stream);
        // As far as this attempt got: its own messages after the history it
        // started from when it finished, otherwise the last request it sent.
        *history = match &finished {
            Some(messages) => [attempt_history, messages.clone()].concat(),
            None => {
                let attempted = hook.last_attempt();
                if attempted.is_empty() { attempt_history } else { attempted }
            }
        };

        total_completed_turns += hook.completed_turns();
        stats.turns = total_completed_turns;
        *total_spend = hook.spend();
        if !hook.final_text().is_empty() {
            final_text = hook.final_text();
        }

        let Some(error) = stream_error else {
            return Some(final_text);
        };

        match error {
            StreamingError::Prompt(boxed) => match *boxed {
                PromptError::PromptCancelled { chat_history, reason } => {
                    *history = chat_history.to_vec();
                    if abort.is_aborted() {
                        obs.emit(RunEvent::Aborted);
                        trace_aborted(obs, role, stats);
                        return None;
                    }
                    let text = last_assistant_text(&chat_history);
                    if !text.is_empty() {
                        final_text = text;
                    }
                    if reason == crate::hooks::STUCK_SENTINEL {
                        obs.emit(RunEvent::Warning(format!(
                            "{}: {} produced the same result {} times in a row — moving on.",
                            role.name, role.stuck_activity, MAX_VALIDATE_REPEATS
                        )));
                        obs.trace(TraceEvent::Control {
                            stage: role.name.to_string(),
                            kind: ControlKind::StuckStop,
                            detail: format!(
                                "{} returned the same result {MAX_VALIDATE_REPEATS} times in a row",
                                role.stuck_tool.unwrap_or("the watched tool")
                            ),
                        });
                        stats.end(StageEnd::Stuck);
                    } else if reason == crate::hooks::SUBMIT_REVIEW_SENTINEL {
                        stats.end(StageEnd::ReviewSubmitted);
                    }
                    return Some(final_text);
                }
                PromptError::MaxTurnsError { chat_history, max_turns, .. } => {
                    *history = chat_history.to_vec();
                    let text = last_assistant_text(&chat_history);
                    if !text.is_empty() {
                        final_text = text;
                    }
                    // The stage went off the rails and burned its whole turn
                    // budget without a `submit_review`/finish — the hand-rolled
                    // loop warned on every `AgentRun` step error, this one
                    // included, and silently finalizing here would hide exactly
                    // the "ran out of budget" case an operator most needs to see.
                    obs.emit(RunEvent::Warning(format!(
                        "{}: reached its {max_turns}-turn budget without finishing — \
                         finalizing with whatever it produced.",
                        role.name
                    )));
                    obs.trace(TraceEvent::Control {
                        stage: role.name.to_string(),
                        kind: ControlKind::TurnBudgetExhausted,
                        detail: format!("{max_turns} turns"),
                    });
                    stats.end(StageEnd::TurnBudgetExhausted);
                    return Some(final_text);
                }
                other => {
                    obs.emit(RunEvent::Warning(format!("{}: {other}", role.name)));
                    obs.trace(TraceEvent::Control {
                        stage: role.name.to_string(),
                        kind: ControlKind::StageError,
                        detail: other.to_string(),
                    });
                    stats.end(StageEnd::Error(other.to_string()));
                    return Some(final_text);
                }
            },
            StreamingError::Completion(completion_error) => {
                let text = describe_completion_error(&completion_error);
                obs.trace(TraceEvent::RequestFailed {
                    stage: role.name.to_string(),
                    attempt: stats.attempts,
                    turn: hook.current_turn(),
                    latency_ms: hook.take_pending_request_ms().unwrap_or(0),
                    error: text.clone(),
                });
                if abort.is_aborted() {
                    obs.emit(RunEvent::Aborted);
                    trace_aborted(obs, role, stats);
                    return None;
                }
                if is_transient_error(&text) && auto_retries < MAX_AUTO_RETRIES {
                    let backoff =
                        (RETRY_BACKOFF_SECS << auto_retries.min(4)).min(MAX_RETRY_BACKOFF_SECS);
                    let wait = parse_retry_after(&text)
                        .map(|secs| secs.min(MAX_RETRY_BACKOFF_SECS))
                        .unwrap_or_else(|| jittered_backoff(backoff, jitter_seed()));
                    auto_retries += 1;
                    obs.emit(RunEvent::Thought(format!(
                        "Request failed ({text}) — retrying in {wait}s \
                         (attempt {auto_retries} of {MAX_AUTO_RETRIES})."
                    )));
                    obs.trace(TraceEvent::Control {
                        stage: role.name.to_string(),
                        kind: ControlKind::TransientRetry,
                        detail: format!(
                            "waiting {wait}s before retry {auto_retries} of {MAX_AUTO_RETRIES}: {text}"
                        ),
                    });
                    if sleep_unless_aborted(std::time::Duration::from_secs(wait), abort).await {
                        obs.emit(RunEvent::Aborted);
                        trace_aborted(obs, role, stats);
                        return None;
                    }
                    restart_from = Some(hook.last_attempt());
                    continue;
                }
                match await_user_retry(obs, abort, role.name, &text).await {
                    RetryAction::Retry => {
                        // A user-driven retry resets the automatic budget, so a
                        // long unattended stall can be resumed repeatedly.
                        auto_retries = 0;
                        restart_from = Some(hook.last_attempt());
                        continue;
                    }
                    RetryAction::Cancel => {
                        stats.end(if abort.is_aborted() {
                            StageEnd::Aborted
                        } else {
                            StageEnd::GaveUp
                        });
                        return None;
                    }
                }
            }
        }
    }
}

/// The trace side of an abort checkpoint: one control event, and the stage
/// marked as aborted. Paired with every `RunEvent::Aborted` in a stage.
fn trace_aborted(obs: &SharedObserver, role: &Role, stats: &mut StageTally) {
    obs.trace(TraceEvent::Control {
        stage: role.name.to_string(),
        kind: ControlKind::Aborted,
        detail: String::new(),
    });
    stats.end(StageEnd::Aborted);
}

/// Build one stage's `Agent`: the model it runs against, plus the tool
/// catalog's own scoped subset bridged onto rig's `DynamicTool` — see
/// [`crate::tools`].
#[allow(clippy::too_many_arguments)]
fn build_stage_agent(
    model: ModelHandle,
    max_tokens: u32,
    shared_agent: &SharedAgent,
    specs: &[serde_json::Value],
) -> Agent {
    let dynamic_tools = tools::dynamic_tools_for(shared_agent, specs);
    let builder = AgentBuilder::new(model).max_tokens(u64::from(max_tokens));

    let mut iter = dynamic_tools.into_iter();
    match iter.next() {
        None => builder.build(),
        Some(first) => {
            let mut builder = builder.dynamic_tool(first);
            for tool in iter {
                builder = builder.dynamic_tool(tool);
            }
            builder.build()
        }
    }
}

/// A stage's conversation memory: the backend and the id it is stored under.
type StageMemory = (Arc<dyn ConversationMemory>, String);

/// The memory `session_id`'s run of the stage `role_name` loads from and
/// appends to, or `None` for a throwaway agent with no session of its own
/// (`describe_reference`'s one-shot pass): it has nothing to key a
/// conversation by, and never runs again under the same id.
///
/// Not attached to the rig `Agent`: rig saves a conversation only when a run
/// reaches a natural end, and bypasses memory entirely for a run given
/// explicit history. A stage ends through a hook stop (`submit_review`, the
/// stuck watch, an abort) or its turn budget as often as naturally, and a
/// restart after a transient failure resumes from explicit history, so
/// [`run_stage`] loads and appends itself, on every exit.
fn stage_memory(session_id: &str, role_name: &str, policy: Arc<dyn MemoryPolicy>) -> Option<StageMemory> {
    if session_id.is_empty() {
        return None;
    }
    // Capped rather than left to grow monotonically (`TemplateCompactor`'s
    // default): rig-memory's own docs warn that pairing `CompactingMemory`
    // with a token-budgeted policy needs a compactor that bounds its own
    // artifact, or the loaded prompt can exceed the policy's budget by the
    // summary's size. The bound is generous — this is a standing rollup
    // note, not per-turn content — since the per-turn `ContextBudget`
    // shaping downstream would otherwise be the only thing standing
    // between an unbounded summary and the request.
    const MAX_COMPACTED_SUMMARY_BYTES: usize = 8 * 1024;
    let compactor = TemplateCompactor::new().with_max_bytes(MAX_COMPACTED_SUMMARY_BYTES);
    let memory: Arc<dyn ConversationMemory> =
        Arc::new(CompactingMemory::new(SqliteConversationMemory, policy, compactor));
    Some((memory, memory::conversation_id(session_id, role_name)))
}

/// What a call left unanswered is answered with when a conversation is
/// resumed.
const UNANSWERED_CALL_RESULT: &str = "The stage ended here; this call returned nothing further.";

/// `history` with a result for every tool call it leaves unanswered, put in
/// the user message right after the call (inserting one where there is
/// none). A stage a hook stops (`submit_review`, the stuck watch, an abort)
/// ends after a call and before rig records its result, and a provider
/// refuses to continue a conversation holding such a call. Pure.
fn answer_unanswered_tool_calls(history: Vec<Message>) -> Vec<Message> {
    let mut repaired: Vec<Message> = Vec::with_capacity(history.len() + 1);
    let mut messages = history.into_iter().peekable();
    while let Some(message) = messages.next() {
        let calls: Vec<(ToolCallId, Option<ProviderCallId>, String)> = match &message {
            Message::Assistant { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::ToolCall(call) => {
                        Some((call.id.clone(), call.provider.clone(), call.function.name.clone()))
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        repaired.push(message);
        if calls.is_empty() {
            continue;
        }
        let mut next = match messages.peek() {
            Some(Message::User { .. }) => messages.next().expect("peeked"),
            _ => Message::User { content: Vec::new() },
        };
        let Message::User { content } = &mut next else { unreachable!("only a user message is taken") };
        let missing: Vec<UserContent> = calls
            .into_iter()
            .filter(|(id, _, _)| {
                !content.iter().any(|c| matches!(c, UserContent::ToolResult(r) if &r.call == id))
            })
            .map(|(id, provider, name)| {
                UserContent::tool_result_for(
                    id,
                    provider,
                    name,
                    vec![ToolResultContent::Text(UNANSWERED_CALL_RESULT.to_string().into())],
                )
            })
            .collect();
        content.splice(0..0, missing);
        if !content.is_empty() {
            repaired.push(next);
        }
    }
    repaired
}

/// Append what a stage added to what it loaded: `full` is the stage's whole
/// history, which starts with the `loaded` messages it was resumed from.
async fn store_stage(
    memory: Option<&StageMemory>,
    loaded: &[Message],
    full: &[Message],
    role: &Role,
    obs: &SharedObserver,
) {
    let Some((memory, id)) = memory else { return };
    if !full.starts_with(loaded) {
        obs.emit(RunEvent::Warning(format!(
            "{}: the stage's history no longer starts with the conversation it resumed, \
             so it was not stored.",
            role.name
        )));
        return;
    }
    let added = &full[loaded.len()..];
    if added.is_empty() {
        return;
    }
    if let Err(e) = memory.append(id, added.to_vec()).await {
        obs.emit(RunEvent::Warning(format!(
            "{}: the stage's conversation could not be stored: {e}",
            role.name
        )));
    }
}

/// Render a bare provider failure the way the retry ladder and the operator
/// prompt have always read it: the error text, plus the transport's own
/// `[status: N]`/`[retry-after: N]` suffixes when the provider reported them.
///
/// `pub`: this is the one place that wording is produced, and `runner`'s own
/// transport tests classify a *real* provider failure through it — the
/// coupling between this text and [`is_transient_error`]'s substring matching
/// is invisible to the compiler, and it has already broken once across this
/// crate boundary when the wording changed on one side only.
pub fn describe_completion_error(error: &CompletionError) -> String {
    let mut text = format!("LLM API error: {error}");
    if let Some(status) = error.provider_response_status() {
        text.push_str(&format!(" [status: {}]", status.as_u16()));
    }
    if let Some(secs) = error
        .provider_response_headers()
        .and_then(|headers| headers.get("retry-after"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        text.push_str(&format!(" [retry-after: {secs}]"));
    }
    text
}

/// The last assistant text in a history rig hands back on a stop or a
/// budget-exhausted run — `run_stage`'s answer to "what did the stage say
/// last" when there is no [`rig_agent::agent::PromptResponse`] to read it
/// from at all.
fn last_assistant_text(history: &[Message]) -> String {
    history
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::Assistant { content, .. } => {
                let text: String = content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                (!text.trim().is_empty()).then(|| text.trim().to_string())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Run one of the agent's own tools as a visible finalize step. Returns whether
/// it succeeded.
async fn tool_step(shared_agent: &SharedAgent, id: &str, tool: &str, obs: &SharedObserver) -> bool {
    obs.emit(RunEvent::ToolStarted {
        id: id.to_string(),
        name: tool.to_string(),
        input_summary: "finalize".into(),
    });
    let reply = {
        let mut agent = shared_agent.lock().await;
        agent.execute(tool, &serde_json::json!({})).await
    };
    let ok = !matches!(reply, ToolReply::Error(_));
    obs.emit(RunEvent::ToolFinished {
        id: id.to_string(),
        ok,
        reply_chars: reply_size_chars(&reply),
    });
    // These finalize tools reply with a size/status line, never the artefact
    // itself — see the executor — so this never actually fires; checked
    // anyway so the two call sites do not silently diverge on the rule.
    if let Some(warning) = oversized_reply_warning(tool, &reply) {
        obs.emit(RunEvent::Warning(warning));
    }
    ok
}

/// A reply's content, split into text characters and image (base64) payload
/// characters. The one place either is actually counted, so
/// [`reply_size_chars`] and [`text_reply_chars`] cannot drift apart into two
/// separate opinions about what a `Blocks` reply contains.
fn reply_char_breakdown(reply: &ToolReply) -> (usize, usize) {
    match reply {
        ToolReply::Text(text) | ToolReply::Error(text) => (text.len(), 0),
        ToolReply::Blocks(blocks) => blocks.iter().fold((0, 0), |(t, i), b| match b {
            agent::ReplyBlock::Text(text) => (t + text.len(), i),
            agent::ReplyBlock::Image { data, .. } => (t, i + data.len()),
        }),
    }
}

/// Characters `RunEvent::ToolFinished` reports for a reply — its text plus
/// each image's base64 payload, which is roughly what actually reaches the
/// model.
pub(crate) fn reply_size_chars(reply: &ToolReply) -> usize {
    let (text, image) = reply_char_breakdown(reply);
    text + image
}

/// Characters of *text* content in a reply — the part [`oversized_reply_warning`]
/// actually checks.
///
/// An image's base64 payload is excluded on purpose: its real cost is the
/// vision encoder's, which — unlike a text tokenizer reading the same bytes as
/// characters — is already bounded by the page-render clamp regardless of how
/// long the base64 string is. A full-page annotated render legitimately runs
/// past 300,000 base64 characters and costs a bounded ~1,500 tokens for it;
/// warning on that would be noise on every ordinary run. What actually caused
/// the incident this instruments — `generate_html`'s inlined fonts and logo —
/// was base64 arriving as *text*, tokenized close to one token per character
/// because nothing marked it as an image.
fn text_reply_chars(reply: &ToolReply) -> usize {
    reply_char_breakdown(reply).0
}

/// Above this, a single reply's *text* is worth flagging on its own: the run
/// that motivated this recorded one text reply at 873,000 characters (base64
/// profile assets `generate_html` no longer inlines) that blew a stage's whole
/// context window before anyone could see it happening. Comfortably above the
/// normal large text replies (a wide `xfa_read` window or a big
/// `read_package_file` runs 50-80,000) so this fires only on the kind of reply that is actually a
/// problem, not on a legitimately large page render.
pub(crate) const LARGE_TOOL_REPLY_WARN_CHARS: usize = 200_000;

/// The warning to raise for `reply`, if its text earns one. Pure, so the
/// threshold and the message it produces are tested without driving a whole
/// stage, and both `ToolFinished` sites share one rule rather than repeating
/// the check inline.
pub(crate) fn oversized_reply_warning(name: &str, reply: &ToolReply) -> Option<String> {
    let text_chars = text_reply_chars(reply);
    (text_chars > LARGE_TOOL_REPLY_WARN_CHARS).then(|| {
        format!(
            "{name} replied with {text_chars} characters of text — unusually large; this is \
             what tends to blow the context window."
        )
    })
}

/// A short, single-line rendering of a tool call's input.
pub(crate) fn summarize_input(input: &serde_json::Value) -> String {
    let s = match input {
        serde_json::Value::Object(m) if m.is_empty() => String::new(),
        _ => input.to_string(),
    };
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a fresh conversion analyses the source. A resumed session already
    /// holds an authored tree, and an Analyst stage there would spend its whole
    /// budget planning work that is already done.
    #[test]
    fn only_a_fresh_run_analyses_the_source() {
        assert!(RunSeed::Fresh.runs_analyst());
        assert!(!RunSeed::Feedback("make it optional".into()).runs_analyst());
        assert!(!RunSeed::Continue.runs_analyst());
    }

    /// The Author's opening message decides whether it authors from scratch,
    /// applies something, or finishes what it was handed — so a continuation
    /// must not be told to begin, which would discard the seeded tree.
    #[test]
    fn the_author_is_told_which_of_the_three_jobs_it_has() {
        for target in [OutputTarget::Aem, OutputTarget::Redacto] {
            let stages = roles::roles_for(target);
            let none: Vec<String> = Vec::new();
            let some = vec!["fix the phone field".to_string()];

            assert_eq!(
                author_seed_for(&RunSeed::Fresh, &none, &stages),
                stages.author_seed
            );
            assert_eq!(
                author_seed_for(&RunSeed::Continue, &none, &stages),
                stages.author_continue_seed,
                "a continuation has to finish the tree, not start one"
            );
            // Feedback is itself pinned as the first review, so it arrives here
            // with a non-empty list.
            assert_eq!(
                author_seed_for(&RunSeed::Feedback("do it".into()), &some, &stages),
                stages.author_fix_seed
            );
            // A Reviewer round during a continuation gives it something to apply.
            assert_eq!(
                author_seed_for(&RunSeed::Continue, &some, &stages),
                stages.author_fix_seed
            );
        }
    }

    /// A provider that names a retry window is telling us when the quota
    /// actually refills; guessing earlier just earns another rejection.
    #[test]
    fn the_providers_own_retry_window_is_read_back() {
        assert_eq!(
            parse_retry_after("Anthropic API error (429 Too Many Requests): slow down [retry-after: 30]"),
            Some(30)
        );
        // No header, or a form we do not read, falls back to the computed wait.
        assert_eq!(parse_retry_after("Anthropic API error (429): slow down"), None);
        assert_eq!(parse_retry_after("boom [retry-after: Wed, 21 Oct 2015 07:28:00 GMT]"), None);
        assert_eq!(parse_retry_after("boom [retry-after: ]"), None);
        // A message that happens to mention it twice reads the last one, which
        // is the suffix this crate appends.
        assert_eq!(parse_retry_after("[retry-after: 5] wrapped [retry-after: 9]"), Some(9));
    }

    /// Parallel conversions share one API key, so they hit a rate limit together
    /// — and with a bare exponential backoff they would wake together, collide
    /// again, and stay in step indefinitely.
    #[test]
    fn backoff_is_spread_so_parallel_runs_stop_colliding() {
        let base = 20;
        let waits: Vec<u64> = (0..64).map(|seed| jittered_backoff(base, seed)).collect();

        for wait in &waits {
            assert!(
                (base / 2..base + base / 2).contains(wait),
                "{wait}s is outside the intended spread around {base}s"
            );
        }
        let distinct: std::collections::HashSet<_> = waits.iter().collect();
        assert!(
            distinct.len() > 8,
            "the spread collapsed to {} values — runs would still retry in lockstep",
            distinct.len()
        );
    }

    /// The shortest backoff must not divide down to an instant retry loop.
    #[test]
    fn the_smallest_backoff_still_waits() {
        for seed in 0..32 {
            assert!(jittered_backoff(1, seed) <= 1);
            assert!(jittered_backoff(5, seed) >= 2);
        }
    }

    #[test]
    fn summarize_input_truncates() {
        assert_eq!(summarize_input(&serde_json::json!({})), "");
        let long = serde_json::json!({"q": "x".repeat(500)});
        assert!(summarize_input(&long).chars().count() <= 121);
    }

    /// Every `ToolReply` shape counts toward the size a run reports —
    /// otherwise a reply that happened to come back as `Blocks` or
    /// `Image` would silently escape the same instrumentation a `Text`
    /// reply gets.
    /// A reply of page images, as the u2s render tools send one.
    fn image_reply(images: Vec<String>) -> ToolReply {
        ToolReply::Blocks(
            images
                .into_iter()
                .map(|data| agent::ReplyBlock::Image {
                    media_type: "image/jpeg".into(),
                    data,
                })
                .collect(),
        )
    }

    #[test]
    fn reply_size_counts_every_reply_shape() {
        assert_eq!(reply_size_chars(&ToolReply::Text("hello".into())), 5);
        assert_eq!(reply_size_chars(&ToolReply::Error("boom!!".into())), 6);
        assert_eq!(
            reply_size_chars(&image_reply(vec!["ab".into(), "cde".into()])),
            5,
            "every image's payload must count, not just the first"
        );
        assert_eq!(
            reply_size_chars(&ToolReply::Blocks(vec![
                agent::ReplyBlock::Text("hi".into()),
                agent::ReplyBlock::Image { media_type: "image/png".into(), data: "xyz".into() },
            ])),
            5
        );
    }

    /// Regression: `generate_html`'s reply once cost a stage over 800,000
    /// prompt tokens in a single call, and nothing recorded a reply's size
    /// at all — the run reported success and moved on. The threshold has
    /// to sit above the normal large replies this run makes
    /// (a wide `xfa_read` window runs 50-80,000 characters) so
    /// it does not fire on ordinary tool traffic.
    #[test]
    fn the_warn_threshold_sits_above_ordinary_large_replies_and_below_the_incident() {
        let ordinary_large = 80_000;
        let the_actual_incident = 873_000;
        assert!(ordinary_large < LARGE_TOOL_REPLY_WARN_CHARS);
        assert!(the_actual_incident > LARGE_TOOL_REPLY_WARN_CHARS);
    }

    /// Regression, caught by this instrumentation on its first real run:
    /// `xfa_render_page` legitimately replies with well over
    /// 300,000 base64 characters for one full-page render — bounded to
    /// under 1,600 real tokens by the vision encoder regardless — and the
    /// size warning must not fire on it. Warning on every ordinary page
    /// render would train the operator to ignore the warning by the time a
    /// reply like `generate_html`'s actually needed it.
    #[test]
    fn a_large_image_reply_does_not_trip_the_text_warning() {
        let big_page_render = "x".repeat(400_000);
        let reply = image_reply(vec![big_page_render]);

        assert_eq!(
            text_reply_chars(&reply),
            0,
            "an image reply carries no text at all"
        );
        // The full size is still recorded on the event, just not what
        // gates the warning.
        assert!(reply_size_chars(&reply) > LARGE_TOOL_REPLY_WARN_CHARS);
    }

    /// The distinction that makes the warning meaningful at all: the same
    /// number of characters warns as text (the shape the actual incident
    /// took) but not as an image (a shape that is already cost-bounded).
    #[test]
    fn the_same_size_warns_as_text_but_not_as_an_image() {
        let payload = "x".repeat(LARGE_TOOL_REPLY_WARN_CHARS + 1);

        assert!(text_reply_chars(&ToolReply::Text(payload.clone())) > LARGE_TOOL_REPLY_WARN_CHARS);
        assert_eq!(
            text_reply_chars(&image_reply(vec![payload])),
            0
        );
    }

    /// The wiring both `ToolFinished` call sites share: an oversized text
    /// reply produces a warning naming the tool and the size, a
    /// legitimately large image reply produces none.
    #[test]
    fn oversized_reply_warning_names_the_tool_for_text_but_stays_silent_for_images() {
        let big_text = ToolReply::Text("x".repeat(LARGE_TOOL_REPLY_WARN_CHARS + 1));
        let warning = oversized_reply_warning("generate_html", &big_text)
            .expect("an oversized text reply must warn");
        assert!(warning.contains("generate_html"), "{warning}");
        assert!(warning.contains(&(LARGE_TOOL_REPLY_WARN_CHARS + 1).to_string()), "{warning}");

        let fits = ToolReply::Text("small".into());
        assert!(oversized_reply_warning("get_source_info", &fits).is_none());

        let big_image = image_reply(vec!["x".repeat(400_000)]);
        assert!(oversized_reply_warning("xfa_render_page", &big_image).is_none());
    }

    #[test]
    fn transient_errors_are_retried_automatically() {
        // The failure seen when the machine is left alone mid-run.
        assert!(is_transient_error(
            "LLM API error: HttpError: error decoding response body — error reading a \
             body from connection — timed out"
        ));
        // The transport's wording for a status error. `runner`'s
        // `transient_statuses_match_what_the_transport_writes` is what keeps
        // these two in step across the crate boundary.
        assert!(is_transient_error(
            "LLM API error: ProviderResponseError: status 529: overloaded [status: 529]"
        ));
        assert!(is_transient_error(
            "LLM API error: ProviderResponseError: status 429: rate limit [status: 429] \
             [retry-after: 30]"
        ));
        assert!(is_transient_error(
            "LLM API error: ProviderResponseError: status 503: upstream [status: 503]"
        ));
        assert!(is_transient_error(
            "LLM API error: ProviderResponseError: status 502: upstream [status: 502]"
        ));
        // Client-side mistakes would just fail again — those go straight to the
        // user's Retry prompt instead of burning automatic attempts.
        assert!(!is_transient_error(
            "LLM API error: ProviderResponseError: status 401: invalid x-api-key \
             [status: 401]"
        ));
        assert!(!is_transient_error(
            "LLM API error: ProviderResponseError: status 400: prompt is too long \
             [status: 400]"
        ));
        assert!(!is_transient_error(
            "Anthropic API key is not configured. Open Settings and paste your API key."
        ));
    }

    /// The backoff between automatic retries can be a minute long, so an abort
    /// during it has to wake the run instead of holding it open.
    #[tokio::test]
    async fn the_retry_backoff_wakes_early_when_the_run_is_aborted() {
        let abort = AbortFlag::default();
        abort.abort();

        let started = std::time::Instant::now();
        let aborted = sleep_unless_aborted(std::time::Duration::from_secs(60), &abort).await;

        assert!(aborted, "an aborted wait must report it");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}, so it slept through the backoff",
            started.elapsed()
        );
    }

    /// An un-aborted wait still waits, otherwise the retry backoff would be gone.
    #[tokio::test]
    async fn an_untouched_wait_sleeps_for_its_full_duration() {
        let abort = AbortFlag::default();

        let started = std::time::Instant::now();
        let aborted = sleep_unless_aborted(std::time::Duration::from_millis(500), &abort).await;

        assert!(!aborted);
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(450),
            "returned after {:?}",
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod controller {
    //! End-to-end sequencing tests.
    //!
    //! These are the reason the controller left the UI crate: rig's own
    //! `MockCompletionModel` drives the real `run` over a real
    //! `ConversionAgent`, with a recording `RunObserver`, no network and no
    //! desktop runtime.

    use super::*;
    use agent::ConversionAgent;
    use crate::observer::RunObserver;
    use rig_core::completion::Usage;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
    use std::sync::{Arc, Mutex};

    fn no_price() -> PriceFn {
        Arc::new(|_| None)
    }

    #[derive(Default)]
    struct Recorder {
        events: Vec<RunEvent>,
        traces: Vec<TraceEvent>,
        /// What to answer when the controller pauses on a failed turn.
        answer: Option<RetryAction>,
        prompts: usize,
    }

    impl Recorder {
        fn stages(&self) -> Vec<&str> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    RunEvent::Stage { role, .. } => Some(*role),
                    _ => None,
                })
                .collect()
        }

        fn warnings(&self) -> Vec<&str> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    RunEvent::Warning(w) => Some(w.as_str()),
                    _ => None,
                })
                .collect()
        }

        fn aborted(&self) -> bool {
            self.events.iter().any(|e| matches!(e, RunEvent::Aborted))
        }
    }

    impl RunObserver for Recorder {
        fn emit(&mut self, event: RunEvent) {
            self.events.push(event);
        }
        fn retry_prompt(&mut self, _role: &str, _error: &str) {
            self.prompts += 1;
        }
        fn poll_retry(&mut self) -> Option<RetryAction> {
            self.answer
        }
        fn retry_resolved(&mut self, _action: RetryAction) {}
        fn trace(&mut self, event: TraceEvent) {
            self.traces.push(event);
        }
    }

    impl Recorder {
        /// How each stage ended, in order.
        fn stage_ends(&self) -> Vec<(String, StageEnd)> {
            self.traces
                .iter()
                .filter_map(|t| match t {
                    TraceEvent::StageFinished { stage, ended, .. } => Some((stage.clone(), ended.clone())),
                    _ => None,
                })
                .collect()
        }

        /// Every review verdict: round, approval, report.
        fn verdicts(&self) -> Vec<(usize, Option<bool>, String)> {
            self.traces
                .iter()
                .filter_map(|t| match t {
                    TraceEvent::ReviewVerdict { round, approved, report, .. } => {
                        Some((*round, *approved, report.clone()))
                    }
                    _ => None,
                })
                .collect()
        }

        fn controls(&self) -> Vec<(ControlKind, String)> {
            self.traces
                .iter()
                .filter_map(|t| match t {
                    TraceEvent::Control { kind, detail, .. } => Some((*kind, detail.clone())),
                    _ => None,
                })
                .collect()
        }
    }

    /// A recorder plus the `Arc` a test reads it back through — `SharedObserver`
    /// erases the concrete type, so this is what lets a test still get at what
    /// it recorded once the run has returned.
    fn recorder() -> (SharedObserver, Arc<Mutex<Recorder>>) {
        recorder_with(Recorder::default())
    }

    fn recorder_with_answer(answer: RetryAction) -> (SharedObserver, Arc<Mutex<Recorder>>) {
        recorder_with(Recorder {
            answer: Some(answer),
            ..Recorder::default()
        })
    }

    fn recorder_with(rec: Recorder) -> (SharedObserver, Arc<Mutex<Recorder>>) {
        let rec = Arc::new(Mutex::new(rec));
        (SharedObserver::from_arc(rec.clone()), rec)
    }

    /// The text of a user message, for asserting what a stage was asked to do.
    fn user_text(message: &Message) -> String {
        match message {
            Message::User { content } => content
                .iter()
                .filter_map(|c| match c {
                    UserContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        }
    }

    /// The system prompt a request was sent with.
    ///
    /// `AgentRunner::preamble` does not populate `CompletionRequest.preamble`
    /// (rig-agent's own request builder leaves that field unset for the
    /// `Agent::runner` path) — it prepends the preamble as a `Message::System`
    /// at `chat_history[0]` instead, verbatim, so that is where a test has to
    /// read it back from.
    fn turn_system(request: &rig_core::completion::CompletionRequest) -> String {
        match request.chat_history.first() {
            Some(Message::System { content }) => content.clone(),
            _ => String::new(),
        }
    }

    /// One scripted stage turn: plain text, the way the Analyst and Author
    /// answer.
    fn text_turn(text: &str) -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::text(text),
            MockStreamEvent::final_response(Usage::new()),
        ]
    }

    /// One scripted `submit_review` call — how the Reviewer stage always ends.
    fn review_turn(approved: bool, report: &str) -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::tool_call(
                "call-review",
                "submit_review",
                serde_json::json!({"approved": approved, "report": report}),
            ),
            MockStreamEvent::final_response(Usage::new()),
        ]
    }

    /// One scripted bare provider failure — the shape a mid-stream transport
    /// error takes, with no assembled turn behind it.
    fn error_turn(message: &str) -> Vec<MockStreamEvent> {
        vec![MockStreamEvent::error(message)]
    }

    /// A Redacto agent with no source: the scripted turns decide what runs, so
    /// the agent only has to be real enough to record a review and finalize.
    ///
    /// Session id deliberately empty: `build_stage_agent` only attaches
    /// `SqliteConversationMemory` (real `agent::db` I/O) when it is
    /// non-empty, and an ordinary controller test has not redirected
    /// `agent::db` to a scratch database — an ordinary non-empty id here
    /// would make every such test write into the developer's real
    /// `history.db`. Tests that specifically exercise memory persistence
    /// build their own agent with a real id and a scratch database instead
    /// (see `a_resumed_session_loads_its_prior_conversation`).
    fn bare_agent() -> SharedAgent {
        Arc::new(tokio::sync::Mutex::new(ConversionAgent::new(None, Vec::new(), String::new(), OutputTarget::Redacto)
            .expect("an agent without sources starts")))
    }

    /// A `ContextBudget` that shapes nothing and records nothing — every
    /// controller test's stages have nothing worth shaping, and the dedicated
    /// context-shaping tests (`a_restart_after_partial_progress_carries_turns_and_spend_forward`)
    /// build a real one directly instead of going through `config`.
    struct NoBudget;

    impl ContextBudget for NoBudget {
        fn policy(&self) -> Arc<dyn MemoryPolicy> {
            Arc::new(rig_memory::NoopMemoryPolicy)
        }
        fn raw_estimate(&self, _history: &[Message]) -> usize {
            0
        }
        fn record_actual(&self, _raw_estimate: usize, _real_tokens: u64) {}
    }

    fn no_budget() -> Arc<dyn ContextBudget> {
        Arc::new(NoBudget)
    }

    fn config(abort: AbortFlag, max_review_rounds: usize, model: MockCompletionModel) -> RunConfig {
        RunConfig {
            profile: None,
            target: OutputTarget::Redacto,
            abort,
            max_review_rounds,
            extra_instructions: String::new(),
            template_note: "",
            model: ModelHandle::new(model),
            price: no_price(),
            max_tokens: 4096,
            context_budget: no_budget(),
        }
    }

    fn shared(agent: ConversionAgent) -> SharedAgent {
        Arc::new(tokio::sync::Mutex::new(agent))
    }

    /// A Redacto agent with its verifier attached. Attaching touches no
    /// Docker; only a verifier call or the teardown would.
    fn redacto_agent_with_verifier() -> SharedAgent {
        let settings = agent::u2s::RedactoVerifySettings::default();
        let agent = ConversionAgent::new(None, Vec::new(), String::new(), OutputTarget::Redacto)
            .expect("an agent without sources starts")
            .with_redacto_verify(&settings)
            .expect("attaching the verifier needs no Docker");
        assert!(agent.has_verifier());
        shared(agent)
    }

    /// Whatever way a run ends, its verifier is torn down: no AEM or Postgres
    /// container outlives the run. The teardown itself may fail where no
    /// Docker runs, but it always detaches the verifier.
    #[tokio::test]
    async fn every_way_out_of_a_run_tears_the_verifier_down() {
        // Approved.
        let agent = redacto_agent_with_verifier();
        let model = MockCompletionModel::from_stream_turns([
            text_turn("PLAN"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let (obs, _) = recorder();
        let outcome = run(agent.clone(), config(AbortFlag::default(), 1, model), RunSeed::Fresh, obs).await;
        assert!(outcome.is_some());
        assert!(!agent.lock().await.has_verifier(), "torn down on approval");

        // Unapproved: the review rounds run out.
        let agent = redacto_agent_with_verifier();
        let model = MockCompletionModel::from_stream_turns([
            text_turn("PLAN"),
            text_turn("BUILT"),
            review_turn(false, "nope"),
            text_turn("FIXED"),
        ]);
        let (obs, _) = recorder();
        let outcome = run(agent.clone(), config(AbortFlag::default(), 1, model), RunSeed::Fresh, obs).await;
        assert!(outcome.is_some());
        assert!(!agent.lock().await.has_verifier(), "torn down when unapproved");

        // Aborted before the first turn.
        let agent = redacto_agent_with_verifier();
        let abort = AbortFlag::default();
        abort.abort();
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let (obs, _) = recorder();
        let outcome = run(agent.clone(), config(abort, 1, model), RunSeed::Fresh, obs).await;
        assert!(outcome.is_none());
        assert!(!agent.lock().await.has_verifier(), "torn down on abort");
    }

    /// A stage that burns its whole turn budget without finishing (no
    /// `submit_review`, no natural end) must not finalize silently: the
    /// hand-rolled loop warned on every `AgentRun` step error, budget
    /// exhaustion included, and an operator who never sees this warning has
    /// no way to know the Analyst's plan is a guess cut off mid-thought.
    #[tokio::test]
    async fn a_stage_that_exhausts_its_turn_budget_warns_rather_than_finishing_silently() {
        // A tool-free text turn is itself a natural end for a stage — the
        // model's plain answer, same as `text_turn` ending each stage in
        // every other controller test — so driving a stage all the way to
        // `MaxTurnsError` needs turns that keep the loop going instead: a
        // tool call, answered every time, never ends the stage on its own.
        // Redacto's Analyst budget is 25 turns; script exactly that many
        // (its `stuck_tool` is `None`, so the repeats never trip the stuck
        // watch instead) so the 26th completion call is refused with
        // `MaxTurnsError`, then one ordinary turn for the Author stage that
        // follows it.
        let repeat_turn = || {
            vec![
                MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(Usage::new()),
            ]
        };
        let mut turns: Vec<Vec<MockStreamEvent>> = std::iter::repeat_with(repeat_turn).take(25).collect();
        turns.push(text_turn("BUILT"));
        let model = MockCompletionModel::from_stream_turns(turns);
        let (obs, rec) = recorder();

        // No review rounds: the point of this test is the Analyst's budget,
        // not the Reviewer, and this keeps the script to exactly one stage's
        // overrun plus the Author's single ordinary turn.
        let outcome = run(bare_agent(), config(AbortFlag::default(), 0, model.clone()), RunSeed::Fresh, obs).await;

        assert!(
            outcome.is_some(),
            "a budget-exhausted stage still finalizes with whatever it built"
        );
        assert_eq!(model.request_count(), 26);
        let warnings: Vec<String> = rec
            .lock()
            .unwrap()
            .warnings()
            .iter()
            .map(|w| w.to_string())
            .collect();
        assert!(
            warnings.iter().any(|w| w.contains("Analyst") && w.contains("25-turn budget")),
            "the operator was never told the Analyst ran out of budget: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn a_fresh_run_sequences_analyst_then_author_then_reviewer() {
        let model = MockCompletionModel::from_stream_turns([
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let (obs, rec) = recorder();

        let outcome = run(bare_agent(), config(AbortFlag::default(), 2, model.clone()), RunSeed::Fresh, obs).await;

        assert!(outcome.is_some(), "an approved run produces a result");
        assert_eq!(rec.lock().unwrap().stages(), ["Analyst", "Author", "Reviewer"]);
        assert_eq!(model.request_count(), 3);
        // Approval means no "finalizing without a clean review" warning.
        let warnings = rec.lock().unwrap().warnings().iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(
            !warnings.iter().any(|w| w.contains("without a clean review")),
            "{warnings:?}"
        );
    }

    /// The Analyst's plan has to reach the Author's prompt, or the second stage
    /// re-derives everything the first one just worked out.
    #[tokio::test]
    async fn the_analysts_plan_is_pinned_into_the_authors_prompt() {
        let model = MockCompletionModel::from_stream_turns([
            text_turn("SECTION MAP: one heading, two fields"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let (obs, _) = recorder();

        run(bare_agent(), config(AbortFlag::default(), 1, model.clone()), RunSeed::Fresh, obs).await;

        let author_request = &model.requests()[1];
        let author_system = turn_system(author_request);
        assert!(
            author_system.contains("SECTION MAP: one heading, two fields"),
            "the Author never saw the plan: {author_system}"
        );
    }

    /// A rejected review sends the run back to the Author with the report
    /// pinned, then finalizes with a warning because it never got a clean pass.
    #[tokio::test]
    async fn a_rejected_review_drives_one_more_author_round() {
        let model = MockCompletionModel::from_stream_turns([
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(false, "The footer is missing."),
            text_turn("FIXED"),
        ]);
        let (obs, rec) = recorder();

        let outcome = run(bare_agent(), config(AbortFlag::default(), 1, model.clone()), RunSeed::Fresh, obs).await;

        assert!(outcome.is_some());
        assert_eq!(rec.lock().unwrap().stages(), ["Analyst", "Author", "Reviewer", "Author"]);
        let fix_request = &model.requests()[3];
        let fix_system = turn_system(fix_request);
        assert!(
            fix_system.contains("The footer is missing."),
            "the review report was not pinned into the fix round"
        );
        let warnings = rec.lock().unwrap().warnings().iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(
            warnings.iter().any(|w| w.contains("without a clean review")),
            "an unapproved run must say so: {warnings:?}"
        );
    }

    /// The run's final spend has to be every stage's usage summed — the
    /// reset this guards against: `run_stage` used to start its own `Spend`
    /// at zero on every call, so only the last stage's total ever reached the
    /// observer.
    #[tokio::test]
    async fn a_full_runs_spend_sums_every_stage() {
        let mut analyst_usage = Usage::new();
        analyst_usage.input_tokens = 100;
        let mut author_usage = Usage::new();
        author_usage.input_tokens = 50;
        let mut reviewer_usage = Usage::new();
        reviewer_usage.input_tokens = 25;

        let model = MockCompletionModel::from_stream_turns([
            vec![MockStreamEvent::text("THE PLAN"), MockStreamEvent::final_response(analyst_usage)],
            vec![MockStreamEvent::text("BUILT"), MockStreamEvent::final_response(author_usage)],
            vec![
                MockStreamEvent::tool_call(
                    "call-review",
                    "submit_review",
                    serde_json::json!({"approved": true, "report": ""}),
                ),
                MockStreamEvent::final_response(reviewer_usage),
            ],
        ]);
        let (obs, rec) = recorder();
        let price: PriceFn = Arc::new(|usage| Some(usage.input_tokens as f64 * 0.01));

        let mut cfg = config(AbortFlag::default(), 1, model.clone());
        cfg.price = price;

        run(bare_agent(), cfg, RunSeed::Fresh, obs).await;

        let events = rec.lock().unwrap();
        let last_spend = events
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                RunEvent::Spend(s) => Some(*s),
                _ => None,
            })
            .expect("a completed run reports spend");

        assert_eq!(
            last_spend.input_tokens, 175,
            "every stage's input tokens must be summed, not just the last stage's"
        );
        assert!(
            (last_spend.cost_usd.expect("priced model") - 1.75).abs() < 1e-9,
            "cost must sum the same way tokens do: {:?}",
            last_spend.cost_usd
        );
    }

    /// Feedback replaces the Analyst: the request becomes the first pinned
    /// review and the Author starts from it.
    #[tokio::test]
    async fn a_feedback_run_skips_the_analyst() {
        let model = MockCompletionModel::from_stream_turns([text_turn("FIXED"), review_turn(true, "")]);
        let (obs, rec) = recorder();

        run(
            bare_agent(),
            config(AbortFlag::default(), 1, model.clone()),
            RunSeed::Feedback("Make the title bigger.".into()),
            obs,
        )
        .await;

        assert_eq!(rec.lock().unwrap().stages(), ["Author", "Reviewer"]);
        let author_system = turn_system(&model.requests()[0]);
        assert!(
            author_system.contains("Make the title bigger."),
            "the feedback never reached the Author"
        );
    }

    /// Continuing a reopened session skips the Analyst too, and pins nothing:
    /// the tree the previous run left is the whole brief, so an empty CONVERSION
    /// PLAN or REVIEW FEEDBACK heading here would be an instruction to nothing.
    #[tokio::test]
    async fn a_continued_run_skips_the_analyst_and_pins_nothing() {
        let model = MockCompletionModel::from_stream_turns([text_turn("FINISHED"), review_turn(true, "")]);
        let (obs, rec) = recorder();

        run(bare_agent(), config(AbortFlag::default(), 1, model.clone()), RunSeed::Continue, obs).await;

        assert_eq!(rec.lock().unwrap().stages(), ["Author", "Reviewer"]);
        let author_request = &model.requests()[0];
        let author_system = turn_system(author_request);
        assert!(
            !author_system.contains("## CONVERSION PLAN"),
            "a continuation has no plan to pin"
        );
        assert!(
            !author_system.contains("## REVIEW FEEDBACK"),
            "a continuation has no feedback to pin"
        );
        // The seeded tree is the whole brief, so the one thing the Author must
        // be told is not to throw it away and author afresh.
        let author_asked = author_request
            .chat_history
            .last()
            .map(user_text)
            .unwrap_or_default();
        assert!(
            author_asked.contains("Do not start over"),
            "the Author was opened with {author_asked:?}"
        );
    }

    /// Blank feedback must not become a pinned review with nothing in it: the
    /// operator pressing Send on an empty field means "carry on", and an Author
    /// stage spent on an empty brief is a billed run that says nothing.
    #[test]
    fn blank_feedback_resumes_rather_than_pinning_an_empty_review() {
        assert!(matches!(RunSeed::resuming(""), RunSeed::Continue));
        assert!(matches!(RunSeed::resuming("   \n\t "), RunSeed::Continue));
        match RunSeed::resuming("  make the title bigger  ") {
            RunSeed::Feedback(text) => assert_eq!(text, "make the title bigger"),
            _ => panic!("real feedback has to stay feedback"),
        }
    }

    /// Aborting before the first turn stops the run without a result, and says
    /// so exactly once per checkpoint rather than silently finishing.
    #[tokio::test]
    async fn an_aborted_run_produces_no_outcome() {
        let abort = AbortFlag::default();
        abort.abort();
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let (obs, rec) = recorder();

        let outcome = run(bare_agent(), config(abort, 1, model.clone()), RunSeed::Fresh, obs).await;

        assert!(outcome.is_none(), "an aborted run has nothing to publish");
        assert!(rec.lock().unwrap().aborted());
        assert_eq!(model.request_count(), 0, "no turn should be attempted");
    }

    /// A permanent failure pauses the run and asks. Answering Cancel ends it
    /// with no result — and, critically, without retrying.
    #[tokio::test]
    async fn giving_up_at_the_retry_prompt_ends_the_run() {
        let model = MockCompletionModel::from_stream_turns([error_turn("Anthropic API error (400 Bad Request)")]);
        let (obs, rec) = recorder_with_answer(RetryAction::Cancel);

        let outcome = run(bare_agent(), config(AbortFlag::default(), 1, model.clone()), RunSeed::Fresh, obs).await;

        assert!(outcome.is_none());
        assert_eq!(rec.lock().unwrap().prompts, 1, "the operator should be asked exactly once");
        assert_eq!(model.request_count(), 1, "a 400 must not be retried");
    }

    /// Answering Retry re-sends the same turn rather than restarting the stage.
    #[tokio::test]
    async fn retrying_re_sends_the_failed_turn() {
        let model = MockCompletionModel::from_stream_turns([
            error_turn("Anthropic API error (400 Bad Request)"),
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let (obs, rec) = recorder_with_answer(RetryAction::Retry);

        let outcome = run(bare_agent(), config(AbortFlag::default(), 1, model.clone()), RunSeed::Fresh, obs).await;

        assert!(outcome.is_some(), "the retry should carry the run to completion");
        assert_eq!(rec.lock().unwrap().prompts, 1);
        assert_eq!(model.request_count(), 4);
        // The retried turn is the Analyst's, not a fresh stage.
        assert_eq!(rec.lock().unwrap().stages(), ["Analyst", "Author", "Reviewer"]);
    }

    /// A role with a turn budget tight enough to reach in two scripted turns,
    /// so a restart's carried-over count is provable rather than merely
    /// plausible.
    const TINY_BUDGET: Role = Role {
        name: "Test",
        scope: agent::scope::AEM_ANALYST,
        max_iterations: 2,
        stuck_tool: None,
        stuck_activity: "testing",
        max_tokens_nudge: "nudge incrementally",
    };

    /// A restart after a turn already succeeded must not lose what that turn
    /// billed, nor hand the fresh attempt the stage's *whole* turn budget
    /// again — both would be a restart quietly cheating either the operator's
    /// spend total or the point of having a turn budget at all. Proven from
    /// outside `StageHook`'s own fields, through the one thing they can
    /// actually move: the events `run_stage` emits and the turns the model
    /// actually sees.
    #[tokio::test]
    async fn a_restart_after_partial_progress_carries_turns_and_spend_forward() {
        let mut usage_first = Usage::new();
        usage_first.input_tokens = 100;
        let mut usage_after_restart = Usage::new();
        usage_after_restart.input_tokens = 300;

        // Each success is a tool call, not a bare text answer: a plain text
        // turn is itself the natural end of a stage (nothing forces the loop
        // to continue), so proving a *budget* boundary needs turns that keep
        // going on their own — get_source_info is in `TINY_BUDGET`'s scope
        // and safe to call repeatedly with no side effects.
        let tool_call_turn = |usage| {
            vec![
                MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(usage),
            ]
        };
        let model = MockCompletionModel::from_stream_turns([
            // Turn 1 of the 2-turn budget: succeeds, billing 100 input tokens.
            tool_call_turn(usage_first),
            // A non-transient failure: goes straight to the operator's retry
            // prompt, which the recorder below answers immediately — no real
            // backoff sleep, unlike a transient error's automatic retry.
            vec![MockStreamEvent::error("Anthropic API error (400 Bad Request)")],
            // The restart's own turn 2 of 2: succeeds, billing 300 more —
            // reaching the budget exactly, so the next completion call (a
            // tool call keeps the loop open, so there would be one) is what
            // `MaxTurnsError`s rather than a natural end.
            tool_call_turn(usage_after_restart),
            // A 4th scripted turn the stage must never reach: reachable only
            // if the restart wrongly reset the budget back to a fresh 2
            // rather than resuming with 1 remaining (1 already spent before
            // the failure).
            tool_call_turn(Usage::new()),
        ]);
        let (obs, rec) = recorder_with_answer(RetryAction::Retry);
        let price: PriceFn = Arc::new(|usage| Some(usage.input_tokens as f64 * 0.001));

        let text = run_stage(
            &bare_agent(),
            &TINY_BUDGET,
            "system prompt",
            "seed",
            &AbortFlag::default(),
            ModelHandle::new(model.clone()),
            price,
            4096,
            no_budget(),
            &obs,
            &mut Spend::default(),
        )
        .await;

        assert!(text.is_some(), "the stage still finishes with whatever it produced");
        assert_eq!(
            model.request_count(),
            3,
            "the restart must resume with 1 turn remaining, not the stage's whole 2-turn budget"
        );

        let events = rec.lock().unwrap();
        let last_spend = events
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                RunEvent::Spend(s) => Some(*s),
                _ => None,
            })
            .expect("a completed turn reports spend");
        assert_eq!(
            last_spend.input_tokens, 400,
            "the turn billed before the failure must still count toward the total"
        );
    }

    /// A stage run under a real session id against the scratch database, and
    /// what memory holds for it afterwards.
    async fn stored_after(
        role: &'static Role,
        turns: Vec<Vec<MockStreamEvent>>,
    ) -> (Vec<Message>, MockCompletionModel) {
        use rig_core::memory::ConversationMemory;
        let session_id = format!("persist-test-{}", uuid::Uuid::new_v4());
        let agent = Arc::new(tokio::sync::Mutex::new(
            ConversionAgent::new(None, Vec::new(), session_id.clone(), OutputTarget::Redacto)
                .expect("an agent without sources starts"),
        ));
        let model = MockCompletionModel::from_stream_turns(turns);
        let (obs, _) = recorder();
        run_stage(
            &agent,
            role,
            "system",
            "the seed",
            &AbortFlag::default(),
            ModelHandle::new(model.clone()),
            no_price(),
            4096,
            no_budget(),
            &obs,
            &mut Spend::default(),
        )
        .await;
        let stored = crate::memory::SqliteConversationMemory
            .load(&crate::memory::conversation_id(&session_id, role.name))
            .await
            .expect("memory loads");
        (stored, model)
    }

    fn seeds(messages: &[Message]) -> usize {
        messages.iter().filter(|m| user_text(m).contains("the seed")).count()
    }

    fn calls_tool(messages: &[Message], tool: &str) -> bool {
        messages.iter().any(|m| match m {
            Message::Assistant { content, .. } => content
                .iter()
                .any(|c| matches!(c, AssistantContent::ToolCall(t) if t.function.name == tool)),
            _ => false,
        })
    }

    /// The Reviewer always ends through `submit_review`, a hook stop rather
    /// than a natural end, and rig saves a conversation only on a natural end:
    /// its conversation, and the tool results a diagnosis needs, were never
    /// stored.
    #[tokio::test]
    async fn a_review_ended_by_submit_review_is_stored() {
        let _guard = crate::memory::test_support::use_scratch_db().await;
        let role = &roles::roles_for(OutputTarget::Redacto).reviewer;
        let (stored, _) = stored_after(role, vec![review_turn(true, "fine")]).await;
        assert_eq!(seeds(&stored), 1, "{stored:?}");
        assert!(calls_tool(&stored, "submit_review"), "{stored:?}");
    }

    /// A stage cut off by its turn budget keeps what it did.
    #[tokio::test]
    async fn a_stage_that_runs_out_of_turns_is_stored() {
        let _guard = crate::memory::test_support::use_scratch_db().await;
        let role = &roles::roles_for(OutputTarget::Redacto).analyst;
        let turns = std::iter::repeat_with(|| {
            vec![
                MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(Usage::new()),
            ]
        })
        .take(role.max_iterations)
        .collect();
        let (stored, _) = stored_after(role, turns).await;
        assert_eq!(seeds(&stored), 1);
        assert!(calls_tool(&stored, "get_source_info"), "{stored:?}");
    }

    /// A restart after a transient failure resumes from explicit history, and
    /// explicit history bypasses rig's memory altogether: a stage that ever
    /// retried was not stored at all, even when it then finished. It is stored
    /// whole, and once.
    #[tokio::test]
    async fn a_stage_restarted_after_a_transient_failure_is_stored_once() {
        let _guard = crate::memory::test_support::use_scratch_db().await;
        let role = &roles::roles_for(OutputTarget::Redacto).author;
        let (stored, model) = stored_after(
            role,
            vec![
                vec![
                    MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                    MockStreamEvent::final_response(Usage::new()),
                ],
                error_turn("overloaded_error: 529"),
                text_turn("built after the retry"),
            ],
        )
        .await;
        assert_eq!(model.request_count(), 3);
        assert_eq!(seeds(&stored), 1, "{stored:?}");
        assert!(calls_tool(&stored, "get_source_info"), "{stored:?}");
        let finished = stored.iter().any(|m| match m {
            Message::Assistant { content, .. } => content.iter().any(|c| {
                matches!(c, AssistantContent::Text(t) if t.text.contains("built after the retry"))
            }),
            _ => false,
        });
        assert!(finished, "{stored:?}");
    }

    /// Every tool call id in `history` that no later user message answers.
    fn unanswered_calls(history: &[Message]) -> Vec<String> {
        let calls = history.iter().flat_map(|m| match m {
            Message::Assistant { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::ToolCall(call) => Some(call.id.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        });
        let answered: Vec<String> = history
            .iter()
            .flat_map(|m| match m {
                Message::User { content } => content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::ToolResult(result) => Some(result.call.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        calls.filter(|id| !answered.contains(id)).collect()
    }

    /// A stage that `submit_review` ends stops before rig records that call's
    /// result. The next round of the same stage loads that conversation, and a
    /// provider refuses a tool call left unanswered (`tool_use ids were found
    /// without tool_result blocks`), so the stored conversation has to answer
    /// every call it makes.
    #[tokio::test]
    async fn a_second_review_round_resumes_a_conversation_with_every_call_answered() {
        let _guard = crate::memory::test_support::use_scratch_db().await;
        let session_id = format!("review-rounds-{}", uuid::Uuid::new_v4());
        let role = &roles::roles_for(OutputTarget::Redacto).reviewer;
        let agent = || {
            Arc::new(tokio::sync::Mutex::new(
                ConversionAgent::new(None, Vec::new(), session_id.clone(), OutputTarget::Redacto)
                    .expect("an agent without sources starts"),
            ))
        };
        let review = |model: MockCompletionModel| {
            let agent = agent();
            async move {
                let (obs, _) = recorder();
                run_stage(
                    &agent,
                    role,
                    "system",
                    "review it",
                    &AbortFlag::default(),
                    ModelHandle::new(model),
                    no_price(),
                    4096,
                    no_budget(),
                    &obs,
                    &mut Spend::default(),
                )
                .await
            }
        };

        review(MockCompletionModel::from_stream_turns([review_turn(false, "The footer is missing.")])).await;
        let second = MockCompletionModel::from_stream_turns([review_turn(true, "")]);
        review(second.clone()).await;

        let sent = &second.requests()[0].chat_history;
        assert!(
            sent.iter().any(|m| matches!(m, Message::Assistant { .. })),
            "the second round must resume the first round's conversation: {sent:?}"
        );
        assert_eq!(unanswered_calls(sent), Vec::<String>::new(), "{sent:?}");
    }

    /// A conversation stored before calls were answered on the way out still
    /// resumes: the missing results are put right after their calls, ahead of
    /// whatever the next user message said.
    #[test]
    fn unanswered_calls_get_a_result_right_after_them() {
        let call = |id: &str| {
            Message::Assistant {
                id: None,
                content: vec![AssistantContent::tool_call(id, "submit_review", serde_json::json!({}))],
            }
        };
        let history = vec![Message::user("review it"), call("a"), Message::user("again"), call("b")];

        let repaired = answer_unanswered_tool_calls(history);

        assert_eq!(unanswered_calls(&repaired), Vec::<String>::new(), "{repaired:?}");
        assert_eq!(repaired.len(), 5, "{repaired:?}");
        match &repaired[2] {
            Message::User { content } => {
                assert!(matches!(content.first(), Some(UserContent::ToolResult(_))), "{content:?}");
                assert_eq!(user_text(&repaired[2]), "again");
            }
            other => panic!("the call's result must follow it: {other:?}"),
        }
        assert!(matches!(&repaired[4], Message::User { .. }), "{repaired:?}");
        assert_eq!(answer_unanswered_tool_calls(repaired.clone()), repaired, "the repair is idempotent");
    }

    /// Parallel calls answered in part keep the answered result and gain the
    /// missing one, results ahead of the message's text.
    #[test]
    fn a_partly_answered_parallel_turn_gains_only_the_missing_result() {
        let calls = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::tool_call("a", "json_get", serde_json::json!({})),
                AssistantContent::tool_call("b", "json_get", serde_json::json!({})),
            ],
        };
        let answered_a = Message::User {
            content: vec![UserContent::tool_result(
                "a",
                "json_get",
                vec![ToolResultContent::Text("{}".to_string().into())],
            )],
        };
        let repaired = answer_unanswered_tool_calls(vec![Message::user("go"), calls, answered_a]);

        assert_eq!(unanswered_calls(&repaired), Vec::<String>::new(), "{repaired:?}");
        assert_eq!(repaired.len(), 3, "{repaired:?}");
        let Message::User { content } = &repaired[2] else { panic!("{repaired:?}") };
        assert_eq!(content.len(), 2, "one result per call, none duplicated: {content:?}");
    }

    /// A run's trace has everything the analysis needs: every stage opened
    /// and closed with how it ended, every turn with its tool calls' full
    /// arguments, every tool call paired start to finish, and the Reviewer's
    /// verdict with its report.
    #[tokio::test]
    async fn a_run_traces_every_stage_turn_and_tool_call() {
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("call-1", "get_source_info", serde_json::json!({"page": 2})),
                MockStreamEvent::final_response(Usage::new()),
            ],
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(false, "The footer is missing."),
            text_turn("FIXED"),
        ]);
        let (obs, rec) = recorder();

        run(bare_agent(), config(AbortFlag::default(), 1, model.clone()), RunSeed::Fresh, obs).await;

        let rec = rec.lock().unwrap();
        let started: Vec<&str> = rec
            .traces
            .iter()
            .filter_map(|t| match t {
                TraceEvent::StageStarted { stage, system_prompt, seed_message, .. } => {
                    assert!(!system_prompt.is_empty(), "the stage's prompt is traced in full");
                    assert!(!seed_message.is_empty());
                    Some(stage.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(started, ["Analyst", "Author", "Reviewer", "Author"]);
        assert_eq!(
            rec.stage_ends(),
            [
                ("Analyst".to_string(), StageEnd::Finished),
                ("Author".to_string(), StageEnd::Finished),
                ("Reviewer".to_string(), StageEnd::ReviewSubmitted),
                ("Author".to_string(), StageEnd::Finished),
            ]
        );

        let analyst_turns: Vec<(usize, Vec<String>)> = rec
            .traces
            .iter()
            .filter_map(|t| match t {
                TraceEvent::TurnFinished { stage, turn, tool_calls, .. } if stage == "Analyst" => {
                    Some((*turn, tool_calls.iter().map(|c| c.args.to_string()).collect()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            analyst_turns,
            [(1, vec![r#"{"page":2}"#.to_string()]), (2, vec![])],
            "turns are numbered within the stage and carry their calls' full arguments"
        );

        let tool_started = rec.traces.iter().find_map(|t| match t {
            TraceEvent::ToolStarted { name, args, turn, call_id, .. } if name == "get_source_info" => {
                Some((args.clone(), *turn, call_id.clone()))
            }
            _ => None,
        });
        let (args, turn, call_id) = tool_started.expect("the tool call is traced when it starts");
        assert_eq!(args, serde_json::json!({"page": 2}));
        assert_eq!(turn, 1, "a call belongs to the turn that asked for it");
        let finished = rec.traces.iter().any(|t| {
            matches!(t, TraceEvent::ToolFinished { call_id: id, name, result_hash, .. }
                if *id == call_id && name == "get_source_info" && result_hash.len() == 16)
        });
        assert!(finished, "the call's finish pairs with its start by id");

        assert_eq!(
            rec.verdicts(),
            [(1, Some(false), "The footer is missing.".to_string())],
            "the verdict is traced with its round and report"
        );
    }

    /// A Reviewer that ends without `submit_review` gave no verdict — traced
    /// as such, not as a rejection, so an analysis never counts a Reviewer
    /// that ran out of budget as one that found problems.
    #[tokio::test]
    async fn a_review_round_without_a_verdict_is_traced_as_none() {
        let model = MockCompletionModel::from_stream_turns([
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            text_turn("I looked at it."),
        ]);
        let (obs, rec) = recorder();

        run(bare_agent(), config(AbortFlag::default(), 1, model), RunSeed::Fresh, obs).await;

        assert_eq!(rec.lock().unwrap().verdicts(), [(1, None, String::new())]);
    }

    /// A role scoped like the Analyst whose stuck watch is on `get_source_info`,
    /// which answers identically on an unchanged agent.
    const STUCK_ON_SOURCE_INFO: Role = Role {
        name: "Test",
        scope: agent::scope::AEM_ANALYST,
        max_iterations: 10,
        stuck_tool: Some("get_source_info"),
        stuck_activity: "testing",
        max_tokens_nudge: "nudge incrementally",
    };

    async fn traced_stage(
        role: &'static Role,
        turns: Vec<Vec<MockStreamEvent>>,
        abort: AbortFlag,
    ) -> Arc<Mutex<Recorder>> {
        let (obs, rec) = recorder_with_answer(RetryAction::Cancel);
        run_stage(
            &bare_agent(),
            role,
            "system prompt",
            "seed",
            &abort,
            ModelHandle::new(MockCompletionModel::from_stream_turns(turns)),
            no_price(),
            4096,
            no_budget(),
            &obs,
            &mut Spend::default(),
        )
        .await;
        rec
    }

    fn last_end(rec: &Arc<Mutex<Recorder>>) -> StageEnd {
        rec.lock().unwrap().stage_ends().last().expect("the stage was closed").1.clone()
    }

    fn control_kinds(rec: &Arc<Mutex<Recorder>>) -> Vec<ControlKind> {
        rec.lock().unwrap().controls().into_iter().map(|(k, _)| k).collect()
    }

    #[tokio::test]
    async fn the_stuck_watch_is_traced_as_a_stuck_stage() {
        let repeat = || {
            vec![
                MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(Usage::new()),
            ]
        };
        let rec = traced_stage(
            &STUCK_ON_SOURCE_INFO,
            std::iter::repeat_with(repeat).take(10).collect(),
            AbortFlag::default(),
        )
        .await;
        assert_eq!(control_kinds(&rec), [ControlKind::StuckStop]);
        assert_eq!(last_end(&rec), StageEnd::Stuck);
    }

    /// A transient failure is traced twice: the failed request with how long
    /// it took, then the automatic retry — and the retry is a second attempt.
    #[tokio::test]
    async fn a_transient_failure_is_traced_as_a_failed_request_and_a_retry() {
        let rec = traced_stage(
            &TINY_BUDGET,
            vec![
                // `retry-after: 0` so the test does not sleep through a backoff.
                error_turn("overloaded [retry-after: 0]"),
                text_turn("DONE"),
            ],
            AbortFlag::default(),
        )
        .await;
        let rec_guard = rec.lock().unwrap();
        let failed: Vec<(usize, usize, String)> = rec_guard
            .traces
            .iter()
            .filter_map(|t| match t {
                TraceEvent::RequestFailed { attempt, turn, error, .. } => Some((*attempt, *turn, error.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!((failed[0].0, failed[0].1), (1, 1));
        assert!(failed[0].2.contains("overloaded"), "{failed:?}");
        let attempts = rec_guard
            .traces
            .iter()
            .filter(|t| matches!(t, TraceEvent::AttemptStarted { .. }))
            .count();
        assert_eq!(attempts, 2);
        drop(rec_guard);
        assert_eq!(control_kinds(&rec), [ControlKind::TransientRetry]);
        assert_eq!(last_end(&rec), StageEnd::Finished);
    }

    #[tokio::test]
    async fn a_call_to_a_tool_the_stage_does_not_have_is_traced() {
        let rec = traced_stage(
            &TINY_BUDGET,
            vec![
                vec![
                    MockStreamEvent::tool_call("call", "no_such_tool", serde_json::json!({"x": 1})),
                    MockStreamEvent::final_response(Usage::new()),
                ],
                text_turn("DONE"),
            ],
            AbortFlag::default(),
        )
        .await;
        let controls = rec.lock().unwrap().controls();
        assert_eq!(controls.len(), 1, "{controls:?}");
        assert_eq!(controls[0].0, ControlKind::InvalidToolCall);
        assert!(controls[0].1.contains("no_such_tool"), "{controls:?}");
    }

    #[tokio::test]
    async fn a_truncated_turn_is_traced_as_a_nudge() {
        use rig_core::completion::FinishReason;
        use rig_core::streaming::StreamFinal;
        let truncated = vec![
            MockStreamEvent::text("partial…"),
            MockStreamEvent::FinalResponse(
                StreamFinal::new(rig_core::test_utils::MOCK_PROVIDER, Usage::new())
                    .with_finish_reason(FinishReason::Length),
            ),
        ];
        let rec = traced_stage(&TINY_BUDGET, vec![truncated, text_turn("DONE")], AbortFlag::default()).await;
        assert_eq!(control_kinds(&rec), [ControlKind::OutputCapNudge]);
        let reasons: Vec<Option<String>> = rec
            .lock()
            .unwrap()
            .traces
            .iter()
            .filter_map(|t| match t {
                TraceEvent::TurnFinished { finish_reason, .. } => Some(finish_reason.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(reasons.first(), Some(&Some("length".to_string())));
    }

    #[tokio::test]
    async fn an_aborted_stage_is_traced_as_aborted() {
        let abort = AbortFlag::default();
        abort.abort();
        let rec = traced_stage(&TINY_BUDGET, vec![], abort).await;
        assert_eq!(control_kinds(&rec), [ControlKind::Aborted]);
        assert_eq!(last_end(&rec), StageEnd::Aborted);
    }

    /// A restart is the second attempt of the same stage, numbered on from the
    /// turn the failed attempt reached — and the operator's decision and the
    /// exhausted budget that follow are traced as the control events they are.
    #[tokio::test]
    async fn a_restart_is_traced_as_a_second_attempt_continuing_the_turn_count() {
        let tool_call_turn = || {
            vec![
                MockStreamEvent::tool_call("call", "get_source_info", serde_json::json!({})),
                MockStreamEvent::final_response(Usage::new()),
            ]
        };
        let model = MockCompletionModel::from_stream_turns([
            tool_call_turn(),
            vec![MockStreamEvent::error("Anthropic API error (400 Bad Request)")],
            tool_call_turn(),
        ]);
        let (obs, rec) = recorder_with_answer(RetryAction::Retry);

        run_stage(
            &bare_agent(),
            &TINY_BUDGET,
            "system prompt",
            "seed",
            &AbortFlag::default(),
            ModelHandle::new(model.clone()),
            no_price(),
            4096,
            no_budget(),
            &obs,
            &mut Spend::default(),
        )
        .await;

        let rec = rec.lock().unwrap();
        let attempts: Vec<(usize, usize)> = rec
            .traces
            .iter()
            .filter_map(|t| match t {
                TraceEvent::AttemptStarted { attempt, history_messages, .. } => Some((*attempt, *history_messages)),
                _ => None,
            })
            .collect();
        assert_eq!(attempts.len(), 2, "{attempts:?}");
        assert_eq!(attempts[0], (1, 0));
        assert!(attempts[1].1 > 0, "the restart resumes from the failed attempt's history");

        let turns: Vec<(usize, usize)> = rec
            .traces
            .iter()
            .filter_map(|t| match t {
                TraceEvent::TurnFinished { attempt, turn, .. } => Some((*attempt, *turn)),
                _ => None,
            })
            .collect();
        assert_eq!(turns, [(1, 1), (2, 2)], "turn numbers continue across the restart");

        let kinds: Vec<ControlKind> = rec.controls().into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            [
                ControlKind::OperatorPrompt,
                ControlKind::OperatorRetried,
                ControlKind::TurnBudgetExhausted
            ]
        );
        match rec.traces.last() {
            Some(TraceEvent::StageFinished { ended, turns, attempts, .. }) => {
                assert_eq!(*ended, StageEnd::TurnBudgetExhausted);
                assert_eq!((*turns, *attempts), (2, 2));
            }
            other => panic!("the stage's last trace event closes it: {other:?}"),
        }
    }

    /// Giving up at the retry prompt ends the stage as given up, not as an
    /// abort: the difference between "the operator stopped it" and "the API
    /// kept failing" is what an analysis of a failed run needs first.
    #[tokio::test]
    async fn giving_up_is_traced_as_giving_up() {
        let model = MockCompletionModel::from_stream_turns([error_turn("Anthropic API error (400 Bad Request)")]);
        let (obs, rec) = recorder_with_answer(RetryAction::Cancel);

        run(bare_agent(), config(AbortFlag::default(), 1, model), RunSeed::Fresh, obs).await;

        assert_eq!(
            rec.lock().unwrap().stage_ends(),
            [("Analyst".to_string(), StageEnd::GaveUp)]
        );
    }

    /// The whole point of wiring `SqliteConversationMemory` in
    /// `build_stage_agent`: a stage that reaches a natural end appends to it,
    /// and a *later* run of the same stage under the same session id — the
    /// shape our own resume flow (`RunSeed::Continue`/`Feedback`) actually
    /// takes — loads that history back before sending its own first turn.
    /// Uses a real, non-empty session id and a scratch database
    /// (`test_support::use_scratch_db`); every other controller test in this
    /// module deliberately uses an empty session id so it never touches
    /// `agent::db` at all — see `bare_agent`'s doc comment.
    #[tokio::test]
    async fn a_resumed_stage_loads_its_prior_conversation() {
        let _guard = crate::memory::test_support::use_scratch_db().await;
        let session_id = format!("resume-test-{}", uuid::Uuid::new_v4());
        let role = &roles::roles_for(OutputTarget::Redacto).author;

        let fresh_agent = || {
            Arc::new(tokio::sync::Mutex::new(
                ConversionAgent::new(None, Vec::new(), session_id.clone(), OutputTarget::Redacto)
                    .expect("an agent without sources starts"),
            ))
        };

        // First run: the Author answers with plain text, which is itself a
        // natural end for a stage — the only way this reaches `Done` and
        // actually triggers `ConversationMemory::append`.
        let first_model = MockCompletionModel::from_stream_turns([text_turn("first pass built")]);
        let (first_obs, _) = recorder();
        let first_text = run_stage(
            &fresh_agent(),
            role,
            "system",
            "seed one",
            &AbortFlag::default(),
            ModelHandle::new(first_model),
            no_price(),
            4096,
            no_budget(),
            &first_obs,
            &mut Spend::default(),
        )
        .await;
        assert_eq!(first_text.as_deref(), Some("first pass built"));

        // Second run: a brand new agent (a fresh process would build one too),
        // same session id. Its own first turn's request has to carry the
        // first run's stored messages, loaded before anything is sent.
        let second_model = MockCompletionModel::from_stream_turns([text_turn("second pass built")]);
        let (second_obs, _) = recorder();
        let second_text = run_stage(
            &fresh_agent(),
            role,
            "system",
            "seed two",
            &AbortFlag::default(),
            ModelHandle::new(second_model.clone()),
            no_price(),
            4096,
            no_budget(),
            &second_obs,
            &mut Spend::default(),
        )
        .await;
        assert_eq!(second_text.as_deref(), Some("second pass built"));

        let sent = &second_model.requests()[0];
        let carries_prior_turn = sent.chat_history.iter().any(|m| match m {
            Message::Assistant { content, .. } => content.iter().any(|c| {
                matches!(c, AssistantContent::Text(t) if t.text.contains("first pass built"))
            }),
            _ => false,
        });
        assert!(
            carries_prior_turn,
            "the second run's first request must carry the first run's stored \
             turn: {:?}",
            sent.chat_history
        );
    }
}
