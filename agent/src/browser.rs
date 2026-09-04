//! Browser verification: a Playwright MCP server run as a child process and
//! driven over stdio, so the Author and Reviewer can open the deployed form in a
//! real browser, click through it, submit it and read the PDF it produces.
//!
//! Hardening choices (see the README's "Browser verification"):
//! - The server version is pinned by [`PLAYWRIGHT_MCP_VERSION`] and `npx` runs
//!   with the npm cache preferred, so a run never reaches the registry once
//!   [`preflight`] has warmed the cache. `latest` never appears anywhere.
//! - The tool surface offered to the model is the checked-in snapshot
//!   (`tests/playwright_mcp_tools.json`), verified against the live server at
//!   start; any difference is a hard error, because with an exact pin it can
//!   only mean a corrupted cache or a wrong binary.
//! - Every phase has its own timeout. A dead or hung server is restarted at
//!   most [`MAX_RESTARTS`] times per run, and each restart is reported through
//!   [`BrowserSession::take_warnings`].
//! - The server is spawned as the leader of its own process group (a job object
//!   on Windows), so closing it also takes the `node` process `npx` forks and
//!   the Chrome that node launched. Killing `npx` alone would leave both alive.
//! - Nothing degrades silently: a failed preflight is an error the caller
//!   surfaces before the run spends a token.

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use blueprint::AemConnection;
use process_wrap::tokio::CommandWrap;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, RawContent, Tool};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::{IntoTransport, TokioChildProcess};

use crate::conversion::{ReplyBlock, ToolReply};

/// The npm package that provides the browser tools.
pub const PLAYWRIGHT_MCP_PACKAGE: &str = "@playwright/mcp";
/// The exact version this build is pinned to. Bumping it is a code change:
/// regenerate `tests/playwright_mcp_tools.json` with the ignored
/// `playwright_mcp_tool_surface_matches_snapshot` test, review the diff, and
/// re-read the prompts that name the tools.
pub const PLAYWRIGHT_MCP_VERSION: &str = "0.0.79";
/// Oldest Node.js major the pinned Playwright MCP runs on.
pub const MIN_NODE_MAJOR: u32 = 18;

/// The browser tools offered to the model, in the order they are offered. This
/// is the whole surface: the server exposes more (coordinate clicks, raw code
/// execution), and none of it is forwarded.
pub const BROWSER_TOOLS: &[&str] = &[
    "browser_navigate",
    "browser_navigate_back",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_fill_form",
    "browser_select_option",
    "browser_press_key",
    "browser_wait_for",
    "browser_take_screenshot",
    "browser_network_requests",
    "browser_console_messages",
    "browser_tabs",
    "browser_handle_dialog",
    "browser_evaluate",
    "browser_close",
];

/// How many times a dead or hung server is restarted within one run before the
/// browser tools give up for good.
pub const MAX_RESTARTS: u8 = 2;

/// Appended once to every preflight error: the operator can always opt out.
pub const DISABLE_HINT: &str = "or disable browser verification (--no-browser on the command \
                                line, Settings > AEM in the app)";

/// The first `npx` run downloads the package; later runs answer from the cache.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(300);
/// Process start plus MCP initialise, with a warm cache.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// One browser tool call. Playwright's own navigation timeout is
/// [`NAVIGATION_TIMEOUT_MS`], so this only fires when the server itself hangs.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(90);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const NODE_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const NAVIGATION_TIMEOUT_MS: u64 = 60_000;
const ACTION_TIMEOUT_MS: u64 = 10_000;
/// Tall enough that a wizard page's toolbar is usually inside the first
/// screenshot.
const VIEWPORT_SIZE: &str = "1280x1400";
const STDERR_TAIL_LINES: usize = 40;

/// The pinned server's tool surface, recorded once per version.
const TOOL_SURFACE_SNAPSHOT: &str = include_str!("../tests/playwright_mcp_tools.json");

// ── Configuration and reports ────────────────────────────────────────────────

/// What the operator decides about the browser: only where `npx` lives, when
/// auto-detection cannot find it (a Finder-launched app has a minimal `PATH`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowserConfig {
    pub npx: Option<PathBuf>,
}

/// What [`prepare`] established: the machine can run the pinned server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub npx: PathBuf,
    pub node_version: String,
    pub chrome: PathBuf,
    pub mcp_version: String,
}

impl fmt::Display for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "npx:            {}", self.npx.display())?;
        writeln!(f, "node:           {}", self.node_version)?;
        writeln!(f, "chrome:         {}", self.chrome.display())?;
        write!(
            f,
            "playwright mcp: {} {}",
            PLAYWRIGHT_MCP_PACKAGE, self.mcp_version
        )
    }
}

/// What [`preflight`] established: [`Prepared`], plus a server that logged in,
/// started and offered the pinned tool surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightReport {
    pub prepared: Prepared,
    pub tool_count: usize,
}

impl fmt::Display for PreflightReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.prepared)?;
        write!(f, "browser tools:  {}", self.tool_count)
    }
}

/// One file in the browser's output directory (downloads, screenshots).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFile {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

// ── Locating the prerequisites ───────────────────────────────────────────────

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Directories a Node install commonly lives in, beyond `PATH`. A desktop app
/// launched from Finder or a shortcut sees a `PATH` without any of these.
fn known_node_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "APPDATA", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(var) {
                dirs.push(PathBuf::from(&base).join("nodejs"));
                dirs.push(PathBuf::from(&base).join("npm"));
            }
        }
        return dirs;
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/opt/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));
    if let Some(home) = home() {
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".fnm/aliases/default/bin"));
        // Every nvm-installed Node, newest first.
        if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut found: Vec<PathBuf> = versions
                .filter_map(|e| e.ok())
                .map(|e| e.path().join("bin"))
                .collect();
            found.sort();
            found.reverse();
            dirs.extend(found);
        }
    }
    dirs
}

/// `PATH` first, then the known Node locations, without duplicates.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in known_node_dirs() {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

fn npx_file_name() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}

/// Where `npx` is: the configured path when given, otherwise the first hit in
/// `PATH` and the known Node locations. The error lists every path tried.
pub fn resolve_npx(cfg: &BrowserConfig) -> Result<PathBuf, String> {
    if let Some(configured) = &cfg.npx {
        return if configured.is_file() {
            Ok(configured.clone())
        } else {
            Err(format!(
                "npx was not found at the configured path {} (browser npx path setting / --npx)",
                configured.display()
            ))
        };
    }
    resolve_npx_in(&candidate_dirs())
}

