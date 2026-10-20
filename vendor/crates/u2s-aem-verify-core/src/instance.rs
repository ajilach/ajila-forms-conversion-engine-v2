//! The persistent AEM (+ Chromium) instances `verify_run` reuses across
//! calls, replacing the old fresh-container-per-run lifecycle: a real AEM
//! instance with the UBS platform Maven-deployed and its Redacto OSGi
//! config already in place (`docker/aem/README.md`) takes minutes to boot,
//! and Docker Desktop on macOS has no checkpoint/restore to fake a cheap
//! reset -- so paying that cost on every call is not viable the way it was
//! for a from-scratch vanilla image.
//!
//! **One instance per `instance_id`, not one per profile.** AGENTS.md
//! requires every MCP to support multiple agents running at once without
//! interference; a single shared AEM instance would instead serialize every
//! caller through one Mutex and let one agent's installed package appear on
//! another's screenshot. [`InstancePool`] keys a separate AEM+Chromium
//! instance per `instance_id` (`crate::specs`'s `verify_run`/`verify_status`
//! argument, populated by the caller -- `u2s-server`'s own conversion run
//! id, in practice), each independently booted, reused, and idle-swept. A
//! caller that omits `instance_id` gets [`DEFAULT_INSTANCE_KEY`], so a
//! human or a test hitting the tool directly still sees the old
//! single-instance behaviour. The tradeoff this buys AGENTS.md's guarantee
//! at: N concurrently-active agents means N full AEM containers, each with
//! its own memory and boot cost -- accepted deliberately, not an oversight.
//!
//! `verify_run` installs and later uninstalls only the package under test
//! (`crate::aem_client::AemClient::upload_and_install`/`uninstall`) against
//! whichever AEM instance its `instance_id` is already running, rather than
//! the whole container being disposable.
//!
//! **The pool itself is `u2s_verify_core::instance::InstancePool`**, shared
//! across verifiers; its module doc covers the locking. Holding an
//! instance's lock for a whole call matters here in particular: a real AEM
//! author cannot safely process concurrent package installs.
//!
//! **Sessions live inside an instance.** Each form `verify_open` opens is a
//! session: its own browser tab on the instance's one Chromium, recorded in
//! [`InstanceState::sessions`] (`crate::interactive`'s module doc).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tokio::sync::OwnedMutexGuard;

use u2s_verify_core::docker::{
    ContainerSpec, ContainerState, DockerLifecycle, RunningContainer, wait_for_http,
};
use u2s_verify_core::instance::{PooledInstance, new_boot_id, remove_instance_network};
pub use u2s_verify_core::instance::{DEFAULT_INSTANCE_KEY, Reach};
use u2s_verify_core::types::{ErrorKind, Finding, VerifyError};

use crate::packages::PackageTable;
use crate::profile::Profile;

const CHROMIUM_CDP_PORT: u16 = 9222;
pub(crate) const CHROMIUM_DOWNLOAD_DIR: &str = "/downloads";
/// Where `profile.aem_data_volume` mounts, matching the `VOLUME` path
/// `ajila.azurecr.io/aemforms-arm` itself declares (confirmed via
/// `docker inspect`) -- a fixed constant, not a second profile setting,
/// since this crate has exactly one real image to design against; a future
/// AEM image using a different path is a reason to add one, not to guess
/// at flexibility now.
const AEM_DATA_VOLUME_CONTAINER_PATH: &str = "/aem/crx-quickstart";
/// AEM's own error log inside its container, under the data volume.
pub(crate) const AEM_ERROR_LOG: &str = "/aem/crx-quickstart/logs/error.log";

/// How `profile` reaches the containers it boots.
pub fn reach_of(profile: &Profile) -> Reach {
    Reach::from_self_container(profile.self_container.as_deref())
}

/// The three addresses an instance is used through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoints {
    pub aem_base_url: String,
    pub aem_network_url: String,
    pub cdp_url: String,
}

