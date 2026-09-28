//! The Anthropic transport, end to end against a local socket.
//!
//! There was never a test here: the hand-rolled client's SSE parsing and its
//! cache-breakpoint request body were only ever exercised against the live API.
//! rig owns the wire now, so what this pins is the part that is still ours —
//! that a streamed turn comes back assembled, that usage reaches the reply, and
//! that the request carries the caching configuration and the tool schemas.
//!
//! This drives `client::model_for`'s [`ModelHandle`] directly against rig's raw
//! `CompletionModel::stream`, one level below `pipeline`'s `Agent::runner` —
//! the invalid-tool-call and abort machinery layered on top of that is rig's
//! own execution model, already covered with no network at all by
//! `pipeline::hooks`'s `MockCompletionModel` tests.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use futures_util::StreamExt;
use rig_agent::agent::run::streamed::StreamedTurnAssembler;
use rig_core::completion::{CompletionModel, CompletionRequest, ToolDefinition};
use rig_core::streaming::StreamedAssistantContent;
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
    r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"xfa_read","input":{}}}"#,
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

/// Serve one request and answer with `status` and `body`, plus the headers in
/// `extra` (each already `name: value`).
fn serve_status(
    listener: TcpListener,
    status: &'static str,
    extra: &'static [&'static str],
    body: &'static str,
) {
    let (mut socket, _) = listener.accept().expect("a connection");
    let mut buf = [0u8; 8192];
    let _ = socket.read(&mut buf);
    let mut head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
        body.len()
    );
    for line in extra {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    socket.write_all(head.as_bytes()).expect("writable");
    socket.write_all(body.as_bytes()).expect("writable");
    socket.flush().ok();
}

/// Serve exactly one request, hand the raw bytes back, and reply with `body`.
fn serve_body(listener: TcpListener, sent: mpsc::Sender<String>, body: &'static str) {
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
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).expect("writable");
    socket.flush().ok();
}

/// Build the request one stage turn sends: the seed prompt, the tool catalog,
/// and the system prompt — the same shape `pipeline::run_stage` hands to
/// `Agent::runner`.
fn request(system: &str, tools: Vec<ToolDefinition>, max_tokens: u32) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: Some(system.to_string()),
        chat_history: vec![rig_core::message::Message::user("Analyse the source.")],
        documents: Vec::new(),
        tools,
        temperature: None,
        max_tokens: Some(u64::from(max_tokens)),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
        record_telemetry_content: false,
    }
}

#[tokio::test]
async fn a_streamed_anthropic_turn_is_assembled_and_billed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let addr = listener.local_addr().expect("an address");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve_body(listener, tx, SSE_BODY));

    let endpoint = LlmEndpoint {
        provider: runner::Provider::Anthropic,
        base_url: format!("http://{addr}"),
        api_key: "sk-test".into(),
        model: "claude-opus-5".into(),
    };
    let resolved = TurnPlan::for_endpoint(endpoint)
        .resolve()
        .expect("the endpoint resolves to a model");

    let tools = vec![ToolDefinition {
        name: "xfa_read".into(),
        description: "Read the XFA for a state.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"state": {"type": "string"}},
            "required": ["state"],
        }),
    }];

    let mut response = resolved
        .model
        .stream(request("You are an analyst.", tools.clone(), resolved.max_tokens))
        .await
        .expect("the request is accepted");

    let tool_names: std::collections::BTreeSet<String> =
        tools.iter().map(|t| t.name.clone()).collect();
    let mut assembler = StreamedTurnAssembler::new(tool_names.clone(), tool_names);
    let mut text = String::new();
    while let Some(item) = response.next().await {
        let item = item.expect("the stream carries no mid-flight error");
        if let StreamedAssistantContent::Text(chunk) = &item {
            text.push_str(&chunk.text);
        }
        assembler
            .ingest(&item)
            .expect("the fixture names only the registered tool");
    }
    let usage = response.usage();
    let turn = assembler.finish(None, &response.choice);

    // The text arrived in two deltas and must be joined.
    assert_eq!(text, "Reading the source.");

    // The tool call's arguments were split across two deltas; they have to be
    // reassembled into one object, or the tool is invoked with nothing.
    let calls: Vec<_> = turn
        .choice
        .iter()
        .filter_map(|c| match c {
            rig_core::message::AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "expected one tool call");
    assert_eq!(calls[0].function.name, "xfa_read");
    assert_eq!(
        calls[0].function.arguments,
        serde_json::json!({"state": "DE"})
    );

    // Usage has to reach the reply: without it the context gauge reads zero and
    // the spend report says a run was free.
    assert_eq!(usage.output_tokens, 57);
    assert_eq!(usage.cached_input_tokens, 800);
    assert_eq!(usage.cache_creation_input_tokens, 64);
    // Occupancy counts the cached buckets too: caching lowers the cost, not how
    // full the window is.
    let prompt_tokens = usage.input_tokens + usage.cached_input_tokens + usage.cache_creation_input_tokens;
    assert_eq!(prompt_tokens, 1200 + 800 + 64);

    // Opus is in the price table, so the call must carry a figure.
    let cost = (resolved.price)(&usage).expect("a priced model reports a cost");
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
    assert_eq!(json["tools"][0]["name"], "xfa_read");
    assert!(json["tools"][0]["input_schema"]["properties"]["state"].is_object());
    // Caching is configured, or a long run pays full input price every turn.
    assert!(
        body.contains("cache_control"),
        "the request carries no cache_control: {body}"
    );
    // The system prompt has to arrive, or every stage runs unprompted.
    assert!(body.contains("You are an analyst."), "{body}");
}

