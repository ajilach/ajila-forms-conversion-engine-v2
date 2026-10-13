//! The vendored u2s tool servers (see `u2s/VENDORED.md`), run in-process.
//!
//! Each server keeps its own tool contract: its specs go into the catalog
//! trimmed to the fields the model API takes, and its calls go through its own
//! `dispatch`. What this module adds is the edge: which files a call may read,
//! which artifact a verifier checks, where the fonts come from, and turning a
//! `CallToolResult` into a [`ToolReply`].
//!
//! The verifier's tools are offered as `aem_verify_*` (upstream names them
//! `verify_*`). They also lose the arguments this module supplies itself: the
//! package to check (the run's latest build) and the instance (the conversions
//! of one process share one verifier server, [`SharedVerifier`], whose one
//! instance is the server's default). The `session` handles of the forms an
//! agent opens are its own; this module only closes the ones that show an older
//! build before the next open or run, which upstream would otherwise refuse.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::fs::{File, TryLockError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rmcp3::model::CallToolResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use u2s_aem_verify_core::profile::Profile;
use u2s_aem_verify_core::server::{AemVerifyServer, ServerConfig};
use u2s_render_core::{BlobStore, Limits};
use u2s_render_pdf_mcp::PdfRenderServer;
use u2s_render_xfa_mcp::XfaRenderServer;
use u2s_verify_core::docker::{DockerLifecycle, RegistryCredentials};
use u2s_xfa_mcp::XfaDataServer;

use crate::container_engine::ContainerEngine;
use crate::conversion::{ReplyBlock, ToolReply};

/// The server a u2s tool belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    XfaData,
    XfaRender,
    PdfRender,
    AemVerify,
}

impl Family {
    /// The prefix the catalog name carries in front of the server's own name.
    fn prefix(self) -> &'static str {
        match self {
            Family::AemVerify => "aem_",
            Family::XfaData | Family::XfaRender | Family::PdfRender => "",
        }
    }

    /// Arguments this module supplies, so the model is never offered them.
    fn supplied_arguments(self) -> &'static [&'static str] {
        match self {
            Family::AemVerify => &["package", "package_path", "instance_id"],
            Family::XfaData | Family::XfaRender | Family::PdfRender => &[],
        }
    }

    /// Appended to the family's descriptions: the upstream wording names the
    /// arguments this module supplies, and the model must not try to pass them.
    fn supplied_note(self) -> &'static str {
        match self {
            Family::AemVerify => {
                " In this run the latest build_aem_package result is checked automatically and the \
                 instance is managed for you: pass no `package`, `package_path` or `instance_id`."
            }
            Family::XfaData | Family::XfaRender | Family::PdfRender => "",
        }
    }

    /// The argument the verified artifact's path is passed in.
    fn artifact_argument(self) -> Option<&'static str> {
        match self {
            Family::AemVerify => Some("package_path"),
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
        for property in properties.values_mut() {
            if let Some(description) = property["description"].as_str() {
                property["description"] = Value::String(rename(description));
            }
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
pub const AEM_IMAGE: &str = "ghcr.io/ajilach/u2s-aem-ubs:2026-10-10";

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

/// Everything that keeps a run from starting, one problem per entry, each
/// saying what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotReady(pub Vec<String>);

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("\n"))
    }
}

impl std::error::Error for NotReady {}

/// Checks everything a run needs before it spends a token: the check rules'
/// sandbox and the AEM verifier (see [`aem_verify_readiness`]). Returns a
/// short report, or every problem found.
pub async fn readiness(aem: &AemVerifySettings) -> Result<String, NotReady> {
    let rules = crate::rules::readiness();
    let verifier = aem_verify_readiness(aem).await;
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
    Ok(format!(
        "AEM image {AEM_IMAGE} ({image_state}), {} reachable, pdfium loaded.",
        crate::container_engine::selected().label()
    ))
}

/// Puts [`AEM_IMAGE`] on the Docker daemon, pulling it from GitHub's registry
/// with the GitHub CLI's login when the daemon does not have it yet. A run
/// calls this before it starts and does not start when it fails. Returns a
/// short report.
pub async fn ensure_aem_image(settings: &AemVerifySettings) -> Result<String, String> {
    let docker = connect_docker().await?;
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

/// Pulls the images the verifier runs where they are missing: its Chromium,
/// and [`AEM_IMAGE`] with the GitHub CLI's login.
pub async fn pull_verifier_images(aem: &AemVerifySettings) -> Result<String, String> {
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
    let docker = connect_docker().await?;
    docker
        .ensure_image(&chromium, &platform, None)
        .await
        .map_err(|e| format!("could not pull {chromium}: {e}"))?;
    let pulled = format!("Pulled {chromium} for {platform}.");
    match ensure_aem_image(aem).await {
        Ok(aem_image) => Ok(format!("{pulled} {aem_image}")),
        Err(e) => Err(format!("{pulled} The AEM image was not pulled: {e}")),
    }
}

/// How long the container engine has to answer a ping. A healthy daemon
/// answers in milliseconds; a hung Docker Desktop would otherwise hold the
/// check for bollard's own two-minute timeout.
pub const DOCKER_PING_DEADLINE: Duration = Duration::from_secs(10);

/// Connects to the container engine and pings it, or says why it cannot.
async fn connect_docker() -> Result<DockerLifecycle, String> {
    connect_within(DockerLifecycle::connect(), DOCKER_PING_DEADLINE).await
}

/// What [`connect_docker`] does with `connect`, the deadline given so a test
/// can stand in a daemon that never answers.
async fn connect_within<E: std::fmt::Display>(
    connect: impl std::future::Future<Output = Result<DockerLifecycle, E>>,
    deadline: Duration,
) -> Result<DockerLifecycle, String> {
    let hint = crate::container_engine::unreachable_hint();
    match tokio::time::timeout(deadline, connect).await {
        Ok(Ok(docker)) => Ok(docker),
        Ok(Err(e)) => Err(format!("{hint} ({e})")),
        Err(_) => Err(format!(
            "{hint} (no answer within {}s: if it is running, it looks hung, so quit and restart it)",
            deadline.as_secs()
        )),
    }
}

async fn docker_problems(problems: &mut Vec<String>) -> Option<DockerLifecycle> {
    let engine = crate::container_engine::problems();
    let connected = connect_docker().await;
    // An engine problem (a Podman socket not found, a Podman too old)
    // already says why nothing answers.
    let unreachable = match (&connected, engine.is_empty()) {
        (Err(e), true) => Some(e.clone()),
        _ => None,
    };
    problems.extend(engine);
    problems.extend(unreachable);
    connected.ok()
}

async fn image_problem(docker: &DockerLifecycle, image: &str, problems: &mut Vec<String>) {
    match docker.image_id(image).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            let engine = crate::container_engine::selected();
            let configs: Vec<String> = registry_config_paths(engine)
                .into_iter()
                .filter_map(|path| std::fs::read_to_string(path).ok())
                .collect();
            let configs: Vec<&str> = configs.iter().map(String::as_str).collect();
            problems.push(missing_image_problem(engine, image, &configs));
        }
        Err(e) => problems.push(format!("could not look up the image {image}: {e}")),
    }
}

