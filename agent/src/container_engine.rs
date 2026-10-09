//! Which container engine runs the verifiers: Docker (the default) or Podman.
//!
//! The vendored u2s verifiers talk to the engine through bollard's
//! `connect_with_local_defaults`, which honours `DOCKER_HOST`. Podman serves
//! the same Docker API on its own socket, so choosing Podman means finding that
//! socket and pointing `DOCKER_HOST` at it — once, at process start, before any
//! thread exists ([`select`]). Nothing in `vendor/` changes.
//!
//! Podman must be 5.3 or newer: the AEM verifier starts its container with
//! `--add-host host.docker.internal:host-gateway`, which older Podman refuses.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// The oldest Podman whose API accepts the `host-gateway` extra host.
pub const MIN_PODMAN: (u32, u32) = (5, 3);

/// The engine the verifiers' containers run on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerEngine {
    #[default]
    Docker,
    Podman,
}

impl ContainerEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerEngine::Docker => "docker",
            ContainerEngine::Podman => "podman",
        }
    }

    /// The name shown to people.
    pub fn label(self) -> &'static str {
        match self {
            ContainerEngine::Docker => "Docker",
            ContainerEngine::Podman => "Podman",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "docker" => Ok(ContainerEngine::Docker),
            "podman" => Ok(ContainerEngine::Podman),
            other => Err(format!("unknown container engine {other:?}: use docker or podman")),
        }
    }
}

/// What [`select`] chose, for the readiness check and its messages.
#[derive(Clone, Debug)]
struct Selection {
    engine: ContainerEngine,
    /// Why Podman cannot be used, when it cannot (no socket found).
    problem: Option<String>,
}

static SELECTED: OnceLock<Selection> = OnceLock::new();

/// Point the verifiers at `engine`. Call once, first thing in `main`, before
/// any thread or async runtime starts: it may set `DOCKER_HOST` for the
/// process. Later calls are ignored — the engine is fixed for the process's
/// lifetime, so a change in the settings takes effect on the next start.
///
/// Docker leaves the environment as it is. Podman keeps a `DOCKER_HOST` the
/// user set themselves, and otherwise sets it to Podman's API socket.
/// Returns a line describing the choice, for the log.
pub fn select(engine: ContainerEngine) -> String {
    let mut note = String::new();
    SELECTED.get_or_init(|| {
        let problem = match engine {
            ContainerEngine::Docker => None,
            ContainerEngine::Podman => match std::env::var("DOCKER_HOST") {
                Ok(host) if !host.trim().is_empty() => {
                    note = format!("Podman via DOCKER_HOST={host}");
                    None
                }
                _ => match podman_socket() {
                    Some(socket) => {
                        let host = format!("unix://{}", socket.display());
                        // SAFETY: called from `main` before any other thread
                        // exists (see the function's contract), so nothing
                        // reads the environment concurrently.
                        unsafe { std::env::set_var("DOCKER_HOST", &host) };
                        note = format!("Podman via DOCKER_HOST={host}");
                        None
                    }
                    None => Some(
                        "Podman is selected but its API socket was not found; start the Podman \
                         machine (`podman machine start`) or Podman Desktop, then restart"
                            .to_string(),
                    ),
                },
            },
        };
        Selection { engine, problem }
    });
    if note.is_empty() {
        note = format!("{} (default socket)", selected().label());
    }
    note
}

/// The engine chosen at start; Docker when [`select`] was never called.
pub fn selected() -> ContainerEngine {
    SELECTED.get().map(|s| s.engine).unwrap_or_default()
}

/// What stands in the way of the selected engine, beyond reachability (which
/// the readiness check tests itself): a Podman socket that was not found, or
/// a Podman older than [`MIN_PODMAN`].
pub fn problems() -> Vec<String> {
    let Some(selection) = SELECTED.get() else {
        return Vec::new();
    };
    if selection.engine != ContainerEngine::Podman {
        return Vec::new();
    }
    if let Some(problem) = &selection.problem {
        return vec![problem.clone()];
    }
    match podman_version() {
        Some(version) if !at_least(&version, MIN_PODMAN) => vec![format!(
            "Podman {version} is too old: the AEM verifier needs Podman {}.{} or newer \
             (for host.docker.internal:host-gateway); upgrade Podman and its machine",
            MIN_PODMAN.0, MIN_PODMAN.1
        )],
        // An unknown version is not a reason to block: reachability is
        // checked separately, and a too-old Podman fails with a clear error.
        _ => Vec::new(),
    }
}

/// The message for an engine that does not answer.
pub fn unreachable_hint() -> String {
    match selected() {
        ContainerEngine::Docker => "Docker is not reachable; start Docker Desktop or the Docker daemon".into(),
        ContainerEngine::Podman => format!(
            "Podman is not reachable{}; start the Podman machine (`podman machine start`) or \
             Podman Desktop",
            std::env::var("DOCKER_HOST").map(|h| format!(" at {h}")).unwrap_or_default()
        ),
    }
}

/// Podman's Docker-compatible API socket on this machine. On macOS and
/// Windows the API lives in the Podman machine and `podman machine inspect`
/// names the forwarded socket; on Linux it is the rootless user socket, or the
/// system one.
fn podman_socket() -> Option<PathBuf> {
    let from_machine = Command::new("podman")
        .args(["machine", "inspect", "--format", "{{.ConnectionInfo.PodmanSocket.Path}}"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            // One line per machine; the first running one's socket exists.
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| PathBuf::from(line.trim()))
                .find(|path| !path.as_os_str().is_empty() && path.exists())
        });
    if from_machine.is_some() {
        return from_machine;
    }
    let mut candidates = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(PathBuf::from(runtime).join("podman/podman.sock"));
    }
    candidates.push(PathBuf::from("/run/podman/podman.sock"));
    candidates.into_iter().find(|path| path.exists())
}

/// The Podman server's version (the machine's, on macOS), as `5.3.1`.
fn podman_version() -> Option<String> {
    let out = Command::new("podman")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// Whether `version` (`major.minor[.patch][-suffix]`) is at least `min`.
fn at_least(version: &str, min: (u32, u32)) -> bool {
    let mut parts = version.split(['.', '-', '+']).map(|p| p.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= min,
        // Unparseable: do not block on it.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_major_then_minor() {
        assert!(at_least("5.3.0", MIN_PODMAN));
        assert!(at_least("5.4.2", MIN_PODMAN));
        assert!(at_least("6.0", MIN_PODMAN));
        assert!(at_least("5.3.0-dev", MIN_PODMAN));
        assert!(!at_least("5.2.5", MIN_PODMAN));
        assert!(!at_least("4.9.4", MIN_PODMAN));
        assert!(at_least("unknown", MIN_PODMAN), "an unreadable version does not block");
    }

    #[test]
    fn the_engine_reads_and_writes_as_lowercase_and_defaults_to_docker() {
        assert_eq!(ContainerEngine::default(), ContainerEngine::Docker);
        assert_eq!(ContainerEngine::parse(" Podman ").unwrap(), ContainerEngine::Podman);
        assert!(ContainerEngine::parse("containerd").is_err());
        assert_eq!(serde_json::to_string(&ContainerEngine::Podman).unwrap(), "\"podman\"");
    }
}
