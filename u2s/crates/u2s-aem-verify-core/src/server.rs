//! The rmcp `ServerHandler` glue shared by every AEM-verifying binary,
//! parameterised by a [`ServerConfig`] so neither `u2s-aem-verify-mcp` nor
//! `u2s-aem-ubs-verify-mcp` needs its own copy of this ~300-line dispatch
//! loop. Three tools: `verify_status` and `verify_package_check` are plain
//! reads, offline; `verify_run` is the one side-effecting tool, declaring
//! `u2s_mcp::manifest::VerifyCapability::Run` in its manifest entry, and is
//! never offered to the Conversion Agent (`u2s-server`'s enablement gate
//! reads `side_effecting`, not this crate's own judgment).
//!
//! `warm` is a CLI subcommand on the same binary (`<binary> warm`), not an
//! MCP tool -- see `crate::warm`.

use std::borrow::Cow;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_blob::BlobStore;
use u2s_verify_core::browser::ImageFormat;
use u2s_verify_core::docker::DockerLifecycle;

use crate::driver::FormDriver;
use crate::interactive;
use crate::package_check;
use crate::profile::Profile;
use crate::session::SessionPool;

const MANIFEST_URI: &str = "u2s://manifest";

/// Everything that differs between `u2s-aem-verify-mcp` and
/// `u2s-aem-ubs-verify-mcp`: the server's own name/version, which format
/// it insists on being registered as (if any), its `FormDriver`, and its
/// tool schemas/manifest/instructions -- all format-specific prompt
/// surface a binary owns, same as `u2s-aem-mcp`/`u2s-aem-ubs-mcp` each own
/// their own `specs.rs`.
pub struct ServerConfig {
    pub name: &'static str,
    pub version: &'static str,
    /// `Some("aem-ubs")` for a binary that must only ever run as that
    /// profile -- a mismatch (this binary registered under the wrong
    /// `U2S_AEM_VERIFY_FORMAT`) is a startup error, not a silent
    /// misconfiguration discovered only once a `verify_run` behaves
    /// strangely. `None` for a binary happy to serve whatever format its
    /// environment names (the generic one).
    pub expected_format: Option<&'static str>,
    pub driver: Arc<dyn FormDriver>,
    pub tool_specs: Vec<Value>,
    pub manifest: Box<dyn Fn(&Profile) -> Value + Send + Sync>,
    pub instructions: Box<dyn Fn(&Profile) -> String + Send + Sync>,
}

#[derive(Clone)]
struct AemVerifyServer {
    profile: Arc<Profile>,
    blobs: Arc<BlobStore>,
    session: Arc<SessionPool>,
    config: Arc<ServerConfig>,
}

impl AemVerifyServer {
    /// Never touches Docker: a profile that cannot reach a daemon right
    /// now must still start and serve `verify_status`/`verify_package_check`
    /// -- see the crate doc's own point that Docker being unreachable is
    /// reported, not fatal.
    fn new(config: Arc<ServerConfig>) -> Result<Self, String> {
        let profile = Profile::from_env().map_err(|err| format!("cannot start: {err}"))?;
        if let Some(expected) = config.expected_format
            && profile.format != expected
        {
            return Err(format!(
                "U2S_AEM_VERIFY_FORMAT={:?} but {} only ever serves {expected:?} -- check this \
                 binary is registered under the right profile",
                profile.format, config.name
            ));
        }
        Ok(Self {
            profile: Arc::new(profile),
            blobs: Arc::new(BlobStore::from_env()),
            session: Arc::new(SessionPool::new()),
            config,
        })
    }

    async fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, String> {
        match name {
            "verify_status" => self.verify_status(args).await,
            "verify_package_check" => self.verify_package_check(args),
            "verify_run" => self.verify_run(args).await,
            "verify_open" => self.verify_open(args).await,
            "verify_controls" => self.verify_controls(args).await,
            "verify_set" => self.verify_set(args).await,
            "verify_next" => self.verify_next(args).await,
            "verify_prev" => self.verify_prev(args).await,
            "verify_reset" => self.verify_reset(args).await,
            "verify_screenshot" => self.verify_screenshot(args).await,
            "verify_submit" => self.verify_submit(args).await,
            "verify_close" => self.verify_close(args).await,
            other => Err(format!("no tool named {other:?}")),
        }
    }