/// The files the selected engine's CLI records registry logins in, most
/// specific first. Docker's is its `config.json` (`docker login`, `az acr
/// login`). Podman writes its own `auth.json` (`$REGISTRY_AUTH_FILE`, else
/// `$XDG_RUNTIME_DIR/containers/auth.json` on Linux, else
/// `~/.config/containers/auth.json`) and also reads Docker's, so both count.
fn registry_config_paths(engine: ContainerEngine) -> Vec<PathBuf> {
    let docker = match std::env::var_os("DOCKER_CONFIG") {
        Some(dir) => Some(PathBuf::from(dir).join("config.json")),
        None => dirs::home_dir().map(|home| home.join(".docker/config.json")),
    };
    let podman = match engine {
        ContainerEngine::Docker => Vec::new(),
        ContainerEngine::Podman => {
            let explicit = std::env::var_os("REGISTRY_AUTH_FILE").map(PathBuf::from);
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(|dir| PathBuf::from(dir).join("containers/auth.json"));
            let home = dirs::home_dir().map(|home| home.join(".config/containers/auth.json"));
            [explicit, runtime, home].into_iter().flatten().collect()
        }
    };
    podman.into_iter().chain(docker).collect()
}

/// What to do about an image other than [`AEM_IMAGE`] that is not present
/// locally, in the words of the selected `engine`'s CLI. A run does not pull
/// these, so whether the registry is logged in matters only here;
/// `registry_configs` are the contents of the files that CLI records logins
/// in ([`registry_config_paths`]).
fn missing_image_problem(engine: ContainerEngine, image: &str, registry_configs: &[&str]) -> String {
    let cli = engine.as_str();
    let Some(registry) = registry_of(image) else {
        return format!(
            "the image {image} is not present locally and names no registry: pull it from Docker \
             Hub with `{cli} pull {image}` (Settings > Pull images pulls the public ones), or \
             tag a locally built image with this name"
        );
    };
    let login = match (registry.strip_suffix(".azurecr.io"), engine) {
        (Some(name), ContainerEngine::Docker) => format!("az acr login --name {name}"),
        // `az acr login` logs in through the Docker CLI; Podman takes the
        // registry's token on stdin instead.
        (Some(name), ContainerEngine::Podman) => format!(
            "az acr login --name {name} --expose-token --query accessToken -o tsv | podman login \
             {registry} -u 00000000-0000-0000-0000-000000000000 --password-stdin"
        ),
        (None, _) => format!("{cli} login {registry}"),
    };
    if registry_configs.iter().any(|config| logged_in(config, registry)) {
        format!(
            "the image {image} is not present locally; pull it with `{cli} pull {image}` (if \
             {registry} refuses, the login expired: run `{login}` again)"
        )
    } else {
        format!(
            "the image {image} is not present locally, and {} is not logged in to \
             {registry}: run `{login}`, then `{cli} pull {image}`",
            engine.label()
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

/// Every AEM verifier boots its AEM on the same data volume, so two processes
/// must never drive one at the same time. The conversions of one process share
/// its one verifier ([`SharedVerifier`]), which holds the exclusive OS lock on
/// this file, next to `history.db`, while its containers may be up; the OS
/// drops it with a crashed process.
const AEM_VERIFIER_LOCK: &str = "aem-verifier.lock";

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

/// The call that boots a fresh platform after a teardown.
const VERIFIER_REBOOT_CALL: &str = "aem_verify_open (or aem_verify_run)";

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
            "verify_status" | "verify_package_check" => Some(Self::Offline),
            "verify_run" | "verify_open" => Some(Self::Boots),
            "verify_submit" => Some(Self::Submit),
            "verify_controls" | "verify_field" | "verify_set" | "verify_click" | "verify_next"
            | "verify_prev" | "verify_goto" | "verify_reset" | "verify_screenshot" | "verify_close" => {
                Some(Self::Step)
            }
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

/// A verifier call that opens or closes a session, or needs the open ones out
/// of its way, so [`U2sTools::call`] has to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCall {
    /// Opens the build in a new session: refused while a session shows another
    /// build of the same form.
    Open,
    /// Walks the build in one call, beside the open sessions: refused, like
    /// an open, while a session shows another build of the same form.
    Run,
    /// Closes one session.
    Close,
}

impl SessionCall {
    fn of(server_name: &str) -> Option<Self> {
        match server_name {
            "verify_open" => Some(Self::Open),
            "verify_run" => Some(Self::Run),
            "verify_close" => Some(Self::Close),
            _ => None,
        }
    }
}

/// Identifies a build's bytes within this process, to tell which open
/// sessions show the current build.
fn fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The sessions in `open` (handle to build [`fingerprint`]) to close before
/// `call` with the build `current`: for an open or a run, those on another
/// build; for a close, none.
fn stale_sessions(open: &BTreeMap<String, u64>, call: SessionCall, current: u64) -> Vec<String> {
    open.iter()
        .filter(|(_, build)| match call {
            SessionCall::Open | SessionCall::Run => **build != current,
            SessionCall::Close => false,
        })
        .map(|(session, _)| session.clone())
        .collect()
}

/// The `session` handle a successful `verify_open` reply carries.
pub(crate) fn opened_session(reply: &ToolReply) -> Option<String> {
    let texts: Vec<&str> = match reply {
        ToolReply::Text(text) => vec![text.as_str()],
        ToolReply::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ReplyBlock::Text(text) => Some(text.as_str()),
                ReplyBlock::Image { .. } => None,
            })
            .collect(),
        ToolReply::Error(_) => Vec::new(),
    };
    texts.into_iter().find_map(|text| {
        serde_json::from_str::<Value>(text).ok()?.get("session")?.as_str().map(str::to_string)
    })
}

/// `reply`, preceded by a line naming the sessions closed (or found gone)
/// before the call. The line is a block of its own, so a JSON reply stays
/// readable as JSON.
fn with_closed_note(reply: ToolReply, closed: &[String]) -> ToolReply {
    if closed.is_empty() {
        return reply;
    }
    let note = format!(
        "Closed {} session(s) first, which the verifier requires before this call: {}.",
        closed.len(),
        closed.join(", ")
    );
    with_note(reply, note)
}