fn resolve_npx_in(dirs: &[PathBuf]) -> Result<PathBuf, String> {
    let mut tried = Vec::new();
    for dir in dirs {
        let candidate = dir.join(npx_file_name());
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(format!(
        "npx (Node.js {MIN_NODE_MAJOR}+) was not found. Looked in: {}. Install Node.js, or set the \
         npx path in the settings (--npx on the command line)",
        tried.join(", ")
    ))
}

/// A command that runs `program` the way a Node install expects: `npx` is a
/// script that starts with `#!/usr/bin/env node`, so the directory `npx` lives
/// in is put first on `PATH` (it is not there for a Finder-launched app, which
/// is the case `resolve_npx`'s known locations exist for), and npm is told to
/// answer from its cache and keep quiet.
fn node_command(program: &Path, npx: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    let mut path: Vec<PathBuf> = npx.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(existing) = std::env::var_os("PATH") {
        let rest: Vec<PathBuf> = std::env::split_paths(&existing)
            .filter(|p| !path.contains(p))
            .collect();
        path.extend(rest);
    }
    if let Ok(joined) = std::env::join_paths(&path) {
        cmd.env("PATH", joined);
    }
    for (k, v) in npm_env() {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);
    cmd
}

/// The `node` next to `npx`, falling back to whatever `node` is on `PATH`.
fn node_next_to(npx: &Path) -> PathBuf {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    match npx.parent().map(|d| d.join(name)) {
        Some(sibling) if sibling.is_file() => sibling,
        _ => PathBuf::from(name),
    }
}

/// Parse `v22.14.0` into its major.
pub fn node_major(version: &str) -> Option<u32> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Run `node --version` and require [`MIN_NODE_MAJOR`] or newer.
pub async fn check_node(npx: &Path) -> Result<String, String> {
    let node = node_next_to(npx);
    let output = tokio::time::timeout(
        NODE_CHECK_TIMEOUT,
        node_command(&node, npx)
            .arg("--version")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| {
        format!(
            "`{} --version` did not answer within {}s",
            node.display(),
            NODE_CHECK_TIMEOUT.as_secs()
        )
    })?
    .map_err(|e| format!("could not run `{} --version`: {e}", node.display()))?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match node_major(&version) {
        Some(major) if major >= MIN_NODE_MAJOR => Ok(version),
        _ => Err(format!(
            "Node.js {MIN_NODE_MAJOR}+ is required for Playwright MCP; `{} --version` reported {:?}. \
             Update Node.js",
            node.display(),
            version
        )),
    }
}

/// Where Google Chrome is installed on this platform.
fn chrome_candidates() -> Vec<PathBuf> {
    let mut c = Vec::new();
    if cfg!(target_os = "macos") {
        c.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        if let Some(home) = home() {
            c.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
        }
    } else if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(var) {
                c.push(PathBuf::from(base).join(r"Google\Chrome\Application\chrome.exe"));
            }
        }
    } else {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ] {
            for dir in candidate_dirs() {
                c.push(dir.join(name));
            }
            c.push(PathBuf::from("/opt/google/chrome").join(name));
        }
    }
    c
}

/// The Chrome executable Playwright will launch. Playwright's own Chromium
/// download is deliberately not used, so `npx playwright install` is never
/// needed.
pub fn find_chrome() -> Result<PathBuf, String> {
    find_chrome_in(&chrome_candidates())
}

fn find_chrome_in(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Ok(found.clone());
    }
    let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    Err(format!(
        "Google Chrome was not found (looked at: {}). Install Google Chrome",
        tried.join(", ")
    ))
}

// ── Spawning the server ──────────────────────────────────────────────────────

/// `@playwright/mcp@0.0.79`: the one place the package and version are joined.
pub fn package_spec() -> String {
    format!("{PLAYWRIGHT_MCP_PACKAGE}@{PLAYWRIGHT_MCP_VERSION}")
}

/// Environment for every `npx` invocation: answer from the cache when the
/// pinned version is there, and keep npm's chatter off the MCP channel.
pub fn npm_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("npm_config_prefer_offline", "true"),
        ("npm_config_update_notifier", "false"),
        ("npm_config_fund", "false"),
        ("npm_config_audit", "false"),
        ("npm_config_loglevel", "error"),
        ("NO_UPDATE_NOTIFIER", "1"),
        ("NO_COLOR", "1"),
    ]
}

/// The arguments `npx` gets for the server itself.
pub fn server_args(
    chrome: &Path,
    storage_state: &Path,
    output_dir: &Path,
    origin: &str,
) -> Vec<String> {
    vec![
        "--yes".into(),
        package_spec(),
        "--headless".into(),
        "--isolated".into(),
        "--executable-path".into(),
        chrome.display().to_string(),
        "--storage-state".into(),
        storage_state.display().to_string(),
        "--output-dir".into(),
        output_dir.display().to_string(),
        "--image-responses".into(),
        "allow".into(),
        "--allowed-origins".into(),
        origin.to_string(),
        "--viewport-size".into(),
        VIEWPORT_SIZE.into(),
        "--timeout-navigation".into(),
        NAVIGATION_TIMEOUT_MS.to_string(),
        "--timeout-action".into(),
        ACTION_TIMEOUT_MS.to_string(),
    ]
}

/// `http://localhost:4502/` -> `localhost`.
pub fn host_of(url: &str) -> Result<String, String> {
    let origin = origin_of(url)?;
    let without_scheme = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&origin);
    let host = without_scheme
        .rsplit_once(':')
        .map_or(without_scheme, |(h, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                h
            } else {
                without_scheme
            }
        });
    Ok(host.trim_start_matches('[').trim_end_matches(']').to_string())
}

/// `http://localhost:4502/some/path` -> `http://localhost:4502`.
pub fn origin_of(url: &str) -> Result<String, String> {
    let url = url.trim();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("AEM host {url:?} is not a URL (expected http://host:port)"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err(format!("AEM host {url:?} has no host name"));
    }
    Ok(format!("{scheme}://{authority}"))
}

/// The Playwright storage state that logs the browser in: the AEM
/// `login-token` cookie, scoped to the AEM host.
pub fn storage_state_json(host_url: &str, login_token: &str) -> Result<String, String> {
    let state = serde_json::json!({
        "cookies": [{
            "name": "login-token",
            "value": login_token,
            "domain": host_of(host_url)?,
            "path": "/",
            "expires": -1,
            "httpOnly": true,
            "secure": false,
            "sameSite": "Lax",
        }],
        "origins": [],
    });
    serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
}

/// A fresh, private directory for one session's storage state and downloads.
fn new_output_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("blueprint-browser-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "could not create the browser output directory {}: {e}",
            dir.display()
        )
    })?;
    Ok(dir)
}

/// Write the storage state for `host_url` into `output_dir` and return its path.
fn write_storage_state(
    output_dir: &Path,
    host_url: &str,
    login_token: &str,
) -> Result<PathBuf, String> {
    let storage_state = output_dir.join("storage-state.json");
    std::fs::write(&storage_state, storage_state_json(host_url, login_token)?)
        .map_err(|e| format!("could not write {}: {e}", storage_state.display()))?;
    Ok(storage_state)
}

type StderrTail = Arc<Mutex<VecDeque<String>>>;

fn new_tail() -> StderrTail {
    Arc::new(Mutex::new(VecDeque::new()))
}

fn push_tail(tail: &StderrTail, line: String) {
    if let Ok(mut t) = tail.lock() {
        if t.len() >= STDERR_TAIL_LINES {
            t.pop_front();
        }
        t.push_back(line);
    }
}

/// The server's recent stderr, for error messages. Empty when it said nothing.
fn stderr_note(tail: &StderrTail) -> String {
    let lines: Vec<String> = tail
        .lock()
        .map(|t| t.iter().cloned().collect())
        .unwrap_or_default();
    if lines.is_empty() {
        String::new()
    } else {
        format!("\nServer output:\n{}", lines.join("\n"))
    }
}

