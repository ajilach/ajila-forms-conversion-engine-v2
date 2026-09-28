//! MCP stdio server verifying a `redacto-ubs` dump: a real Postgres import
//! on a throwaway (session-reused) database, plus an optional render call
//! against an already-running Redacto platform.
//!
//! **No format module.** The output format this verifies is already owned
//! by its own encoder server, `u2s-redacto-ubs-mcp` -- registering this
//! server never registers a format. See `specs.rs` for the exact tool
//! contract and `u2s_redacto_verify_core::flow` for the orchestration.
//!
//! **Why this binary needs no `FormDriver`-shaped seam of its own.**
//! `u2s-aem-verify-mcp`/`u2s-aem-ubs-verify-mcp` share one runtime through a
//! `FormDriver` because their *behaviour* actually differs (URL parameters,
//! terminal-panel detection, the submit routine). This crate's own
//! `RenderProfile` already carries the one thing a Redacto profile can
//! vary (which rendering format to request, where to reach it) as plain
//! data, so a second profile needs a different `RenderProfile::from_env`
//! call, not a second binary -- there being only one profile today is not
//! a reason to build the seam early.

pub mod specs;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_blob::BlobStore;
use u2s_redacto_verify_core::flow::{self, RunOutcome};
use u2s_redacto_verify_core::profile::RenderProfile;
use u2s_redacto_verify_core::session::{self, SessionPool};
use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::types::{Artefact, ArtefactKind, BlobDescriptor};

const MANIFEST_URI: &str = "u2s://manifest";
/// `U2S_REDACTO_VERIFY_UBS_*` -- must be `U2S_*`-prefixed to pass
/// `u2s_mcp::registration::validate_stdio_env`, the same discipline every
/// other server's own configuration variable in this workspace follows.
const ENV_PREFIX: &str = "U2S_REDACTO_VERIFY_UBS";

#[derive(Clone)]
pub struct RedactoVerifyServer {
    blobs: Arc<BlobStore>,
    pool: Arc<SessionPool>,
    profile: Arc<RenderProfile>,
}

impl RedactoVerifyServer {
    fn new() -> Self {
        // A UBS profile requests PDF/UA conformance; a generic profile
        // would request plain "pdf" here instead -- the one behavioural
        // difference this crate's own module doc says a Redacto profile
        // can carry.
        let profile = RenderProfile::from_env("redacto-ubs", ENV_PREFIX, "pdf-ua");
        Self::with_parts(profile, BlobStore::from_env())
    }

    /// [`Self::new`] for in-process hosts: the profile and blob store come
    /// from the caller instead of the environment.
    pub fn with_parts(profile: RenderProfile, blobs: BlobStore) -> Self {
        let pool = SessionPool::new(profile.postgres_image.clone());
        Self {
            blobs: Arc::new(blobs),
            pool: Arc::new(pool),
            profile: Arc::new(profile),
        }
    }

    /// Tears down the default session's container. In-process hosts never
    /// pass a `session_id`, so this is every container they caused.
    pub async fn shutdown(&self) -> Result<(), String> {
        let docker = DockerLifecycle::connect()
            .await
            .map_err(|err| format!("could not reach Docker to tear down the session: {err}"))?;
        self.pool.teardown(session::DEFAULT_SESSION_KEY, &docker).await;
        Ok(())
    }

    fn resolve_bytes(&self, args: &Value) -> Result<Vec<u8>, String> {
        let blob_handle = args.get("artifact_blob").and_then(Value::as_str);
        let path = args.get("artifact_path").and_then(Value::as_str);
        match (blob_handle, path) {
            (Some(handle), _) => self
                .blobs
                .get(handle)
                .map_err(|err| format!("could not read artifact_blob {handle}: {err}")),
            (None, Some(path)) => {
                std::fs::read(path).map_err(|err| format!("could not read artifact_path {path}: {err}"))
            }
            (None, None) => {
                Err("missing required argument: exactly one of artifact_blob or artifact_path".to_owned())
            }
        }
    }

    pub async fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, String> {
        match name {
            "verify_status" => self.verify_status(args).await,
            "verify_dump_check" => self.verify_dump_check(args),
            "verify_run" => self.verify_run(args).await,
            other => Err(format!("no tool named {other:?}")),
        }
    }