/// `reply`, preceded by `note` in a block of its own.
fn with_note(reply: ToolReply, note: String) -> ToolReply {
    match reply {
        ToolReply::Text(text) => ToolReply::Blocks(vec![ReplyBlock::Text(note), ReplyBlock::Text(text)]),
        ToolReply::Blocks(mut blocks) => {
            blocks.insert(0, ReplyBlock::Text(note));
            ToolReply::Blocks(blocks)
        }
        ToolReply::Error(e) => ToolReply::Error(format!("{note}\n\n{e}")),
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

/// When the shared verifier was last used, and the verifier lock it holds
/// while its containers may be up.
struct Activity {
    last_used: Instant,
    lock: Option<File>,
}

/// The verifier a run checks its output with: the AEM verifier's server and
/// its profile.
#[derive(Clone)]
struct Verifier {
    server: AemVerifyServer,
    profile: Arc<Profile>,
}

impl Verifier {
    /// Removes the verifier's containers, giving up after
    /// [`VERIFY_TEARDOWN_DEADLINE`].
    async fn shutdown(&self) -> Result<(), String> {
        tokio::time::timeout(VERIFY_TEARDOWN_DEADLINE, self.server.shutdown()).await.unwrap_or_else(|_| {
            Err(format!("removing its containers did not finish within {VERIFY_TEARDOWN_DEADLINE:?}"))
        })
    }

    async fn dispatch(&self, server_name: &str, input: &Value) -> Result<CallToolResult, String> {
        self.server.dispatch(server_name, input).await
    }

    /// How long the verifier's platform may take to boot.
    fn boot_timeout(&self) -> Duration {
        self.profile.boot_timeout
    }

    /// The call that boots a fresh platform after a teardown.
    fn reboot_call(&self) -> &'static str {
        VERIFIER_REBOOT_CALL
    }
}

/// The AEM verifier of this process, which every conversion in it shares.
///
/// They all check their builds on its one AEM instance, each in sessions of
/// its own: upstream keeps an installed package per form path and runs one
/// call at a time on an instance, so different forms coexist on it. A second
/// server in the same process would boot a second AEM on the same data volume,
/// which [`AEM_VERIFIER_LOCK`] keeps out; this verifier holds that lock for all
/// of the process's conversions.
///
/// Interior mutability, by design: the conversions sharing it run on their own
/// tasks, and so does the idle watcher, so `activity` and `watcher` sit behind
/// mutexes and `turn` orders the calls.
struct SharedVerifier {
    /// What it was started with; a conversion asking for other settings is
    /// refused rather than given a second AEM.
    settings: AemVerifySettings,
    verifier: Verifier,
    /// Where the verifier writes the PDFs and screenshots it returns, for every
    /// conversion sharing it.
    blobs: tempfile::TempDir,
    activity: Mutex<Activity>,
    /// One verifier call at a time. Upstream runs them one at a time anyway
    /// (its instance lock); taking turns here first is what lets a call's
    /// deadline start when its turn comes, not while it waits behind another
    /// conversion's boot. Fair (FIFO), so no conversion is starved. Every
    /// teardown takes its turn too, so none pulls the containers from under a
    /// call.
    turn: tokio::sync::Mutex<()>,
    /// Tears the containers down once nobody used them for
    /// [`VERIFIER_IDLE_TIMEOUT`]; started with the first call that needs them.
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// How often the containers were torn down, so a conversion can tell that
    /// its sessions went with them although another one's deadline or the
    /// idle watcher did it ([`U2sTools::restarts_seen`]).
    restarts: std::sync::atomic::AtomicU64,
}

/// The one live `T` of a process and how many users hold it: the
/// bookkeeping behind the process's [`SharedVerifier`] (its [`LIVE`]).
struct Shares<T> {
    live: Option<(Arc<T>, usize)>,
}

impl<T> Shares<T> {
    const fn new() -> Self {
        Self { live: None }
    }

    /// The live `T`, counting one more user, when `fits` accepts it; a new
    /// one from `start` when none is live. One that does not fit is `Err`
    /// with `None` (the caller says why); a failed `start` is its error.
    fn join(
        &mut self,
        fits: impl FnOnce(&T) -> bool,
        start: impl FnOnce() -> Result<T, String>,
    ) -> Result<Arc<T>, Option<String>> {
        if let Some((live, users)) = self.live.as_mut() {
            if !fits(live) {
                return Err(None);
            }
            *users += 1;
            return Ok(Arc::clone(live));
        }
        let live = Arc::new(start().map_err(Some)?);
        self.live = Some((Arc::clone(&live), 1));
        Ok(live)
    }

    /// Counts one user of `shared` out; true when it was the last. It stays
    /// live until [`Self::forget_if_unused`], so a user joining in between
    /// gets it again.
    fn leave(&mut self, shared: &Arc<T>) -> bool {
        match self.live.as_mut() {
            Some((live, users)) if Arc::ptr_eq(live, shared) => {
                assert!(*users > 0, "a share was left more often than joined");
                *users -= 1;
                *users == 0
            }
            _ => false,
        }
    }

    /// Counts one more user of `shared`, when it is still the live one.
    fn rejoin(&mut self, shared: &Arc<T>) -> bool {
        match self.live.as_mut() {
            Some((live, users)) if Arc::ptr_eq(live, shared) => {
                *users += 1;
                true
            }
            _ => false,
        }
    }

    /// Forgets `shared` when nobody joined it since the last user left.
    fn forget_if_unused(&mut self, shared: &Arc<T>) {
        if self.live.as_ref().is_some_and(|(live, users)| Arc::ptr_eq(live, shared) && *users == 0) {
            self.live = None;
        }
    }
}

/// The process's [`SharedVerifier`] and how many conversions (or
/// [`VerifierHold`]s) hold it.
static LIVE: Mutex<Shares<SharedVerifier>> = Mutex::new(Shares::new());

/// [`LIVE`]. A panic while it was held cannot leave the counts half-changed
/// (each change is one statement), so a poisoned lock is taken over rather than
/// leaving the verifier never torn down.
fn live_shares() -> std::sync::MutexGuard<'static, Shares<SharedVerifier>> {
    LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl SharedVerifier {
    /// The process's verifier, started when nobody holds one, counting the
    /// caller as a user until [`Self::release`] or [`Self::abandon`]. One
    /// running on other settings is an error: a second one would boot a second
    /// AEM on the same data volume.
    fn join(settings: &AemVerifySettings) -> Result<Arc<Self>, String> {
        let start = || {
            let blobs = tempfile::Builder::new()
                .prefix("blueprint-aem-verify-")
                .tempdir()
                .map_err(|e| format!("could not create the verifier's working directory: {e}"))?;
            let server = settings.server(u2s_blob::BlobStore::new(blobs.path()))?;
            Ok(Self {
                settings: settings.clone(),
                verifier: Verifier { server, profile: Arc::new(settings.profile()?) },
                blobs,
                activity: Mutex::new(Activity { last_used: Instant::now(), lock: None }),
                turn: tokio::sync::Mutex::new(()),
                watcher: Mutex::new(None),
                restarts: std::sync::atomic::AtomicU64::new(0),
            })
        };
        live_shares().join(|live| live.settings == *settings, start).map_err(|e| {
            e.unwrap_or_else(|| {
                "another conversion in this process uses the AEM verifier with other verification \
                 settings; start this one once it has finished"
                    .into()
            })
        })
    }

    /// Counts one user of `shared` out; true when it was the last. It stays
    /// registered until [`Self::forget_if_unused`], so a conversion joining
    /// while the last one tears it down shares it (and waits its turn behind
    /// the teardown) instead of starting a second AEM beside it.
    fn leave(shared: &Arc<Self>) -> bool {
        live_shares().leave(shared)
    }

    /// Unregisters `shared` when nobody joined it since the last user left.
    fn forget_if_unused(shared: &Arc<Self>) {
        live_shares().forget_if_unused(shared);
    }

    /// One more share of `shared`, for work that must keep it from being torn
    /// down under it; `None` once its last user has let go.
    fn share(shared: &Arc<Self>) -> Option<VerifierHold> {
        live_shares().rejoin(shared).then(|| VerifierHold(Some(Arc::clone(shared))))
    }

    /// Lets go of one user's share; the last one tears the containers down and
    /// releases the verifier lock.
    async fn release(shared: Arc<Self>) -> Result<(), String> {
        if !Self::leave(&shared) {
            return Ok(());
        }
        let result = on_turn(&shared.turn, shared.tear_down()).await;
        Self::forget_if_unused(&shared);
        result
    }

    /// Lets go of a share without [`Self::release`] (a run that ended in a
    /// panic, say), which cannot reach Docker from a `Drop`: the verifier lock
    /// goes with the last share, and the next conversion that takes it removes
    /// the containers left behind.
    fn abandon(shared: &Arc<Self>) {
        if Self::leave(shared) {
            Self::forget_if_unused(shared);
        }
    }

    fn restarts(&self) -> u64 {
        self.restarts.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether this verifier holds the verifier lock, so its containers may be up.
    fn holds_lock(&self) -> bool {
        self.activity.lock().is_ok_and(|a| a.lock.is_some())
    }

    /// Removes the containers and releases the verifier lock, so another
    /// process may boot the verifier: what the idle watcher, the last user and
    /// a missed deadline do. Containers are only ever booted under the lock, so
    /// without it there is nothing to remove. Run on the caller's turn.
    async fn tear_down(&self) -> Result<(), String> {
        if !self.holds_lock() {
            return Ok(());
        }
        let result = self.verifier.shutdown().await;
        if let Ok(mut activity) = self.activity.lock() {
            activity.lock = None;
        }
        self.restarts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        result
    }

    /// Takes the verifier lock, unless it is held already, before a call that
    /// boots containers. Leftovers of a crashed run are removed only while no
    /// other process can be using the verifier. Run on the caller's turn, which
    /// keeps two calls of this process from both trying.
    async fn acquire_lock(&self) -> Result<(), String> {
        if self.holds_lock() {
            return Ok(());
        }
        let file = open_lock(AEM_VERIFIER_LOCK)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err("The AEM verifier is in use by another process (the app or the CLI). \
                            It frees up when that process's conversions end; carry on with \
                            other checks and try again later."
                    .into());
            }
            Err(TryLockError::Error(e)) => {
                return Err(format!("could not take the AEM verifier lock: {e}"));
            }
        }
        let profile = &self.verifier.profile;
        remove_leftovers(&profile.format, &u2s_aem_verify_core::instance::reach_of(profile)).await?;
        self.activity.lock().map_err(poisoned)?.lock = Some(file);
        Ok(())
    }

    /// Whether the containers sat unused for [`VERIFIER_IDLE_TIMEOUT`].
    fn is_idle(&self) -> bool {
        self.activity
            .lock()
            .is_ok_and(|a| a.lock.is_some() && a.last_used.elapsed() >= VERIFIER_IDLE_TIMEOUT)
    }

    /// Marks the verifier used now, and starts the idle watcher on first use.
    fn touch(self: &Arc<Self>) {
        if let Ok(mut activity) = self.activity.lock() {
            activity.last_used = Instant::now();
        }
        let Ok(mut watcher) = self.watcher.lock() else {
            return;
        };
        if watcher.is_some() {
            return;
        }
        let shared = Arc::downgrade(self);
        *watcher = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                if !shared.is_idle() {
                    continue;
                }
                let _turn = shared.turn.lock().await;
                if shared.is_idle() && let Err(e) = shared.tear_down().await {
                    eprintln!("blueprint: the idle verifier could not be torn down: {e}");
                }
            }
        }));
    }

    /// Runs the server's tool `server_name` (the catalog's `tool`). A call
    /// that needs the containers runs on its turn, within its deadline counted
    /// from then; one that misses it tears the verifier down.
    async fn call(
        self: Arc<Self>,
        tool: String,
        server_name: String,
        call: VerifyCall,
        input: Value,
    ) -> Result<CallToolResult, String> {
        let verifier = self.verifier.clone();
        let dispatch = async move { verifier.dispatch(&server_name, &input).await };
        let Some(deadline) = call.deadline(self.verifier.boot_timeout()) else {
            return spawned(dispatch).await;
        };
        on_turn(&self.turn, async {
            self.acquire_lock().await?;
            self.touch();
            let reboot = self.verifier.reboot_call();
            let result = within_deadline(tool, deadline, dispatch, || self.tear_down(), reboot).await;
            self.touch();
            result
        })
        .await
    }

    /// Boots the platform ahead of its first use, in the background and on
    /// its turn like any call; the receiver gets the outcome, a verifier lock
    /// another process holds included. With the platform up already (another
    /// conversion booted it), it only confirms that. The last user's teardown
    /// waits its turn behind a boot still under way. The boot holds a share of
    /// its own, so one that outlives every run is still torn down after it.
    fn start_warm_up(self: &Arc<Self>) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (sender, outcome) = tokio::sync::oneshot::channel();
        let Some(hold) = Self::share(self) else {
            let _ = sender.send(Err("the verifier was let go before it booted".into()));
            return outcome;
        };
        tokio::spawn(async move {
            let shared = Arc::clone(hold.shared());
            let booted = on_turn(&shared.turn, async {
                shared.acquire_lock().await?;
                shared.touch();
                let deadline = shared.verifier.boot_timeout();
                let booted = tokio::time::timeout(deadline, shared.verifier.server.warm_up(None))
                    .await
                    .unwrap_or_else(|_| Err(format!("the platform did not boot within {deadline:?}")));
                // The idle watcher measures from a platform that is up.
                shared.touch();
                booted
            })
            .await;
            // Nobody listening is fine: the outcome is only reported.
            let _ = sender.send(booted);
            if let Err(e) = hold.release().await {
                eprintln!("blueprint: the verifier could not be torn down after its boot: {e}");
            }
        });
        outcome
    }
}

