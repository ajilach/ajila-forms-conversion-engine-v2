//! The vendored u2s tool servers (see `u2s/VENDORED.md`), run in-process.
//!
//! Each server keeps its own tool contract: its specs go into the catalog
//! trimmed to the fields the model API takes, and its calls go through its own
//! `dispatch`. What this module adds is the edge: which files a call may read,
//! which artifact a verifier checks, where the fonts come from, and turning a
//! `CallToolResult` into a [`ToolReply`].
//!
//! The two verifiers both name tools `verify_status` and `verify_run`, so their
//! families are offered as `aem_verify_*` and `redacto_verify_*`. They also
//! lose the arguments this module supplies itself: the artifact to check (the
//! run's latest build) and the session (one per agent).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use rmcp3::model::{CallToolResult, ContentBlock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use u2s_aem_verify_core::profile::Profile;
use u2s_aem_verify_core::server::{AemVerifyServer, ServerConfig};
use u2s_redacto_ubs_verify_mcp::RedactoVerifyServer;
use u2s_redacto_verify_core::profile::RenderProfile;
use u2s_render_core::{BlobStore, Limits};
use u2s_render_pdf_mcp::PdfRenderServer;
use u2s_render_xfa_mcp::XfaRenderServer;
use u2s_verify_core::docker::DockerLifecycle;
use u2s_xfa_mcp::XfaDataServer;

use crate::conversion::{ReplyBlock, ToolReply};

/// The server a u2s tool belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    XfaData,
    XfaRender,
    PdfRender,
    AemVerify,
    RedactoVerify,
}

impl Family {
    /// The prefix the catalog name carries in front of the server's own name.
    fn prefix(self) -> &'static str {
        match self {
            Family::AemVerify => "aem_",
            Family::RedactoVerify => "redacto_",
            Family::XfaData | Family::XfaRender | Family::PdfRender => "",
        }
    }

    /// Arguments this module supplies, so the model is never offered them.
    fn supplied_arguments(self) -> &'static [&'static str] {
        match self {
            Family::AemVerify => &["package", "package_path", "session_id"],
            Family::RedactoVerify => &["artifact_blob", "artifact_path", "session_id"],
            Family::XfaData | Family::XfaRender | Family::PdfRender => &[],
        }
    }

    /// Appended to the family's descriptions: the upstream wording names the
    /// arguments this module supplies, and the model must not try to pass them.
    fn supplied_note(self) -> &'static str {
        match self {
            Family::AemVerify => {
                " In this run the latest build_aem_package result is checked automatically and the \
                 session is managed for you: pass no `package`, `package_path` or `session_id`."
            }
            Family::RedactoVerify => {
                " In this run the latest build_redacto_dump result is checked automatically and the \
                 session is managed for you: pass no `artifact_blob`, `artifact_path` or \
                 `session_id`."
            }
            Family::XfaData | Family::XfaRender | Family::PdfRender => "",
        }
    }

    /// The argument the verified artifact's path is passed in.
    fn artifact_argument(self) -> Option<&'static str> {
        match self {
            Family::AemVerify => Some("package_path"),
            Family::RedactoVerify => Some("artifact_path"),
            Family::XfaData | Family::XfaRender | Family::PdfRender => None,
        }
    }
}

/// One u2s tool as the catalog offers it.
struct Entry {
    /// `{name, description, input_schema}` under the catalog name.
    spec: Value,
    family: Family,
    /// The name the server itself dispatches on.
    server_name: String,
    /// Whether the server's own schema takes the artifact to verify.
    takes_artifact: bool,
}

/// Every u2s tool spec, as the catalog takes them.
pub(crate) fn tool_specs() -> Vec<Value> {
    entries().iter().map(|e| e.spec.clone()).collect()
}

/// Whether `name` is a u2s tool.
pub(crate) fn is_u2s_tool(name: &str) -> bool {
    entry(name).is_some()
}

/// Whether `name` checks the run's latest build, which the caller then has to
/// hand to [`U2sTools::call`].
pub(crate) fn takes_artifact(name: &str) -> bool {
    entry(name).is_some_and(|e| e.takes_artifact)
}

