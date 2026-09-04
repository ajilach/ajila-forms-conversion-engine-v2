//! What a model can take and what it can produce: the context window, the
//! output ceiling, and the budget a request's prompt has to fit inside.
//!
//! Provider-independent by design. The transport asks these questions and the
//! settings picker offers the answers, so the table lives on its own rather
//! than inside whichever client happens to be sending the request.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Tokens reserved below the context window for the model's own reply plus
/// estimation slack, so the assembled prompt lands comfortably under the hard
/// limit even when the char-based estimate runs low.
const CONTEXT_SAFETY_MARGIN: usize = 48_000;

/// Context window learned from the API (parsed from a `prompt is too long: …
/// > N maximum` error). `0` means "not learned yet — use the heuristic". This is
/// authoritative once set, so a wrong heuristic guess self-corrects after at most
/// one overflow. See [`learn_context_window_from_error`].
static CFG_CONTEXT_WINDOW: AtomicUsize = AtomicUsize::new(0);

/// The model's maximum context window in tokens.
///
/// Prefer the value learned from the API; otherwise fall back to a heuristic that
/// is **optimistic** — modern large-context families default to 1M. Guessing high
/// is the safe direction: too-high costs at most one `400` (caught and learned
/// from by [`anthropic_stream_turn`]), whereas too-low silently shrinks the budget
/// and makes the agent evict its own context every turn (an amnesia loop). Only
/// known-small models (Haiku, pre-4 families) default to 200K.
pub fn context_window_for(model: &str) -> usize {
    let learned = CFG_CONTEXT_WINDOW.load(Ordering::Relaxed);
    if learned > 0 {
        return learned;
    }
    if let Some(known) = KNOWN_MODELS.iter().find(|m| m.id == model) {
        return known.context_window;
    }
    let m = model.to_ascii_lowercase();
    let large = m.contains("[1m]")
        || m.contains("-1m")
        || LARGE_CONTEXT_FAMILIES.iter().any(|f| m.contains(f));
    if large { 1_000_000 } else { 200_000 }
}

/// A model this app knows the limits of.
pub struct ModelInfo {
    /// The exact API model id.
    pub id: &'static str,
    /// Maximum context window, in tokens.
    pub context_window: usize,
    /// Maximum tokens the model can emit in one turn.
    pub max_output_tokens: u32,
}

/// The models the settings picker offers, most capable first.
///
/// One table, because these facts used to live in four places that could — and
/// did — disagree: the default model, the picker's offline fallback list, and
/// two family tables that described the same families at different
/// granularities. `KNOWN_MODELS[0]` is the default model.
///
/// The picker normally lists what the API reports; this is the fallback when
/// that call fails, and the authority on limits either way. Models newer than
/// this table still resolve through the family heuristics below.
pub const KNOWN_MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "claude-opus-5",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-sonnet-5",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-opus-4-8",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-sonnet-4-6",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-haiku-4-5",
        context_window: 200_000,
        max_output_tokens: 64_000,
    },
];

/// The model used when none has been chosen.
pub const DEFAULT_MODEL: &str = KNOWN_MODELS[0].id;

/// Families whose models carry a 1M-token context window. Only consulted for ids
/// absent from [`KNOWN_MODELS`] — the API serves models newer than any table we
/// ship, so these are deliberately generation-agnostic: a not-yet-released Opus
/// should inherit the optimistic guess rather than silently fall to 200K.
/// Haiku matches none of them and so keeps the small default.
const LARGE_CONTEXT_FAMILIES: &[&str] = &["opus", "sonnet", "fable"];

/// Families that can emit 128K output tokens in one turn. Same fallback role as
/// [`LARGE_CONTEXT_FAMILIES`].
const LARGE_OUTPUT_FAMILIES: &[&str] = &[
    "opus-4-6",
    "opus-4-7",
    "opus-4-8",
    "opus-5",
    "sonnet-4-6",
    "sonnet-5",
    "fable-5",
];

/// Output-token cap for models we don't recognize.
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16_000;

