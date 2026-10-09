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
    /// The Reviewer's model at the selected provider, when it is not the
    /// model above (which the Author runs on). Empty = the same model. One
    /// field for both providers: an id is only meaningful at the endpoint it
    /// was picked for, so switching the provider means picking again.
    #[serde(default)]
    pub reviewer_model: String,
    /// The judges' model at the selected provider (the agents `rule_check`
    /// dispatches, one per judged rule), when it is not the model of the stage
    /// that dispatches them. Empty = the same model.
    #[serde(default)]
    pub judge_model: String,
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
    /// Whether every run is recorded for analysis: a folder per run with a
    /// report, a timeline, per-stage transcripts and the full trace (see
    /// [`crate::analysis`]). Off unless switched on.
    #[serde(default)]
    pub run_analysis: bool,
    /// Where those folders go. Empty = `run-analysis/` in the engine's checkout
    /// (see [`crate::analysis::default_root`]).
    #[serde(default)]
    pub run_analysis_dir: String,
    /// The container engine the verifiers run on: Docker unless switched to
    /// Podman. Read once at start (see [`agent::container_engine::select`]), so
    /// a change takes effect on the next start.
    #[serde(default)]
    pub container_engine: agent::container_engine::ContainerEngine,
    /// Output targets the operator does not use. A switched-off target is not
    /// offered in the app's Output picker and is left out of the readiness
    /// check and its banner, so a machine without, say, the Redacto images is
    /// not reported as unfinished. It is never a way to run a target
    /// unverified: a target that is offered is checked before every run.
    /// Empty (every target on) unless switched; never holds every target.
    #[serde(default)]
    pub disabled_targets: Vec<agent::OutputTarget>,
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
            reviewer_model: String::new(),
            judge_model: String::new(),
            max_review_rounds: DEFAULT_MAX_REVIEW_ROUNDS,
            aem_verify: agent::u2s::AemVerifySettings::default(),
            redacto_verify: agent::u2s::RedactoVerifySettings::default(),
            max_concurrent_requests: default_max_concurrent_requests(),
            agent_instructions: String::new(),
            run_analysis: false,
            run_analysis_dir: String::new(),
            container_engine: agent::container_engine::ContainerEngine::default(),
            disabled_targets: Vec::new(),
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

    /// The endpoint a role with its own model id talks to: the selected
    /// provider's, with `model` instead of the main one. `None` when `model`
    /// is empty or the main model, so the role runs on the run's own.
    pub fn role_endpoint(&self, model: &str) -> Option<LlmEndpoint> {
        let model = model.trim();
        if model.is_empty() || model == self.active_model() {
            return None;
        }
        Some(LlmEndpoint { model: model.to_string(), ..self.llm_endpoint() })
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

    /// Whether `target` is offered: not switched off in the settings.
    pub fn target_enabled(&self, target: agent::OutputTarget) -> bool {
        !self.disabled_targets.contains(&target)
    }

    /// The targets that are offered, in [`agent::OutputTarget::ALL`] order.
    pub fn enabled_targets(&self) -> Vec<agent::OutputTarget> {
        agent::OutputTarget::ALL
            .into_iter()
            .filter(|&target| self.target_enabled(target))
            .collect()
    }

    /// Switch `target` on or off. Switching off the last target that is on is
    /// refused, since a machine that offers no output cannot convert anything;
    /// returns whether the settings changed.
    pub fn set_target_enabled(&mut self, target: agent::OutputTarget, enabled: bool) -> bool {
        if enabled == self.target_enabled(target) {
            return false;
        }
        if enabled {
            self.disabled_targets.retain(|&t| t != target);
        } else {
            if self.enabled_targets().len() <= 1 {
                return false;
            }
            self.disabled_targets.push(target);
        }
        true
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

        // Every target switched off (a hand-edited file) would leave nothing to
        // convert to: offer them all again rather than a dead app.
        self.disabled_targets.sort_by_key(|t| t.as_str());
        self.disabled_targets.dedup();
        if self.enabled_targets().is_empty() {
            self.disabled_targets.clear();
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

    /// A role's own model reuses the selected provider's endpoint and key; an
    /// empty one, or the main model again, means the run's own.
    #[test]
    fn a_role_model_is_the_main_endpoint_with_another_model() {
        let mut settings = AppSettings {
            anthropic_api_key: "k".into(),
            anthropic_model: "claude-opus-5-5".into(),
            ..AppSettings::default()
        };
        assert_eq!(settings.role_endpoint(""), None);
        assert_eq!(settings.role_endpoint(" claude-opus-5-5 "), None);
        let judge = settings.role_endpoint("claude-haiku-5-5").unwrap();
        assert_eq!((judge.model.as_str(), judge.api_key.as_str()), ("claude-haiku-5-5", "k"));
        assert_eq!(judge.base_url, settings.llm_endpoint().base_url);
        settings.llm_provider = Provider::OpenAi;
        settings.openai_model = "anthropic/claude-opus-5.5".into();
        assert_eq!(settings.role_endpoint("anthropic/claude-haiku-5.5").unwrap().provider, Provider::OpenAi);
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

    /// Switching a target off and on round-trips through the saved JSON, and
    /// the last target that is on cannot be switched off.
    #[test]
    fn a_target_can_be_switched_off_but_never_the_last_one() {
        use agent::OutputTarget::{Aem, Redacto};
        let mut settings = AppSettings::default();
        assert_eq!(settings.enabled_targets(), vec![Aem, Redacto]);

        assert!(settings.set_target_enabled(Redacto, false));
        assert!(!settings.target_enabled(Redacto));
        assert_eq!(settings.enabled_targets(), vec![Aem]);
        assert!(!settings.set_target_enabled(Aem, false), "the last target stays on");
        assert!(settings.target_enabled(Aem));

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""disabled_targets":["redacto"]"#), "{json}");
        let loaded: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.enabled_targets(), vec![Aem]);

        assert!(settings.set_target_enabled(Redacto, true));
        assert_eq!(settings.enabled_targets(), vec![Aem, Redacto]);
        assert!(!settings.set_target_enabled(Redacto, true), "already on");
    }

    /// Settings saved before the switch existed offer every target, and a file
    /// that switches every target off is read as switching none off.
    #[test]
    fn old_or_inconsistent_settings_offer_every_target() {
        let old: AppSettings = serde_json::from_str(r#"{"anthropic_api_key":"k"}"#).unwrap();
        assert_eq!(old.enabled_targets(), agent::OutputTarget::ALL.to_vec());

        let mut all_off: AppSettings =
            serde_json::from_str(r#"{"disabled_targets":["aem","redacto","redacto"]}"#).unwrap();
        all_off.normalize();
        assert!(all_off.disabled_targets.is_empty());
        assert_eq!(all_off.enabled_targets(), agent::OutputTarget::ALL.to_vec());
    }
}
