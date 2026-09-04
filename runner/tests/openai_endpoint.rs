//! The OpenAI-compatible transport, end to end against a local socket.
//!
//! The dialect translation this file used to police is rig's now, so what is
//! left to pin is that the second path is wired at all: that a streamed
//! chat-completions reply comes back as the same `ModelReply` the Anthropic
//! path produces, and that the request goes to the right URL with the right
//! auth. Without it, only one of the two endpoints an operator can pick has any
//! end-to-end coverage.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use pipeline::{AbortFlag, TurnProvider};
use runner::provider::LlmEndpoint;
use runner::turns::TurnPlan;

/// A streamed chat-completions reply: two content deltas, one tool call split
/// across chunks, a finish reason, and a usage-only final chunk — the shape a
/// real endpoint sends.
const SSE_BODY: &str = concat!(
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"looking\"}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" now\"}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",",
    "\"type\":\"function\",\"function\":{\"name\":\"get_source_info\",\"arguments\":\"{\\\"depth\\\"\"}}]}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,",
    "\"function\":{\"arguments\":\":2}\"}}]}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":1234,\"completion_tokens\":21,\"total_tokens\":1255}}\n\n",
    "data: [DONE]\n\n",
);

/// Serve exactly one request, hand the raw bytes back, and reply with `SSE_BODY`.
fn serve_once(listener: TcpListener, sent: mpsc::Sender<String>) {
    let (mut socket, _) = listener.accept().expect("a connection");
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = socket.read(&mut buf).expect("readable");
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&raw);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len: usize = head
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim())
                })
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if body.len() >= len {
                break;
            }
        }
    }
    sent.send(String::from_utf8_lossy(&raw).to_string()).ok();

    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
        SSE_BODY.len(),
        SSE_BODY
    );
    socket.write_all(response.as_bytes()).expect("writable");
    socket.flush().ok();
}

#[tokio::test]
async fn a_streamed_openai_compatible_turn_reads_back_the_same_way() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let addr = listener.local_addr().expect("an address");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve_once(listener, tx));

    // A base URL that is not OpenRouter's, so this exercises the plain
    // OpenAI-compatible client — the one an operator's own gateway gets.
    let endpoint = LlmEndpoint::openai(format!("http://{addr}/v1"), "sk-test", "some/model");
    let turns = TurnPlan::for_endpoint(endpoint)
        .provider()
        .expect("the endpoint resolves to a model");

    let tools = pipeline::tool_definitions(&[serde_json::json!({
        "name": "get_source_info",
        "description": "Summarise the source.",
        "input_schema": {
            "type": "object",
            "properties": {"depth": {"type": "integer"}},
            "required": [],
        },
    })]);

    let reply = turns
        .call_model(
            rig_core::message::Message::user("Analyse the source."),
            Vec::new(),
            &tools,
            "You are an analyst.",
            &AbortFlag::default(),
            &mut |_: &_, _: &_| unreachable!("the scripted call is a known tool"),
        )
        .await
        .expect("the turn completes");

    assert_eq!(reply.text, "looking now");

    // Arguments split across two chunks have to be reassembled, same as on the
    // Anthropic path.
    let calls: Vec<_> = reply
        .turn
        .choice
        .iter()
        .filter_map(|c| match c {
            rig_core::message::AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "expected one tool call");
    assert_eq!(calls[0].function.name, "get_source_info");
    assert_eq!(calls[0].function.arguments, serde_json::json!({"depth": 2}));

    assert_eq!(reply.prompt_tokens, 1234);
    assert_eq!(reply.usage.output_tokens, 21);
    // `some/model` is in no price table, and that must read as unknown rather
    // than as free.
    assert_eq!(reply.cost_usd, None);

    // ── the outgoing request ────────────────────────────────────────────────
    let sent = rx.recv().expect("the server captured a request");
    let (head, body) = sent.split_once("\r\n\r\n").expect("a well-formed request");
    assert!(head.starts_with("POST /v1/chat/completions "), "{head}");
    assert!(
        head.to_lowercase().contains("authorization: bearer sk-test"),
        "{head}"
    );

    let json: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    assert_eq!(json["model"], "some/model");
    assert_eq!(json["stream"], true);
    // This dialect nests the schema under `function.parameters`; sending the
    // catalog's `input_schema` verbatim would offer every tool an empty schema.
    assert_eq!(json["tools"][0]["function"]["name"], "get_source_info");
    assert!(json["tools"][0]["function"]["parameters"]["properties"]["depth"].is_object());
    // The system prompt has to arrive, or every stage runs unprompted.
    assert!(body.contains("You are an analyst."), "{body}");
}