/// Keep reading the child's stderr so the pipe never fills, remembering the
/// tail for diagnostics.
fn drain_stderr(stderr: tokio::process::ChildStderr, tail: StderrTail) {
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            push_tail(&tail, line);
        }
    });
}

/// Make sure the pinned package is in the npm cache and runnable, streaming
/// npm's progress to `progress`. Returns the version the package reported.
pub async fn warm_cache(
    npx: &Path,
    progress: &mut (impl FnMut(&str) + ?Sized),
) -> Result<String, String> {
    use tokio::io::AsyncBufReadExt;

    let mut child = node_command(npx, npx)
        .arg("--yes")
        .arg(package_spec())
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", npx.display()))?;

    let deadline = tokio::time::Instant::now() + WARM_UP_TIMEOUT;
    let mut stderr_lines = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        loop {
            match tokio::time::timeout_at(deadline, lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    progress(&line);
                    stderr_lines.push(line);
                }
                Ok(_) => break,
                Err(_) => {
                    let _ = child.kill().await;
                    return Err(format!(
                        "preparing {} took longer than {}s (network?). Run `blueprint browser \
                         prepare` once with a good connection",
                        package_spec(),
                        WARM_UP_TIMEOUT.as_secs()
                    ));
                }
            }
        }
    }
    let output = tokio::time::timeout_at(deadline, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "`npx {} --version` did not finish within {}s",
                package_spec(),
                WARM_UP_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("`npx {} --version` failed: {e}", package_spec()))?;
    if !output.status.success() {
        return Err(format!(
            "`npx {} --version` exited with {}\n{}",
            package_spec(),
            output.status,
            stderr_lines.join("\n")
        ));
    }
    // Commander prints `Version 0.0.79`; keep the bare version.
    let version = String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches("Version")
        .trim()
        .to_string();
    if version != PLAYWRIGHT_MCP_VERSION {
        return Err(format!(
            "`npx {} --version` reported {version:?}, not the pinned {PLAYWRIGHT_MCP_VERSION}. The \
             npm cache or the npx on PATH is not what this build expects",
            package_spec()
        ));
    }
    Ok(version)
}

/// Initialise an MCP client over `transport`, bounded by [`START_TIMEOUT`].
async fn serve_client<T, E, A>(
    transport: T,
    tail: &StderrTail,
) -> Result<RunningService<RoleClient, ()>, String>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(START_TIMEOUT, ().serve(transport)).await {
        Ok(Ok(service)) => Ok(service),
        Ok(Err(e)) => Err(format!(
            "Playwright MCP did not initialise: {e}{}",
            stderr_note(tail)
        )),
        Err(_) => Err(format!(
            "Playwright MCP did not answer within {}s{}",
            START_TIMEOUT.as_secs(),
            stderr_note(tail)
        )),
    }
}

/// Spawn `cmd` as an MCP server and initialise a client on it.
///
/// The child becomes the leader of its own process group (a job object on
/// Windows). `npx` forks the real server (`node .../playwright-mcp`), which
/// launches Chrome; killing only `npx` would orphan both, so the whole group
/// goes when the child does.
async fn spawn_mcp(
    cmd: tokio::process::Command,
    tail: &StderrTail,
) -> Result<RunningService<RoleClient, ()>, String> {
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    let mut wrapped = CommandWrap::from(cmd);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);
    let (process, stderr) = TokioChildProcess::builder(wrapped)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start `{program} {}`: {e}", package_spec()))?;
    if let Some(stderr) = stderr {
        drain_stderr(stderr, tail.clone());
    }
    serve_client(process, tail).await
}

/// Close a client and its server without caring how; a session that is being
/// torn down has nothing left to learn from the outcome.
async fn cancel_quietly(service: RunningService<RoleClient, ()>) {
    let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, service.cancel()).await;
}

/// Write the storage state for `token` and spawn the server logged in with it.
async fn spawn_server(
    npx: &Path,
    chrome: &Path,
    host_url: &str,
    token: &str,
    output_dir: &Path,
    tail: &StderrTail,
) -> Result<RunningService<RoleClient, ()>, String> {
    let storage_state = write_storage_state(output_dir, host_url, token)?;
    let origin = origin_of(host_url)?;
    let mut cmd = node_command(npx, npx);
    cmd.args(server_args(chrome, &storage_state, output_dir, &origin));
    cmd.current_dir(output_dir);
    spawn_mcp(cmd, tail).await
}

// ── The tool surface ─────────────────────────────────────────────────────────

/// The checked-in tool surface of the pinned server: `{name, description,
/// input_schema}` per [`BROWSER_TOOLS`] entry, in that order.
pub fn tool_surface_snapshot() -> &'static [serde_json::Value] {
    static SNAPSHOT: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        serde_json::from_str(TOOL_SURFACE_SNAPSHOT)
            .expect("tests/playwright_mcp_tools.json is valid JSON (it is compiled in)")
    })
}

/// One live tool in the snapshot's shape.
pub fn tool_to_spec(tool: &Tool) -> serde_json::Value {
    serde_json::json!({
        "name": tool.name.as_ref(),
        "description": tool.description.as_deref().unwrap_or_default(),
        "input_schema": serde_json::Value::Object((*tool.input_schema).clone()),
    })
}

/// Check the live server against the snapshot and return the specs to offer.
///
/// Every snapshot tool must exist with the same description and input schema,
/// and none may collide with an engine tool. Anything else is a hard error:
/// with an exact version pin the surface cannot legitimately differ.
pub fn verify_tool_surface(live: &[Tool]) -> Result<Vec<serde_json::Value>, String> {
    let snapshot = tool_surface_snapshot();
    if snapshot.is_empty() {
        return Err(
            "the Playwright MCP tool-surface snapshot (agent/tests/playwright_mcp_tools.json) is \
             empty; regenerate it with `UPDATE_SNAPSHOTS=1 cargo test -p agent -- --ignored \
             playwright_mcp_tool_surface_matches_snapshot`"
                .into(),
        );
    }
    let mut missing = Vec::new();
    let mut drifted = Vec::new();
    let mut colliding = Vec::new();
    for expected in snapshot {
        let name = expected["name"].as_str().unwrap_or_default();
        if crate::conversion::catalog().iter().any(|t| t.name() == name) {
            colliding.push(name.to_string());
        }
        match live.iter().find(|t| t.name.as_ref() == name) {
            None => missing.push(name.to_string()),
            Some(tool) if tool_to_spec(tool) != *expected => drifted.push(name.to_string()),
            Some(_) => {}
        }
    }
    if !colliding.is_empty() {
        return Err(format!(
            "browser tools collide with engine tools: {colliding:?}"
        ));
    }
    if !missing.is_empty() || !drifted.is_empty() {
        return Err(format!(
            "the Playwright MCP tool surface differs from the pinned {} snapshot (missing: {:?}, \
             changed: {:?}). The npm cache or the npx on PATH is not what this build expects",
            package_spec(),
            missing,
            drifted
        ));
    }
    Ok(snapshot.to_vec())
}

