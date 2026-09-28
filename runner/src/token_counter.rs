//! What a message actually costs, and the running correction learned from
//! the API's own billed usage.
//!
//! `image_token_cost` and the calibration EMA below are recovered from the
//! hand-rolled eviction ladder this crate used to drive
//! (`fcd916c~1:runner/src/context.rs`), which Part 2 of the rig migration
//! deleted wholesale instead of porting this half first as planned. Nothing
//! else from that file survives: the eviction passes it drove are rig's
//! `MemoryPolicy` job now (see [`RunnerContextBudget`]), not ours.
//!
//! [`rig_memory::HeuristicTokenCounter`] is deliberately not used here: it
//! charges a flat 256 tokens for every image, where a rendered page really
//! costs up to ~1533 (Anthropic's vision cost scales with pixel area). A
//! provider-agnostic default cannot know that; this crate, which already
//! renders the pages, does.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pipeline::ContextBudget;
use rig_core::message::{AssistantContent, DocumentSourceKind, Message, ToolResultContent, UserContent};
use rig_memory::{MemoryPolicy, TokenCounter, TokenWindowMemory};

// ── Real vision-token cost ───────────────────────────────────────────────────

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

/// Raw (uncalibrated) token estimate for one message: serialized byte length
/// ÷ 4 (the byte count already includes every brace/quote/colon, so this
/// tracks real tokens well for text and JSON alike), except that images are
/// counted at their real vision cost rather than their base64 length.
fn estimate_message_tokens(msg: &Message) -> usize {
    let total = serde_json::to_string(msg).map(|s| s.len()).unwrap_or(0);
    let (image_bytes, image_tokens) = image_payload_stats(msg);
    total.saturating_sub(image_bytes) / 4 + image_tokens
}

// ── Calibration EMA ──────────────────────────────────────────────────────────

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
/// raw value.
fn record_token_calibration(real: usize, estimate: usize) {
    if real == 0 || estimate == 0 {
        return;
    }
    let observed = (real as f64 / estimate as f64).clamp(1.0, 8.0);
    let blended = (token_calibration() * 0.7 + observed * 0.3).clamp(1.0, 8.0);
    CFG_TOKEN_CALIBRATION_MILLI.store((blended * 1000.0) as u64, Ordering::Relaxed);
}

// ── The rig-facing pieces ────────────────────────────────────────────────────

/// A [`TokenCounter`] whose per-message cost is the calibrated estimate above
/// — what [`TokenWindowMemory`] actually budgets against.
#[derive(Debug, Default, Clone, Copy)]
struct CalibratedTokenCounter;

impl TokenCounter for CalibratedTokenCounter {
    fn count(&self, message: &Message) -> usize {
        calibrated_tokens(estimate_message_tokens(message))
    }
}

/// The [`ContextBudget`] `pipeline` shapes a stage's history through: a
/// [`TokenWindowMemory`] over the calibrated counter, budgeted to
/// `prompt_target` tokens — the same figure `evict_to_fit` used to budget
/// against (`models::prompt_token_target`).
pub struct RunnerContextBudget {
    policy: Arc<dyn MemoryPolicy>,
}

impl RunnerContextBudget {
    pub fn new(prompt_target: usize) -> Self {
        Self {
            policy: Arc::new(TokenWindowMemory::new(prompt_target, CalibratedTokenCounter)),
        }
    }
}

impl ContextBudget for RunnerContextBudget {
    fn policy(&self) -> Arc<dyn MemoryPolicy> {
        self.policy.clone()
    }

    fn raw_estimate(&self, history: &[Message]) -> usize {
        history.iter().map(estimate_message_tokens).sum()
    }