/// Builds [`Endpoints`] from what boot learned about the two containers.
/// Pure function. Unit-tested.
///
/// Chromium is addressed by IP, never by container name: its DevTools
/// endpoint refuses a `Host` header that is neither an IP nor `localhost`,
/// and `chromiumoxide` keeps the address it connected through for the
/// websocket it opens next.
pub(crate) fn endpoints(
    reach: &Reach,
    aem_container_name: &str,
    aem_container_port: u16,
    aem_published_port: Option<u16>,
    chromium_ip: Option<&str>,
    chromium_published_port: Option<u16>,
) -> Result<Endpoints, String> {
    let aem_network_url = format!("http://{aem_container_name}:{aem_container_port}");
    match reach {
        Reach::Published => {
            let aem_port = aem_published_port.ok_or_else(|| {
                format!("the AEM container never published port {aem_container_port}")
            })?;
            let cdp_port = chromium_published_port.ok_or_else(|| {
                format!("the Chromium container never published port {CHROMIUM_CDP_PORT}")
            })?;
            Ok(Endpoints {
                aem_base_url: format!("http://127.0.0.1:{aem_port}"),
                aem_network_url,
                cdp_url: format!("http://127.0.0.1:{cdp_port}"),
            })
        }
        Reach::InstanceNetwork { .. } => {
            let ip = chromium_ip
                .ok_or_else(|| "the Chromium container has no address on the instance network".to_owned())?;
            Ok(Endpoints {
                aem_base_url: aem_network_url.clone(),
                aem_network_url,
                cdp_url: format!("http://{ip}:{CHROMIUM_CDP_PORT}"),
            })
        }
    }
}

/// The containers and network one instance's AEM and Chromium share --
/// booted together in [`boot`] since Chromium's boot cost is negligible
/// next to AEM's and both need the same network, so splitting their
/// lifecycles would add complexity for no benefit.
pub struct Containers {
    pub network: String,
    pub aem: RunningContainer,
    pub chromium: RunningContainer,
    /// How this process reaches the instance: see [`Reach`]. Kept so
    /// [`teardown`] can leave the network it joined.
    pub reach: Reach,
    /// The AEM address this *process* uses: a published loopback port
    /// ([`Reach::Published`]) or the container's name on the instance
    /// network ([`Reach::InstanceNetwork`]). What
    /// [`crate::aem_client::AemClient`] and every readiness wait use.
    pub aem_base_url: String,
    /// `http://<aem container name>:<container-internal port>` --
    /// reachable from *inside* `chromium`'s own container instead, over
    /// the Docker network both containers share (Docker's embedded DNS
    /// resolves a container by the name it was created with). Confirmed
    /// live this is genuinely a different address from `aem_base_url`, not
    /// an equivalent one: the Chromium container cannot reach
    /// `127.0.0.1:<host port>` at all (that loopback is its own, not the
    /// AEM container's) -- `chromiumoxide`'s driven navigation must use
    /// this one instead.
    pub aem_network_url: String,
    pub cdp_url: String,
    /// The image this instance booted -- what `verify_status` reports.
    pub aem_image: String,
}

pub struct InstanceState {
    pub containers: Containers,
    started_at: Instant,
    last_used: Instant,
    /// Every package this instance's AEM has installed and who still holds
    /// it (`crate::packages`'s module doc). `verify_run` and every open
    /// session acquire and release through this one table, so neither can
    /// uninstall a package the other still needs.
    pub(crate) packages: PackageTable,
    /// The forms open on this instance, one browser tab each, keyed by the
    /// handle `verify_open` returned. `crate::flow::run` walks beside them
    /// in a page of its own. `pub(crate)`, like
    /// `crate::interactive::OpenSession` itself.
    pub(crate) sessions: BTreeMap<String, crate::interactive::OpenSession>,
}

impl InstanceState {
    /// Marks this instance used right now -- every interactive call touches
    /// this the same way `InstancePool::ensure` already does for
    /// `verify_run`, so the idle sweep measures "last interaction", not
    /// "last `verify_run`".
    pub(crate) fn touch(&mut self) {
        self.last_used = Instant::now();
    }
}

