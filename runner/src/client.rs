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
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
};
use rig_core::message::{Message, ToolResultContent, UserContent};
use rig_core::model::Model;
use rig_core::streaming::StreamingCompletionResponse;
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

/// How long a streamed turn may go without receiving any bytes before the
/// request is failed.
///
/// The stream emits deltas continuously, so a long silence means the connection
/// is dead — typically because the machine slept or the network dropped
/// mid-run. Failing fast turns that into a retryable error instead of a run
/// that hangs forever on a socket nobody is talking on: the abort flag is only
/// polled between chunks, so a stalled stream is unstoppable without this.
const STREAM_READ_TIMEOUT_SECS: u64 = 300;

/// The HTTP client every provider is built over: one connection pool for the
/// process, with an inactivity timeout on the response body.
fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .read_timeout(std::time::Duration::from_secs(STREAM_READ_TIMEOUT_SECS))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// Whether this base URL is OpenRouter.
///
/// Matched on host rather than on the exact default string: an operator may
/// point at a regional URL or a proxy in front of OpenRouter, and getting this
/// wrong silently downgrades them to the plain OpenAI client — no caching, no
/// usage details, no provider routing, with nothing to see in the UI.
fn is_openrouter(base_url: &str) -> bool {
    let after_scheme = base_url.split_once("://").map_or(base_url, |(_, rest)| rest);
    after_scheme
        .split('/')
        .next()
        .and_then(|authority| authority.rsplit('@').next())
        .map(|host| {
            let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
            host == "openrouter.ai" || host.ends_with(".openrouter.ai")
        })
        .unwrap_or(false)
}

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
                .http_client(http_client())
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
        // reports usage details per response and takes explicit cache
        // breakpoints. Any other host is somebody's OpenAI-compatible gateway.
        Provider::OpenAi if is_openrouter(&endpoint.base_url) => {
            let client = openrouter::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .http_client(http_client())
                .build()
                .map_err(|e| format!("OpenRouter client: {e}"))?;
            // Opt-in, unlike Anthropic's: rig only writes a `cache_control`
            // breakpoint on the system prompt when asked. Without it this path
            // pays full input price every turn, which is what it did before the
            // migration and is worth several francs on a long run.
            Ok(ModelHandle::named(
                "openrouter",
                ImagesAfterToolResults(
                    client
                        .completion_model(&endpoint.model)
                        .with_prompt_caching(),
                ),
            ))
        }
        Provider::OpenAi => {
            // `.completions_api()` matters: rig's default OpenAI model speaks
            // the Responses API, which is OpenAI's own and which a vLLM,
            // Ollama or LiteLLM gateway does not implement. This switch exists
            // for those gateways, and `/chat/completions` is what they serve.
            let client = openai::Client::builder()
                .api_key(endpoint.api_key.clone())
                .base_url(endpoint.base_url.clone())
                .http_client(http_client())
                .build()
                .map_err(|e| format!("OpenAI-compatible client at {}: {e}", endpoint.base_url))?
                .completions_api();
            Ok(ModelHandle::named(
                "openai",
                ImagesAfterToolResults(client.completion_model(&endpoint.model)),
            ))
        }
    }
}

/// Tells the model where a tool result's images went, in the result itself.
const IMAGES_FOLLOW_NOTE: &str = "(image output follows in the next message)";

/// A chat-completions model whose requests carry no image inside a tool result.
///
/// That dialect only lets a `tool` message hold text, and rig refuses such a
/// request outright rather than translating it, so a stage that renders a page
/// would stop at its first look. The images move to the end of the same user
/// message instead, which rig sends as the user message right after the tool
/// messages: what the hand-written transport did before the rig migration.
/// Anthropic takes images in a tool result natively and is not wrapped.
struct ImagesAfterToolResults<M>(M);

impl<M: CompletionModel> CompletionModel for ImagesAfterToolResults<M> {
    async fn completion(
        &self,
        mut request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        request.chat_history = images_after_tool_results(request.chat_history);
        self.0.completion(request).await
    }

    async fn stream(
        &self,
        mut request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        request.chat_history = images_after_tool_results(request.chat_history);
        self.0.stream(request).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.0.capabilities()
    }
}

/// Move every image out of each user message's tool results to the end of that
/// message, leaving [`IMAGES_FOLLOW_NOTE`] in each result that lost one.
///
/// Order is kept: the images follow in the order their results came, and each
/// result keeps its text in place, so a tool message is never empty.
fn images_after_tool_results(history: Vec<Message>) -> Vec<Message> {
    history
        .into_iter()
        .map(|message| match message {
            Message::User { content } => Message::User {
                content: user_content_with_images_last(content),
            },
            other => other,
        })
        .collect()
}