fn entry(name: &str) -> Option<&'static Entry> {
    entries().iter().find(|e| e.spec["name"] == name)
}

fn entries() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        [
            (u2s_xfa_mcp::specs::tool_specs(), Family::XfaData),
            (u2s_render_xfa_mcp::specs::tool_specs(), Family::XfaRender),
            (u2s_render_pdf_mcp::specs::tool_specs(), Family::PdfRender),
            (u2s_aem_ubs_verify_mcp::specs::tool_specs(), Family::AemVerify),
            (
                u2s_redacto_ubs_verify_mcp::specs::tool_specs(),
                Family::RedactoVerify,
            ),
        ]
        .into_iter()
        .flat_map(|(specs, family)| {
            let names: Vec<String> = specs
                .iter()
                .filter_map(|s| s["name"].as_str().map(str::to_string))
                .collect();
            specs.into_iter().map(move |spec| to_entry(&spec, family, &names))
        })
        .collect()
    })
}

/// The server's spec under its catalog name: renamed, with every mention of a
/// sibling tool renamed too, and without the arguments this module supplies.
fn to_entry(spec: &Value, family: Family, siblings: &[String]) -> Entry {
    let server_name = spec["name"].as_str().unwrap_or_default().to_string();
    let rename = |text: &str| {
        let mut out = text.to_string();
        if !family.prefix().is_empty() {
            for sibling in siblings {
                let pattern = format!(r"\b{}\b", regex_lite::escape(sibling));
                let re = regex_lite::Regex::new(&pattern).expect("an escaped tool name is a valid pattern");
                out = re
                    .replace_all(&out, format!("{}{sibling}", family.prefix()).as_str())
                    .into_owned();
            }
        }
        out
    };

    let mut schema = spec["input_schema"].clone();
    let takes_artifact = family
        .artifact_argument()
        .is_some_and(|arg| schema["properties"].get(arg).is_some());
    if let Some(properties) = schema["properties"].as_object_mut() {
        for arg in family.supplied_arguments() {
            properties.remove(*arg);
        }
    }
    if let Some(required) = schema["required"].as_array_mut() {
        required.retain(|r| !r.as_str().is_some_and(|r| family.supplied_arguments().contains(&r)));
    }

    Entry {
        spec: serde_json::json!({
            "name": format!("{}{server_name}", family.prefix()),
            "description": format!(
                "{}{}",
                rename(spec["description"].as_str().unwrap_or_default()),
                family.supplied_note()
            ),
            "input_schema": schema,
        }),
        family,
        server_name,
        takes_artifact,
    }
}

/// How to reach the Docker-hosted AEM the UBS verifier boots and drives. See
/// `docker/aem/README.md` for what each value is and how the image and data
/// volume are prepared.
///
/// Stored flat in the app settings blob under the serde names below: the
/// desktop app's settings embed this struct, and the MCP server reads the same
/// blob (see [`stored_verify_settings`]). An empty optional string means unset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AemVerifySettings {
    /// The AEM Forms image (ajila's private registry, pulled ahead of time
    /// after `az acr login`). Empty means not configured.
    #[serde(rename = "aem_verify_image")]
    pub image: String,
    #[serde(rename = "aem_verify_user")]
    pub user: String,
    #[serde(rename = "aem_verify_password")]
    pub password: String,
    /// The Docker volume holding the deployed UBS platform.
    #[serde(rename = "aem_verify_data_volume")]
    pub data_volume: String,
    /// The port AEM listens on inside the image (8080 for ajila's images).
    #[serde(rename = "aem_verify_container_port")]
    pub container_port: u16,
    /// The Docker platform to run the AEM image as.
    #[serde(rename = "aem_verify_platform")]
    pub platform: String,
    /// A separately running Redacto renderer to check before submitting.
    /// Empty when the renderer is installed into AEM itself (the default).
    #[serde(rename = "aem_verify_redacto_url")]
    pub redacto_url: String,
    /// The mandator to open forms with, for a form declaring several.
    #[serde(rename = "aem_verify_mandator")]
    pub mandator: String,
}

