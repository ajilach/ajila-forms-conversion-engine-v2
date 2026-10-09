//! What a run costs.
//!
//! OpenRouter does report a `cost` per response, but rig's normalized `Usage`
//! has no slot for it and drops it in conversion, so it is not reachable from
//! here — both paths are priced from this table. A table that can go stale is
//! worth having only if it refuses to guess: an id it does not know yields
//! `None`, and the operator sees token counts with no figure rather than a
//! wrong one.
//!
//! Rates are USD per million tokens, matching how both providers bill. Verified
//! against OpenRouter's published Anthropic pricing on 2026-09-04; Opus 5.5,
//! Sonnet 5.5 and Fable 5.1 against Anthropic's published pricing on
//! 2026-10-02, and Sonnet 5.5's cache read and Haiku 5.5 on 2026-10-09.

/// What one model costs, in USD per million tokens.
pub struct Price {
    /// The exact API model id.
    pub id: &'static str,
    pub input: f64,
    pub output: f64,
    /// Tokens served from the cache: a tenth of the input rate up to Opus 5,
    /// a flat rate from the 5.5 generation on.
    pub cache_read: f64,
    /// Tokens written to the cache. This is the 5-minute rate (1.25x input);
    /// the 1-hour TTL we put on the static prefix costs 2x input, so a run whose
    /// prefix is large relative to its tail is charged slightly more than this
    /// reports. The tail, rewritten every turn, is the bulk of cache writes and
    /// is billed at exactly this rate.
    pub cache_write: f64,
    /// The rates of a request whose prompt is longer than a threshold, for a
    /// model priced in tiers (Haiku 5.5). The whole request is billed at
    /// them, not just the tokens above the threshold.
    pub long_prompt: Option<LongPrompt>,
}

/// A model's rates above a prompt length (see [`Price::long_prompt`]).
pub struct LongPrompt {
    /// Prompt tokens (fresh, cache reads and cache writes together) above
    /// which the request is billed at these rates.
    pub above: u64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Published rates for the models in [`crate::models::KNOWN_MODELS`].
pub const PRICES: &[Price] = &[
    Price {
        id: "claude-opus-5-5",
        input: 4.00,
        output: 20.00,
        cache_read: 0.20,
        cache_write: 5.00,
        long_prompt: None,
    },
    Price {
        id: "claude-sonnet-5-5",
        input: 2.00,
        output: 10.00,
        cache_read: 0.10,
        cache_write: 2.50,
        long_prompt: None,
    },
    Price {
        id: "claude-haiku-5-5",
        input: 0.10,
        output: 0.50,
        cache_read: 0.01,
        cache_write: 0.125,
        long_prompt: Some(LongPrompt {
            above: 100_000,
            input: 0.50,
            output: 2.50,
            cache_read: 0.05,
            cache_write: 0.625,
        }),
    },
    Price {
        id: "claude-fable-5-1",
        input: 10.00,
        output: 50.00,
        cache_read: 0.25,
        cache_write: 12.50,
        long_prompt: None,
    },
    Price {
        id: "claude-opus-5",
        input: 5.00,
        output: 25.00,
        cache_read: 0.50,
        cache_write: 6.25,
        long_prompt: None,
    },
    Price {
        id: "claude-sonnet-5",
        input: 2.00,
        output: 10.00,
        cache_read: 0.20,
        cache_write: 2.50,
        long_prompt: None,
    },
    Price {
        id: "claude-opus-4-8",
        input: 5.00,
        output: 25.00,
        cache_read: 0.50,
        cache_write: 6.25,
        long_prompt: None,
    },
    Price {
        id: "claude-sonnet-4-6",
        input: 3.00,
        output: 15.00,
        cache_read: 0.30,
        cache_write: 3.75,
        long_prompt: None,
    },
    Price {
        id: "claude-haiku-4-5",
        input: 1.00,
        output: 5.00,
        cache_read: 0.10,
        cache_write: 1.25,
        long_prompt: None,
    },
];

/// The published rates for `model`, or `None` for an id this build has never
/// heard of.
///
/// A leading vendor segment is stripped first, so the same table prices
/// `claude-opus-5` on the Anthropic path and `anthropic/claude-opus-5` routed
/// through OpenRouter. Beyond that the match is exact: a family guess would be
/// a made-up number presented as a bill.
pub fn price_for(model: &str) -> Option<&'static Price> {
    let bare = model.rsplit('/').next().unwrap_or(model);
    PRICES.iter().find(|p| p.id == bare)
}

/// What one call's token buckets cost in USD, or `None` when the model has no
/// published rate here.
///
/// A tiered model is priced by the request's prompt length; `usage` is one
/// request's, which is how every caller hands it over (a turn at a time).
pub fn cost_usd(model: &str, usage: &rig_core::completion::Usage) -> Option<f64> {
    let price = price_for(model)?;
    let prompt = usage.input_tokens + usage.cached_input_tokens + usage.cache_creation_input_tokens;
    let (input, output, cache_read, cache_write) = match &price.long_prompt {
        Some(long) if prompt > long.above => (long.input, long.output, long.cache_read, long.cache_write),
        _ => (price.input, price.output, price.cache_read, price.cache_write),
    };
    let per_million = |tokens: u64, rate: f64| tokens as f64 * rate / 1_000_000.0;
    Some(
        per_million(usage.input_tokens, input)
            + per_million(usage.output_tokens, output)
            + per_million(usage.cached_input_tokens, cache_read)
            + per_million(usage.cache_creation_input_tokens, cache_write),
    )
}

