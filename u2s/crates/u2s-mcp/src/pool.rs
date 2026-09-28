//! The MCP client pool: lazy, keyed by server id, stdio today.
//!
//! **Lazy** — a registered server is not connected until something calls it,
//! so registering ten servers does not spawn ten processes. **Per-server
//! concurrency-limited** — the XFA renderer's font manager is a
//! process-global `OnceLock<Mutex<_>>` (documented in PLAN.md's risks), so
//! two concurrent renders in one process cross-contaminate; the pool
//! serializes calls to a given server through a `Semaphore` rather than
//! trusting every future server author to serialize internally.
//!
//! `list_tools` and `read_resource` (the manifest) are **not** gated by the
//! semaphore: they touch no renderer state, and gating them would make
//! discovery wait behind an unrelated in-flight render for no reason.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, ListToolsResult, ReadResourceRequestParams, ResourceContents,
};
// Re-exported (via `crate::lib`'s `pub use pool::{..., CallToolResult, ...}`)
// so a caller like `service::normalize`, which deliberately names no
// `rmcp` type of its own (see `service::agent_bridge`'s module doc), can
// still type the result of `call_tool` without adding an `rmcp` dependency
// itself.
pub use rmcp::model::CallToolResult;
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};

pub type McpClient = RunningService<RoleClient, ()>;

/// How to reach a registered server. `Http` is declared, not yet
/// implemented — see [`PoolError::HttpNotYetImplemented`] — because no HTTP
/// MCP server exists in this workspace to prove an implementation against;
/// the three real servers are all stdio, and that path is fully built and
/// tested here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        /// The canonical path from [`crate::registration::validate_stdio_command`]
        /// — this crate does not re-validate it, so a caller skipping that
        /// check gets whatever `Command::new` does with an arbitrary path,
        /// which is the caller's mistake to have made.
        command: PathBuf,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("connecting to {server_id}: {detail}")]
    Connect { server_id: String, detail: String },
    #[error("HTTP transport is not yet implemented (server {server_id})")]
    HttpNotYetImplemented { server_id: String },
    #[error("calling {tool} on {server_id}: {detail}")]
    Call {
        server_id: String,
        tool: String,
        detail: String,
    },
    #[error("reading the manifest from {server_id}: {detail}")]
    Manifest { server_id: String, detail: String },
}

struct Pooled {
    client: McpClient,
    /// Bounds concurrent `call_tool`s against this one server process.
    concurrency: Semaphore,
}

/// Lazily-connected MCP clients, one per registered server.
pub struct McpClientPool {
    clients: Mutex<HashMap<String, Arc<Pooled>>>,
}

impl Default for McpClientPool {
    fn default() -> Self {
        Self::new()
    }
}

impl McpClientPool {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the pooled client for `server_id`, connecting it on first use.
    /// `max_concurrency` only takes effect on that first connect — a later
    /// call with a different value against an already-pooled server is
    /// ignored, which is safe because the value only ever tightens a known
    /// engine-safety constraint, never loosens per-call.
    async fn client(
        &self,
        server_id: &str,
        transport: &Transport,
        max_concurrency: usize,
    ) -> Result<Arc<Pooled>, PoolError> {
        {
            let clients = self.clients.lock().await;
            if let Some(pooled) = clients.get(server_id) {
                return Ok(pooled.clone());
            }
        }

        let client = connect(server_id, transport).await?;
        let pooled = Arc::new(Pooled {
            client,
            concurrency: Semaphore::new(max_concurrency.max(1)),
        });

        let mut clients = self.clients.lock().await;
        // Another task may have connected the same server while this one was
        // spawning; keep whichever landed first rather than leaking a second
        // process.
        let pooled = clients
            .entry(server_id.to_owned())
            .or_insert(pooled)
            .clone();
        Ok(pooled)
    }

