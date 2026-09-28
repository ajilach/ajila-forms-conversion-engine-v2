//! The conformance suite a third-party author runs against their own server
//! before registering it — and what the render servers' own test suites now
//! run through, via `u2s-render-test-harness` delegating the generic checks
//! here (see that crate's `handshake_matches_manifest`).
//!
//! Gates *enablement*, not registration: registering a server means
//! executing it, which happens before any check is possible, so
//! `POST /v1/mcp-servers` runs first and conformance is a separate step
//! (PLAN.md: "Sequence is connect → discover → conform → enable, and only
//! the last edge is gated").
//!
//! Returns a [`ConformanceReport`] rather than panicking or erroring out on
//! the first problem — a CLI (`u2s-conformance`) prints it, a test suite
//! asserts [`ConformanceReport::passed`] — so a manifest with three separate
//! problems is reported with all three, the same accumulate-don't-stop
//! discipline `u2s_core::ConfigError` uses for the same reason.
//!
//! **The battery is per role and much weaker for `query`** (PLAN.md):
//! determinism is wrong by definition for a tool whose whole point is fresh
//! data, and whether it mutates external state is not testable at all. A
//! green tick on a `query` tool here means "honours its schema and does not
//! hang" — which is exactly what the timeout and the subset-match check in
//! [`run_vector`] check, no more.
//!
//! **`normalize`/`decode`/`encode` get one more check `query` does not:
//! determinism.** Each of their own test vectors is called twice; the two
//! `structured_content` results must match exactly. This is real signal for
//! these three roles specifically because their own contract already
//! implies it: a `decode`/`encode` result is a blob reference
//! (`{handle, byte_len, digest}`), and this workspace's blob store is
//! content-addressed, so identical input bytes must yield an identical
//! digest and handle on every call, full stop — a server whose two calls
//! disagree is either non-deterministic (a real bug: this workspace's own
//! round-trip and caching guarantees assume it is not) or not actually
//! reading its declared `$FIXTURES` path deterministically. First proven
//! against a real server in `u2s-aem-ubs-mcp`'s own `decode`/`encode` pair.
//!
//! **What this battery still does not check: round-trip fidelity between a
//! `decode` vector and its format's own `encode` tool** (PLAN.md's other
//! named check — "round-trip fidelity for `decode`/`encode` pairs"). Doing
//! that generically means reading the blob `decode` returns and feeding its
//! bytes into `encode`, which needs a `BlobStore` this function has no
//! access to today (`run`'s own signature takes only a connected client and
//! a fixture directory, shared by every render harness caller too). Left
//! for whoever wires a `BlobStore` through here — `u2s-aem-ubs-mcp`'s own
//! `tests/e2e.rs::decode_then_encode_reproduces_the_real_fixture` already
//! proves that specific pair by hand in the meantime, just not generically
//! for a third party's own server.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::manifest::{ServerManifest, ToolRole};
use crate::pool::{McpClient, McpClientPool, PoolError, Transport};

const FIXTURE_PLACEHOLDER: &str = "$FIXTURES";
/// A query tool must answer within this long to pass — "does not hang" made
/// concrete. Generous on purpose: a cold-start render can be slow, and a
/// false failure here is worse than a slow true one.
const VECTOR_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Fail,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    fn fail(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConformanceReport {
    pub findings: Vec<Finding>,
}

impl ConformanceReport {
    pub fn passed(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Fail)
    }
}

/// Runs the full battery against a connected client. `fixture_dir` resolves
/// every `$FIXTURES` placeholder in a declared test vector's arguments.
pub async fn run(client: &McpClient, fixture_dir: &Path) -> ConformanceReport {
    let mut findings = Vec::new();

    let advertised = match client.list_tools(Default::default()).await {
        Ok(tools) => tools.tools,
        Err(e) => {
            findings.push(Finding::fail(format!("tools/list failed: {e}")));
            return ConformanceReport { findings };
        }
    };

    for tool in &advertised {
        if tool.description.as_deref().unwrap_or("").is_empty() {
            findings.push(Finding::fail(format!(
                "tool {} has no description — tool descriptions are prompt surface",
                tool.name
            )));
        }
    }

    let manifest_json = match read_manifest(client).await {
        Ok(json) => json,
        Err(e) => {
            findings.push(Finding::fail(format!("reading u2s://manifest: {e}")));
            return ConformanceReport { findings };
        }
    };

    let manifest = match ServerManifest::parse(&manifest_json) {
        Ok(m) => m,
        Err(e) => {
            findings.push(Finding::fail(format!("manifest does not parse: {e}")));
            return ConformanceReport { findings };
        }
    };

    if let Err(e) = manifest.check_compatible() {
        findings.push(Finding::fail(e.to_string()));
    }

    check_tools_agree(&advertised, &manifest, &mut findings);

    for vector in &manifest.test_vectors {
        let role = manifest
            .tools
            .iter()
            .find(|t| t.tool == vector.tool)
            .map(|t| t.role);
        run_vector(client, vector, fixture_dir, role, &mut findings).await;
    }

    if let Some(sessions) = &manifest.sessions {
        session_battery(client, sessions, &manifest.test_vectors, fixture_dir, &mut findings).await;
    }

    ConformanceReport { findings }
}

