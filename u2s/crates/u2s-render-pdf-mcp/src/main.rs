//! MCP stdio server over `u2s-render-pdf`.
//!
//! Two conventions from the parent design are load-bearing here:
//!
//! * **Failures are tool results, not protocol errors.** Everything the caller
//!   could act on — a bad page number, an encrypted file — comes back as
//!   `CallToolResult::error` so the model reads the message. `Err(McpError)` is
//!   reserved for a request that cannot be routed at all.
//! * **Large payloads never enter the context.** An image over the inline
//!   threshold is written to the blob directory and referenced by handle.

pub mod specs;

use std::borrow::Cow;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_render_pdf::{
    BlobStore, ImageFormat, Limits, Pattern, RectPt, RenderError, RenderedPage, Renderer,
};

const MANIFEST_URI: &str = "u2s://manifest";

#[derive(Clone)]
pub struct PdfRenderServer {
    renderer: Renderer,
    blobs: Arc<BlobStore>,
    max_inline_bytes: usize,
}

impl PdfRenderServer {
    fn new() -> Result<Self, RenderError> {
        Self::with_parts(Limits::from_env(), BlobStore::from_env())
    }

    /// [`Self::new`] for in-process hosts: limits and blob store come from the
    /// caller.
    pub fn with_parts(limits: Limits, blobs: BlobStore) -> Result<Self, RenderError> {
        let max_inline_bytes = limits.max_inline_bytes;
        Ok(PdfRenderServer {
            renderer: Renderer::start(limits)?,
            blobs: Arc::new(blobs),
            max_inline_bytes,
        })
    }

    /// Turn a rendered page into MCP content: the image itself when it is small
    /// enough to be worth putting in front of a model, a handle when it is not.
    fn page_content(&self, page: &RenderedPage) -> Result<(Vec<ContentBlock>, Value), RenderError> {
        let mut meta = json!({
            "page": page.page,
            "width_px": page.width_px,
            "height_px": page.height_px,
            "dpi_effective": page.dpi_effective,
            "mime": page.mime,
            "byte_len": page.data.len(),
        });

        if page.data.len() <= self.max_inline_bytes {
            meta["inline"] = json!(true);
            let block = ContentBlock::image(B64.encode(&page.data), page.mime.to_string());
            Ok((vec![block], meta))
        } else {
            let ext = if page.mime == "image/png" {
                "png"
            } else {
                "jpg"
            };
            let blob = self.blobs.put(&page.data, page.mime, ext)?;
            meta["inline"] = json!(false);
            meta["blob"] = json!({
                "handle": blob.handle,
                "path": blob.path.display().to_string(),
                "media_type": blob.media_type,
                "byte_len": blob.byte_len,
                "digest": blob.digest,
            });
            let block = ContentBlock::text(format!(
                "page {} rendered to blob {} ({} bytes, {}) — too large to inline",
                page.page, blob.handle, blob.byte_len, blob.media_type
            ));
            Ok((vec![block], meta))
        }
    }

    pub fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, RenderError> {
        let path = arg_str(args, "doc_path")?;
        match name {
            "pdf_info" => {
                let info = self.renderer.info(&path)?;
                let mut value = serde_json::to_value(&info)
                    .map_err(|e| RenderError::backend("server", format!("serialize info: {e}")))?;
                // The `info` ingest capability's `applicable` field: `false`
                // means "I can answer, but I am the wrong handler" -- this
                // server's own XFA shim page, which the routing signal above
                // already exists to name. Merged onto the existing response
                // rather than a new field on `DocumentInfo`, since it is a
                // property of the ingest *contract*, not of the document.
                value["applicable"] = json!(!info.form_type.is_xfa());
                Ok(CallToolResult::structured(value))
            }

            "pdf_render_page" => {
                let (page, warning) = self.renderer.render_page(
                    &path,
                    arg_u32(args, "page")?,
                    arg_f32(args, "dpi"),
                    arg_u32_opt(args, "max_edge_px"),
                    ImageFormat::parse(args.get("format").and_then(Value::as_str))?,
                )?;
                let (content, mut meta) = self.page_content(&page)?;
                if let Some(w) = warning {
                    meta["warning"] = json!(w);
                }
                Ok(with_structured(CallToolResult::success(content), meta))
            }

            "pdf_render_pages" => {
                let pages = args.get("pages").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_u64())
                        .map(|v| v as u32)
                        .collect::<Vec<_>>()
                });
                let batch = self.renderer.render_pages(
                    &path,
                    pages,
                    arg_u32_opt(args, "from"),
                    args.get("limit")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                    arg_f32(args, "dpi"),
                    arg_u32_opt(args, "max_edge_px"),
                    ImageFormat::parse(args.get("format").and_then(Value::as_str))?,
                )?;

