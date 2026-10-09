//! The LLM seam, filled in: the configured endpoint resolved to what
//! [`pipeline::RunConfig`] needs to drive a run.
//!
//! The model id and the output cap live here rather than in the controller —
//! they are provider knowledge, and keeping them on this side is what lets the
//! `pipeline` crate carry no model tables at all. Which provider answers is
//! decided once in [`crate::client`], so nothing above this line branches on
//! it; the rate limit is applied here too, since it is the same per-endpoint
//! knowledge the model resolution already needs.

use pipeline::{ContextBudget, PriceFn};
use rig_agent::agent::model::ModelHandle;
use std::sync::Arc;

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

/// Everything [`pipeline::RunConfig`] needs to run a stage against one
/// resolved model: the handle itself, its pricing, and its output cap.
///
/// No `Debug`: [`PriceFn`] is a trait object closure, and [`ModelHandle`]
/// deliberately carries live process state (clients, credentials).
pub struct ResolvedModel {
    pub model: ModelHandle,
    pub price: PriceFn,
    pub max_tokens: u32,
    /// Shapes a growing stage's history to a budget sized from this model's
    /// context window, and learns from what it actually bills — see
    /// [`crate::token_counter::RunnerContextBudget`].
    pub context_budget: Arc<dyn ContextBudget>,
}

impl ResolvedModel {
    /// The same model as a pipeline stage model, for a role of its own.
    pub fn into_stage_model(self) -> pipeline::StageModel {
        pipeline::StageModel {
            model: self.model,
            price: self.price,
            max_tokens: self.max_tokens,
            context_budget: self.context_budget,
        }
    }
}

/// The models of the roles that have their own (see
/// [`AppSettings::reviewer_model`] and [`AppSettings::judge_model`]), each
/// resolved like the run's.
pub struct RoleModels {
    pub reviewer: Option<(TurnPlan, ResolvedModel)>,
    pub judge: Option<(TurnPlan, ResolvedModel)>,
}

impl RoleModels {
    /// Resolves the roles' own models of `settings`, each rate-limited with the
    /// run's endpoint (they share its gate: one account, one limit).
    pub fn for_settings(settings: &AppSettings) -> Result<Self, String> {
        let resolve = |model: &str, role: &str| -> Result<Option<(TurnPlan, ResolvedModel)>, String> {
            let Some(endpoint) = settings.role_endpoint(model) else {
                return Ok(None);
            };
            let plan = TurnPlan {
                max_concurrent: settings.max_concurrent_requests,
                ..TurnPlan::for_endpoint(endpoint)
            };
            let resolved = plan.resolve().map_err(|e| format!("The {role} model cannot be used: {e}"))?;
            Ok(Some((plan, resolved)))
        };
        Ok(Self {
            reviewer: resolve(&settings.reviewer_model, "Reviewer")?,
            judge: resolve(&settings.judge_model, "judge")?,
        })
    }
}

/// What the banner adds for the roles with a model of their own, or nothing:
/// the Reviewer's and the judges' model ids, when they are not the run's.
pub fn describe_role_models(settings: &AppSettings) -> String {
    let mut text = String::new();
    for (role, model) in [("Reviewer", &settings.reviewer_model), ("judges", &settings.judge_model)] {
        if let Some(endpoint) = settings.role_endpoint(model) {
            text.push_str(&format!(" · {role}: {}", endpoint.model));
        }
    }
    text
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

    /// Resolve the model this plan describes, rate-limited to `max_concurrent`
    /// and priced from the local table.
    ///
    /// Resolving the model here means a missing key or an unbuildable client is
    /// reported before the run starts, not on its first turn.
    pub fn resolve(&self) -> Result<ResolvedModel, String> {
        let model = crate::client::model_for(&self.endpoint)?;
        let model = crate::ratelimit::wrap(model, &self.endpoint, self.max_concurrent);
        let model_id = self.endpoint.model.clone();
        Ok(ResolvedModel {
            model,
            price: Arc::new(move |usage| crate::pricing::cost_usd(&model_id, usage)),
            max_tokens: self.max_tokens,
            context_budget: Arc::new(crate::token_counter::RunnerContextBudget::new(
                self.prompt_target,
            )),
        })
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

    /// A usable endpoint resolves to a model, priced from the local table.
    #[tokio::test]
    async fn a_usable_endpoint_resolves_a_priced_model() {
        let plan = TurnPlan::for_endpoint(LlmEndpoint::anthropic("k", crate::models::DEFAULT_MODEL));
        let resolved = plan.resolve().expect("resolves");
        assert_eq!(resolved.max_tokens, plan.max_tokens);
        let mut usage = rig_core::completion::Usage::new();
        usage.input_tokens = 1000;
        assert!((resolved.price)(&usage).is_some(), "Opus is in the price table");
    }

    /// An unusable endpoint must fail here, before a client exists — the whole
    /// point of resolving up front rather than on the first turn.
    #[test]
    fn an_unusable_endpoint_is_refused_at_resolution() {
        let plan = TurnPlan::for_endpoint(LlmEndpoint::anthropic("", "claude-opus-5"));
        let err = match plan.resolve() {
            Ok(_) => panic!("no key means no model"),
            Err(e) => e,
        };
        assert!(err.contains("API key"), "{err}");
    }
}