/// The battery for a server that declares [`crate::manifest::SessionSupport`]
/// -- generic across every adopter, since it drives only the tools the
/// manifest itself names and needs no server-specific knowledge.
///
/// Deliberately does not exercise a mutator's `expected_revision` behaviour:
/// a mutator's *other* arguments (which field to set, say) are format- and
/// server-specific and nothing in the manifest declares a minimal valid
/// call, so a generic attempt would fail for reasons unrelated to session
/// mechanics. That half of the contract is proven by each server's own
/// end-to-end suite instead; see this crate's own doc for the follow-up
/// (richer, sequence-capable test vectors) that would close this gap
/// generically.
async fn session_battery(
    client: &McpClient,
    sessions: &crate::manifest::SessionSupport,
    test_vectors: &[crate::manifest::TestVector],
    fixture_dir: &Path,
    findings: &mut Vec<Finding>,
) {
    let Some(doc_path) = first_fixture_doc_path(test_vectors, fixture_dir) else {
        findings.push(Finding::fail(
            "sessions declared but no test vector names a $FIXTURES document to open one \
             against"
                .to_owned(),
        ));
        return;
    };

    let opened = match call_tool(client, &sessions.open, json!({ "doc_path": doc_path })).await {
        Ok(result) => result,
        Err(message) => {
            findings.push(Finding::fail(format!("{}: {message}", sessions.open)));
            return;
        }
    };
    let Some(handle) = structured_str(&opened, "session") else {
        findings.push(Finding::fail(format!(
            "{} did not return a string \"session\" in its structured result",
            sessions.open
        )));
        return;
    };
    if structured_u64(&opened, "revision") != Some(0) {
        findings.push(Finding::fail(format!(
            "{} must open at revision 0",
            sessions.open
        )));
    }

    match call_tool(client, &sessions.open, json!({ "doc_path": doc_path })).await {
        Ok(second_open) => {
            if structured_str(&second_open, "session") == Some(handle.clone()) {
                findings.push(Finding::fail(format!(
                    "two calls to {} returned the same session handle",
                    sessions.open
                )));
            }
            if let Some(second_handle) = structured_str(&second_open, "session") {
                let _ = call_tool(client, &sessions.close, json!({ "session": second_handle })).await;
            }
        }
        Err(message) => findings.push(Finding::fail(format!(
            "a second call to {}: {message}",
            sessions.open
        ))),
    }

    let via_path = call_tool(client, &sessions.probe, json!({ "doc_path": doc_path })).await;
    let via_session = call_tool(
        client,
        &sessions.probe,
        json!({ "session": handle, "revision": 0 }),
    )
    .await;
    match (via_path, via_session) {
        (Ok(a), Ok(b)) => {
            if !probe_results_agree(&a, &b) {
                findings.push(Finding::fail(format!(
                    "{} answers differently addressed by doc_path than by an untouched session \
                     at revision 0",
                    sessions.probe
                )));
            }
        }
        (Err(message), _) | (_, Err(message)) => findings.push(Finding::fail(format!(
            "{}: {message}",
            sessions.probe
        ))),
    }

    let bogus = call_tool(
        client,
        &sessions.probe,
        json!({ "session": "sess_00000000000000000000000000000000", "revision": 0 }),
    )
    .await;
    if !matches!(&bogus, Ok(r) if r.is_error == Some(true)) && bogus.is_ok() {
        findings.push(Finding::fail(format!(
            "{} did not refuse an unknown session handle",
            sessions.probe
        )));
    }

    if let Err(message) = call_tool(client, &sessions.close, json!({ "session": handle })).await {
        findings.push(Finding::fail(format!("{}: {message}", sessions.close)));
        return;
    }

    let after_close = call_tool(
        client,
        &sessions.probe,
        json!({ "session": handle, "revision": 0 }),
    )
    .await;
    if !matches!(&after_close, Ok(r) if r.is_error == Some(true)) {
        findings.push(Finding::fail(format!(
            "{} answered after its session was closed with {}",
            sessions.probe, sessions.close
        )));
    }
}

