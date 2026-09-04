//! Keeping a turn inside the model's context window: what a prompt is estimated
//! to cost, and what gets shrunk when it does not fit.
//!
//! Ported from the hand-rolled Anthropic client onto rig's typed [`Message`], so
//! the passes match on `UserContent::ToolResult` / `AssistantContent::ToolCall`
//! instead of indexing string keys into `serde_json::Value`. The behaviour is
//! unchanged and pinned by the tests at the bottom of this file: the ladder is a
//! no-op under budget, never breaks `tool_use`↔`tool_result` pairing, and is
//! idempotent so a second pass cannot invalidate the cached prefix.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use rig_core::completion::ToolDefinition;
use rig_core::message::{
    AssistantContent, DocumentSourceKind, Message, ToolResultContent, UserContent,
};

use crate::models::prompt_token_target;

/// Default trailing messages kept verbatim by [`evict_stale_history_with`]. Even,
/// so whole assistant+`tool_result` turn-pairs survive (the latest data stays
/// intact). Overridable at runtime via [`configure_eviction`].
pub const DEFAULT_KEEP_RECENT_MESSAGES: usize = 4;
/// Default: tool-result text longer than this (chars) is elided once stale.
pub const DEFAULT_ELIDE_TEXT_OVER_CHARS: usize = 2000;
/// Default: `tool_use` input longer than this (chars) is elided once stale.
pub const DEFAULT_ELIDE_INPUT_OVER_CHARS: usize = 2000;
/// Sentinel prefix marking an already-elided block. Makes eviction idempotent:
/// repeated passes are byte-identical, so the cached prefix is not invalidated.
pub(crate) const ELIDED_MARKER: &str = "\u{1}elided";

// Live, runtime-configurable eviction tuning (synced from `AppSettings`).
pub(crate) static CFG_KEEP_RECENT: AtomicUsize = AtomicUsize::new(DEFAULT_KEEP_RECENT_MESSAGES);
pub(crate) static CFG_TEXT_OVER: AtomicUsize = AtomicUsize::new(DEFAULT_ELIDE_TEXT_OVER_CHARS);
pub(crate) static CFG_INPUT_OVER: AtomicUsize = AtomicUsize::new(DEFAULT_ELIDE_INPUT_OVER_CHARS);

/// Override the history-eviction tuning (called from settings on startup and on
/// change). A `0` argument resets that parameter to its default. `keep_recent`
/// is clamped to an even number ≥ 2 so whole turn-pairs always stay verbatim.
pub fn configure_eviction(keep_recent: usize, text_over: usize, input_over: usize) {
    let keep = if keep_recent == 0 {
        DEFAULT_KEEP_RECENT_MESSAGES
    } else {
        (keep_recent + (keep_recent & 1)).max(2) // round up to even, min 2
    };
    CFG_KEEP_RECENT.store(keep, Ordering::Relaxed);
    CFG_TEXT_OVER.store(
        if text_over == 0 {
            DEFAULT_ELIDE_TEXT_OVER_CHARS
        } else {
            text_over
        },
        Ordering::Relaxed,
    );
    CFG_INPUT_OVER.store(
        if input_over == 0 {
            DEFAULT_ELIDE_INPUT_OVER_CHARS
        } else {
            input_over
        },
        Ordering::Relaxed,
    );
}

// ── Token estimation ─────────────────────────────────────────────────────────

/// Fallback per-image token cost when the base64 can't be decoded to read its
/// dimensions (near the observed max, so a fallback errs high/safe).
const IMAGE_TOKEN_FALLBACK: usize = 1_600;
/// Anthropic's vision cost is `(width * height) / PX_PER_TOKEN`, after the image
/// is downscaled to at most `MAX_IMAGE_PX` pixels — so the cost is bounded at
/// `MAX_IMAGE_PX / PX_PER_TOKEN` (~1533).
const PX_PER_TOKEN: usize = 750;
const MAX_IMAGE_PX: usize = 1_150_000;

