//! Persistent application settings stored in the local SQLite database.
//!
//! Settings are serialized as JSON and stored under the `app` key in the
//! `settings` table of `<config_dir>/blueprint/history.db`.

use serde::{Deserialize, Serialize};

use crate::provider::{DEFAULT_OPENAI_BASE_URL, LlmEndpoint, Provider};

/// Key under which the serialized settings are stored.
const SETTINGS_KEY: &str = "app";

/// Default cap on Reviewer → Author-fix rounds in the conversion pipeline.
pub const DEFAULT_MAX_REVIEW_ROUNDS: usize = 3;

/// Render operator-configured extra instructions as a prompt section, or an
/// empty string when none are set. Appended after the built-in guidance so the
/// hard constraints still take precedence.
pub fn extra_instructions_block(instructions: &str) -> String {
    let trimmed = instructions.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!(
        "\n\n--- ADDITIONAL USER INSTRUCTIONS ---\n\
         The operator configured the following extra instructions. Follow them \
         wherever they do not conflict with the hard constraints above:\n{trimmed}"
    )
}

/// Application settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub always_on_top: bool,
    /// Which API the run talks to. Anthropic unless switched; the other choice
    /// is any OpenAI-compatible endpoint (OpenRouter, a local gateway). The
    /// per-provider key/model/base-URL fields below are kept side by side rather
    /// than shared, so flipping the switch back and forth does not lose either
    /// configuration.
    #[serde(default)]
    pub llm_provider: Provider,
    /// Anthropic API key used for AI features. Stored in the settings file on disk.
    pub anthropic_api_key: String,
    /// Anthropic model used for AI features (e.g. "claude-opus-4-8").
    pub anthropic_model: String,
    /// API root of the OpenAI-compatible endpoint, without a trailing
    /// `/chat/completions`. Blank is normalized to [`DEFAULT_OPENAI_BASE_URL`]
    /// in [`AppSettings::load`].
    #[serde(default)]
    pub openai_base_url: String,
    /// API key sent as `Authorization: Bearer` to that endpoint. Stored on disk.
    #[serde(default)]
    pub openai_api_key: String,
    /// Model id at that endpoint (e.g. "anthropic/claude-opus-4.1" on
    /// OpenRouter). No default: only the operator knows what their endpoint
    /// serves, so an unset model fails the run rather than guessing.
    #[serde(default)]
    pub openai_model: String,
    /// Maximum Reviewer → Author-fix rounds in the conversion pipeline before
    /// finalizing with whatever is built. Missing/0 is normalized to
    /// [`DEFAULT_MAX_REVIEW_ROUNDS`] in [`AppSettings::load`].
    #[serde(default)]
    pub max_review_rounds: usize,
    /// The Docker-hosted AEM the UBS verifier boots for an AEM run. Stored
    /// flat (`aem_verify_*`).
    #[serde(flatten)]
    pub aem_verify: agent::u2s::AemVerifySettings,
    /// The throwaway Postgres the Redacto verifier imports dumps into, stored
    /// flat (`redacto_verify_*`).
    #[serde(flatten)]
    pub redacto_verify: agent::u2s::RedactoVerifySettings,
    /// How many model requests may be in flight at once against one endpoint,
    /// across every conversion running in parallel. `0` means no cap.
    ///
    /// Without one, N tabs mean N times the request rate on a single API key,
    /// and a shared rate limit turns parallel runs into slower serial ones.
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: usize,
    /// Extra operator instructions appended to the autonomous conversion agent's
    /// system prompt. Empty = none.
    #[serde(default)]
    pub agent_instructions: String,
}

/// Requests in flight per endpoint. Three keeps several conversions moving
/// without making a shared rate limit the bottleneck.
fn default_max_concurrent_requests() -> usize {
    3
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            always_on_top: false,
            llm_provider: Provider::default(),
            anthropic_api_key: String::new(),
            anthropic_model: crate::models::DEFAULT_MODEL.to_string(),
            openai_base_url: DEFAULT_OPENAI_BASE_URL.to_string(),
            openai_api_key: String::new(),
            openai_model: String::new(),
            max_review_rounds: DEFAULT_MAX_REVIEW_ROUNDS,
            aem_verify: agent::u2s::AemVerifySettings::default(),
            redacto_verify: agent::u2s::RedactoVerifySettings::default(),
            max_concurrent_requests: default_max_concurrent_requests(),
            agent_instructions: String::new(),
        }
    }
}

impl AppSettings {
    /// The endpoint every AI feature talks to: the selected provider's base URL,
    /// key and model, resolved in one place so no caller pairs the wrong two.
    pub fn llm_endpoint(&self) -> LlmEndpoint {
        match self.llm_provider {
            Provider::Anthropic => {
                LlmEndpoint::anthropic(self.anthropic_api_key.trim(), self.anthropic_model.trim())
            }
            Provider::OpenAi => LlmEndpoint::openai(
                &self.openai_base_url,
                self.openai_api_key.trim(),
                self.openai_model.trim(),
            ),
        }
    }