/// Map an MCP tool result onto the engine's reply type, keeping text and images
/// in order. A result flagged `isError` becomes an error reply.
pub fn reply_from_result(result: CallToolResult) -> ToolReply {
    let mut blocks: Vec<ReplyBlock> = Vec::new();
    for content in result.content {
        blocks.push(match content.raw {
            RawContent::Text(t) => ReplyBlock::Text(t.text),
            RawContent::Image(i) => ReplyBlock::Image {
                media_type: i.mime_type,
                data: i.data,
            },
            RawContent::Audio(_) => ReplyBlock::Text("[audio content omitted]".into()),
            RawContent::Resource(_) => ReplyBlock::Text("[embedded resource omitted]".into()),
            RawContent::ResourceLink(r) => {
                ReplyBlock::Text(format!("[resource link: {}]", r.uri))
            }
        });
    }
    let text_only = blocks.iter().all(|b| matches!(b, ReplyBlock::Text(_)));
    let joined = || {
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
        let msg = joined();
        return ToolReply::Error(if msg.is_empty() {
            "the browser tool reported an error".into()
        } else {
            msg
        });
    }
    if text_only {
        ToolReply::Text(joined())
    } else {
        ToolReply::Blocks(blocks)
    }
}

// ── The session ──────────────────────────────────────────────────────────────

/// A way to bring a server back: what a restart calls.
pub type Reconnector = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<RunningService<RoleClient, ()>, String>> + Send>>
        + Send
        + Sync,
>;

/// How the session's server was started, which decides how it is restarted.
#[derive(Clone)]
enum Launch {
    /// The real thing: `npx` with the pinned package, logged in to AEM afresh
    /// on every start.
    Npx {
        npx: PathBuf,
        chrome: PathBuf,
        conn: AemConnection,
    },
    /// A caller-supplied way to connect again (tests).
    Reconnect(Reconnector),
    /// A transport handed in once. Not restartable.
    Fixed,
}

/// One browser, alive for one run: the MCP connection, the tool specs the
/// model is offered, and the directory downloads land in.
pub struct BrowserSession {
    launch: Launch,
    output_dir: PathBuf,
    /// `None` between a failure and the next (re)start.
    service: Option<RunningService<RoleClient, ()>>,
    stderr_tail: StderrTail,
    tools: Vec<serde_json::Value>,
    restarts: u8,
    call_timeout: Duration,
    /// Notes for the operator (restarts), drained by [`take_warnings`](Self::take_warnings).
    warnings: Vec<String>,
}

impl fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserSession")
            .field("output_dir", &self.output_dir)
            .field("connected", &self.service.is_some())
            .field("tools", &self.tools.len())
            .field("restarts", &self.restarts)
            .finish()
    }
}

impl BrowserSession {
    fn assemble(
        launch: Launch,
        output_dir: PathBuf,
        service: Option<RunningService<RoleClient, ()>>,
        stderr_tail: StderrTail,
        tools: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            launch,
            output_dir,
            service,
            stderr_tail,
            tools,
            restarts: 0,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            warnings: Vec::new(),
        }
    }

    /// Start the pinned server through `npx` in a fresh output directory,
    /// logged in to `conn` with `token`. The directory is removed again when
    /// the start fails, so a refused preflight leaves nothing (in particular no
    /// login token) in the temp dir.
    async fn start_npx(
        npx: PathBuf,
        chrome: PathBuf,
        conn: AemConnection,
        token: &str,
    ) -> Result<Self, String> {
        let output_dir = new_output_dir()?;
        let tail = new_tail();
        let started = async {
            let service = spawn_server(&npx, &chrome, &conn.host, token, &output_dir, &tail).await?;
            let tools = Self::discover(&service).await?;
            Ok::<_, String>((service, tools))
        }
        .await;
        match started {
            Ok((service, tools)) => Ok(Self::assemble(
                Launch::Npx { npx, chrome, conn },
                output_dir,
                Some(service),
                tail,
                tools,
            )),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&output_dir);
                Err(e)
            }
        }
    }

    /// Drive a server over an existing transport, e.g. an in-process fake in a
    /// test. Not restartable, no login, no `npx`.
    pub async fn connect<T, E, A>(transport: T, output_dir: PathBuf) -> Result<Self, String>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let tail = new_tail();
        let service = serve_client(transport, &tail).await?;
        let tools = Self::discover(&service).await?;
        Ok(Self::assemble(Launch::Fixed, output_dir, Some(service), tail, tools))
    }

    /// Drive a server that `reconnect` connects to, now and after every
    /// failure within the restart budget (tests of the restart path).
    pub async fn connect_with(reconnect: Reconnector, output_dir: PathBuf) -> Result<Self, String> {
        let tail = new_tail();
        let service = (reconnect)().await?;
        let tools = Self::discover(&service).await?;
        Ok(Self::assemble(
            Launch::Reconnect(reconnect),
            output_dir,
            Some(service),
            tail,
            tools,
        ))
    }

    /// A session with no server behind it: it offers `tools` and fails every
    /// call. For tests of the callers that only need the tool list.
    #[doc(hidden)]
    pub fn detached(tools: Vec<serde_json::Value>, output_dir: PathBuf) -> Self {
        let mut session = Self::assemble(Launch::Fixed, output_dir, None, new_tail(), tools);
        session.restarts = MAX_RESTARTS;
        session
    }

    /// Override how long one tool call may take before the session counts as
    /// hung. For tests; a run keeps the default.
    #[doc(hidden)]
    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    async fn discover(
        service: &RunningService<RoleClient, ()>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let live = tokio::time::timeout(START_TIMEOUT, service.list_all_tools())
            .await
            .map_err(|_| {
                format!(
                    "Playwright MCP did not list its tools within {}s",
                    START_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| format!("Playwright MCP could not list its tools: {e}"))?;
        verify_tool_surface(&live)
    }

    /// The tool specs to offer the model, in the catalog's shape.
    pub fn tools(&self) -> &[serde_json::Value] {
        &self.tools
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t["name"].as_str() == Some(name))
    }

    /// Where the browser writes downloads and screenshots.
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    /// Notes the session accumulated (restarts), for the run's observer.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Whether the server is currently connected.
    pub fn is_connected(&self) -> bool {
        self.service.is_some()
    }

    async fn mark_broken(&mut self, why: String) {
        if let Some(service) = self.service.take() {
            cancel_quietly(service).await;
        }
        self.warnings.push(why);
    }

    /// Bring a broken session back, within the restart budget.
    async fn restart(&mut self) -> Result<(), String> {
        if self.restarts >= MAX_RESTARTS {
            return Err(format!(
                "the browser session failed {MAX_RESTARTS} times in this run and is no longer \
                 available; verify with fetch_aem_dor_pdf instead"
            ));
        }
        let service = match self.launch.clone() {
            Launch::Fixed => {
                return Err("the browser session ended and this session cannot be restarted; \
                            verify with fetch_aem_dor_pdf instead"
                    .into());
            }
            Launch::Reconnect(reconnect) => (reconnect)().await?,
            Launch::Npx { npx, chrome, conn } => {
                let token = crate::aem_client::aem_login(&conn).await?;
                spawn_server(
                    &npx,
                    &chrome,
                    &conn.host,
                    &token,
                    &self.output_dir,
                    &self.stderr_tail,
                )
                .await?
            }
        };
        self.restarts += 1;
        self.tools = Self::discover(&service).await?;
        self.service = Some(service);
        self.warnings.push(format!(
            "Browser session restarted ({} of {MAX_RESTARTS} restarts used).",
            self.restarts
        ));
        Ok(())
    }

    /// Call one browser tool. A transport failure or a hang marks the session
    /// broken; the next call restarts it within the budget.
    pub async fn call(&mut self, name: &str, input: &serde_json::Value) -> ToolReply {
        if !self.has_tool(name) {
            return ToolReply::Error(format!("Unknown browser tool: {name}"));
        }
        if self.service.is_none()
            && let Err(e) = self.restart().await
        {
            return ToolReply::Error(format!("browser unavailable: {e}"));
        }
        let service = self.service.as_ref().expect("restart succeeded");
        let arguments = input.as_object().cloned().unwrap_or_default();
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(arguments);
        match tokio::time::timeout(self.call_timeout, service.call_tool(params)).await {
            Ok(Ok(result)) => reply_from_result(result),
            // The server answered with a protocol-level error (bad arguments, a
            // Playwright failure it chose to report that way): the session is fine.
            Ok(Err(ServiceError::McpError(e))) => ToolReply::Error(e.message.to_string()),
            Ok(Err(e)) => {
                let why = format!("{name} failed: {e}{}", stderr_note(&self.stderr_tail));
                self.mark_broken(format!("Browser session lost ({why})"))
                    .await;
                ToolReply::Error(format!(
                    "{why}. The browser will be restarted on the next call."
                ))
            }
            Err(_) => {
                let why = format!(
                    "{name} did not answer within {}s",
                    self.call_timeout.as_secs_f32()
                );
                self.mark_broken(format!("Browser session hung ({why})"))
                    .await;
                ToolReply::Error(format!(
                    "{why}. The browser will be restarted on the next call."
                ))
            }
        }
    }

    /// Close the server (and, with it, its process group) and delete the
    /// output directory.
    pub async fn shutdown(mut self) {
        if let Some(service) = self.service.take() {
            cancel_quietly(service).await;
        }
        let _ = std::fs::remove_dir_all(&self.output_dir);
    }
}