fn user_content_with_images_last(content: Vec<UserContent>) -> Vec<UserContent> {
    let mut moved = Vec::new();
    let mut kept: Vec<UserContent> = content
        .into_iter()
        .map(|item| match item {
            UserContent::ToolResult(mut result) => {
                let before = moved.len();
                let mut remaining = Vec::with_capacity(result.content.len());
                for part in result.content {
                    match part {
                        ToolResultContent::Image(image) => moved.push(UserContent::Image(image)),
                        other => remaining.push(other),
                    }
                }
                if moved.len() > before {
                    remaining.push(ToolResultContent::text(IMAGES_FOLLOW_NOTE));
                }
                result.content = remaining;
                UserContent::ToolResult(result)
            }
            other => other,
        })
        .collect();
    kept.extend(moved);
    kept
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
                .http_client(http_client())
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
                .http_client(http_client())
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
                .http_client(http_client())
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

    /// `ModelHandle` snapshots capabilities when it erases the model, so a
    /// wrapper that did not forward them would quietly drop what the OpenAI
    /// model declares (structured output composing with tools).
    #[test]
    fn the_image_wrapper_keeps_the_models_capabilities() {
        let endpoint = LlmEndpoint::openai("https://vllm.internal/v1", "k", "m");
        let bare = openai::Client::builder()
            .api_key("k")
            .base_url("https://vllm.internal/v1")
            .build()
            .expect("a client")
            .completions_api()
            .completion_model("m")
            .capabilities();
        assert_ne!(bare, ProviderCapabilities::default());
        assert_eq!(model_for(&endpoint).expect("resolves").capabilities(), bare);
    }

    fn image(data: &str) -> ToolResultContent {
        ToolResultContent::image_base64(data, None, None)
    }

    fn result(call: &str, content: Vec<ToolResultContent>) -> UserContent {
        UserContent::tool_result_from_wire(call, "pdf_render_page", content)
    }

    /// Each result keeps its text and gains the note; the images follow all
    /// results in the order they came, and a text-only result is untouched.
    #[test]
    fn tool_result_images_move_behind_the_results_in_order() {
        let history = vec![Message::User {
            content: vec![
                result("a", vec![ToolResultContent::text("page 1"), image("one")]),
                result("b", vec![ToolResultContent::text("plain")]),
                result("c", vec![image("two"), image("three")]),
            ],
        }];

        let shaped = images_after_tool_results(history);
        let [Message::User { content }] = shaped.as_slice() else {
            panic!("one user message stays one user message");
        };
        let texts = |item: &UserContent| match item {
            UserContent::ToolResult(r) => r
                .content
                .iter()
                .map(|p| match p {
                    ToolResultContent::Text(t) => t.text.clone(),
                    other => panic!("an image stayed in a tool result: {other:?}"),
                })
                .collect::<Vec<_>>(),
            other => panic!("expected a tool result, got {other:?}"),
        };
        assert_eq!(texts(&content[0]), ["page 1", IMAGES_FOLLOW_NOTE]);
        assert_eq!(texts(&content[1]), ["plain"]);
        assert_eq!(texts(&content[2]), [IMAGES_FOLLOW_NOTE]);

        let moved: Vec<_> = content[3..]
            .iter()
            .map(|item| match item {
                UserContent::Image(image) => format!("{:?}", image.data),
                other => panic!("expected an image, got {other:?}"),
            })
            .collect();
        assert_eq!(moved.len(), 3);
        for (got, want) in moved.iter().zip(["one", "two", "three"]) {
            assert!(got.contains(want), "{got} should be {want}");
        }
    }

    /// Somebody else's gateway must not be built as an OpenRouter client — the
    /// two disagree about request extensions — and OpenRouter must not be built
    /// as a plain one, which would cost it caching and usage details.
    #[test]
    fn the_client_follows_the_host_not_the_exact_url() {
        let plain = LlmEndpoint::openai("https://vllm.internal/v1", "k", "m");
        assert_eq!(model_for(&plain).expect("resolves").label(), Some("openai"));

        for url in [
            DEFAULT_OPENAI_BASE_URL,
            "https://openrouter.ai/api/v1/",
            "https://OpenRouter.ai/api/v1",
            "https://eu.openrouter.ai/api/v1",
        ] {
            let endpoint = LlmEndpoint::openai(url, "k", "m");
            assert_eq!(
                model_for(&endpoint).expect("resolves").label(),
                Some("openrouter"),
                "{url} should reach the OpenRouter client"
            );
        }

        // A host that merely mentions it is not it.
        assert!(!is_openrouter("https://openrouter.ai.evil.test/v1"));
        assert!(!is_openrouter("https://my-openrouter.internal/v1"));
    }
}