impl Drop for SharedVerifier {
    fn drop(&mut self) {
        if let Ok(mut watcher) = self.watcher.lock()
            && let Some(watcher) = watcher.take()
        {
            watcher.abort();
        }
    }
}

/// `work`, started once `turn` is had: a deadline inside it counts from then,
/// not from when the caller started waiting.
async fn on_turn<T>(turn: &tokio::sync::Mutex<()>, work: impl std::future::Future<Output = T>) -> T {
    let _turn = turn.lock().await;
    work.await
}

/// A share of this process's AEM verifier. Each conversion holds one (through
/// [`U2sTools::attach_aem_verify`]); a batch of conversions holds one more for
/// its whole run, so the containers stay up between one form ending and the
/// next starting instead of being torn down and booted again. The last share
/// released tears them down.
pub struct VerifierHold(Option<Arc<SharedVerifier>>);

impl VerifierHold {
    /// Joins the process's verifier, starting it (without booting anything)
    /// when nobody holds one. Touches no Docker.
    pub fn new(settings: &AemVerifySettings) -> Result<Self, String> {
        Ok(Self(Some(SharedVerifier::join(settings)?)))
    }

    fn shared(&self) -> &Arc<SharedVerifier> {
        self.0.as_ref().expect("a hold keeps its verifier until it is released")
    }