    pub async fn list_tools(
        &self,
        server_id: &str,
        transport: &Transport,
    ) -> Result<ListToolsResult, PoolError> {
        let pooled = self.client(server_id, transport, 1).await?;
        pooled
            .client
            .list_tools(Default::default())
            .await
            .map_err(|e| PoolError::Manifest {
                server_id: server_id.to_owned(),
                detail: e.to_string(),
            })
    }

    /// The tools a server advertises, in this crate's own vocabulary.
    ///
    /// `u2s-agent`'s `ToolDecl` needs each tool's description and input
    /// schema, and neither is stored in `mcp_tools` -- by design, since a
    /// schema belongs to the running server's manifest rather than to our
    /// registry. This is what a run calls once at start to get them,
    /// without `u2s-server` having to name an `rmcp` type.
    pub async fn list_advertised(
        &self,
        server_id: &str,
        transport: &Transport,
    ) -> Result<Vec<AdvertisedTool>, PoolError> {
        let listed = self.list_tools(server_id, transport).await?;
        Ok(listed
            .tools
            .into_iter()
            .map(|tool| AdvertisedTool {
                // An absent description becomes empty rather than a
                // placeholder: the conformance suite already fails a server
                // whose tool description is empty, so an empty string here
                // is a fact worth surfacing, not a gap worth papering over.
                description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: Value::Object((*tool.input_schema).clone()),
                name: tool.name.to_string(),
            })
            .collect())
    }

    /// Reads and JSON-parses the `u2s://manifest` resource. Parsing into a
    /// [`crate::manifest::ServerManifest`] is the caller's job — this stays
    /// at the `Value` level so a manifest that fails to parse is still
    /// available to report *why*.
    pub async fn read_manifest(
        &self,
        server_id: &str,
        transport: &Transport,
    ) -> Result<Value, PoolError> {
        let pooled = self.client(server_id, transport, 1).await?;
        let res = pooled
            .client
            .read_resource(ReadResourceRequestParams::new("u2s://manifest"))
            .await
            .map_err(|e| PoolError::Manifest {
                server_id: server_id.to_owned(),
                detail: e.to_string(),
            })?;

        let text = res
            .contents
            .first()
            .and_then(|c| match c {
                ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
                _ => None,
            })
            .ok_or_else(|| PoolError::Manifest {
                server_id: server_id.to_owned(),
                detail: "u2s://manifest did not return text content".to_owned(),
            })?;

        serde_json::from_str(&text).map_err(|e| PoolError::Manifest {
            server_id: server_id.to_owned(),
            detail: format!("manifest is not valid JSON: {e}"),
        })
    }

    /// Calls a tool, serialized against every other call to the same server
    /// by the concurrency permit taken here.
    pub async fn call_tool(
        &self,
        server_id: &str,
        transport: &Transport,
        max_concurrency: usize,
        tool: &str,
        args: Value,
    ) -> Result<CallToolResult, PoolError> {
        let pooled = self.client(server_id, transport, max_concurrency).await?;
        let _permit = pooled
            .concurrency
            .acquire()
            .await
            .expect("semaphore not closed");

        let mut params = CallToolRequestParams::new(tool.to_owned());
        if let Some(obj) = args.as_object() {
            params = params.with_arguments(obj.clone());
        }
        pooled
            .client
            .call_tool(params)
            .await
            .map_err(|e| PoolError::Call {
                server_id: server_id.to_owned(),
                tool: tool.to_owned(),
                detail: e.to_string(),
            })
    }

    /// Disconnects every pooled client. Idempotent connection failures during
    /// shutdown are swallowed — there is nothing useful to do with them, and
    /// a hung server should not stop the others from shutting down.
    pub async fn shutdown(&self) {
        let mut clients = self.clients.lock().await;
        for (_, pooled) in clients.drain() {
            if let Ok(pooled) = Arc::try_unwrap(pooled) {
                pooled.client.cancel().await.ok();
            }
            // A still-shared Arc (a call in flight) is left to drop and clean
            // up on its own; forcing a cancel out from under an in-flight
            // call would race the caller's own error handling.
        }
    }
}

