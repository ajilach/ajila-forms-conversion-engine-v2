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
//! **Interior mutability, documented.** Each session's state lives behind
//! its own `Arc<tokio::sync::Mutex<Option<SessionState>>>`; [`SessionPool::ensure`]
//! hands the caller an owned guard for the whole duration of a `verify_run`.
//! This is not a workaround for something better solved otherwise: only one
//! call may use a *given* shared AEM instance at a time, since a real AEM
//! author cannot safely process concurrent package installs -- the lock
//! *is* that serialization point, scoped per session so it never blocks a
//! different `session_id`'s call. The pool's own outer map is behind a
//! second, short-lived `tokio::sync::Mutex` guarding only insertion/lookup
//! of which sessions exist, never held across a boot or a `verify_run`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedMutexGuard};

use u2s_verify_core::docker::{ContainerSpec, DockerLifecycle, RunningContainer, wait_for_http};
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

/// The `session_id` a caller that omits one gets -- preserves the
/// single-shared-session behaviour for a human or a test calling the tool
/// directly, with no `u2s-server` conversion run behind it.
pub const DEFAULT_SESSION_KEY: &str = "default";

/// The containers, network, and host-visible download directory one
/// session's AEM and Chromium share -- booted together in [`boot`] since
/// Chromium's boot cost is negligible next to AEM's and both need the same
/// network and bind mount, so splitting their lifecycles would add
/// complexity for no benefit.
pub struct Instances {
    pub network: String,
    pub aem: RunningContainer,
    pub chromium: RunningContainer,
    /// `http://127.0.0.1:<host-published-port>` -- reachable from this
    /// *process*, which runs on the host, not inside any container. What
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
    /// The image tag actually booted (the warm image, or the base image
    /// when no fresh warm image existed) -- what `verify_status` reports
    /// this session is running, since it need not match `profile.aem_image`
    /// literally.
    pub aem_image: String,
    /// Host-visible directory bind-mounted into the Chromium container at
    /// [`CHROMIUM_DOWNLOAD_DIR`] -- fixed for the session's lifetime
    /// (Docker bind mounts cannot be changed after a container is created),
    /// unlike the old per-run directory this replaces. Each call still
    /// removes the one file it downloaded once read; only [`teardown`]
    /// removes the directory itself.
    pub downloads_dir: PathBuf,
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

type Slot = Arc<Mutex<Option<SessionState>>>;

/// The per-profile collection of per-`session_id` sessions -- see the
/// module doc for why one process holds more than one session at a time.
pub struct SessionPool {
    slots: Mutex<HashMap<String, Slot>>,
}

impl Default for SessionPool {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionPool {
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The slot for `key`, creating an empty one if this is the first call
    /// for it. Only the map's own short-lived lock is held here -- never
    /// the slot's, so this never blocks on another call's in-flight boot or
    /// `verify_run`.
    async fn slot(&self, key: &str) -> Slot {
        let mut slots = self.slots.lock().await;
        Arc::clone(
            slots
                .entry(key.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(None))),
        )
    }

