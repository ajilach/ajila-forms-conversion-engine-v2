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
use std::fs::{File, TryLockError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rmcp3::model::CallToolResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use u2s_aem_verify_core::profile::Profile;
use u2s_aem_verify_core::server::{AemVerifyServer, ServerConfig};
use u2s_redacto_ubs_verify_mcp::RedactoVerifyServer;
use u2s_redacto_verify_core::profile::RenderProfile;
use u2s_render_core::{BlobStore, Limits};
use u2s_render_pdf_mcp::PdfRenderServer;
use u2s_render_xfa_mcp::XfaRenderServer;
use u2s_verify_core::docker::{DockerLifecycle, RegistryCredentials};
use u2s_xfa_mcp::XfaDataServer;

use crate::OutputTarget;
use crate::conversion::ToolReply;

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

/// The prebaked AEM Forms image with the UBS platform, a private package of
/// the `ajilach` GitHub organization (`docker/aem/README.md`). It seeds its
/// own data volume, which the verifier names after the image. Pinned here
/// like pdfium in `agent/build.rs`: move it to the tag upstream's
/// `.env.example` names whenever the vendored u2s crates are re-synced.
pub const AEM_IMAGE: &str = "ghcr.io/ajilach/u2s-aem-ubs:2026-10-09.2";

/// What signs the GitHub CLI in with the scope pulling [`AEM_IMAGE`] needs.
const GH_LOGIN: &str = "gh auth login -s read:packages";

/// The Docker platform matching this host, for which [`AEM_IMAGE`] is
/// published (arm64 and amd64).
fn host_platform() -> &'static str {
    if cfg!(target_arch = "aarch64") { "linux/arm64" } else { "linux/amd64" }
}

/// How to reach the Docker-hosted AEM the UBS verifier boots and drives. See
/// `docker/aem/README.md` for what each value is. The image is
/// [`AEM_IMAGE`], pulled with the GitHub CLI's login ([`ensure_aem_image`]).
///
/// Stored flat in the app settings blob under the serde names below: the
/// desktop app's settings embed this struct. An empty optional string means unset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AemVerifySettings {
    #[serde(rename = "aem_verify_user")]
    pub user: String,
    #[serde(rename = "aem_verify_password")]
    pub password: String,
    /// The port AEM listens on inside the image (8080 for ajila's images).
    #[serde(rename = "aem_verify_container_port")]
    pub container_port: u16,
    /// The Docker platform to run the AEM image and Chromium as. Empty lets
    /// Docker decide; the default is this host's.
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
            user: "admin".into(),
            password: "admin".into(),
            container_port: 8080,
            platform: host_platform().into(),
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
    /// What the operator still has to set, by the settings' own names.
    fn missing(&self) -> Vec<String> {
        let mut missing: Vec<String> = [
            (&self.user, "the AEM user"),
            (&self.password, "the AEM password"),
        ]
        .into_iter()
        .filter(|(value, _)| value.trim().is_empty())
        .map(|(_, name)| format!("{name} is not set in the verification settings"))
        .collect();
        if self.container_port == 0 {
            missing.push("the AEM container port is not set in the verification settings".into());
        }
        missing
    }

    fn profile(&self) -> Result<Profile, String> {
        let port = self.container_port.to_string();
        let platform = optional(&self.platform);
        let redacto_url = optional(&self.redacto_url);
        Profile::from_reader(|key| {
            let value = match key {
                "U2S_AEM_VERIFY_FORMAT" => Some("aem-ubs"),
                "U2S_AEM_VERIFY_IMAGE" => Some(AEM_IMAGE),
                "U2S_AEM_VERIFY_USER" => Some(self.user.trim()),
                "U2S_AEM_VERIFY_PASSWORD" => Some(self.password.as_str()),
                "U2S_AEM_VERIFY_SUBMIT" => Some("download"),
                "U2S_AEM_VERIFY_CONTAINER_PORT" => Some(port.as_str()),
                "U2S_AEM_VERIFY_PLATFORM" => platform.as_deref(),
                "U2S_AEM_VERIFY_REDACTO_URL" => redacto_url.as_deref(),
                _ => None,
            };
            value.map(str::to_string)
        })
        .map_err(|e| format!("the AEM verification settings are invalid: {e}"))
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

/// The images of the Redacto platform the verifier boots per session
/// (`docker/redacto/README.md`), its Docker platform and the rendering
/// service's basic auth. Stored like [`AemVerifySettings`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactoVerifySettings {
    /// The platform's database.
    #[serde(rename = "redacto_verify_postgres_image")]
    pub postgres_image: String,
    /// The platform's Flyway migrations, run once per boot.
    #[serde(rename = "redacto_verify_migration_image")]
    pub migration_image: String,
    #[serde(rename = "redacto_verify_core_image")]
    pub core_image: String,
    #[serde(rename = "redacto_verify_rendering_image")]
    pub rendering_image: String,
    /// The Docker platform to run the platform images as. Empty lets the
    /// daemon decide.
    #[serde(rename = "redacto_verify_platform")]
    pub platform: String,
    #[serde(rename = "redacto_verify_user")]
    pub user: String,
    #[serde(rename = "redacto_verify_password")]
    pub password: String,
}

impl Default for RedactoVerifySettings {
    /// The images `ajila-redacto-platform`'s CI publishes, as upstream's
    /// `.env.example` names them.
    fn default() -> Self {
        Self {
            postgres_image: "postgres:16-alpine".into(),
            migration_image: "ajilaclouddev.azurecr.io/ajila-redacto-platform-ajila-redacto-migration:latest"
                .into(),
            core_image: "ajilaclouddev.azurecr.io/ajila-redacto-platform-ajila-redacto-core:latest".into(),
            rendering_image: "ajilaclouddev.azurecr.io/ajila-redacto-platform-ajila-redacto-rendering:latest"
                .into(),
            platform: String::new(),
            user: "admin".into(),
            password: "admin".into(),
        }
    }
}

/// The Redacto verifier's profile name, which is also the owner label on
/// every container and network it boots.
const REDACTO_PROFILE_NAME: &str = "redacto-ubs";

