//! The conversion controller: Analyst → Author → (Reviewer → Author-fix)* →
//! finalize, sequenced over one shared [`ConversionAgent`].
//!
//! Everything it needs from the outside arrives through two traits — a
//! [`TurnProvider`] for the model and a [`RunObserver`] for progress and retry
//! decisions — so the whole pipeline is drivable from a test with no network and
//! no UI framework.

use agent::{ConversionAgent, ToolReply};
use blueprint::{DocumentEnvelope, OutputTarget};

use crate::observer::{AbortFlag, RetryAction, RunEvent, RunObserver, Spend};
use crate::roles::{
    self, MAX_AUTO_RETRIES, MAX_MAX_TOKEN_NUDGES, MAX_RETRY_BACKOFF_SECS, MAX_VALIDATE_REPEATS,
    RETRY_BACKOFF_SECS, RETRY_POLL_MS, Role,
};
use rig_agent::agent::hook::RetryRequest;
use rig_agent::agent::run::{AgentRun, AgentRunStep};
use rig_core::completion::request::ResponseIdentity;
use rig_core::completion::{FinishReason, ToolDefinition};
use rig_core::message::{AssistantContent, Message};

use crate::turns::{ModelReply, TurnProvider, tool_definitions, tool_results};

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
    /// Whether an AEM connection is configured — gates the finalize upload step.
    pub has_aem_connection: bool,
}

/// What starts a run: a fresh analysis, or feedback on the previous result.
/// Feedback skips the Analyst and becomes the first pinned review.
pub enum RunSeed {
    Fresh,
    Feedback(String),
}

/// What a finished run produced.
pub struct RunOutcome {
    pub envelope: DocumentEnvelope,
    pub aem_package: Option<Vec<u8>>,
    /// The same package built with `bind_to_xsd` on: every field carries a
    /// `bindRef` and the schema is bundled. Offered as a separate download.
    pub aem_package_bound: Option<Vec<u8>>,
    pub xsd_schema: Option<String>,
    pub redacto_sql: Option<String>,
    pub form_code: Option<String>,
    pub aem_uploaded: bool,
    pub aem_form_path: Option<String>,
    /// Notes the run accumulated that did not stop it.
    pub warnings: Vec<String>,
}

/// Sequence the pipeline over `agent`.
///
/// `None` means the run ended before producing anything — the user aborted, or
/// gave up at a retry prompt. The observer has already been told why.
pub async fn run(
    mut agent: ConversionAgent,
    config: RunConfig,
    seed: RunSeed,
    turns: &impl TurnProvider,
    obs: &mut impl RunObserver,
) -> Option<RunOutcome> {
    let outcome = run_stages(&mut agent, &config, seed, turns, obs).await;
    // Every way out (approved, unapproved, aborted, given up at a retry
    // prompt) closes the browser, so no headless Chrome outlives the run.
    agent.shutdown_browser().await;
    outcome
}

