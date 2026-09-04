//! Turning a configured endpoint into a model that can run a turn.
//!
//! One function, [`model_for`], is the only place in the app that names a rig
//! provider. Everything above it holds a [`ModelHandle`] and never learns which
//! dialect answered — the same role `LlmEndpoint` plays for the settings.
//!
//! Caching is configured here rather than at the call site because it is
//! provider knowledge: Anthropic wants explicit breakpoints, OpenRouter places
//! its own, and neither is the controller's business.

use rig_agent::agent::model::ModelHandle;
use rig_core::client::{CompletionClient, ModelLister};
use rig_core::model::Model;
use rig_core::providers::{anthropic, openai, openrouter};

use crate::provider::{DEFAULT_OPENAI_BASE_URL, LlmEndpoint, Provider};

/// Cache lifetime for the static prefix — the tools array and the system
/// prompt, which are byte-identical for every turn of a run.
///
/// An hour rather than the 5-minute default: a run can stall for longer than
/// five minutes on a slow tool or an operator retry prompt, and re-writing the
/// whole prefix afterwards is the expensive outcome. A 1-hour write costs ~2x
/// base input where a 5-minute write costs ~1.25x, so this only pays off on the
/// prefix — the conversation tail keeps the 5-minute default, which is what
/// Anthropic's moving breakpoint applies.
const STATIC_PREFIX_TTL: anthropic::completion::CacheTtl =
    anthropic::completion::CacheTtl::OneHour;

/// Build the model `endpoint` names.
///
/// Fails with the operator-facing message from [`LlmEndpoint::check`] when the
/// endpoint is not usable, so a missing key is reported before the run spends a
/// token rather than as a `401` mid-stream.
pub fn model_for(endpoint: &LlmEndpoint) -> Result<ModelHandle, String> {
    endpoint.check()?;

    match endpoint.provider {
        Provider::Anthropic => {
            let client = anthropic::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .build()
                .map_err(|e| format!("Anthropic client: {e}"))?;
            // `automatic_caching` lets Anthropic own the moving breakpoint on
            // the conversation tail; rig still marks the tools and the system
            // prompt, at the longer TTL.
            let model = client
                .completion_model(&endpoint.model)
                .with_automatic_caching()
                .with_static_prefix_cache_ttl(STATIC_PREFIX_TTL);
            Ok(ModelHandle::named("anthropic", model))
        }
        // OpenRouter is the default and the one this switch was added for; it
        // reports `cost` per response and places its own cache breakpoints.
        // Any other base URL is somebody's OpenAI-compatible gateway.
        Provider::OpenAi if endpoint.base_url == DEFAULT_OPENAI_BASE_URL => {
            let client = openrouter::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .build()
                .map_err(|e| format!("OpenRouter client: {e}"))?;
            Ok(ModelHandle::named(
                "openrouter",
                client.completion_model(&endpoint.model),
            ))
        }
        Provider::OpenAi => {
            let client = openai::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .build()
                .map_err(|e| format!("OpenAI-compatible client at {}: {e}", endpoint.base_url))?;
            Ok(ModelHandle::named(
                "openai",
                client.completion_model(&endpoint.model),
            ))
        }
    }
}

/// The models `endpoint` offers, as the provider reports them.
///
/// Anthropic populates only the id and display name; OpenRouter also fills in
/// `context_length` and `max_output_tokens`, which is why the limits table is a
/// fallback rather than dead weight.
pub async fn list_models(endpoint: &LlmEndpoint) -> Result<Vec<Model>, String> {
    let listed = match endpoint.provider {
        Provider::Anthropic => {
            let client = anthropic::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .build()
                .map_err(|e| format!("Anthropic client: {e}"))?;
            anthropic::model_listing::AnthropicModelLister::new(client)
                .list_all()
                .await
        }
        Provider::OpenAi if endpoint.base_url == DEFAULT_OPENAI_BASE_URL => {
            let client = openrouter::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .build()
                .map_err(|e| format!("OpenRouter client: {e}"))?;
            openrouter::model_listing::OpenRouterModelLister::new(client)
                .list_all()
                .await
        }
        Provider::OpenAi => {
            let client = openai::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .build()
                .map_err(|e| format!("OpenAI-compatible client at {}: {e}", endpoint.base_url))?;
            openai::model_listing::OpenAIModelLister::new(client)
                .list_all()
                .await
        }
    };
    listed
        .map(|list| list.data)
        .map_err(|e| format!("Could not list models at {}: {e}", endpoint.base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing key must fail here, before a request is built — the whole
    /// point of `check()` running at resolution time.
    #[test]
    fn an_unusable_endpoint_is_refused_before_a_client_exists() {
        let err = model_for(&LlmEndpoint::anthropic("", "claude-opus-5")).unwrap_err();
        assert!(err.contains("API key"), "{err}");
    }

    /// Every provider variant has to resolve; a new one must not fall into a
    /// silent default.
    #[test]
    fn every_provider_resolves_to_a_model() {
        for provider in Provider::ALL {
            let endpoint = match provider {
                Provider::Anthropic => LlmEndpoint::anthropic("k", "claude-opus-5"),
                Provider::OpenAi => LlmEndpoint::openai("", "k", "some/model"),
            };
            assert!(
                model_for(&endpoint).is_ok(),
                "{} did not resolve",
                provider.as_str()
            );
        }
    }

    /// A non-default base URL is somebody else's gateway and must not be built
    /// as an OpenRouter client — the two disagree about request extensions.
    #[test]
    fn a_custom_base_url_resolves_to_the_plain_openai_client() {
        let endpoint = LlmEndpoint::openai("https://vllm.internal/v1", "k", "m");
        let handle = model_for(&endpoint).expect("a custom gateway resolves");
        assert_eq!(handle.label(), Some("openai"));
        let router = LlmEndpoint::openai(DEFAULT_OPENAI_BASE_URL, "k", "m");
        assert_eq!(model_for(&router).unwrap().label(), Some("openrouter"));
    }
}
