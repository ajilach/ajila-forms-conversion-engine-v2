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
use serde_json::{Value, json};
use u2s_render_xfa::states::{ControlKind, SelectionSpec, Step};
use u2s_render_xfa::{
    BlobStore, ImageFormat, Limits, Pattern, RectPt, RenderError, RenderedPage, Renderer, Target,
    fonts, states::StateSpec,
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

        Self::with_parts(Limits::from_env(), BlobStore::from_env())
    }

    /// [`Self::new`] for in-process hosts: limits and blob store come from the
    /// caller, and so do the fonts, which the caller registers first
    /// (`u2s_xfa::fonts::register_dir_once`, or the font manager directly). Refuses
    /// to start without a fallback font, for the reason [`Self::new`] does.
    pub fn with_parts(limits: Limits, blobs: BlobStore) -> Result<Self, RenderError> {
        if !fonts::fallback_registered() {
            return Err(RenderError::EngineUnavailable {
                engine: "xfa",
                searched: vec!["no fallback font is registered: register the fonts before \
                                creating the server"
                    .into()],
            });
        }
        let max_inline_bytes = limits.max_inline_bytes;
        Ok(XfaRenderServer {
            renderer: Renderer::new(limits),
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
                "path": blob.path.display().to_string(),
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

    /// Parse the shared `state` argument. Absent or explicitly null means
    /// the default state; anything else that does not match the shape is a
    /// hard failure rather than a silent fallback to default, since a
    /// misspelled key would otherwise render the wrong thing without a word.
    fn state_of(args: &Value) -> Result<StateSpec, RenderError> {
        let Some(state) = args.get("state") else {
            return Ok(StateSpec::default());
        };
        if state.is_null() {
            return Ok(StateSpec::default());
        }
        let Some(obj) = state.as_object() else {
            return Err(RenderError::invalid_argument(
                "state",
                "must be an object of the shape { \"steps\": [...] }",
            ));
        };
        let field_of = |item: &Value, what: &str| -> Result<String, RenderError> {
            item.get("field")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    RenderError::invalid_argument("state", format!("each {what} needs a \"field\""))
                })
        };
        let selection_of = |item: &Value, what: &str| -> Result<SelectionSpec, RenderError> {
            let value = item.get("value").and_then(Value::as_str).ok_or_else(|| {
                RenderError::invalid_argument("state", format!("each {what} needs a \"value\""))
            })?;
            Ok(SelectionSpec {
                field: field_of(item, what)?,
                value: value.to_string(),
            })
        };
        match (obj.get("steps"), obj.get("selections")) {
            (Some(_), Some(_)) => Err(RenderError::invalid_argument(
                "state",
                "has both \"steps\" and \"selections\"; give one (selections are set steps)",
            )),
            (Some(steps), None) => {
                let list = steps.as_array().ok_or_else(|| {
                    RenderError::invalid_argument("state", "\"steps\" must be an array")
                })?;
                let mut out = Vec::with_capacity(list.len());
                for item in list {
                    match (item.get("set"), item.get("click")) {
                        (Some(set), None) => out.push(Step::Set(selection_of(set, "set step")?)),
                        (None, Some(click)) => out.push(Step::Click {
                            field: field_of(click, "click step")?,
                        }),
                        _ => {
                            return Err(RenderError::invalid_argument(
                                "state",
                                "each step is {\"set\": {field, value}} or {\"click\": {field}}",
                            ));
                        }
                    }
                }
                Ok(StateSpec { steps: out })
            }
            (None, Some(selections)) => {
                let list = selections.as_array().ok_or_else(|| {
                    RenderError::invalid_argument("state", "\"selections\" must be an array")
                })?;
                let selections = list
                    .iter()
                    .map(|item| selection_of(item, "selection"))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(StateSpec::selections(selections))
            }
            (None, None) => {
                let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
                Err(RenderError::invalid_argument(
                    "state",
                    format!(
                        "must have a \"steps\" array (or the older \"selections\"); this \
                         object has keys {keys:?}"
                    ),
                ))
            }
        }
    }

    /// The optional `kinds` filter of `xfa_controls`, parsed once here into
    /// [`ControlKind`]s; an unknown kind is refused, naming the valid ones.
    fn kinds_of(args: &Value) -> Result<Option<Vec<ControlKind>>, RenderError> {
        let Some(list) = args.get("kinds") else {
            return Ok(None);
        };
        let list = list.as_array().ok_or_else(|| {
            RenderError::invalid_argument("kinds", "must be an array of control kinds")
        })?;
        list.iter()
            .map(|k| {
                serde_json::from_value::<ControlKind>(k.clone()).map_err(|_| {
                    RenderError::invalid_argument(
                        "kinds",
                        format!(
                            "{k} is not a control kind; use any of {}",
                            ControlKind::ALL.map(ControlKind::wire_name).join(", ")
                        ),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// What a call addresses: a document on disk in a requested state, or
    /// one revision of an open session. Exactly one of `doc_path` and
    /// `session` must be given; naming both or neither is a mistake worth
    /// reporting rather than resolving by precedence, since the two would
    /// often answer about different things.
    fn target_of(args: &Value) -> Result<Target, RenderError> {
        let doc_path = args.get("doc_path").and_then(Value::as_str);
        let session = args.get("session").and_then(Value::as_str);
        match (doc_path, session) {
            (Some(_), Some(_)) => Err(RenderError::invalid_argument(
                "doc_path",
                "both doc_path and session were given; supply exactly one",
            )),
            (None, None) => Err(RenderError::invalid_argument(
                "doc_path",
                "neither doc_path nor session was given; supply exactly one",
            )),
            (Some(path), None) => Ok(Target::doc(path, &Self::state_of(args)?)),
            (None, Some(handle)) => {
                let revision = args.get("revision").and_then(Value::as_u64).ok_or_else(|| {
                    RenderError::invalid_argument(
                        "revision",
                        "required when addressing by session — pass the revision you last saw",
                    )
                })?;
                Ok(Target::view(handle, revision))
            }
        }
    }

    pub fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, RenderError> {
        match name {
            "xfa_open" => {
                let path = arg_str(args, "doc_path")?;
                let (session, revision, info) = self.renderer.open(&path)?;
                let mut value = serde_json::to_value(&info)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize info: {e}")))?;
                value["session"] = json!(session);
                value["revision"] = json!(revision);
                Ok(CallToolResult::structured(value))
            }

            "xfa_close" => {
                let handle = arg_str(args, "session")?;
                self.renderer.close(&handle)?;
                Ok(CallToolResult::structured(json!({ "closed": true })))
            }

            "xfa_set" => {
                let handle = arg_str(args, "session")?;
                let expected_revision = arg_u64(args, "expected_revision")?;
                let field = arg_str(args, "field")?;
                let value = arg_str(args, "value")?;
                let interaction = self.renderer.set(handle, expected_revision, field, value)?;
                let value = serde_json::to_value(&interaction).map_err(|e| {
                    RenderError::backend("xfa", format!("serialize interaction: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_click" => {
                let handle = arg_str(args, "session")?;
                let expected_revision = arg_u64(args, "expected_revision")?;
                let field = arg_str(args, "field")?;
                let interaction = self.renderer.click(handle, expected_revision, field)?;
                let value = serde_json::to_value(&interaction).map_err(|e| {
                    RenderError::backend("xfa", format!("serialize interaction: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_reset" => {
                let handle = arg_str(args, "session")?;
                let expected_revision = arg_u64(args, "expected_revision")?;
                let interaction = self.renderer.reset(handle, expected_revision)?;
                let value = serde_json::to_value(&interaction).map_err(|e| {
                    RenderError::backend("xfa", format!("serialize interaction: {e}"))
                })?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_info" => {
                let target = Self::target_of(args)?;
                let info = self.renderer.info(&target)?;
                let value = serde_json::to_value(&info)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize info: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_render_page" => {
                let target = Self::target_of(args)?;
                let (page, warning) = self.renderer.render_page(
                    &target,
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

            "xfa_render_pages" => {
                let target = Self::target_of(args)?;
                let pages = args.get("pages").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_u64())
                        .map(|v| v as u32)
                        .collect::<Vec<_>>()
                });
                let batch = self.renderer.render_pages(
                    &target,
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

            "xfa_render_region" => {
                let target = Self::target_of(args)?;
                let rect_arg = args.get("rect_pt").ok_or_else(|| {
                    RenderError::backend("xfa", "missing required argument rect_pt")
                })?;
                let rect = RectPt {
                    x: arg_f32(rect_arg, "x").unwrap_or(0.0),
                    y: arg_f32(rect_arg, "y").unwrap_or(0.0),
                    width: arg_f32(rect_arg, "width")
                        .ok_or_else(|| RenderError::backend("xfa", "rect_pt.width is required"))?,
                    height: arg_f32(rect_arg, "height")
                        .ok_or_else(|| RenderError::backend("xfa", "rect_pt.height is required"))?,
                };
                let page = self.renderer.render_region(
                    &target,
                    arg_u32(args, "page")?,
                    rect,
                    arg_f32(args, "dpi"),
                    ImageFormat::parse(args.get("format").and_then(Value::as_str))?,
                )?;
                let (content, meta) = self.page_content(&page)?;
                Ok(with_structured(CallToolResult::success(content), meta))
            }

            "xfa_page_text" => {
                let target = Self::target_of(args)?;
                let text = self.renderer.page_text(
                    &target,
                    arg_u32(args, "page")?,
                    args.get("offset")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                    args.get("limit")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                )?;
                let value = serde_json::to_value(&text)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize text: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_search_text" => {
                let target = Self::target_of(args)?;
                let search = self.renderer.search_text(
                    &target,
                    arg_u32_opt(args, "from"),
                    pattern_of(args)?,
                    args.get("limit")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                )?;
                let value = serde_json::to_value(&search)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize search: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_controls" => {
                let target = Self::target_of(args)?;
                let kinds = Self::kinds_of(args)?;
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
                let limit = match args.get("limit").and_then(Value::as_u64) {
                    None => CONTROLS_DEFAULT_LIMIT,
                    Some(0) => {
                        return Err(RenderError::invalid_argument(
                            "limit",
                            "must be at least 1",
                        ));
                    }
                    Some(n) => (n as usize).min(CONTROLS_MAX_LIMIT),
                };
                let c = self
                    .renderer
                    .controls(&target)?
                    .window(kinds.as_deref(), offset, limit);
                let value = serde_json::to_value(&c)
                    .map_err(|e| RenderError::backend("xfa", format!("serialize controls: {e}")))?;
                Ok(CallToolResult::structured(value))
            }

            "xfa_field" => {
                let target = Self::target_of(args)?;
                let field = arg_str(args, "field")?;
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

/// Compile the caller's query once, here at the edge, so nothing downstream
/// can hold an invalid pattern. A bad regex is the caller's fault, so it is
/// reported as a bad argument rather than as an engine failure.
fn pattern_of(args: &Value) -> Result<Pattern, RenderError> {
    let query = arg_str(args, "query")?;
    let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    Pattern::parse(&query, regex).map_err(|detail| RenderError::invalid_argument("query", detail))
}

fn arg_str(args: &Value, key: &str) -> Result<String, RenderError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RenderError::backend("xfa", format!("missing required argument {key}")))
}

fn arg_u32(args: &Value, key: &str) -> Result<u32, RenderError> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .ok_or_else(|| RenderError::backend("xfa", format!("missing required argument {key}")))
}

fn arg_u32_opt(args: &Value, key: &str) -> Option<u32> {
    args.get(key).and_then(Value::as_u64).map(|v| v as u32)
}

fn arg_u64(args: &Value, key: &str) -> Result<u64, RenderError> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| RenderError::backend("xfa", format!("missing required argument {key}")))
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