impl Default for AemVerifySettings {
    fn default() -> Self {
        Self {
            image: String::new(),
            user: "admin".into(),
            password: "admin".into(),
            data_volume: "u2s-aem-ubs-data".into(),
            container_port: 8080,
            platform: String::new(),
            redacto_url: String::new(),
            mandator: String::new(),
        }
    }
}

/// `value`, or `None` when it is blank.
fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

impl AemVerifySettings {
    fn profile(&self) -> Result<Profile, String> {
        let port = self.container_port.to_string();
        let platform = optional(&self.platform);
        let redacto_url = optional(&self.redacto_url);
        Profile::from_reader(|key| {
            let value = match key {
                "U2S_AEM_VERIFY_FORMAT" => Some("aem-ubs"),
                "U2S_AEM_VERIFY_IMAGE" => Some(self.image.trim()),
                "U2S_AEM_VERIFY_USER" => Some(self.user.trim()),
                "U2S_AEM_VERIFY_PASSWORD" => Some(self.password.as_str()),
                "U2S_AEM_VERIFY_SUBMIT" => Some("download"),
                "U2S_AEM_VERIFY_DATA_VOLUME" => Some(self.data_volume.trim()),
                "U2S_AEM_VERIFY_CONTAINER_PORT" => Some(port.as_str()),
                "U2S_AEM_VERIFY_PLATFORM" => platform.as_deref(),
                "U2S_AEM_VERIFY_REDACTO_URL" => redacto_url.as_deref(),
                _ => None,
            };
            value.map(str::to_string)
        })
        .map_err(|e| format!("the AEM verification settings are incomplete: {e}"))
    }

    fn server(&self, blobs: u2s_blob::BlobStore) -> Result<AemVerifyServer, String> {
        let config = ServerConfig {
            name: "u2s-aem-ubs-verify-mcp",
            version: env!("CARGO_PKG_VERSION"),
            expected_format: Some("aem-ubs"),
            driver: Arc::new(u2s_aem_ubs_verify_mcp::driver::UbsDriver::new(optional(
                &self.mandator,
            ))),
            tool_specs: u2s_aem_ubs_verify_mcp::specs::tool_specs(),
            manifest: Box::new(u2s_aem_ubs_verify_mcp::specs::manifest),
            instructions: Box::new(|_| String::new()),
        };
        AemVerifyServer::with_parts(Arc::new(config), self.profile()?, blobs)
    }
}

/// Where the Redacto verifier's throwaway Postgres comes from, and the
/// optional already-running platform it renders against. Stored like
/// [`AemVerifySettings`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactoVerifySettings {
    /// The public Postgres image dumps are imported into.
    #[serde(rename = "redacto_verify_postgres_image")]
    pub postgres_image: String,
    /// Base URL of a running Redacto platform's rendering endpoint. Empty
    /// means rendering is reported as skipped; the import check still runs.
    #[serde(rename = "redacto_verify_rendering_url")]
    pub rendering_url: String,
    #[serde(rename = "redacto_verify_user")]
    pub user: String,
    #[serde(rename = "redacto_verify_password")]
    pub password: String,
}

impl Default for RedactoVerifySettings {
    fn default() -> Self {
        Self {
            postgres_image: "postgres:16-alpine".into(),
            rendering_url: String::new(),
            user: "admin".into(),
            password: "admin".into(),
        }
    }
}

impl RedactoVerifySettings {
    fn profile(&self) -> RenderProfile {
        RenderProfile {
            name: "redacto-ubs",
            postgres_image: self.postgres_image.trim().to_string(),
            rendering_base_url: optional(&self.rendering_url),
            basic_auth: Some((self.user.trim().to_string(), self.password.clone())),
            render_format: "pdf-ua",
        }
    }
}

/// Key under which the desktop app stores its settings blob in `history.db`.
const APP_SETTINGS_KEY: &str = "app";

/// The verifier settings the desktop app stored, for a host without its own
/// settings (the MCP server); defaults when nothing is stored.
pub fn stored_verify_settings() -> (AemVerifySettings, RedactoVerifySettings) {
    let blob = crate::db::get_setting(APP_SETTINGS_KEY).unwrap_or_default();
    (
        serde_json::from_str(&blob).unwrap_or_default(),
        serde_json::from_str(&blob).unwrap_or_default(),
    )
}

