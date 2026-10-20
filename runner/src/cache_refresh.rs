//! Keeping a stage's prompt cache warm while it waits on a slow tool.
//!
//! Anthropic's prompt cache of the conversation tail lives five minutes after
//! its last use. A stage that waits longer on one tool call — an AEM boot, a
//! verifier call that runs long — comes back to a cold cache
//! and pays to write its whole prefix again (USD 1.13 for one turn of a
//! 265,000-token Author, against USD 0.08 for a warm one). A one-hour TTL on
//! every write would avoid that, but costs 2x instead of 1.25x on every write
//! of every turn, which is several times what the misses cost.
//!
//! Instead, [`Recording`] remembers the last request each stage sent (by its
//! system prompt), and [`refresh`] sends it again with a one-token output cap.
//! The provider reads the cached prefix, which resets its five minutes, for a
//! tenth of the input price. The pipeline calls it every few minutes while a
//! tool call is running (see `pipeline::hooks`), and never otherwise.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pipeline::memory::CacheRefresh;
use rig_agent::agent::model::ModelHandle;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
};
use rig_core::message::Message;
use rig_core::streaming::StreamingCompletionResponse;

/// How many stages' last requests are kept: the Author, the Reviewer and a
/// few judges at once. Older ones are dropped first.
const KEPT: usize = 8;

/// The last request sent under each system prompt.
#[derive(Default)]
pub struct RequestLog {
    last: Mutex<VecDeque<(String, CompletionRequest)>>,
}

/// The system prompt a request carries: its legacy `preamble`, or the
/// leading system message rig's request builder moves the preamble into.
fn system_of(request: &CompletionRequest) -> Option<String> {
    if let Some(preamble) = &request.preamble {
        return Some(preamble.clone());
    }
    let system: Vec<&str> = request
        .chat_history
        .iter()
        .map_while(|message| match message {
            Message::System { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    (!system.is_empty()).then(|| system.join("\n"))
}

impl RequestLog {
    fn record(&self, request: &CompletionRequest) {
        let Some(system) = system_of(request) else {
            return;
        };
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        last.retain(|(s, _)| *s != system);
        last.push_back((system, request.clone()));
        while last.len() > KEPT {
            last.pop_front();
        }
    }

    /// The latest request whose system prompt holds `system`: the stage's
    /// own prompt, which the agent may have extended with what it adds.
    fn last_for(&self, system: &str) -> Option<CompletionRequest> {
        let last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        last.iter().rev().find(|(s, _)| s.contains(system)).map(|(_, r)| r.clone())
    }
}

/// A model that records every request it sends in a [`RequestLog`].
struct Recording {
    inner: ModelHandle,
    log: Arc<RequestLog>,
}

impl CompletionModel for Recording {
    async fn completion(&self, request: CompletionRequest) -> Result<CompletionResponse, CompletionError> {
        self.log.record(&request);
        self.inner.completion(request).await
    }

    async fn stream(&self, request: CompletionRequest) -> Result<StreamingCompletionResponse, CompletionError> {
        self.log.record(&request);
        self.inner.stream(request).await
    }

    /// Forwarded, not defaulted — see `ratelimit::RateLimited`.
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}

/// `model`, recording its requests in `log`.
pub fn record(model: ModelHandle, log: Arc<RequestLog>) -> ModelHandle {
    let label = model.label().unwrap_or("model").to_string();
    ModelHandle::named(label, Recording { inner: model, log })
}

/// The last request sent under `system`, sent again through `model` (the
/// model *under* the recording, so a refresh is not itself recorded) with a
/// one-token output cap. `None` when nothing was sent under `system` yet.
pub fn refresh(log: &RequestLog, model: &ModelHandle, system: &str) -> Option<CacheRefresh> {
    let request = ping(log.last_for(system)?);
    let model = model.clone();
    Some(Box::pin(async move {
        model.completion(request).await.map(|response| response.usage).map_err(|e| e.to_string())
    }))
}

/// `request` as a cache refresh: everything that makes up the cached prefix
/// unchanged, and a one-token output.
fn ping(mut request: CompletionRequest) -> CompletionRequest {
    request.max_tokens = Some(1);
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(system: Option<&str>, text: &str) -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: system.map(str::to_string),
            chat_history: vec![Message::user(text)],
            documents: vec![],
            tools: vec![],
            temperature: None,
            max_tokens: Some(128_000),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        }
    }

    #[test]
    fn the_last_request_of_each_stage_is_kept() {
        let log = RequestLog::default();
        log.record(&request(Some("author"), "one"));
        log.record(&request(Some("judge"), "a"));
        log.record(&request(Some("author"), "two"));
        log.record(&request(None, "no system prompt"));
        let author = log.last_for("author").unwrap();
        assert_eq!(author.chat_history, vec![Message::user("two")]);
        assert!(log.last_for("judge").is_some());
        assert!(log.last_for("reviewer").is_none());
    }

    /// rig's request builder moves the preamble into a leading system
    /// message; that is where the stage's system prompt is found then.
    #[test]
    fn the_system_prompt_is_found_in_a_leading_system_message() {
        let log = RequestLog::default();
        let mut sent = request(None, "turn");
        sent.chat_history.insert(0, Message::system("the author's system prompt\n\nmore context"));
        log.record(&sent);
        assert!(log.last_for("the author's system prompt").is_some());
        assert!(log.last_for("the reviewer's").is_none());
    }

    #[test]
    fn old_stages_are_dropped_first() {
        let log = RequestLog::default();
        for n in 0..=KEPT {
            log.record(&request(Some(&format!("judge {n}")), "x"));
        }
        assert!(log.last_for("judge 0").is_none());
        assert!(log.last_for(&format!("judge {KEPT}")).is_some());
    }

    #[test]
    fn a_refresh_keeps_the_prefix_and_caps_the_output() {
        let original = request(Some("author"), "history");
        let refresh = ping(original.clone());
        assert_eq!(refresh.max_tokens, Some(1));
        assert_eq!(refresh.preamble, original.preamble);
        assert_eq!(refresh.chat_history, original.chat_history);
        assert_eq!(refresh.tools.len(), original.tools.len());
    }
}
