//! Persistent per-`session_id` verification sessions, shared by every
//! verifier that boots its own containers (`u2s-aem-verify-core`'s AEM +
//! Chromium, `u2s-redacto-verify-core`'s Redacto platform).
//!
//! **One session per `session_id`.** AGENTS.md requires every MCP to support
//! multiple agents running at once without interference, so [`SessionPool`]
//! keys a separate session per `session_id` (the tool argument a caller
//! populates, `u2s-server`'s own conversion run id in practice), each
//! independently booted, reused, and idle-swept. A caller that omits
//! `session_id` gets [`DEFAULT_SESSION_KEY`].
//!
//! **Interior mutability, documented.** Each session lives behind its own
//! `Arc<tokio::sync::Mutex<Option<S>>>`; [`SessionPool::ensure`] hands the
//! caller an owned guard for the whole duration of one call. The lock *is*
//! the serialization point a session needs (one call at a time against one
//! booted instance), scoped per session so it never blocks a different
//! `session_id`. The pool's outer map sits behind a second, short-lived
//! `tokio::sync::Mutex` guarding only which sessions exist, never held
//! across a boot or a call.
//!
//! Also here: how this process reaches the containers it boots ([`Reach`]),
//! and the startup and idle cleanup every such verifier runs.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::docker::DockerLifecycle;

const LOG_PREFIX: &str = "u2s-verify-core";

/// The `session_id` a caller that omits one gets.
pub const DEFAULT_SESSION_KEY: &str = "default";

/// The Docker label key naming which verifier profile a container or
/// network belongs to. One constant for the writer ([`owner_labels`]) and
/// the filter reading it back ([`owner_label`]): a key that drifted between
/// them would fail silently, as an empty match.
const OWNER_LABEL_KEY: &str = "u2s.verify.format";

/// The label filter matching every resource `owner` created
/// ([`DockerLifecycle::find_by_label`]'s argument).
pub fn owner_label(owner: &str) -> String {
    format!("{OWNER_LABEL_KEY}={owner}")
}

/// The labels every container and network `owner` creates carries: the
/// write side of [`owner_label`].
pub fn owner_labels(owner: &str) -> HashMap<String, String> {
    HashMap::from([(OWNER_LABEL_KEY.to_owned(), owner.to_owned())])
}

/// How this process reaches the containers a session boots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// This process runs on the Docker host itself (`cargo run`): through
    /// ports published on `127.0.0.1`.
    Published,
    /// This process runs in the container `self_container`: it joins each
    /// session's network and addresses the siblings there directly, so no
    /// port is published and nothing depends on sharing the host's network.
    SessionNetwork { self_container: String },
}

impl Reach {
    /// `U2S_VERIFY_SELF_CONTAINER`'s value: set and non-empty means
    /// [`Reach::SessionNetwork`].
    pub fn from_self_container(self_container: Option<&str>) -> Self {
        match self_container.filter(|name| !name.is_empty()) {
            Some(name) => Self::SessionNetwork {
                self_container: name.to_owned(),
            },
            None => Self::Published,
        }
    }

    pub fn publishes(&self) -> bool {
        matches!(self, Self::Published)
    }
}

/// A container name component derived from `session_id`: Docker names only
/// allow `[a-zA-Z0-9_.-]`, and a `session_id` from an external caller is not
/// guaranteed to be one. Anything else becomes `_`; callers append a
/// boot-time UUID regardless, so two sessions never collide on a name even
/// if this maps them to the same string. Pure function. Unit-tested.
pub fn sanitize_for_docker_name(session_id: &str) -> String {
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

/// Leaves `network` (when this process joined it) and removes it.
pub async fn remove_session_network(docker: &DockerLifecycle, network: &str, reach: &Reach) {
    if let Reach::SessionNetwork { self_container } = reach
        && let Err(err) = docker.disconnect_network(network, self_container).await
    {
        log::warn!("{LOG_PREFIX}: could not leave network {network}: {err}");
    }
    if let Err(err) = docker.remove_network(network).await {
        log::warn!("{LOG_PREFIX}: could not remove network {network}: {err}");
    }
}

/// Tears down every container and network labelled for `owner`, found
/// rather than tracked in memory. Run once at server startup: a session a
/// crashed or restarted process booted is otherwise never noticed again.
/// Best-effort: Docker being unreachable is logged, not a startup failure,
/// since the server must still start and serve `verify_status`.
pub async fn remove_leftovers(owner: &str, reach: &Reach) {
    let docker = match DockerLifecycle::connect().await {
        Ok(docker) => docker,
        Err(err) => {
            log::warn!("{LOG_PREFIX}: could not check for leftover containers: {err}");
            return;
        }
    };
    let label = owner_label(owner);
    match docker.find_by_label(&label).await {
        Ok(ids) => {
            for id in ids {
                log::warn!("{LOG_PREFIX}: removing a leftover container from a prior run: {id}");
                if let Err(err) = docker.teardown(&id).await {
                    log::warn!("{LOG_PREFIX}: could not remove leftover container {id}: {err}");
                }
            }
        }
        Err(err) => log::warn!("{LOG_PREFIX}: could not list leftover containers: {err}"),
    }
    // Session networks carry the same label. A network this process joined
    // in a prior life must be left before it can go.
    match docker.find_networks_by_label(&label).await {
        Ok(networks) => {
            for network in networks {
                remove_session_network(&docker, &network, reach).await;
            }
        }
        Err(err) => log::warn!("{LOG_PREFIX}: could not list leftover networks: {err}"),
    }
}

/// One booted session a [`SessionPool`] holds.
pub trait PooledSession: Send + 'static {
    /// What [`Self::teardown`] needs: [`DockerLifecycle`] in production,
    /// `()` in a unit test.
    type Ctx: Sync;

    fn last_used(&self) -> Instant;

    /// Marks the session used right now, so the idle sweep measures "last
    /// interaction".
    fn touch(&mut self);

    /// A quick liveness probe: `false` makes [`SessionPool::ensure`] tear
    /// the session down and boot a new one.
    fn is_alive(&self) -> impl Future<Output = bool> + Send;

    /// Releases everything the session holds. Best-effort: failures are
    /// logged by the implementation, since there is nothing a caller could
    /// do about them.
    fn teardown(self, ctx: &Self::Ctx) -> impl Future<Output = ()> + Send;
}