/// Run one call against a server that answers `status`, and return the error
/// text the controller would classify.
async fn error_text_for(status: &'static str, extra: &'static [&'static str]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let addr = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        serve_status(listener, status, extra, r#"{"error":{"message":"upstream"}}"#)
    });

    let endpoint = LlmEndpoint {
        provider: runner::Provider::Anthropic,
        base_url: format!("http://{addr}"),
        api_key: "sk-test".into(),
        model: "claude-opus-5".into(),
    };
    let resolved = TurnPlan::for_endpoint(endpoint)
        .resolve()
        .expect("the endpoint resolves to a model");

    // Anthropic's `raw_stream()` defers the actual send past the point where
    // `.stream()`'s own await could observe a non-2xx status — it always
    // resolves `Ok`, and the failure surfaces only once the stream is polled.
    // `run_stage` already drains the whole stream item by item for exactly
    // this reason; this mirrors that instead of trusting the outer await.
    let error = match resolved
        .model
        .stream(request("sys", Vec::new(), resolved.max_tokens))
        .await
    {
        Err(e) => e,
        Ok(mut response) => {
            use futures_util::StreamExt;
            response
                .next()
                .await
                .expect("a non-success status yields at least one stream item")
                .expect_err("a non-success status is an error")
        }
    };
    pipeline::describe_completion_error(&error)
}

/// The controller decides whether to retry by matching substrings of whatever
/// the transport wrote. That coupling is invisible to the compiler, and it has
/// already broken once: a gateway 503 stopped being retried when the wording
/// changed. Generate the real text and classify it.
#[tokio::test]
async fn transient_statuses_match_what_the_transport_writes() {
    for status in ["429 Too Many Requests", "502 Bad Gateway", "503 Service Unavailable"] {
        let text = error_text_for(status, &[]).await;
        assert!(
            pipeline::is_transient_error(&text),
            "{status} should be retried automatically, got: {text}"
        );
    }

    // A client mistake must not burn the automatic budget.
    let text = error_text_for("401 Unauthorized", &[]).await;
    assert!(
        !pipeline::is_transient_error(&text),
        "401 must go straight to the operator, got: {text}"
    );
}

/// A provider that names a wait window knows when its quota refills; guessing
/// an exponential backoff instead just earns another rejection.
#[tokio::test]
async fn a_retry_after_header_reaches_the_controller() {
    let text = error_text_for("429 Too Many Requests", &["retry-after: 42"]).await;
    assert!(
        text.contains("[retry-after: 42]"),
        "the retry-after window was dropped: {text}"
    );
    assert!(pipeline::is_transient_error(&text));
}
