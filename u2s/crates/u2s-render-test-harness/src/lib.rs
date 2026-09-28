//! Black-box MCP test battery shared by the u2s render servers.
//!
//! The `pdf_*` and `xfa_*` tool surfaces are deliberately parallel, so the
//! scenarios that check *contract* rather than *content* — cursor completeness,
//! blob integrity, error semantics, boot behaviour, determinism — are written
//! once here and parameterised by tool prefix. Each server's own suite then
//! covers only what is genuinely specific to it.
//!
//! Everything here drives a real server binary over real stdio MCP. Nothing
//! reaches into a server's internals; that is the point.

use std::path::{Path, PathBuf};

use rmcp::model::{CallToolRequestParams, CallToolResult, ReadResourceRequestParams};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use tokio::process::Command;

pub type Client = RunningService<RoleClient, ()>;

/// A server under test: where its binary is, what environment it needs, and
/// which tool names it answers to.
#[derive(Clone, Debug)]
pub struct ServerUnderTest {
    pub binary: PathBuf,
    /// Tool-name prefix, e.g. `"pdf"` or `"xfa"`.
    pub prefix: &'static str,
    pub env: Vec<(String, String)>,
}

impl ServerUnderTest {
    /// Locate a sibling binary from the running test executable. Test binaries
    /// live in `target/<profile>/deps`; servers sit one level up.
    pub fn locate(name: &str, prefix: &'static str) -> Self {
        let mut dir = std::env::current_exe().expect("current exe");
        dir.pop();
        if dir.ends_with("deps") {
            dir.pop();
        }
        let binary = dir.join(name);
        assert!(
            binary.exists(),
            "server binary not built at {} — run `cargo build -p {name}`",
            binary.display()
        );
        ServerUnderTest {
            binary,
            prefix,
            env: Vec::new(),
        }
    }

    pub fn env(mut self, key: &str, value: impl AsRef<str>) -> Self {
        self.env.push((key.to_string(), value.as_ref().to_string()));
        self
    }

    pub fn tool(&self, suffix: &str) -> String {
        format!("{}_{}", self.prefix, suffix)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Spawn the server and complete the MCP handshake.
    pub async fn connect(&self) -> Client {
        ().serve(TokioChildProcess::new(self.command()).expect("spawn server"))
            .await
            .expect("initialize")
    }

    /// Run the server to completion with an overridden environment, returning
    /// its exit status and streams. Used by the boot-failure contract.
    pub fn run_to_exit(&self, overrides: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = std::process::Command::new(&self.binary);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        for (k, v) in overrides {
            cmd.env(k, v);
        }
        cmd.output().expect("run server")
    }
}

// ------------------------------------------------------------------ calling

pub async fn call(client: &Client, name: &str, args: Value) -> CallToolResult {
    let mut params = CallToolRequestParams::new(name.to_string());
    if let Some(obj) = args.as_object() {
        params = params.with_arguments(obj.clone());
    }
    client.call_tool(params).await.expect("tool call transport")
}

/// The structured payload of a successful call. Panics with the tool's own
/// error text when the call failed, so a broken assertion reads as the server's
/// message rather than "None unwrapped".
pub fn structured(result: &CallToolResult) -> &Value {
    assert_ne!(
        result.is_error,
        Some(true),
        "tool returned an error: {}",
        error_text(result)
    );
    result
        .structured_content
        .as_ref()
        .expect("tool returned no structured content")
}

pub fn error_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|b| b.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn image_blocks(result: &CallToolResult) -> usize {
    result
        .content
        .iter()
        .filter(|b| b.as_image().is_some())
        .count()
}

pub async fn read_manifest(client: &Client) -> Value {
    let res = client
        .read_resource(ReadResourceRequestParams::new("u2s://manifest"))
        .await
        .expect("read manifest");
    let body = match &res.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        _ => panic!("manifest must be text"),
    };
    serde_json::from_str(&body).expect("manifest is json")
}

// ------------------------------------------------------------- the battery