// ── Preflight ────────────────────────────────────────────────────────────────

/// Locate the machine-side prerequisites: `npx`, a recent Node, Google Chrome.
async fn locate_tools(cfg: &BrowserConfig) -> Result<(PathBuf, String, PathBuf), String> {
    let npx = resolve_npx(cfg)?;
    let node_version = check_node(&npx).await?;
    let chrome = find_chrome()?;
    Ok((npx, node_version, chrome))
}

/// The machine-side half of the preflight, which needs no AEM: `npx` and a
/// recent Node, Google Chrome, and the pinned package present in the npm cache
/// and answering `--version`. This is what `blueprint browser prepare` runs to
/// warm the cache once, with a good connection.
pub async fn prepare(
    cfg: &BrowserConfig,
    progress: &mut dyn FnMut(&str),
) -> Result<Prepared, String> {
    let (npx, node_version, chrome) = locate_tools(cfg).await?;
    progress(&format!(
        "Preparing Playwright MCP {PLAYWRIGHT_MCP_VERSION} (the first run downloads it)..."
    ));
    let mcp_version = warm_cache(&npx, progress).await?;
    Ok(Prepared {
        npx,
        node_version,
        chrome,
        mcp_version,
    })
}

/// Everything that has to be true before a run may count on the browser:
/// the machine-side prerequisites, the AEM login (checked before the possibly
/// slow cache warm-up, so a wrong password fails in a second), the pinned
/// package in the cache, and a server that starts, offers the pinned tool
/// surface and opens the AEM instance. Returns the report and the started
/// session, which the run then uses.
///
/// Fails loudly, every error ending with [`DISABLE_HINT`]. The caller surfaces
/// it before the run starts; nothing degrades on its own.
///
/// `progress` is `Send` because a trait object erases auto traits: without the
/// bound the whole preflight future is non-`Send`, and a run could not be driven
/// on a worker thread — which is what lets conversions make progress at the same
/// time.
pub async fn preflight(
    cfg: &BrowserConfig,
    conn: &AemConnection,
    progress: &mut (dyn FnMut(&str) + Send),
) -> Result<(PreflightReport, BrowserSession), String> {
    preflight_steps(cfg, conn, progress).await.map_err(|e| {
        let e = e.trim_end_matches('.');
        format!("{e}. Fix that, {DISABLE_HINT}.")
    })
}

async fn preflight_steps(
    cfg: &BrowserConfig,
    conn: &AemConnection,
    progress: &mut (dyn FnMut(&str) + Send),
) -> Result<(PreflightReport, BrowserSession), String> {
    let (npx, node_version, chrome) = locate_tools(cfg).await?;
    progress(&format!("Logging in to {}...", conn.host));
    let token = crate::aem_client::aem_login(conn).await?;
    progress(&format!(
        "Preparing Playwright MCP {PLAYWRIGHT_MCP_VERSION} (the first run downloads it)..."
    ));
    let mcp_version = warm_cache(&npx, progress).await?;
    progress("Starting the browser...");
    let mut session =
        BrowserSession::start_npx(npx.clone(), chrome.clone(), conn.clone(), &token).await?;
    let origin = origin_of(&conn.host)?;
    if let ToolReply::Error(e) = session
        .call(
            "browser_navigate",
            &serde_json::json!({ "url": format!("{origin}/") }),
        )
        .await
    {
        session.shutdown().await;
        return Err(format!(
            "the browser smoke test (opening {origin}/) failed: {e}"
        ));
    }
    let report = PreflightReport {
        prepared: Prepared {
            npx,
            node_version,
            chrome,
            mcp_version,
        },
        tool_count: session.tools().len(),
    };
    Ok((report, session))
}

/// Start the pinned server without AEM (no login, no storage state), for the
/// tests that talk to the real thing.
async fn spawn_bare_server(npx: &Path, tail: &StderrTail) -> Result<RunningService<RoleClient, ()>, String> {
    let chrome = find_chrome()?;
    let mut cmd = node_command(npx, npx);
    cmd.args([
        "--yes",
        &package_spec(),
        "--headless",
        "--isolated",
        "--executable-path",
    ])
    .arg(&chrome);
    spawn_mcp(cmd, tail).await
}

/// The pinned server's tool surface as it is right now, for the snapshot test.
pub async fn discover_tool_surface(npx: &Path) -> Result<Vec<serde_json::Value>, String> {
    let tail = new_tail();
    let service = spawn_bare_server(npx, &tail).await?;
    let live = service.list_all_tools().await.map_err(|e| e.to_string())?;
    cancel_quietly(service).await;
    let mut specs = Vec::new();
    let mut missing = Vec::new();
    for name in BROWSER_TOOLS {
        match live.iter().find(|t| t.name.as_ref() == *name) {
            Some(tool) => specs.push(tool_to_spec(tool)),
            None => missing.push(*name),
        }
    }
    if !missing.is_empty() {
        return Err(format!("the server does not offer {missing:?}"));
    }
    Ok(specs)
}

// ── Output files ─────────────────────────────────────────────────────────────