impl RedactoVerifySettings {
    fn profile(&self) -> Result<RenderProfile, String> {
        RenderProfile::from_reader(REDACTO_PROFILE_NAME, "U2S_REDACTO_VERIFY_UBS", "pdf-ua", |key| {
            let value = match key.strip_prefix("U2S_REDACTO_VERIFY_UBS_")? {
                "POSTGRES_IMAGE" => self.postgres_image.trim(),
                "MIGRATION_IMAGE" => self.migration_image.trim(),
                "CORE_IMAGE" => self.core_image.trim(),
                "RENDERING_IMAGE" => self.rendering_image.trim(),
                "PLATFORM" => self.platform.trim(),
                "USER" => self.user.trim(),
                "PASSWORD" => self.password.as_str(),
                _ => return None,
            };
            optional(value)
        })
        .map_err(|e| format!("the Redacto verification settings are incomplete: {e}"))
    }
}

/// Everything that keeps a target's runs from starting, one problem per entry,
/// each saying what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotReady(pub Vec<String>);

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("\n"))
    }
}

impl std::error::Error for NotReady {}

/// Checks everything a run for `target` needs before it spends a token: the
/// check rules' sandbox and the target's verifier (see
/// [`aem_verify_readiness`] and [`redacto_verify_readiness`]). Returns a short
/// report, or every problem found.
pub async fn readiness(
    target: OutputTarget,
    aem: &AemVerifySettings,
    redacto: &RedactoVerifySettings,
) -> Result<String, NotReady> {
    let rules = crate::rules::readiness(target);
    let verifier = match target {
        OutputTarget::Aem => aem_verify_readiness(aem).await,
        OutputTarget::Redacto => redacto_verify_readiness(redacto).await,
    };
    match (rules, verifier) {
        (Ok(rules), Ok(verifier)) => Ok(format!("Check rules: {rules}. {verifier}")),
        (rules, verifier) => {
            let mut problems = Vec::new();
            if let Err(e) = rules {
                problems.push(format!("the check rules cannot run: {e}"));
            }
            if let Err(NotReady(more)) = verifier {
                problems.extend(more);
            }
            Err(NotReady(problems))
        }
    }
}

/// Checks everything the AEM verifier needs: complete settings, a reachable
/// Docker, the Chromium image present locally, [`AEM_IMAGE`] present or the
/// GitHub CLI signed in to pull it when the run starts ([`ensure_aem_image`]),
/// and pdfium for reading the submitted PDF. Returns a short report, or every
/// problem found.
async fn aem_verify_readiness(settings: &AemVerifySettings) -> Result<String, NotReady> {
    let mut problems = settings.missing();
    let profile = if problems.is_empty() {
        settings.profile()
    } else {
        Err(String::new())
    };
    if let Err(e) = &profile
        && !e.is_empty()
    {
        problems.push(e.clone());
    }
    let docker = docker_problems(&mut problems).await;
    let mut image_state = "";
    if let (Some(docker), Ok(profile)) = (&docker, &profile) {
        image_problem(docker, &profile.chromium_image, &mut problems).await;
        match docker.image_id(AEM_IMAGE).await {
            Ok(Some(_)) => image_state = "present",
            // The same login the pull asks for: `gh auth token` alone can
            // still return a stored token while gh is signed out.
            Ok(None) => match github_login().await {
                Ok(_) => image_state = "pulled from GitHub when the run starts",
                Err(e) => problems.push(format!(
                    "the AEM image {AEM_IMAGE} is not present locally and is pulled with the \
                     GitHub CLI's login, but {e}"
                )),
            },
            Err(e) => problems.push(format!("could not look up the image {AEM_IMAGE}: {e}")),
        }
    }
    pdf_problem(&mut problems);
    if !problems.is_empty() {
        return Err(NotReady(problems));
    }
    Ok(format!("AEM image {AEM_IMAGE} ({image_state}), Docker reachable, pdfium loaded."))
}

/// Puts [`AEM_IMAGE`] on the Docker daemon, pulling it from GitHub's registry
/// with the GitHub CLI's login when the daemon does not have it yet. A run
/// calls this before it starts and does not start when it fails. Returns a
/// short report.
pub async fn ensure_aem_image(settings: &AemVerifySettings) -> Result<String, String> {
    let docker = DockerLifecycle::connect()
        .await
        .map_err(|e| format!("Docker is not reachable: {e}"))?;
    if docker
        .image_id(AEM_IMAGE)
        .await
        .map_err(|e| format!("could not look up the image {AEM_IMAGE}: {e}"))?
        .is_some()
    {
        return Ok(format!("The AEM image {AEM_IMAGE} is present."));
    }
    let credentials = github_login().await?;
    let platform = optional(&settings.platform).unwrap_or_else(|| host_platform().into());
    docker
        .ensure_image(AEM_IMAGE, &platform, Some(&credentials))
        .await
        .map_err(|e| pull_problem(AEM_IMAGE, &e.to_string()))?;
    Ok(format!("Pulled the AEM image {AEM_IMAGE} for {platform}."))
}

/// What to do about a failed pull of [`AEM_IMAGE`]: a refused login most
/// likely lacks the package scope or the package's read access.
fn pull_problem(image: &str, error: &str) -> String {
    let lower = error.to_lowercase();
    if ["denied", "unauthorized", "forbidden"].iter().any(|word| lower.contains(word)) {
        format!(
            "GitHub refused to pull {image} ({error}): give the GitHub CLI the package scope with \
             `gh auth refresh -s read:packages`, and ask for read access to the package in the \
             ajilach organization if that is not enough"
        )
    } else {
        format!("could not pull {image}: {error}")
    }
}

/// The GitHub CLI's signed-in account and its token, the login GitHub's
/// registry takes. Nothing of it is stored.
async fn github_login() -> Result<RegistryCredentials, String> {
    let token = gh(&["auth", "token"]).await?;
    let user = gh(&["api", "user", "--jq", ".login"]).await?;
    credentials_from_gh(&user, &token)
}