    pub async fn status(&self, key: &str) -> SessionStatus {
        let slot = self.slot(key).await;
        let guard = slot.lock().await;
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

    /// How many `session_id`s currently have a booted session -- the
    /// aggregate half of `verify_status`, next to the one `session_id`'s
    /// own detail [`Self::status`] reports.
    pub async fn active_count(&self) -> usize {
        let slots: Vec<Slot> = self.slots.lock().await.values().cloned().collect();
        let mut count = 0;
        for slot in slots {
            if slot.lock().await.is_some() {
                count += 1;
            }
        }
        count
    }

    /// Boots `key`'s session if none exists yet, or the previous one no
    /// longer answers (detected by a quick liveness probe, not just "a slot
    /// exists") -- then hands back an *owned* lock guard (outliving `self`,
    /// since it is cloned out of the `Arc`) for the caller's whole
    /// `verify_run`, plus any findings from a fresh boot (e.g. "not
    /// warmed"; empty when an existing session was reused).
    pub async fn ensure(
        &self,
        key: &str,
        docker: &DockerLifecycle,
        profile: &Profile,
    ) -> Result<(OwnedMutexGuard<Option<SessionState>>, Vec<Finding>), VerifyError> {
        let slot = self.slot(key).await;
        let mut guard = slot.lock_owned().await;

        let alive = match guard.as_ref() {
            Some(state) => probe_alive(&state.instances).await,
            None => false,
        };

        let findings = if alive {
            guard.as_mut().expect("alive implies Some").touch();
            Vec::new()
        } else {
            if let Some(mut stale) = guard.take() {
                log::warn!(
                    "{}: session {key:?}'s containers no longer answer; rebooting",
                    crate::LOG_PREFIX
                );
                // The containers are about to be torn down out from under
                // whatever form `crate::interactive` had open on them --
                // close its browser side (no uninstall: the package is
                // going away with the container regardless) before
                // discarding it, so its `LiveForm` does not leak a CDP
                // target and a background task nobody will ever join.
                if let Some(form) = stale.open_form.take() {
                    form.live.close().await;
                }
                teardown(docker, &stale.instances).await;
            }
            let (instances, boot_findings) = boot(docker, profile, key).await?;
            *guard = Some(SessionState {
                instances,
                started_at: Instant::now(),
                last_used: Instant::now(),
                installed_package_path: None,
                open_form: None,
            });
            boot_findings
        };

        Ok((guard, findings))
    }

    /// Locks `key`'s slot without booting anything -- `None` inside means
    /// no session is currently up for this key. This is the lock
    /// `crate::interactive`'s tools hold for the length of one call, the
    /// same owned-guard shape [`Self::ensure`] hands `verify_run`, so an
    /// interactive call and a `verify_run` on the same `session_id` can
    /// never race each other, and two different `session_id`s never
    /// contend on each other's lock.
    pub async fn lock(&self, key: &str) -> OwnedMutexGuard<Option<SessionState>> {
        let slot = self.slot(key).await;
        slot.lock_owned().await
    }

    /// Tears down every session that has gone unused for at least
    /// `idle_timeout`, and forgets the slot for any session that is not
    /// currently booted -- the safety net a persistent instance needs that
    /// a disposable one never did, so an agent that never comes back does
    /// not run its AEM instance indefinitely. Call periodically from a
    /// background task, never from inside a `verify_run` itself.
    pub async fn sweep_idle(&self, docker: &DockerLifecycle, idle_timeout: Duration) {
        let slots: Vec<(String, Slot)> = self
            .slots
            .lock()
            .await
            .iter()
            .map(|(key, slot)| (key.clone(), Arc::clone(slot)))
            .collect();

        for (key, slot) in &slots {
            let mut guard = slot.lock().await;
            let Some(state) = guard.as_ref() else {
                continue;
            };
            if state.last_used.elapsed() < idle_timeout {
                continue;
            }
            log::info!(
                "{}: tearing down session {key:?} after {:?} idle",
                crate::LOG_PREFIX,
                state.last_used.elapsed()
            );
            if let Some(mut state) = guard.take() {
                // Same reasoning as the reboot path in `ensure`: whatever
                // form was left open dies with these containers, so close
                // its browser side first rather than leak it.
                if let Some(form) = state.open_form.take() {
                    form.live.close().await;
                }
                teardown(docker, &state.instances).await;
            }
        }

        // Forgets a slot only when nothing else is using it right now
        // (`try_lock`): a slot mid-`ensure()` elsewhere must not be pruned
        // out from under it. An empty slot left behind costs nothing but a
        // `HashMap` entry, so leaving one on contention is harmless -- the
        // next sweep tries again.
        let mut slots_map = self.slots.lock().await;
        slots_map.retain(|_, slot| match slot.try_lock() {
            Ok(guard) => guard.is_some(),
            Err(_) => true,
        });
    }
}

/// `true` iff the session's AEM container still answers its login page --
/// enough to trust reusing it without paying for a full readiness wait
/// again, since a session that passed [`boot`]'s own wait once is assumed
/// to still be in the same state unless something external touched it.
async fn probe_alive(instances: &Instances) -> bool {
    u2s_verify_core::http::is_reachable(
        &format!(
            "{}/libs/granite/core/content/login.html",
            instances.aem_base_url
        ),
        Duration::from_secs(3),
    )
    .await
}

/// Picks the AEM image to boot from -- `crate::warm::warm_image_tag`'s warm
/// image when it exists *and* its `u2s.base_image_id` label still matches the
/// base image's current id, the base image otherwise (with a finding naming
/// why, since a cold boot from here can take many minutes longer than a
/// warm one). Never fails on a missing or stale warm image: that is the
/// ordinary state before the first `warm` run, not a problem with the
/// verifier itself.
///
/// **Never called at all when `profile.aem_data_volume` is set.** A
/// commit-based warm image cannot exist for an image that declares
/// `VOLUME /aem/crx-quickstart`: `docker commit` never captures a volume's
/// contents, so `warm_command`'s own commit would silently produce an image
/// missing the entire JCR repository -- confirmed live (the committed image
/// crashed on boot, missing its own quickstart jar). A data-volume profile
/// gets its fast-reboot benefit from the volume itself instead, so
/// [`boot`] boots `profile.aem_image` directly and skips this function
/// rather than risk it picking a warm image `warm_command` should not have
/// been able to build in the first place.
async fn select_aem_image(
    docker: &DockerLifecycle,
    profile: &Profile,
) -> (String, Option<Finding>) {
    let warm_tag = crate::warm::warm_image_tag(&profile.format);
    let base_id = docker.image_id(&profile.aem_image).await.ok().flatten();
    let warm_labels = docker.image_labels(&warm_tag).await.ok().flatten();

    match (&base_id, &warm_labels) {
        (Some(base_id), Some(labels)) if labels.get("u2s.base_image_id") == Some(base_id) => {
            (warm_tag, None)
        }
        (_, Some(_)) => (
            profile.aem_image.clone(),
            Some(Finding::warning(
                "not_warmed",
                format!(
                    "{warm_tag} exists but no longer matches {}; booting the base image \
                     instead -- run this profile's binary's `warm` subcommand again to refresh it",
                    profile.aem_image
                ),
            )),
        ),
        (_, None) => (
            profile.aem_image.clone(),
            Some(Finding::warning(
                "not_warmed",
                format!(
                    "no warm image found for this profile ({warm_tag}); booting {} cold -- \
                     run this profile's binary's `warm` subcommand once to build one",
                    profile.aem_image
                ),
            )),
        ),
    }
}

/// A container name/label component derived from `session_id`: Docker
/// container names only allow `[a-zA-Z0-9_.-]`, and a `session_id` from an
/// external caller (a `RunId` today, but the tool contract does not pin
/// that) is not guaranteed to already be one. Anything else becomes `_`,
/// and a boot-time UUID is appended regardless so two sessions can never
/// collide on a name even if this sanitization maps them to the same
/// string.
fn sanitize_for_docker_name(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

async fn boot(
    docker: &DockerLifecycle,
    profile: &Profile,
    session_id: &str,
) -> Result<(Instances, Vec<Finding>), VerifyError> {
    // The warm image is a local commit, never a registry pull; the base
    // image is the only one `ensure_image` ever needs to reach for, and it
    // must happen before `select_aem_image` so a first-ever boot (base
    // image not pulled yet) still has an id to compare a warm image
    // against rather than always reading as "no match".
    docker
        .ensure_image(&profile.aem_image, &profile.platform)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ImageMissing, err.to_string()))?;
    docker
        .ensure_image(&profile.chromium_image, "")
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ImageMissing, err.to_string()))?;

    // A data-volume profile has nothing for select_aem_image to pick
    // between: no commit-based warm image can exist for it in the first
    // place (see select_aem_image's own doc), and the volume already makes
    // this boot fast once it has been populated once.
    let (aem_image, image_finding) = if profile.aem_data_volume.is_some() {
        (profile.aem_image.clone(), None)
    } else {
        select_aem_image(docker, profile).await
    };

    let boot_id = format!(
        "{}-{}",
        sanitize_for_docker_name(session_id),
        uuid::Uuid::new_v4().simple()
    );
    let network = format!("u2s-verify-{boot_id}");
    docker
        .ensure_network(&network)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;

    let downloads_dir = std::env::temp_dir().join(format!("u2s-verify-downloads-{boot_id}"));
    std::fs::create_dir_all(&downloads_dir)
        .map_err(|err| VerifyError::new(ErrorKind::StorageFailed, err.to_string()))?;

    let mut labels = profile.owner_labels();
    labels.insert("u2s.verify".to_owned(), "1".to_owned());
    labels.insert("u2s.verify.session_id".to_owned(), session_id.to_owned());

    // `bollard`'s bind syntax ("source:dest") does not distinguish a named
    // volume from a host path -- Docker resolves that itself from whether
    // the source looks like an absolute path, auto-creating the volume on
    // first use if it does not already exist. Same mechanism `chromium`'s
    // own download-dir bind below uses, just a volume name instead of a
    // host path as the source.
    let aem_binds = match &profile.aem_data_volume {
        Some(volume) => vec![format!("{volume}:{AEM_DATA_VOLUME_CONTAINER_PATH}")],
        None => Vec::new(),
    };
    let aem_container_name = format!("u2s-verify-aem-{boot_id}");
    let aem = docker
        .run(&ContainerSpec {
            name: aem_container_name.clone(),
            image: aem_image.clone(),
            platform: profile.platform.clone(),
            network: network.clone(),
            env: Vec::new(),
            labels: labels.clone(),
            publish_ports: vec![profile.aem_container_port],
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
            publish_ports: vec![CHROMIUM_CDP_PORT],
            binds: vec![format!(
                "{}:{CHROMIUM_DOWNLOAD_DIR}",
                downloads_dir.display()
            )],
            extra_hosts: Vec::new(),
            memory_bytes: None,
        })
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))?;

    let aem_port = aem
        .published_port(profile.aem_container_port)
        .ok_or_else(|| {
            VerifyError::new(
                ErrorKind::AemNotReady,
                format!(
                    "the AEM container never published port {}",
                    profile.aem_container_port
                ),
            )
        })?;
    let aem_base_url = format!("http://127.0.0.1:{aem_port}");
    let aem_network_url = format!("http://{aem_container_name}:{}", profile.aem_container_port);
    wait_for_http(
        &format!("{aem_base_url}/libs/granite/core/content/login.html"),
        200,
        profile.boot_timeout,
        Duration::from_secs(3),
        None,
    )
    .await
    .map_err(|err| VerifyError::new(ErrorKind::AemNotReady, err.to_string()))?;

    let chromium_port = chromium.published_port(CHROMIUM_CDP_PORT).ok_or_else(|| {
        VerifyError::new(
            ErrorKind::ChromiumNotReady,
            format!("the Chromium container never published port {CHROMIUM_CDP_PORT}"),
        )
    })?;
    let cdp_url = format!("http://127.0.0.1:{chromium_port}");
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
            aem_base_url,
            aem_network_url,
            cdp_url,
            aem_image,
            downloads_dir,
        },
        image_finding.into_iter().collect(),
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
    if let Err(err) = docker.remove_network(&instances.network).await {
        log::warn!(
            "{}: could not remove network {}: {err}",
            crate::LOG_PREFIX,
            instances.network,
        );
    }
    let _ = std::fs::remove_dir_all(&instances.downloads_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing_replaces_anything_unsafe_for_a_docker_name() {
        assert_eq!(sanitize_for_docker_name("run-abc123"), "run-abc123");
        assert_eq!(sanitize_for_docker_name("run/abc:123"), "run_abc_123");
        assert_eq!(sanitize_for_docker_name(""), "");
    }

    #[tokio::test]
    async fn a_pool_with_no_sessions_reports_nothing_active() {
        let pool = SessionPool::new();
        let status = pool.status(DEFAULT_SESSION_KEY).await;
        assert!(!status.active);
        assert_eq!(status.uptime_secs, None);
        assert_eq!(pool.active_count().await, 0);
    }

    #[tokio::test]
    async fn different_session_ids_get_independent_slots() {
        let pool = SessionPool::new();
        // Merely asking for a slot's status must not create a phantom
        // "active" session, and two different keys must not alias to the
        // same slot.
        let a = pool.slot("agent-a").await;
        let b = pool.slot("agent-b").await;
        assert!(!Arc::ptr_eq(&a, &b));
        let a_again = pool.slot("agent-a").await;
        assert!(Arc::ptr_eq(&a, &a_again));
    }
}