    async fn verify_status(&self, args: &Value) -> Result<CallToolResult, String> {
        let session_id =
            args.get("session_id").and_then(Value::as_str).unwrap_or(session::DEFAULT_SESSION_KEY);

        let docker_reachable = DockerLifecycle::connect().await.is_ok();
        let rendering_reachable = match &self.profile.rendering_base_url {
            Some(url) => Some(u2s_verify_core::http::is_reachable(url, Duration::from_secs(3)).await),
            None => None,
        };
        let session_age_secs = self.pool.peek(session_id).await.map(|d| d.as_secs());
        let sessions_active = self.pool.active_count().await;

        Ok(CallToolResult::structured(json!({
            "profile": {
                "name": self.profile.name,
                "postgres_image": self.profile.postgres_image,
                "rendering_configured": self.profile.rendering_base_url.is_some(),
                "render_format": self.profile.render_format,
            },
            "docker_reachable": docker_reachable,
            "rendering_reachable": rendering_reachable,
            "session": {
                "id": session_id,
                "up": session_age_secs.is_some(),
                "age_secs": session_age_secs,
            },
            "sessions_active": sessions_active,
        })))
    }

    fn verify_dump_check(&self, args: &Value) -> Result<CallToolResult, String> {
        let bytes = self.resolve_bytes(args)?;
        let report = u2s_redacto_verify_core::dump_check::check(&bytes);
        Ok(CallToolResult::structured(json!({
            "ok": report.ok,
            "document_id": report.document_id,
            "languages": report.languages,
            "asset_count": report.asset_count,
            "problem": report.problem,
        })))
    }

    async fn verify_run(&self, args: &Value) -> Result<CallToolResult, String> {
        let bytes = self.resolve_bytes(args)?;
        let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(false);

        if dry_run {
            let report = flow::dry_run(&bytes);
            let text = report.summary_text();
            return Ok(with_structured(
                CallToolResult::success(vec![ContentBlock::text(text)]),
                report.to_structured_content(),
            ));
        }

        let session_id =
            args.get("session_id").and_then(Value::as_str).unwrap_or(session::DEFAULT_SESSION_KEY);

        let RunOutcome { mut report, rendered } =
            flow::run(&self.pool, session_id, &bytes, &self.profile)
                .await
                .map_err(|err| err.to_string())?;

        for artefact in rendered {
            let blob = self
                .blobs
                .put(&artefact.bytes, artefact.media_type, "pdf")
                .map_err(|err| format!("could not store the rendered artefact: {err}"))?;
            report.artefacts.push(Artefact {
                kind: ArtefactKind::Download,
                label: format!("rendered ({})", artefact.language),
                blob: BlobDescriptor::from(&blob),
            });
        }

        let text = report.summary_text();
        Ok(with_structured(
            CallToolResult::success(vec![ContentBlock::text(text)]),
            report.to_structured_content(),
        ))
    }
}

fn with_structured(mut result: CallToolResult, value: Value) -> CallToolResult {
    result.structured_content = Some(value);
    result
}

fn to_mcp_tool(spec: &Value) -> Option<Tool> {
    let name = spec.get("name")?.as_str()?.to_string();
    let description = spec.get("description").and_then(Value::as_str).unwrap_or_default().to_string();
    let schema = spec.get("input_schema")?.as_object()?.clone();
    Some(Tool::new(name, description, Arc::new(schema)))
}

impl ServerHandler for RedactoVerifyServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder().enable_tools().enable_resources().build(),
        );
        info.server_info = Implementation::new("u2s-redacto-ubs-verify-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Verifies a redacto-ubs dump: imports it into a throwaway (session-reused) Postgres \
             database and reports row counts, plus -- when this profile has a rendering endpoint \
             configured -- renders it and returns each language's PDF as an artefact. Pass \
             `session_id` (any stable string identifying the calling agent/run) so concurrent \
             callers each get their own database rather than sharing and blocking on one; omit it \
             to use a single shared default session. `verify_run` is side-effecting and offered \
             to the Output Review Agent alone; `verify_status` and `verify_dump_check` are plain \
             reads. Read `u2s://manifest` for the exact `verify_run` contract."
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
                .with_description("This verifier's own manifest -- no format module; see specs.rs.")
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != MANIFEST_URI {
            return Err(McpError::resource_not_found(format!("unknown resource {}", request.uri), None));
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
        let args = request.arguments.clone().map(Value::Object).unwrap_or(Value::Object(Default::default()));

        Ok(match self.dispatch(&name, &args).await {
            Ok(result) => result,
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
        .into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = RedactoVerifyServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