fn credentials_from_gh(user: &str, token: &str) -> Result<RegistryCredentials, String> {
    RegistryCredentials::new(user.trim().to_string(), token.trim().to_string())
        .ok_or_else(|| format!("the GitHub CLI reported no signed-in account; run `{GH_LOGIN}`"))
}

/// How long one GitHub CLI call may take; `gh api` reaches the network.
const GH_DEADLINE: Duration = Duration::from_secs(30);

/// Runs the GitHub CLI and returns its output, or what to do when it is not
/// installed, not signed in, or does not answer in time.
async fn gh(args: &[&str]) -> Result<String, String> {
    let command = format!("gh {}", args.join(" "));
    let output = tokio::time::timeout(
        GH_DEADLINE,
        tokio::process::Command::new(gh_program()).args(args).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| format!("`{command}` did not answer within {}s; check the network", GH_DEADLINE.as_secs()))?
    .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!("the GitHub CLI (gh) is not installed: install it (`brew install gh`), then run `{GH_LOGIN}`")
            }
            _ => format!("could not run the GitHub CLI: {e}"),
        })?;
    if !output.status.success() {
        return Err(gh_failure(&command, &String::from_utf8_lossy(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a failed GitHub CLI call means: a missing login says how to sign in,
/// anything else (the network, GitHub itself) is passed on as gh reported it.
fn gh_failure(command: &str, stderr: &str) -> String {
    let stderr = stderr.trim();
    let lower = stderr.to_lowercase();
    if ["gh auth login", "not logged in", "authentication", "bad credentials"]
        .iter()
        .any(|hint| lower.contains(hint))
    {
        format!("the GitHub CLI is not signed in (`{command}`: {stderr}); run `{GH_LOGIN}`")
    } else {
        format!("the GitHub CLI failed (`{command}`: {stderr})")
    }
}

/// Where the GitHub CLI is: on `PATH`, or where Homebrew or MacPorts install
/// it, since an app started from the Finder gets a `PATH` without them.
fn gh_program() -> PathBuf {
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>());
    let homebrew = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].into_iter().map(PathBuf::from);
    on_path
        .chain(homebrew)
        .map(|dir| dir.join("gh"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("gh"))
}

/// The Redacto counterpart of [`aem_verify_readiness`]: complete settings, a
/// reachable Docker, every platform image present locally, and pdfium.
async fn redacto_verify_readiness(settings: &RedactoVerifySettings) -> Result<String, NotReady> {
    let mut problems = Vec::new();
    let profile = settings.profile();
    if let Err(e) = &profile {
        problems.push(e.clone());
    }
    let docker = docker_problems(&mut problems).await;
    if let (Some(docker), Ok(profile)) = (&docker, &profile) {
        for image in profile.images.all() {
            image_problem(docker, image, &mut problems).await;
        }
    }
    pdf_problem(&mut problems);
    if !problems.is_empty() {
        return Err(NotReady(problems));
    }
    let images = profile.map(|p| p.images.all().join(", ")).unwrap_or_default();
    Ok(format!("Redacto platform images {images}, Docker reachable, pdfium loaded."))
}

/// Pulls the images the verifiers run where they are missing: the AEM
/// verifier's Chromium, the Redacto platform's Postgres, and [`AEM_IMAGE`]
/// with the GitHub CLI's login. The other Redacto platform images live in a
/// private Azure registry and have to be pulled by hand after `az acr login`
/// (see docker/redacto/README.md).
pub async fn pull_verifier_images(
    aem: &AemVerifySettings,
    redacto: &RedactoVerifySettings,
) -> Result<String, String> {
    // The Chromium image is the verifier's own default; reading it from the
    // profile keeps the two from drifting apart.
    let chromium = Profile::from_reader(|key| match key {
        "U2S_AEM_VERIFY_FORMAT" => Some("aem-ubs".into()),
        "U2S_AEM_VERIFY_IMAGE" | "U2S_AEM_VERIFY_USER" | "U2S_AEM_VERIFY_PASSWORD" => {
            Some("unused".into())
        }
        _ => None,
    })
    .map_err(|e| format!("could not read the verifier's defaults: {e}"))?
    .chromium_image;
    let platform = optional(&aem.platform).unwrap_or_else(|| host_platform().into());
    let docker = DockerLifecycle::connect()
        .await
        .map_err(|e| format!("Docker is not reachable: {e}"))?;
    let images = [chromium, redacto.postgres_image.trim().to_string()];
    for image in &images {
        docker
            .ensure_image(image, &platform, None)
            .await
            .map_err(|e| format!("could not pull {image}: {e}"))?;
    }
    let pulled = format!("Pulled {} for {platform}.", images.join(" and "));
    match ensure_aem_image(aem).await {
        Ok(aem_image) => Ok(format!("{pulled} {aem_image}")),
        Err(e) => Err(format!("{pulled} The AEM image was not pulled: {e}")),
    }
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
        Ok(None) => {
            let config = docker_config_path().and_then(|path| std::fs::read_to_string(path).ok());
            problems.push(missing_image_problem(image, config.as_deref()));
        }
        Err(e) => problems.push(format!("could not look up the image {image}: {e}")),
    }
}

/// The Docker CLI's config file, where `docker login` and `az acr login`
/// record a registry.
fn docker_config_path() -> Option<PathBuf> {
    match std::env::var_os("DOCKER_CONFIG") {
        Some(dir) => Some(PathBuf::from(dir).join("config.json")),
        None => dirs::home_dir().map(|home| home.join(".docker/config.json")),
    }
}

/// What to do about an image other than [`AEM_IMAGE`] that is not present
/// locally. A run does not pull these, so whether the registry is logged in
/// matters only here; `docker_config` is
/// the Docker CLI's config file, if there is one.
fn missing_image_problem(image: &str, docker_config: Option<&str>) -> String {
    let Some(registry) = registry_of(image) else {
        return format!(
            "the image {image} is not present locally and names no registry: pull it from Docker \
             Hub with `docker pull {image}` (Settings > Pull images pulls the public ones), or \
             tag a locally built image with this name"
        );
    };
    let login = match registry.strip_suffix(".azurecr.io") {
        Some(name) => format!("az acr login --name {name}"),
        None => format!("docker login {registry}"),
    };
    if docker_config.is_some_and(|config| logged_in(config, registry)) {
        format!(
            "the image {image} is not present locally; pull it with `docker pull {image}` (if \
             {registry} refuses, the login expired: run `{login}` again)"
        )
    } else {
        format!(
            "the image {image} is not present locally, and Docker is not logged in to \
             {registry}: run `{login}`, then `docker pull {image}`"
        )
    }
}

/// The registry host an image reference names, or `None` for Docker Hub: as
/// Docker reads it, the first path segment is a registry only if it looks
/// like a host.
fn registry_of(image: &str) -> Option<&str> {
    let (first, _) = image.split_once('/')?;
    (first.contains('.') || first.contains(':') || first == "localhost").then_some(first)
}

/// Whether the Docker CLI config records credentials for `registry`, either
/// in `auths` (the entry is empty when a credential store holds the secret) or
/// through a per-registry credential helper. Whether those credentials are
/// still valid only the registry knows.
fn logged_in(docker_config: &str, registry: &str) -> bool {
    let Ok(config) = serde_json::from_str::<Value>(docker_config) else {
        return false;
    };
    let auths = config["auths"].as_object().into_iter().flat_map(|auths| auths.keys());
    let helpers = config["credHelpers"].as_object().into_iter().flat_map(|helpers| helpers.keys());
    auths.chain(helpers).any(|key| {
        let host = key.trim_start_matches("https://").trim_start_matches("http://");
        host.trim_end_matches('/') == registry
    })
}

fn pdf_problem(problems: &mut Vec<String>) {
    let scratch = std::env::temp_dir().join("blueprint-u2s-pdf-check");
    if let Err(e) = crate::pdfium::server(BlobStore::new(scratch)) {
        problems.push(e);
    }
}

/// Every AEM verifier session boots its AEM on the same data volume, so two
/// conversions must never drive one at the same time, in this process or
/// another. The exclusive OS lock on this file, next to `history.db`, is what
/// says one does; the OS drops it with a crashed process.
const AEM_VERIFIER_LOCK: &str = "aem-verifier.lock";

/// Redacto verifier sessions each get their own platform, so any number may
/// run: each holds this file's lock shared. Only a process that can briefly
/// take it exclusively knows that no Redacto container anywhere is in use.
const REDACTO_VERIFIER_LOCK: &str = "redacto-verifier.lock";

/// How long a verifier may sit unused before its containers are torn down.
/// Matches upstream's own idle timeout.
const VERIFIER_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What a verifier call that may boot the platform gets on top of the
/// profile's boot timeout: installing the package and walking or rendering it.
const VERIFY_WORK_AFTER_BOOT: Duration = Duration::from_secs(10 * 60);

/// `aem_verify_submit`: upstream waits up to 90 s for the download, and the
/// Document of Record renders on top of that.
const VERIFY_SUBMIT_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// One interactive step on an open form (set, next, reset, screenshot...),
/// which takes seconds on a live AEM. Generous, because missing it costs a
/// reboot.
const VERIFY_STEP_DEADLINE: Duration = Duration::from_secs(3 * 60);

/// Tearing the verifier down talks to Docker, which may be what went away.
const VERIFY_TEARDOWN_DEADLINE: Duration = Duration::from_secs(2 * 60);

/// What a verifier call does, which decides its deadline and whether it needs
/// the verifier lock. Every verifier tool has one (`VerifyCall::of`), so a
/// call can never wait on a container that is gone for longer than its
/// deadline while it holds the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyCall {
    /// Needs no lock and boots nothing; bounded by its own Docker and HTTP
    /// timeouts, so it gets no deadline here.
    Offline,
    /// May boot the platform first.
    Boots,
    /// Submits the open form and renders what that produces.
    Submit,
    /// One step on the open form.
    Step,
}

impl VerifyCall {
    /// The class of the server's tool `server_name`, or `None` for a tool
    /// this adapter does not know, which is then refused.
    fn of(server_name: &str) -> Option<Self> {
        match server_name {
            "verify_status" | "verify_package_check" | "verify_dump_check" => Some(Self::Offline),
            "verify_run" | "verify_open" => Some(Self::Boots),
            "verify_submit" => Some(Self::Submit),
            "verify_controls" | "verify_set" | "verify_next" | "verify_prev" | "verify_reset"
            | "verify_screenshot" | "verify_close" => Some(Self::Step),
            _ => None,
        }
    }

    /// How long the call may run on a verifier whose platform boots within
    /// `boot_timeout`.
    fn deadline(self, boot_timeout: Duration) -> Option<Duration> {
        match self {
            Self::Offline => None,
            Self::Boots => Some(boot_timeout + VERIFY_WORK_AFTER_BOOT),
            Self::Submit => Some(VERIFY_SUBMIT_DEADLINE),
            Self::Step => Some(VERIFY_STEP_DEADLINE),
        }
    }
}

fn open_lock(name: &str) -> Result<File, String> {
    let dir = crate::db::state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(name);
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("could not open the verifier lock {}: {e}", path.display()))
}

