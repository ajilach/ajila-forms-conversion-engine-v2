//! Prompt-cache breakpoints on the conversation for Claude through OpenRouter.
//!
//! Anthropic caches only up to an explicit `cache_control` breakpoint, and
//! OpenRouter passes those through from the request's message parts. rig's
//! OpenRouter path places one, on the system prompt
//! (`with_prompt_caching`), and none on the conversation, so every turn of a
//! stage re-sent its whole history at the full input price: the AAOV Author
//! through OpenRouter read 5 M tokens from the cache and paid 17.5 M fresh,
//! where the same kind of stage on Anthropic's own API pays a few hundred.
//!
//! [`CacheTail`] is the HTTP client the OpenRouter model is built over. For a
//! request to an `anthropic/…` model it marks the last two messages that can
//! carry a breakpoint, so the next turn reads everything up to the previous
//! one from the cache: Anthropic matches a breakpoint against earlier writes,
//! and the second mark keeps a turn that appended many blocks within reach.
//! With the system prompt's that is three of the four breakpoints Anthropic
//! allows. Other models are left alone: OpenAI, Google and DeepSeek cache a
//! repeated prefix on their own, and a field they do not know is a risk for
//! nothing.

use bytes::Bytes;
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;
use serde_json::{Value, json};

/// How many messages at the end of the conversation get a breakpoint.
const TAIL_BREAKPOINTS: usize = 2;

/// An HTTP client that adds conversation breakpoints to Claude requests on
/// their way to OpenRouter, then sends them through `inner`.
#[derive(Clone, Debug, Default)]
pub struct CacheTail {
    inner: reqwest::Client,
}

impl CacheTail {
    pub fn new(inner: reqwest::Client) -> Self {
        Self { inner }
    }
}

/// `body` with the conversation breakpoints added, when it is a chat request
/// to a Claude model; otherwise unchanged.
pub fn with_tail_breakpoints(body: Bytes) -> Bytes {
    let Ok(mut request) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let is_claude = request
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with("anthropic/"));
    if !is_claude || !mark_tail(&mut request) {
        return body;
    }
    serde_json::to_vec(&request).map(Bytes::from).unwrap_or(body)
}

/// Marks the last [`TAIL_BREAKPOINTS`] non-system messages that hold content
/// a breakpoint can sit on. True when it marked any.
fn mark_tail(request: &mut Value) -> bool {
    let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut marked = 0;
    for message in messages.iter_mut().rev() {
        if marked == TAIL_BREAKPOINTS {
            break;
        }
        if message.get("role").and_then(Value::as_str) == Some("system") {
            continue;
        }
        if mark_message(message) {
            marked += 1;
        }
    }
    marked > 0
}

/// Puts a breakpoint on the message's last text part, turning plain string
/// content into one text part first. False when it has no text to mark (an
/// assistant turn that is only tool calls, an image-only message).
fn mark_message(message: &mut Value) -> bool {
    let Some(content) = message.get_mut("content") else {
        return false;
    };
    match content {
        Value::String(text) if !text.is_empty() => {
            *content = json!([{ "type": "text", "text": text.clone(), "cache_control": { "type": "ephemeral" } }]);
            true
        }
        Value::Array(parts) => match parts
            .iter_mut()
            .rev()
            .find(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        {
            Some(Value::Object(part)) => {
                part.insert("cache_control".into(), json!({ "type": "ephemeral" }));
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// The request with its body rewritten; the length header, if any, goes, as
/// the body's own length is what is sent.
fn rewritten<T: Into<Bytes>>(req: Request<T>) -> Request<Bytes> {
    let (mut parts, body) = req.into_parts();
    parts.headers.remove(http::header::CONTENT_LENGTH);
    Request::from_parts(parts, with_tail_breakpoints(body.into()))
}

impl HttpClientExt for CacheTail {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.inner.send(rewritten(req))
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.inner.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        self.inner.send_streaming(rewritten(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(body: Value) -> Value {
        serde_json::from_slice(&with_tail_breakpoints(Bytes::from(serde_json::to_vec(&body).unwrap()))).unwrap()
    }

    fn marked(message: &Value) -> bool {
        message["content"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| p["cache_control"]["type"] == "ephemeral"))
    }

    /// A Claude conversation ending in tool results: the last two messages
    /// with text are marked, the tool-call-only assistant turn and the system
    /// prompt are skipped, and nothing earlier is touched.
    #[test]
    fn a_claude_request_gets_breakpoints_on_its_last_two_text_messages() {
        let body = rewrite(json!({
            "model": "anthropic/claude-opus-5.5",
            "messages": [
                { "role": "system", "content": [{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }] },
                { "role": "user", "content": "convert the form" },
                { "role": "assistant", "content": "reading it" },
                { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
                { "role": "tool", "tool_call_id": "c1", "content": "outline…" },
                { "role": "user", "content": [{ "type": "text", "text": "note" }, { "type": "image_url", "image_url": { "url": "data:" } }] }
            ]
        }));
        let messages = body["messages"].as_array().unwrap();
        let flags: Vec<bool> = messages.iter().map(marked).collect();
        assert_eq!(flags, [true, false, false, false, true, true], "{body:#}");
        // The image part stays as it was; the text part before it is marked.
        assert!(messages[5]["content"][1].get("cache_control").is_none());
        assert_eq!(messages[4]["content"][0]["text"], "outline…");
    }

    /// Other vendors cache a repeated prefix themselves: their requests go
    /// out byte for byte as rig built them.
    #[test]
    fn another_vendors_request_is_left_alone() {
        let body = json!({ "model": "openai/gpt-6.1-sol", "messages": [{ "role": "user", "content": "hi" }] });
        let bytes = Bytes::from(serde_json::to_vec(&body).unwrap());
        assert_eq!(with_tail_breakpoints(bytes.clone()), bytes);
    }

    /// What goes over the wire is the rewritten body, with a length that
    /// matches it: the client is exercised against a local socket.
    #[tokio::test]
    async fn the_rewritten_body_is_what_is_sent() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then as many body bytes as Content-Length says.
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                received.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length: usize = text[..end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                        .unwrap();
                    if received.len() >= end + 4 + length {
                        let body = received[end + 4..end + 4 + length].to_vec();
                        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").await.unwrap();
                        return body;
                    }
                }
                if n == 0 {
                    panic!("connection closed early");
                }
            }
        });
        let body = serde_json::to_vec(&json!({
            "model": "anthropic/claude-haiku-5.5",
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .unwrap();
        let request = Request::post(format!("http://{addr}/api/v1/chat/completions"))
            .header(http::header::CONTENT_LENGTH, body.len())
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Bytes::from(body))
            .unwrap();
        let response: Response<LazyBody<Bytes>> = CacheTail::default().send(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let sent: Value = serde_json::from_slice(&server.await.unwrap()).unwrap();
        assert_eq!(sent["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral", "{sent}");
    }

    /// A body that is not JSON (or has no messages) passes through.
    #[test]
    fn a_body_without_messages_passes_through() {
        let bytes = Bytes::from_static(b"not json");
        assert_eq!(with_tail_breakpoints(bytes.clone()), bytes);
        let listing = Bytes::from(serde_json::to_vec(&json!({ "model": "anthropic/claude-haiku-5.5" })).unwrap());
        assert_eq!(with_tail_breakpoints(listing.clone()), listing);
    }
}
