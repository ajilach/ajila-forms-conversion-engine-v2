//! MCP stdio server for the `aem-ubs` output format.
//!
//! **An output format IS an MCP server.** PLAN.md: "There is no
//! hand-maintained format table... registering the server registers the
//! format: schema, description and encoder arrive together, or not at
//! all." This binary is that registration surface for UBS AEM.
//!
//! Three tools, all output-side only:
//!
//! - `decode` -- the format's `decode`-role tool, `u2s_mcp::manifest::ToolRole::Decode`'s
//!   exact contract: a FileVault package blob (or path, for conformance
//!   vectors) in, an `output_json` blob out. `encode`'s exact inverse --
//!   seeding a reference run from a real, delivered package (rather than
//!   hand-typed JSON) is what this tool exists for.
//! - `encode` -- the format's `encode`-role tool: `output_json` in, a
//!   FileVault package blob out. All the actual work lives in
//!   `u2s-mapper-aem`, which is a mechanical mapper with no business
//!   logic of its own (see that crate's module doc for why).
//! - `fragment_search` -- a `query`-role tool letting the Conversion Agent
//!   browse the fragment library itself. This is deliberately **not**
//!   part of `encode`: matching a fragment to a panel is a judgment call,
//!   and this workspace's rule sandbox has no I/O to make that call
//!   either, so the choice belongs to the agent, not to Rust running
//!   inside this server.
//!
//! This crate is the **only** one that may name an AEM type. `u2s-server`
//! never links it: the server is registered at runtime and its schema
//! comes back as JSON in a database column, which is precisely the
//! structural argument `crates/u2s-server/tests/genericity.rs` enforces.

mod specs;

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_blob::BlobStore;
use u2s_mapper_aem::fragment_library::FragmentLibrary;

const MANIFEST_URI: &str = "u2s://manifest";

#[derive(Clone)]
struct AemUbsServer {
    blobs: Arc<BlobStore>,
    fragments: Arc<FragmentLibrary>,
}

impl AemUbsServer {
    /// `U2S_AEM_FRAGMENT_DIR` unset means "no fragment library for this
    /// deployment" -- a legitimate state (a fresh dataset with no fragment
    /// corpus yet), not an error, so the server still starts and
    /// `fragment_search` simply returns no hits. A directory that *is*
    /// configured but cannot be scanned is a misconfiguration and refuses
    /// to start, the same discipline `U2S_FONT_DIR` already applies
    /// elsewhere in this workspace: a confusing per-call error forever is
    /// worse than one clear error at startup.
    fn new() -> Result<Self, String> {
        let fragments = match std::env::var("U2S_AEM_FRAGMENT_DIR") {
            Ok(dir) => FragmentLibrary::scan(std::path::Path::new(&dir))
                .map_err(|err| format!("cannot start: {err}"))?,
            Err(_) => FragmentLibrary::empty(),
        };
        Ok(Self {
            blobs: Arc::new(BlobStore::from_env()),
            fragments: Arc::new(fragments),
        })
    }

    fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, String> {
        match name {
            "decode" => self.decode(args),
            "encode" => self.encode(args),
            "fragment_search" => self.fragment_search(args),
            other => Err(format!("no tool named {other:?}")),
        }
    }

    /// `u2s_mcp::manifest::ToolRole::Decode`'s own contract: exactly one of
    /// `artifact_blob`/`artifact_path`, a blob handle back on success
    /// (never the document inline -- a decoded real-world form is easily
    /// 300-500 KB of JSON), a tool error naming what could not be
    /// represented on failure. `u2s_mapper_aem::decode::decode` is
    /// already lossless-or-error by construction (see its own module doc),
    /// so this function adds no judgment of its own -- it only moves bytes
    /// in and out of the blob store around that call.
    fn decode(&self, args: &Value) -> Result<CallToolResult, String> {
        let blob_handle = args.get("artifact_blob").and_then(Value::as_str);
        let path = args.get("artifact_path").and_then(Value::as_str);

        let bytes = match (blob_handle, path) {
            (Some(handle), _) => self
                .blobs
                .get(handle)
                .map_err(|err| format!("could not read artifact_blob {handle}: {err}"))?,
            (None, Some(path)) => std::fs::read(path)
                .map_err(|err| format!("could not read artifact_path {path}: {err}"))?,
            (None, None) => {
                return Err(
                    "missing required argument: exactly one of artifact_blob or artifact_path"
                        .to_owned(),
                );
            }
        };

        let form = u2s_mapper_aem::decode::decode(&bytes)
            .map_err(|err| format!("could not decode the package: {err}"))?;

        let output_json = serde_json::to_vec(form.form())
            .map_err(|err| format!("could not serialize the decoded document: {err}"))?;

        let blob = self
            .blobs
            .put(&output_json, "application/json", "json")
            .map_err(|err| format!("could not store the decoded document: {err}"))?;

        Ok(CallToolResult::structured(json!({
            "output_json": {
                "handle": blob.handle,
                "byte_len": blob.byte_len,
                "digest": blob.digest,
            },
            // Not yet instrumented: `decode` either fully succeeds
            // (everything the source carried is now either a typed field
            // or `Passthrough`) or returns a tool error above, so there is
            // no partial state a per-property count would describe today.
            "notes": [],
            "coverage": { "properties_total": 0, "modelled": 0, "carried": 0 },
            "format_version": specs::FORMAT_VERSION,
        })))
    }