/// `tools/list` and the manifest must describe the same set, every tool must
/// carry a description (they are prompt surface), every declared test vector
/// must reference a fixture that exists and, called for real, must produce a
/// result matching its declared `expect`, and every tool name must carry this
/// server's prefix.
///
/// The generic half of this — everything except the prefix check, which is
/// this workspace's own naming convention rather than part of PLAN.md's
/// contract, and so has no way to derive itself from a manifest — is
/// [`u2s_mcp::conformance::run`], generalized out of what this function used
/// to check inline. Kept here as the one entry point every server's test
/// suite already calls, so generalizing it changed no call site.
pub async fn handshake_matches_manifest(server: &ServerUnderTest, fixture_dir: &Path) {
    let client = server.connect().await;

    for t in &client
        .list_tools(Default::default())
        .await
        .expect("list_tools")
        .tools
    {
        assert!(
            t.name.starts_with(server.prefix),
            "tool {} does not carry the {} prefix — names would collide when both \
             servers are registered",
            t.name,
            server.prefix
        );
    }

    let report = u2s_mcp::conformance::run(&client, fixture_dir).await;
    assert!(
        report.passed(),
        "{} failed conformance:\n{:#?}",
        server.binary.display(),
        report.findings
    );

    client.cancel().await.ok();
}

/// Walking `next_from` to exhaustion must render every page exactly once, in
/// order. `expected_pages` is the document's true page count.
///
/// Returns how many batches stopped on the byte budget, so a caller can assert
/// that both caps were genuinely exercised rather than only the count cap.
pub async fn cursor_walk_complete(
    server: &ServerUnderTest,
    doc_path: &str,
    expected_pages: u64,
    limit: u64,
    extra_args: Value,
) -> usize {
    let client = server.connect().await;

    let mut seen: Vec<u64> = Vec::new();
    let mut from = Some(1u64);
    let mut budget_stops = 0;
    let mut hops = 0;

    while let Some(start) = from {
        let mut args = serde_json::json!({
            "doc_path": doc_path, "from": start, "limit": limit
        });
        if let (Some(a), Some(extra)) = (args.as_object_mut(), extra_args.as_object()) {
            for (k, v) in extra {
                a.insert(k.clone(), v.clone());
            }
        }

        let batch = call(&client, &server.tool("render_pages"), args).await;
        let s = structured(&batch);
        let rendered: Vec<u64> = s["rendered"]
            .as_array()
            .expect("rendered array")
            .iter()
            .map(|v| v.as_u64().expect("page number"))
            .collect();

        assert!(!rendered.is_empty(), "a batch must make progress");
        assert!(
            rendered.len() as u64 <= limit,
            "count cap exceeded: {rendered:?}"
        );
        if s["budget_hit"] == "bytes" {
            budget_stops += 1;
        }
        seen.extend(rendered);
        from = s["next_from"].as_u64();

        hops += 1;
        assert!(hops < 2000, "cursor failed to terminate");
    }

    let expected: Vec<u64> = (1..=expected_pages).collect();
    assert_eq!(
        seen, expected,
        "the walk must cover every page once, in order"
    );

    client.cancel().await.ok();
    budget_stops
}