/// What `model` costs, as the settings picker shows it beside the id: USD
/// per million input and output tokens, and the long-prompt rates of a
/// tiered model. `None` for a model with no published rate here.
pub fn price_label(model: &str) -> Option<String> {
    let price = price_for(model)?;
    // Whole dollars bare, cents to two places: "4", "0.10", "2.50".
    let rate = |r: f64| if r.fract() == 0.0 { format!("{r}") } else { format!("{r:.2}") };
    let mut label = format!("USD {} in · {} out per M tokens", rate(price.input), rate(price.output));
    if let Some(long) = &price.long_prompt {
        label.push_str(&format!(
            " (prompts over {}k: {} · {})",
            long.above / 1_000,
            rate(long.input),
            rate(long.output)
        ));
    }
    Some(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::Usage;

    /// Every model the picker offers must be priced, or the operator sees a
    /// run report no cost at all for a perfectly ordinary model.
    #[test]
    fn every_known_model_has_a_price() {
        for model in crate::models::KNOWN_MODELS {
            assert!(
                price_for(model.id).is_some(),
                "{} has no row in PRICES",
                model.id
            );
        }
    }

    /// An unpriced model must yield nothing rather than a plausible-looking
    /// zero — a run that silently reports free is worse than one that reports
    /// nothing.
    #[test]
    fn an_unknown_model_is_not_priced_at_zero() {
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Usage::new()
        };
        assert_eq!(cost_usd("some-model-from-the-future", &usage), None);
    }

    /// The same model routed through OpenRouter carries a vendor prefix, and
    /// must price identically — otherwise switching endpoint silently turns
    /// the cost display off.
    #[test]
    fn a_vendor_prefixed_id_prices_the_same() {
        assert_eq!(
            price_for("anthropic/claude-opus-5").map(|p| p.input),
            price_for("claude-opus-5").map(|p| p.input)
        );
        assert!(price_for("openai/gpt-5").is_none());
    }

    /// The arithmetic, on round numbers: a million input tokens of Opus is its
    /// input rate, and the cache buckets are billed at their own rates.
    #[test]
    fn each_bucket_is_billed_at_its_own_rate() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cached_input_tokens: 1_000_000,
            cache_creation_input_tokens: 1_000_000,
            ..Usage::new()
        };
        let cost = cost_usd("claude-opus-5", &usage).expect("opus is priced");
        // 5.00 + 25.00 + 0.50 + 6.25
        assert!((cost - 36.75).abs() < 1e-9, "got {cost}");
    }

    /// Haiku 5.5 is billed by prompt length: a request over 100k prompt
    /// tokens pays the higher rates on all of it, cache reads included.
    #[test]
    fn a_tiered_model_bills_a_long_prompt_at_its_long_rates() {
        let short = Usage { input_tokens: 50_000, cached_input_tokens: 50_000, output_tokens: 1_000, ..Usage::new() };
        let cost = cost_usd("claude-haiku-5-5", &short).unwrap();
        // 0.05 * 0.10 + 0.05 * 0.01 + 0.001 * 0.50
        assert!((cost - 0.006).abs() < 1e-9, "got {cost}");
        let long = Usage { input_tokens: 50_001, cached_input_tokens: 50_000, output_tokens: 1_000, ..Usage::new() };
        let cost = cost_usd("claude-haiku-5-5", &long).unwrap();
        // 0.050001 * 0.50 + 0.05 * 0.05 + 0.001 * 2.50
        assert!((cost - (0.050_001 * 0.50 + 0.0025 + 0.0025)).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn the_picker_shows_each_models_price() {
        assert_eq!(price_label("claude-opus-5-5").unwrap(), "USD 4 in · 20 out per M tokens");
        assert_eq!(
            price_label("claude-haiku-5-5").unwrap(),
            "USD 0.10 in · 0.50 out per M tokens (prompts over 100k: 0.50 · 2.50)"
        );
        assert_eq!(price_label("anthropic/claude-sonnet-5-5").unwrap(), "USD 2 in · 10 out per M tokens");
        assert_eq!(price_label("some-model-from-the-future"), None);
    }

    /// Caching is the whole reason the buckets are separate: reading a cached
    /// prompt has to cost a tenth of sending it fresh.
    #[test]
    fn a_cache_hit_costs_a_tenth_of_a_fresh_prompt() {
        let fresh = cost_usd(
            "claude-sonnet-5",
            &Usage {
                input_tokens: 1_000_000,
                ..Usage::new()
            },
        )
        .unwrap();
        let cached = cost_usd(
            "claude-sonnet-5",
            &Usage {
                cached_input_tokens: 1_000_000,
                ..Usage::new()
            },
        )
        .unwrap();
        assert!((fresh / cached - 10.0).abs() < 1e-9, "{fresh} vs {cached}");
    }
}
