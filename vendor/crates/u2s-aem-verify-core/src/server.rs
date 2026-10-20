//! The rmcp `ServerHandler` glue shared by every AEM-verifying binary,
//! parameterised by a [`ServerConfig`] so neither `u2s-aem-verify-mcp` nor
//! `u2s-aem-ubs-verify-mcp` needs its own copy of this ~300-line dispatch
//! loop. Three tools: `verify_status` and `verify_package_check` are plain
//! reads, offline; `verify_run` is the one side-effecting tool, declaring
//! `u2s_mcp::manifest::VerifyCapability::Run` in its manifest entry, and is
//! never offered to the Conversion Agent (`u2s-server`'s enablement gate
//! reads `side_effecting`, not this crate's own judgment).

use std::borrow::Cow;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer, ServiceExt};
use rmcp::{ErrorData as McpError, transport::stdio};
use serde_json::{Value, json};
use u2s_blob::{BlobOrPath, BlobStore, SourceArgs};
use u2s_verify_core::docker::DockerLifecycle;

use crate::args::{
    ClickArgs, CloseArgs, ControlsArgs, FieldArgs, GotoArgs, LanguageArg, OpenArgs,
    PackageCheckArgs, ScreenshotArgs, SetArgs, SetRequests, StatusArgs, StepArgs, VerifyRunArgs,
    parse_args,
};

use crate::driver::FormDriver;
use crate::instance::InstancePool;
use crate::interactive;
use crate::package_check;
use crate::profile::Profile;

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
pub struct AemVerifyServer {
    profile: Arc<Profile>,
    blobs: Arc<BlobStore>,
    instances: Arc<InstancePool>,
    config: Arc<ServerConfig>,
}

impl AemVerifyServer {
    /// Never touches Docker: a profile that cannot reach a daemon right
    /// now must still start and serve `verify_status`/`verify_package_check`
    /// -- see the crate doc's own point that Docker being unreachable is
    /// reported, not fatal.
    fn new(config: Arc<ServerConfig>) -> Result<Self, String> {
        let profile = Profile::from_env().map_err(|err| format!("cannot start: {err}"))?;
        let blobs = BlobStore::from_env().map_err(|err| format!("cannot start: {err}"))?;
        Self::with_parts(config, profile, blobs)
    }