/// Real vision-token cost of one base64 image: decode just enough to read its
/// dimensions, then apply Anthropic's `min(w*h, MAX_IMAGE_PX) / PX_PER_TOKEN`.
/// Falls back to [`IMAGE_TOKEN_FALLBACK`] if the payload can't be read.
fn image_token_cost(data_b64: &str) -> usize {
    use base64::Engine;
    let Ok(bytes) = base64::prelude::BASE64_STANDARD.decode(data_b64) else {
        return IMAGE_TOKEN_FALLBACK;
    };
    match image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
    {
        Some((w, h)) => (w as usize * h as usize).min(MAX_IMAGE_PX) / PX_PER_TOKEN,
        None => IMAGE_TOKEN_FALLBACK,
    }
}

/// The base64 payload of an image block, if it carries one inline. A URL or a
/// provider-side file id costs nothing locally, so neither is counted.
fn image_base64(data: &DocumentSourceKind) -> Option<&str> {
    match data {
        DocumentSourceKind::Base64(b64) => Some(b64.as_str()),
        _ => None,
    }
}

/// Base64 payload length across every image in `msg` (to subtract from the
/// byte-based estimate) and their combined real vision-token cost (to add back).
/// Counting images by base64 length would dwarf everything and skew both the
/// budget and the calibration factor.
fn image_payload_stats(msg: &Message) -> (usize, usize) {
    let mut bytes = 0;
    let mut tokens = 0;
    let mut count = |data: &DocumentSourceKind| {
        if let Some(b64) = image_base64(data) {
            bytes += b64.len();
            tokens += image_token_cost(b64);
        }
    };

    match msg {
        Message::System { .. } => {}
        Message::User { content } => {
            for block in content {
                match block {
                    UserContent::Image(image) => count(&image.data),
                    UserContent::ToolResult(result) => {
                        for inner in &result.content {
                            if let ToolResultContent::Image(image) = inner {
                                count(&image.data);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Message::Assistant { content, .. } => {
            for block in content {
                if let AssistantContent::Image(image) = block {
                    count(&image.data);
                }
            }
        }
    }
    (bytes, tokens)
}

/// Token estimate for one message: serialized byte length ÷ 4 (the byte count
/// already includes every brace/quote/colon, so this tracks real tokens well for
/// text and JSON alike), except that images are counted at their real vision
/// cost rather than their base64 length. The figure is scaled by the calibration
/// factor before being compared to a budget.
fn estimate_message_tokens(msg: &Message) -> usize {
    let total = serde_json::to_string(msg).map(|s| s.len()).unwrap_or(0);
    let (image_bytes, image_tokens) = image_payload_stats(msg);
    total.saturating_sub(image_bytes) / 4 + image_tokens
}

/// Token estimate for one tool definition. No images are reachable here, so this
/// is the plain byte-based figure.
fn estimate_tool_tokens(tool: &ToolDefinition) -> usize {
    serde_json::to_string(tool).map(|s| s.len()).unwrap_or(0) / 4
}

/// Calibration factor (real prompt tokens ÷ raw char-based estimate), in
/// thousandths. Learned from the API's reported `usage`; starts at 1.0 and is
/// nudged toward each turn's observed ratio (see [`record_token_calibration`]).
static CFG_TOKEN_CALIBRATION_MILLI: AtomicU64 = AtomicU64::new(1000);

/// The current calibration factor as a float (defaults to 1.0).
fn token_calibration() -> f64 {
    CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed) as f64 / 1000.0
}

/// A raw estimate scaled by the learned calibration factor — the actual token
/// count we predict the API will bill for `raw` estimated tokens.
fn calibrated_tokens(raw: usize) -> usize {
    (raw as f64 * token_calibration()) as usize
}

/// Fold one observed (real prompt tokens, raw estimate) pair into the calibration
/// factor with an EMA. Clamped to `[1.0, 8.0]`: the factor may only ever make us
/// evict *more*, never less — under-counting is the direction that overflows the
/// context window, so calibration is not allowed to shrink the estimate below its
/// raw value. `real` comes from the API's `usage` (input + both cache buckets);
/// `estimate` is the raw estimate of the same assembled prompt.
pub fn record_token_calibration(real: usize, estimate: usize) {
    if real == 0 || estimate == 0 {
        return;
    }
    let observed = (real as f64 / estimate as f64).clamp(1.0, 8.0);
    let blended = (token_calibration() * 0.7 + observed * 0.3).clamp(1.0, 8.0);
    CFG_TOKEN_CALIBRATION_MILLI.store((blended * 1000.0) as u64, Ordering::Relaxed);
}

/// Raw char-based token estimate for the whole assembled prompt: messages +
/// `tools` + `system`.
pub fn assembled_prompt_estimate(
    history: &[Message],
    tools: &[ToolDefinition],
    system: Option<&str>,
) -> usize {
    tools.iter().map(estimate_tool_tokens).sum::<usize>()
        + system.map_or(0, |s| s.len() / 4)
        + history.iter().map(estimate_message_tokens).sum::<usize>()
}

/// The estimated-token budget for the assembled prompt, for `model`'s window.
pub fn target_for(model: &str, max_tokens: u32) -> usize {
    prompt_token_target(model, max_tokens)
}

// ── The eviction ladder ──────────────────────────────────────────────────────

/// Build a `tool_call_id -> tool name` map over the whole transcript. Shared by
/// the eviction passes that need to label result stubs by their originating
/// tool.
fn tool_name_by_id(history: &[Message]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for msg in history {
        if let Message::Assistant { content, .. } = msg {
            for block in content {
                if let AssistantContent::ToolCall(call) = block {
                    names.insert(
                        call.id.as_str().to_string(),
                        call.function.name.clone(),
                    );
                }
            }
        }
    }
    names
}

/// Shrink heavy, stale content in `history` **in place** to bound context growth
/// on long tool loops. Older base64 images, oversized tool-result text, and
/// oversized `set_*` tool inputs are replaced with short stubs; the model can
/// re-fetch the real data via tools (the engine + SQLite are the source of
/// truth, not the transcript). Blocks are never removed, so the API's
/// `tool_use`↔`tool_result` pairing stays intact.
///
/// Protects `history[0]` (the instruction prefix) and the last `keep_recent`
/// messages. Idempotent (already-stubbed blocks are skipped), so it cooperates
/// with prompt caching instead of busting the cached prefix. [`evict_to_fit`]
/// drives it with escalating tuning — a tiny recent window and near-zero
/// thresholds — as a turn approaches the context window.
fn evict_stale_history_with(
    history: &mut [Message],
    keep_recent: usize,
    text_over: usize,
    input_over: usize,
) {
    let len = history.len();
    if len <= 1 + keep_recent {
        return;
    }
    let cutoff = len - keep_recent; // index >= cutoff is protected

    // Pass 1 (read-only): map tool call id -> tool name, to label result stubs.
    let names = tool_name_by_id(history);

    // Pass 2 (mutate): elide older messages, skipping index 0 + the recent tail.
    for msg in history.iter_mut().take(cutoff).skip(1) {
        match msg {
            Message::System { .. } => {}
            Message::Assistant { content, .. } => {
                for block in content.iter_mut() {
                    if let AssistantContent::ToolCall(call) = block {
                        elide_tool_call(call, input_over);
                    }
                }
            }
            Message::User { content } => {
                for block in content.iter_mut() {
                    if let UserContent::ToolResult(result) = block {
                        elide_tool_result(result, &names, text_over);
                    }
                }
            }
        }
    }
}

/// Replace an oversized tool call's `arguments` with a small stub object.
/// `arguments` must stay a JSON object; history replay does not re-validate it
/// against the tool schema, so the stub is safe.
fn elide_tool_call(call: &mut rig_core::message::ToolCall, input_over: usize) {
    if call.function.arguments.get("_elided").is_some() {
        return; // already stubbed
    }
    let size = serde_json::to_string(&call.function.arguments)
        .map(|s| s.len())
        .unwrap_or(0);
    if size <= input_over {
        return;
    }
    let name = &call.function.name;
    call.function.arguments = serde_json::json!({
        "_elided": ELIDED_MARKER,
        "note": format!("{name} input elided: {size} chars — re-read current state with a get_* tool"),
    });
}

/// Elide the inner blocks of a stale tool result: images become a text stub,
/// oversized text is truncated to a stub. Keeps at least one block, and never
/// touches the call id, so the pairing survives.
fn elide_tool_result(
    result: &mut rig_core::message::ToolResult,
    names: &HashMap<String, String>,
    text_over: usize,
) {
    let tool = names
        .get(result.call.as_str())
        .map(|s| s.as_str())
        .unwrap_or("tool")
        .to_string();

    for block in result.content.iter_mut() {
        match block {
            ToolResultContent::Image(_) => {
                *block = ToolResultContent::text(format!(
                    "{ELIDED_MARKER} image elided — re-fetch with the tool if needed"
                ));
            }
            ToolResultContent::Text(text) => {
                if text.text.starts_with(ELIDED_MARKER) || text.text.len() <= text_over {
                    continue;
                }
                let n = text.text.len();
                text.text = format!(
                    "{ELIDED_MARKER} {tool} output elided: {n} chars — re-read for current state"
                );
            }
            ToolResultContent::Json { value } => {
                let n = serde_json::to_string(value).map(|s| s.len()).unwrap_or(0);
                if n <= text_over {
                    continue;
                }
                *block = ToolResultContent::text(format!(
                    "{ELIDED_MARKER} {tool} output elided: {n} chars — re-read for current state"
                ));
            }
        }
    }
}

/// Shrink `history` in place until the assembled prompt (messages + `tools` +
/// `system`) is estimated to fit under `target` input tokens. Does **nothing**
/// while the prompt is under budget — so with a 1M-token window, context is kept
/// intact right up to the limit; nothing is stubbed prematurely. Only once over
/// budget does it escalate through four stages, reusing the stubbing pass:
///   1. Normal-tuned stubbing (large stale blocks; keeps the recent window).
///   2. Aggressive stubbing — tiny thresholds and a minimal recent window — so
///      even recent / smaller heavy blocks are stubbed.
///   3. Sliding window — drop the oldest `(assistant, user)` turn-pairs, the only
///      lever that removes rather than shrinks, keeping `history[0]` and the most
///      recent pair, until it fits or nothing more is safe to drop.
///   4. Last resort — stub the most-recent pair too (no protected window), so a
///      single oversized tool result / tool input can't blow the limit alone.
pub fn evict_to_fit(
    history: &mut Vec<Message>,
    tools: &[ToolDefinition],
    system: Option<&str>,
    target: usize,
) {
    let fits = |h: &[Message]| {
        calibrated_tokens(assembled_prompt_estimate(h, tools, system)) <= target
    };

    // Under budget → keep the full transcript verbatim. This is the common case
    // and MUST not touch anything: stubbing while there is ample token headroom
    // makes the agent re-fetch context it still had room for. Every stage below
    // runs unconditionally, because we only reach them once genuinely over
    // budget.
    if fits(history) {
        return;
    }

    // Stage 1: normal-tuned stubbing (stale big blocks; keep the recent window).
    evict_stale_history_with(
        history,
        CFG_KEEP_RECENT.load(Ordering::Relaxed),
        CFG_TEXT_OVER.load(Ordering::Relaxed),
        CFG_INPUT_OVER.load(Ordering::Relaxed),
    );
    if fits(history) {
        return;
    }

    // Stage 2: aggressive stubbing (keep only the last pair verbatim; stub any
    // text/input over ~200 chars).
    evict_stale_history_with(history, 2, 200, 200);
    if fits(history) {
        return;
    }

    // Stage 3: drop oldest turn-pairs. Keep `history[0]` (the kickoff/user
    // prefix) and at least the most recent pair; stop if the head isn't a clean
    // (assistant, user) pair, rather than risk breaking tool_use↔tool_result
    // pairing.
    while !fits(history) && history.len() > 3 {
        let head_is_pair = matches!(history.get(1), Some(Message::Assistant { .. }))
            && matches!(history.get(2), Some(Message::User { .. }));
        if !head_is_pair {
            break;
        }
        history.drain(1..3);
    }
    if fits(history) {
        return;
    }

    // Stage 4: nothing left to drop but still over budget — a single recent block
    // (e.g. a whole-XFA `get_xfa`, or a monolithic `set_*` input) exceeds it on
    // its own. Stub with no protected window (`keep_recent = 0`) so even the last
    // pair is shrunk; the model re-fetches from the engine if it still needs it.
    evict_stale_history_with(history, 0, 200, 200);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{Text, ToolCall, ToolCallId, ToolFunction, ToolResult};
    use serde_json::json;

    fn user_text(text: &str) -> Message {
        Message::User {
            content: vec![UserContent::Text(Text::from(text))],
        }
    }

    fn assistant_tool_use(id: &str, name: &str, input: &serde_json::Value) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: ToolCallId::new(id).expect("a non-empty id"),
                provider: None,
                function: ToolFunction {
                    name: name.to_string(),
                    arguments: input.clone(),
                },
                signature: None,
                additional_params: None,
            })],
        }
    }

    fn tool_result(id: &str, content: Vec<ToolResultContent>) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: ToolCallId::new(id).expect("a non-empty id"),
                provider: None,
                name: String::new(),
                content,
            })],
        }
    }

    fn result_text(id: &str, text: &str) -> Message {
        tool_result(id, vec![ToolResultContent::text(text)])
    }

    fn result_image(id: &str, data: &str) -> Message {
        tool_result(
            id,
            vec![ToolResultContent::image_base64(
                data,
                Some(rig_core::message::ImageMediaType::JPEG),
                None,
            )],
        )
    }

    /// The text of the first block of the first tool result in `msg`.
    fn block_text(msg: &Message) -> String {
        let Message::User { content } = msg else {
            return String::new();
        };
        match content.first() {
            Some(UserContent::ToolResult(r)) => match r.content.first() {
                Some(ToolResultContent::Text(t)) => t.text.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// The arguments of the first tool call in `msg`.
    fn call_args(msg: &Message) -> serde_json::Value {
        let Message::Assistant { content, .. } = msg else {
            return serde_json::Value::Null;
        };
        match content.first() {
            Some(AssistantContent::ToolCall(c)) => c.function.arguments.clone(),
            _ => serde_json::Value::Null,
        }
    }

    /// History with stale heavy content (big image, big text, big tool input) in
    /// old turns and a small recent turn-pair.
    fn big_history() -> Vec<Message> {
        let big_input = json!({"tree": "X".repeat(3000)});
        vec![
            user_text("SYSTEM PROMPT"),                              // 0 protected
            assistant_tool_use("tu1", "set_structured", &big_input), // 1 evict
            result_image("tu1", &"A".repeat(250_000)),               // 2 evict (drives size)
            assistant_tool_use("tu2", "get_xfa", &json!({})),        // 3 evict
            result_text("tu2", &"x".repeat(5000)),                   // 4 evict
            assistant_tool_use("tu3", "get_structured", &json!({})), // 5 recent
            result_text("tu3", "small recent result"),               // 6 recent
            assistant_tool_use("tu4", "finish", &json!({})),         // 7 recent
            result_text("tu4", "done"),                              // 8 recent
        ]
    }

    /// The stubbing pass with the shipped defaults — exercises the exact tuning
    /// the live config resets to. (Production drives [`evict_stale_history_with`]
    /// via [`evict_to_fit`].)
    fn evict_stale_history(history: &mut [Message]) {
        evict_stale_history_with(
            history,
            DEFAULT_KEEP_RECENT_MESSAGES,
            DEFAULT_ELIDE_TEXT_OVER_CHARS,
            DEFAULT_ELIDE_INPUT_OVER_CHARS,
        );
    }

    #[test]
    fn evicts_stale_protects_recent() {
        let original = big_history();
        let mut h = original.clone();
        evict_stale_history(&mut h);

        // Index 0 and the last DEFAULT_KEEP_RECENT_MESSAGES are byte-identical.
        assert_eq!(h[0], original[0]);
        let len = h.len();
        for i in (len - DEFAULT_KEEP_RECENT_MESSAGES)..len {
            assert_eq!(h[i], original[i], "recent message {i} changed");
        }

        // Old set_structured input is stubbed to an object with `_elided`.
        assert!(call_args(&h[1]).get("_elided").is_some());
        assert!(call_args(&h[1]).is_object());

        // Old image became a text stub; old big text shrank to a marker stub.
        assert!(block_text(&h[2]).starts_with(ELIDED_MARKER));
        assert!(block_text(&h[2]).contains("image elided"));
        assert!(block_text(&h[4]).starts_with(ELIDED_MARKER));
        // The stub names the originating tool (get_xfa).
        assert!(block_text(&h[4]).contains("get_xfa"));
    }

    #[test]
    fn pairing_preserved() {
        let mut h = big_history();
        evict_stale_history(&mut h);
        let calls = h
            .iter()
            .filter_map(|m| match m {
                Message::Assistant { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter(|b| matches!(b, AssistantContent::ToolCall(_)))
            .count();
        let results = h
            .iter()
            .filter_map(|m| match m {
                Message::User { content } => Some(content),
                _ => None,
            })
            .flatten()
            .filter(|b| matches!(b, UserContent::ToolResult(_)))
            .count();
        // No blocks deleted: every tool call still has its result.
        assert_eq!(calls, 4);
        assert_eq!(results, 4);
    }

    #[test]
    fn idempotent() {
        let mut once = big_history();
        evict_stale_history(&mut once);
        let mut twice = once.clone();
        evict_stale_history(&mut twice);
        assert_eq!(once, twice, "second pass must be a no-op");
    }

    #[test]
    fn images_survive_while_the_prompt_still_fits() {
        // Images go through the same pass as text, so they are equally safe
        // while there is budget left. This is the regression guard against an
        // unconditional image pass stubbing a render the agent had just fetched
        // and was in the middle of comparing.
        let original = vec![
            user_text("SYSTEM"),
            assistant_tool_use("tu1", "get_plain_state_image", &json!({})),
            result_image("tu1", "AAAA"),
            assistant_tool_use("tu2", "get_plain_state_image", &json!({})),
            result_image("tu2", "BBBB"),
            assistant_tool_use("tu3", "get_plain_state_image", &json!({})),
            result_image("tu3", "CCCC"),
        ];
        let mut h = original.clone();
        evict_to_fit(&mut h, &[], None, 800_000);
        assert_eq!(h, original);
    }

    #[test]
    fn several_verbose_results_survive_together() {
        // Two get_xfa reads within budget must BOTH stay intact — the agent
        // legitimately holds several verbose results at once (regression guard:
        // an over-aggressive verbose-eviction once stubbed all but the latest,
        // forcing an endless re-fetch loop when comparing languages).
        let original = vec![
            user_text("SYSTEM"),
            assistant_tool_use("tu1", "get_xfa", &json!({})),
            result_text("tu1", &"x".repeat(400)),
            assistant_tool_use("tu2", "get_xfa", &json!({})),
            result_text("tu2", &"y".repeat(400)),
        ];
        let mut h = original.clone();
        evict_to_fit(&mut h, &[], None, 800_000);
        assert_eq!(h, original);
    }

    #[test]
    fn calibration_ema_moves_toward_observed_and_clamps() {
        // Save + restore the shared factor so this test can't perturb others.
        let saved = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        CFG_TOKEN_CALIBRATION_MILLI.store(1000, Ordering::Relaxed);

        // Real is 2x the estimate → factor moves from 1.0 toward 2.0 (EMA, so
        // it lands between), and stays within the clamp band.
        record_token_calibration(2000, 1000);
        let k = token_calibration();
        assert!(
            k > 1.0 && k < 2.0,
            "EMA should land between 1.0 and 2.0, got {k}"
        );
        // Zero inputs are ignored (no divide-by-zero, no change).
        let before = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        record_token_calibration(0, 1000);
        record_token_calibration(1000, 0);
        assert_eq!(CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed), before);

        CFG_TOKEN_CALIBRATION_MILLI.store(saved, Ordering::Relaxed);
    }

    #[test]
    fn estimate_counts_image_by_vision_cost_not_base64_length() {
        // Undecodable base64 must not be counted at ~100k tokens (byte/4); it
        // falls back to the flat per-image figure so it can't drag calibration
        // down and cause later text turns to under-evict.
        let huge = "A".repeat(400_000);
        let est = estimate_message_tokens(&result_image("i1", &huge));
        assert!(
            est < IMAGE_TOKEN_FALLBACK + 1_000,
            "image over-counted ({est}) — should be ~{IMAGE_TOKEN_FALLBACK}"
        );
    }

    #[test]
    fn image_tokens_computed_from_real_dimensions() {
        use base64::Engine;
        // A real 300x300 PNG → 90_000 px / 750 = 120 vision tokens, regardless of
        // its (tiny, well-compressed) base64 length.
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(300, 300));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let b64 = base64::prelude::BASE64_STANDARD.encode(buf.get_ref());

        let est = estimate_message_tokens(&result_image("i1", &b64));
        assert!(
            (120..400).contains(&est),
            "expected ~120 computed image tokens + small wrapper, got {est}"
        );
    }

    #[test]
    fn evict_to_fit_stubs_single_oversized_recent_result() {
        // One turn-pair whose result alone exceeds the target. Stage 3 can't drop
        // it (the last pair is kept for pairing), so stage 4 must stub it in place.
        let big = "x".repeat(400_000);
        let mut h = vec![
            user_text("KICK"),
            assistant_tool_use("t1", "get_xfa", &json!({})),
            result_text("t1", &big),
        ];
        evict_to_fit(&mut h, &[], None, 5_000);

        assert_eq!(h.len(), 3, "pairing preserved — nothing safe to drop");
        assert!(
            block_text(&h[2]).starts_with(ELIDED_MARKER),
            "oversized recent result should be stubbed by stage 4"
        );
    }

    #[test]
    fn evict_to_fit_keeps_everything_under_token_budget() {
        // ~250KB of history — well over the legacy 200KB byte gate — but far under
        // a 1M-window token budget. Nothing may be stubbed or dropped. Regression
        // guard against premature eviction that made the agent re-fetch its own
        // context and loop on re-inspection.
        let big = "x".repeat(250_000); // ~62K estimated tokens
        let original = vec![
            user_text("KICK"),
            assistant_tool_use("t1", "get_xfa", &json!({})),
            result_text("t1", &big),
            assistant_tool_use("t2", "list_states", &json!({})),
            result_text("t2", "small recent result"),
        ];
        let mut h = original.clone();
        evict_to_fit(&mut h, &[], None, 800_000);
        assert_eq!(h, original, "must not evict while under the token budget");
    }

    #[test]
    fn evict_to_fit_drops_oldest_pairs_under_tiny_target() {
        // Several big turn-pairs. A tiny target forces escalation all the way to
        // dropping the oldest (assistant, user) pairs, keeping the kickoff message
        // and the most recent pair, with call↔result pairing intact.
        let big = "x".repeat(50_000);
        let kick = user_text("KICK");
        let mut h = vec![
            kick.clone(),
            assistant_tool_use("t1", "get_xfa", &json!({})),
            result_text("t1", &big),
            assistant_tool_use("t2", "get_xfa", &json!({})),
            result_text("t2", &big),
            assistant_tool_use("t3", "get_xfa", &json!({})),
            result_text("t3", &big),
            assistant_tool_use("t4", "get_xfa", &json!({})),
            result_text("t4", &big),
        ];
        evict_to_fit(&mut h, &[], None, 5_000);

        // Kickoff preserved; dropped down toward the floor (kickoff + last pair).
        assert_eq!(h[0], kick);
        assert!(h.len() <= 3, "expected drop to floor, got {} msgs", h.len());
        // Head after the kickoff is an assistant turn — no orphaned tool result.
        assert!(matches!(h[1], Message::Assistant { .. }));
    }

    #[test]
    fn nothing_is_touched_while_the_prompt_still_fits() {
        // A history that fits the turn's budget is kept verbatim even though it
        // holds an over-threshold text block. What guards this is the budget
        // check in `evict_to_fit`, so the test has to go through it — calling
        // the stubbing pass directly bypasses the very thing under test.
        let original = vec![
            user_text("SYSTEM"),
            assistant_tool_use("tu1", "get_xfa", &json!({})),
            result_text("tu1", &"x".repeat(DEFAULT_ELIDE_TEXT_OVER_CHARS + 100)),
            assistant_tool_use("tu2", "get_structured", &json!({})),
            result_text("tu2", "recent"),
            assistant_tool_use("tu3", "finish", &json!({})),
            result_text("tu3", "done"),
        ];
        let mut h = original.clone();
        evict_to_fit(&mut h, &[], None, 800_000);
        assert_eq!(h, original);
    }
}