/// The stages themselves; [`run`] wraps this with the teardown that must happen
/// on every exit path.
async fn run_stages(
    agent: &mut ConversionAgent,
    config: &RunConfig,
    seed: RunSeed,
    turns: &impl TurnProvider,
    obs: &mut impl RunObserver,
) -> Option<RunOutcome> {
    // Load the selected profile's fonts so on-demand renders have the right
    // typefaces (the font store is global, shared with rendering). Both a fresh
    // run and a feedback re-run funnel through here, so this covers both.
    if let Some(profile_name) = config.profile.as_deref() {
        let _ = blueprint::load_profile_fonts(profile_name);
    }

    let target = config.target;
    let extra = &config.extra_instructions;
    let stages = roles::roles_for(target);
    let mut plan = String::new();
    let mut reviews: Vec<String> = Vec::new();

    match &seed {
        // Feedback run: no Analyst; the request is the first pinned "review".
        RunSeed::Feedback(fb) => {
            reviews.push(format!("User feedback to apply to the form:\n{fb}"));
        }
        RunSeed::Fresh => {
            // ── Stage 1: Analyst → conversion plan ──────────────────────────
            obs.emit(RunEvent::Stage {
                role: "Analyst",
                doing: "analysing the source and researching precedents".into(),
            });
            plan = run_stage(
                agent,
                stages.analyst,
                &roles::sys_analyst(target, extra),
                "Analyse the source form and produce the detailed CONVERSION PLAN. \
                 Your final message is the plan.",
                &config.abort,
                turns,
                obs,
            )
            .await?; // fatal API error or abort, already surfaced
        }
    }

    // ── Stage 2: Author → build the artefact ────────────────────────────────
    obs.emit(RunEvent::Stage {
        role: "Author",
        doing: stages.author_doing.into(),
    });
    let author_seed = if reviews.is_empty() {
        stages.author_seed
    } else {
        stages.author_fix_seed
    };
    run_stage(
        agent,
        stages.author,
        &roles::sys_author(target, extra, config.template_note, &plan, &reviews),
        author_seed,
        &config.abort,
        turns,
        obs,
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
            agent,
            stages.reviewer,
            &roles::sys_reviewer(target, extra, &plan, &reviews),
            "Review the built form end to end against the source and the CONVERSION PLAN, \
             then finish by calling submit_review.",
            &config.abort,
            turns,
            obs,
        )
        .await?;

        match agent.take_review() {
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
                    agent,
                    stages.author,
                    &roles::sys_author(target, extra, config.template_note, &plan, &reviews),
                    stages.author_fix_seed,
                    &config.abort,
                    turns,
                    obs,
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

    // Building and uploading a CRX package is AEM-only; for any other target the
    // dump the Author already validated is the artefact, and calling this would
    // paint a failed build step on an otherwise successful run.
    if target == OutputTarget::Aem {
        ensure_built_and_uploaded(agent, config.has_aem_connection, obs).await;
    }

    Some(finalize(agent, config, warnings))
}

/// Assemble the run's artefacts from the agent's working trees.
fn finalize(
    agent: &mut ConversionAgent,
    config: &RunConfig,
    mut warnings: Vec<String>,
) -> RunOutcome {
    let profile = config.profile.clone();
    let agent::outputs::Outputs {
        envelope,
        redacto_sql,
        warnings: build_warnings,
    } = agent::outputs::build(agent, profile.as_deref());
    warnings.extend(build_warnings);

    let form_code = agent.form_code();

    // The schema describes an AEM form, so only that target offers it. The form
    // code has to be resolved first: it names the form.
    let xsd_schema = (config.target == OutputTarget::Aem)
        .then(|| agent::outputs::xsd_schema_for(&envelope, profile.as_deref(), form_code.as_deref()))
        .flatten();

    RunOutcome {
        envelope,
        aem_package: agent.package(),
        aem_package_bound: agent.package_bound(),
        xsd_schema,
        redacto_sql,
        form_code,
        aem_uploaded: agent.aem_uploaded(),
        aem_form_path: agent.aem_form_path(),
        warnings,
    }
}

// ── Turn-level failure recovery ──────────────────────────────────────────────

/// Whether a turn error looks transient — i.e. worth re-sending the same turn
/// unchanged.
pub(crate) fn is_transient_error(err: &str) -> bool {
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
    // `Anthropic API error (429 Too Many Requests): …` — retry the statuses the
    // API documents as retryable, but not 4xx client errors we'd just repeat.
    const RETRYABLE_STATUSES: &[&str] = &["(429", "(500", "(502", "(503", "(504", "(529"];

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
    obs: &mut impl RunObserver,
    abort: &AbortFlag,
    role: &str,
    err: &str,
) -> RetryAction {
    obs.retry_prompt(role, err);
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
    if action == RetryAction::Retry {
        obs.emit(RunEvent::Thought("Retrying the failed request…".into()));
    }
    action
}

/// Run one turn, absorbing transient failures: automatic retries with
/// exponential backoff first, then a pause that hands the decision to the
/// operator. `None` means the run should stop.
#[allow(clippy::too_many_arguments)]
async fn turn_with_retry(
    prompt: &Message,
    history: &[Message],
    tools: &[ToolDefinition],
    role: &Role,
    system: &str,
    abort: &AbortFlag,
    turns: &impl TurnProvider,
    obs: &mut impl RunObserver,
) -> Option<ModelReply> {
    let mut auto_retries = 0usize;
    loop {
        // The prompt and history are cloned per attempt: a retry must re-send
        // exactly what failed, and the provider evicts its own copy.
        match turns
            .call_model(prompt.clone(), history.to_vec(), tools, system, abort)
            .await
        {
            Ok(turn) => return Some(turn),
            Err(e) => {
                // An aborted turn is a stop, not a failure: it must not be
                // retried, nor hand the operator a Retry button for a run they
                // just asked to end.
                if abort.is_aborted() {
                    obs.emit(RunEvent::Aborted);
                    return None;
                }
                if is_transient_error(&e) && auto_retries < MAX_AUTO_RETRIES {
                    let backoff =
                        (RETRY_BACKOFF_SECS << auto_retries.min(4)).min(MAX_RETRY_BACKOFF_SECS);
                    // The provider's own answer wins over the computed backoff;
                    // otherwise spread the wait so parallel runs that failed
                    // together do not retry together.
                    let wait = parse_retry_after(&e)
                        .map(|secs| secs.min(MAX_RETRY_BACKOFF_SECS))
                        .unwrap_or_else(|| jittered_backoff(backoff, jitter_seed()));
                    auto_retries += 1;
                    obs.emit(RunEvent::Thought(format!(
                        "Request failed ({e}) — retrying in {wait}s \
                         (attempt {auto_retries} of {MAX_AUTO_RETRIES})."
                    )));
                    if sleep_unless_aborted(std::time::Duration::from_secs(wait), abort).await {
                        obs.emit(RunEvent::Aborted);
                        return None;
                    }
                    continue;
                }
                match await_user_retry(obs, abort, role.name, &e).await {
                    RetryAction::Retry => {
                        // A user-driven retry resets the automatic budget, so a
                        // long unattended stall can be resumed repeatedly.
                        auto_retries = 0;
                        continue;
                    }
                    RetryAction::Cancel => return None,
                }
            }
        }
    }
}

/// Watches one tool for "same answer, again and again". A stage that keeps
/// re-running `validate_aem_package` (or `build_redacto_dump`) on an unchanged
/// tree is going in circles, and the run has to move on rather than burn its
/// whole turn budget.
pub(crate) struct StuckWatch {
    /// The tool whose repeated identical output counts. `None` disables the watch.
    tool: Option<&'static str>,
    /// Digest of the last output seen from that tool.
    last: Option<u64>,
    repeats: usize,
}

impl StuckWatch {
    pub(crate) fn new(tool: Option<&'static str>) -> Self {
        Self {
            tool,
            last: None,
            repeats: 0,
        }
    }

    /// Record one tool result; `true` once the watched tool has produced the
    /// same output [`MAX_VALIDATE_REPEATS`] times running. Any other tool call
    /// means the stage is making progress, and resets the count.
    pub(crate) fn observe(&mut self, name: &str, reply: &ToolReply) -> bool {
        if self.tool != Some(name) {
            self.last = None;
            self.repeats = 0;
            return false;
        }

        // Hash rather than keep the text: a validation report can be large, and
        // all this needs is "same as last time?".
        let digest = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            match reply {
                ToolReply::Text(t) => (0u8, t).hash(&mut hasher),
                ToolReply::Error(e) => (1u8, e).hash(&mut hasher),
                ToolReply::Image { .. } => 2u8.hash(&mut hasher),
                ToolReply::Blocks(blocks) => {
                    3u8.hash(&mut hasher);
                    for block in blocks {
                        match block {
                            agent::ReplyBlock::Text(t) => (0u8, t).hash(&mut hasher),
                            agent::ReplyBlock::Image { .. } => 1u8.hash(&mut hasher),
                        }
                    }
                }
            }
            hasher.finish()
        };

        if self.last == Some(digest) {
            self.repeats += 1;
        } else {
            self.last = Some(digest);
            self.repeats = 1;
        }
        self.repeats >= MAX_VALIDATE_REPEATS
    }
}