    fn record_actual(&self, raw_estimate: usize, real_tokens: u64) {
        record_token_calibration(real_tokens as usize, raw_estimate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{Text, ToolCall, ToolCallId, ToolFunction, ToolResult};
    use serde_json::json;
    use std::sync::Mutex;

    /// The calibration EMA is one process-wide static; tests that touch it
    /// have to run one at a time or they perturb each other.
    static CALIBRATION_LOCK: Mutex<()> = Mutex::new(());

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
        let b64 = base64::prelude::BASE64_STANDARD.encode(buf.into_inner());

        assert_eq!(image_token_cost(&b64), 90_000 / PX_PER_TOKEN);
    }

    #[test]
    fn a_tool_call_and_a_plain_text_reply_cost_roughly_their_serialized_size() {
        let text = result_text("t1", &"x".repeat(400));
        let est = estimate_message_tokens(&text);
        // Serialized JSON is a bit over 400 bytes (the envelope around the
        // text); byte/4 should land in the right order of magnitude.
        assert!((90..=150).contains(&est), "got {est}");

        let call = assistant_tool_use("t1", "xfa_read", &json!({"state": "DE"}));
        assert!(estimate_message_tokens(&call) > 0);
    }

    #[test]
    fn calibration_ema_moves_toward_observed_and_clamps() {
        let _guard = CALIBRATION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        CFG_TOKEN_CALIBRATION_MILLI.store(1000, Ordering::Relaxed);

        // Real is 2x the estimate → factor moves from 1.0 toward 2.0 (EMA, so
        // it lands between), and stays within the clamp band.
        record_token_calibration(2000, 1000);
        let k = token_calibration();
        assert!(k > 1.0 && k < 2.0, "EMA should land between 1.0 and 2.0, got {k}");

        // Zero inputs are ignored (no divide-by-zero, no change).
        let before = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        record_token_calibration(0, 1000);
        record_token_calibration(1000, 0);
        assert_eq!(CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed), before);

        CFG_TOKEN_CALIBRATION_MILLI.store(saved, Ordering::Relaxed);
    }

    #[test]
    fn the_calibrated_counter_scales_by_the_learned_factor() {
        let _guard = CALIBRATION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        CFG_TOKEN_CALIBRATION_MILLI.store(2000, Ordering::Relaxed); // factor = 2.0

        let msg = user_text("hello world");
        let raw = estimate_message_tokens(&msg);
        let counted = CalibratedTokenCounter.count(&msg);
        assert_eq!(counted, raw * 2);

        CFG_TOKEN_CALIBRATION_MILLI.store(saved, Ordering::Relaxed);
    }

    /// `RunnerContextBudget::raw_estimate` must be uncalibrated — it is the
    /// figure `record_actual` compares real usage against, so if it already
    /// baked in the calibration factor, calibration would compound on
    /// itself every turn instead of converging on the true ratio.
    #[test]
    fn raw_estimate_is_not_scaled_by_calibration() {
        let _guard = CALIBRATION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        CFG_TOKEN_CALIBRATION_MILLI.store(3000, Ordering::Relaxed); // factor = 3.0

        let budget = RunnerContextBudget::new(1_000_000);
        let history = vec![user_text("hello"), Message::assistant("hi")];
        let expected: usize = history.iter().map(estimate_message_tokens).sum();
        assert_eq!(budget.raw_estimate(&history), expected);

        CFG_TOKEN_CALIBRATION_MILLI.store(saved, Ordering::Relaxed);
    }

    /// `record_actual` is the one seam `pipeline` uses to feed the
    /// calibration EMA — pin that it actually reaches the same static the
    /// counter reads, not a disconnected copy.
    #[test]
    fn record_actual_feeds_the_same_calibration_the_counter_reads() {
        let _guard = CALIBRATION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = CFG_TOKEN_CALIBRATION_MILLI.load(Ordering::Relaxed);
        CFG_TOKEN_CALIBRATION_MILLI.store(1000, Ordering::Relaxed);

        let budget = RunnerContextBudget::new(1_000_000);
        budget.record_actual(1000, 2000);
        assert!(token_calibration() > 1.0, "the shared calibration factor must have moved");

        CFG_TOKEN_CALIBRATION_MILLI.store(saved, Ordering::Relaxed);
    }
}