/// The files in the browser's output directory, newest first.
pub fn list_output_files(dir: &Path) -> Result<Vec<OutputFile>, String> {
    let mut files: Vec<(std::time::SystemTime, OutputFile)> = Vec::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        files.push((
            modified,
            OutputFile {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: meta.len(),
                modified: chrono::DateTime::<chrono::Local>::from(modified)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            },
        ));
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(files.into_iter().map(|(_, f)| f).collect())
}

/// Resolve `path` (a file name, or a path) to a file inside `dir`, refusing
/// anything that escapes it.
pub fn resolve_inside(dir: &Path, path: &str) -> Result<PathBuf, String> {
    let candidate = {
        let p = Path::new(path.trim());
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        }
    };
    let root = dir.canonicalize().map_err(|e| {
        format!(
            "browser output directory {} is unavailable: {e}",
            dir.display()
        )
    })?;
    let resolved = candidate
        .canonicalize()
        .map_err(|_| format!("no such file in the browser output directory: {path:?}"))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "{path:?} is outside the browser output directory; only files the browser produced can be inspected"
        ));
    }
    if !resolved.is_file() {
        return Err(format!("{path:?} is not a file"));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Content;

    fn scratch(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_spawn_is_pinned_and_never_latest() {
        let args = server_args(
            Path::new("/Applications/Chrome"),
            Path::new("/tmp/s.json"),
            Path::new("/tmp/out"),
            "http://localhost:4502",
        );
        assert_eq!(args[0], "--yes", "npx must never wait on its install prompt");
        assert_eq!(args[1], format!("@playwright/mcp@{PLAYWRIGHT_MCP_VERSION}"));
        assert!(args.iter().all(|a| !a.contains("latest")), "{args:?}");
        for flag in [
            "--headless",
            "--isolated",
            "--storage-state",
            "--output-dir",
            "--allowed-origins",
            "--executable-path",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag} in {args:?}");
        }
        let origin_at = args.iter().position(|a| a == "--allowed-origins").unwrap();
        assert_eq!(args[origin_at + 1], "http://localhost:4502");
        assert!(npm_env().contains(&("npm_config_prefer_offline", "true")));
    }

    /// `npx` is `#!/usr/bin/env node`: run from a known location outside `PATH`
    /// it only works when its own directory is on the `PATH` it is given.
    #[test]
    fn node_commands_put_the_npx_directory_first_on_path() {
        let npx = Path::new("/somewhere/odd/bin/npx");
        let cmd = node_command(npx, npx);
        let std_cmd = cmd.as_std();
        let path = std_cmd
            .get_envs()
            .find(|(k, _)| *k == "PATH")
            .and_then(|(_, v)| v)
            .expect("PATH is set")
            .to_string_lossy()
            .into_owned();
        assert!(
            path.starts_with("/somewhere/odd/bin"),
            "npx's own directory must come first: {path}"
        );
        assert!(
            std_cmd
                .get_envs()
                .any(|(k, v)| k == "npm_config_prefer_offline" && v.is_some_and(|v| v == "true"))
        );
    }

    #[test]
    fn hosts_and_origins_are_taken_from_the_aem_url() {
        assert_eq!(
            origin_of("http://localhost:4502/").unwrap(),
            "http://localhost:4502"
        );
        assert_eq!(
            origin_of("https://aem.example.com/content/x").unwrap(),
            "https://aem.example.com"
        );
        assert_eq!(host_of("http://localhost:4502").unwrap(), "localhost");
        assert_eq!(host_of("https://aem.example.com/").unwrap(), "aem.example.com");
        assert!(origin_of("localhost:4502").is_err());
        assert!(origin_of("http://").is_err());
    }

    #[test]
    fn the_storage_state_carries_the_login_token_for_the_aem_host() {
        let json = storage_state_json("http://localhost:4502", "tok.en").unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let cookie = &v["cookies"][0];
        assert_eq!(cookie["name"], "login-token");
        assert_eq!(cookie["value"], "tok.en");
        assert_eq!(cookie["domain"], "localhost");
        assert_eq!(cookie["path"], "/");
        assert_eq!(cookie["expires"], -1);
        assert_eq!(v["origins"], serde_json::json!([]));
    }

    #[test]
    fn node_majors_are_parsed_from_the_version_string() {
        assert_eq!(node_major("v22.14.0"), Some(22));
        assert_eq!(node_major("18.0.1\n"), Some(18));
        assert_eq!(node_major("garbage"), None);
    }

    #[test]
    fn npx_and_chrome_resolution_report_every_path_tried() {
        let tmp = scratch("blueprint-npx");
        let err = resolve_npx_in(std::slice::from_ref(&tmp)).unwrap_err();
        assert!(
            err.contains(&tmp.join(npx_file_name()).display().to_string()),
            "{err}"
        );
        std::fs::write(tmp.join(npx_file_name()), "").unwrap();
        assert_eq!(
            resolve_npx_in(std::slice::from_ref(&tmp)).unwrap(),
            tmp.join(npx_file_name())
        );
        let configured = BrowserConfig {
            npx: Some(tmp.join("nope")),
        };
        assert!(resolve_npx(&configured).unwrap_err().contains("nope"));

        let missing = tmp.join("Chrome");
        let err = find_chrome_in(std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.contains(&missing.display().to_string()), "{err}");
        std::fs::write(&missing, "").unwrap();
        assert_eq!(find_chrome_in(std::slice::from_ref(&missing)).unwrap(), missing);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn inspecting_is_confined_to_the_output_directory() {
        let dir = scratch("blueprint-out");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.pdf"), b"%PDF-1.4").unwrap();
        let outside = std::env::temp_dir().join(format!(
            "blueprint-outside-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&outside, b"x").unwrap();

        assert_eq!(
            resolve_inside(&dir, "a.pdf").unwrap(),
            dir.canonicalize().unwrap().join("a.pdf")
        );
        assert!(resolve_inside(&dir, "../").is_err());
        assert!(
            resolve_inside(
                &dir,
                &format!("../{}", outside.file_name().unwrap().to_string_lossy())
            )
            .is_err()
        );
        assert!(
            resolve_inside(&dir, &outside.display().to_string())
                .unwrap_err()
                .contains("outside")
        );
        assert!(resolve_inside(&dir, "sub").unwrap_err().contains("not a file"));
        assert!(
            resolve_inside(&dir, "missing.pdf")
                .unwrap_err()
                .contains("no such file")
        );

        let listed = list_output_files(&dir).unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].name, "a.pdf");
        assert_eq!(listed[0].size, 8);

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn results_keep_text_and_images_in_order_and_errors_are_errors() {
        let text_only = CallToolResult::success(vec![Content::text("a"), Content::text("b")]);
        assert!(matches!(reply_from_result(text_only), ToolReply::Text(t) if t == "a\nb"));

        let mixed = CallToolResult::success(vec![
            Content::text("snapshot"),
            Content::image("AAAA", "image/png"),
        ]);
        match reply_from_result(mixed) {
            ToolReply::Blocks(blocks) => assert_eq!(
                blocks,
                vec![
                    ReplyBlock::Text("snapshot".into()),
                    ReplyBlock::Image {
                        media_type: "image/png".into(),
                        data: "AAAA".into()
                    },
                ]
            ),
            other => panic!("expected blocks, got {other:?}"),
        }

        let failed = CallToolResult::error(vec![Content::text("Timeout 60000ms exceeded")]);
        assert!(
            matches!(reply_from_result(failed), ToolReply::Error(e) if e.contains("Timeout"))
        );
    }

    /// The snapshot is the surface the model sees and the prompts name, so it
    /// has to be exactly [`BROWSER_TOOLS`], in order, with a schema each.
    #[test]
    fn the_snapshot_is_exactly_the_browser_tool_list() {
        let names: Vec<&str> = tool_surface_snapshot()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, BROWSER_TOOLS.to_vec());
        for tool in tool_surface_snapshot() {
            assert_eq!(tool["input_schema"]["type"], "object", "{tool}");
            assert!(
                tool["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{tool}"
            );
        }
    }

    /// A detached session offers its tools and refuses to run them: what the
    /// pipeline tests attach to check the stage tool sets.
    #[tokio::test]
    async fn a_detached_session_lists_tools_but_cannot_call_them() {
        let mut session =
            BrowserSession::detached(tool_surface_snapshot().to_vec(), scratch("blueprint-det"));
        assert!(session.has_tool("browser_navigate"));
        assert!(!session.is_connected());
        let reply = session
            .call("browser_navigate", &serde_json::json!({"url": "about:blank"}))
            .await;
        assert!(
            matches!(&reply, ToolReply::Error(e) if e.contains("no longer available")),
            "{reply:?}"
        );
        assert!(matches!(
            session.call("browser_run_code_unsafe", &serde_json::json!({})).await,
            ToolReply::Error(e) if e.contains("Unknown browser tool")
        ));
        session.shutdown().await;
    }

    /// Processes whose command line mentions the server, for the leak checks.
    fn playwright_mcp_processes() -> usize {
        std::process::Command::new("pgrep")
            .args(["-f", "playwright-mcp"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
            .unwrap_or(0)
    }

    /// The pinned server must accept every flag [`server_args`] passes and then
    /// offer the snapshot surface; the flags are only validated by the server
    /// itself. And closing the session must take the forked `node` server with
    /// it, not just `npx`. Needs Node and Chrome (no AEM: a dummy login token
    /// is written).
    #[tokio::test]
    #[ignore = "needs Node.js, Google Chrome and the npm cache"]
    async fn the_pinned_server_accepts_the_full_argument_list_and_dies_with_the_session() {
        let npx = resolve_npx(&BrowserConfig::default()).expect("npx");
        let chrome = find_chrome().expect("chrome");
        let before = playwright_mcp_processes();
        let dir = new_output_dir().unwrap();
        let tail = new_tail();
        let service = spawn_server(&npx, &chrome, "http://localhost:4502", "dummy", &dir, &tail)
            .await
            .expect("the server starts with our arguments");
        let live = service.list_all_tools().await.expect("tools");
        verify_tool_surface(&live).expect("the pinned surface");
        // A page that needs no network: proves the browser itself launches.
        let result = service
            .call_tool(
                CallToolRequestParams::new("browser_navigate").with_arguments(
                    serde_json::json!({"url": "about:blank"})
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .expect("navigate");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert!(playwright_mcp_processes() > before, "the server runs");

        cancel_quietly(service).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            playwright_mcp_processes() <= before,
            "closing the session must not leave a playwright-mcp process behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Records the pinned server's real tool surface. Needs Node and Chrome, so
    /// it is ignored by default: run it after bumping [`PLAYWRIGHT_MCP_VERSION`]
    /// with `UPDATE_SNAPSHOTS=1 cargo test -p agent -- --ignored
    /// playwright_mcp_tool_surface_matches_snapshot`, then review the diff.
    #[tokio::test]
    #[ignore = "needs Node.js, Google Chrome and the npm cache"]
    async fn playwright_mcp_tool_surface_matches_snapshot() {
        let npx = resolve_npx(&BrowserConfig::default()).expect("npx");
        let live = discover_tool_surface(&npx).await.expect("tool surface");
        let actual = format!("{}\n", serde_json::to_string_pretty(&live).unwrap());
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/playwright_mcp_tools.json"
        );
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(path, &actual).expect("write snapshot");
            return;
        }
        let expected = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            actual, expected,
            "regenerate with UPDATE_SNAPSHOTS=1 and review the diff"
        );
    }
}

/// Tests against an in-process fake MCP server standing in for Playwright: no
/// Node, no Chrome, no network.
#[cfg(test)]
mod fake_server_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rmcp::handler::server::ServerHandler;
    use rmcp::model::*;
    use rmcp::service::{RequestContext, RoleServer};
    use rmcp::{ErrorData, ServiceExt};

    use super::*;

    /// Serves whatever tools it is given; `browser_take_screenshot` answers with
    /// text plus an image, `browser_close` with an error result,
    /// `browser_wait_for` sleeps for the `time` it is given (seconds), and
    /// everything else echoes its arguments.
    #[derive(Clone)]
    struct FakeBrowser {
        tools: Vec<Tool>,
    }

    fn tools_from_specs(specs: &[serde_json::Value]) -> Vec<Tool> {
        specs
            .iter()
            .map(|s| {
                Tool::new(
                    s["name"].as_str().unwrap().to_string(),
                    s["description"].as_str().unwrap().to_string(),
                    Arc::new(s["input_schema"].as_object().cloned().unwrap()),
                )
            })
            .collect()
    }

    impl ServerHandler for FakeBrowser {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult::with_all_items(self.tools.clone()))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResult, ErrorData> {
            let args = request.arguments.unwrap_or_default();
            Ok(match request.name.as_ref() {
                "browser_take_screenshot" => CallToolResult::success(vec![
                    Content::text("### Page state\nurl: about:blank"),
                    Content::image("iVBORw0KGgo=", "image/png"),
                ]),
                "browser_close" => CallToolResult::error(vec![Content::text("no page to close")]),
                "browser_wait_for" => {
                    let secs = args.get("time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
                    CallToolResult::success(vec![Content::text("waited")])
                }
                name => CallToolResult::success(vec![Content::text(format!(
                    "{name} {}",
                    serde_json::Value::Object(args)
                ))]),
            })
        }
    }

    /// Start a fake server; the handle aborts it (simulating a crash).
    fn serve_fake(tools: Vec<Tool>) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
        let (client_io, server_io) = tokio::io::duplex(1 << 16);
        let handle = tokio::spawn(async move {
            if let Ok(running) = (FakeBrowser { tools }).serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        (client_io, handle)
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blueprint-fake-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn connect(tools: Vec<Tool>) -> Result<BrowserSession, String> {
        let (client_io, _handle) = serve_fake(tools);
        BrowserSession::connect(client_io, scratch()).await
    }

    #[tokio::test]
    async fn the_snapshot_surface_is_discovered_and_offered() {
        let session = connect(tools_from_specs(tool_surface_snapshot()))
            .await
            .unwrap();
        assert_eq!(session.tools(), tool_surface_snapshot());
        assert!(session.is_connected());
        session.shutdown().await;
    }

    #[tokio::test]
    async fn extra_server_tools_are_not_forwarded() {
        let mut specs = tool_surface_snapshot().to_vec();
        specs.push(serde_json::json!({
            "name": "browser_run_code_unsafe",
            "description": "run arbitrary code",
            "input_schema": {"type": "object", "properties": {}},
        }));
        let session = connect(tools_from_specs(&specs)).await.unwrap();
        assert!(!session.has_tool("browser_run_code_unsafe"));
        assert_eq!(session.tools().len(), BROWSER_TOOLS.len());
        session.shutdown().await;
    }

    #[tokio::test]
    async fn a_missing_tool_is_a_hard_error() {
        let mut tools = tools_from_specs(tool_surface_snapshot());
        tools.retain(|t| t.name != "browser_click");
        let err = connect(tools).await.unwrap_err();
        assert!(err.contains("browser_click"), "{err}");
        assert!(err.contains(PLAYWRIGHT_MCP_VERSION), "{err}");
    }

    #[tokio::test]
    async fn a_changed_schema_or_description_is_a_hard_error() {
        let mut specs = tool_surface_snapshot().to_vec();
        specs[0]["description"] = serde_json::json!("something else");
        let err = connect(tools_from_specs(&specs)).await.unwrap_err();
        assert!(
            err.contains("changed") && err.contains(BROWSER_TOOLS[0]),
            "{err}"
        );

        let mut specs = tool_surface_snapshot().to_vec();
        specs[1]["input_schema"]["properties"]["extra"] = serde_json::json!({"type": "string"});
        let err = connect(tools_from_specs(&specs)).await.unwrap_err();
        assert!(err.contains(BROWSER_TOOLS[1]), "{err}");
    }

    #[tokio::test]
    async fn an_engine_tool_name_can_never_enter_the_browser_surface() {
        for name in BROWSER_TOOLS {
            assert!(
                !crate::conversion::catalog().iter().any(|t| t.name() == *name),
                "{name}"
            );
        }
        // Renaming a snapshot tool to an engine tool would be caught as missing.
        let mut specs = tool_surface_snapshot().to_vec();
        specs[0]["name"] = serde_json::json!("build_aem_package");
        let err = verify_tool_surface(&tools_from_specs(&specs)).unwrap_err();
        assert!(err.contains(BROWSER_TOOLS[0]), "{err}");
    }

    #[tokio::test]
    async fn calls_are_forwarded_and_mapped() {
        let mut session = connect(tools_from_specs(tool_surface_snapshot()))
            .await
            .unwrap();

        let reply = session
            .call(
                "browser_navigate",
                &serde_json::json!({"url": "http://localhost:4502/x"}),
            )
            .await;
        assert!(
            matches!(&reply, ToolReply::Text(t) if t.contains("browser_navigate") && t.contains("localhost:4502/x")),
            "{reply:?}"
        );

        let reply = session
            .call("browser_take_screenshot", &serde_json::json!({}))
            .await;
        match reply {
            ToolReply::Blocks(blocks) => {
                assert!(matches!(&blocks[0], ReplyBlock::Text(t) if t.contains("about:blank")));
                assert!(
                    matches!(&blocks[1], ReplyBlock::Image { media_type, .. } if media_type == "image/png")
                );
            }
            other => panic!("expected blocks, got {other:?}"),
        }

        let reply = session.call("browser_close", &serde_json::json!({})).await;
        assert!(matches!(reply, ToolReply::Error(e) if e.contains("no page to close")));
        assert!(
            session.is_connected(),
            "a tool error does not break the session"
        );

        let reply = session
            .call("browser_run_code_unsafe", &serde_json::json!({}))
            .await;
        assert!(matches!(reply, ToolReply::Error(e) if e.contains("Unknown browser tool")));
        assert!(session.take_warnings().is_empty());
        session.shutdown().await;
    }

    /// A call that outlives the call timeout marks the session broken (and says
    /// so); a fixed transport cannot come back.
    #[tokio::test]
    async fn a_hung_call_breaks_the_session() {
        let mut session = connect(tools_from_specs(tool_surface_snapshot()))
            .await
            .unwrap()
            .with_call_timeout(Duration::from_millis(200));
        let reply = session
            .call("browser_wait_for", &serde_json::json!({"time": 5}))
            .await;
        assert!(
            matches!(&reply, ToolReply::Error(e) if e.contains("did not answer") && e.contains("restarted")),
            "{reply:?}"
        );
        assert!(!session.is_connected());
        let warnings = session.take_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("hung")),
            "{warnings:?}"
        );
        let reply = session
            .call("browser_snapshot", &serde_json::json!({}))
            .await;
        assert!(
            matches!(&reply, ToolReply::Error(e) if e.contains("cannot be restarted")),
            "{reply:?}"
        );
        session.shutdown().await;
    }

    /// A server that dies mid-run is restarted on the next call, at most
    /// [`MAX_RESTARTS`] times, each restart reported; after that the browser
    /// tools stay unavailable for the rest of the run.
    #[tokio::test]
    async fn a_lost_server_is_restarted_within_the_budget() {
        let handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let starts = Arc::new(AtomicUsize::new(0));
        let reconnect: Reconnector = {
            let handles = handles.clone();
            let starts = starts.clone();
            Arc::new(move || {
                let handles = handles.clone();
                let starts = starts.clone();
                Box::pin(async move {
                    starts.fetch_add(1, Ordering::SeqCst);
                    let (client_io, handle) = serve_fake(tools_from_specs(tool_surface_snapshot()));
                    handles.lock().unwrap().push(handle);
                    serve_client(client_io, &new_tail()).await
                })
            })
        };
        let kill_server = |handles: &Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>| {
            for h in handles.lock().unwrap().drain(..) {
                h.abort();
            }
        };

        let mut session = BrowserSession::connect_with(reconnect, scratch())
            .await
            .unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        for restart in 1..=MAX_RESTARTS {
            kill_server(&handles);
            let reply = session
                .call("browser_snapshot", &serde_json::json!({}))
                .await;
            assert!(
                matches!(&reply, ToolReply::Error(e) if e.contains("failed") && e.contains("restarted")),
                "{reply:?}"
            );
            assert!(!session.is_connected());

            let reply = session
                .call("browser_snapshot", &serde_json::json!({}))
                .await;
            assert!(matches!(&reply, ToolReply::Text(t) if t.contains("browser_snapshot")), "{reply:?}");
            assert!(session.is_connected());
            assert_eq!(starts.load(Ordering::SeqCst), 1 + usize::from(restart));
            let warnings = session.take_warnings();
            assert!(
                warnings.iter().any(|w| w.contains("lost")) && warnings.iter().any(|w| w.contains(&format!("{restart} of {MAX_RESTARTS}"))),
                "{warnings:?}"
            );
        }

        // One failure too many.
        kill_server(&handles);
        let reply = session
            .call("browser_snapshot", &serde_json::json!({}))
            .await;
        assert!(matches!(&reply, ToolReply::Error(_)), "{reply:?}");
        let reply = session
            .call("browser_snapshot", &serde_json::json!({}))
            .await;
        assert!(
            matches!(&reply, ToolReply::Error(e) if e.contains("no longer available") && e.contains("fetch_aem_dor_pdf")),
            "{reply:?}"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1 + usize::from(MAX_RESTARTS));
        session.shutdown().await;
    }
}
