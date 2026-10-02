//! The persistent AEM (+ Chromium) sessions `verify_run` reuses across
//! calls, replacing the old fresh-container-per-run lifecycle: a real AEM
//! instance with the UBS platform Maven-deployed and its Redacto OSGi
//! config already in place (`docker/aem/README.md`) takes minutes to boot,
//! and Docker Desktop on macOS has no checkpoint/restore to fake a cheap
//! reset -- so paying that cost on every call is not viable the way it was
//! for a from-scratch vanilla image.
//!
//! **One session per `session_id`, not one per profile.** AGENTS.md
//! requires every MCP to support multiple agents running at once without
//! interference; a single shared AEM instance would instead serialize every
//! caller through one Mutex and let one agent's installed package appear on
//! another's screenshot. [`SessionPool`] keys a separate AEM+Chromium
//! session per `session_id` (`crate::specs`'s `verify_run`/`verify_status`
//! argument, populated by the caller -- `u2s-server`'s own conversion run
//! id, in practice), each independently booted, reused, and idle-swept. A
//! caller that omits `session_id` gets [`DEFAULT_SESSION_KEY`], so a
//! human or a test hitting the tool directly still sees the old
//! single-session behaviour. The tradeoff this buys AGENTS.md's guarantee
//! at: N concurrently-active agents means N full AEM instances, each with
//! its own memory and boot cost -- accepted deliberately, not an oversight.
//!
//! `verify_run` installs and later uninstalls only the package under test
//! (`crate::aem_client::AemClient::upload_and_install`/`uninstall`) against
//! whichever AEM instance its `session_id` is already running, rather than
//! the whole container being disposable.
//!
//! **The pool itself is `u2s_verify_core::session::SessionPool`**, shared
//! with the Redacto verifier; its module doc covers the locking. Holding a
//! session's lock for a whole call matters here in particular: a real AEM
//! author cannot safely process concurrent package installs.

use std::time::{Duration, Instant};

use tokio::sync::OwnedMutexGuard;

use u2s_verify_core::docker::{ContainerSpec, DockerLifecycle, RunningContainer, wait_for_http};
use u2s_verify_core::session::{PooledSession, remove_session_network, sanitize_for_docker_name};
pub use u2s_verify_core::session::{DEFAULT_SESSION_KEY, Reach};
use u2s_verify_core::types::{ErrorKind, Finding, VerifyError};

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

/// How `profile` reaches the containers it boots.
pub fn reach_of(profile: &Profile) -> Reach {
    Reach::from_self_container(profile.self_container.as_deref())
}

/// The three addresses a session is used through.
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
        Reach::SessionNetwork { .. } => {
            let ip = chromium_ip
                .ok_or_else(|| "the Chromium container has no address on the session network".to_owned())?;
            Ok(Endpoints {
                aem_base_url: aem_network_url.clone(),
                aem_network_url,
                cdp_url: format!("http://{ip}:{CHROMIUM_CDP_PORT}"),
            })
        }
    }
}

/// The containers and network one session's AEM and Chromium share --
/// booted together in [`boot`] since Chromium's boot cost is negligible
/// next to AEM's and both need the same network, so splitting their
/// lifecycles would add complexity for no benefit.
pub struct Instances {
    pub network: String,
    pub aem: RunningContainer,
    pub chromium: RunningContainer,
    /// How this process reaches the session: see [`Reach`]. Kept so
    /// [`teardown`] can leave the network it joined.
    pub reach: Reach,
    /// The AEM address this *process* uses: a published loopback port
    /// ([`Reach::Published`]) or the container's name on the session
    /// network ([`Reach::SessionNetwork`]). What
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
    /// The image this session booted -- what `verify_status` reports.
    pub aem_image: String,
}