/// The document a session battery opens: the first `$FIXTURES`-containing
/// string argument any declared test vector carries, resolved. Session
/// support has no test-vector shape of its own to draw a fixture from, so it
/// borrows whichever one the server's own vectors already name.
fn first_fixture_doc_path(
    test_vectors: &[crate::manifest::TestVector],
    fixture_dir: &Path,
) -> Option<String> {
    fn find(value: &Value) -> Option<&str> {
        match value {
            Value::String(s) if s.contains(FIXTURE_PLACEHOLDER) => Some(s),
            Value::Object(map) => map.values().find_map(find),
            Value::Array(items) => items.iter().find_map(find),
            _ => None,
        }
    }
    test_vectors.iter().find_map(|v| find(&v.args)).map(|s| {
        s.replace(FIXTURE_PLACEHOLDER, &fixture_dir.display().to_string())
    })
}

async fn call_tool(
    client: &McpClient,
    tool: &str,
    args: Value,
) -> Result<rmcp::model::CallToolResult, String> {
    let mut params = rmcp::model::CallToolRequestParams::new(tool.to_owned());
    if let Some(obj) = args.as_object() {
        params = params.with_arguments(obj.clone());
    }
    match tokio::time::timeout(VECTOR_TIMEOUT, client.call_tool(params)).await {
        Err(_) => Err(format!(
            "{tool} did not respond within {}s",
            VECTOR_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(format!("{tool} failed transport: {e}")),
        Ok(Ok(result)) => Ok(result),
    }
}

fn structured_str(result: &rmcp::model::CallToolResult, key: &str) -> Option<String> {
    result
        .structured_content
        .as_ref()
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn structured_u64(result: &rmcp::model::CallToolResult, key: &str) -> Option<u64> {
    result
        .structured_content
        .as_ref()
        .and_then(|v| v.get(key))
        .and_then(Value::as_u64)
}

/// Two `probe` results agree when they say the same thing about the
/// document, ignoring the addressing-specific `session`/`revision` keys a
/// session-addressed answer may echo back that a `doc_path`-addressed one
/// has no occasion to carry.
fn probe_results_agree(a: &rmcp::model::CallToolResult, b: &rmcp::model::CallToolResult) -> bool {
    fn without_session_keys(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(k, _)| k.as_str() != "session" && k.as_str() != "revision")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let a = without_session_keys(&a.structured_content.clone().unwrap_or(Value::Null));
    let b = without_session_keys(&b.structured_content.clone().unwrap_or(Value::Null));
    a == b
}

async fn read_manifest(client: &McpClient) -> Result<Value, String> {
    let res = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "u2s://manifest",
        ))
        .await
        .map_err(|e| e.to_string())?;
    let text = res
        .contents
        .first()
        .and_then(|c| match c {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
            _ => None,
        })
        .ok_or_else(|| "u2s://manifest did not return text content".to_owned())?;
    serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))
}

fn check_tools_agree(
    advertised: &[rmcp::model::Tool],
    manifest: &ServerManifest,
    findings: &mut Vec<Finding>,
) {
    let mut advertised_names: Vec<&str> = advertised.iter().map(|t| t.name.as_ref()).collect();
    let mut declared_names: Vec<&str> = manifest.tools.iter().map(|t| t.tool.as_str()).collect();
    advertised_names.sort_unstable();
    declared_names.sort_unstable();

    if advertised_names != declared_names {
        findings.push(Finding::fail(format!(
            "tools/list and the manifest disagree: tools/list has {advertised_names:?}, \
             manifest declares {declared_names:?}"
        )));
    }
}