/// A search hit must address itself: its `page`, `offset` and `length`, handed
/// straight to the page-text tool, must return the match itself.
///
/// This is the whole contract of the search tools — find, then read only the
/// page that matters — and it is identical on both servers, so it lives here
/// rather than twice. `expected` is the text the query must find; `extra_args`
/// carries whatever a particular server needs (the XFA server's `state`).
///
/// Asserted by composition rather than against a literal offset on purpose:
/// every fixture in this workspace is ASCII, so a number could not tell a
/// character offset from a byte one, while the composition holds on any input.
pub async fn search_hit_addresses_itself(
    server: &ServerUnderTest,
    doc_path: &str,
    query: &str,
    expected: &str,
    extra_args: Value,
) {
    let client = server.connect().await;

    let mut args = serde_json::json!({ "doc_path": doc_path, "query": query });
    merge(&mut args, &extra_args);
    let found = call(&client, &server.tool("search_text"), args).await;
    let s = structured(&found);

    assert!(
        s["total_matches"].as_u64().unwrap_or(0) >= 1,
        "{query:?} should be found in {doc_path}: {s}"
    );
    let hit = &s["matches"].as_array().expect("matches array")[0];
    assert!(
        hit["context"].as_str().unwrap_or("").contains(expected),
        "the context should quote the match: {hit}"
    );

    let mut args = serde_json::json!({
        "doc_path": doc_path,
        "page": hit["page"],
        "offset": hit["offset"],
        "limit": hit["length"],
    });
    merge(&mut args, &extra_args);
    let read = call(&client, &server.tool("page_text"), args).await;

    assert_eq!(
        structured(&read)["text"],
        expected,
        "a hit's page and offset must address it in {}'s own coordinates",
        server.tool("page_text")
    );

    client.cancel().await.ok();
}

/// Walking `next_from` to exhaustion must cover every page of the document
/// exactly once, in order — the same guarantee the render cursor makes, and
/// what makes a resumed search safe from repeated matches.
pub async fn search_cursor_walk_complete(
    server: &ServerUnderTest,
    doc_path: &str,
    expected_pages: u64,
    query: &str,
    extra_args: Value,
) {
    let client = server.connect().await;

    let mut seen: Vec<u64> = Vec::new();
    let mut from = Some(1u64);
    let mut hops = 0;

    while let Some(start) = from {
        let mut args = serde_json::json!({
            "doc_path": doc_path, "query": query, "from": start
        });
        merge(&mut args, &extra_args);
        let found = call(&client, &server.tool("search_text"), args).await;
        let s = structured(&found);

        let through = s["through"].as_u64().expect("through");
        assert!(through >= start, "a call must make progress: {s}");
        seen.extend(start..=through);

        from = s["next_from"].as_u64();
        if from.is_some() {
            assert!(
                s["budget_hit"].is_string(),
                "a cursor must name the cap that produced it: {s}"
            );
        }

        hops += 1;
        assert!(hops < 2000, "cursor failed to terminate");
    }

    let expected: Vec<u64> = (1..=expected_pages).collect();
    assert_eq!(
        seen, expected,
        "the walk must cover every page once, in order"
    );

    client.cancel().await.ok();
}

/// Fold `extra` into `args`, which is how a shared scenario stays usable by a
/// server that needs an argument the other does not.
fn merge(args: &mut Value, extra: &Value) {
    if let (Some(a), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            a.insert(k.clone(), v.clone());
        }
    }
}

/// An image too large to inline must come back as a handle whose file exists
/// and whose digest describes its actual contents.
pub async fn blob_digest_valid(server: &ServerUnderTest, doc_path: &str, dpi: u64) {
    let client = server.connect().await;

    let render = call(
        &client,
        &server.tool("render_page"),
        serde_json::json!({ "doc_path": doc_path, "page": 1, "dpi": dpi, "format": "png" }),
    )
    .await;

    let s = structured(&render);
    assert_eq!(
        s["inline"], false,
        "expected this render to exceed the inline cap"
    );
    assert_eq!(image_blocks(&render), 0, "a blob must not also be inlined");

    let blob = &s["blob"];
    let path = blob["path"].as_str().expect("blob path");
    let bytes = std::fs::read(path).expect("blob file must exist");
    assert_eq!(bytes.len() as u64, blob["byte_len"].as_u64().unwrap());

    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        blob["digest"].as_str().unwrap(),
        "digest must match contents"
    );

    client.cancel().await.ok();
}

/// One expected failure: the call to make, and a fragment its message must
/// contain.
pub struct ErrorCase {
    pub tool: String,
    pub args: Value,
    pub expect_contains: Vec<String>,
}