pub struct SessionState {
    pub instances: Instances,
    started_at: Instant,
    last_used: Instant,
    /// The CRX path of whatever `verify_run` most recently installed and
    /// has not yet uninstalled -- `None` once cleaned up. Read and cleared
    /// by `crate::flow::run`, which owns the install/uninstall pairing;
    /// this module only boots, reuses, and tears down the containers
    /// themselves.
    pub installed_package_path: Option<String>,
    /// The one form `crate::interactive` currently has open on this AEM
    /// session, if any. At most one: this session has exactly one
    /// `installed_package_path` slot, so it can only ever have one package
    /// installed and open at a time regardless of which path (`verify_run`
    /// or the interactive tools) put it there. `crate::interactive::open`
    /// fills this and `installed_package_path` together;
    /// `crate::interactive::close` clears both together; `crate::flow::run`
    /// refuses outright (`ErrorKind::FormOpen`) while this is `Some`,
    /// rather than risk uninstalling a package an interactive caller still
    /// has open. `pub(crate)`, like `crate::interactive::OpenForm` itself:
    /// nothing outside this crate ever touches a session's open form
    /// directly.
    pub(crate) open_form: Option<crate::interactive::OpenForm>,
}

impl SessionState {
    /// Marks this session used right now -- every interactive call touches
    /// this the same way `SessionPool::ensure` already does for
    /// `verify_run`, so the idle sweep measures "last interaction", not
    /// "last `verify_run`".
    pub(crate) fn touch(&mut self) {
        self.last_used = Instant::now();
    }
}

/// A point-in-time snapshot for `verify_status`, decoupled from
/// [`SessionState`] so a caller outside this module never needs the lock
/// type in scope.
pub struct SessionStatus {
    pub active: bool,
    pub uptime_secs: Option<u64>,
    pub aem_image: Option<String>,
    /// The form (if any) `crate::interactive` currently has open on this
    /// session -- lets `verify_status` diagnose a handle an agent believes
    /// should still be open (a crashed run that never called
    /// `verify_close`, say) without a dedicated tool for it.
    pub open_form: Option<OpenFormStatus>,
}

pub struct OpenFormStatus {
    pub handle: String,
    pub revision: u64,
}

/// The per-`session_id` AEM + Chromium sessions of one profile.
pub type SessionPool = u2s_verify_core::session::SessionPool<SessionState>;

impl PooledSession for SessionState {
    type Ctx = DockerLifecycle;

    fn last_used(&self) -> Instant {
        self.last_used
    }

    fn touch(&mut self) {
        SessionState::touch(self);
    }

    /// The AEM login page still answers: enough to trust reusing the
    /// session without paying for a full readiness wait again.
    async fn is_alive(&self) -> bool {
        u2s_verify_core::http::is_reachable(
            &format!("{}/libs/granite/core/content/login.html", self.instances.aem_base_url),
            Duration::from_secs(3),
        )
        .await
    }

    /// Whatever form `crate::interactive` had open dies with these
    /// containers, so its browser side is closed first (no uninstall: the
    /// package goes away with the container), rather than leaking a CDP
    /// target and a background task nobody will ever join.
    async fn teardown(mut self, docker: &DockerLifecycle) {
        if let Some(form) = self.open_form.take() {
            form.live.close().await;
        }
        teardown(docker, &self.instances).await;
    }
}

/// `pool`'s session for `key`, booted if none exists or the previous one no
/// longer answers, locked for the caller's whole call -- plus any findings
/// from a fresh boot (empty when an existing session was reused).
pub async fn ensure(
    pool: &SessionPool,
    key: &str,
    docker: &DockerLifecycle,
    profile: &Profile,
) -> Result<(OwnedMutexGuard<Option<SessionState>>, Vec<Finding>), VerifyError> {
    let mut findings = Vec::new();
    let (guard, _booted) = pool
        .ensure(key, docker, || async {
            let (instances, boot_findings) = boot(docker, profile, key).await?;
            findings = boot_findings;
            let now = Instant::now();
            Ok::<_, VerifyError>(SessionState {
                instances,
                started_at: now,
                last_used: now,
                installed_package_path: None,
                open_form: None,
            })
        })
        .await?;
    Ok((guard, findings))
}