    fn encode(&self, args: &Value) -> Result<CallToolResult, String> {
        let output_json = args
            .get("output_json")
            .ok_or_else(|| "missing required argument output_json".to_owned())?;

        let form = u2s_aem::model::AemForm::from_json(output_json)
            .map_err(|err| format!("output_json does not match the aem-ubs schema: {err}"))?;
        let valid = form.validate().map_err(|violations| {
            let detail = violations
                .iter()
                .map(|v| format!("{}: {}", v.pointer, v.message))
                .collect::<Vec<_>>()
                .join("; ");
            format!("the document fails aem-ubs semantic validation: {detail}")
        })?;

        let package = u2s_mapper_aem::encode(&valid)
            .map_err(|err| format!("could not encode the package: {err}"))?;

        let blob = self
            .blobs
            .put(&package.bytes, package.media_type, "zip")
            .map_err(|err| format!("could not store the package: {err}"))?;

        let meta = json!({
            "blob": {
                "handle": blob.handle,
                "media_type": blob.media_type,
                "byte_len": blob.byte_len,
                "digest": blob.digest,
            }
        });
        let text = format!(
            "encoded to blob {} ({} bytes, {})",
            blob.handle, blob.byte_len, blob.media_type
        );
        Ok(with_structured(
            CallToolResult::success(vec![ContentBlock::text(text)]),
            meta,
        ))
    }

    fn fragment_search(&self, args: &Value) -> Result<CallToolResult, String> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument query".to_owned())?;

        let hits: Vec<Value> = self
            .fragments
            .search(query)
            .into_iter()
            .map(|hit| {
                json!({
                    "frag_ref": hit.frag_ref,
                    "title": hit.title,
                    "preview": hit.preview,
                })
            })
            .collect();
        Ok(CallToolResult::structured(json!({ "hits": hits })))
    }
}

fn with_structured(mut result: CallToolResult, value: Value) -> CallToolResult {
    result.structured_content = Some(value);
    result
}

fn to_mcp_tool(spec: &Value) -> Option<Tool> {
    let name = spec.get("name")?.as_str()?.to_string();
    let description = spec
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The spec files say `input_schema`; MCP wants `inputSchema`. The rename
    // happens here and only here, as in every other u2s server.
    let schema = spec.get("input_schema")?.as_object()?.clone();
    Some(Tool::new(name, description, Arc::new(schema)))
}

impl ServerHandler for AemUbsServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("u2s-aem-ubs-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Defines the `aem-ubs` output format. Its JSON Schema and description live in \
             the `u2s://manifest` resource, under `format`; read that to learn the shape a \
             conversion must produce. `encode` lowers a finished, valid document into a \
             FileVault package; `decode` is its exact inverse, for seeding a reference run \
             from a real, delivered package instead of hand-typed JSON; `fragment_search` \
             lets you browse the fragment library yourself before referencing one in a \
             Fragment node -- neither this server nor any rule script judges which fragment \
             fits, that is your call to make."
                .to_string(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = specs::tool_specs().iter().filter_map(to_mcp_tool).collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(MANIFEST_URI, "u2s.manifest")
                .with_description(
                    "The aem-ubs format module (key, version, description, JSON Schema), \
                     plus roles, format scope and contract version.",
                )
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != MANIFEST_URI {
            return Err(McpError::resource_not_found(
                format!("unknown resource {}", request.uri),
                None,
            ));
        }
        let body = serde_json::to_string_pretty(&specs::manifest())
            .map_err(|e| McpError::internal_error(format!("manifest: {e}"), None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(body, MANIFEST_URI).with_mime_type("application/json"),
        ])
        .into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name: Cow<'static, str> = request.name.clone();
        let args = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Object(Default::default()));

        // A tool error, never a protocol error, for every failure the
        // caller could act on -- so a refusal reads as "this call was
        // wrong" rather than "this server is broken".
        Ok(match self.dispatch(&name, &args) {
            Ok(result) => result,
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
        .into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = match AemUbsServer::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("u2s-aem-ubs-mcp: cannot start: {e}");
            eprintln!("hint: check U2S_AEM_FRAGMENT_DIR points at a readable directory");
            std::process::exit(2);
        }
    };

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
