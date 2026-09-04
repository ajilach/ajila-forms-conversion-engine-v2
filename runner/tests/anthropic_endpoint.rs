//! The Anthropic transport, end to end against a local socket.
//!
//! There was never a test here: the hand-rolled client's SSE parsing and its
//! cache-breakpoint request body were only ever exercised against the live API.
//! rig owns the wire now, so what this pins is the part that is still ours —
//! that a streamed turn comes back assembled, that usage reaches the reply, and
//! that the request carries the caching configuration and the tool schemas.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use pipeline::{AbortFlag, TurnProvider};
use runner::provider::LlmEndpoint;
use runner::turns::TurnPlan;

/// A Messages stream: some text, then a tool call whose arguments arrive split
/// across two deltas, then usage on `message_delta`.
const SSE_BODY: &str = concat!(
    "event: message_start\n",
    r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-opus-5","stop_reason":null,"usage":{"input_tokens":1200,"output_tokens":1,"cache_read_input_tokens":800,"cache_creation_input_tokens":64}}}"#,
    "\n\n",
    "event: content_block_start\n",
    r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Reading "}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"the source."}}"#,
    "\n\n",
    "event: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":0}"#,
    "\n\n",
    "event: content_block_start\n",
    r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_xfa","input":{}}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"state\""}}"#,
    "\n\n",
    "event: content_block_delta\n",
    r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":":\"DE\"}"}}"#,
    "\n\n",
    "event: content_block_stop\n",
    r#"data: {"type":"content_block_stop","index":1}"#,
    "\n\n",
    "event: message_delta\n",
    // The cache buckets are cumulative totals on `message_delta` and that is
    // the frame they are read from, so a fixture that reports them only on
    // `message_start` would show a run reading nothing from the cache.
    r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":57,"cache_read_input_tokens":800,"cache_creation_input_tokens":64}}"#,
    "\n\n",
    "event: message_stop\n",
    r#"data: {"type":"message_stop"}"#,
    "\n\n",
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
        // Stop once the whole body has arrived, rather than waiting for a close
        // the client will not perform until it has our response.
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
async fn a_streamed_anthropic_turn_is_assembled_and_billed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let addr = listener.local_addr().expect("an address");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve_once(listener, tx));

    let endpoint = LlmEndpoint {
        provider: runner::Provider::Anthropic,
        base_url: format!("http://{addr}"),
        api_key: "sk-test".into(),
        model: "claude-opus-5".into(),
    };
    let turns = TurnPlan::for_endpoint(endpoint)
        .provider()
        .expect("the endpoint resolves to a model");

    let tools = pipeline::tool_definitions(&[serde_json::json!({
        "name": "get_xfa",
        "description": "Read the XFA for a state.",
        "input_schema": {
            "type": "object",
            "properties": {"state": {"type": "string"}},
            "required": ["state"],
        },
    })]);

    let reply = turns
        .call_model(
            rig_core::message::Message::user("Analyse the source."),
            Vec::new(),
            &tools,
            "You are an analyst.",
            &AbortFlag::default(),
        )
        .await
        .expect("the turn completes");

    // The text arrived in two deltas and must be joined.
    assert_eq!(reply.text, "Reading the source.");

    // The tool call's arguments were split across two deltas; they have to be
    // reassembled into one object, or the tool is invoked with nothing.
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
    assert_eq!(calls[0].function.name, "get_xfa");
    assert_eq!(
        calls[0].function.arguments,
        serde_json::json!({"state": "DE"})
    );

    // Usage has to reach the reply: without it the context gauge reads zero and
    // the spend report says a run was free.
    assert_eq!(reply.usage.output_tokens, 57);
    assert_eq!(reply.usage.cached_input_tokens, 800);
    assert_eq!(reply.usage.cache_creation_input_tokens, 64);
    // Occupancy counts the cached buckets too: caching lowers the cost, not how
    // full the window is.
    assert_eq!(reply.prompt_tokens, 1200 + 800 + 64);

    // Opus is in the price table, so the call must carry a figure.
    let cost = reply.cost_usd.expect("a priced model reports a cost");
    assert!(cost > 0.0, "got {cost}");

    // ── the outgoing request ────────────────────────────────────────────────
    let sent = rx.recv().expect("the server captured a request");
    let (head, body) = sent.split_once("\r\n\r\n").expect("a well-formed request");
    let head_lower = head.to_lowercase();
    assert!(head.starts_with("POST /v1/messages "), "{head}");
    assert!(head_lower.contains("x-api-key: sk-test"), "{head}");
    assert!(head_lower.contains("anthropic-version:"), "{head}");

    let json: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    assert_eq!(json["model"], "claude-opus-5");
    assert_eq!(json["stream"], true);
    // The tool goes out under the key a model reads, not the catalog's.
    assert_eq!(json["tools"][0]["name"], "get_xfa");
    assert!(json["tools"][0]["input_schema"]["properties"]["state"].is_object());
    // Caching is configured, or a long run pays full input price every turn.
    assert!(
        body.contains("cache_control"),
        "the request carries no cache_control: {body}"
    );
}