type Slot<S> = Arc<Mutex<Option<S>>>;

/// The per-`session_id` sessions of one verifier process. See the module
/// doc for the locking.
pub struct SessionPool<S> {
    slots: Mutex<HashMap<String, Slot<S>>>,
}

impl<S: PooledSession> Default for SessionPool<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: PooledSession> SessionPool<S> {
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The slot for `key`, created empty on first use. Only the map's own
    /// short-lived lock is held here.
    async fn slot(&self, key: &str) -> Slot<S> {
        let mut slots = self.slots.lock().await;
        Arc::clone(
            slots
                .entry(key.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(None))),
        )
    }

    /// Locks `key`'s slot without booting anything: `None` inside means no
    /// session is up for this key.
    pub async fn lock(&self, key: &str) -> OwnedMutexGuard<Option<S>> {
        self.slot(key).await.lock_owned().await
    }

    /// How many `session_id`s currently have a booted session.
    pub async fn active_count(&self) -> usize {
        let slots: Vec<Slot<S>> = self.slots.lock().await.values().cloned().collect();
        let mut count = 0;
        for slot in slots {
            if slot.lock().await.is_some() {
                count += 1;
            }
        }
        count
    }

    /// Hands back `key`'s session locked for the caller's whole call,
    /// booting one first if none exists or the previous one no longer
    /// answers ([`PooledSession::is_alive`]). The `bool` is `true` when this
    /// call booted it.
    pub async fn ensure<E, F, Fut>(
        &self,
        key: &str,
        ctx: &S::Ctx,
        boot: F,
    ) -> Result<(OwnedMutexGuard<Option<S>>, bool), E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<S, E>>,
    {
        let mut guard = self.lock(key).await;
        let alive = match guard.as_ref() {
            Some(session) => session.is_alive().await,
            None => false,
        };
        if alive {
            guard.as_mut().expect("alive implies Some").touch();
            return Ok((guard, false));
        }
        if let Some(stale) = guard.take() {
            log::warn!("{LOG_PREFIX}: session {key:?} no longer answers; rebooting");
            stale.teardown(ctx).await;
        }
        *guard = Some(boot().await?);
        Ok((guard, true))
    }

    /// Tears down every session unused for at least `idle_timeout`, and
    /// forgets every slot that holds no session. Call periodically from a
    /// background task ([`idle_sweep_loop`]), never from inside a call.
    pub async fn sweep_idle(&self, ctx: &S::Ctx, idle_timeout: Duration) {
        let slots: Vec<(String, Slot<S>)> = self
            .slots
            .lock()
            .await
            .iter()
            .map(|(key, slot)| (key.clone(), Arc::clone(slot)))
            .collect();

        for (key, slot) in &slots {
            let mut guard = slot.lock().await;
            let idle = match guard.as_ref() {
                Some(session) => session.last_used().elapsed(),
                None => continue,
            };
            if idle < idle_timeout {
                continue;
            }
            log::info!("{LOG_PREFIX}: tearing down session {key:?} after {idle:?} idle");
            if let Some(session) = guard.take() {
                session.teardown(ctx).await;
            }
        }

        // Forgets a slot only when nothing else holds it right now: a slot
        // mid-`ensure` elsewhere must not be pruned out from under it. An
        // empty slot left behind costs one map entry; the next sweep retries.
        self.slots.lock().await.retain(|_, slot| match slot.try_lock() {
            Ok(guard) => guard.is_some(),
            Err(_) => true,
        });
    }
}

