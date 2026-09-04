//! Which API a run talks to, and where.
//!
//! Two transports serve the same [`pipeline::TurnProvider`]: the Anthropic
//! Messages API ([`crate::llm`]) and any OpenAI-compatible `/chat/completions`
//! endpoint ([`crate::openai`]) — OpenRouter, a local vLLM/Ollama gateway, or
//! OpenAI itself. The rest of the app never branches on this: it resolves one
//! [`LlmEndpoint`] from the settings and hands it to [`crate::TurnPlan`].

use serde::{Deserialize, Serialize};

/// The API dialect an endpoint speaks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// The Anthropic Messages API (`/v1/messages`), streamed, with prompt caching.
    #[default]
    Anthropic,
    /// Any OpenAI-compatible chat-completions endpoint, streamed. No prompt
    /// caching: `cache_control` is an Anthropic extension that a strict
    /// OpenAI-compatible server rejects, so that path sends the prompt plain.
    OpenAi,
}

impl Provider {
    /// Every provider, in the order the settings picker offers them.
    pub const ALL: &'static [Self] = &[Self::Anthropic, Self::OpenAi];

    /// The value used on the command line and in the settings file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }

    /// What the settings picker shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI-compatible (OpenRouter, …)",
        }
    }

    /// Parse a provider name, accepting the aliases an operator is likely to
    /// type. `None` for anything else — callers turn that into a hard error
    /// listing [`Provider::ALL`] rather than silently falling back.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Self::Anthropic),
            "openai" | "openai-compatible" | "openrouter" | "generic" => Some(Self::OpenAi),
            _ => None,
        }
    }
}

/// The Anthropic API root. Not configurable: the Anthropic path exists to talk
/// to Anthropic, and anything else is what [`Provider::OpenAi`] is for.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Where the OpenAI-compatible path points when the operator has not said.
/// OpenRouter, because that is the endpoint this switch was added for.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// A resolved endpoint: which dialect, where, with which credential and model.
///
/// Resolved once from the settings (see [`crate::AppSettings::llm_endpoint`])
/// and passed around whole, so no consumer has to re-decide which key belongs
/// to which base URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmEndpoint {
    pub provider: Provider,
    /// API root without a trailing slash — `/messages` or `/chat/completions`
    /// is appended by the transport.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl LlmEndpoint {
    /// An Anthropic endpoint for `model`.
    pub fn anthropic(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: Provider::Anthropic,
            base_url: ANTHROPIC_BASE_URL.to_string(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// An OpenAI-compatible endpoint. A blank `base_url` resolves to
    /// [`DEFAULT_OPENAI_BASE_URL`]; a trailing slash is trimmed so the
    /// transports can append their path unconditionally.
    pub fn openai(
        base_url: impl AsRef<str>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let base = base_url.as_ref().trim().trim_end_matches('/');
        Self {
            provider: Provider::OpenAi,
            base_url: if base.is_empty() {
                DEFAULT_OPENAI_BASE_URL.to_string()
            } else {
                base.to_string()
            },
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// The full URL of `path` (e.g. `"/messages"`) on this endpoint.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// The reason this endpoint cannot be used, or `Ok(())`. Checked before the
    /// first turn so a missing key or model fails the run with a message that
    /// says what to set, instead of a `401` from the provider mid-stream.
    pub fn check(&self) -> Result<(), String> {
        if self.api_key.trim().is_empty() {
            return Err(match self.provider {
                Provider::Anthropic => {
                    "Anthropic API key is not configured. Open Settings and paste your API key."
                        .to_string()
                }
                Provider::OpenAi => format!(
                    "No API key for the OpenAI-compatible endpoint at {}. \
                     Open Settings and paste one.",
                    self.base_url
                ),
            });
        }
        if self.model.trim().is_empty() {
            return Err(format!(
                "No model configured for the endpoint at {}. Open Settings and pick one.",
                self.base_url
            ));
        }
        Ok(())
    }

    /// The model ids the endpoint offers, sorted. Used by the settings picker.
    ///
    /// Anthropic's listing reports ids and display names only, so the limits
    /// still come from [`crate::models::KNOWN_MODELS`]; OpenRouter reports the
    /// context window and output ceiling too. Either way the ids are the live
    /// ones, so a model released after this build still shows up.
    pub async fn list_models(&self) -> Result<Vec<String>, String> {
        let models = crate::client::list_models(self).await?;
        let mut ids: Vec<String> = models.into_iter().map(|m| m.id).collect();
        ids.sort();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_openai_base_url_resolves_to_the_default() {
        let e = LlmEndpoint::openai("  ", "k", "m");
        assert_eq!(e.base_url, DEFAULT_OPENAI_BASE_URL);
    }

    #[test]
    fn a_trailing_slash_does_not_double_up_in_urls() {
        let e = LlmEndpoint::openai("https://example.test/v1/", "k", "m");
        assert_eq!(
            e.url("/chat/completions"),
            "https://example.test/v1/chat/completions"
        );
    }

    /// A missing key or model has to fail before the run starts, and the message
    /// has to name what is missing.
    #[test]
    fn check_rejects_a_missing_key_and_a_missing_model() {
        assert!(LlmEndpoint::openai("", "", "m").check().is_err());
        let err = LlmEndpoint::openai("", "k", " ").check().unwrap_err();
        assert!(err.contains("model"), "{err}");
        assert!(LlmEndpoint::anthropic("k", "m").check().is_ok());
    }

    #[test]
    fn provider_names_round_trip() {
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.as_str()), Some(*p));
        }
        assert_eq!(Provider::parse("openrouter"), Some(Provider::OpenAi));
        assert_eq!(Provider::parse("nope"), None);
    }
}