/// A point-in-time snapshot of `key`'s session for `verify_status`.
pub async fn status(pool: &SessionPool, key: &str) -> SessionStatus {
    let guard = pool.lock(key).await;
    match guard.as_ref() {
        Some(state) => SessionStatus {
            active: true,
            uptime_secs: Some(state.started_at.elapsed().as_secs()),
            aem_image: Some(state.instances.aem_image.clone()),
            open_form: state.open_form.as_ref().map(|form| OpenFormStatus {
                handle: form.handle.clone(),
                revision: form.revision,
            }),
        },
        None => SessionStatus {
            active: false,
            uptime_secs: None,
            aem_image: None,
            open_form: None,
        },
    }
}

async fn boot(
    docker: &DockerLifecycle,
    profile: &Profile,
    session_id: &str,
) -> Result<(Instances, Vec<Finding>), VerifyError> {
    // The daemon must already hold the AEM image: it is private, and this
    // process has no registry credentials (compose pulls it with the host's
    // login). `ensure_image` finds it locally and never reaches a registry.
    docker
        .ensure_image(&profile.aem_image, &profile.platform)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ImageMissing, err.to_string()))?;
    docker
        .ensure_image(&profile.chromium_image, "")
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ImageMissing, err.to_string()))?;
    let aem_image = profile.aem_image.clone();

    let boot_id = format!(
        "{}-{}",
        sanitize_for_docker_name(session_id),
        uuid::Uuid::new_v4().simple()
    );
    let reach = reach_of(profile);
    let network = format!("u2s-verify-{boot_id}");
    docker
        .ensure_network(&network, &profile.owner_labels())
        .await
        .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    if let Reach::SessionNetwork { self_container } = &reach {
        docker
            .connect_network(&network, self_container)
            .await
            .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    }

    let mut labels = profile.owner_labels();
    labels.insert("u2s.verify".to_owned(), "1".to_owned());
    labels.insert("u2s.verify.session_id".to_owned(), session_id.to_owned());

    // `bollard`'s bind syntax ("source:dest") does not distinguish a named
    // volume from a host path -- Docker resolves that itself from whether
    // the source looks like an absolute path, auto-creating the volume on
    // first use if it does not already exist.
    let aem_binds = vec![format!(
        "{}:{AEM_DATA_VOLUME_CONTAINER_PATH}",
        profile.aem_data_volume
    )];
    let aem_container_name = format!("u2s-verify-aem-{boot_id}");
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

    let chromium = docker
        .run(&ContainerSpec {
            name: format!("u2s-verify-chromium-{boot_id}"),
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

    let chromium_ip = match &reach {
        Reach::Published => None,
        Reach::SessionNetwork { .. } => Some(
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
        &reach,
        &aem_container_name,
        profile.aem_container_port,
        aem.published_port(profile.aem_container_port),
        chromium_ip.as_deref(),
        chromium.published_port(CHROMIUM_CDP_PORT),
    )
    .map_err(|message| VerifyError::new(ErrorKind::AemNotReady, message))?;
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
    .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))?;

    Ok((
        Instances {
            network,
            aem,
            chromium,
            reach,
            aem_base_url,
            aem_network_url,
            cdp_url,
            aem_image,
        },
        Vec::new(),
    ))
}

async fn teardown(docker: &DockerLifecycle, instances: &Instances) {
    for id in [&instances.aem.id, &instances.chromium.id] {
        if let Err(err) = docker.teardown(id).await {
            log::warn!(
                "{}: could not tear down container {id}: {err}",
                crate::LOG_PREFIX
            );
        }
    }
    remove_session_network(docker, &instances.network, &instances.reach).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_endpoints_use_the_host_loopback() {
        let e = endpoints(&Reach::Published, "u2s-verify-aem-x", 8080, Some(49001), None, Some(49002))
            .unwrap();
        assert_eq!(e.aem_base_url, "http://127.0.0.1:49001");
        assert_eq!(e.aem_network_url, "http://u2s-verify-aem-x:8080");
        assert_eq!(e.cdp_url, "http://127.0.0.1:49002");
    }

    #[test]
    fn session_network_endpoints_use_the_container_name_and_chromium_ip() {
        let reach = Reach::SessionNetwork {
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
        let reach = Reach::SessionNetwork {
            self_container: "u2s".to_owned(),
        };
        assert!(endpoints(&reach, "a", 8080, None, None, None).is_err());
    }
}