async fn connect(server_id: &str, transport: &Transport) -> Result<McpClient, PoolError> {
    match transport {
        Transport::Stdio { command, args, env } => {
            let mut cmd = Command::new(command);
            cmd.args(args);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let process = TokioChildProcess::new(cmd).map_err(|e| PoolError::Connect {
                server_id: server_id.to_owned(),
                detail: format!("spawning: {e}"),
            })?;
            ().serve(process).await.map_err(|e| PoolError::Connect {
                server_id: server_id.to_owned(),
                detail: format!("initializing: {e}"),
            })
        }
        Transport::Http { .. } => Err(PoolError::HttpNotYetImplemented {
            server_id: server_id.to_owned(),
        }),
    }
}

/// A tool as its server advertises it, without `rmcp` in the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedTool {
    pub name: String,
    /// Empty when the server declared none.
    pub description: String,
    /// The JSON Schema the server declares for its arguments.
    pub input_schema: Value,
}

/// One inline image lifted out of a tool result's content block.
///
/// Exists so a caller can read an image a server returned inline without
/// naming an `rmcp` type: this crate is the workspace's only boundary onto
/// `rmcp`, and `u2s-server`'s normalizer needs exactly this much of it to
/// store a rendered page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage<'a> {
    /// Base64, as the protocol carries it. Decoding is the caller's job --
    /// `u2s_core::base64::decode` is the workspace's one decoder.
    pub data_base64: &'a str,
    pub media_type: &'a str,
}

/// One content block in this crate's own vocabulary.
///
/// The point of the enum is that `u2s-server` can read a tool result
/// without naming an `rmcp` type -- this crate is the workspace's only
/// boundary onto `rmcp`, enforced by `tests/genericity.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultBlock<'a> {
    Text(&'a str),
    Image(InlineImage<'a>),
    /// Audio, an embedded resource or a resource link. Named rather than
    /// silently mapped to text, so a caller decides what to do with a kind
    /// it did not expect instead of receiving a misleading empty string.
    Other,
}

/// Classifies one content block. The primitive; [`inline_image`] is the
/// convenience over it, so there is one match on `rmcp`'s enum.
pub fn classify_block(block: &rmcp::model::ContentBlock) -> ResultBlock<'_> {
    match block {
        rmcp::model::ContentBlock::Text(text) => ResultBlock::Text(&text.text),
        rmcp::model::ContentBlock::Image(image) => ResultBlock::Image(InlineImage {
            data_base64: &image.data,
            media_type: &image.mime_type,
        }),
        _ => ResultBlock::Other,
    }
}

/// The image in `block`, or `None` when the block is any other kind.
pub fn inline_image(block: &rmcp::model::ContentBlock) -> Option<InlineImage<'_>> {
    match classify_block(block) {
        ResultBlock::Image(image) => Some(image),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_transport_is_a_clear_not_yet_implemented_error() {
        let pool = McpClientPool::new();
        let transport = Transport::Http {
            url: "https://example.invalid".to_owned(),
        };
        let err = pool
            .list_tools("srv-http", &transport)
            .await
            .expect_err("HTTP is not implemented");
        assert!(matches!(err, PoolError::HttpNotYetImplemented { .. }));
    }

    #[tokio::test]
    async fn connecting_to_a_nonexistent_stdio_command_fails_with_a_pool_error() {
        let pool = McpClientPool::new();
        let transport = Transport::Stdio {
            command: PathBuf::from("/does/not/exist/u2s-fake-server"),
            args: vec![],
            env: vec![],
        };
        let err = pool
            .list_tools("srv-missing", &transport)
            .await
            .expect_err("must fail");
        assert!(matches!(err, PoolError::Connect { .. }));
    }
}
