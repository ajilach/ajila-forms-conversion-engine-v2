//! MCP stdio server over `u2s-render-xfa`.
//!
//! Same conventions as the PDF server: failures the caller could act on are
//! tool results rather than protocol errors, so the model reads the message;
//! and anything too large to put in front of a model becomes a blob handle.
//!
//! The boot-time gate differs. The PDF server refuses to start without pdfium;
//! this one refuses without fonts, because the layout engine degrades
//! *silently* when it cannot measure text — producing wrong heights and
//! therefore wrong page breaks rather than an error.
//!
//! Sessions are the interaction surface (`xfa_open`, `xfa_set`, `xfa_reset`,
//! `xfa_close`): an agent opens a document once, sets one control at a time
//! the way a person would, and watches what the form did in response,
//! rather than addressing a point in the state space and never seeing the
//! form react. `doc_path` plus the one-shot `state` argument survives on
//! every read tool as the stateless shorthand the normalizer and a
//! single-call conformance vector both need — see `Target`'s own doc in
//! `u2s-render-xfa` for why the two are additive rather than one replacing
//! the other.
//!
//! The server is this library; `src/main.rs` serves it over stdio, configured
//! from the environment (`new`). A host can run it in-process instead, with
//! `with_parts` taking the configuration as values and `dispatch` running a
//! tool.

pub mod specs;

use std::borrow::Cow;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use u2s_render_core::args::{
    InfoArgs, PageTextArgs, RenderPageArgs, RenderPagesArgs, RenderRegionArgs, SearchTextArgs,
    ServerArgs, parse_args,
};
use u2s_render_core::document::DocumentArg;
use u2s_render_xfa::RenderOptions;
use u2s_render_xfa::{BlobStore, Limits, RenderError, RenderedPage, Renderer, Target, fonts};

use crate::specs::{
    ClickArgs, CloseArgs, ControlsArgs, FieldArgs, ResetArgs, SetArgs, StateArg, XfaArgs,
    XfaDocument, XfaTarget,
};

const MANIFEST_URI: &str = "u2s://manifest";

/// How many controls `xfa_controls` returns when no `limit` is given, and
/// the most it returns whatever `limit` asks for: a large form has several
/// hundred fields, far more than one response should carry.
const CONTROLS_DEFAULT_LIMIT: usize = 100;
const CONTROLS_MAX_LIMIT: usize = 500;

#[derive(Clone)]
pub struct XfaRenderServer {
    renderer: Renderer,
    blobs: Arc<BlobStore>,
    max_inline_bytes: usize,
}

impl XfaRenderServer {
    pub fn new() -> Result<Self, RenderError> {
        // Fonts first: without them the engine lays text out wrongly rather
        // than refusing, so a server that starts fontless is a server that
        // quietly produces wrong page counts.
        fonts::register_from_env().map_err(|e| RenderError::EngineUnavailable {
            engine: "xfa",
            searched: vec![e.to_string()],
        })?;

        Self::with_parts(Limits::from_env(), BlobStore::from_env()?)
    }

    /// [`Self::new`] for in-process hosts: limits and blob store come from the
    /// caller, and so do the fonts, which the caller registers first
    /// (`u2s_xfa::fonts::register_dir_once`, or the font manager directly). Refuses
    /// to start without a fallback font, for the reason [`Self::new`] does.
    pub fn with_parts(limits: Limits, blobs: BlobStore) -> Result<Self, RenderError> {
        if !fonts::fallback_registered() {
            return Err(RenderError::EngineUnavailable {
                engine: "xfa",
                searched: vec![
                    "no fallback font is registered: register the fonts before \
                                creating the server"
                        .into(),
                ],
            });
        }
        let max_inline_bytes = limits.max_inline_bytes;
        Ok(XfaRenderServer {
            // Open text fields are underlined so a model reading the image
            // can tell where it may type; the template itself draws no such
            // line (see the xfa_render_* tool descriptions).
            renderer: Renderer::new(limits).with_chrome(RenderOptions {
                field_underline: true,
            }),
            blobs: Arc::new(blobs),
            max_inline_bytes,
        })
    }

