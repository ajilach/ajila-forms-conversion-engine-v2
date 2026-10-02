//! MCP stdio server verifying a `redacto-ubs` dump: it boots a Redacto
//! platform per `session_id` (`u2s_redacto_verify_core::session`), imports
//! the dump there, and renders every declared language.
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

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_blob::BlobStore;
use u2s_redacto_verify_core::flow::{self, RunOutcome};
use u2s_redacto_verify_core::profile::{ProfileError, RenderProfile};
use u2s_redacto_verify_core::session::{self, SessionPool};
use u2s_verify_core::docker::DockerLifecycle;
use u2s_verify_core::session::{DEFAULT_SESSION_KEY, Reach};
use u2s_verify_core::types::{Artefact, ArtefactKind, BlobDescriptor, ErrorKind, VerifyError};

const MANIFEST_URI: &str = "u2s://manifest";
/// `U2S_REDACTO_VERIFY_UBS_*` -- must be `U2S_*`-prefixed to pass
/// `u2s_mcp::registration::validate_stdio_env`, the same discipline every
/// other server's own configuration variable in this workspace follows.
const ENV_PREFIX: &str = "U2S_REDACTO_VERIFY_UBS";
const PROFILE_NAME: &str = "redacto-ubs";

#[derive(Clone)]
pub struct RedactoVerifyServer {
    blobs: Arc<BlobStore>,
    /// An error names the missing setting. The server still starts, so its
    /// offline tools and conformance vectors work without the platform
    /// images, but `verify_run` refuses, and `u2s-server`'s deployment
    /// bootstrap refuses to start until every image setting is present.
    profile: Arc<Result<RenderProfile, ProfileError>>,
    sessions: Arc<SessionPool>,
}

impl RedactoVerifyServer {
    fn new() -> Self {
        // A UBS profile requests PDF/UA conformance; a generic profile
        // would request plain "pdf" here instead -- the one behavioural
        // difference this crate's own module doc says a Redacto profile
        // can carry.
        let profile = RenderProfile::from_env(PROFILE_NAME, ENV_PREFIX, "pdf-ua");
        if let Err(err) = &profile {
            log::warn!("u2s-redacto-ubs-verify-mcp: verify_run is unavailable: {err}");
        }
        Self::with_parts(profile, BlobStore::from_env())
    }

    /// [`Self::new`] for in-process hosts: the profile and blob store come
    /// from the caller instead of the environment.
    pub fn with_parts(profile: Result<RenderProfile, ProfileError>, blobs: BlobStore) -> Self {
        Self {
            blobs: Arc::new(blobs),
            profile: Arc::new(profile),
            sessions: Arc::new(SessionPool::new()),
        }
    }

    /// Tears down every session that is not in use right now.
    pub async fn shutdown(&self) -> Result<(), String> {
        let docker = DockerLifecycle::connect()
            .await
            .map_err(|err| format!("could not reach Docker to tear down sessions: {err}"))?;
        self.sessions
            .sweep_idle(&docker, std::time::Duration::ZERO)
            .await;
        Ok(())
    }

    /// The caller's `session_id`, or [`DEFAULT_SESSION_KEY`] for a caller
    /// with no `u2s-server` run behind it (a human, a test).
    fn session_id(args: &Value) -> String {
        args.get("session_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_SESSION_KEY)
            .to_owned()
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
        let docker = DockerLifecycle::connect().await.ok();
        let profile = match self.profile.as_ref() {
            Ok(profile) => {
                // The verifier cannot pull the private images itself, so
                // their presence on the daemon is worth reporting before a
                // run fails on it.
                let mut missing_images = Vec::new();
                if let Some(docker) = &docker {
                    for image in profile.images.all() {
                        if !matches!(docker.image_id(image).await, Ok(Some(_))) {
                            missing_images.push(image);
                        }
                    }
                }
                json!({
                    "name": profile.name,
                    "configured": true,
                    "images": {
                        "postgres": profile.images.postgres,
                        "migration": profile.images.migration,
                        "core": profile.images.core,
                        "rendering": profile.images.rendering,
                    },
                    "missing_images": docker.as_ref().map(|_| missing_images),
                    "render_format": profile.render_format,
                })
            }
            Err(err) => json!({ "name": PROFILE_NAME, "configured": false, "problem": err.to_string() }),
        };

        let session_id = Self::session_id(args);
        let (session_active, session_uptime_secs) = match self.sessions.lock(&session_id).await.as_ref() {
            Some(session) => (true, Some(session.started_at.elapsed().as_secs())),
            None => (false, None),
        };
        Ok(CallToolResult::structured(json!({
            "profile": profile,
            "docker_reachable": docker.is_some(),
            "session_id": session_id,
            "session_active": session_active,
            "session_uptime_secs": session_uptime_secs,
            "active_session_count": self.sessions.active_count().await,
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

        let profile = self
            .profile
            .as_ref()
            .as_ref()
            .map_err(|err| format!("verify_run is unavailable: {err}"))?;
        let docker = DockerLifecycle::connect()
            .await
            .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()).to_string())?;
        let session_id = Self::session_id(args);
        // Held until the render is done, so this call's render always sees
        // this call's import.
        let guard = session::ensure(&self.sessions, &session_id, &docker, profile)
            .await
            .map_err(|err| err.to_string())?;
        let platform = guard.as_ref().expect("ensure always leaves Some on success");
        let RunOutcome { mut report, rendered } = flow::run(&bytes, profile, &docker, platform).await;
        drop(guard);

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
            "Verifies a redacto-ubs dump: imports it into a Redacto platform of its own \
             `session_id` (booted on that session's first `verify_run`, which takes some seconds, \
             then reused), replacing any earlier import of the same document, renders every \
             declared language there, and returns each PDF as an artefact. Sessions never see \
             each other's imports. `verify_run` is side-effecting and offered to the Output \
             Review Agent alone; `verify_status` and `verify_dump_check` are plain reads. Read \
             `u2s://manifest` for the exact `verify_run` contract."
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
    if let Ok(profile) = server.profile.as_ref() {
        u2s_verify_core::session::remove_leftovers(
            profile.name,
            &Reach::from_self_container(profile.self_container.as_deref()),
        )
        .await;
        tokio::spawn(u2s_verify_core::session::idle_sweep_loop(
            Arc::clone(&server.sessions),
            profile.idle_timeout,
        ));
    }
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