    /// `session_id` names which caller's session a call belongs to
    /// (`crate::session`'s module doc explains why this exists at all);
    /// callers that omit it -- a human, a test, anything with no
    /// `u2s-server` run behind it -- get [`crate::session::DEFAULT_SESSION_KEY`],
    /// preserving the single-session behaviour this crate had before.
    fn session_id(args: &Value) -> String {
        args.get("session_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .unwrap_or(crate::session::DEFAULT_SESSION_KEY)
            .to_owned()
    }

    async fn verify_status(&self, args: &Value) -> Result<CallToolResult, String> {
        // A commit-based warm image cannot exist for a data-volume profile
        // (see `crate::warm::warm_command`'s own doc) -- skip the lookup
        // entirely rather than report `warm_image_present`/`_stale` for an
        // image nothing ever builds or reads.
        let (docker_reachable, warm_image_present, warm_image_stale) =
            match DockerLifecycle::connect().await {
                Ok(docker) if self.profile.aem_data_volume.is_some() => {
                    (docker.is_reachable().await, false, false)
                }
                Ok(docker) => {
                    let reachable = docker.is_reachable().await;
                    let warm_tag = crate::warm::warm_image_tag(&self.profile.format);
                    let base_id = docker
                        .image_id(&self.profile.aem_image)
                        .await
                        .ok()
                        .flatten();
                    let warm_labels = docker.image_labels(&warm_tag).await.ok().flatten();
                    let present = warm_labels.is_some();
                    let stale = match (&base_id, &warm_labels) {
                        (Some(base_id), Some(labels)) => {
                            labels.get("u2s.base_image_id") != Some(base_id)
                        }
                        _ => false,
                    };
                    (reachable, present, stale)
                }
                Err(_) => (false, false, false),
            };

        let redacto_reachable = match &self.profile.redacto_url {
            Some(url) => Some(
                u2s_verify_core::http::is_reachable(url, std::time::Duration::from_secs(5)).await,
            ),
            None => None,
        };
        let session_id = Self::session_id(args);
        let session_status = self.session.status(&session_id).await;
        let active_session_count = self.session.active_count().await;

        Ok(CallToolResult::structured(json!({
            "format": self.profile.format,
            "aem_image": self.profile.aem_image,
            "chromium_image": self.profile.chromium_image,
            "platform": self.profile.platform,
            "submit": submit_str(&self.profile.submit),
            "docker_reachable": docker_reachable,
            "warm_image_present": warm_image_present,
            "warm_image_stale": warm_image_stale,
            "aem_data_volume": self.profile.aem_data_volume,
            "redacto_url": self.profile.redacto_url,
            "redacto_reachable": redacto_reachable,
            "session_id": session_id,
            "session_active": session_status.active,
            "session_uptime_secs": session_status.uptime_secs,
            "session_aem_image": session_status.aem_image,
            "session_open_form": session_status.open_form.map(|form| json!({
                "form": form.handle,
                "revision": form.revision,
            })),
            "active_session_count": active_session_count,
        })))
    }

    fn verify_package_check(&self, args: &Value) -> Result<CallToolResult, String> {
        let bytes = self.read_package(args)?;
        let inspection = package_check::inspect(&bytes).map_err(|err| {
            format!(
                "{}: {err}",
                u2s_verify_core::types::ErrorKind::PackageInvalid
            )
        })?;
        let mut structured = serde_json::to_value(&inspection.summary)
            .map_err(|err| format!("could not serialize the package summary: {err}"))?;
        let extension = self.config.driver.package_summary_extension(&inspection);
        if let (Some(target), Some(extra)) = (structured.as_object_mut(), extension.as_object()) {
            target.extend(extra.clone());
        }
        Ok(CallToolResult::structured(structured))
    }

    async fn verify_run(&self, args: &Value) -> Result<CallToolResult, String> {
        let bytes = self.read_package(args)?;
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let report = if dry_run {
            crate::flow::dry_run(&bytes).await
        } else {
            let fill = args
                .get("fill")
                .and_then(Value::as_object)
                .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let submit = args.get("submit").and_then(Value::as_bool).unwrap_or(true);
            let session_id = Self::session_id(args);
            crate::flow::run(
                &self.profile,
                self.config.driver.as_ref(),
                &self.blobs,
                &self.session,
                &session_id,
                crate::flow::RunRequest {
                    package_bytes: bytes,
                    fill,
                    submit,
                },
            )
            .await
            .map_err(|err| err.to_string())?
        };

        let text = report.summary_text();
        Ok(with_structured(
            CallToolResult::success(vec![ContentBlock::text(text)]),
            report.to_structured_content(),
        ))
    }

    async fn verify_open(&self, args: &Value) -> Result<CallToolResult, String> {
        let bytes = self.read_package(args)?;
        let session_id = Self::session_id(args);
        let result = interactive::open(
            &self.profile,
            self.config.driver.as_ref(),
            &self.session,
            &session_id,
            bytes,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_controls(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let revision = require_u64(args, "revision")?;
        let session_id = Self::session_id(args);
        let result = interactive::controls(
            &self.session,
            self.config.driver.as_ref(),
            &session_id,
            form,
            revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_set(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let expected_revision = require_u64(args, "expected_revision")?;
        let field = require_str(args, "field")?;
        let value = args
            .get("value")
            .ok_or_else(|| "missing required argument: value".to_owned())?;
        let session_id = Self::session_id(args);
        let result = interactive::set(
            &self.session,
            self.config.driver.as_ref(),
            &session_id,
            form,
            expected_revision,
            field,
            value,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_next(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let expected_revision = require_u64(args, "expected_revision")?;
        let session_id = Self::session_id(args);
        let result = interactive::next(
            &self.session,
            self.config.driver.as_ref(),
            &session_id,
            form,
            expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_prev(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let expected_revision = require_u64(args, "expected_revision")?;
        let session_id = Self::session_id(args);
        let result = interactive::prev(
            &self.session,
            self.config.driver.as_ref(),
            &session_id,
            form,
            expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_reset(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let expected_revision = require_u64(args, "expected_revision")?;
        let session_id = Self::session_id(args);
        let result = interactive::reset(
            &self.profile,
            self.config.driver.as_ref(),
            &self.session,
            &session_id,
            form,
            expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_screenshot(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let revision = require_u64(args, "revision")?;
        let format = parse_image_format(args)?;
        let field = args.get("field").and_then(Value::as_str);
        let session_id = Self::session_id(args);
        let result = interactive::screenshot(&self.session, &session_id, form, revision, format, field)
            .await
            .map_err(|err| err.to_string())?;
        self.screenshot_content(result)
    }

    async fn verify_submit(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let expected_revision = require_u64(args, "expected_revision")?;
        let session_id = Self::session_id(args);
        let result = interactive::submit(
            &self.session,
            self.config.driver.as_ref(),
            &self.profile,
            &self.blobs,
            &session_id,
            form,
            expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_close(&self, args: &Value) -> Result<CallToolResult, String> {
        let form = require_str(args, "form")?;
        let session_id = Self::session_id(args);
        interactive::close(&self.session, &self.profile, &session_id, form)
            .await
            .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(json!({ "closed": true })))
    }

    /// Builds `verify_screenshot`'s tool result: an inline image under
    /// `profile.max_inline_bytes`, or a blob handle plus a text block
    /// otherwise -- the same `xfa_render_page` convention
    /// `u2s-render-xfa-mcp` already uses (see that server's own
    /// `page_content`), applied here to a live browser screenshot instead
    /// of a rendered document page.
    fn screenshot_content(
        &self,
        result: interactive::ScreenshotResult,
    ) -> Result<CallToolResult, String> {
        let byte_len = result.bytes.len();
        if byte_len <= self.profile.max_inline_bytes {
            let data = BASE64.encode(&result.bytes);
            let meta = json!({
                "width_px": result.width_px,
                "height_px": result.height_px,
                "mime": result.mime,
                "byte_len": byte_len,
                "inline": true,
            });
            Ok(with_structured(
                CallToolResult::success(vec![ContentBlock::image(data, result.mime)]),
                meta,
            ))
        } else {
            let blob = self
                .blobs
                .put(&result.bytes, result.mime, result.ext)
                .map_err(|err| format!("storing the screenshot: {err}"))?;
            let text = format!(
                "screenshot rendered to blob {} ({byte_len} bytes, {}) -- too large to inline",
                blob.handle, result.mime
            );
            let meta = json!({
                "width_px": result.width_px,
                "height_px": result.height_px,
                "mime": result.mime,
                "byte_len": byte_len,
                "inline": false,
                "blob": {
                    "handle": blob.handle,
                    "media_type": blob.media_type,
                    "byte_len": blob.byte_len,
                    "digest": blob.digest,
                },
            });
            Ok(with_structured(
                CallToolResult::success(vec![ContentBlock::text(text)]),
                meta,
            ))
        }
    }

    fn read_package(&self, args: &Value) -> Result<Vec<u8>, String> {
        let blob_handle = args.get("package").and_then(Value::as_str);
        let path = args.get("package_path").and_then(Value::as_str);
        match (blob_handle, path) {
            (Some(handle), _) => self
                .blobs
                .get(handle)
                .map_err(|err| format!("could not read package {handle}: {err}")),
            (None, Some(path)) => std::fs::read(path)
                .map_err(|err| format!("could not read package_path {path}: {err}")),
            (None, None) => {
                Err("missing required argument: exactly one of package or package_path".to_owned())
            }
        }
    }
}

fn submit_str(submit: &crate::profile::SubmitArtefact) -> &'static str {
    match submit {
        crate::profile::SubmitArtefact::Download => "download",
        crate::profile::SubmitArtefact::DocumentOfRecord => "dor",
        crate::profile::SubmitArtefact::None => "none",
    }
}

fn with_structured(mut result: CallToolResult, value: Value) -> CallToolResult {
    result.structured_content = Some(value);
    result
}

/// Serializes an interactive tool's result type into the `Value`
/// `CallToolResult::structured` needs -- a `Serialize` type failing to
/// serialize would only ever mean a bug in this crate's own result types,
/// never bad caller input, but this still reports it as a tool error
/// rather than panicking.
fn to_structured(value: &impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| format!("could not serialize the result: {err}"))
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

fn require_u64(args: &Value, key: &str) -> Result<u64, String> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing required argument: {key}"))
}

/// `format`'s default is `"png"`, matching every screenshot `verify_run`
/// itself has always taken (`page.screenshot_full_page()`, PNG); `"jpeg"`
/// always uses quality 82, the same vision-payload quality
/// `u2s-render-core::limits::Limits`'s own default settled on, rather than
/// exposing a third knob nothing has needed yet.
fn parse_image_format(args: &Value) -> Result<ImageFormat, String> {
    match args.get("format").and_then(Value::as_str) {
        None | Some("png") => Ok(ImageFormat::Png),
        Some("jpeg") => Ok(ImageFormat::Jpeg { quality: 82 }),
        Some(other) => Err(format!(
            "format must be \"png\" or \"jpeg\", not {other:?}"
        )),
    }
}

fn to_mcp_tool(spec: &Value) -> Option<Tool> {
    let name = spec.get("name")?.as_str()?.to_string();
    let description = spec
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let schema = spec.get("input_schema")?.as_object()?.clone();
    Some(Tool::new(name, description, Arc::new(schema)))
}

impl ServerHandler for AemVerifyServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new(self.config.name, self.config.version);
        info.instructions = Some((self.config.instructions)(&self.profile));
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .config
            .tool_specs
            .iter()
            .filter_map(to_mcp_tool)
            .collect();
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
                    "This profile's verify capability: which output format it verifies, and \
                     the verify_run contract (side_effecting, verify: \"run\").",
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
        let body = serde_json::to_string_pretty(&(self.config.manifest)(&self.profile))
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

        Ok(match self.dispatch(&name, &args).await {
            Ok(result) => result,
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
        .into())
    }
}

/// Tears down every container this profile's own label
/// (`profile.owner_label()`) names, found rather than tracked in memory --
/// the precedent every leftover-cleanup in this workspace follows. Run
/// once at server startup: a container `verify_run` created before a crash
/// or a forced restart is otherwise never noticed again, since nothing
/// else in this process remembers it existed. Best-effort: Docker being
/// unreachable here is not a startup failure, only something logged, since
/// the server must still start and serve `verify_status` either way.
async fn remove_leftover_containers(profile: &Profile) {
    let docker = match DockerLifecycle::connect().await {
        Ok(docker) => docker,
        Err(err) => {
            log::warn!(
                "{}: could not check for leftover containers: {err}",
                crate::LOG_PREFIX
            );
            return;
        }
    };
    let ids = match docker.find_by_label(&profile.owner_label()).await {
        Ok(ids) => ids,
        Err(err) => {
            log::warn!(
                "{}: could not list leftover containers: {err}",
                crate::LOG_PREFIX
            );
            return;
        }
    };
    for id in ids {
        log::warn!(
            "{}: removing a leftover container from a prior run: {id}",
            crate::LOG_PREFIX
        );
        if let Err(err) = docker.teardown(&id).await {
            log::warn!(
                "{}: could not remove leftover container {id}: {err}",
                crate::LOG_PREFIX
            );
        }
    }
}

/// Runs forever, checking every minute whether any of the profile's
/// per-`session_id` sessions (`crate::session`) has gone unused for at
/// least `profile.idle_timeout` and tearing that one down if so -- the
/// safety net a long-lived session needs that the old disposable-per-run
/// containers never did, so a forgotten agent's session does not occupy
/// AEM's fairly heavy memory footprint indefinitely. Reconnects to Docker
/// each tick rather than holding one connection open for the process
/// lifetime, since `DockerLifecycle::connect` is cheap and this way a
/// Docker daemon that restarted mid-session is not a reason for the sweep
/// itself to die.
async fn idle_sweep_loop(profile: Arc<Profile>, session: Arc<SessionPool>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        match DockerLifecycle::connect().await {
            Ok(docker) => session.sweep_idle(&docker, profile.idle_timeout).await,
            Err(err) => log::warn!(
                "{}: could not reach Docker for the idle-session sweep: {err}",
                crate::LOG_PREFIX
            ),
        }
    }
}

/// The whole `fn main` body for an AEM-verifying binary: `<binary> warm`
/// dispatches to `crate::warm::warm_command`, anything else starts the MCP
/// server. Each binary's own `main.rs` is left as a one-line
/// `#[tokio::main] async fn main() { u2s_aem_verify_core::server::run_main(config()).await }`
/// (or exits with the error this returns), so neither binary duplicates
/// argument parsing, startup logging, or the leftover-cleanup/idle-sweep
/// wiring.
pub async fn run_main(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("warm") {
        let profile = match Profile::from_env() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{} warm: cannot start: {e}", config.name);
                std::process::exit(2);
            }
        };
        if let Some(expected) = config.expected_format
            && profile.format != expected
        {
            eprintln!(
                "{} warm: U2S_AEM_VERIFY_FORMAT={:?} but this binary only ever serves \
                 {expected:?}",
                config.name, profile.format
            );
            std::process::exit(2);
        }
        if let Err(e) = crate::warm::warm_command(&profile, config.name).await {
            eprintln!("{} warm: {e}", config.name);
            std::process::exit(1);
        }
        return Ok(());
    }

    let config = Arc::new(config);
    let server = match AemVerifyServer::new(Arc::clone(&config)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: cannot start: {e}", config.name);
            eprintln!(
                "hint: check U2S_AEM_VERIFY_FORMAT, U2S_AEM_VERIFY_IMAGE, U2S_AEM_VERIFY_USER \
                 and U2S_AEM_VERIFY_PASSWORD are all set"
            );
            std::process::exit(2);
        }
    };
    remove_leftover_containers(&server.profile).await;
    tokio::spawn(idle_sweep_loop(
        Arc::clone(&server.profile),
        Arc::clone(&server.session),
    ));

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
