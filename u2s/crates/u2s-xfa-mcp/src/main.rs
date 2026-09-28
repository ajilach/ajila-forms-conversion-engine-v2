//! MCP stdio server over the raw `/XFA` data inside a PDF: packets, windowed
//! text reads, search, and a structural outline — everything a caller needs
//! to *read* an XFA form without laying it out or running a single script.
//!
//! This is deliberately not the render server. `u2s-render-xfa-mcp` answers
//! "what does this form look like"; this one answers "what does this form
//! actually say" — cheaper, font-free, and useful even on a machine that
//! cannot render anything. Both happen to use an `xfa_` tool prefix, but
//! their tool names never collide (`xfa_packets`/`xfa_read`/`xfa_search`/
//! `xfa_outline`/`xfa_node` here vs `xfa_info`/`xfa_render_page`/… there), so
//! registering both at once is safe.
//!
//! The same convention as every other u2s server: failures the caller could
//! act on are tool errors, not protocol errors, and nothing here ever dumps a
//! whole document — every read is windowed, bounded, and honest about what it
//! left out.

pub mod specs;

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_render_core::{Grep, Pattern, RenderError, window_chars};
use u2s_xfa::query;
use u2s_xfa::{XfaNode, XfaPacket, extract_xfa_packets};

const MANIFEST_URI: &str = "u2s://manifest";

/// Characters of context either side of a search match. Matches the render
/// servers' `Limits::search_context_radius`, so the two surfaces agree.
const CONTEXT_RADIUS: usize = 80;

#[derive(Clone)]
pub struct XfaDataServer;

impl XfaDataServer {
    fn packets_of(&self, doc_path: &str) -> Result<Vec<XfaPacket>, RenderError> {
        let bytes = std::fs::read(doc_path).map_err(|source| RenderError::Io {
            path: doc_path.to_string(),
            source,
        })?;
        extract_xfa_packets(&bytes)
            .map_err(|e| RenderError::backend("xfa", e.to_string()))?
            .ok_or_else(|| RenderError::UnsupportedInput {
                path: doc_path.to_string(),
                detail: "not an XFA form — no /XFA in the AcroForm dictionary".to_string(),
            })
    }