/// A point-in-time snapshot for `verify_status`, decoupled from
/// [`InstanceState`] so a caller outside this module never needs the lock
/// type in scope.
pub struct InstanceStatus {
    pub active: bool,
    pub uptime_secs: Option<u64>,
    pub aem_image: Option<String>,
    /// The sessions open on this instance -- lets `verify_status` diagnose
    /// a handle an agent believes should still be open (a crashed run that
    /// never called `verify_close`, say) without a dedicated tool for it.
    pub sessions: Vec<SessionStatus>,
}

pub struct SessionStatus {
    pub handle: String,
    pub form_jcr_path: String,
    pub revision: u64,
}

/// The per-`instance_id` AEM + Chromium instances of one profile.
pub type InstancePool = u2s_verify_core::instance::InstancePool<InstanceState>;

impl PooledInstance for InstanceState {
    type Ctx = DockerLifecycle;

    fn last_used(&self) -> Instant {
        self.last_used
    }

    fn touch(&mut self) {
        InstanceState::touch(self);
    }

    /// The AEM login page and Chromium's DevTools endpoint both still
    /// answer: enough to trust reusing the instance without paying for a
    /// full readiness wait again. Chromium counts too: an instance whose
    /// browser died would otherwise be reused, and every open on it fail.
    async fn is_alive(&self) -> bool {
        let probe = Duration::from_secs(3);
        let login = format!("{}/libs/granite/core/content/login.html", self.containers.aem_base_url);
        let devtools = format!("{}/json/version", self.containers.cdp_url);
        let (aem, chromium) = tokio::join!(
            u2s_verify_core::http::is_reachable(&login, probe),
            u2s_verify_core::http::is_reachable(&devtools, probe),
        );
        aem && chromium
    }

    /// Whatever form `crate::interactive` had open dies with these
    /// containers, so its browser side is closed first (no uninstall: the
    /// package goes away with the container), rather than leaking a CDP
    /// target and a background task nobody will ever join.
    async fn teardown(self, docker: &DockerLifecycle) {
        for session in self.sessions.into_values() {
            session.live.close().await;
        }
        teardown(docker, &self.containers).await;
    }
}

/// `pool`'s instance for `key`, booted if none exists or the previous one no
/// longer answers, locked for the caller's whole call -- plus any findings
/// from a fresh boot (empty when an existing instance was reused).
pub async fn ensure(
    pool: &InstancePool,
    key: &str,
    docker: &DockerLifecycle,
    profile: &Profile,
) -> Result<(OwnedMutexGuard<Option<InstanceState>>, Vec<Finding>), VerifyError> {
    let mut findings = Vec::new();
    let (guard, _booted) = pool
        .ensure(key, docker, || async {
            let (containers, boot_findings) = boot(docker, profile, key).await?;
            findings = boot_findings;
            let now = Instant::now();
            Ok::<_, VerifyError>(InstanceState {
                containers,
                started_at: now,
                last_used: now,
                packages: PackageTable::default(),
                sessions: BTreeMap::new(),
            })
        })
        .await?;
    Ok((guard, findings))
}

/// Boots `key`'s instance ahead of its first use, so the call that needs
/// it (`verify_open`, `verify_run`) finds AEM already up instead of
/// waiting minutes for it. Does nothing to an instance that is already up.
/// Holds the instance's lock only while booting: a call that arrives
/// meanwhile waits for the boot rather than starting a second one.
pub async fn warm_up(
    pool: &InstancePool,
    key: &str,
    profile: &Profile,
) -> Result<Vec<Finding>, VerifyError> {
    let docker = DockerLifecycle::connect()
        .await
        .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    let (_guard, findings) = ensure(pool, key, &docker, profile).await?;
    Ok(findings)
}