    /// [`Self::new`] with the profile and blob store supplied by the caller
    /// instead of read from the environment, for in-process hosts.
    pub fn with_parts(
        config: Arc<ServerConfig>,
        profile: Profile,
        blobs: BlobStore,
    ) -> Result<Self, String> {
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
            blobs: Arc::new(blobs),
            instances: Arc::new(InstancePool::new()),
            config,
        })
    }

    /// Tears down every session that is not in use right now.
    pub async fn shutdown(&self) -> Result<(), String> {
        let docker = DockerLifecycle::connect()
            .await
            .map_err(|err| format!("could not reach Docker to tear down sessions: {err}"))?;
        self.instances
            .sweep_idle(&docker, std::time::Duration::ZERO)
            .await;
        Ok(())
    }

    /// Boots `instance_id`'s instance (the default one when `None`) in
    /// advance -- see [`crate::instance::warm_up`]. Not an MCP tool: an
    /// in-process host calls it when it knows a form will be opened soon,
    /// so the boot overlaps work it does anyway.
    pub async fn warm_up(&self, instance_id: Option<&str>) -> Result<(), String> {
        let key = instance_id
            .filter(|id| !id.is_empty())
            .unwrap_or(crate::instance::DEFAULT_INSTANCE_KEY);
        crate::instance::warm_up(&self.instances, key, &self.profile)
            .await
            .map(|_findings| ())
            .map_err(|err| err.to_string())
    }

    pub async fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, String> {
        match name {
            "verify_status" => self.verify_status(args).await,
            "verify_package_check" => self.verify_package_check(args),
            "verify_run" => self.verify_run(args).await,
            "verify_open" => self.verify_open(args).await,
            "verify_controls" => self.verify_controls(args).await,
            "verify_field" => self.verify_field(args).await,
            "verify_set" => self.verify_set(args).await,
            "verify_click" => self.verify_click(args).await,
            "verify_next" => self.verify_next(args).await,
            "verify_prev" => self.verify_prev(args).await,
            "verify_goto" => self.verify_goto(args).await,
            "verify_reset" => self.verify_reset(args).await,
            "verify_screenshot" => self.verify_screenshot(args).await,
            "verify_submit" => self.verify_submit(args).await,
            "verify_close" => self.verify_close(args).await,
            other => Err(format!("no tool named {other:?}")),
        }
    }

    async fn verify_status(&self, args: &Value) -> Result<CallToolResult, String> {
        // A missing private AEM image is pulled on the first run only with a
        // registry login configured, so both facts are worth reporting before
        // a run fails on them.
        let (docker_reachable, aem_image_present) = match DockerLifecycle::connect().await {
            Ok(docker) => (
                docker.is_reachable().await,
                docker
                    .image_id(&self.profile.aem_image)
                    .await
                    .ok()
                    .flatten()
                    .is_some(),
            ),
            Err(_) => (false, false),
        };

        let redacto_reachable = match &self.profile.redacto_url {
            Some(url) => Some(
                u2s_verify_core::http::is_reachable(url, std::time::Duration::from_secs(5)).await,
            ),
            None => None,
        };
        let instance_id = parse_args::<StatusArgs>(args)?.instance_key();
        let instance_status = crate::instance::status(&self.instances, &instance_id).await;
        let active_instance_count = self.instances.active_count().await;

        Ok(CallToolResult::structured(json!({
            "format": self.profile.format,
            "aem_image": self.profile.aem_image,
            "chromium_image": self.profile.chromium_image,
            "platform": self.profile.platform,
            "submit": submit_str(&self.profile.submit),
            "docker_reachable": docker_reachable,
            "aem_image_present": aem_image_present,
            "registry_login_configured": self.profile.registry_credentials.is_some(),
            "aem_data_volume": self.profile.aem_data_volume,
            "redacto_url": self.profile.redacto_url,
            "redacto_reachable": redacto_reachable,
            "instance_id": instance_id,
            "instance_active": instance_status.active,
            "instance_uptime_secs": instance_status.uptime_secs,
            "instance_aem_image": instance_status.aem_image,
            "sessions": instance_status.sessions.iter().map(|session| json!({
                "session": session.handle,
                "form_jcr_path": session.form_jcr_path,
                "revision": session.revision,
            })).collect::<Vec<_>>(),
            "active_instance_count": active_instance_count,
        })))
    }

    fn verify_package_check(&self, args: &Value) -> Result<CallToolResult, String> {
        let bytes = parse_args::<PackageCheckArgs>(args)?
            .package()?
            .read(&self.blobs)?;
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
        // Read with the language argument whether or not this profile offers
        // it: a driver that cannot open a language refuses one by name.
        let run = parse_args::<VerifyRunArgs<LanguageArg>>(args)?;
        let instance_id = run.instance_key();
        let bytes = BlobOrPath::from_parts(run.package, run.package_path, SourceArgs::PACKAGE)?.read(&self.blobs)?;

        let report = if run.dry_run {
            crate::flow::dry_run(&bytes).await
        } else {
            let fill = run.fill.map(|obj| obj.into_iter().collect()).unwrap_or_default();
            let submit = run.submit;
            let language = run.language.language;
            crate::flow::run(
                &self.profile,
                self.config.driver.as_ref(),
                &self.blobs,
                &self.instances,
                &instance_id,
                crate::flow::RunRequest {
                    package_bytes: bytes,
                    fill,
                    submit,
                    language,
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
        let open = parse_args::<OpenArgs>(args)?;
        let instance_id = open.instance.key();
        let bytes = BlobOrPath::from_parts(open.package, open.package_path, SourceArgs::PACKAGE)?.read(&self.blobs)?;
        let result = interactive::open(
            &self.profile,
            self.config.driver.as_ref(),
            &self.instances,
            &instance_id,
            bytes,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_controls(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<ControlsArgs>(args)?;
        let query = a.query()?;
        let result = interactive::controls(
            &self.instances,
            self.config.driver.as_ref(),
            &a.instance.key(),
            &a.session.session,
            a.revision,
            &query,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_field(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<FieldArgs>(args)?;
        let result = interactive::field(
            &self.instances,
            &a.instance.key(),
            &a.session.session,
            a.revision,
            &a.field,
        )
            .await
            .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_set(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<SetArgs>(args)?;
        let session = a.session.session.clone();
        let expected_revision = a.based_on.expected_revision;
        let instance_id = a.instance.key();
        match a.requests()? {
            SetRequests::Many(requests) => {
                let result = interactive::set_many(
                    &self.instances,
                    self.config.driver.as_ref(),
                    &instance_id,
                    &session,
                    expected_revision,
                    &requests,
                )
                .await
                .map_err(|err| err.to_string())?;
                Ok(CallToolResult::structured(to_structured(&result)?))
            }
            SetRequests::One { field, value } => {
                let result = interactive::set(
                    &self.instances,
                    self.config.driver.as_ref(),
                    &instance_id,
                    &session,
                    expected_revision,
                    &field,
                    &value,
                )
                .await
                .map_err(|err| err.to_string())?;
                Ok(CallToolResult::structured(to_structured(&result)?))
            }
        }
    }

    async fn verify_click(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<ClickArgs>(args)?;
        let result = interactive::click(
            &self.instances,
            self.config.driver.as_ref(),
            &a.instance.key(),
            &a.session.session,
            a.based_on.expected_revision,
            &a.field,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_next(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<StepArgs>(args)?;
        let result = interactive::next(
            &self.instances,
            self.config.driver.as_ref(),
            &a.instance.key(),
            &a.session.session,
            a.based_on.expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_prev(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<StepArgs>(args)?;
        let result = interactive::prev(
            &self.instances,
            self.config.driver.as_ref(),
            &a.instance.key(),
            &a.session.session,
            a.based_on.expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_goto(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<GotoArgs>(args)?;
        let result = interactive::goto(
            &self.instances,
            self.config.driver.as_ref(),
            &a.instance.key(),
            &a.session.session,
            a.based_on.expected_revision,
            &a.panel,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_reset(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<StepArgs>(args)?;
        let result = interactive::reset(
            &self.profile,
            self.config.driver.as_ref(),
            &self.instances,
            &a.instance.key(),
            &a.session.session,
            a.based_on.expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_screenshot(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<ScreenshotArgs>(args)?;
        let result = interactive::screenshot(
            &self.instances,
            &a.instance.key(),
            &a.session.session,
            a.revision,
            a.format.unwrap_or_default().into(),
            a.field.as_deref(),
        )
        .await
        .map_err(|err| err.to_string())?;
        self.screenshot_content(result)
    }

    async fn verify_submit(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<StepArgs>(args)?;
        let result = interactive::submit(
            &self.instances,
            self.config.driver.as_ref(),
            &self.profile,
            &self.blobs,
            &a.instance.key(),
            &a.session.session,
            a.based_on.expected_revision,
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(CallToolResult::structured(to_structured(&result)?))
    }

    async fn verify_close(&self, args: &Value) -> Result<CallToolResult, String> {
        let a = parse_args::<CloseArgs>(args)?;
        interactive::close(&self.instances, &self.profile, &a.instance.key(), &a.session.session)
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

/// The whole `fn main` body for an AEM-verifying binary: starts the MCP
/// server. Each binary's own `main.rs` is left as a one-line
/// `#[tokio::main] async fn main() { u2s_aem_verify_core::server::run_main(config()).await }`
/// (or exits with the error this returns), so neither binary duplicates
/// argument parsing, startup logging, or the leftover-cleanup/idle-sweep
/// wiring.
pub async fn run_main(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = Arc::new(config);
    let server = match AemVerifyServer::new(Arc::clone(&config)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: cannot start: {e}", config.name);
            eprintln!(
                "hint: check U2S_AEM_VERIFY_FORMAT, U2S_AEM_VERIFY_IMAGE, U2S_AEM_VERIFY_USER, \
                 and U2S_AEM_VERIFY_PASSWORD are all set"
            );
            std::process::exit(2);
        }
    };
    u2s_verify_core::instance::remove_leftovers(
        &server.profile.format,
        &crate::instance::reach_of(&server.profile),
    )
    .await;
    tokio::spawn(u2s_verify_core::instance::idle_sweep_loop(
        Arc::clone(&server.instances),
        server.profile.idle_timeout,
    ));

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