/// Drive one stage to completion: a fresh run seeded with `seed_user_msg`, the
/// stage's scoped tool subset, and its `system` prompt, over the same
/// [`TurnProvider`] as every other stage. Returns the last non-tool assistant
/// message; `None` if the run should stop.
///
/// The loop is rig's [`AgentRun`] — a sans-IO state machine that owns turn
/// counting, tool-call validation and history threading, and hands every piece
/// of I/O back here. That is what lets the abort flag, the operator retry
/// prompt, the stuck watchdog and the observer events keep working exactly as
/// they did when this loop was hand-rolled.
pub(crate) async fn run_stage(
    agent: &mut ConversionAgent,
    role: &Role,
    system: &str,
    seed_user_msg: &str,
    abort: &AbortFlag,
    turns: &impl TurnProvider,
    obs: &mut impl RunObserver,
) -> Option<String> {
    let tools = tool_definitions(&agent.tools_for_stage(role.scope));
    let mut final_text = String::new();

    let mut stuck_watch = StuckWatch::new(role.stuck_tool);
    let mut consecutive_max_tokens: usize = 0;
    let mut spend = Spend::default();

    let mut run = AgentRun::new(seed_user_msg)
        .max_turns(role.max_iterations)
        // A truncated argument stream is the model's mistake to correct, not a
        // reason to fail the stage; repairing it is what the hand-rolled loop
        // approximated by substituting an empty input.
        .max_invalid_tool_call_retries(MAX_AUTO_RETRIES);

    loop {
        if abort.is_aborted() {
            obs.emit(RunEvent::Aborted);
            return None;
        }

        let step = match run.next_step() {
            Ok(step) => step,
            Err(e) => {
                obs.emit(RunEvent::Warning(format!("{}: {e}", role.name)));
                break;
            }
        };

        match step {
            AgentRunStep::CallModel {
                prompt, history, ..
            } => {
                // Transient failures are retried automatically, then handed to
                // the operator — a dropped connection must not throw away a
                // long run.
                let reply =
                    turn_with_retry(&prompt, &history, &tools, role, system, abort, turns, obs)
                        .await?;

                // Per-stage context-window fill indicator.
                if reply.prompt_tokens > 0 {
                    obs.emit(RunEvent::ContextUsed(reply.prompt_tokens));
                }
                spend.add(&reply.usage, reply.cost_usd);
                obs.emit(RunEvent::Spend(spend));
                if !reply.text.trim().is_empty() {
                    final_text = reply.text.trim().to_string();
                    obs.emit(RunEvent::Thought(final_text.clone()));
                }

                let finish_reason = reply.turn.finish_reason.clone();
                let had_tool_calls = reply
                    .turn
                    .choice
                    .iter()
                    .any(|c| matches!(c, AssistantContent::ToolCall(_)));

                // Usage does not reach the run on its own on the streamed path:
                // `StreamedTurn` carries none, so without this the run's totals
                // stay at zero and the spend report reads as free.
                if let Err(e) = run.record_streamed_completion_call(
                    reply.usage,
                    ResponseIdentity::default(),
                    finish_reason.clone(),
                    serde_json::Value::Null,
                ) {
                    obs.emit(RunEvent::Warning(format!("{}: {e}", role.name)));
                    break;
                }
                if let Err(e) = run.streamed_turn(reply.turn) {
                    obs.emit(RunEvent::Warning(format!("{}: {e}", role.name)));
                    break;
                }

                // A turn cut off at the output-token cap didn't decide to stop —
                // nudge toward incremental authoring rather than ending the
                // stage. Only a tool-free turn can be retried this way; one that
                // did request tools carries them into the next step as usual.
                if finish_reason == Some(FinishReason::Length)
                    && !had_tool_calls
                    && consecutive_max_tokens < MAX_MAX_TOKEN_NUDGES
                {
                    consecutive_max_tokens += 1;
                    if run
                        .retry_model_turn(RetryRequest::Feedback(role.max_tokens_nudge.to_string()))
                        .is_ok()
                    {
                        obs.emit(RunEvent::Thought(
                            "Turn hit the output-token limit — asking the agent to build the \
                             result incrementally instead of in one call."
                                .into(),
                        ));
                        continue;
                    }
                }
                if had_tool_calls {
                    consecutive_max_tokens = 0;
                }
            }

            AgentRunStep::CallTools { calls } => {
                let mut results = Vec::new();
                let mut stuck = false;
                let mut terminal = false;

                for call in calls {
                    if abort.is_aborted() {
                        obs.emit(RunEvent::Aborted);
                        return None;
                    }
                    let name = call.tool_call.function.name.clone();
                    let id = call.tool_call.id.clone();
                    let input = call.tool_call.function.arguments.clone();

                    obs.emit(RunEvent::ToolStarted {
                        id: id.as_str().to_string(),
                        name: name.clone(),
                        input_summary: summarize_input(&input),
                    });
                    let reply = agent.execute(&name, &input).await;
                    let ok = !matches!(reply, ToolReply::Error(_));
                    obs.emit(RunEvent::ToolFinished {
                        id: id.as_str().to_string(),
                        ok,
                    });
                    // A browser restart is the agent's business to perform and
                    // the operator's to know about.
                    for warning in agent.take_warnings() {
                        obs.emit(RunEvent::Warning(warning));
                    }

                    // `submit_review` ends the stage after its result is recorded.
                    if name == "submit_review" {
                        terminal = true;
                    }
                    stuck |= stuck_watch.observe(&name, &reply);

                    results.push((id, name, reply));
                }

                if let Err(e) = run.tool_results(tool_results(results)) {
                    obs.emit(RunEvent::Warning(format!("{}: {e}", role.name)));
                    break;
                }

                if terminal {
                    break;
                }
                if stuck {
                    obs.emit(RunEvent::Warning(format!(
                        "{}: {} produced the same result {} times in a row — moving on.",
                        role.name, role.stuck_activity, MAX_VALIDATE_REPEATS
                    )));
                    break;
                }
            }

            AgentRunStep::Done(_) => break,
        }
    }

    Some(final_text)
}