/// A point-in-time snapshot of `key`'s instance for `verify_status`.
pub async fn status(pool: &InstancePool, key: &str) -> InstanceStatus {
    let guard = pool.lock(key).await;
    match guard.as_ref() {
        Some(state) => InstanceStatus {
            active: true,
            uptime_secs: Some(state.started_at.elapsed().as_secs()),
            aem_image: Some(state.containers.aem_image.clone()),
            sessions: state
                .sessions
                .values()
                .map(|session| SessionStatus {
                    handle: session.handle.clone(),
                    form_jcr_path: session.live.package.summary.form_jcr_path.clone(),
                    revision: session.revision,
                })
                .collect(),
        },
        None => InstanceStatus {
            active: false,
            uptime_secs: None,
            aem_image: None,
            sessions: Vec::new(),
        },
    }
}

/// The names of one boot's network and containers. Chromium opens the form
/// by the AEM container's name, so every name must be one DNS label
/// (`u2s_verify_core::instance::DNS_LABEL_MAX`); `boot_id` comes from
/// `new_boot_id`. Pure. Unit-tested.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Names {
    network: String,
    aem: String,
    chromium: String,
}

impl Names {
    fn for_boot(boot_id: &str) -> Self {
        Self {
            network: format!("u2s-verify-{boot_id}"),
            aem: format!("u2s-verify-aem-{boot_id}"),
            chromium: format!("u2s-verify-chromium-{boot_id}"),
        }
    }
}

/// What a boot has created so far, so a boot that fails partway removes it
/// again rather than leaving containers and a network running that nothing
/// will ever reuse (the leftover sweep only runs at the next server start).
/// Containers are recorded by name before they are created, since a create
/// that succeeds followed by a start that fails still leaves one behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Created {
    network: Option<String>,
    containers: Vec<String>,
}

/// One step of removing what a boot created ([`Created::cleanup_steps`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum CleanupStep {
    RemoveContainer(String),
    RemoveNetwork(String),
}

impl Created {
    /// Containers first, newest first, then the network: Docker refuses to
    /// remove a network a container is still attached to. Pure.
    /// Unit-tested.
    fn cleanup_steps(&self) -> Vec<CleanupStep> {
        self.containers
            .iter()
            .rev()
            .cloned()
            .map(CleanupStep::RemoveContainer)
            .chain(self.network.iter().cloned().map(CleanupStep::RemoveNetwork))
            .collect()
    }
}

/// Removes everything `created` names, logging what could not be removed:
/// the one teardown both a finished instance and a failed boot go through.
async fn clean_up(docker: &DockerLifecycle, created: &Created, reach: &Reach) {
    for step in created.cleanup_steps() {
        match step {
            CleanupStep::RemoveContainer(id) => {
                if let Err(err) = docker.teardown(&id).await {
                    log::warn!(
                        "{}: could not tear down container {id}: {err}",
                        crate::LOG_PREFIX
                    );
                }
            }
            CleanupStep::RemoveNetwork(network) => {
                remove_instance_network(docker, &network, reach).await;
            }
        }
    }
}

/// How often [`boot`] checks that its containers are still running while it
/// waits for them to become ready.
const CONTAINER_STATE_POLL: Duration = Duration::from_secs(10);
/// How many of a dead container's last log lines its boot error carries.
const DEAD_CONTAINER_LOG_LINES: usize = 30;

/// One container [`boot`] waits on: what to call it in an error, and which
/// error kind its death is.
struct Watched<'a> {
    id: &'a str,
    role: &'static str,
    kind: ErrorKind,
}

/// Why a container that should be running is not, or `None` while it runs.
/// `logs` is its log tail, or why that could not be read. Pure.
/// Unit-tested.
fn dead_container_message(role: &str, state: &ContainerState, logs: &Result<String, String>) -> Option<String> {
    let what = match state {
        ContainerState::Running => return None,
        ContainerState::Gone => format!("the {role} container no longer exists"),
        ContainerState::Stopped {
            status,
            exit_code: Some(code),
        } if status == "exited" => format!("the {role} container exited with code {code}"),
        ContainerState::Stopped { status, exit_code } => format!(
            "the {role} container is {status}{}",
            exit_code.map(|code| format!(" (exit code {code})")).unwrap_or_default()
        ),
    };
    let logs = match logs {
        Ok(logs) if logs.trim().is_empty() => "it wrote no log output".to_owned(),
        Ok(logs) => format!("its last {DEAD_CONTAINER_LOG_LINES} log lines:\n{}", logs.trim_end()),
        Err(err) => format!("its logs could not be read: {err}"),
    };
    Some(format!("{what} while booting; {logs}"))
}