impl ErrorCase {
    pub fn new(tool: impl Into<String>, args: Value, contains: &[&str]) -> Self {
        ErrorCase {
            tool: tool.into(),
            args,
            expect_contains: contains.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Every failure a caller could act on must be a *tool* error carrying a
/// readable message — never a protocol error — and the session must survive all
/// of them. `healthy` is a call that must still succeed afterwards.
pub async fn errors_are_tool_errors(
    server: &ServerUnderTest,
    cases: &[ErrorCase],
    healthy: (String, Value),
) {
    let client = server.connect().await;

    for case in cases {
        let result = call(&client, &case.tool, case.args.clone()).await;
        assert_eq!(
            result.is_error,
            Some(true),
            "{} should have failed for {}",
            case.tool,
            case.args
        );
        let msg = error_text(&result);
        for fragment in &case.expect_contains {
            assert!(
                msg.contains(fragment.as_str()),
                "{} message should mention {fragment:?}, got: {msg}",
                case.tool
            );
        }
    }

    let ok = call(&client, &healthy.0, healthy.1).await;
    assert_ne!(ok.is_error, Some(true), "session died after errors");

    client.cancel().await.ok();
}

/// A server that cannot initialise its engine must say so and exit, before
/// emitting any MCP output — one clear failure rather than a confusing one per
/// call forever.
pub fn boot_failure_contract(
    server: &ServerUnderTest,
    overrides: &[(&str, &str)],
    stderr_must_mention: &[&str],
) {
    let out = server.run_to_exit(overrides);

    assert!(!out.status.success(), "server must exit non-zero");
    assert_eq!(out.status.code(), Some(2), "documented exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot start"), "{stderr}");
    for fragment in stderr_must_mention {
        assert!(
            stderr.contains(fragment),
            "startup failure must say how to fix it ({fragment}): {stderr}"
        );
    }
    assert!(
        out.stdout.is_empty(),
        "must not emit MCP output before failing"
    );
}

/// The same request in two separate processes must produce identical bytes.
///
/// This is the check a process-global hash-order dependency fails: within one
/// process a lookup is at least consistent, so only a fresh process with a new
/// hash seed exposes it.
pub async fn cross_process_determinism(server: &ServerUnderTest, tool_args: Value, tool: &str) {
    let mut digests = Vec::new();

    for _ in 0..2 {
        let client = server.connect().await;
        let result = call(&client, tool, tool_args.clone()).await;
        let s = structured(&result);

        // Compare the image itself, whether it arrived inline or as a blob.
        let bytes: Vec<u8> = if s["inline"] == false {
            std::fs::read(s["blob"]["path"].as_str().expect("blob path")).expect("blob")
        } else {
            use base64_lite::decode;
            let b64 = result
                .content
                .iter()
                .find_map(|b| b.as_image().map(|i| i.data.clone()))
                .expect("inline image");
            decode(&b64)
        };

        use sha2::{Digest, Sha256};
        digests.push(format!("{:x}", Sha256::digest(&bytes)));
        client.cancel().await.ok();
    }

    assert_eq!(
        digests[0], digests[1],
        "two processes rendered the same request differently — a hash-order or \
         other nondeterminism is leaking into layout"
    );
}

/// Minimal base64 decoding, so the harness does not pull a dependency for one
/// call site.
mod base64_lite {
    pub fn decode(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut lut = [255u8; 256];
        for (i, c) in T.iter().enumerate() {
            lut[*c as usize] = i as u8;
        }
        let mut out = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0u32;
        for b in s.bytes() {
            let v = lut[b as usize];
            if v == 255 {
                continue; // padding and whitespace
            }
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }
}

// ------------------------------------------------------------ image checks

/// Fraction of pixels that are not near-white. A rendered page of a real form
/// has ink on it; a blank page or an unrendered shim does not.
pub fn dark_pixel_ratio(png_or_jpeg: &[u8]) -> f64 {
    let img = image::load_from_memory(png_or_jpeg)
        .expect("decode image")
        .to_luma8();
    let total = (img.width() * img.height()) as f64;
    let dark = img.pixels().filter(|p| p.0[0] < 200).count() as f64;
    dark / total.max(1.0)
}