    /// The image itself when it is small enough to be worth putting in front of
    /// a model, a handle when it is not.
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
            Ok((
                vec![ContentBlock::image(
                    B64.encode(&page.data),
                    page.mime.to_string(),
                )],
                meta,
            ))
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
                "media_type": blob.media_type,
                "byte_len": blob.byte_len,
                "digest": blob.digest,
            });
            Ok((
                vec![ContentBlock::text(format!(
                    "page {} rendered to blob {} ({} bytes, {}) — too large to inline",
                    page.page, blob.handle, blob.byte_len, blob.media_type
                ))],
                meta,
            ))
        }
    }

    /// What a call addresses: a document in a requested state, or one
    /// revision of an open session. Exactly one of `doc`, `doc_path` and
    /// `session` must be given; naming two or none is a mistake worth
    /// reporting rather than resolving by precedence, since they would often
    /// answer about different things.
    fn target_of(&self, target: XfaTarget) -> Result<Target, RenderError> {
        let XfaTarget {
            document: XfaDocument { doc, doc_path },
            session,
            revision,
            state,
        } = target;
        let document = DocumentArg::from_parts(doc, doc_path)?;
        match (document, session) {
            (Some(document), Some(_)) => Err(RenderError::invalid_argument(
                "session",
                format!(
                    "both {} and session were given; supply exactly one",
                    match document {
                        DocumentArg::Blob(_) => "doc",
                        DocumentArg::Path(_) => "doc_path",
                    }
                ),
            )),
            (None, None) => Err(RenderError::invalid_argument(
                "doc",
                "none of doc, doc_path and session was given; supply exactly one",
            )),
            (Some(document), None) => Ok(Target::doc(
                &document.path(&self.blobs)?,
                &state.map(StateArg::into_spec).unwrap_or_default(),
            )),
            (None, Some(handle)) => {
                let revision = revision.ok_or_else(|| {
                    RenderError::invalid_argument(
                        "revision",
                        "required when addressing by session — pass the revision you last saw",
                    )
                })?;
                Ok(Target::view(&handle, revision))
            }
        }
    }

    /// A read tool's arguments as `XfaArgs<T>`: what it addresses, and the
    /// tool's own arguments.
    fn read<T: DeserializeOwned>(&self, args: &Value) -> Result<(Target, T), RenderError> {
        let XfaArgs { document, args } = parse_args::<XfaArgs<T>>(args)?;
        Ok((self.target_of(document)?, args))
    }

    pub fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, RenderError> {
        match name {
            "xfa_open" => {
                let ServerArgs {
                    document: XfaDocument { doc, doc_path },
                    args: InfoArgs {},
                } = parse_args::<ServerArgs<XfaDocument, InfoArgs>>(args)?;
                let path = DocumentArg::required(doc, doc_path)?.path(&self.blobs)?;
                let (session, revision, info) = self.renderer.open(&path)?;
                let mut value = serde_json::to_value(&info)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize info: {e}")))?;
                value["session"] = json!(session);
                value["revision"] = json!(revision);
                Ok(CallToolResult::structured(value))
            }

            "xfa_close" => {
                let CloseArgs { session } = parse_args(args)?;
                self.renderer.close(&session)?;
                Ok(CallToolResult::structured(json!({ "closed": true })))
            }

            "xfa_set" => {
                let SetArgs { at, field, value } = parse_args(args)?;
                let interaction =
                    self.renderer.set(at.session, at.expected_revision, field, value)?;
                let value = serde_json::to_value(&interaction).map_err(|e| {
                    RenderError::backend("xfa", format!("serialize interaction: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_click" => {
                let ClickArgs { at, field } = parse_args(args)?;
                let interaction = self.renderer.click(at.session, at.expected_revision, field)?;
                let value = serde_json::to_value(&interaction).map_err(|e| {
                    RenderError::backend("xfa", format!("serialize interaction: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_reset" => {
                let ResetArgs { at } = parse_args(args)?;
                let interaction = self.renderer.reset(at.session, at.expected_revision)?;
                let value = serde_json::to_value(&interaction).map_err(|e| {
                    RenderError::backend("xfa", format!("serialize interaction: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_info" => {
                let (target, InfoArgs {}) = self.read(args)?;
                let info = self.renderer.info(&target)?;
                let mut value = serde_json::to_value(&info)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize info: {e}")))?;
                // The `info` ingest capability's `applicable` field: this
                // server is the right handler exactly for an XFA form (an
                // ordinary PDF is in practice refused with `not_xfa` before
                // this point). A property of the ingest contract, not of the
                // document, so merged here rather than added to `DocumentInfo`.
                value["applicable"] = json!(info.kind == u2s_render_xfa::DocumentKind::Xfa);
                Ok(CallToolResult::structured(value))
            }

            "xfa_render_page" => {
                let (target, a): (_, RenderPageArgs) = self.read(args)?;
                let (page, warning) = self.renderer.render_page(
                    &target,
                    a.page,
                    a.dpi,
                    a.max_edge_px,
                    a.format.unwrap_or_default(),
                )?;
                let (content, mut meta) = self.page_content(&page)?;
                if let Some(w) = warning {
                    meta["warning"] = json!(w);
                }
                Ok(with_structured(CallToolResult::success(content), meta))
            }

            "xfa_render_pages" => {
                let (target, a): (_, RenderPagesArgs) = self.read(args)?;
                let batch = self.renderer.render_pages(
                    &target,
                    a.pages,
                    a.from,
                    a.limit,
                    a.dpi,
                    a.max_edge_px,
                    a.format.unwrap_or_default(),
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

            "xfa_render_region" => {
                let (target, a): (_, RenderRegionArgs) = self.read(args)?;
                let page = self.renderer.render_region(
                    &target,
                    a.page,
                    a.rect_pt,
                    a.dpi,
                    a.format.unwrap_or_default(),
                )?;
                let (content, meta) = self.page_content(&page)?;
                Ok(with_structured(CallToolResult::success(content), meta))
            }

            "xfa_page_text" => {
                let (target, a): (_, PageTextArgs) = self.read(args)?;
                let text = self.renderer.page_text(&target, a.page, a.offset, a.limit)?;
                let value = serde_json::to_value(&text)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize text: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_search_text" => {
                let (target, a): (_, SearchTextArgs) = self.read(args)?;
                let search = self.renderer.search_text(&target, a.from, a.pattern()?, a.limit)?;
                let value = serde_json::to_value(&search)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize search: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_controls" => {
                let (target, a): (_, ControlsArgs) = self.read(args)?;
                // The schema states both bounds; a caller that skips it
                // (a standalone client) still gets the same answer.
                let limit = match a.limit {
                    None => CONTROLS_DEFAULT_LIMIT,
                    Some(0) => {
                        return Err(RenderError::invalid_argument("limit", "must be at least 1"));
                    }
                    Some(n) => n.min(CONTROLS_MAX_LIMIT),
                };
                let c = self.renderer.controls(&target)?.window(
                    a.kinds.as_deref(),
                    a.offset.unwrap_or(0),
                    limit,
                );
                let value = serde_json::to_value(&c)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize controls: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_field" => {
                let (target, FieldArgs { field }) = self.read(args)?;
                let control = self.renderer.field(&target, field.as_str())?.ok_or_else(|| {
                    RenderError::invalid_argument(
                        "field",
                        format!("no field {field} on this form; call xfa_controls for the available ones"),
                    )
                })?;
                let value = serde_json::to_value(&control)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize field: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            other => Err(RenderError::backend("xfa", format!("unknown tool {other}"))),
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

impl ServerHandler for XfaRenderServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("u2s-render-xfa-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Renders Adobe XFA forms, which pdfium cannot: it draws a static \
             'please update your reader' shim for them instead.\n\n\
             Call `xfa_info` first. If it reports kind 'not_xfa' the document is an \
             ordinary PDF — use the PDF renderer for it.\n\n\
             To use the form the way a person would, call `xfa_open` once, then `xfa_set` \
             one control at a time and `xfa_click` to press a button. Each call fires the \
             field's own scripts and reports what changed, including fields that appeared or \
             disappeared, plus the `revision` to carry forward: pass that `session` and `revision` to any read tool \
             (`xfa_render_page`, `xfa_page_text`, `xfa_search_text`, `xfa_controls`, ...) to \
             see or read the form as it now stands. `xfa_reset` puts a session back the way it \
             opened without losing the handle; `xfa_close` releases it, though an unused \
             session is reclaimed on its own after a while.\n\n\
             On a form with a repeatable section, pressing its add button creates the next \
             instance; its fields are listed under indexed paths (`Row[1].Amount`) in \
             `appeared` and in `xfa_controls`.\n\n\
             For a single one-shot read with no interaction, `doc_path` plus the `state` \
             argument addresses a state directly by its ordered `steps` (sets and presses) — \
             call `xfa_controls` with `doc_path` to see a form's controls before opening a \
             session on it.\n\n\
             For multi-page work use `xfa_render_pages` and follow `next_from` until it \
             is null.\n\n\
             To locate something rather than read everything, use `xfa_search_text`: it \
             greps every page's text and hands back each match's page, offset and length, \
             which `xfa_page_text` takes directly. Search to find the page, then read or \
             render only that page."
                .to_string(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            specs::tool_specs().iter().filter_map(to_mcp_tool).collect(),
        ))
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

        // Layout and rendering are blocking and can take seconds; they must not
        // sit on the async runtime's worker.
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