/// The output-token ceiling to request for a given model.
///
/// The agent loop streams every turn (see [`anthropic_stream_turn`]), so we can
/// request up to the model's true max output without risking the HTTP timeouts
/// that cap non-streaming requests near 16k. `max_tokens` is a ceiling, not a
/// target — we're billed only for tokens actually generated — so requesting the
/// full max costs nothing extra and just lets a large authoring turn complete in
/// one call.
///
/// Matches on family substrings so date/suffix variants (e.g. `-20251001`,
/// `[1m]`) still resolve; unrecognized models fall back to
/// [`DEFAULT_MAX_OUTPUT_TOKENS`]. Lives here next to [`context_window_for`] so
/// the two model tables stay in one place.
pub fn max_output_tokens_for(model: &str) -> u32 {
    if let Some(known) = KNOWN_MODELS.iter().find(|m| m.id == model) {
        return known.max_output_tokens;
    }
    let m = model.to_ascii_lowercase();
    if m.contains("haiku") {
        64_000
    } else if LARGE_OUTPUT_FAMILIES.iter().any(|f| m.contains(f)) {
        128_000
    } else {
        DEFAULT_MAX_OUTPUT_TOKENS
    }
}

/// Whether an error body says the request blew the model's context window.
///
/// Both dialects are recognized, because both are reachable: Anthropic's
/// `prompt is too long: X tokens > N maximum` and the OpenAI-compatible
/// `This model's maximum context length is N tokens` (and OpenRouter's
/// `context_length_exceeded` code, which it echoes in the message).
pub(crate) fn is_context_overflow(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("prompt is too long")
        || m.contains("maximum context length")
        || m.contains("context_length_exceeded")
        || m.contains("context length exceeded")
}

/// The real context window an overflow error reports, in tokens.
///
/// Both dialects are read, because both are reachable: the Anthropic phrasing
/// (`prompt is too long: X tokens > N maximum`) and the OpenAI-compatible one
/// (`This model's maximum context length is N tokens`). `None` for a message
/// that names no limit, which leaves the heuristic in charge.
fn parse_context_limit(msg: &str) -> Option<usize> {
    let after_marker = |marker: &str| {
        let lower = msg.to_ascii_lowercase();
        let at = lower.find(marker)? + marker.len();
        msg[at..].split_whitespace().find_map(|tok| {
            tok.trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<usize>()
                .ok()
        })
    };
    msg.split('>')
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|tok| tok.parse::<usize>().ok())
        .or_else(|| after_marker("maximum context length is"))
}

/// Learn the real context window from an overflow error, clamping the stored
/// window to the smallest maximum any endpoint has reported.
pub(crate) fn learn_context_window_from_error(msg: &str) {
    let Some(n) = parse_context_limit(msg) else {
        return;
    };
    let prev = CFG_CONTEXT_WINDOW.load(Ordering::Relaxed);
    if prev == 0 || n < prev {
        CFG_CONTEXT_WINDOW.store(n, Ordering::Relaxed);
    }
}

/// The estimated-token budget for the assembled prompt (messages + tools +
/// system), leaving room for the reply and estimation slack.
pub fn prompt_token_target(model: &str, max_tokens: u32) -> usize {
    context_window_for(model).saturating_sub(max_tokens as usize + CONTEXT_SAFETY_MARGIN)
}

#[cfg(test)]
mod known_models {
    use super::*;

    /// Every model the picker offers must resolve to its table limits, not to a
    /// heuristic guess.
    #[test]
    fn every_known_model_resolves_to_its_own_limits() {
        for model in KNOWN_MODELS {
            assert_eq!(
                context_window_for(model.id),
                model.context_window,
                "{} resolved to the wrong context window",
                model.id
            );
            assert_eq!(
                max_output_tokens_for(model.id),
                model.max_output_tokens,
                "{} resolved to the wrong output cap",
                model.id
            );
        }
    }

    /// Regression: the family tables listed the same families at different
    /// granularities and neither knew about `opus-5`, so the model the app was
    /// about to default to would have resolved to a 200K window — a fifth of its
    /// real one — and quietly evicted its own context every turn.
    #[test]
    fn the_family_heuristics_agree_with_the_table() {
        for model in KNOWN_MODELS {
            let lower = model.id.to_ascii_lowercase();
            let by_family = LARGE_CONTEXT_FAMILIES.iter().any(|f| lower.contains(f));
            assert_eq!(
                by_family,
                model.context_window > 200_000,
                "{} disagrees between LARGE_CONTEXT_FAMILIES and the table",
                model.id
            );

            let large_output = LARGE_OUTPUT_FAMILIES.iter().any(|f| lower.contains(f));
            assert_eq!(
                large_output,
                model.max_output_tokens >= 128_000,
                "{} disagrees between LARGE_OUTPUT_FAMILIES and the table",
                model.id
            );
        }
    }