async fn run_vector(
    client: &McpClient,
    vector: &crate::manifest::TestVector,
    fixture_dir: &Path,
    role: Option<ToolRole>,
    findings: &mut Vec<Finding>,
) {
    if let Err(missing) = fixtures_exist(&vector.args, fixture_dir) {
        findings.push(Finding::fail(format!(
            "test vector for {}: fixture not found: {missing}",
            vector.tool
        )));
        return;
    }

    let result = match call_vector(client, vector, fixture_dir).await {
        Ok(result) => result,
        Err(message) => {
            findings.push(Finding::fail(message));
            return;
        }
    };

    // A vector whose whole point is that the call is refused
    // (`expect.error: true`) skips every check below that assumes success
    // -- see `TestVector`'s own doc.
    if vector.expect.get("error").and_then(Value::as_bool) == Some(true) {
        if let Err(reason) = error_expectation_met(&result, &vector.expect) {
            findings.push(Finding::fail(format!(
                "test vector for {}: {reason}",
                vector.tool
            )));
        }
        return;
    }

    if result.is_error == Some(true) {
        findings.push(Finding::fail(format!(
            "test vector for {} returned a tool error",
            vector.tool
        )));
        return;
    }

    if let Some(expected_structured) = vector.expect.get("structured") {
        let actual = result.structured_content.clone().unwrap_or(Value::Null);
        if !subset_matches(&actual, expected_structured) {
            findings.push(Finding::fail(format!(
                "test vector for {}: structured result does not match the manifest's \
                 declared expectation.\n  expected (subset): {expected_structured}\n  actual: {actual}",
                vector.tool
            )));
        }
    }

    // `normalize`/`decode`/`encode` get one more check `query` does not --
    // see this module's own doc for why determinism is real signal for
    // exactly these three roles.
    if matches!(role, Some(ToolRole::Normalize | ToolRole::Decode | ToolRole::Encode)) {
        check_determinism(client, vector, fixture_dir, &result, findings).await;
    }
}

/// The pure check behind `expect.error: true`: `result` must actually be a
/// tool error, and -- when `expect.error_contains` names a string -- some
/// content block's text must contain it. Split out from [`run_vector`] so
/// it is directly unit-testable without a live server.
fn error_expectation_met(
    result: &rmcp::model::CallToolResult,
    expect: &Value,
) -> Result<(), String> {
    if result.is_error != Some(true) {
        return Err("expect.error is true, but the call succeeded".to_owned());
    }
    let Some(needle) = expect.get("error_contains").and_then(Value::as_str) else {
        return Ok(());
    };
    let found = result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .any(|text| text.text.contains(needle));
    if found {
        Ok(())
    } else {
        let texts: Vec<&str> = result
            .content
            .iter()
            .filter_map(|block| block.as_text())
            .map(|text| text.text.as_str())
            .collect();
        Err(format!(
            "expect.error_contains {needle:?} was not found in any content block: {texts:?}"
        ))
    }
}

/// Resolves `$FIXTURES` in `vector.args` and calls it once, mapping every
/// transport/timeout failure to a single `Err(message)` -- the one place
/// [`run_vector`] and [`check_determinism`] both make this exact call, so
/// neither has to repeat the timeout/error-mapping boilerplate.
async fn call_vector(
    client: &McpClient,
    vector: &crate::manifest::TestVector,
    fixture_dir: &Path,
) -> Result<rmcp::model::CallToolResult, String> {
    let resolved_args = substitute_fixtures(&vector.args, fixture_dir);

    let mut params = rmcp::model::CallToolRequestParams::new(vector.tool.clone());
    if let Some(obj) = resolved_args.as_object() {
        params = params.with_arguments(obj.clone());
    }

    match tokio::time::timeout(VECTOR_TIMEOUT, client.call_tool(params)).await {
        Err(_) => Err(format!(
            "test vector for {} did not respond within {}s",
            vector.tool,
            VECTOR_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(format!("test vector for {} failed transport: {e}", vector.tool)),
        Ok(Ok(result)) => Ok(result),
    }
}

/// Calls `vector` a second time and requires its `structured_content` to
/// match the first call's exactly -- see this module's own doc for why
/// that is real signal for `normalize`/`decode`/`encode` specifically
/// (content-addressed blob handles), not a check borrowed from a different
/// kind of test suite without justification.
async fn check_determinism(
    client: &McpClient,
    vector: &crate::manifest::TestVector,
    fixture_dir: &Path,
    first: &rmcp::model::CallToolResult,
    findings: &mut Vec<Finding>,
) {
    let second = match call_vector(client, vector, fixture_dir).await {
        Ok(result) => result,
        Err(message) => {
            findings.push(Finding::fail(format!(
                "determinism check for {}: second call failed: {message}",
                vector.tool
            )));
            return;
        }
    };

    if let Err(reason) = determinism_agrees(first, &second) {
        findings.push(Finding::fail(format!("determinism check for {}: {reason}", vector.tool)));
    }
}

/// The pure comparison `check_determinism` needs: no network, so it is
/// directly unit-testable without a live server. `Err` names exactly what
/// disagreed, folded into `check_determinism`'s own finding message.
fn determinism_agrees(
    first: &rmcp::model::CallToolResult,
    second: &rmcp::model::CallToolResult,
) -> Result<(), String> {
    if second.is_error == Some(true) {
        return Err(
            "first call succeeded but the second returned a tool error".to_owned(),
        );
    }
    let first_structured = first.structured_content.clone().unwrap_or(Value::Null);
    let second_structured = second.structured_content.clone().unwrap_or(Value::Null);
    if first_structured != second_structured {
        return Err(format!(
            "two calls with the same arguments returned different structured results -- this \
             role's own contract requires identical input to produce an identical, \
             content-addressed result.\n  first:  {first_structured}\n  second: {second_structured}"
        ));
    }
    Ok(())
}

/// Every key `expected` declares must be present and equal in `actual`;
/// `actual` may carry more. Recurses into nested objects the same way, so
/// `{"blob": {"media_type": "image/png"}}` matches a real result carrying a
/// `blob` object with additional fields (`handle`, `digest`, …) the vector
/// does not care about. Arrays and scalars must match exactly — no current
/// manifest declares an array expectation, and a partial array match has no
/// obvious meaning (a prefix? a subset regardless of order?), so exact
/// equality is the honest default rather than a guessed one.
pub fn subset_matches(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual_map), Value::Object(expected_map)) => {
            expected_map.iter().all(|(key, expected_value)| {
                actual_map
                    .get(key)
                    .is_some_and(|actual_value| subset_matches(actual_value, expected_value))
            })
        }
        _ => actual == expected,
    }
}