    /// Lets go of this share; the last one tears the containers down.
    pub async fn release(mut self) -> Result<(), String> {
        match self.0.take() {
            Some(shared) => SharedVerifier::release(shared).await,
            None => Ok(()),
        }
    }
}

impl Drop for VerifierHold {
    /// A share dropped without [`Self::release`] must not keep the verifier
    /// lock from every later conversion.
    fn drop(&mut self) {
        if let Some(shared) = self.0.take() {
            SharedVerifier::abandon(&shared);
        }
    }
}

/// Removes the containers a crashed run left behind, giving up after
/// [`VERIFY_TEARDOWN_DEADLINE`]: Docker may be what went away, and the caller
/// holds the agent.
async fn remove_leftovers(owner: &str, reach: &u2s_verify_core::instance::Reach) -> Result<(), String> {
    tokio::time::timeout(VERIFY_TEARDOWN_DEADLINE, u2s_verify_core::instance::remove_leftovers(owner, reach))
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
    /// This agent's share of the process's AEM verifier.
    verifier: Option<VerifierHold>,
    /// Canonical paths a `doc_path` argument may name, besides the PDFs in the
    /// blob store (which only a verifier writes).
    documents: HashSet<PathBuf>,
    /// The verifier sessions this agent opened and has not closed, each with
    /// the [`fingerprint`] of the build it shows. Kept only by [`Self::call`],
    /// which every verifier tool goes through. A session the verifier lost
    /// (torn down idle or after a missed deadline) stays listed until the next
    /// open or run tries to close it.
    open_sessions: BTreeMap<String, u64>,
    /// The verifier's [`SharedVerifier::restarts`] when this agent last
    /// called it: once it moved on, the agent's sessions are gone.
    restarts_seen: u64,
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
            documents: HashSet::new(),
            open_sessions: BTreeMap::new(),
            restarts_seen: 0,
            dir,
        })
    }

    /// Joins the process's AEM verifier, which the agent's `aem_verify_*`
    /// calls then use. Touches no Docker.
    pub fn attach_aem_verify(&mut self, settings: &AemVerifySettings) -> Result<(), String> {
        let hold = VerifierHold::new(settings)?;
        self.restarts_seen = hold.shared().restarts();
        self.verifier = Some(hold);
        Ok(())
    }

    pub fn has_verifier(&self) -> bool {
        self.verifier.is_some()
    }

    /// Boots the verifier's platform ahead of its first use, in the
    /// background, and returns where the boot's outcome arrives. `Ok(None)`
    /// when no verifier is attached. See [`SharedVerifier::start_warm_up`].
    pub async fn start_warm_up(
        &mut self,
    ) -> Result<Option<tokio::sync::oneshot::Receiver<Result<(), String>>>, String> {
        Ok(self.verifier.as_ref().map(|hold| hold.shared().start_warm_up()))
    }

    /// Whether the verifier was torn down since this agent last called it (by
    /// the idle watcher, or by a call that missed its deadline, this agent's
    /// or another conversion's), and if so what to tell the model: its
    /// sessions are gone, so they are forgotten here too.
    fn notice_restart(&mut self) -> Option<String> {
        let restarts = self.verifier.as_ref()?.shared().restarts();
        if restarts == self.restarts_seen {
            return None;
        }
        self.restarts_seen = restarts;
        if self.open_sessions.is_empty() {
            return None;
        }
        let gone: Vec<String> = std::mem::take(&mut self.open_sessions).into_keys().collect();
        Some(format!(
            "The AEM verifier was restarted since your last verifier call (it sat idle, or a call \
             missed its deadline), so the sessions you had open are gone: {}. {} opens the form again.",
            gone.join(", "),
            VERIFIER_REBOOT_CALL
        ))
    }

    /// Closes the sessions this agent left open, so they do not hold the
    /// shared instance's browser for the conversions still running, and lets
    /// go of its share of the verifier: the last share tears the containers
    /// down and releases the verifier lock.
    pub async fn shutdown(&mut self) -> Result<(), String> {
        let Some(hold) = &self.verifier else {
            return Ok(());
        };
        // A session can only be open while the containers are up.
        if hold.shared().holds_lock() {
            let open = self.open_sessions.keys().cloned().collect();
            self.close_sessions(open).await;
        }
        match self.verifier.take() {
            Some(hold) => hold.release().await,
            None => Ok(()),
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

    /// Runs `name`. `artifact` is the run's latest build (the package), handed
    /// to a verifier tool that checks one.
    ///
    /// Upstream refuses to open or run a build while a session still shows
    /// another build of the same form, so the sessions [`stale_sessions`]
    /// names are closed first and the reply says so.
    pub async fn call(&mut self, name: &str, input: &Value, artifact: Option<Artifact>) -> ToolReply {
        let Some(call) = entry(name)
            .filter(|e| e.family == Family::AemVerify)
            .and_then(|e| SessionCall::of(&e.server_name))
        else {
            return self.run(name, input, artifact).await;
        };
        let build = artifact.as_ref().map(|a| fingerprint(&a.bytes));
        let closed = match build {
            Some(build) => self.close_sessions(stale_sessions(&self.open_sessions, call, build)).await,
            None => Vec::new(),
        };
        let reply = self.run(name, input, artifact).await;
        match call {
            SessionCall::Open => {
                if let (Some(session), Some(build)) = (opened_session(&reply), build) {
                    self.open_sessions.insert(session, build);
                }
            }
            SessionCall::Close => {
                if let Some(session) = input.get("session").and_then(Value::as_str) {
                    self.open_sessions.remove(session);
                }
            }
            SessionCall::Run => {}
        }
        with_closed_note(reply, &closed)
    }

    async fn run(&mut self, name: &str, input: &Value, artifact: Option<Artifact>) -> ToolReply {
        match self.prepare(name, input, artifact).await {
            Ok(work) => work.await,
            Err(refusal) => refusal,
        }
    }

    /// Closes `sessions`, forgetting each whether or not the verifier still had
    /// it, and returns one line per session for the reply.
    async fn close_sessions(&mut self, sessions: Vec<String>) -> Vec<String> {
        let mut closed = Vec::new();
        for session in sessions {
            let reply = self.run("aem_verify_close", &serde_json::json!({ "session": session }), None).await;
            self.open_sessions.remove(&session);
            closed.push(match reply {
                ToolReply::Error(e) => format!("{session} (could not be closed, so it is forgotten: {e})"),
                _ => session,
            });
        }
        closed
    }

    /// Everything [`Self::call`] needs from `self`, done now: the checks, the
    /// supplied arguments, the verifier lock, the server handle. What is left
    /// is the server's own work, which owns everything it touches, so a caller
    /// may run it after letting go of the agent. A tool that opens, closes or
    /// runs a verifier session is refused here: it must go through
    /// [`Self::call`], which keeps the agent's sessions.
    pub async fn start(
        &mut self,
        name: &str,
        input: &Value,
        artifact: Option<Artifact>,
    ) -> Result<impl std::future::Future<Output = ToolReply> + Send + 'static, ToolReply> {
        if entry(name).is_some_and(|e| e.family == Family::AemVerify && SessionCall::of(&e.server_name).is_some()) {
            return Err(ToolReply::Error(format!("{name} runs only on its own, not beside other calls")));
        }
        self.prepare(name, input, artifact).await
    }

    async fn prepare(
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
                return Err(ToolReply::Error("No package built yet; call build_aem_package first.".into()));
            };
            let path = self
                .write("artifacts", artifact.file_name, &artifact.bytes)
                .map_err(ToolReply::Error)?;
            if let (Some(object), Some(arg)) = (input.as_object_mut(), entry.family.artifact_argument()) {
                object.insert(arg.to_string(), Value::String(path.display().to_string()));
            }
        }

        let server_name = entry.server_name.clone();
        let blobs = match (entry.family, &self.verifier) {
            (Family::AemVerify, Some(hold)) => hold.shared().blobs.path().to_path_buf(),
            _ => self.dir.path().join("blobs"),
        };
        // What the reply says first: that the verifier restarted since this
        // agent last used it.
        let mut note = None;
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
                Family::AemVerify => {
                    let Some(call) = VerifyCall::of(&server_name) else {
                        return Err(ToolReply::Error(format!(
                            "{name} has no deadline in this engine, so it is not run"
                        )));
                    };
                    let restarted = self.notice_restart();
                    let Some(hold) = &self.verifier else {
                        return Err(ToolReply::Error(format!(
                            "{name} is not available in this run: its verifier was not started"
                        )));
                    };
                    note = restarted;
                    let shared = Arc::clone(hold.shared());
                    Box::pin(shared.call(name.to_string(), server_name, call, input))
                }
            };
        Ok(async move {
            let reply = match work.await {
                Ok(result) => crate::mcp_reply::reply_from_result(result, Some(&blobs)),
                Err(message) => ToolReply::Error(message),
            };
            match note {
                Some(note) => with_note(reply, note),
                None => reply,
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

    /// A `doc_path` must name a document this agent wrote, or a PDF in a blob
    /// store: this agent's, or the verifier's, which the conversions sharing
    /// it share.
    fn check_document(&self, input: &Value) -> Result<(), String> {
        let Some(doc_path) = input.get("doc_path") else {
            return Ok(());
        };
        let raw = doc_path
            .as_str()
            .ok_or("doc_path must be a string path from get_source_info")?;
        let blob_stores: Vec<PathBuf> = std::iter::once(self.dir.path().join("blobs"))
            .chain(self.verifier.as_ref().map(|hold| hold.shared().blobs.path().to_path_buf()))
            .filter_map(|dir| dir.canonicalize().ok())
            .collect();
        let allowed = Path::new(raw).canonicalize().is_ok_and(|p| {
            self.documents.contains(&p)
                || (blob_stores.iter().any(|b| p.starts_with(b))
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

    /// A daemon that never answers is reported within the deadline, as hung,
    /// and a failed connection carries its own error.
    #[tokio::test]
    async fn an_unresponsive_container_engine_is_reported_within_the_deadline() {
        let started = Instant::now();
        let hung = std::future::pending::<Result<DockerLifecycle, String>>();
        let err = connect_within(hung, Duration::from_millis(50)).await.err().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(err.contains("no answer within"), "{err}");

        let refused = async { Err::<DockerLifecycle, _>("connection refused") };
        let err = connect_within(refused, Duration::from_secs(5)).await.err().unwrap();
        assert!(err.contains("connection refused"), "{err}");
    }

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

    /// Every user of the process's one live value shares it; the last to
    /// leave says so, and only then is it forgotten, so a user joining while
    /// the last one tears it down still gets the same one.
    #[test]
    fn shares_hand_out_one_value_until_the_last_user_leaves() {
        let mut shares = Shares::<u32>::new();
        let first = shares.join(|_| true, || Ok(1)).unwrap();
        let second = shares.join(|_| true, || panic!("a live value is not started again")).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(shares.join(|live| *live == 2, || Ok(2)), Err(None)), "one that does not fit is refused");

        assert!(!shares.leave(&first));
        assert!(shares.leave(&second), "the second leaving is the last");
        let rejoined = shares.join(|_| true, || panic!("still live until forgotten")).unwrap();
        assert!(Arc::ptr_eq(&rejoined, &first));
        shares.forget_if_unused(&first);
        assert!(shares.live.is_some(), "a user joined since, so it stays");

        assert!(shares.leave(&rejoined));
        shares.forget_if_unused(&rejoined);
        let fresh = shares.join(|_| true, || Ok(3)).unwrap();
        assert_eq!(*fresh, 3);
        assert!(matches!(Shares::<u32>::new().join(|_| true, || Err("no".into())), Err(Some(e)) if e == "no"));
    }

    /// Two conversions of one process check their builds on one verifier, and
    /// one asking for other settings is refused instead of booting a second
    /// AEM on the same data volume. Attaching touches no Docker, and neither
    /// does letting go of a verifier that never booted.
    #[tokio::test]
    async fn conversions_in_one_process_share_one_verifier() {
        let settings = AemVerifySettings::default();
        let mut first = U2sTools::new().unwrap();
        let mut second = U2sTools::new().unwrap();
        first.attach_aem_verify(&settings).unwrap();
        second.attach_aem_verify(&settings).unwrap();
        let shared = |tools: &U2sTools| Arc::clone(tools.verifier.as_ref().unwrap().shared());
        assert!(Arc::ptr_eq(&shared(&first), &shared(&second)));

        let other = AemVerifySettings { user: "someone-else".into(), ..settings.clone() };
        let err = U2sTools::new().unwrap().attach_aem_verify(&other).unwrap_err();
        assert!(err.contains("other verification settings"), "{err}");

        let kept = shared(&second);
        first.shutdown().await.unwrap();
        assert!(!first.has_verifier());
        let mut third = U2sTools::new().unwrap();
        third.attach_aem_verify(&settings).unwrap();
        assert!(Arc::ptr_eq(&shared(&third), &kept), "a share still held keeps the verifier");
        second.shutdown().await.unwrap();
        third.shutdown().await.unwrap();
    }

    /// Another conversion's missed deadline (or the idle watcher) tears down
    /// the AEM this conversion's sessions were on: its next verifier call says
    /// so, once, and the sessions are forgotten rather than closed in vain.
    #[tokio::test]
    async fn a_restart_by_anyone_is_noticed_once_by_a_conversion_with_open_sessions() {
        let mut tools = U2sTools::new().unwrap();
        tools.attach_aem_verify(&AemVerifySettings::default()).unwrap();
        assert_eq!(tools.notice_restart(), None);
        tools.open_sessions.insert("sess_a".into(), 1);
        tools.verifier.as_ref().unwrap().shared().restarts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let note = tools.notice_restart().expect("the restart is noticed");
        assert!(note.contains("sess_a") && note.contains("aem_verify_open"), "{note}");
        assert!(tools.open_sessions.is_empty());
        assert_eq!(tools.notice_restart(), None, "only once");
        tools.shutdown().await.unwrap();
    }

    /// A call waiting its turn behind another conversion's long call does not
    /// spend its deadline waiting: the deadline counts from its turn.
    #[tokio::test]
    async fn a_deadline_counts_from_the_calls_turn() {
        let turn = Arc::new(tokio::sync::Mutex::new(()));
        let held = turn.clone().lock_owned().await;
        let waiting = tokio::spawn({
            let turn = turn.clone();
            async move {
                let stop = || async { panic!("the call made its deadline, nothing is torn down") };
                on_turn(&turn, within_deadline("aem_verify_set".into(), Duration::from_millis(100), async { Ok(7) }, stop, "x"))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(held);
        assert_eq!(waiting.await.unwrap(), Ok(7));
    }

    /// The PDFs the shared verifier returns live in its own blob store, which
    /// every conversion sharing it may read with the `pdf_*` tools.
    #[tokio::test]
    async fn pdf_tools_read_the_shared_verifiers_output() {
        let mut tools = U2sTools::new().unwrap();
        tools.attach_aem_verify(&AemVerifySettings::default()).unwrap();
        let produced = tools.verifier.as_ref().unwrap().shared().blobs.path().join("dor.pdf");
        std::fs::copy(fixture_pdf(), &produced).unwrap();
        let reply = tools
            .call("pdf_info", &serde_json::json!({ "doc_path": produced.display().to_string() }), None)
            .await;
        assert!(!matches!(reply, ToolReply::Error(_)), "the shared verifier's PDF must be readable");
        let _ = std::fs::remove_file(&produced);
        tools.shutdown().await.unwrap();
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
        let logged_out = missing_image_problem(ContainerEngine::Docker, image, &[]);
        assert!(logged_out.contains("Docker is not logged in to ajilaclouddev.azurecr.io"), "{logged_out}");
        assert!(logged_out.contains("az acr login --name ajilaclouddev`"), "{logged_out}");
        assert!(logged_out.contains(&format!("docker pull {image}")), "{logged_out}");

        let config = r#"{"auths": {"ajilaclouddev.azurecr.io": {}}}"#;
        let logged_in = missing_image_problem(ContainerEngine::Docker, image, &["not json", config]);
        assert!(!logged_in.contains("not logged in"), "{logged_in}");
        assert!(logged_in.contains(&format!("docker pull {image}")), "{logged_in}");

        let public = missing_image_problem(ContainerEngine::Docker, "postgres:16", &[]);
        assert!(!public.contains("login"), "{public}");
    }

    /// Under Podman the hint names Podman and its commands, never Docker's:
    /// `az acr login` alone would log the Docker CLI in, not Podman.
    #[test]
    fn a_missing_image_under_podman_says_podman() {
        let image = "ajilaclouddev.azurecr.io/redacto/core:1.2";
        let logged_out = missing_image_problem(ContainerEngine::Podman, image, &[]);
        assert!(logged_out.contains("Podman is not logged in to ajilaclouddev.azurecr.io"), "{logged_out}");
        assert!(logged_out.contains("--expose-token"), "{logged_out}");
        assert!(logged_out.contains("podman login ajilaclouddev.azurecr.io"), "{logged_out}");
        assert!(logged_out.contains(&format!("podman pull {image}")), "{logged_out}");
        assert!(!logged_out.contains("docker"), "{logged_out}");
        assert!(!logged_out.contains("Docker"), "{logged_out}");

        let auth = r#"{"auths": {"ajilaclouddev.azurecr.io": {"auth": "eDp5"}}}"#;
        let logged_in = missing_image_problem(ContainerEngine::Podman, image, &[auth]);
        assert!(!logged_in.contains("not logged in"), "{logged_in}");
        assert!(logged_in.contains(&format!("podman pull {image}")), "{logged_in}");

        let public = missing_image_problem(ContainerEngine::Podman, "postgres:16", &[]);
        assert!(public.contains("podman pull postgres:16"), "{public}");
    }

    /// The verifier's tools carry the `aem_` prefix, mention their siblings by
    /// the prefixed name, and are not offered the arguments the adapter
    /// supplies.
    #[test]
    fn verifier_specs_are_renamed_and_stripped() {
        let specs = tool_specs();
        for name in ["aem_verify_run", "aem_verify_open"] {
            let spec = specs.iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("{name}"));
            let properties = spec["input_schema"]["properties"].as_object().cloned().unwrap_or_default();
            for supplied in ["package", "package_path"] {
                assert!(!properties.contains_key(supplied), "{name} still offers {supplied}");
            }
        }
        for spec in specs.iter().filter(|s| s["name"].as_str().is_some_and(|n| n.starts_with("aem_verify_"))) {
            let properties = &spec["input_schema"]["properties"];
            assert!(properties.get("instance_id").is_none(), "{} still offers instance_id", spec["name"]);
        }
        let controls = specs.iter().find(|s| s["name"] == "aem_verify_controls").unwrap();
        assert!(controls["input_schema"]["properties"].get("session").is_some(), "the session handle is the agent's own");
        assert!(!specs.iter().any(|s| s["name"] == "verify_run"));
        let open = specs.iter().find(|s| s["name"] == "aem_verify_open").unwrap();
        let description = open["description"].as_str().unwrap();
        assert!(description.contains("aem_verify_controls"), "{description}");
        assert!(!description.contains(" verify_controls"), "{description}");
        let set = specs.iter().find(|s| s["name"] == "aem_verify_set").unwrap();
        let field = set["input_schema"]["properties"]["field"]["description"].as_str().unwrap();
        assert!(field.contains("aem_verify_controls"), "argument descriptions are renamed too: {field}");
        assert!(takes_artifact("aem_verify_run"));
        assert!(!takes_artifact("aem_verify_controls"));
    }

    #[test]
    fn an_open_closes_only_the_sessions_on_another_build() {
        let open = BTreeMap::from([("a".to_string(), 1), ("b".to_string(), 2), ("c".to_string(), 1)]);
        assert_eq!(stale_sessions(&open, SessionCall::Open, 1), vec!["b".to_string()]);
        assert_eq!(stale_sessions(&open, SessionCall::Open, 3), vec!["a", "b", "c"]);
        assert!(stale_sessions(&BTreeMap::new(), SessionCall::Open, 1).is_empty());
    }

    /// A run walks beside the open sessions upstream, so like an open it
    /// closes only those on another build.
    #[test]
    fn a_run_closes_only_the_sessions_on_another_build_and_a_close_none() {
        let open = BTreeMap::from([("a".to_string(), 1), ("b".to_string(), 2)]);
        assert_eq!(stale_sessions(&open, SessionCall::Run, 1), vec!["b".to_string()]);
        assert!(stale_sessions(&open, SessionCall::Close, 3).is_empty());
    }

    #[test]
    fn the_opened_session_is_read_from_the_reply() {
        let reply = ToolReply::Blocks(vec![ReplyBlock::Text(r#"{"session":"s-1","revision":0}"#.into())]);
        assert_eq!(opened_session(&reply).as_deref(), Some("s-1"));
        assert_eq!(opened_session(&ToolReply::Error("refused".into())), None);
        assert_eq!(opened_session(&ToolReply::Text("not json".into())), None);
    }

    #[test]
    fn the_reply_names_the_closed_sessions_first() {
        let reply = with_closed_note(ToolReply::Blocks(vec![ReplyBlock::Text("{}".into())]), &["s-1".into()]);
        let ToolReply::Blocks(blocks) = reply else { panic!("blocks stay blocks") };
        assert!(matches!(&blocks[0], ReplyBlock::Text(t) if t.contains("s-1")), "{blocks:?}");
        assert!(matches!(with_closed_note(ToolReply::Text("x".into()), &[]), ToolReply::Text(t) if t == "x"));
    }

    /// The note must not break the reply it precedes: the stage evidence reads
    /// the opened session from the reply the model sees.
    #[test]
    fn a_noted_open_reply_still_names_its_session() {
        let reply = ToolReply::Text(r#"{"session":"s-2","revision":0}"#.into());
        assert_eq!(opened_session(&with_closed_note(reply, &["s-1".into()])).as_deref(), Some("s-2"));
    }

    /// The tools that open or close a session go through `call`, which keeps
    /// the agent's sessions, never beside other calls.
    #[tokio::test]
    async fn session_tools_are_refused_beside_other_calls() {
        let mut tools = U2sTools::new().expect("tools");
        for name in ["aem_verify_open", "aem_verify_close", "aem_verify_run"] {
            let Err(ToolReply::Error(e)) = tools.start(name, &serde_json::json!({}), None).await else {
                panic!("{name} was not refused");
            };
            assert!(e.contains("on its own"), "{e}");
        }
    }

    /// Regression guard for the verifier's instance/session model: a session
    /// still showing an older build of the form makes upstream refuse to open
    /// the new one. Opening a rebuilt package must close it first and succeed.
    #[tokio::test]
    #[ignore = "needs Docker and the AEM image"]
    async fn opening_a_rebuilt_package_closes_the_session_on_the_old_build() {
        let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../vendor/crates/u2s-aem-ubs-mcp/tests/fixtures/golden/AABF_019/document.json");
        let text = std::fs::read_to_string(golden).unwrap();
        let build = |text: &str| Artifact {
            file_name: "package.zip",
            bytes: u2s_aem_ubs_mcp::encode(
                &u2s_aem_ubs_mcp::UbsAemDocument::from_json(&serde_json::from_str(text).unwrap()).unwrap(),
            )
            .unwrap()
            .package,
        };
        let first = build(&text);
        let second = build(&text.replace("UBS Europe SE", "UBS Europe SE (rebuilt)"));
        assert_ne!(fingerprint(&first.bytes), fingerprint(&second.bytes));

        let mut tools = U2sTools::new().expect("tools");
        tools.attach_aem_verify(&AemVerifySettings::default()).expect("verifier");
        let opened = tools.call("aem_verify_open", &serde_json::json!({}), Some(first)).await;
        let old = opened_session(&opened).unwrap_or_else(|| panic!("first open: {opened:?}"));

        let reopened = tools.call("aem_verify_open", &serde_json::json!({}), Some(second)).await;
        let new = opened_session(&reopened).unwrap_or_else(|| panic!("second open: {reopened:?}"));
        let note = match &reopened {
            ToolReply::Text(text) => text.as_str(),
            ToolReply::Blocks(blocks) => match &blocks[0] {
                ReplyBlock::Text(text) => text.as_str(),
                ReplyBlock::Image { .. } => "",
            },
            ToolReply::Error(e) => e.as_str(),
        };
        assert!(note.starts_with("Closed 1 session(s)") && note.contains(&old), "{reopened:?}");
        assert_eq!(tools.open_sessions.keys().collect::<Vec<_>>(), vec![&new]);

        let closed = tools.call("aem_verify_close", &serde_json::json!({ "session": new }), None).await;
        assert!(!matches!(closed, ToolReply::Error(_)), "{closed:?}");
        assert!(tools.open_sessions.is_empty());
        tools.shutdown().await.expect("teardown");
    }

    /// Two conversions check their forms on the one shared AEM at once: each
    /// opens its own form, and one walks its form with `aem_verify_run` while
    /// both sessions stay open and answering. The last to let go removes the
    /// containers.
    #[tokio::test]
    #[ignore = "needs Docker and the AEM image"]
    async fn two_conversions_verify_their_forms_on_one_aem() {
        let build = |name: &str| {
            let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../vendor/crates/u2s-aem-ubs-mcp/tests/fixtures/golden/{name}/document.json"));
            let text = std::fs::read_to_string(golden).unwrap();
            Artifact {
                file_name: "package.zip",
                bytes: u2s_aem_ubs_mcp::encode(
                    &u2s_aem_ubs_mcp::UbsAemDocument::from_json(&serde_json::from_str(&text).unwrap()).unwrap(),
                )
                .unwrap()
                .package,
            }
        };
        let settings = AemVerifySettings::default();
        let mut first = U2sTools::new().unwrap();
        let mut second = U2sTools::new().unwrap();
        first.attach_aem_verify(&settings).unwrap();
        second.attach_aem_verify(&settings).unwrap();

        let opened = first.call("aem_verify_open", &serde_json::json!({}), Some(build("AABF_019"))).await;
        let first_session = opened_session(&opened).unwrap_or_else(|| panic!("first open: {opened:?}"));
        let opened = second.call("aem_verify_open", &serde_json::json!({}), Some(build("AAEV_019_EN"))).await;
        let second_session = opened_session(&opened).unwrap_or_else(|| panic!("second open: {opened:?}"));

        let ran = second.call("aem_verify_run", &serde_json::json!({}), Some(build("AAEV_019_EN"))).await;
        assert!(!matches!(ran, ToolReply::Error(_)), "a run beside open sessions: {ran:?}");
        assert_eq!(second.open_sessions.len(), 1, "a run keeps the sessions on its own build open");

        for (tools, session) in [(&mut first, &first_session), (&mut second, &second_session)] {
            let controls = tools
                .call("aem_verify_controls", &serde_json::json!({ "session": session, "revision": 0 }), None)
                .await;
            assert!(!matches!(controls, ToolReply::Error(_)), "{session} still answers: {controls:?}");
        }

        let shared = Arc::clone(second.verifier.as_ref().unwrap().shared());
        first.shutdown().await.expect("the first lets go");
        assert!(shared.holds_lock(), "the second still uses the containers");
        second.shutdown().await.expect("teardown");
        assert!(!shared.holds_lock(), "the last one removed the containers");
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
            let Some(server_name) = name.strip_prefix("aem_") else {
                continue;
            };
            seen += 1;
            let call = VerifyCall::of(server_name).unwrap_or_else(|| panic!("{name} has no deadline class"));
            let offline = matches!(server_name, "verify_status" | "verify_package_check");
            assert_eq!(call.deadline(boot).is_none(), offline, "{name}");
        }
        assert!(seen >= 12, "expected every aem_verify_* tool, saw {seen}");
        let open = VerifyCall::of("verify_open").unwrap().deadline(boot).unwrap();
        assert!(open > boot, "a call that may boot the platform must outlast the boot");
    }
}