/// Checks everything the AEM verifier needs before a run spends a token:
/// complete settings, a reachable Docker, the AEM and Chromium images present
/// locally, the data volume, and pdfium for reading the submitted PDF. Removes
/// containers a crashed earlier run left behind. Returns a short report, or
/// every problem found.
pub async fn aem_verify_readiness(settings: &AemVerifySettings) -> Result<String, String> {
    let mut problems = Vec::new();
    let profile = settings.profile();
    if let Err(e) = &profile {
        problems.push(e.clone());
    }
    if settings.data_volume.trim().is_empty() {
        problems.push("no AEM data volume is configured".into());
    }
    let docker = docker_problems(&mut problems).await;
    if let (Some(docker), Ok(profile)) = (&docker, &profile) {
        for image in [&profile.aem_image, &profile.chromium_image] {
            image_problem(docker, image, &mut problems).await;
        }
        match bollard::Docker::connect_with_local_defaults() {
            Ok(client) => {
                if client.inspect_volume(&settings.data_volume).await.is_err() {
                    problems.push(format!(
                        "the Docker volume {:?} does not exist; bake it with \
                         docker/aem/bake-ubs-platform.sh (see docker/aem/README.md)",
                        settings.data_volume
                    ));
                }
            }
            Err(e) => problems.push(format!("could not inspect Docker volumes: {e}")),
        }
    }
    pdf_problem(&mut problems);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    // Once per process, so a crashed earlier process's containers go but a
    // conversion already running in this one keeps its own.
    static CLEANED: AtomicBool = AtomicBool::new(false);
    if let Ok(profile) = &profile
        && !CLEANED.swap(true, Ordering::SeqCst)
    {
        u2s_aem_verify_core::server::remove_leftover_containers(profile).await;
    }
    Ok(format!(
        "AEM image {}, data volume {}, Docker reachable, pdfium loaded.",
        settings.image, settings.data_volume
    ))
}

/// The Redacto counterpart of [`aem_verify_readiness`]: a reachable Docker,
/// the Postgres image present locally, and pdfium.
pub async fn redacto_verify_readiness(settings: &RedactoVerifySettings) -> Result<String, String> {
    let mut problems = Vec::new();
    if settings.postgres_image.trim().is_empty() {
        problems.push("no Postgres image is configured for Redacto verification".into());
    }
    if let Some(docker) = docker_problems(&mut problems).await
        && !settings.postgres_image.trim().is_empty()
    {
        image_problem(&docker, &settings.postgres_image, &mut problems).await;
    }
    pdf_problem(&mut problems);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    Ok(format!(
        "Postgres image {}, Docker reachable, pdfium loaded{}.",
        settings.postgres_image,
        match optional(&settings.rendering_url) {
            Some(url) => format!(", rendering against {url}"),
            None => ", no rendering endpoint (rendering is skipped)".into(),
        }
    ))
}

/// Pulls the public images the verifiers run (Chromium, Postgres). The AEM
/// image lives in a private registry and has to be pulled by hand after
/// `az acr login`.
pub async fn pull_public_images(images: &[&str], platform: &str) -> Result<(), String> {
    let docker = DockerLifecycle::connect()
        .await
        .map_err(|e| format!("Docker is not reachable: {e}"))?;
    for image in images {
        docker
            .ensure_image(image, platform)
            .await
            .map_err(|e| format!("could not pull {image}: {e}"))?;
    }
    Ok(())
}

async fn docker_problems(problems: &mut Vec<String>) -> Option<DockerLifecycle> {
    match DockerLifecycle::connect().await {
        Ok(docker) if docker.is_reachable().await => Some(docker),
        Ok(_) | Err(_) => {
            problems.push("Docker is not reachable; start Docker Desktop or the Docker daemon".into());
            None
        }
    }
}

async fn image_problem(docker: &DockerLifecycle, image: &str, problems: &mut Vec<String>) {
    match docker.image_id(image).await {
        Ok(Some(_)) => {}
        Ok(None) => problems.push(format!(
            "the image {image} is not present locally; pull it first (`verify prepare` pulls the \
             public images, the AEM image needs `az acr login` and `docker pull`)"
        )),
        Err(e) => problems.push(format!("could not look up the image {image}: {e}")),
    }
}