    /// The named packet, or the first one when no name is given. Errors name
    /// every known packet so a caller can retry without another round trip.
    fn packet_by_name<'a>(
        &self,
        packets: &'a [XfaPacket],
        name: Option<&str>,
    ) -> Result<&'a XfaPacket, RenderError> {
        match name {
            Some(n) => packets.iter().find(|p| p.name == n).ok_or_else(|| {
                let known: Vec<&str> = packets.iter().map(|p| p.name.as_str()).collect();
                RenderError::backend(
                    "xfa",
                    format!("no packet {n:?} — known: {known:?} (run xfa_packets)"),
                )
            }),
            None => packets
                .first()
                .ok_or_else(|| RenderError::backend("xfa", "document has no XFA packets")),
        }
    }

    /// Every packet parses to its own root — the concatenation of packet
    /// fragments is not one well-formed document, so `XfaNode::parse` returns
    /// one root per packet, in declaration order.
    fn roots_of(&self, packets: &[XfaPacket]) -> Result<Vec<XfaNode>, RenderError> {
        let concatenated: Vec<u8> = packets.iter().flat_map(|p| p.content.clone()).collect();
        XfaNode::parse(&concatenated)
            .map_err(|e| RenderError::backend("xfa", format!("XFA parse: {e}")))
    }

    pub fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, RenderError> {
        let path = arg_str(args, "doc_path")?;
        match name {
            "xfa_packets" => {
                let packets = self.packets_of(&path)?;
                let list: Vec<Value> = packets
                    .iter()
                    .map(|p| json!({ "name": p.name, "byte_len": p.content.len() }))
                    .collect();
                let count = list.len();
                Ok(CallToolResult::structured(
                    json!({ "packets": list, "count": count }),
                ))
            }

            "xfa_read" => {
                let packets = self.packets_of(&path)?;
                let packet =
                    self.packet_by_name(&packets, args.get("packet").and_then(Value::as_str))?;
                let text = String::from_utf8_lossy(&packet.content);
                let offset = arg_u64_opt(args, "offset").unwrap_or(0) as usize;
                let limit = arg_u64_opt(args, "limit").unwrap_or(4000) as usize;
                let w = window_chars(&text, offset, limit);
                Ok(CallToolResult::structured(json!({
                    "packet": packet.name,
                    "text": w.text,
                    "offset": w.offset,
                    "total_chars": w.total_chars,
                    "truncated": w.truncated,
                })))
            }

            "xfa_search" => {
                let packets = self.packets_of(&path)?;
                let query_str = arg_str(args, "query")?;
                let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
                let limit = arg_u64_opt(args, "limit").unwrap_or(50) as usize;
                let requested = args.get("packet").and_then(Value::as_str);
                let selected: Vec<&XfaPacket> = match requested {
                    Some(n) => vec![self.packet_by_name(&packets, Some(n))?],
                    None => packets.iter().collect(),
                };

                // A bad pattern is the caller's, not the engine's, so it is
                // converted at the edge and named as an argument fault.
                let pattern = Pattern::parse(&query_str, regex)
                    .map_err(|detail| RenderError::invalid_argument("query", detail))?;

                // `Grep` carries the cap and the running total across packets:
                // once the cap is spent, later packets are still scanned (for
                // an honest total_matches) but add no further entries.
                let mut grep = Grep::new(pattern, limit, CONTEXT_RADIUS);
                let mut matches = Vec::new();
                for p in &selected {
                    let text = String::from_utf8_lossy(&p.content);
                    for m in grep.scan(&text) {
                        matches.push(json!({
                            "packet": p.name,
                            "offset": m.offset,
                            "length": m.length,
                            "context": m.context,
                        }));
                    }
                }
                Ok(CallToolResult::structured(json!({
                    "matches": matches,
                    "total_matches": grep.total_matches(),
                    "truncated": grep.truncated(),
                })))
            }

            "xfa_outline" => {
                let packets = self.packets_of(&path)?;
                let roots = self.roots_of(&packets)?;
                let max_depth = arg_u64_opt(args, "max_depth").unwrap_or(10) as usize;
                let limit = arg_u64_opt(args, "limit").unwrap_or(200) as usize;
                let out = query::outline(&roots, max_depth, limit);
                let value = serde_json::to_value(&out).map_err(|e| {
                    RenderError::backend("server", format!("serialize outline: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_node" => {
                let packets = self.packets_of(&path)?;
                let roots = self.roots_of(&packets)?;
                let node_path = arg_str(args, "path")?;
                let info = query::node_at(&roots, &node_path).ok_or_else(|| {
                    RenderError::backend(
                        "xfa",
                        format!("no node at path {node_path:?} (run xfa_outline)"),
                    )
                })?;
                let value = serde_json::to_value(&info)
                    .map_err(|e| RenderError::backend("server", format!("serialize node: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            other => Err(RenderError::backend(
                "server",
                format!("unknown tool {other}"),
            )),
        }
    }
}

fn arg_str(args: &Value, key: &str) -> Result<String, RenderError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RenderError::backend("server", format!("missing required argument {key}")))
}

fn arg_u64_opt(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn to_mcp_tool(spec: &Value) -> Option<Tool> {
    let name = spec.get("name")?.as_str()?.to_string();
    let description = spec
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The spec files say `input_schema`; MCP wants `inputSchema`. The rename
    // happens here and only here.
    let schema = spec.get("input_schema")?.as_object()?.clone();
    Some(Tool::new(name, description, Arc::new(schema)))
}

impl ServerHandler for XfaDataServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("u2s-xfa-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Reads the raw /XFA data inside a PDF — packets, text, structure — with no \
             layout and no scripting. Call `xfa_packets` first to see what packets exist \
             (usually `template`, `datasets`, `config`); every other tool defaults to the \
             first packet when `packet` is omitted. Use `xfa_outline`/`xfa_node` to browse \
             structure, `xfa_read` to quote exact text, `xfa_search` to find where \
             something lives before reading it. A document with no XFA at all is a tool \
             error naming pdf_info — use the PDF or XFA render server for the actual form."
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
                .with_description("Roles, format scope, contract version and conformance vectors.")
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
        let args = Value::Object(request.arguments.unwrap_or_default());

        // Parsing and search on a large minified packet is real CPU work; it
        // must not sit on the async runtime's worker.
        let this = self.clone();
        let name_owned = name.to_string();
        let result = tokio::task::spawn_blocking(move || this.dispatch(&name_owned, &args))
            .await
            .map_err(|e| McpError::internal_error(format!("task panicked: {e}"), None))?;

        Ok(match result {
            Ok(ok) => ok.into(),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = XfaDataServer;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