/// Runs forever: every minute, tears down `pool`'s sessions idle for at
/// least `idle_timeout`. Reconnects to Docker each tick, so a daemon that
/// restarted is not a reason for the sweep itself to die.
pub async fn idle_sweep_loop<S>(pool: Arc<SessionPool<S>>, idle_timeout: Duration)
where
    S: PooledSession<Ctx = DockerLifecycle>,
{
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        match DockerLifecycle::connect().await {
            Ok(docker) => pool.sweep_idle(&docker, idle_timeout).await,
            Err(err) => {
                log::warn!("{LOG_PREFIX}: could not reach Docker for the idle-session sweep: {err}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A session with no containers: alive as configured, counting its
    /// teardowns in the shared context.
    struct Fake {
        alive: bool,
        last_used: Instant,
    }

    impl PooledSession for Fake {
        type Ctx = AtomicUsize;

        fn last_used(&self) -> Instant {
            self.last_used
        }

        fn touch(&mut self) {
            self.last_used = Instant::now();
        }

        async fn is_alive(&self) -> bool {
            self.alive
        }

        async fn teardown(self, ctx: &AtomicUsize) {
            ctx.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn fresh(alive: bool) -> Fake {
        Fake {
            alive,
            last_used: Instant::now(),
        }
    }

    #[test]
    fn sanitizing_replaces_anything_unsafe_for_a_docker_name() {
        assert_eq!(sanitize_for_docker_name("run-abc123"), "run-abc123");
        assert_eq!(sanitize_for_docker_name("run/abc:123"), "run_abc_123");
        assert_eq!(sanitize_for_docker_name(""), "");
    }

    #[test]
    fn reach_follows_the_self_container_setting() {
        assert_eq!(Reach::from_self_container(None), Reach::Published);
        assert_eq!(Reach::from_self_container(Some("")), Reach::Published);
        assert_eq!(
            Reach::from_self_container(Some("u2s")),
            Reach::SessionNetwork {
                self_container: "u2s".to_owned()
            }
        );
    }

    #[test]
    fn owner_label_and_owner_labels_agree() {
        let labels = owner_labels("redacto-ubs");
        let (key, value) = labels.iter().next().unwrap();
        assert_eq!(owner_label("redacto-ubs"), format!("{key}={value}"));
    }

    #[tokio::test]
    async fn different_session_ids_get_independent_slots() {
        let pool = SessionPool::<Fake>::new();
        let a = pool.slot("agent-a").await;
        let b = pool.slot("agent-b").await;
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&a, &pool.slot("agent-a").await));
        assert_eq!(pool.active_count().await, 0);
    }

    #[tokio::test]
    async fn a_live_session_is_reused_and_a_dead_one_rebooted() {
        let pool = SessionPool::<Fake>::new();
        let teardowns = AtomicUsize::new(0);

        let (guard, booted) = pool
            .ensure("a", &teardowns, || async { Ok::<_, ()>(fresh(true)) })
            .await
            .unwrap();
        assert!(booted);
        drop(guard);
        let (guard, booted) = pool
            .ensure("a", &teardowns, || async { Ok::<_, ()>(fresh(true)) })
            .await
            .unwrap();
        assert!(!booted, "an alive session must be reused");
        drop(guard);
        assert_eq!(teardowns.load(Ordering::SeqCst), 0);

        pool.lock("a").await.as_mut().unwrap().alive = false;
        let (guard, booted) = pool
            .ensure("a", &teardowns, || async { Ok::<_, ()>(fresh(true)) })
            .await
            .unwrap();
        assert!(booted, "a dead session must be rebooted");
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);
        drop(guard);
        assert_eq!(pool.active_count().await, 1);
    }

    #[tokio::test]
    async fn a_failed_boot_leaves_the_slot_empty() {
        let pool = SessionPool::<Fake>::new();
        let teardowns = AtomicUsize::new(0);
        let result = pool
            .ensure("a", &teardowns, || async { Err::<Fake, _>("boot failed") })
            .await;
        assert!(matches!(result, Err("boot failed")));
        assert!(pool.lock("a").await.is_none());
    }

    #[tokio::test]
    async fn the_idle_sweep_tears_down_only_stale_sessions() {
        let pool = SessionPool::<Fake>::new();
        let teardowns = AtomicUsize::new(0);
        for key in ["stale", "recent"] {
            let (guard, _) = pool
                .ensure(key, &teardowns, || async { Ok::<_, ()>(fresh(true)) })
                .await
                .unwrap();
            drop(guard);
        }
        pool.lock("stale").await.as_mut().unwrap().last_used =
            Instant::now() - Duration::from_secs(120);
        // An empty slot, to be forgotten.
        drop(pool.lock("never-booted").await);

        pool.sweep_idle(&teardowns, Duration::from_secs(60)).await;

        assert_eq!(teardowns.load(Ordering::SeqCst), 1);
        assert!(pool.lock("recent").await.is_some());
        assert_eq!(pool.active_count().await, 1);
        // The stale slot, now empty, and the never-booted one are forgotten.
        assert_eq!(pool.slots.lock().await.len(), 1);
    }
}