fn pdf_problem(problems: &mut Vec<String>) {
    let scratch = std::env::temp_dir().join("blueprint-u2s-pdf-check");
    if let Err(e) = PdfRenderServer::with_parts(Limits::default(), BlobStore::new(scratch)) {
        problems.push(format!(
            "the PDF renderer cannot load pdfium ({e}); run scripts/fetch-pdfium.sh or ship \
             libpdfium next to the binary"
        ));
    }
}

/// Every AEM verifier session boots its AEM on the same data volume, so two
/// conversions must never drive one at the same time: the first to boot one
/// holds this until its run ends. Only guards this process; a CLI run and a
/// desktop-app run started at the same moment are not protected.
static AEM_VERIFIER_LEASE: AtomicBool = AtomicBool::new(false);

/// The AEM verifier tools that touch neither Docker nor AEM, and so need no
/// lease.
const OFFLINE_AEM_TOOLS: &[&str] = &["verify_status", "verify_package_check"];

/// The verifier a run checks its output with.
enum Verifier {
    Aem(AemVerifyServer),
    Redacto(RedactoVerifyServer),
}

/// The u2s servers of one conversion agent, plus the documents its calls may
/// open.
pub struct U2sTools {
    /// Holds the source PDFs the tools read, the artifacts the verifiers check
    /// and the blob store they all write. Removed with the agent.
    dir: tempfile::TempDir,
    data: XfaDataServer,
    render: XfaRenderServer,
    /// Started on the first `pdf_*` call: it loads pdfium, which the XFA tools
    /// do not need.
    pdf: Option<PdfRenderServer>,
    verifier: Option<Verifier>,
    /// Whether this agent holds [`AEM_VERIFIER_LEASE`].
    holds_aem_lease: bool,
    /// Canonical paths a `doc_path` argument may name, besides the PDFs in the
    /// blob store (which only a verifier writes).
    documents: HashSet<PathBuf>,
}

impl U2sTools {
    pub fn new() -> Result<Self, String> {
        register_fonts()?;
        let dir = tempfile::Builder::new()
            .prefix("blueprint-u2s-")
            .tempdir()
            .map_err(|e| format!("could not create the u2s working directory: {e}"))?;
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Ok(Self {
            data: XfaDataServer,
            render: XfaRenderServer::with_parts(Limits::default(), blobs),
            pdf: None,
            verifier: None,
            holds_aem_lease: false,
            documents: HashSet::new(),
            dir,
        })
    }

    pub fn attach_aem_verify(&mut self, settings: &AemVerifySettings) -> Result<(), String> {
        let server = settings.server(self.verify_blobs())?;
        self.verifier = Some(Verifier::Aem(server));
        Ok(())
    }

    pub fn attach_redacto_verify(&mut self, settings: &RedactoVerifySettings) {
        let server = RedactoVerifyServer::with_parts(settings.profile(), self.verify_blobs());
        self.verifier = Some(Verifier::Redacto(server));
    }

    fn verify_blobs(&self) -> u2s_blob::BlobStore {
        u2s_blob::BlobStore::new(self.dir.path().join("blobs"))
    }

    pub fn has_verifier(&self) -> bool {
        self.verifier.is_some()
    }

    /// Tears down whatever containers the verifier started, and frees the AEM
    /// verifier for the next conversion.
    pub async fn shutdown(&mut self) -> Result<(), String> {
        let result = match self.verifier.take() {
            Some(Verifier::Aem(server)) => server.shutdown().await,
            Some(Verifier::Redacto(server)) => server.shutdown().await,
            None => Ok(()),
        };
        self.release_aem_lease();
        result
    }

    /// Takes the process-wide AEM verifier for this agent, or says who has it.
    fn acquire_aem_lease(&mut self) -> Result<(), String> {
        if self.holds_aem_lease {
            return Ok(());
        }
        if AEM_VERIFIER_LEASE.swap(true, Ordering::SeqCst) {
            return Err("The AEM verifier is in use by another conversion running in this app. It \
                        frees up when that run ends; carry on with other checks and try again later."
                .into());
        }
        self.holds_aem_lease = true;
        Ok(())
    }

