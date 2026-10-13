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
//! 2026-10-02, and Sonnet 5.5's cache read and Haiku 5.5 on 2026-10-09. The
//! other vendors' rows ([`crate::models::OPENROUTER_MODELS`]) are each vendor's
//! own published rates on 2026-10-13: OpenRouter passes a provider's price
//! through, and a provider it routes to may charge less than the vendor.
//!
//! The two endpoints count a prompt differently. Anthropic reports fresh input,
//! cache reads and cache writes as three disjoint buckets; an OpenAI-compatible
//! endpoint (OpenRouter included) reports `prompt_tokens` as the whole prompt,
//! with the cached and written tokens as a breakdown of it. Pricing the second
//! as if it were the first bills every cached token twice, once at the input
//! rate, so [`disjoint_usage`] brings it to Anthropic's shape first.

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

/// Published rates for the models in [`crate::models::KNOWN_MODELS`] and
/// [`crate::models::OPENROUTER_MODELS`].
pub const PRICES: &[Price] = &[
    // OpenAI. Cache writes are billed (2.50), unlike earlier GPT generations.
    Price {
        id: "openai/gpt-6.1-sol",
        input: 2.00,
        output: 10.00,
        cache_read: 0.10,
        cache_write: 2.50,
        long_prompt: None,
    },
    // Google. Introductory rates, valid to 2026-12-31; from 2027-01-01 they
    // double (1.50 / 7.50 / 0.15). Implicit caching bills no write, so a
    // written token costs the input rate; cache storage per hour is not
    // modelled.
    Price {
        id: "google/gemini-3.8-flash",
        input: 0.75,
        output: 3.75,
        cache_read: 0.075,
        cache_write: 0.75,
        long_prompt: None,
    },
    // DeepSeek. Peak rates: off-peak (outside 01-04 and 06-10 UTC on weekdays,
    // and all weekend) is half of these, so a run is reported at most at what
    // it cost. No write is billed; a cache miss is the input rate.
    Price {
        id: "deepseek/deepseek-v4.1-flash",
        input: 0.30,
        output: 1.20,
        cache_read: 0.006,
        cache_write: 0.30,
        long_prompt: None,
    },
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
/// Ids are compared as [`crate::models::canonical_id`] spells them, so the
/// same table prices `claude-opus-5-5` on the Anthropic path and
/// `anthropic/claude-opus-5.5` routed through OpenRouter. Beyond that the
/// match is exact: a family guess would be a made-up number presented as a
/// bill.
pub fn price_for(model: &str) -> Option<&'static Price> {
    let wanted = crate::models::canonical_id(model);
    PRICES.iter().find(|p| crate::models::canonical_id(p.id) == wanted)
}

/// `usage` from an OpenAI-compatible endpoint, in Anthropic's disjoint
/// buckets: the fresh input is what is left of the prompt once the cached and
/// written tokens are taken out. See the module docs.
pub fn disjoint_usage(usage: &rig_core::completion::Usage) -> rig_core::completion::Usage {
    let mut disjoint = *usage;
    disjoint.input_tokens = usage
        .input_tokens
        .saturating_sub(usage.cached_input_tokens + usage.cache_creation_input_tokens);
    disjoint
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

    /// OpenRouter spells Anthropic's versions with dots; they must price the
    /// same as Anthropic's dashed ids, or an OpenRouter run reports no cost.
    #[test]
    fn a_dotted_openrouter_id_prices_the_same() {
        assert_eq!(price_for("anthropic/claude-opus-5.5").map(|p| p.input), Some(4.00));
        assert_eq!(price_for("anthropic/claude-haiku-5.5").map(|p| p.id), Some("claude-haiku-5-5"));
    }

    /// Every OpenRouter model the limits table knows is priced too.
    #[test]
    fn every_openrouter_model_has_a_price() {
        for model in crate::models::OPENROUTER_MODELS {
            assert!(price_for(model.id).is_some(), "{} has no row in PRICES", model.id);
        }
        assert_eq!(price_for("gpt-6.1-sol").map(|p| p.output), Some(10.00));
    }

    /// An OpenAI-compatible prompt of 1M tokens, 800k of them cached and 100k
    /// written, is 100k fresh: priced as reported it would bill the cached
    /// tokens at the input rate on top of the cache-read rate.
    #[test]
    fn an_openai_style_prompt_is_split_before_it_is_priced() {
        let reported = Usage {
            input_tokens: 1_000_000,
            cached_input_tokens: 800_000,
            cache_creation_input_tokens: 100_000,
            output_tokens: 0,
            ..Usage::new()
        };
        let disjoint = disjoint_usage(&reported);
        assert_eq!(disjoint.input_tokens, 100_000);
        assert_eq!(disjoint.cached_input_tokens, 800_000);
        let cost = cost_usd("openai/gpt-6.1-sol", &disjoint).unwrap();
        // 0.1 * 2.00 + 0.8 * 0.10 + 0.1 * 2.50
        assert!((cost - 0.53).abs() < 1e-9, "got {cost}");
        // A gateway that reports no breakdown leaves the prompt as it is.
        let plain = Usage { input_tokens: 500, ..Usage::new() };
        assert_eq!(disjoint_usage(&plain).input_tokens, 500);
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