/// Polls `watched` every [`CONTAINER_STATE_POLL`] and returns once one of
/// them is no longer running, with the error naming it. Never returns
/// while all run: [`boot`] races it against the readiness waits.
async fn first_dead_container(docker: &DockerLifecycle, watched: &[Watched<'_>]) -> VerifyError {
    let mut ticks = tokio::time::interval(CONTAINER_STATE_POLL);
    loop {
        ticks.tick().await;
        for container in watched {
            let state = match docker.container_state(container.id).await {
                Ok(state) => state,
                Err(err) => return VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()),
            };
            if state == ContainerState::Running {
                continue;
            }
            let logs = match state {
                ContainerState::Gone => Err("the container is gone".to_owned()),
                _ => docker
                    .container_logs(container.id, DEAD_CONTAINER_LOG_LINES)
                    .await
                    .map_err(|err| err.to_string()),
            };
            let message = dead_container_message(container.role, &state, &logs)
                .expect("a container that is not running has a message");
            return VerifyError::new(container.kind, message);
        }
    }
}

async fn boot(
    docker: &DockerLifecycle,
    profile: &Profile,
    instance_id: &str,
) -> Result<(Containers, Vec<Finding>), VerifyError> {
    // The AEM image is private: a daemon that does not have it yet pulls it
    // with the profile's registry login (none configured: the pull fails
    // unless an operator pulled it beforehand).
    docker
        .ensure_image(
            &profile.aem_image,
            &profile.platform,
            profile.registry_credentials.as_ref(),
        )
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ImageMissing, err.to_string()))?;
    docker
        .ensure_image(&profile.chromium_image, "", None)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ImageMissing, err.to_string()))?;

    let names = Names::for_boot(&new_boot_id());
    let reach = reach_of(profile);
    let mut created = Created::default();
    match boot_containers(docker, profile, instance_id, names, &reach, &mut created).await {
        Ok(containers) => Ok((containers, Vec::new())),
        Err(err) => {
            log::warn!(
                "{}: boot of instance {instance_id:?} failed, removing what it created: {err}",
                crate::LOG_PREFIX
            );
            clean_up(docker, &created, &reach).await;
            Err(err)
        }
    }
}