    fn release_aem_lease(&mut self) {
        if std::mem::take(&mut self.holds_aem_lease) {
            AEM_VERIFIER_LEASE.store(false, Ordering::SeqCst);
        }
    }

    /// Writes a document the tools may read and returns the path to pass as
    /// `doc_path`. Writing the same name twice under one `group` replaces it.
    pub fn add_document(&mut self, group: &str, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.write(group, name, bytes)?;
        self.documents.insert(path.clone());
        Ok(path)
    }

    fn write(&self, group: &str, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let file_name = Path::new(name)
            .file_name()
            .ok_or_else(|| format!("document name {name:?} has no file name"))?;
        let dir = self.dir.path().join(group);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let path = dir.join(file_name);
        std::fs::write(&path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        path.canonicalize()
            .map_err(|e| format!("could not resolve {}: {e}", path.display()))
    }

    /// Runs `name`. `artifact` is the run's latest build (the package, or the
    /// Redacto dump), handed to a verifier tool that checks one.
    pub async fn call(&mut self, name: &str, input: &Value, artifact: Option<Artifact>) -> ToolReply {
        let Some(entry) = entry(name) else {
            return ToolReply::Error(format!("Unknown tool: {name}"));
        };
        if let Err(e) = self.check_document(input) {
            return ToolReply::Error(e);
        }
        let mut input = input.clone();
        if let Some(object) = input.as_object_mut() {
            for arg in entry.family.supplied_arguments() {
                object.remove(*arg);
            }
        }
        if entry.takes_artifact {
            let Some(artifact) = artifact else {
                return ToolReply::Error(match entry.family {
                    Family::RedactoVerify => "No dump built yet; call build_redacto_dump first.",
                    _ => "No package built yet; call build_aem_package first.",
                }
                .into());
            };
            let path = match self.write("artifacts", artifact.file_name, &artifact.bytes) {
                Ok(path) => path,
                Err(e) => return ToolReply::Error(e),
            };
            if let (Some(object), Some(arg)) = (input.as_object_mut(), entry.family.artifact_argument()) {
                object.insert(arg.to_string(), Value::String(path.display().to_string()));
            }
        }

        let server_name = entry.server_name.clone();
        let result = match entry.family {
            Family::XfaData => {
                let server = self.data.clone();
                blocking(move || server.dispatch(&server_name, &input).map_err(|e| e.to_string())).await
            }
            Family::XfaRender => {
                let server = self.render.clone();
                blocking(move || server.dispatch(&server_name, &input).map_err(|e| e.to_string())).await
            }
            Family::PdfRender => {
                let server = match self.pdf_server() {
                    Ok(server) => server.clone(),
                    Err(e) => return ToolReply::Error(e),
                };
                blocking(move || server.dispatch(&server_name, &input).map_err(|e| e.to_string())).await
            }
            Family::AemVerify | Family::RedactoVerify => {
                if entry.family == Family::AemVerify
                    && !OFFLINE_AEM_TOOLS.contains(&server_name.as_str())
                    && matches!(self.verifier, Some(Verifier::Aem(_)))
                    && let Err(e) = self.acquire_aem_lease()
                {
                    return ToolReply::Error(e);
                }
                match &self.verifier {
                Some(Verifier::Aem(server)) if entry.family == Family::AemVerify => {
                    let server = server.clone();
                    spawned(async move { server.dispatch(&server_name, &input).await }).await
                }
                Some(Verifier::Redacto(server)) if entry.family == Family::RedactoVerify => {
                    let server = server.clone();
                    spawned(async move { server.dispatch(&server_name, &input).await }).await
                }
                _ => Err(format!(
                    "{name} is not available in this run: its verifier was not started"
                )),
                }
            }
        };
        match result {
            Ok(result) => reply_from_result(result),
            Err(message) => ToolReply::Error(message),
        }
    }

    fn pdf_server(&mut self) -> Result<&PdfRenderServer, String> {
        let server = match self.pdf.take() {
            Some(server) => server,
            None => {
                let blobs = BlobStore::new(self.dir.path().join("blobs"));
                PdfRenderServer::with_parts(Limits::default(), blobs).map_err(|e| {
                    format!("the PDF renderer is unavailable ({e}); run scripts/fetch-pdfium.sh")
                })?
            }
        };
        Ok(self.pdf.insert(server))
    }

    /// A `doc_path` must name a document this agent wrote, or a PDF a
    /// verifier put in the blob store.
    fn check_document(&self, input: &Value) -> Result<(), String> {
        let Some(doc_path) = input.get("doc_path") else {
            return Ok(());
        };
        let raw = doc_path
            .as_str()
            .ok_or("doc_path must be a string path from get_source_info")?;
        let blobs = self.dir.path().join("blobs").canonicalize().ok();
        let allowed = Path::new(raw).canonicalize().is_ok_and(|p| {
            self.documents.contains(&p)
                || (blobs.as_ref().is_some_and(|b| p.starts_with(b))
                    && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
        });
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "doc_path {raw:?} is not one of this run's documents; use a path from \
                 get_source_info or a PDF a verifier returned"
            ))
        }
    }
}