/// What the idle watcher shares with the tools: when the verifier was last
/// used, and the verifier lock, which it releases when it tears the containers
/// down. Behind a mutex because the watcher is its own task.
struct Activity {
    last_used: Instant,
    lock: Option<File>,
}

/// The verifier a run checks its output with.
#[derive(Clone)]
enum Verifier {
    Aem(AemVerifyServer, Arc<Profile>),
    /// The server, and how long its platform may take to boot.
    Redacto(RedactoVerifyServer, Duration),
}

impl Verifier {
    /// Removes the verifier's containers, giving up after
    /// [`VERIFY_TEARDOWN_DEADLINE`].
    async fn shutdown(&self) -> Result<(), String> {
        let shutdown = async {
            match self {
                Verifier::Aem(server, _) => server.shutdown().await,
                Verifier::Redacto(server, _) => server.shutdown().await,
            }
        };
        tokio::time::timeout(VERIFY_TEARDOWN_DEADLINE, shutdown).await.unwrap_or_else(|_| {
            Err(format!("removing its containers did not finish within {VERIFY_TEARDOWN_DEADLINE:?}"))
        })
    }

    async fn dispatch(&self, server_name: &str, input: &Value) -> Result<CallToolResult, String> {
        match self {
            Verifier::Aem(server, _) => server.dispatch(server_name, input).await,
            Verifier::Redacto(server, _) => server.dispatch(server_name, input).await,
        }
    }

