//! One streamed model call: evict to fit, send, drain the stream, assemble.
//!
//! This is the whole transport now. Everything that used to live here — SSE
//! framing, the two dialects' event shapes, the Anthropic↔OpenAI translation —
//! is rig's job; what is left is the parts that are this app's own: the context
//! budget, the abort flag checked per chunk, and the calibration feedback.

use std::collections::BTreeSet;

use futures_util::StreamExt;
use pipeline::{AbortFlag, ModelReply};
use rig_agent::agent::model::ModelHandle;
use rig_agent::agent::run::streamed::StreamedTurnAssembler;
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

/// Everything one model call needs beyond the model itself.
pub struct CallPlan<'a> {
    pub prompt: Message,
    pub history: Vec<Message>,
    pub tools: &'a [ToolDefinition],
    pub system: &'a str,
    pub max_tokens: u32,
    /// Estimated-token budget the assembled prompt must fit inside.
    pub target: usize,
    /// The model id, for pricing. The handle has erased it.
    pub model_id: &'a str,
}

/// Run one streamed model call and assemble the turn.
pub async fn call_model(
    model: &ModelHandle,
    plan: CallPlan<'_>,
    abort: &AbortFlag,
) -> Result<ModelReply, String> {
    let CallPlan {
        prompt,
        mut history,
        tools,
        system,
        max_tokens,
        target,
        model_id,
    } = plan;

    let system = (!system.is_empty()).then_some(system);
    let owned_system = system.map(str::to_string);

    // The estimate is char-based, so a prompt that fits it can still trip the
    // provider's hard limit. Rather than fail the run, halve the budget, force a
    // harder eviction and try again — and remember the real window the error
    // reported, so the next turn starts from the truth instead of the guess.
    let mut target = target;
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

        let mut chat_history = history.clone();
        chat_history.push(prompt.clone());

        let request = CompletionRequest {
            model: None,
            preamble: owned_system.clone(),
            chat_history,
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
                    return Err(format!("LLM API error: {msg}"));
                }
                models::learn_context_window_from_error(&msg);
                target = (target / 2).max(MIN_TARGET);
            }
        }
    };

    let tool_names: BTreeSet<String> = tools.iter().map(|t| t.name.clone()).collect();
    let mut assembler = StreamedTurnAssembler::new(tool_names.clone(), tool_names);

    let mut text = String::new();
    while let Some(item) = response.next().await {
        // Checked per chunk, not per turn: a stage's turn can run for minutes,
        // and a stop control that only takes effect at the end of one is not a
        // stop control.
        if abort.is_aborted() {
            return Err(ABORTED.to_string());
        }
        let item = item.map_err(|e| format!("LLM API error: {e}"))?;
        if let StreamedAssistantContent::Text(chunk) = &item {
            text.push_str(&chunk.text);
        }
        assembler
            .ingest(&item)
            .map_err(|e| format!("LLM API error: {e}"))?;
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
    })
}