impl Drop for U2sTools {
    /// A run that ends without [`U2sTools::shutdown`] (a panic, say) must not
    /// keep the AEM verifier from every later conversion.
    fn drop(&mut self) {
        self.release_aem_lease();
    }
}

/// The run's latest build, as a verifier receives it.
pub struct Artifact {
    pub file_name: &'static str,
    pub bytes: Vec<u8>,
}

/// Runs synchronous tool work off the async workers; a panic becomes an error.
async fn blocking(
    work: impl FnOnce() -> Result<CallToolResult, String> + Send + 'static,
) -> Result<CallToolResult, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|join| Err(format!("the tool server failed: {join}")))
}

/// Runs async tool work as its own task; a panic becomes an error.
async fn spawned(
    work: impl std::future::Future<Output = Result<CallToolResult, String>> + Send + 'static,
) -> Result<CallToolResult, String> {
    tokio::spawn(work)
        .await
        .unwrap_or_else(|join| Err(format!("the tool server failed: {join}")))
}

/// Registers every profile's parser fonts with the u2s font manager, once per
/// process (the manager is process-global).
fn register_fonts() -> Result<(), String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            use u2s_xfa::xfa::font_manager::{get_font_manager, register_profile_font_data};

            let mut fonts = Vec::new();
            for profile in blueprint::list_profiles() {
                if let Ok(files) = blueprint::profile_font_files(&profile) {
                    fonts.extend(files);
                }
            }
            let fallback = fallback_font(&fonts)
                .ok_or("no profile ships parser fonts; the u2s renderer cannot lay out text")?;
            let manager = get_font_manager();
            let mut manager = manager
                .lock()
                .map_err(|e| format!("u2s font manager lock: {e}"))?;
            for (_, data) in &fonts {
                register_profile_font_data(&mut manager, data);
            }
            manager.set_fallback(fallback);
            Ok(())
        })
        .clone()
}