/// [`boot`]'s part from the network on: everything it creates is recorded
/// in `created` before it is created, for [`boot`] to remove on failure.
async fn boot_containers(
    docker: &DockerLifecycle,
    profile: &Profile,
    instance_id: &str,
    names: Names,
    reach: &Reach,
    created: &mut Created,
) -> Result<Containers, VerifyError> {
    let aem_image = profile.aem_image.clone();
    let Names {
        network,
        aem: aem_container_name,
        chromium: chromium_container_name,
    } = names;
    created.network = Some(network.clone());
    docker
        .ensure_network(&network, &profile.owner_labels())
        .await
        .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    if let Reach::InstanceNetwork { self_container } = reach {
        docker
            .connect_network(&network, self_container)
            .await
            .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    }

    let mut labels = profile.owner_labels();
    labels.insert("u2s.verify".to_owned(), "1".to_owned());
    labels.insert("u2s.verify.instance_id".to_owned(), instance_id.to_owned());

    // `bollard`'s bind syntax ("source:dest") does not distinguish a named
    // volume from a host path -- Docker resolves that itself from whether
    // the source looks like an absolute path, auto-creating the volume on
    // first use if it does not already exist.
    let aem_binds = vec![format!(
        "{}:{AEM_DATA_VOLUME_CONTAINER_PATH}",
        profile.aem_data_volume
    )];
    created.containers.push(aem_container_name.clone());
    let aem = docker
        .run(&ContainerSpec {
            name: aem_container_name.clone(),
            image: aem_image.clone(),
            platform: profile.platform.clone(),
            network: network.clone(),
            env: Vec::new(),
            labels: labels.clone(),
            publish_ports: if reach.publishes() {
                vec![profile.aem_container_port]
            } else {
                Vec::new()
            },
            binds: aem_binds,
            // Needed for a UBS profile's Redacto OSGi config
            // (`profile.redacto_url`, typically `http://host.docker.internal:.../`)
            // to resolve from inside the container: Docker Desktop on macOS
            // provides this mapping automatically, but a Linux Docker host
            // does not unless told to (the same flag `docker/aem/README.md`'s
            // bake script already passes for exactly this reason).
            extra_hosts: vec!["host.docker.internal:host-gateway".to_owned()],
            memory_bytes: None,
        })
        .await
        .map_err(|err| VerifyError::new(ErrorKind::AemNotReady, err.to_string()))?;

    created.containers.push(chromium_container_name.clone());
    let chromium = docker
        .run(&ContainerSpec {
            name: chromium_container_name,
            image: profile.chromium_image.clone(),
            // The Chromium image ships its own architecture-appropriate
            // build; only the AEM image needs the platform pin this host
            // would otherwise not run.
            platform: String::new(),
            network: network.clone(),
            env: Vec::new(),
            labels,
            publish_ports: if reach.publishes() {
                vec![CHROMIUM_CDP_PORT]
            } else {
                Vec::new()
            },
            // Downloads stay inside the container; `crate::flow` copies
            // each one out through the Engine's archive endpoint.
            binds: Vec::new(),
            extra_hosts: Vec::new(),
            memory_bytes: None,
        })
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))?;

    let chromium_ip = match reach {
        Reach::Published => None,
        Reach::InstanceNetwork { .. } => Some(
            docker
                .container_ip(&chromium.id, &network)
                .await
                .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))?,
        ),
    };
    let Endpoints {
        aem_base_url,
        aem_network_url,
        cdp_url,
    } = endpoints(
        reach,
        &aem_container_name,
        profile.aem_container_port,
        aem.published_port(profile.aem_container_port),
        chromium_ip.as_deref(),
        chromium.published_port(CHROMIUM_CDP_PORT),
    )
    .map_err(|message| VerifyError::new(ErrorKind::AemNotReady, message))?;

    // AEM takes minutes to boot. A container that dies meanwhile (out of
    // memory, a corrupt data volume) is noticed within one state poll
    // rather than after the whole boot timeout.
    let ready = async {
        wait_for_http(
            &format!("{aem_base_url}/libs/granite/core/content/login.html"),
            200,
            profile.boot_timeout,
            Duration::from_secs(3),
            None,
        )
        .await
        .map_err(|err| VerifyError::new(ErrorKind::AemNotReady, err.to_string()))?;
        wait_for_http(
            &format!("{cdp_url}/json/version"),
            200,
            Duration::from_secs(30),
            Duration::from_millis(300),
            None,
        )
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))
    };
    let watched = [
        Watched {
            id: &aem.id,
            role: "AEM",
            kind: ErrorKind::AemNotReady,
        },
        Watched {
            id: &chromium.id,
            role: "Chromium",
            kind: ErrorKind::ChromiumNotReady,
        },
    ];
    tokio::select! {
        ready = ready => ready?,
        dead = first_dead_container(docker, &watched) => return Err(dead),
    }

    Ok(Containers {
        network,
        aem,
        chromium,
        reach: reach.clone(),
        aem_base_url,
        aem_network_url,
        cdp_url,
        aem_image,
    })
}

