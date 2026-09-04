//! One streamed model call: evict to fit, send, drain the stream, assemble.
//!
//! This is the whole transport now. Everything that used to live here — SSE
//! framing, the two dialects' event shapes, the Anthropic↔OpenAI translation —
//! is rig's job; what is left is the parts that are this app's own: the context
//! budget, the abort flag checked per chunk, and the calibration feedback.

use std::collections::BTreeSet;

use futures_util::StreamExt;
use pipeline::{AbortFlag, ModelReply, ResolveInvalidCall};
use rig_agent::agent::model::ModelHandle;
use rig_agent::agent::run::streamed::{StreamedResolution, StreamedTurnAssembler, StreamedTurnEvent};
use rig_core::completion::{CompletionModel, CompletionRequest, ToolDefinition};
use rig_core::message::Message;
use rig_core::streaming::StreamedAssistantContent;

use crate::{context, models};

/// The error a call returns when the run was aborted mid-stream. Callers check
/// the abort flag rather than this string; it exists so the failure has a
/// readable cause if one ever escapes.
pub const ABORTED: &str = "Run aborted.";

/// The floor the context-overflow retry shrinks the prompt target to.
const MIN_TARGET: usize = 16_000;

/// Render a provider error for the controller's retry classifier.
///
/// Two things have to survive into the string, because it is all the controller
/// sees: the status, so a `503` is retried and a `400` is not, and the
/// `retry-after` the provider asked for, so a rate limit waits the window it
/// named instead of a guess.
///
/// Both are appended as fixed tokens rather than left to the error's own
/// wording. Matching prose across a crate boundary is how gateway `503`s
/// quietly stopped being retried once already; `status_and_retry_after` and
/// `runner`'s `transient_statuses_match_what_the_transport_writes` pin the
/// tokens from both sides.
fn describe_error(error: &rig_core::completion::CompletionError) -> String {
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

/// Everything one model call needs beyond the model itself.
pub struct CallPlan<'a> {
    pub prompt: Message,
    pub history: Vec<Message>,
    pub tools: &'a [ToolDefinition],
    pub system: &'a str,
    pub max_tokens: u32,
    /// The model id: the handle has erased it, and it is what the token budget
    /// and the price are both looked up by.
    pub model_id: &'a str,
}

/// Run one streamed model call and assemble the turn.
pub async fn call_model(
    model: &ModelHandle,
    plan: CallPlan<'_>,
    abort: &AbortFlag,
    resolve_invalid: ResolveInvalidCall<'_>,
) -> Result<ModelReply, String> {
    let CallPlan {
        prompt,
        mut history,
        tools,
        system,
        max_tokens,
        model_id,
    } = plan;

    let system = (!system.is_empty()).then_some(system);
    let owned_system = system.map(str::to_string);

    // `CallModel` splits the transcript with `split_last`, so `prompt` is the
    // newest message — on every tool round, the batch of tool results, which is
    // the largest and freshest payload there is. Put it back before the budget
    // is applied: excluded, it would escape both the estimate and the ladder,
    // and a single oversized result could blow the window with nothing able to
    // shrink it.
    history.push(prompt);

    // Recomputed per call rather than snapshotted, so a window learned from a
    // 400 sizes every later request instead of only the one that overflowed.
    let mut target = models::prompt_token_target(model_id, max_tokens);

    // The estimate is char-based, so a prompt that fits it can still trip the
    // provider's hard limit. Rather than fail the run, halve the budget, force a
    // harder eviction and try again — and remember the real window the error
    // reported, so the next turn starts from the truth instead of the guess.
    let (mut response, sent_estimate) = loop {
        // Shrinking happens off the caller's thread: the run loop is spawned on
        // Dioxus's main-thread executor, and a deep clone plus a recursive token
        // walk over a multi-MB history freezes rendering. Ownership goes in and
        // comes back, so a retry never loses the transcript.
        let owned_tools = tools.to_vec();
        let system_for_task = owned_system.clone();
        let (shrunk, sent_estimate) = tokio::task::spawn_blocking(move || {
            let mut history = history;
            context::evict_to_fit(
                &mut history,
                &owned_tools,
                system_for_task.as_deref(),
                target,
            );
            let estimate = context::assembled_prompt_estimate(
                &history,
                &owned_tools,
                system_for_task.as_deref(),
            );
            (history, estimate)
        })
        .await
        .map_err(|e| format!("Preparing the request failed: {e}"))?;
        history = shrunk;

        let request = CompletionRequest {
            model: None,
            preamble: owned_system.clone(),
            chat_history: history.clone(),
            documents: Vec::new(),
            tools: tools.to_vec(),
            temperature: None,
            max_tokens: Some(u64::from(max_tokens)),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        };

        match model.stream(request).await {
            Ok(response) => break (response, sent_estimate),
            Err(e) => {
                let msg = e.to_string();
                if !models::is_context_overflow(&msg) || target <= MIN_TARGET {
                    return Err(describe_error(&e));
                }
                models::learn_context_window_from_error(&msg);
                // Drop straight to what the error said the window really is,
                // rather than halving toward it over several more rejected
                // round-trips.
                let learned = models::prompt_token_target(model_id, max_tokens);
                target = learned.min(target / 2).max(MIN_TARGET);
            }
        }
    };

    let tool_names: BTreeSet<String> = tools.iter().map(|t| t.name.clone()).collect();
    let mut assembler = StreamedTurnAssembler::new(tool_names.clone(), tool_names);

    let mut text = String::new();
    let mut abandoned = false;
    while let Some(item) = response.next().await {
        // Checked per chunk, not per turn: a stage's turn can run for minutes,
        // and a stop control that only takes effect at the end of one is not a
        // stop control.
        if abort.is_aborted() {
            return Err(ABORTED.to_string());
        }
        let item = item.map_err(|e| describe_error(&e))?;

        // Once the turn is abandoned the run holds the corrective messages and
        // the assembler is done with it. Keep pulling so the provider's usage
        // still arrives, but feed it nothing more.
        if abandoned {
            continue;
        }

        if let StreamedAssistantContent::Text(chunk) = &item {
            text.push_str(&chunk.text);
        }
        let events = assembler.ingest(&item).map_err(|e| describe_error(&e))?;

        // A tool call the model invented, or reached for outside this stage's
        // scope, parks inside the assembler; the very next `ingest` then fails
        // the stage. It has to be resolved here, in the stream, or an ordinary
        // model mistake ends a run that used to shrug it off.
        for event in events {
            let StreamedTurnEvent::InvalidToolCall(invalid) = event else {
                continue;
            };
            let partial = assembler.partial_turn(None);
            let resolution = resolve_invalid(&partial, &invalid)?;
            assembler.resolve_pending_invalid(&resolution);
            if matches!(resolution, StreamedResolution::TurnAbandoned { .. }) {
                abandoned = true;
                break;
            }
        }
    }

    let usage = response.usage();
    let turn = assembler.finish(None, &response.choice);

    // What the API billed for the prompt is the only ground truth for how full
    // the window was; caching lowers the cost but not the occupancy, so the
    // cached buckets count too.
    let prompt_tokens = (usage.input_tokens
        + usage.cached_input_tokens
        + usage.cache_creation_input_tokens) as usize;
    context::record_token_calibration(prompt_tokens, sent_estimate);

    let cost_usd = crate::pricing::cost_usd(model_id, &usage);

    Ok(ModelReply {
        turn,
        usage,
        text,
        prompt_tokens,
        cost_usd,
        abandoned,
    })
}