/// The face every unresolvable typeface falls back to: the first upright,
/// normal-weight one, else the first light one, else the first of all. A bold
/// or italic fallback would restyle every such run of text.
fn fallback_font(fonts: &[(String, &'static [u8])]) -> Option<&'static [u8]> {
    let stem = |s: &str| s.to_ascii_lowercase();
    let styled = |s: &str| ["bold", "italic", "oblique", "black"].iter().any(|w| stem(s).contains(w));
    fonts
        .iter()
        .find(|(s, _)| !styled(s) && !stem(s).contains("light"))
        .or_else(|| fonts.iter().find(|(s, _)| !styled(s)))
        .or_else(|| fonts.first())
        .map(|(_, data)| *data)
}

fn reply_from_result(result: CallToolResult) -> ToolReply {
    let blocks: Vec<ReplyBlock> = result
        .content
        .into_iter()
        .map(|content| match content {
            ContentBlock::Text(t) => ReplyBlock::Text(t.text),
            ContentBlock::Image(i) => ReplyBlock::Image {
                media_type: i.mime_type,
                data: i.data,
            },
            ContentBlock::ResourceLink(r) => ReplyBlock::Text(format!("[resource link: {}]", r.uri)),
            _ => ReplyBlock::Text("[non-text content omitted]".into()),
        })
        .collect();
    let text = || {
        blocks
            .iter()
            .filter_map(|b| match b {
                ReplyBlock::Text(t) => Some(t.as_str()),
                ReplyBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if result.is_error == Some(true) {
        let message = text();
        return ToolReply::Error(if message.is_empty() {
            "the tool reported an error".into()
        } else {
            message
        });
    }
    if blocks.iter().all(|b| matches!(b, ReplyBlock::Text(_))) {
        ToolReply::Text(text())
    } else {
        ToolReply::Blocks(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_pdf() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../u2s/crates/u2s-render-pdf/fixtures/generated/unicode-text.pdf")
    }

    /// A PDF a verifier put in the blob store is readable with the `pdf_*`
    /// tools; the same file anywhere else is not.
    #[tokio::test]
    async fn pdf_tools_read_verifier_output_and_nothing_else() {
        let mut tools = U2sTools::new().expect("the u2s tools start");
        let blobs = tools.dir.path().join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        let produced = blobs.join("submitted.pdf");
        std::fs::copy(fixture_pdf(), &produced).unwrap();

        let reply = tools
            .call(
                "pdf_render_page",
                &serde_json::json!({ "doc_path": produced.display().to_string(), "page": 1, "dpi": 72 }),
                None,
            )
            .await;
        match reply {
            ToolReply::Blocks(blocks) => assert!(
                blocks.iter().any(|b| matches!(b, ReplyBlock::Image { .. })),
                "the render must carry the page image"
            ),
            ToolReply::Error(e) => panic!("the verifier's PDF must render: {e}"),
            _ => panic!("expected image blocks"),
        }

        let outside = fixture_pdf().canonicalize().unwrap();
        match tools
            .call(
                "pdf_info",
                &serde_json::json!({ "doc_path": outside.display().to_string() }),
                None,
            )
            .await
        {
            ToolReply::Error(e) => assert!(e.contains("not one of this run's documents"), "{e}"),
            _ => panic!("a PDF outside the run must be refused"),
        }
    }

    /// Two conversions never drive the AEM verifier at once, and a run that
    /// ends, however it ends, frees it for the next.
    #[tokio::test]
    async fn only_one_conversion_holds_the_aem_verifier() {
        let mut first = U2sTools::new().unwrap();
        let mut second = U2sTools::new().unwrap();
        first.acquire_aem_lease().expect("a free verifier is taken");
        first.acquire_aem_lease().expect("taking it again is a no-op");
        let busy = second.acquire_aem_lease().unwrap_err();
        assert!(busy.contains("in use by another conversion"), "{busy}");

        first.shutdown().await.expect("nothing attached to tear down");
        second.acquire_aem_lease().expect("freed by the first run's shutdown");
        drop(second);
        let mut third = U2sTools::new().unwrap();
        third.acquire_aem_lease().expect("freed when the holder is dropped");
        third.release_aem_lease();
    }

    /// The two verifiers share tool names upstream; here each family carries
    /// its own prefix, mentions its siblings by the prefixed name, and is not
    /// offered the arguments the adapter supplies.
    #[test]
    fn verifier_specs_are_renamed_and_stripped() {
        let specs = tool_specs();
        for name in ["aem_verify_run", "aem_verify_open", "redacto_verify_run"] {
            let spec = specs.iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("{name}"));
            let properties = spec["input_schema"]["properties"].as_object().cloned().unwrap_or_default();
            for supplied in ["package", "package_path", "artifact_blob", "artifact_path", "session_id"] {
                assert!(!properties.contains_key(supplied), "{name} still offers {supplied}");
            }
        }
        assert!(!specs.iter().any(|s| s["name"] == "verify_run"));
        let open = specs.iter().find(|s| s["name"] == "aem_verify_open").unwrap();
        let description = open["description"].as_str().unwrap();
        assert!(description.contains("aem_verify_controls"), "{description}");
        assert!(!description.contains(" verify_controls"), "{description}");
        assert!(takes_artifact("aem_verify_run") && takes_artifact("redacto_verify_run"));
        assert!(!takes_artifact("aem_verify_controls"));
    }
}