    /// The API key of the selected provider.
    pub fn active_api_key(&self) -> &str {
        match self.llm_provider {
            Provider::Anthropic => self.anthropic_api_key.trim(),
            Provider::OpenAi => self.openai_api_key.trim(),
        }
    }

    /// The model identifier of the selected provider.
    pub fn active_model(&self) -> &str {
        match self.llm_provider {
            Provider::Anthropic => self.anthropic_model.trim(),
            Provider::OpenAi => self.openai_model.trim(),
        }
    }

    /// Coerce missing/zero values to their real defaults. Guards against configs
    /// saved before these fields had sensible defaults (where a `0` would
    /// otherwise show in the UI and read as "off").
    fn normalize(&mut self) {
        fn or_default(value: &mut usize, default: usize) {
            if *value == 0 {
                *value = default;
            }
        }

        let d = Self::default();
        or_default(&mut self.max_review_rounds, d.max_review_rounds);
        if self.aem_verify.container_port == 0 {
            self.aem_verify.container_port = d.aem_verify.container_port;
        }

        // Settings saved before the provider switch existed carry no base URL.
        if self.openai_base_url.trim().is_empty() {
            self.openai_base_url = d.openai_base_url;
        }
    }

    /// Load settings from the database, falling back to defaults on any error.
    pub fn load() -> Self {
        if let Some(json) = agent::db::get_setting(SETTINGS_KEY)
            && let Ok(mut settings) = serde_json::from_str::<AppSettings>(&json)
        {
            settings.normalize();
            return settings;
        }

        Self::default()
    }

    /// Save settings to the database.
    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            agent::db::set_setting(SETTINGS_KEY, &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> AppSettings {
        AppSettings {
            anthropic_api_key: "sk-ant-key".to_string(),
            anthropic_model: "claude-opus-5".to_string(),
            openai_base_url: "https://openrouter.ai/api/v1".to_string(),
            openai_api_key: "sk-or-key".to_string(),
            openai_model: "anthropic/claude-opus-4.1".to_string(),
            ..AppSettings::default()
        }
    }

    /// The switch has to reroute the key *and* the model together. Pairing an
    /// Anthropic key with an OpenRouter model id (or the reverse) is the failure
    /// this one accessor exists to make impossible.
    #[test]
    fn the_provider_switch_selects_a_whole_endpoint() {
        let mut settings = configured();

        settings.llm_provider = Provider::Anthropic;
        let anthropic = settings.llm_endpoint();
        assert_eq!(anthropic.provider, Provider::Anthropic);
        assert_eq!(anthropic.api_key, "sk-ant-key");
        assert_eq!(anthropic.model, "claude-opus-5");
        assert_eq!(settings.active_model(), "claude-opus-5");

        settings.llm_provider = Provider::OpenAi;
        let openai = settings.llm_endpoint();
        assert_eq!(openai.provider, Provider::OpenAi);
        assert_eq!(openai.api_key, "sk-or-key");
        assert_eq!(openai.model, "anthropic/claude-opus-4.1");
        assert_eq!(
            openai.url("/chat/completions"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(settings.active_model(), "anthropic/claude-opus-4.1");
    }

    /// Settings written while the app still uploaded to a configured AEM and
    /// drove a Playwright browser carry fields that no longer exist; they must
    /// load all the same, with the verifier defaults filled in.
    #[test]
    fn settings_saved_with_the_retired_aem_connection_still_load() {
        let json = r#"{"anthropic_api_key":"k","aem_host":"http://localhost:4502","aem_username":"admin","aem_password":"admin","browser_enabled":true,"browser_npx_path":""}"#;
        let settings: AppSettings = serde_json::from_str(json).expect("old settings load");
        assert_eq!(settings.anthropic_api_key, "k");
        assert_eq!(settings.aem_verify.container_port, 8080);
        assert_eq!(settings.aem_verify.data_volume, "u2s-aem-ubs-data");
        assert!(settings.aem_verify.image.is_empty());
    }

    /// Settings written before the switch existed carry neither field, and must
    /// still load onto the Anthropic path with a usable OpenAI-compatible
    /// default sitting behind it.
    #[test]
    fn settings_saved_before_the_switch_keep_working() {
        let json =
            r#"{"always_on_top":false,"anthropic_api_key":"k","anthropic_model":"claude-opus-5"}"#;
        let mut settings: AppSettings = serde_json::from_str(json).expect("old settings load");
        settings.normalize();
        assert_eq!(settings.llm_provider, Provider::Anthropic);
        assert_eq!(settings.openai_base_url, DEFAULT_OPENAI_BASE_URL);
        assert_eq!(settings.llm_endpoint().api_key, "k");
    }
}
