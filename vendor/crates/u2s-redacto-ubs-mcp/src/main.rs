//! MCP stdio server for the `redacto-ubs` output format.
//!
//! **An output format IS an MCP server.** PLAN.md: "There is no
//! hand-maintained format table... registering the server registers the
//! format: schema, description and encoder arrive together, or not at
//! all." This binary is that registration surface for UBS Redacto, the
//! same role `u2s-aem-ubs-mcp` plays for `aem-ubs` -- three tools, all
//! output-side only:
//!
//! - `decode` -- the format's `decode`-role tool: a real, delivered
//!   `INSERT` script in, an `output_json` blob out. `encode`'s exact
//!   inverse -- seeding a reference run from a real dump rather than
//!   hand-typed JSON is what this tool exists for.
//! - `encode` -- the format's `encode`-role tool: `output_json` in, the
//!   platform's own transactional `INSERT` script out. All the actual work
//!   lives in `u2s-mapper-redacto`, a mechanical mapper with no business
//!   logic of its own (see that crate's module doc for why).
//! - `style_search` -- a `query`-role tool letting the Conversion Agent
//!   browse the CSS class vocabulary itself, mirroring
//!   `u2s-aem-ubs-mcp`'s own `fragment_search`: which class fits a panel
//!   is a judgment call neither this server nor the rule sandbox makes.
//!
//! This crate is the **only** one that may name a Redacto type, alongside
//! `u2s-redacto`/`u2s-mapper-redacto`/`u2s-redacto-verify-core`/
//! `u2s-redacto-ubs-verify-mcp`. `u2s-server` never links it: the server is
//! registered at runtime and its schema comes back as JSON in a database
//! column, the same structural argument `crates/u2s-server/tests/genericity.rs`
//! already enforces for the AEM side.

mod search;
mod specs;
mod style_catalog;

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use style_catalog::StyleCatalog;
use u2s_blob::BlobStore;

const MANIFEST_URI: &str = "u2s://manifest";

#[derive(Clone)]
struct RedactoUbsServer {
    blobs: Arc<BlobStore>,
    styles: Arc<StyleCatalog>,
}

impl RedactoUbsServer {
    /// `U2S_REDACTO_STYLE_DIR` unset means "no tenant stylesheet directory
    /// for this deployment" -- a legitimate state (the published vocabulary
    /// is still fully searchable), not an error, so the server still
    /// starts. A directory that *is* configured but cannot be scanned is a
    /// misconfiguration and refuses to start, the same discipline
    /// `U2S_AEM_FRAGMENT_DIR`/`U2S_FONT_DIR` already apply elsewhere in
    /// this workspace: a confusing per-call error forever is worse than
    /// one clear error at startup.
    fn new() -> Result<Self, String> {
        let styles = match std::env::var("U2S_REDACTO_STYLE_DIR") {
            Ok(dir) => StyleCatalog::scan(std::path::Path::new(&dir))
                .map_err(|err| format!("cannot start: {err}"))?,
            Err(_) => StyleCatalog::published_only(),
        };
        log::info!(
            "style catalog ready: {} classes ({})",
            styles.len(),
            if styles.is_empty() { "empty" } else { "including the published vocabulary" }
        );
        Ok(Self {
            blobs: Arc::new(BlobStore::from_env()),
            styles: Arc::new(styles),
        })
    }

    fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, String> {
        match name {
            "decode" => self.decode(args),
            "encode" => self.encode(args),
            "style_search" => self.style_search(args),
            other => Err(format!("no tool named {other:?}")),
        }
    }

    /// `u2s_mcp::manifest::ToolRole::Decode`'s own contract: exactly one of
    /// `artifact_blob`/`artifact_path`, a blob handle back on success, a
    /// tool error naming what could not be represented on failure.
    /// `u2s_mapper_redacto::decode::decode` is already lossless-or-error by
    /// construction (see that module's own doc), so this function adds no
    /// judgment of its own -- it only moves bytes in and out of the blob
    /// store around that call.
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

        let doc = u2s_mapper_redacto::decode::decode(&bytes)
            .map_err(|err| format!("could not decode the dump: {err}"))?;

        let output_json = serde_json::to_vec(doc.document())
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
            "notes": [
                "master_language is recomputed, not recovered from the source dump",
                "every asset's own key is an invented label, stable within this decode only"
            ],
            "coverage": { "properties_total": 0, "modelled": 0, "carried": 0 },
            "format_version": specs::FORMAT_VERSION,
        })))
    }

    fn encode(&self, args: &Value) -> Result<CallToolResult, String> {
        let output_json = args
            .get("output_json")
            .ok_or_else(|| "missing required argument output_json".to_owned())?;

        let doc = u2s_redacto::RedactoDocument::from_json(output_json)
            .map_err(|err| format!("output_json does not match the redacto-ubs schema: {err}"))?;
        let valid = doc.validate().map_err(|violations| {
            let detail = violations
                .iter()
                .map(|v| format!("{}: {}", v.pointer, v.message))
                .collect::<Vec<_>>()
                .join("; ");
            format!("the document fails redacto-ubs semantic validation: {detail}")
        })?;

        let dump = u2s_mapper_redacto::encode(&valid)
            .map_err(|err| format!("could not encode the dump: {err}"))?;

        let blob = self
            .blobs
            .put(&dump.bytes, dump.media_type, "sql")
            .map_err(|err| format!("could not store the dump: {err}"))?;

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

    fn style_search(&self, args: &Value) -> Result<CallToolResult, String> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument query".to_owned())?;

        let hits: Vec<Value> = self
            .styles
            .search(query)
            .into_iter()
            .map(|hit| {
                json!({
                    "class": hit.class,
                    "kind": hit.kind_str(),
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

impl ServerHandler for RedactoUbsServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("u2s-redacto-ubs-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Defines the `redacto-ubs` output format. Its JSON Schema and description live in \
             the `u2s://manifest` resource, under `format`; read that to learn the shape a \
             conversion must produce. `encode` lowers a finished, valid document into the \
             platform's own transactional INSERT script; `decode` is its (mostly) exact \
             inverse, for seeding a reference run from a real, delivered dump instead of \
             hand-typed JSON; `style_search` lets you browse the CSS class vocabulary yourself \
             before referencing one on a styledPanel or in furniture -- neither this server nor \
             any rule script judges which class fits, that is your call to make."
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
                    "The redacto-ubs format module (key, version, description, JSON Schema), \
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

    let server = match RedactoUbsServer::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("u2s-redacto-ubs-mcp: cannot start: {e}");
            eprintln!("hint: check U2S_REDACTO_STYLE_DIR points at a readable directory");
            std::process::exit(2);
        }
    };

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