    /// How long the verifier's platform may take to boot.
    fn boot_timeout(&self) -> Duration {
        match self {
            Verifier::Aem(_, profile) => profile.boot_timeout,
            Verifier::Redacto(_, boot_timeout) => *boot_timeout,
        }
    }

    /// The call that boots a fresh platform after a teardown.
    fn reboot_call(&self) -> &'static str {
        match self {
            Verifier::Aem(..) => "aem_verify_open (or aem_verify_run)",
            Verifier::Redacto(..) => "redacto_verify_run",
        }
    }
}

/// Removes the verifier's containers and releases its lock for the next
/// conversion: what the idle watcher, the end of a run and a missed deadline
/// all do.
async fn tear_down(verifier: &Verifier, activity: &Mutex<Activity>) -> Result<(), String> {
    let result = verifier.shutdown().await;
    release(activity);
    result
}

/// Releases the verifier lock, so another conversion may boot the verifier.
fn release(activity: &Mutex<Activity>) {
    if let Ok(mut activity) = activity.lock() {
        activity.lock = None;
    }
}

/// Removes the containers a crashed run left behind, giving up after
/// [`VERIFY_TEARDOWN_DEADLINE`]: Docker may be what went away, and the caller
/// holds the agent.
async fn remove_leftovers(owner: &str, reach: &u2s_verify_core::session::Reach) -> Result<(), String> {
    tokio::time::timeout(VERIFY_TEARDOWN_DEADLINE, u2s_verify_core::session::remove_leftovers(owner, reach))
        .await
        .map_err(|_| {
            format!(
                "removing a previous run's verifier containers did not finish within \
                 {VERIFY_TEARDOWN_DEADLINE:?}; check that Docker is running, then try again"
            )
        })
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
    activity: Arc<Mutex<Activity>>,
    /// Tears the verifier's containers down once it sits idle; started with
    /// the first call that needs them.
    watcher: Option<tokio::task::JoinHandle<()>>,
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
        let render = XfaRenderServer::with_parts(Limits::default(), blobs)
            .map_err(|e| format!("the XFA renderer cannot start: {e}"))?;
        Ok(Self {
            data: XfaDataServer,
            render,
            pdf: None,
            verifier: None,
            activity: Arc::new(Mutex::new(Activity {
                last_used: Instant::now(),
                lock: None,
            })),
            watcher: None,
            documents: HashSet::new(),
            dir,
        })
    }

    pub fn attach_aem_verify(&mut self, settings: &AemVerifySettings) -> Result<(), String> {
        let server = settings.server(self.verify_blobs())?;
        self.verifier = Some(Verifier::Aem(server, Arc::new(settings.profile()?)));
        Ok(())
    }

    pub fn attach_redacto_verify(&mut self, settings: &RedactoVerifySettings) -> Result<(), String> {
        let profile = settings.profile()?;
        let boot_timeout = profile.boot_timeout;
        let server = RedactoVerifyServer::with_parts(Ok(profile), self.verify_blobs());
        self.verifier = Some(Verifier::Redacto(server, boot_timeout));
        Ok(())
    }

    fn verify_blobs(&self) -> u2s_blob::BlobStore {
        u2s_blob::BlobStore::new(self.dir.path().join("blobs"))
    }

    pub fn has_verifier(&self) -> bool {
        self.verifier.is_some()
    }

    /// Tears down whatever containers the verifier started, and releases its
    /// lock for the next conversion.
    pub async fn shutdown(&mut self) -> Result<(), String> {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
        match self.verifier.take() {
            Some(verifier) => tear_down(&verifier, &self.activity).await,
            None => {
                release(&self.activity);
                Ok(())
            }
        }
    }

    /// Takes the verifier's lock, if this agent does not hold it yet, before a
    /// call that boots containers. Leftovers of a crashed run are removed only
    /// while no other conversion anywhere can be using the verifier.
    async fn acquire_lock(&mut self) -> Result<(), String> {
        let held = self.activity.lock().map_err(poisoned)?.lock.is_some();
        if held {
            return Ok(());
        }
        let file = match &self.verifier {
            Some(Verifier::Aem(_, profile)) => {
                let file = open_lock(AEM_VERIFIER_LOCK)?;
                match file.try_lock() {
                    Ok(()) => {}
                    Err(TryLockError::WouldBlock) => {
                        return Err("The AEM verifier is in use by another conversion (in this app or \
                                    the CLI). It frees up when that run ends; carry \
                                    on with other checks and try again later."
                            .into());
                    }
                    Err(TryLockError::Error(e)) => {
                        return Err(format!("could not take the AEM verifier lock: {e}"));
                    }
                }
                remove_leftovers(&profile.format, &u2s_aem_verify_core::session::reach_of(profile)).await?;
                file
            }
            Some(Verifier::Redacto(..)) => {
                let file = open_lock(REDACTO_VERIFIER_LOCK)?;
                if file.try_lock().is_ok() {
                    remove_leftovers(
                        REDACTO_PROFILE_NAME,
                        &u2s_verify_core::session::Reach::from_self_container(None),
                    )
                    .await?;
                    file.unlock()
                        .map_err(|e| format!("could not release the Redacto verifier lock: {e}"))?;
                }
                file.lock_shared()
                    .map_err(|e| format!("could not take the Redacto verifier lock: {e}"))?;
                file
            }
            None => return Ok(()),
        };
        self.activity.lock().map_err(poisoned)?.lock = Some(file);
        Ok(())
    }


    /// Marks the verifier used now, and starts the idle watcher on first use.
    fn touch(&mut self) {
        if let Ok(mut activity) = self.activity.lock() {
            activity.last_used = Instant::now();
        }
        if self.watcher.is_some() {
            return;
        }
        let (Some(verifier), activity) = (self.verifier.clone(), Arc::clone(&self.activity)) else {
            return;
        };
        self.watcher = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let idle = activity.lock().is_ok_and(|a| {
                    a.lock.is_some() && a.last_used.elapsed() >= VERIFIER_IDLE_TIMEOUT
                });
                if idle && let Err(e) = tear_down(&verifier, &activity).await {
                    eprintln!("blueprint: the idle verifier could not be torn down: {e}");
                }
            }
        }));
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
        match self.start(name, input, artifact).await {
            Ok(work) => work.await,
            Err(refusal) => refusal,
        }
    }

    /// Everything [`Self::call`] needs from `self`, done now: the checks, the
    /// supplied arguments, the verifier lock, the server handle. What is left
    /// is the server's own work, which owns everything it touches, so a caller
    /// may run it after letting go of the agent.
    pub async fn start(
        &mut self,
        name: &str,
        input: &Value,
        artifact: Option<Artifact>,
    ) -> Result<impl std::future::Future<Output = ToolReply> + Send + 'static, ToolReply> {
        let Some(entry) = entry(name) else {
            return Err(ToolReply::Error(format!("Unknown tool: {name}")));
        };
        if let Err(e) = self.check_document(input) {
            return Err(ToolReply::Error(e));
        }
        let mut input = input.clone();
        if let Some(object) = input.as_object_mut() {
            for arg in entry.family.supplied_arguments() {
                object.remove(*arg);
            }
        }
        if entry.takes_artifact {
            let Some(artifact) = artifact else {
                return Err(ToolReply::Error(match entry.family {
                    Family::RedactoVerify => "No dump built yet; call build_redacto_dump first.",
                    _ => "No package built yet; call build_aem_package first.",
                }
                .into()));
            };
            let path = self
                .write("artifacts", artifact.file_name, &artifact.bytes)
                .map_err(ToolReply::Error)?;
            if let (Some(object), Some(arg)) = (input.as_object_mut(), entry.family.artifact_argument()) {
                object.insert(arg.to_string(), Value::String(path.display().to_string()));
            }
        }

        let server_name = entry.server_name.clone();
        let blobs = self.dir.path().join("blobs");
        let work: std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallToolResult, String>> + Send>> =
            match entry.family {
                Family::XfaData => {
                    let server = self.data.clone();
                    Box::pin(blocking(move || server.dispatch(&server_name, &input).map_err(|e| e.to_string())))
                }
                Family::XfaRender => {
                    let server = self.render.clone();
                    Box::pin(blocking(move || server.dispatch(&server_name, &input).map_err(|e| e.to_string())))
                }
                Family::PdfRender => {
                    let server = self.pdf_server().map_err(ToolReply::Error)?.clone();
                    Box::pin(blocking(move || server.dispatch(&server_name, &input).map_err(|e| e.to_string())))
                }
                Family::AemVerify | Family::RedactoVerify => {
                    let Some(call) = VerifyCall::of(&server_name) else {
                        return Err(ToolReply::Error(format!(
                            "{name} has no deadline in this engine, so it is not run"
                        )));
                    };
                    let verifier = match &self.verifier {
                        Some(verifier @ Verifier::Aem(..)) if entry.family == Family::AemVerify => verifier.clone(),
                        Some(verifier @ Verifier::Redacto(..)) if entry.family == Family::RedactoVerify => {
                            verifier.clone()
                        }
                        _ => {
                            return Err(ToolReply::Error(format!(
                                "{name} is not available in this run: its verifier was not started"
                            )));
                        }
                    };
                    if call != VerifyCall::Offline {
                        self.acquire_lock().await.map_err(ToolReply::Error)?;
                        self.touch();
                    }
                    let dispatch = {
                        let verifier = verifier.clone();
                        async move { verifier.dispatch(&server_name, &input).await }
                    };
                    match call.deadline(verifier.boot_timeout()) {
                        None => Box::pin(spawned(dispatch)),
                        Some(deadline) => {
                            let activity = Arc::clone(&self.activity);
                            let reboot = verifier.reboot_call();
                            let stop = move || async move { tear_down(&verifier, &activity).await };
                            Box::pin(within_deadline(name.to_string(), deadline, dispatch, stop, reboot))
                        }
                    }
                }
            };
        Ok(async move {
            match work.await {
                Ok(result) => crate::mcp_reply::reply_from_result(result, Some(&blobs)),
                Err(message) => ToolReply::Error(message),
            }
        })
    }

    fn pdf_server(&mut self) -> Result<&PdfRenderServer, String> {
        let server = match self.pdf.take() {
            Some(server) => server,
            None => {
                let blobs = BlobStore::new(self.dir.path().join("blobs"));
                crate::pdfium::server(blobs)?
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
    /// keep the verifier from every later conversion.
    fn drop(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
        release(&self.activity);
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> String {
    "the verifier state lock was poisoned by a panic".into()
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

/// A spawned tool call, aborted when its handle is dropped: a caller that
/// stops waiting (a stopped run, say) must not leave the call running, still
/// holding the verifier session.
struct CallTask<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for CallTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs async tool work as its own task; a panic becomes an error.
async fn spawned<T: Send + 'static>(
    work: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    let mut task = CallTask(tokio::spawn(work));
    joined((&mut task.0).await)
}

/// A spawned task's outcome, a panic as an error.
fn joined<T>(outcome: Result<Result<T, String>, tokio::task::JoinError>) -> Result<T, String> {
    outcome.unwrap_or_else(|join| Err(format!("the tool server failed: {join}")))
}

/// [`spawned`], within `deadline`: a call still running then is aborted (it
/// may hold the verifier session the teardown needs), the verifier is torn
/// down with `stop`, and the call is reported as an error naming `reboot`, the
/// call that boots a fresh verifier. `tool` names the call in that error.
async fn within_deadline<T, Stop, Stopped>(
    tool: String,
    deadline: Duration,
    work: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    stop: Stop,
    reboot: &'static str,
) -> Result<T, String>
where
    T: Send + 'static,
    Stop: FnOnce() -> Stopped,
    Stopped: std::future::Future<Output = Result<(), String>>,
{
    let mut task = CallTask(tokio::spawn(work));
    match tokio::time::timeout(deadline, &mut task.0).await {
        Ok(outcome) => joined(outcome),
        Err(_) => {
            drop(task);
            let stopped = match stop().await {
                Ok(()) => "its containers are removed".to_string(),
                Err(e) => format!("removing its containers failed: {e}"),
            };
            Err(format!(
                "{tool} did not answer within {deadline:?}, so the verifier was stopped and {stopped}. \
                 Whatever it had open is gone: {reboot} boots a fresh one."
            ))
        }
    }
}

/// Registers every profile's parser fonts with the u2s font manager, once per
/// process (the manager is process-global).
pub(crate) fn register_fonts() -> Result<(), String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            use u2s_xfa::xfa::font_manager::{get_font_manager, register_profile_font_data};

            let mut fonts = Vec::new();
            for profile in crate::profiles::list_profiles() {
                fonts.extend(crate::profiles::profile_font_files(&profile));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::ReplyBlock;

    fn fixture_pdf() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../vendor/crates/u2s-render-pdf/fixtures/generated/unicode-text.pdf")
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

    /// The verifier lock is an OS file lock: a second holder, in this process
    /// or another, is refused until the first lets go, and letting go is
    /// dropping the file. A test-only name keeps real conversions unaffected.
    #[test]
    fn a_verifier_lock_has_one_holder_at_a_time() {
        let first = open_lock("test-verifier.lock").unwrap();
        first.try_lock().expect("a free lock is taken");
        let second = open_lock("test-verifier.lock").unwrap();
        assert!(matches!(second.try_lock(), Err(TryLockError::WouldBlock)));
        drop(first);
        second.try_lock().expect("freed when the first holder is dropped");
    }

    /// Nothing is tested against Docker here, so every missing setting has to
    /// be named by the setting itself, all at once.
    #[tokio::test]
    async fn incomplete_aem_settings_name_every_missing_setting() {
        let settings = AemVerifySettings {
            user: String::new(),
            password: " ".into(),
            ..Default::default()
        };
        let err = aem_verify_readiness(&settings).await.unwrap_err().to_string();
        assert!(err.contains("the AEM user is not set"), "{err}");
        assert!(err.contains("the AEM password is not set"), "{err}");
        assert!(!err.contains("U2S_AEM_VERIFY"), "{err}");
    }

    /// The pinned image, not a setting, is what the verifier boots, and the
    /// data volume is the one upstream derives from it, so a new tag never
    /// boots on another tag's seeded volume.
    #[test]
    fn the_aem_profile_boots_the_pinned_image_on_its_own_volume() {
        let profile = AemVerifySettings::default().profile().expect("the defaults are complete");
        assert_eq!(profile.aem_image, AEM_IMAGE);
        assert_eq!(
            profile.aem_data_volume,
            u2s_aem_verify_core::profile::default_data_volume("aem-ubs", AEM_IMAGE)
        );
        assert_eq!(profile.registry_credentials, None, "the login is never kept in the profile");
    }

    #[test]
    fn a_gh_login_needs_both_an_account_and_a_token() {
        let credentials = credentials_from_gh("octocat\n", "gho_token\n").expect("a login");
        assert_eq!(credentials, RegistryCredentials::new("octocat".into(), "gho_token".into()).unwrap());
        for (user, token) in [("", "gho_token"), ("octocat", " \n")] {
            let err = credentials_from_gh(user, token).unwrap_err();
            assert!(err.contains(GH_LOGIN), "{err}");
        }
    }

    #[test]
    fn only_a_missing_gh_login_asks_to_sign_in() {
        let signed_out = gh_failure("gh api user", "To get started with GitHub CLI, please run:  gh auth login");
        assert!(signed_out.contains(GH_LOGIN), "{signed_out}");
        let offline = gh_failure("gh api user", "error connecting to api.github.com");
        assert!(!offline.contains(GH_LOGIN), "{offline}");
        assert!(offline.contains("api.github.com"), "{offline}");
    }

    #[test]
    fn a_refused_pull_names_the_package_scope() {
        let refused = pull_problem(AEM_IMAGE, "Docker responded with status code 500: denied: denied");
        assert!(refused.contains("gh auth refresh -s read:packages"), "{refused}");
        let offline = pull_problem(AEM_IMAGE, "error trying to connect: dns error");
        assert!(!offline.contains("read:packages"), "{offline}");
        assert!(offline.contains("dns error"), "{offline}");
    }

    /// Run with the GitHub CLI signed in and Docker running: removes a local
    /// copy of the AEM image and pulls it the way a run does.
    #[tokio::test]
    #[ignore = "needs Docker, a GitHub CLI login with read:packages, and downloads the AEM image"]
    async fn the_aem_image_is_pulled_with_the_gh_login() {
        let _ = std::process::Command::new("docker").args(["rmi", AEM_IMAGE]).status();
        let still_there = std::process::Command::new("docker")
            .args(["image", "inspect", AEM_IMAGE])
            .output()
            .expect("docker runs")
            .status
            .success();
        assert!(!still_there, "{AEM_IMAGE} is still on Docker (in use by a container?)");
        let report = ensure_aem_image(&AemVerifySettings::default()).await.expect("pulled");
        assert!(report.starts_with("Pulled"), "{report}");
        let again = ensure_aem_image(&AemVerifySettings::default()).await.expect("present");
        assert!(again.contains("is present"), "{again}");
    }

    #[test]
    fn a_registry_is_the_first_segment_only_when_it_looks_like_a_host() {
        assert_eq!(registry_of("ajilaclouddev.azurecr.io/redacto/core:1.2"), Some("ajilaclouddev.azurecr.io"));
        assert_eq!(registry_of("localhost:5000/aem:latest"), Some("localhost:5000"));
        assert_eq!(registry_of("postgres:16"), None);
        assert_eq!(registry_of("library/postgres:16"), None);
    }

    /// `az acr login` leaves an empty `auths` entry when a credential store
    /// holds the token, and a full one without; both count as logged in, any
    /// other registry does not.
    #[test]
    fn a_registry_login_is_read_from_the_docker_config() {
        let host = "ajilaclouddev.azurecr.io";
        let store = r#"{"auths": {"ajilaclouddev.azurecr.io": {}}, "credsStore": "desktop"}"#;
        let plain = r#"{"auths": {"https://ajilaclouddev.azurecr.io/": {"auth": "eDp5"}}}"#;
        let helper = r#"{"credHelpers": {"ajilaclouddev.azurecr.io": "acr-env"}}"#;
        let other = r#"{"auths": {"ghcr.io": {}}, "credsStore": "desktop"}"#;
        assert!(logged_in(store, host));
        assert!(logged_in(plain, host));
        assert!(logged_in(helper, host));
        assert!(!logged_in(other, host));
        assert!(!logged_in("not json", host));
    }

    #[test]
    fn a_missing_private_image_says_how_to_log_in_and_pull() {
        let image = "ajilaclouddev.azurecr.io/redacto/core:1.2";
        let logged_out = missing_image_problem(image, None);
        assert!(logged_out.contains("not logged in to ajilaclouddev.azurecr.io"), "{logged_out}");
        assert!(logged_out.contains("az acr login --name ajilaclouddev"), "{logged_out}");
        assert!(logged_out.contains(&format!("docker pull {image}")), "{logged_out}");

        let config = r#"{"auths": {"ajilaclouddev.azurecr.io": {}}}"#;
        let logged_in = missing_image_problem(image, Some(config));
        assert!(!logged_in.contains("not logged in"), "{logged_in}");
        assert!(logged_in.contains(&format!("docker pull {image}")), "{logged_in}");

        let public = missing_image_problem("postgres:16", None);
        assert!(!public.contains("login"), "{public}");
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

    /// Sets its flag when dropped, which is how a test sees that a spawned
    /// call's future was aborted rather than left running.
    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Regression guard: an `aem_verify_reset` on a verifier whose container
    /// Docker had removed never answered, and the agent lock it held stalled
    /// the whole run. A call past its deadline must end with an error, its
    /// task aborted (it held the verifier session) and the verifier torn down.
    #[tokio::test]
    async fn a_verifier_call_past_its_deadline_is_aborted_torn_down_and_reported() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = DropFlag(Arc::clone(&dropped));
        let torn_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tear_down = {
            let torn_down = Arc::clone(&torn_down);
            move || async move {
                torn_down.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        };

        let hung = async move {
            let _flag = flag;
            std::future::pending::<()>().await;
            Ok(())
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            within_deadline("aem_verify_reset".into(), Duration::from_millis(50), hung, tear_down, "aem_verify_open"),
        )
        .await
        .expect("the call must end at its deadline, not hang");

        let error = outcome.expect_err("a call past its deadline is an error");
        assert!(error.contains("aem_verify_reset") && error.contains("50ms"), "{error}");
        assert!(error.contains("aem_verify_open"), "the error must say how to recover: {error}");
        assert!(torn_down.load(std::sync::atomic::Ordering::SeqCst), "the verifier was not torn down");
        for _ in 0..100 {
            if dropped.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the hung call was left running instead of aborted");
    }

    /// A caller that stops waiting (a stopped run) takes its call down with
    /// it, rather than leaving it running on the verifier session.
    #[tokio::test]
    async fn a_verifier_call_whose_caller_stops_waiting_is_aborted() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = DropFlag(Arc::clone(&dropped));
        let hung = async move {
            let _flag = flag;
            std::future::pending::<()>().await;
            Ok(())
        };
        let call = within_deadline(
            "aem_verify_set".into(),
            Duration::from_secs(600),
            hung,
            || async { Ok(()) },
            "aem_verify_open",
        );
        let gave_up = tokio::time::timeout(Duration::from_millis(50), call).await;
        assert!(gave_up.is_err(), "the call should still be running when its caller gives up");
        for _ in 0..100 {
            if dropped.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the call was left running after its caller stopped waiting");
    }

    /// A call that answers in time keeps its result and leaves the verifier up.
    #[tokio::test]
    async fn a_verifier_call_within_its_deadline_keeps_its_result() {
        let torn_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tear_down = {
            let torn_down = Arc::clone(&torn_down);
            move || async move {
                torn_down.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        };
        let outcome =
            within_deadline("aem_verify_set".into(), Duration::from_secs(10), async { Ok(7) }, tear_down, "aem_verify_open").await;
        assert_eq!(outcome, Ok(7));
        assert!(!torn_down.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Every verifier tool the catalog offers is classified, so a tool added
    /// upstream cannot reach the model without a deadline; only the offline
    /// ones, which touch neither Docker nor the platform, run without one.
    #[test]
    fn every_verifier_tool_has_a_deadline() {
        let boot = Duration::from_secs(900);
        let mut seen = 0;
        for spec in tool_specs() {
            let name = spec["name"].as_str().unwrap();
            let Some(server_name) = name.strip_prefix("aem_").or_else(|| name.strip_prefix("redacto_")) else {
                continue;
            };
            seen += 1;
            let call = VerifyCall::of(server_name).unwrap_or_else(|| panic!("{name} has no deadline class"));
            let offline = matches!(server_name, "verify_status" | "verify_package_check" | "verify_dump_check");
            assert_eq!(call.deadline(boot).is_none(), offline, "{name}");
        }
        assert!(seen >= 15, "expected every aem_verify_* and redacto_verify_* tool, saw {seen}");
        let open = VerifyCall::of("verify_open").unwrap().deadline(boot).unwrap();
        assert!(open > boot, "a call that may boot the platform must outlast the boot");
    }
}