                let mut blocks = Vec::new();
                let mut images = Vec::new();
                for page in &batch.pages {
                    let (mut content, meta) = self.page_content(page)?;
                    blocks.append(&mut content);
                    images.push(meta);
                }
                let mut meta = json!({
                    "rendered": batch.pages.iter().map(|p| p.page).collect::<Vec<_>>(),
                    "next_from": batch.next_from,
                    "budget_hit": batch.budget_hit,
                    "images": images,
                });
                if let Some(w) = batch.warning {
                    meta["warning"] = json!(w);
                }
                Ok(with_structured(CallToolResult::success(blocks), meta))
            }

            "pdf_render_region" => {
                let rect = args.get("rect_pt").ok_or_else(|| {
                    RenderError::backend("server", "missing required argument rect_pt")
                })?;
                let rect = RectPt {
                    x: arg_f32(rect, "x").unwrap_or(0.0),
                    y: arg_f32(rect, "y").unwrap_or(0.0),
                    width: arg_f32(rect, "width").ok_or_else(|| {
                        RenderError::backend("server", "rect_pt.width is required")
                    })?,
                    height: arg_f32(rect, "height").ok_or_else(|| {
                        RenderError::backend("server", "rect_pt.height is required")
                    })?,
                };
                let page = self.renderer.render_region(
                    &path,
                    arg_u32(args, "page")?,
                    rect,
                    arg_f32(args, "dpi"),
                    ImageFormat::parse(args.get("format").and_then(Value::as_str))?,
                )?;
                let (content, meta) = self.page_content(&page)?;
                Ok(with_structured(CallToolResult::success(content), meta))
            }

            "pdf_page_text" => {
                let text = self.renderer.page_text(
                    &path,
                    arg_u32(args, "page")?,
                    args.get("offset")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                    args.get("limit")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                )?;
                let value = serde_json::to_value(&text)
                    .map_err(|e| RenderError::backend("server", format!("serialize text: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "pdf_search_text" => {
                let search = self.renderer.search_text(
                    &path,
                    arg_u32_opt(args, "from"),
                    pattern_of(args)?,
                    args.get("limit")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                )?;
                let value = serde_json::to_value(&search).map_err(|e| {
                    RenderError::backend("server", format!("serialize search: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            other => Err(RenderError::backend(
                "server",
                format!("unknown tool {other}"),
            )),
        }
    }
}

/// Attach structured metadata to a content-bearing result. `CallToolResult`'s
/// own `structured` constructor replaces content, which would throw the image
/// away.
fn with_structured(mut result: CallToolResult, value: Value) -> CallToolResult {
    result.structured_content = Some(value);
    result
}

/// Compile the caller's query once, here at the edge, so nothing downstream
/// can hold an invalid pattern. A bad regex is the caller's fault, so it is
/// reported as a bad argument rather than as a pdfium failure.
fn pattern_of(args: &Value) -> Result<Pattern, RenderError> {
    let query = arg_str(args, "query")?;
    let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    Pattern::parse(&query, regex).map_err(|detail| RenderError::invalid_argument("query", detail))
}

fn arg_str(args: &Value, key: &str) -> Result<String, RenderError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RenderError::backend("server", format!("missing required argument {key}")))
}

fn arg_u32(args: &Value, key: &str) -> Result<u32, RenderError> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .ok_or_else(|| RenderError::backend("server", format!("missing required argument {key}")))
}

fn arg_u32_opt(args: &Value, key: &str) -> Option<u32> {
    args.get(key).and_then(Value::as_u64).map(|v| v as u32)
}

fn arg_f32(args: &Value, key: &str) -> Option<f32> {
    args.get(key).and_then(Value::as_f64).map(|v| v as f32)
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

impl ServerHandler for PdfRenderServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("u2s-render-pdf-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Renders ordinary PDF pages to images with pdfium.\n\n\
             Call `pdf_info` first. If it reports form_type xfa_full or xfa_foreground the \
             document is an XFA form and this server can only produce its static shim page \
             — use the XFA renderer for those.\n\n\
             For multi-page work use `pdf_render_pages` and follow `next_from` until it is \
             null. Images larger than the inline threshold come back as blob handles rather \
             than inline data.\n\n\
             To locate something rather than read everything, use `pdf_search_text`: it greps \
             every page's text and hands back each match's page, offset and length, which \
             `pdf_page_text` takes directly. Search to find the page, then read or render \
             only that page."
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

        // Blocking pdfium work must not sit on the async runtime's worker.
        let this = self.clone();
        let name_owned = name.to_string();
        let result = tokio::task::spawn_blocking(move || this.dispatch(&name_owned, &args))
            .await
            .map_err(|e| McpError::internal_error(format!("render task panicked: {e}"), None))?;

        Ok(match result {
            Ok(ok) => ok.into(),
            // Every failure the caller could act on is a tool error, so the
            // message reaches the model rather than being rendered opaquely.
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Bind pdfium before serving. A render server that starts without its
    // renderer produces confusing per-call errors forever; refusing to start
    // produces one clear error once.
    let server = match PdfRenderServer::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("u2s-render-pdf-mcp: cannot start: {e}");
            eprintln!(
                "hint: run scripts/fetch-pdfium.sh, or set PDFIUM_LIB_PATH to the directory \
                 containing the pdfium dynamic library"
            );
            std::process::exit(2);
        }
    };

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