    /// A model newer than the table still has to get a usable budget — guessing
    /// high costs one recoverable 400, guessing low starts an amnesia loop. That
    /// covers both a dated or suffixed variant of a known family and a
    /// generation this table has never heard of.
    #[test]
    fn an_unknown_model_falls_back_to_the_heuristics() {
        for unknown in [
            "claude-opus-5-20260101",
            "claude-sonnet-5[1m]",
            "claude-opus-6-future",
        ] {
            assert_eq!(
                context_window_for(unknown),
                1_000_000,
                "{unknown} must inherit the optimistic guess"
            );
        }
        // Haiku is the one family that is genuinely small.
        assert_eq!(context_window_for("claude-haiku-9-future"), 200_000);
        assert_eq!(max_output_tokens_for("claude-haiku-9-future"), 64_000);
        assert_eq!(
            max_output_tokens_for("some-other-vendor-model"),
            DEFAULT_MAX_OUTPUT_TOKENS
        );
    }

    #[test]
    fn the_default_model_is_one_the_picker_offers() {
        assert!(KNOWN_MODELS.iter().any(|m| m.id == DEFAULT_MODEL));
        assert_eq!(
            crate::settings::AppSettings::default().anthropic_model,
            DEFAULT_MODEL
        );
    }

    #[test]
    fn context_overflow_is_recognized_in_both_dialects() {
        let anthropic = "prompt is too long: 1050000 tokens > 1000000 maximum";
        let openai = "This model's maximum context length is 128000 tokens. \
                      However, your messages resulted in 130000 tokens.";
        assert!(is_context_overflow(anthropic));
        assert!(is_context_overflow(openai));
        assert!(!is_context_overflow("invalid x-api-key"));
        assert_eq!(parse_context_limit(anthropic), Some(1_000_000));
        assert_eq!(parse_context_limit(openai), Some(128_000));
        assert_eq!(parse_context_limit("something else entirely"), None);
    }

    /// The two model tables live together; keep their groupings pinned so a new
    /// model id cannot silently fall into the wrong bucket.
    #[test]
    fn model_limits_resolve_by_family() {
        // Suffix variants must still resolve.
        assert_eq!(max_output_tokens_for("claude-opus-4-8-20260101"), 128_000);
        assert_eq!(max_output_tokens_for("claude-haiku-4-5"), 64_000);
        // Unknown ids fall back rather than over-promising.
        assert_eq!(
            max_output_tokens_for("claude-something-new"),
            DEFAULT_MAX_OUTPUT_TOKENS
        );
        // Matching is case-insensitive.
        assert_eq!(max_output_tokens_for("Claude-Opus-4-8"), 128_000);
    }

    #[test]
    fn context_window_heuristic_and_learning() {
        // Isolate the shared learned-window state.
        let saved = CFG_CONTEXT_WINDOW.load(Ordering::Relaxed);
        CFG_CONTEXT_WINDOW.store(0, Ordering::Relaxed);

        // Optimistic heuristic: modern large-context families → 1M (even without a
        // literal `[1m]` in the id, which real API model strings lack); Haiku/older
        // → 200K.
        assert_eq!(context_window_for("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(context_window_for("claude-opus-4-8"), 1_000_000);
        assert_eq!(context_window_for("claude-sonnet-5"), 1_000_000);
        assert_eq!(context_window_for("claude-haiku-4-5-20251001"), 200_000);

        // A 400 teaches the real maximum; it's authoritative and only ratchets down.
        learn_context_window_from_error("prompt is too long: 1316205 tokens > 250000 maximum");
        assert_eq!(context_window_for("claude-opus-4-8"), 250_000);
        learn_context_window_from_error("prompt is too long: 900000 tokens > 500000 maximum");
        assert_eq!(context_window_for("claude-opus-4-8"), 250_000);

        CFG_CONTEXT_WINDOW.store(saved, Ordering::Relaxed);
    }
}
