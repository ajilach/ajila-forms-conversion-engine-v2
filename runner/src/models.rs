//! What a model can take and what it can produce: the context window, the
//! output ceiling, and the budget a request's prompt has to fit inside.
//!
//! Provider-independent by design. The transport asks these questions and the
//! settings picker offers the answers, so the table lives on its own rather
//! than inside whichever client happens to be sending the request.

/// Tokens reserved below the context window for the model's own reply plus
/// estimation slack, so the assembled prompt lands comfortably under the hard
/// limit even when the char-based estimate runs low.
const CONTEXT_SAFETY_MARGIN: usize = 48_000;

/// The model's maximum context window in tokens.
///
/// A heuristic that is **optimistic** when the model is not in [`KNOWN_MODELS`]:
/// modern large-context families default to 1M. Guessing high is the safe
/// direction — too-low silently shrinks the reported budget, which used to
/// feed a per-turn eviction loop; that loop is gone (rig runs each stage on
/// its own turn budget now), so this figure is informational only, feeding
/// the context-window banner and gauge denominator. Only known-small models
/// (Haiku, pre-4 families) default to 200K.
pub fn context_window_for(model: &str) -> usize {
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
        id: "claude-opus-5-5",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-fable-5-1",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-sonnet-5-5",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
    ModelInfo {
        id: "claude-haiku-5-5",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
    },
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
/// Haiku before 5 matches none of them and so keeps the small default.
const LARGE_CONTEXT_FAMILIES: &[&str] = &["opus", "sonnet", "fable", "haiku-5"];

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
    "haiku-5",
];

/// Output-token cap for models we don't recognize.
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16_000;

/// The output-token ceiling to request for a given model.
///
/// Every stage streams its turns (rig's `Agent::runner` always does), so we can
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
    if m.contains("haiku") && !LARGE_OUTPUT_FAMILIES.iter().any(|f| m.contains(f)) {
        64_000
    } else if LARGE_OUTPUT_FAMILIES.iter().any(|f| m.contains(f)) {
        128_000
    } else {
        DEFAULT_MAX_OUTPUT_TOKENS
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
    fn context_window_heuristic() {
        // Optimistic heuristic: modern large-context families → 1M (even without a
        // literal `[1m]` in the id, which real API model strings lack); Haiku/older
        // → 200K.
        assert_eq!(context_window_for("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(context_window_for("claude-opus-4-8"), 1_000_000);
        assert_eq!(context_window_for("claude-sonnet-5"), 1_000_000);
        assert_eq!(context_window_for("claude-haiku-4-5-20251001"), 200_000);
    }
}
