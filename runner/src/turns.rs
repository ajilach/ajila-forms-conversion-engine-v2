//! The LLM seam, filled in: the configured endpoint behind
//! [`pipeline::TurnProvider`].
//!
//! The model id and the output cap live here rather than in the controller —
//! they are provider knowledge, and keeping them on this side is what lets the
//! `pipeline` crate carry no model tables at all. Which transport runs the turn
//! (Anthropic Messages or an OpenAI-compatible endpoint) is decided here too, so
//! nothing above this line branches on it.

use pipeline::{AbortFlag, TurnOutput, TurnProvider};

use crate::provider::{LlmEndpoint, Provider};
use crate::settings::AppSettings;

/// The provider-side numbers a run is about to work with.
///
/// Resolved before the first turn and reported, so a mis-detected context window
/// is visible in the transcript instead of showing up as unexplained eviction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnPlan {
    /// Where the turns go, and as which model.
    pub endpoint: LlmEndpoint,
    /// Output-token cap sent with every request.
    pub max_tokens: u32,
    /// The model's full context window.
    pub context_window: usize,
    /// How much of that window a request's prompt may occupy.
    pub prompt_target: usize,
    /// Requests this run may have in flight at once against its endpoint,
    /// shared with every other run pointed at the same one. `0` means no cap.
    pub max_concurrent: usize,
}

impl TurnPlan {
    pub fn for_endpoint(endpoint: LlmEndpoint) -> Self {
        let max_tokens = crate::models::max_output_tokens_for(&endpoint.model);
        Self {
            context_window: crate::models::context_window_for(&endpoint.model),
            prompt_target: crate::models::prompt_token_target(&endpoint.model, max_tokens),
            max_tokens,
            // No cap unless a consumer supplies one: a lone run has nothing to
            // contend with, and the CLI is one run by construction.
            max_concurrent: 0,
            endpoint,
        }
    }

    pub fn for_settings(settings: &AppSettings) -> Self {
        Self {
            max_concurrent: settings.max_concurrent_requests,
            ..Self::for_endpoint(settings.llm_endpoint())
        }
    }

    /// The model this plan resolved its limits for.
    pub fn model(&self) -> &str {
        &self.endpoint.model
    }

    /// The banner every consumer reports before the first turn. One wording, so
    /// a CLI transcript and the app's timeline say the same thing. The endpoint
    /// is named only when it is not the Anthropic default, so an ordinary run
    /// reads exactly as it did before the switch existed.
    pub fn describe(&self) -> String {
        let mut text = format!(
            "Context window: {} tokens · per-turn budget: {} tokens · output cap: {} · model: {}",
            self.context_window, self.prompt_target, self.max_tokens, self.endpoint.model
        );
        if self.endpoint.provider != Provider::Anthropic {
            text.push_str(&format!(" · endpoint: {}", self.endpoint.base_url));
        }
        text
    }

    /// The turn provider these numbers describe.
    pub fn provider(&self) -> ConfiguredTurns {
        ConfiguredTurns {
            endpoint: self.endpoint.clone(),
            max_tokens: self.max_tokens,
            max_concurrent: self.max_concurrent,
        }
    }
}

/// Runs the controller's turns against the configured endpoint.
pub struct ConfiguredTurns {
    endpoint: LlmEndpoint,
    max_tokens: u32,
    /// Snapshotted from the plan, so a run keeps the cap it started under even
    /// if the operator changes the setting while it is going.
    max_concurrent: usize,
}

impl TurnProvider for ConfiguredTurns {
    async fn turn(
        &self,
        history: &mut Vec<serde_json::Value>,
        tools: &[serde_json::Value],
        system: &str,
        abort: &AbortFlag,
    ) -> Result<TurnOutput, String> {
        // Held for the whole turn, streamed response included. Releasing it when
        // the response headers arrive would cap the rate at which requests are
        // *started* while leaving any number of them streaming — which is not
        // what a provider's rate limit counts.
        let _permit = match crate::ratelimit::gate_for(&self.endpoint, self.max_concurrent) {
            Some(gate) => Some(
                gate.acquire_owned()
                    .await
                    .map_err(|e| format!("Request gate closed: {e}"))?,
            ),
            None => None,
        };

        match self.endpoint.provider {
            Provider::Anthropic => {
                crate::llm::anthropic_stream_turn(
                    history,
                    tools,
                    &self.endpoint,
                    self.max_tokens,
                    Some(system),
                    abort,
                )
                .await
            }
            Provider::OpenAi => {
                crate::openai::openai_stream_turn(
                    history,
                    tools,
                    &self.endpoint,
                    self.max_tokens,
                    Some(system),
                    abort,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The banner has to carry the resolved numbers, not the defaults: a window
    /// reported as the fallback is how a silent eviction loop stays invisible.
    #[test]
    fn the_plan_reports_the_model_it_resolved() {
        let plan = TurnPlan::for_endpoint(LlmEndpoint::anthropic("k", crate::models::DEFAULT_MODEL));
        let text = plan.describe();
        assert!(text.contains(crate::models::DEFAULT_MODEL), "{text}");
        assert!(text.contains(&plan.context_window.to_string()), "{text}");
        assert_eq!(
            plan.context_window,
            crate::models::context_window_for(plan.model())
        );
        assert_eq!(
            plan.max_tokens,
            crate::models::max_output_tokens_for(plan.model())
        );
    }

    /// A run against somebody else's endpoint has to say so in the banner —
    /// otherwise a transcript gives no clue which service produced it.
    #[test]
    fn a_non_anthropic_endpoint_is_named_in_the_banner() {
        let plan = TurnPlan::for_endpoint(LlmEndpoint::openai(
            "https://openrouter.ai/api/v1",
            "k",
            "anthropic/claude-opus-4.1",
        ));
        let text = plan.describe();
        assert!(text.contains("https://openrouter.ai/api/v1"), "{text}");
        assert!(
            !TurnPlan::for_endpoint(LlmEndpoint::anthropic("k", "claude-opus-5"))
                .describe()
                .contains("endpoint:")
        );
    }
}