async fn teardown(docker: &DockerLifecycle, containers: &Containers) {
    let created = Created {
        network: Some(containers.network.clone()),
        containers: vec![containers.aem.id.clone(), containers.chromium.id.clone()],
    };
    clean_up(docker, &created, &containers.reach).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_boot_removes_its_containers_before_its_network() {
        let created = Created {
            network: Some("net".to_owned()),
            containers: vec!["aem".to_owned(), "chromium".to_owned()],
        };
        assert_eq!(
            created.cleanup_steps(),
            vec![
                CleanupStep::RemoveContainer("chromium".to_owned()),
                CleanupStep::RemoveContainer("aem".to_owned()),
                CleanupStep::RemoveNetwork("net".to_owned()),
            ]
        );
        let only_network = Created {
            network: Some("net".to_owned()),
            containers: Vec::new(),
        };
        assert_eq!(
            only_network.cleanup_steps(),
            vec![CleanupStep::RemoveNetwork("net".to_owned())]
        );
        assert!(Created::default().cleanup_steps().is_empty());
    }

    #[test]
    fn a_running_container_is_not_dead() {
        assert_eq!(dead_container_message("AEM", &ContainerState::Running, &Ok(String::new())), None);
    }

    #[test]
    fn a_dead_container_is_named_with_its_exit_code_and_logs() {
        let exited = ContainerState::Stopped {
            status: "exited".to_owned(),
            exit_code: Some(137),
        };
        let message =
            dead_container_message("AEM", &exited, &Ok("line 1\nOutOfMemoryError\n".to_owned()))
                .unwrap();
        assert_eq!(
            message,
            "the AEM container exited with code 137 while booting; its last 30 log lines:\nline 1\nOutOfMemoryError"
        );

        let gone = dead_container_message(
            "Chromium",
            &ContainerState::Gone,
            &Err("the container is gone".to_owned()),
        )
        .unwrap();
        assert!(gone.starts_with("the Chromium container no longer exists"), "{gone}");
        assert!(gone.ends_with("its logs could not be read: the container is gone"), "{gone}");

        let paused = ContainerState::Stopped {
            status: "paused".to_owned(),
            exit_code: None,
        };
        let message = dead_container_message("AEM", &paused, &Ok(String::new())).unwrap();
        assert_eq!(message, "the AEM container is paused while booting; it wrote no log output");
    }

    #[test]
    fn the_error_log_lives_on_the_data_volume() {
        assert!(AEM_ERROR_LOG.starts_with(&format!("{AEM_DATA_VOLUME_CONTAINER_PATH}/")));
    }

    #[test]
    fn every_name_fits_one_dns_label() {
        let names = Names::for_boot(&new_boot_id());
        for name in [&names.network, &names.aem, &names.chromium] {
            assert!(
                name.len() <= u2s_verify_core::instance::DNS_LABEL_MAX,
                "{name} is longer than one DNS label"
            );
        }
        assert_ne!(names.aem, names.chromium);
    }

    #[test]
    fn published_endpoints_use_the_host_loopback() {
        let e = endpoints(&Reach::Published, "u2s-verify-aem-x", 8080, Some(49001), None, Some(49002))
            .unwrap();
        assert_eq!(e.aem_base_url, "http://127.0.0.1:49001");
        assert_eq!(e.aem_network_url, "http://u2s-verify-aem-x:8080");
        assert_eq!(e.cdp_url, "http://127.0.0.1:49002");
    }

    #[test]
    fn instance_network_endpoints_use_the_container_name_and_chromium_ip() {
        let reach = Reach::InstanceNetwork {
            self_container: "u2s".to_owned(),
        };
        let e = endpoints(&reach, "u2s-verify-aem-x", 8080, None, Some("172.20.0.3"), None).unwrap();
        assert_eq!(e.aem_base_url, "http://u2s-verify-aem-x:8080");
        assert_eq!(e.aem_network_url, e.aem_base_url);
        assert_eq!(e.cdp_url, "http://172.20.0.3:9222");
    }

    #[test]
    fn a_missing_address_is_an_error_not_a_guess() {
        assert!(endpoints(&Reach::Published, "a", 8080, None, None, Some(1)).is_err());
        assert!(endpoints(&Reach::Published, "a", 8080, Some(1), None, None).is_err());
        let reach = Reach::InstanceNetwork {
            self_container: "u2s".to_owned(),
        };
        assert!(endpoints(&reach, "a", 8080, None, None, None).is_err());
    }
}