/// Rewrites every `$FIXTURES` occurrence in a string value under `args` to
/// `fixture_dir`'s path — generalized from the render harness's
/// `doc_path`-only substitution, since a future server's arguments need not
/// use that key.
pub fn substitute_fixtures(args: &Value, fixture_dir: &Path) -> Value {
    match args {
        Value::String(s) if s.contains(FIXTURE_PLACEHOLDER) => {
            Value::String(s.replace(FIXTURE_PLACEHOLDER, &fixture_dir.display().to_string()))
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute_fixtures(v, fixture_dir)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| substitute_fixtures(v, fixture_dir))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Confirms every `$FIXTURES`-containing string in `args`, once resolved,
/// names a file that actually exists — returning the first that does not.
fn fixtures_exist(args: &Value, fixture_dir: &Path) -> Result<(), String> {
    match args {
        Value::String(s) if s.contains(FIXTURE_PLACEHOLDER) => {
            let resolved = s.replace(FIXTURE_PLACEHOLDER, &fixture_dir.display().to_string());
            if Path::new(&resolved).exists() {
                Ok(())
            } else {
                Err(resolved)
            }
        }
        Value::Object(map) => map
            .values()
            .try_for_each(|v| fixtures_exist(v, fixture_dir)),
        Value::Array(items) => items
            .iter()
            .try_for_each(|v| fixtures_exist(v, fixture_dir)),
        _ => Ok(()),
    }
}

/// The same battery, driven through a [`McpClientPool`] rather than a bare
/// client — what the app's conformance endpoint calls.
pub async fn run_via_pool(
    pool: &McpClientPool,
    server_id: &str,
    transport: &Transport,
    fixture_dir: &Path,
) -> Result<ConformanceReport, PoolError> {
    // The pool has no method that hands back a bare `McpClient` (its pooled
    // connections are private), so this drives the same steps `run` does
    // through the pool's own call surface instead of duplicating `run`
    // itself. Kept as a second entry point rather than unifying the two,
    // because `run` needs a bare client for the render-harness delegation,
    // where a pool would be one-shot-per-test overhead for no benefit.
    let advertised = pool.list_tools(server_id, transport).await?;
    let mut findings = Vec::new();

    for tool in &advertised.tools {
        if tool.description.as_deref().unwrap_or("").is_empty() {
            findings.push(Finding::fail(format!(
                "tool {} has no description",
                tool.name
            )));
        }
    }

    let manifest_json = pool.read_manifest(server_id, transport).await?;
    let manifest = match ServerManifest::parse(&manifest_json) {
        Ok(m) => m,
        Err(e) => {
            findings.push(Finding::fail(format!("manifest does not parse: {e}")));
            return Ok(ConformanceReport { findings });
        }
    };
    if let Err(e) = manifest.check_compatible() {
        findings.push(Finding::fail(e.to_string()));
    }
    check_tools_agree(&advertised.tools, &manifest, &mut findings);

    for vector in &manifest.test_vectors {
        if let Err(missing) = fixtures_exist(&vector.args, fixture_dir) {
            findings.push(Finding::fail(format!(
                "test vector for {}: fixture not found: {missing}",
                vector.tool
            )));
            continue;
        }

        let result = match call_vector_via_pool(pool, server_id, transport, vector, fixture_dir).await {
            Ok(result) => result,
            Err(message) => {
                findings.push(Finding::fail(message));
                continue;
            }
        };

        if result.is_error == Some(true) {
            findings.push(Finding::fail(format!(
                "test vector for {} returned a tool error",
                vector.tool
            )));
            continue;
        }
        if let Some(expected) = vector.expect.get("structured") {
            let actual = result.structured_content.clone().unwrap_or(Value::Null);
            if !subset_matches(&actual, expected) {
                findings.push(Finding::fail(format!(
                    "test vector for {}: structured result does not match",
                    vector.tool
                )));
            }
        }

        // Same determinism check `run` applies -- see this module's own
        // doc for why it is real signal for exactly these three roles.
        let role = manifest.tools.iter().find(|t| t.tool == vector.tool).map(|t| t.role);
        if matches!(role, Some(ToolRole::Normalize | ToolRole::Decode | ToolRole::Encode)) {
            let second = call_vector_via_pool(pool, server_id, transport, vector, fixture_dir).await;
            match second {
                Ok(second) => {
                    if let Err(reason) = determinism_agrees(&result, &second) {
                        findings.push(Finding::fail(format!("determinism check for {}: {reason}", vector.tool)));
                    }
                }
                Err(message) => findings.push(Finding::fail(format!(
                    "determinism check for {}: second call failed: {message}",
                    vector.tool
                ))),
            }
        }
    }

    if let Some(sessions) = &manifest.sessions {
        session_battery_via_pool(
            pool,
            server_id,
            transport,
            sessions,
            &manifest.test_vectors,
            fixture_dir,
            &mut findings,
        )
        .await;
    }

    Ok(ConformanceReport { findings })
}

/// [`session_battery`]'s pool-driven equivalent -- see `run_via_pool`'s own
/// doc for why this is a deliberate second entry point rather than a
/// unification of the two. Shares every pure helper with `session_battery`;
/// only the call mechanics differ.
#[allow(clippy::too_many_arguments)]
async fn session_battery_via_pool(
    pool: &McpClientPool,
    server_id: &str,
    transport: &Transport,
    sessions: &crate::manifest::SessionSupport,
    test_vectors: &[crate::manifest::TestVector],
    fixture_dir: &Path,
    findings: &mut Vec<Finding>,
) {
    let Some(doc_path) = first_fixture_doc_path(test_vectors, fixture_dir) else {
        findings.push(Finding::fail(
            "sessions declared but no test vector names a $FIXTURES document to open one \
             against"
                .to_owned(),
        ));
        return;
    };
    let call = |tool: &str, args: Value| {
        call_tool_via_pool(pool, server_id, transport, tool.to_owned(), args)
    };

    let opened = match call(&sessions.open, json!({ "doc_path": doc_path })).await {
        Ok(result) => result,
        Err(message) => {
            findings.push(Finding::fail(format!("{}: {message}", sessions.open)));
            return;
        }
    };
    let Some(handle) = structured_str(&opened, "session") else {
        findings.push(Finding::fail(format!(
            "{} did not return a string \"session\" in its structured result",
            sessions.open
        )));
        return;
    };
    if structured_u64(&opened, "revision") != Some(0) {
        findings.push(Finding::fail(format!(
            "{} must open at revision 0",
            sessions.open
        )));
    }

    let via_path = call(&sessions.probe, json!({ "doc_path": doc_path })).await;
    let via_session = call(
        &sessions.probe,
        json!({ "session": handle, "revision": 0 }),
    )
    .await;
    match (via_path, via_session) {
        (Ok(a), Ok(b)) => {
            if !probe_results_agree(&a, &b) {
                findings.push(Finding::fail(format!(
                    "{} answers differently addressed by doc_path than by an untouched session \
                     at revision 0",
                    sessions.probe
                )));
            }
        }
        (Err(message), _) | (_, Err(message)) => {
            findings.push(Finding::fail(format!("{}: {message}", sessions.probe)))
        }
    }

    let bogus = call(
        &sessions.probe,
        json!({ "session": "sess_00000000000000000000000000000000", "revision": 0 }),
    )
    .await;
    if !matches!(&bogus, Ok(r) if r.is_error == Some(true)) && bogus.is_ok() {
        findings.push(Finding::fail(format!(
            "{} did not refuse an unknown session handle",
            sessions.probe
        )));
    }

    if let Err(message) = call(&sessions.close, json!({ "session": handle.clone() })).await {
        findings.push(Finding::fail(format!("{}: {message}", sessions.close)));
        return;
    }

    let after_close = call(&sessions.probe, json!({ "session": handle, "revision": 0 })).await;
    if !matches!(&after_close, Ok(r) if r.is_error == Some(true)) {
        findings.push(Finding::fail(format!(
            "{} answered after its session was closed with {}",
            sessions.probe, sessions.close
        )));
    }
}

async fn call_tool_via_pool(
    pool: &McpClientPool,
    server_id: &str,
    transport: &Transport,
    tool: String,
    args: Value,
) -> Result<rmcp::model::CallToolResult, String> {
    let call = pool.call_tool(server_id, transport, 1, &tool, args);
    match tokio::time::timeout(VECTOR_TIMEOUT, call).await {
        Err(_) => Err(format!(
            "{tool} did not respond within {}s",
            VECTOR_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(format!("{tool} failed: {e}")),
        Ok(Ok(result)) => Ok(result),
    }
}

/// [`call_vector`]'s pool-driven equivalent -- see `run_via_pool`'s own doc
/// for why this is a deliberate second entry point rather than a
/// unification of the two.
async fn call_vector_via_pool(
    pool: &McpClientPool,
    server_id: &str,
    transport: &Transport,
    vector: &crate::manifest::TestVector,
    fixture_dir: &Path,
) -> Result<rmcp::model::CallToolResult, String> {
    let resolved_args = substitute_fixtures(&vector.args, fixture_dir);
    let call = pool.call_tool(server_id, transport, 1, &vector.tool, resolved_args);
    match tokio::time::timeout(VECTOR_TIMEOUT, call).await {
        Err(_) => Err(format!(
            "test vector for {} did not respond within {}s",
            vector.tool,
            VECTOR_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(format!("test vector for {} failed: {e}", vector.tool)),
        Ok(Ok(result)) => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subset_matches_a_partial_object() {
        let actual = json!({ "count": 1, "extra": "ignored" });
        let expected = json!({ "count": 1 });
        assert!(subset_matches(&actual, &expected));
    }

    #[test]
    fn subset_matches_rejects_a_wrong_value() {
        let actual = json!({ "count": 2 });
        let expected = json!({ "count": 1 });
        assert!(!subset_matches(&actual, &expected));
    }

    #[test]
    fn subset_matches_recurses_into_nested_objects() {
        let actual = json!({ "blob": { "media_type": "image/png", "byte_len": 999 } });
        let expected = json!({ "blob": { "media_type": "image/png" } });
        assert!(subset_matches(&actual, &expected));
    }

    #[test]
    fn subset_matches_requires_the_key_to_be_present() {
        let actual = json!({ "other": 1 });
        let expected = json!({ "count": 1 });
        assert!(!subset_matches(&actual, &expected));
    }

    #[test]
    fn determinism_agrees_when_two_calls_return_identical_structured_content() {
        let first = rmcp::model::CallToolResult::structured(json!({ "digest": "abc123" }));
        let second = rmcp::model::CallToolResult::structured(json!({ "digest": "abc123" }));
        assert!(determinism_agrees(&first, &second).is_ok());
    }

    /// The success criterion this check exists for: two calls with the same
    /// arguments to a `decode`/`encode`/`normalize` tool disagreeing on a
    /// content-addressed digest is exactly the non-determinism this check
    /// must catch.
    #[test]
    fn determinism_agrees_fails_when_two_calls_disagree() {
        let first = rmcp::model::CallToolResult::structured(json!({ "digest": "abc123" }));
        let second = rmcp::model::CallToolResult::structured(json!({ "digest": "def456" }));
        assert!(determinism_agrees(&first, &second).is_err());
    }

    #[test]
    fn error_expectation_met_requires_the_call_to_have_actually_failed() {
        let succeeded = rmcp::model::CallToolResult::structured(json!({ "ok": true }));
        assert!(error_expectation_met(&succeeded, &json!({ "error": true })).is_err());
    }

    #[test]
    fn error_expectation_met_passes_with_no_error_contains_as_long_as_it_errored() {
        let failed = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
            "is not open".to_owned(),
        )]);
        assert!(error_expectation_met(&failed, &json!({ "error": true })).is_ok());
    }

    #[test]
    fn error_expectation_met_checks_error_contains_against_every_text_block() {
        let failed = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
            "form \"form_x\" is not open".to_owned(),
        )]);
        assert!(
            error_expectation_met(
                &failed,
                &json!({ "error": true, "error_contains": "is not open" })
            )
            .is_ok()
        );
        assert!(
            error_expectation_met(
                &failed,
                &json!({ "error": true, "error_contains": "something else entirely" })
            )
            .is_err()
        );
    }

    #[test]
    fn determinism_agrees_fails_when_the_second_call_errors() {
        let first = rmcp::model::CallToolResult::structured(json!({ "digest": "abc123" }));
        let second = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
            "transient failure",
        )]);
        assert!(determinism_agrees(&first, &second).is_err());
    }

    #[test]
    fn arrays_and_scalars_match_exactly() {
        assert!(subset_matches(&json!([1, 2]), &json!([1, 2])));
        assert!(!subset_matches(&json!([1, 2, 3]), &json!([1, 2])));
        assert!(subset_matches(&json!("x"), &json!("x")));
        assert!(!subset_matches(&json!("x"), &json!("y")));
    }

    #[test]
    fn substitute_fixtures_replaces_only_the_placeholder_not_the_whole_string() {
        let args = json!({ "doc_path": "$FIXTURES/minimal.xfa.pdf", "page": 1 });
        let resolved = substitute_fixtures(&args, Path::new("/tmp/fixtures"));
        assert_eq!(resolved["doc_path"], json!("/tmp/fixtures/minimal.xfa.pdf"));
        assert_eq!(resolved["page"], json!(1));
    }

    #[test]
    fn substitute_fixtures_is_not_limited_to_a_doc_path_key() {
        let args = json!({ "any_key_name": "$FIXTURES/x.pdf" });
        let resolved = substitute_fixtures(&args, Path::new("/tmp/f"));
        assert_eq!(resolved["any_key_name"], json!("/tmp/f/x.pdf"));
    }

    #[test]
    fn fixtures_exist_reports_the_first_missing_file() {
        let dir = std::env::temp_dir().join("u2s-mcp-conformance-test-existing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("present.pdf"), b"").unwrap();

        assert!(fixtures_exist(&json!({ "p": "$FIXTURES/present.pdf" }), &dir).is_ok());
        assert!(fixtures_exist(&json!({ "p": "$FIXTURES/missing.pdf" }), &dir).is_err());
    }

    #[test]
    fn a_report_with_only_warnings_still_passes() {
        let report = ConformanceReport {
            findings: vec![Finding {
                severity: Severity::Warn,
                message: "fyi".to_owned(),
            }],
        };
        assert!(report.passed());
    }

    #[test]
    fn a_report_with_a_failure_does_not_pass() {
        let report = ConformanceReport {
            findings: vec![Finding::fail("bad")],
        };
        assert!(!report.passed());
    }

    #[test]
    fn probe_results_agree_ignoring_session_and_revision() {
        let a = rmcp::model::CallToolResult::structured(json!({ "page_count": 3 }));
        let b = rmcp::model::CallToolResult::structured(
            json!({ "page_count": 3, "session": "sess_x", "revision": 0 }),
        );
        assert!(probe_results_agree(&a, &b));
    }

    #[test]
    fn probe_results_disagree_on_a_real_difference() {
        let a = rmcp::model::CallToolResult::structured(json!({ "page_count": 3 }));
        let b = rmcp::model::CallToolResult::structured(json!({ "page_count": 4 }));
        assert!(!probe_results_agree(&a, &b));
    }

    #[test]
    fn first_fixture_doc_path_finds_the_first_placeholder_string() {
        let vectors = vec![crate::manifest::TestVector {
            tool: "xfa_info".to_owned(),
            args: json!({ "doc_path": "$FIXTURES/minimal.xfa.pdf" }),
            expect: Value::Null,
        }];
        let resolved = first_fixture_doc_path(&vectors, Path::new("/tmp/fixtures"));
        assert_eq!(
            resolved,
            Some("/tmp/fixtures/minimal.xfa.pdf".to_string())
        );
    }

    #[test]
    fn first_fixture_doc_path_is_none_without_any_vector() {
        assert_eq!(first_fixture_doc_path(&[], Path::new("/tmp/fixtures")), None);
    }
}