/// Run one of the agent's own tools as a visible finalize step. Returns whether
/// it succeeded.
async fn tool_step(
    agent: &mut ConversionAgent,
    id: &str,
    tool: &str,
    obs: &mut impl RunObserver,
) -> bool {
    obs.emit(RunEvent::ToolStarted {
        id: id.to_string(),
        name: tool.to_string(),
        input_summary: "finalize".into(),
    });
    let ok = !matches!(
        agent.execute(tool, &serde_json::json!({})).await,
        ToolReply::Error(_)
    );
    obs.emit(RunEvent::ToolFinished {
        id: id.to_string(),
        ok,
    });
    ok
}

/// Ensure the package reflects the latest tree (rebuild), then upload if an AEM
/// connection is configured and it hasn't been uploaded yet. Reuses the agent's
/// own tools.
async fn ensure_built_and_uploaded(
    agent: &mut ConversionAgent,
    has_aem_connection: bool,
    obs: &mut impl RunObserver,
) {
    let built = tool_step(agent, "finalize-build", "build_aem_package", obs).await;

    if built && has_aem_connection && !agent.aem_uploaded() {
        tool_step(agent, "finalize-upload", "upload_to_aem", obs).await;
    }
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

    
        /// The stuck guard is what stops a stage burning its whole turn budget
        /// re-validating an unchanged tree, so pin down when it fires — and when it
        /// must not.
        #[test]
        fn the_stuck_watch_fires_only_on_repeats_of_the_watched_tool() {
            let same = || ToolReply::Text("3 errors".to_string());
            let mut watch = StuckWatch::new(Some("validate_aem_package"));
    
            assert!(!watch.observe("validate_aem_package", &same()));
            assert!(!watch.observe("validate_aem_package", &same()));
            // Third identical result in a row: the stage is going in circles.
            assert!(watch.observe("validate_aem_package", &same()));
        }

    
        #[test]
        fn the_stuck_watch_resets_on_progress() {
            let mut watch = StuckWatch::new(Some("validate_aem_package"));
    
            assert!(!watch.observe("validate_aem_package", &ToolReply::Text("3 errors".into())));
            assert!(!watch.observe("validate_aem_package", &ToolReply::Text("3 errors".into())));
            // A different output means the tree changed.
            assert!(!watch.observe("validate_aem_package", &ToolReply::Text("1 error".into())));
            assert!(!watch.observe("validate_aem_package", &ToolReply::Text("1 error".into())));
    
            // An intervening edit resets the count too.
            assert!(!watch.observe("set_aem_translated_field", &ToolReply::Text("ok".into())));
            assert!(!watch.observe("validate_aem_package", &ToolReply::Text("1 error".into())));
        }

    
        /// A role with no watched tool must never report stuck, however repetitive.
        #[test]
        fn a_stage_without_a_stuck_tool_never_reports_stuck() {
            let mut watch = StuckWatch::new(None);
            for _ in 0..10 {
                assert!(!watch.observe("get_source_info", &ToolReply::Text("same".into())));
            }
        }

    
        #[test]
        fn summarize_input_truncates() {
            assert_eq!(summarize_input(&serde_json::json!({})), "");
            let long = serde_json::json!({"q": "x".repeat(500)});
            assert!(summarize_input(&long).chars().count() <= 121);
        }

    
        #[test]
        fn transient_errors_are_retried_automatically() {
            // The failure seen when the machine is left alone mid-run.
            assert!(is_transient_error(
                "Anthropic API error: error decoding response body — error reading a body \
                 from connection — timed out"
            ));
            assert!(is_transient_error(
                "Anthropic API error (529 ): {\"type\":\"overloaded_error\"}"
            ));
            assert!(is_transient_error(
                "Anthropic API error (429 Too Many Requests): rate limit"
            ));
            assert!(is_transient_error(
                "Anthropic API error (503 Service Unavailable): upstream"
            ));
            // Client-side mistakes would just fail again — those go straight to the
            // user's Retry prompt instead of burning automatic attempts.
            assert!(!is_transient_error(
                "Anthropic API error (401 Unauthorized): invalid x-api-key"
            ));
            assert!(!is_transient_error(
                "Anthropic API error (400 Bad Request): prompt is too long"
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
mod outputs_tests {
    use super::*;
    use blueprint::OutputTarget;

    
        fn fixture_agent(target: OutputTarget) -> ConversionAgent {
            let pdf =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/input/AAEV_019_EN.pdf");
            let bytes = std::fs::read(&pdf).expect("read AAEV_019_EN.pdf");
            ConversionAgent::new(
                Some("ubs".into()),
                vec![("AAEV_019_EN.pdf".to_string(), bytes)],
                None,
                format!("test-outputs-{}", target.as_str()),
                target,
            )
        }

    
        /// The original defect: the Redacto dump was built from the engine's
        /// conversion of the source while the agent had authored something else, so
        /// what shipped was never what the run produced or the Reviewer approved.
        /// Under the Redacto target the authored tree is the document, full stop.
        #[test]
        fn redacto_outputs_are_built_from_the_authored_tree() {
            use blueprint::structured::{ParagraphNode, StructuredNode, TranslatedText};
    
            let mut agent = fixture_agent(OutputTarget::Redacto);
            // A sentence that appears nowhere in the source PDF, so its presence in
            // the SQL can only come from the authored tree.
            agent.seed_structured(vec![StructuredNode::Paragraph(ParagraphNode {
                content: TranslatedText::plain("AUTHORED-BY-THE-AGENT-MARKER"),
                som_path: None,
                source_name: None,
            })]);
    
            let outputs = agent::outputs::build(&mut agent, Some("ubs"));
    
            let sql = outputs
                .redacto_sql
                .expect("the authored document yields a dump");
            assert!(
                sql.contains("AUTHORED-BY-THE-AGENT-MARKER"),
                "the dump must be generated from the authored tree"
            );
            assert_eq!(
                outputs.envelope.content.len(),
                1,
                "the envelope is the authored tree, not the engine's parse of the source"
            );
            assert!(
                agent.aem_translated().is_none(),
                "a Redacto run produces no AEM tree"
            );
            // The recovered master-page header must survive into the configuration.
            assert!(
                outputs.envelope.context.header.is_some(),
                "the context must come from the merged source envelope"
            );
        }

    
        /// An authored tree that produces no assets must yield no file and say why,
        /// rather than a valid-looking dump describing an empty document.
        #[test]
        fn an_empty_redacto_document_produces_no_sql() {
            let mut agent = fixture_agent(OutputTarget::Redacto);
    
            let outputs = agent::outputs::build(&mut agent, Some("ubs"));
    
            assert!(outputs.redacto_sql.is_none());
            assert!(
                outputs
                    .warnings
                    .iter()
                    .any(|w| w.contains("No Redacto dump")),
                "the reason must be reported: {:?}",
                outputs.warnings
            );
        }

    
        /// An AEM run produces no Redacto dump, even on a profile that configures
        /// one. The result panel does not offer it, so deriving it from the source
        /// was a full extraction and dump generation for a file nobody could reach.
        #[test]
        fn the_aem_target_derives_no_redacto_dump() {
            let mut agent = fixture_agent(OutputTarget::Aem);
    
            let outputs = agent::outputs::build(&mut agent, Some("ubs"));
    
            assert!(
                outputs.redacto_sql.is_none(),
                "the dump belongs to the Redacto target"
            );
            // The envelope is the authored AEM tree lifted back into structured
            // content — empty here because this agent never authored one.
            assert!(outputs.envelope.content.is_empty());
            assert!(outputs.warnings.is_empty(), "{:?}", outputs.warnings);
        }
}

#[cfg(test)]
mod controller {
    //! End-to-end sequencing tests.
    //!
    //! These are the reason the controller left the UI crate: a scripted
    //! [`TurnProvider`] and a recording [`RunObserver`] drive the real `run`
    //! over a real `ConversionAgent`, with no network and no desktop runtime.
    //! None of this was reachable before.

    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Replays a fixed script, one model call per invocation. A script that
    /// runs dry fails the test rather than hanging, so an unexpected extra
    /// stage is loud.
    struct ScriptedTurns {
        script: RefCell<VecDeque<Result<ModelReply, String>>>,
        /// Every `system` prompt the controller asked with, in order — the
        /// record of which stage ran.
        seen: RefCell<Vec<String>>,
    }

    impl ScriptedTurns {
        fn new(script: Vec<Result<ModelReply, String>>) -> Self {
            Self {
                script: RefCell::new(script.into()),
                seen: RefCell::new(Vec::new()),
            }
        }

        /// How many model calls the controller actually made.
        fn turns_taken(&self) -> usize {
            self.seen.borrow().len()
        }
    }

    impl TurnProvider for ScriptedTurns {
        async fn call_model(
            &self,
            _prompt: Message,
            _history: Vec<Message>,
            _tools: &[ToolDefinition],
            system: &str,
            _abort: &AbortFlag,
        ) -> Result<ModelReply, String> {
            self.seen.borrow_mut().push(system.to_string());
            let next = self.script.borrow_mut().pop_front();
            next.expect("the controller took more turns than the script provides")
        }
    }

    #[derive(Default)]
    struct Recorder {
        events: Vec<RunEvent>,
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
    }

    /// Assemble a scripted reply the way a real streamed turn arrives.
    fn scripted(
        text: &str,
        calls: Vec<rig_core::message::ToolCall>,
        prompt_tokens: usize,
    ) -> Result<ModelReply, String> {
        let names: std::collections::BTreeSet<String> =
            calls.iter().map(|c| c.function.name.clone()).collect();
        let mut choice: Vec<AssistantContent> = Vec::new();
        if !text.is_empty() {
            choice.push(AssistantContent::text(text));
        }
        choice.extend(calls.into_iter().map(AssistantContent::ToolCall));

        let finish_reason = if choice
            .iter()
            .any(|c| matches!(c, AssistantContent::ToolCall(_)))
        {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };

        let turn = rig_agent::agent::run::streamed::StreamedTurnAssembler::new(
            names.clone(),
            names,
        )
        .finish(None, &choice);
        let turn = rig_agent::agent::run::streamed::StreamedTurn {
            finish_reason: Some(finish_reason),
            ..turn
        };

        Ok(ModelReply {
            turn,
            usage: rig_core::completion::Usage::new(),
            text: text.to_string(),
            prompt_tokens,
            cost_usd: None,
        })
    }

    fn text_turn(text: &str) -> Result<ModelReply, String> {
        scripted(text, Vec::new(), 42)
    }

    fn review_turn(approved: bool, report: &str) -> Result<ModelReply, String> {
        scripted(
            "",
            vec![rig_core::message::ToolCall {
                id: rig_core::message::ToolCallId::new("call-review").expect("a non-empty id"),
                provider: None,
                function: rig_core::message::ToolFunction {
                    name: "submit_review".into(),
                    arguments: serde_json::json!({"approved": approved, "report": report}),
                },
                signature: None,
                additional_params: None,
            }],
            7,
        )
    }

    /// A Redacto agent with no source: the scripted turns decide what runs, so
    /// the agent only has to be real enough to record a review and finalize.
    fn bare_agent() -> ConversionAgent {
        ConversionAgent::new(None, Vec::new(), None, "test-controller".into(), OutputTarget::Redacto)
    }

    fn config(abort: AbortFlag, max_review_rounds: usize) -> RunConfig {
        RunConfig {
            profile: None,
            target: OutputTarget::Redacto,
            abort,
            max_review_rounds,
            extra_instructions: String::new(),
            template_note: "",
            has_aem_connection: false,
        }
    }

    fn browser_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blueprint-pipeline-browser-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_browser_tools() -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "name": "browser_navigate",
            "description": "fake",
            "input_schema": {"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]},
        })]
    }

    fn names(tools: &[serde_json::Value]) -> Vec<&str> {
        tools.iter().filter_map(|t| t["name"].as_str()).collect()
    }

    fn aem_agent() -> ConversionAgent {
        ConversionAgent::new(None, Vec::new(), None, "t".into(), OutputTarget::Aem)
    }

    fn with_fake_browser(agent: ConversionAgent, dir: std::path::PathBuf) -> ConversionAgent {
        agent.with_browser(agent::browser::BrowserSession::detached(
            fake_browser_tools(),
            dir,
        ))
    }

    /// The browser family is offered to exactly the stages `BROWSER_SCOPES`
    /// names, and only when a session is attached: the Author and Reviewer of
    /// an AEM run see it, the Analyst never does, and a Redacto run has nothing
    /// to click through even with a session attached.
    #[test]
    fn browser_tools_reach_only_the_aem_author_and_reviewer() {
        let roles = roles::roles_for(OutputTarget::Aem);

        let without = aem_agent();
        for role in [roles.analyst, roles.author, roles.reviewer] {
            assert_eq!(
                without.tools_for_stage(role.scope),
                agent::tools_for(OutputTarget::Aem, role.scope),
                "{}",
                role.name
            );
        }

        let with = with_fake_browser(aem_agent(), browser_dir());
        assert!(with.has_browser());
        for role in [roles.author, roles.reviewer] {
            let tools = with.tools_for_stage(role.scope);
            assert!(
                names(&tools).contains(&"browser_navigate"),
                "{}: {:?}",
                role.name,
                names(&tools)
            );
            // The catalog tools come first and are untouched.
            assert_eq!(
                &tools[..tools.len() - 1],
                &agent::tools_for(OutputTarget::Aem, role.scope)[..]
            );
        }
        assert!(!names(&with.tools_for_stage(roles.analyst.scope)).contains(&"browser_navigate"));

        let redacto = with_fake_browser(bare_agent(), browser_dir());
        let redacto_roles = roles::roles_for(OutputTarget::Redacto);
        for role in [
            redacto_roles.analyst,
            redacto_roles.author,
            redacto_roles.reviewer,
        ] {
            assert!(
                !names(&redacto.tools_for_stage(role.scope)).contains(&"browser_navigate"),
                "{}",
                role.name
            );
        }
    }

    /// Whatever way a run ends, its browser is closed and its output directory
    /// removed: no headless Chrome and no downloads outlive the run.
    #[tokio::test]
    async fn every_way_out_of_a_run_closes_the_browser() {
        // Approved.
        let dir = browser_dir();
        let agent = with_fake_browser(aem_agent(), dir.clone());
        let turns = ScriptedTurns::new(vec![
            text_turn("PLAN"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let mut run_config = config(AbortFlag::default(), 1);
        run_config.target = OutputTarget::Aem;
        let outcome = run(agent, run_config, RunSeed::Fresh, &turns, &mut Recorder::default()).await;
        assert!(outcome.is_some());
        assert!(!dir.exists(), "the browser output directory must be removed on approval");

        // Unapproved: the review rounds run out.
        let dir = browser_dir();
        let agent = with_fake_browser(aem_agent(), dir.clone());
        let turns = ScriptedTurns::new(vec![
            text_turn("PLAN"),
            text_turn("BUILT"),
            review_turn(false, "nope"),
            text_turn("FIXED"),
        ]);
        let mut run_config = config(AbortFlag::default(), 1);
        run_config.target = OutputTarget::Aem;
        let outcome = run(agent, run_config, RunSeed::Fresh, &turns, &mut Recorder::default()).await;
        assert!(outcome.is_some());
        assert!(!dir.exists(), "the browser output directory must be removed when unapproved");

        // Aborted before the first turn.
        let dir = browser_dir();
        let agent = with_fake_browser(aem_agent(), dir.clone());
        let abort = AbortFlag::default();
        abort.abort();
        let mut run_config = config(abort, 1);
        run_config.target = OutputTarget::Aem;
        let outcome = run(
            agent,
            run_config,
            RunSeed::Fresh,
            &ScriptedTurns::new(vec![]),
            &mut Recorder::default(),
        )
        .await;
        assert!(outcome.is_none());
        assert!(!dir.exists(), "the browser output directory must be removed on abort");
    }

    #[tokio::test]
    async fn a_fresh_run_sequences_analyst_then_author_then_reviewer() {
        let turns = ScriptedTurns::new(vec![
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let mut obs = Recorder::default();

        let outcome = run(
            bare_agent(),
            config(AbortFlag::default(), 2),
            RunSeed::Fresh,
            &turns,
            &mut obs,
        )
        .await;

        assert!(outcome.is_some(), "an approved run produces a result");
        assert_eq!(obs.stages(), ["Analyst", "Author", "Reviewer"]);
        assert_eq!(turns.turns_taken(), 3);
        // Approval means no "finalizing without a clean review" warning.
        assert!(
            !obs.warnings()
                .iter()
                .any(|w| w.contains("without a clean review")),
            "{:?}",
            obs.warnings()
        );
    }

    /// The Analyst's plan has to reach the Author's prompt, or the second stage
    /// re-derives everything the first one just worked out.
    #[tokio::test]
    async fn the_analysts_plan_is_pinned_into_the_authors_prompt() {
        let turns = ScriptedTurns::new(vec![
            text_turn("SECTION MAP: one heading, two fields"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let mut obs = Recorder::default();

        run(
            bare_agent(),
            config(AbortFlag::default(), 1),
            RunSeed::Fresh,
            &turns,
            &mut obs,
        )
        .await;

        let author_prompt = &turns.seen.borrow()[1];
        assert!(
            author_prompt.contains("SECTION MAP: one heading, two fields"),
            "the Author never saw the plan"
        );
    }

    /// A rejected review sends the run back to the Author with the report
    /// pinned, then finalizes with a warning because it never got a clean pass.
    #[tokio::test]
    async fn a_rejected_review_drives_one_more_author_round() {
        let turns = ScriptedTurns::new(vec![
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(false, "The footer is missing."),
            text_turn("FIXED"),
        ]);
        let mut obs = Recorder::default();

        let outcome = run(
            bare_agent(),
            config(AbortFlag::default(), 1),
            RunSeed::Fresh,
            &turns,
            &mut obs,
        )
        .await;

        assert!(outcome.is_some());
        assert_eq!(obs.stages(), ["Analyst", "Author", "Reviewer", "Author"]);
        let fix_prompt = &turns.seen.borrow()[3];
        assert!(
            fix_prompt.contains("The footer is missing."),
            "the review report was not pinned into the fix round"
        );
        assert!(
            obs.warnings()
                .iter()
                .any(|w| w.contains("without a clean review")),
            "an unapproved run must say so: {:?}",
            obs.warnings()
        );
    }

    /// Feedback replaces the Analyst: the request becomes the first pinned
    /// review and the Author starts from it.
    #[tokio::test]
    async fn a_feedback_run_skips_the_analyst() {
        let turns = ScriptedTurns::new(vec![text_turn("FIXED"), review_turn(true, "")]);
        let mut obs = Recorder::default();

        run(
            bare_agent(),
            config(AbortFlag::default(), 1),
            RunSeed::Feedback("Make the title bigger.".into()),
            &turns,
            &mut obs,
        )
        .await;

        assert_eq!(obs.stages(), ["Author", "Reviewer"]);
        assert!(
            turns.seen.borrow()[0].contains("Make the title bigger."),
            "the feedback never reached the Author"
        );
    }

    /// Aborting before the first turn stops the run without a result, and says
    /// so exactly once per checkpoint rather than silently finishing.
    #[tokio::test]
    async fn an_aborted_run_produces_no_outcome() {
        let abort = AbortFlag::default();
        abort.abort();
        let turns = ScriptedTurns::new(Vec::new());
        let mut obs = Recorder::default();

        let outcome = run(
            bare_agent(),
            config(abort, 1),
            RunSeed::Fresh,
            &turns,
            &mut obs,
        )
        .await;

        assert!(outcome.is_none(), "an aborted run has nothing to publish");
        assert!(obs.aborted());
        assert_eq!(turns.turns_taken(), 0, "no turn should be attempted");
    }

    /// A permanent failure pauses the run and asks. Answering Cancel ends it
    /// with no result — and, critically, without retrying.
    #[tokio::test]
    async fn giving_up_at_the_retry_prompt_ends_the_run() {
        let turns = ScriptedTurns::new(vec![Err("Anthropic API error (400 Bad Request)".into())]);
        let mut obs = Recorder {
            answer: Some(RetryAction::Cancel),
            ..Recorder::default()
        };

        let outcome = run(
            bare_agent(),
            config(AbortFlag::default(), 1),
            RunSeed::Fresh,
            &turns,
            &mut obs,
        )
        .await;

        assert!(outcome.is_none());
        assert_eq!(obs.prompts, 1, "the operator should be asked exactly once");
        assert_eq!(turns.turns_taken(), 1, "a 400 must not be retried");
    }

    /// Answering Retry re-sends the same turn rather than restarting the stage.
    #[tokio::test]
    async fn retrying_re_sends_the_failed_turn() {
        let turns = ScriptedTurns::new(vec![
            Err("Anthropic API error (400 Bad Request)".into()),
            text_turn("THE PLAN"),
            text_turn("BUILT"),
            review_turn(true, ""),
        ]);
        let mut obs = Recorder {
            answer: Some(RetryAction::Retry),
            ..Recorder::default()
        };

        let outcome = run(
            bare_agent(),
            config(AbortFlag::default(), 1),
            RunSeed::Fresh,
            &turns,
            &mut obs,
        )
        .await;

        assert!(outcome.is_some(), "the retry should carry the run to completion");
        assert_eq!(obs.prompts, 1);
        assert_eq!(turns.turns_taken(), 4);
        // The retried turn is the Analyst's, not a fresh stage.
        assert_eq!(obs.stages(), ["Analyst", "Author", "Reviewer"]);
    }
}
