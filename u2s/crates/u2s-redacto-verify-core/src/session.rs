//! The Redacto platform one `session_id` verifies against, booted by this
//! verifier itself: Postgres, the platform's bootstrap SQL and Flyway
//! migrations, `core` and `rendering`, on a network of their own.
//!
//! **One platform per `session_id`**, held in a
//! `u2s_verify_core::session::SessionPool` like the AEM verifiers' sessions.
//! Two agents therefore never see each other's imports, even of the same
//! document id; the cost is two JVMs (`core`, `rendering`, about 1.5 GB
//! together) per active session, and a boot on a session's first
//! `verify_run`. The database is not persisted: a session's platform
//! holds only what that session imported, and goes away with it.
//!
//! Boot order mirrors the dependency chain:
//! 1. Postgres, ready once it accepts TCP connections. The image's own
//!    first-start initialisation runs a temporary server on the socket
//!    only, so a TCP probe never mistakes it for the final one.
//! 2. [`BOOTSTRAP_SQL`], through `psql` inside the Postgres container.
//! 3. The migration image, run to completion.
//! 4. `core`, ready once its own health action answers.
//! 5. `rendering`, ready once its integration servlet is registered.
//!
//! Readiness is probed from inside each container (`pg_isready`, `curl`),
//! so `core` never needs a published port.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::OwnedMutexGuard;
use u2s_verify_core::docker::{ContainerSpec, DockerLifecycle, ExecOutput, RunningContainer};
use u2s_verify_core::session::{
    PooledSession, Reach, new_boot_id, owner_labels, remove_session_network,
};
use u2s_verify_core::types::{ErrorKind, VerifyError};

use crate::platform::{DB_NAME, DB_USER};
use crate::profile::RenderProfile;

/// The role, schema, and grants Flyway cannot create itself, vendored from
/// `ajila-redacto-platform` (see the file's own header).
pub const BOOTSTRAP_SQL: &str = include_str!("../sql/bootstrap.sql");

/// The superuser password of a session's own Postgres, and the password
/// [`BOOTSTRAP_SQL`] gives the `app_redacto` role. Neither leaves the
/// session's network unless this process runs on the host, where Postgres
/// is still never published.
const DB_PASSWORD: &str = "password";
const APP_ROLE: &str = "app_redacto";
const APP_PASSWORD: &str = "password";
/// The port `core` and `rendering` listen on inside their containers.
const PLATFORM_PORT: u16 = 8080;
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// The pool of per-`session_id` platforms.
pub type SessionPool = u2s_verify_core::session::SessionPool<RedactoSession>;

/// One booted platform.
pub struct RedactoSession {
    pub network: String,
    pub postgres: RunningContainer,
    pub core: RunningContainer,
    pub rendering: RunningContainer,
    pub reach: Reach,
    /// The `rendering` service, as this process reaches it.
    pub rendering_base_url: String,
    pub started_at: Instant,
    last_used: Instant,
}

impl PooledSession for RedactoSession {
    type Ctx = DockerLifecycle;

    fn last_used(&self) -> Instant {
        self.last_used
    }

    fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    async fn is_alive(&self) -> bool {
        u2s_verify_core::http::is_reachable(&self.rendering_base_url, Duration::from_secs(3)).await
    }

    async fn teardown(self, docker: &DockerLifecycle) {
        let ids = [&self.rendering.id, &self.core.id, &self.postgres.id];
        teardown_parts(docker, ids.into_iter().cloned(), &self.network, &self.reach).await;
    }
}

/// The names of one boot's network and containers. The services address
/// each other by container name, so every name must be one DNS label
/// (`u2s_verify_core::session::DNS_LABEL_MAX`); `boot_id` comes from
/// [`new_boot_id`]. Pure.
/// Unit-tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub network: String,
    pub postgres: String,
    pub migration: String,
    pub core: String,
    pub rendering: String,
}

impl Names {
    pub fn for_boot(boot_id: &str) -> Self {
        let name = |part: &str| format!("u2s-verify-redacto-{part}-{boot_id}");
        Self {
            network: format!("u2s-verify-redacto-{boot_id}"),
            postgres: name("postgres"),
            migration: name("migration"),
            core: name("core"),
            rendering: name("rendering"),
        }
    }

    fn jdbc_url(&self) -> String {
        format!("jdbc:postgresql://{}:5432/{DB_NAME}", self.postgres)
    }
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn spec(
    profile: &RenderProfile,
    name: &str,
    image: &str,
    network: &str,
    labels: &HashMap<String, String>,
    env: Vec<(String, String)>,
    publish_ports: Vec<u16>,
) -> ContainerSpec {
    ContainerSpec {
        name: name.to_owned(),
        image: image.to_owned(),
        platform: profile.platform.clone(),
        network: network.to_owned(),
        env,
        labels: labels.clone(),
        publish_ports,
        binds: Vec::new(),
        extra_hosts: Vec::new(),
        memory_bytes: None,
    }
}

/// The four container specs of one boot, in boot order: postgres,
/// migration, core, rendering. Only `rendering` is ever published, and
/// only when this process reaches the session through the host. Pure.
/// Unit-tested.
pub fn container_specs(
    profile: &RenderProfile,
    names: &Names,
    reach: &Reach,
    labels: &HashMap<String, String>,
) -> [ContainerSpec; 4] {
    let jdbc = names.jdbc_url();
    let core_url = format!("http://{}:{PLATFORM_PORT}", names.core);
    let images = &profile.images;
    let net = &names.network;
    [
        spec(
            profile,
            &names.postgres,
            &images.postgres,
            net,
            labels,
            env(&[
                ("POSTGRES_DB", DB_NAME),
                ("POSTGRES_USER", DB_USER),
                ("POSTGRES_PASSWORD", DB_PASSWORD),
            ]),
            Vec::new(),
        ),
        spec(
            profile,
            &names.migration,
            &images.migration,
            net,
            labels,
            env(&[
                ("FLYWAY_URL", &jdbc),
                ("FLYWAY_USER", APP_ROLE),
                ("FLYWAY_PASSWORD", APP_PASSWORD),
                ("FLYWAY_SCHEMAS", APP_ROLE),
            ]),
            Vec::new(),
        ),
        spec(
            profile,
            &names.core,
            &images.core,
            net,
            labels,
            env(&[
                ("REDACTO_DB_URL", &jdbc),
                ("REDACTO_DB_USERNAME", APP_ROLE),
                ("REDACTO_DB_PASSWORD", APP_PASSWORD),
            ]),
            Vec::new(),
        ),
        spec(
            profile,
            &names.rendering,
            &images.rendering,
            net,
            labels,
            env(&[("REDACTO_CORE_BASE_URL", &core_url)]),
            if reach.publishes() {
                vec![PLATFORM_PORT]
            } else {
                Vec::new()
            },
        ),
    ]
}

/// The `rendering` service as this process reaches it. Pure. Unit-tested.
pub fn rendering_url(
    reach: &Reach,
    rendering_name: &str,
    published_port: Option<u16>,
) -> Result<String, String> {
    match reach {
        Reach::Published => published_port
            .map(|port| format!("http://127.0.0.1:{port}"))
            .ok_or_else(|| format!("the rendering container never published port {PLATFORM_PORT}")),
        Reach::SessionNetwork { .. } => Ok(format!("http://{rendering_name}:{PLATFORM_PORT}")),
    }
}

/// The readiness probe run inside the `rendering` container: `curl`
/// printing the integration servlet's status code. No shell, so the
/// credentials are never quoted into a script. Pure. Unit-tested.
pub fn rendering_probe(basic_auth: &(String, String)) -> Vec<String> {
    let (user, password) = basic_auth;
    [
        "curl",
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-u",
        &format!("{user}:{password}"),
        &format!("http://localhost:{PLATFORM_PORT}/bin/redacto/rendering/integration"),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Whether [`rendering_probe`]'s output means the servlet is registered
/// and serving: it only answers POST, so a ready service answers a GET with
/// 405. While Sling is still starting its bundles it answers 503, before
/// the servlet is registered 404, and before it listens at all `000`; so
/// any status below 500 except 404 means ready. Pure. Unit-tested.
pub fn rendering_is_ready(probe: &ExecOutput) -> bool {
    probe.succeeded()
        && probe
            .output
            .trim()
            .parse::<u16>()
            .is_ok_and(|code| (100..500).contains(&code) && code != 404)
}

/// `pool`'s platform for `key`, booted if none exists or the previous one no
/// longer answers, locked for the caller's whole call.
pub async fn ensure(
    pool: &SessionPool,
    key: &str,
    docker: &DockerLifecycle,
    profile: &RenderProfile,
) -> Result<OwnedMutexGuard<Option<RedactoSession>>, VerifyError> {
    let (guard, _booted) = pool.ensure(key, docker, || boot(docker, profile, key)).await?;
    Ok(guard)
}

/// Boots one platform. Whatever was already started when a step fails is
/// torn down before the error is returned.
pub async fn boot(
    docker: &DockerLifecycle,
    profile: &RenderProfile,
    session_id: &str,
) -> Result<RedactoSession, VerifyError> {
    // The daemon must already hold the private images: this process has no
    // registry credentials (compose pulls them with the host's login).
    for image in profile.images.all() {
        docker.ensure_image(image, &profile.platform).await.map_err(|err| {
            VerifyError::new(
                ErrorKind::ImageMissing,
                format!("{err} (`docker compose pull` puts the platform images on this host)"),
            )
        })?;
    }

    let names = Names::for_boot(&new_boot_id());
    let reach = Reach::from_self_container(profile.self_container.as_deref());
    let mut labels = owner_labels(profile.name);
    docker
        .ensure_network(&names.network, &labels)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    labels.insert("u2s.verify".to_owned(), "1".to_owned());
    labels.insert("u2s.verify.session_id".to_owned(), session_id.to_owned());

    let mut started = Vec::new();
    let result = boot_on_network(docker, profile, &names, &reach, &labels, &mut started).await;
    if result.is_err() {
        teardown_parts(docker, started.into_iter().rev(), &names.network, &reach).await;
    }
    result
}

/// The steps of [`boot`] after its network exists. Every container it
/// starts is pushed to `started` first, so [`boot`] can clean up after a
/// failure at any step.
async fn boot_on_network(
    docker: &DockerLifecycle,
    profile: &RenderProfile,
    names: &Names,
    reach: &Reach,
    labels: &HashMap<String, String>,
    started: &mut Vec<String>,
) -> Result<RedactoSession, VerifyError> {
    let not_ready = |step: &str, err: &dyn std::fmt::Display| {
        VerifyError::new(ErrorKind::RedactoNotReady, format!("{step}: {err}"))
    };
    if let Reach::SessionNetwork { self_container } = reach {
        docker
            .connect_network(&names.network, self_container)
            .await
            .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;
    }
    let [postgres_spec, migration_spec, core_spec, rendering_spec] =
        container_specs(profile, names, reach, labels);
    let timeout = profile.boot_timeout;

    let postgres = docker
        .run(&postgres_spec)
        .await
        .map_err(|err| not_ready("starting postgres", &err))?;
    started.push(postgres.id.clone());
    docker
        .wait_for_exec(
            &postgres.id,
            &["pg_isready", "-h", "127.0.0.1", "-U", DB_USER, "-d", DB_NAME],
            ExecOutput::succeeded,
            timeout,
            POLL_INTERVAL,
        )
        .await
        .map_err(|err| not_ready("waiting for postgres", &err))?;

    let bootstrap = docker
        .exec(
            &postgres.id,
            ["psql", "-v", "ON_ERROR_STOP=1", "-U", DB_USER, "-d", DB_NAME]
                .map(str::to_owned)
                .to_vec(),
            Some(BOOTSTRAP_SQL.as_bytes()),
        )
        .await
        .map_err(|err| not_ready("running the bootstrap SQL", &err))?;
    if !bootstrap.succeeded() {
        return Err(not_ready("the bootstrap SQL failed", &bootstrap.output));
    }

    let migration = docker
        .run_to_completion(&migration_spec, timeout)
        .await
        .map_err(|err| not_ready("running the migration", &err))?;
    if !migration.succeeded() {
        let message = format!("exit {}: {}", migration.exit_code, migration.output);
        return Err(not_ready("the migration failed", &message));
    }

    let core = docker
        .run(&core_spec)
        .await
        .map_err(|err| not_ready("starting core", &err))?;
    started.push(core.id.clone());
    let health = format!("http://localhost:{PLATFORM_PORT}/bin/redacto/core/integration?action=health");
    docker
        .wait_for_exec(
            &core.id,
            &["curl", "-fsS", &health],
            ExecOutput::succeeded,
            timeout,
            POLL_INTERVAL,
        )
        .await
        .map_err(|err| not_ready("waiting for core", &err))?;

    let rendering = docker
        .run(&rendering_spec)
        .await
        .map_err(|err| not_ready("starting rendering", &err))?;
    started.push(rendering.id.clone());
    let probe = rendering_probe(&profile.basic_auth);
    let probe: Vec<&str> = probe.iter().map(String::as_str).collect();
    docker
        .wait_for_exec(&rendering.id, &probe, rendering_is_ready, timeout, POLL_INTERVAL)
        .await
        .map_err(|err| not_ready("waiting for rendering", &err))?;

    let rendering_base_url = rendering_url(
        reach,
        &names.rendering,
        rendering.published_port(PLATFORM_PORT),
    )
    .map_err(|message| not_ready("addressing rendering", &message))?;

    let now = Instant::now();
    Ok(RedactoSession {
        network: names.network.clone(),
        postgres,
        core,
        rendering,
        reach: reach.clone(),
        rendering_base_url,
        started_at: now,
        last_used: now,
    })
}

async fn teardown_parts(
    docker: &DockerLifecycle,
    container_ids: impl Iterator<Item = String>,
    network: &str,
    reach: &Reach,
) {
    for id in container_ids {
        if let Err(err) = docker.teardown(&id).await {
            log::warn!("u2s-redacto-verify-core: could not tear down container {id}: {err}");
        }
    }
    remove_session_network(docker, network, reach).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use u2s_verify_core::session::DNS_LABEL_MAX;

    fn profile() -> RenderProfile {
        RenderProfile::from_reader("redacto-ubs", "P", "pdf-ua", |key| {
            Some(
                match key {
                    "P_MIGRATION_IMAGE" => "r/migration:1",
                    "P_CORE_IMAGE" => "r/core:1",
                    "P_RENDERING_IMAGE" => "r/rendering:1",
                    _ => return None,
                }
                .to_owned(),
            )
        })
        .unwrap()
    }

    fn value<'a>(spec: &'a ContainerSpec, key: &str) -> &'a str {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("{} has no {key}", spec.name))
    }

    #[test]
    fn names_share_the_boot_id_never_collide_and_fit_one_dns_label() {
        let names = Names::for_boot(&new_boot_id());
        assert!(names.postgres.ends_with(&names.network["u2s-verify-redacto-".len()..]));
        let names = Names::for_boot("0123456789ab");
        assert_eq!(names.network, "u2s-verify-redacto-0123456789ab");
        assert_eq!(names.postgres, "u2s-verify-redacto-postgres-0123456789ab");
        let all = [&names.network, &names.postgres, &names.migration, &names.core, &names.rendering];
        let distinct: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(distinct.len(), all.len());
        for name in all {
            assert!(name.len() <= DNS_LABEL_MAX, "{name} is longer than one DNS label");
        }
    }

    #[test]
    fn the_services_are_wired_to_each_other_by_container_name() {
        let names = Names::for_boot("b");
        let labels = owner_labels("redacto-ubs");
        let [postgres, migration, core, rendering] =
            container_specs(&profile(), &names, &Reach::Published, &labels);

        assert_eq!(postgres.image, "postgres:16-alpine");
        assert_eq!(value(&postgres, "POSTGRES_DB"), DB_NAME);
        assert_eq!(value(&postgres, "POSTGRES_USER"), DB_USER);

        let jdbc = format!("jdbc:postgresql://{}:5432/redacto", names.postgres);
        assert_eq!(migration.image, "r/migration:1");
        assert_eq!(value(&migration, "FLYWAY_URL"), jdbc);
        assert_eq!(value(&migration, "FLYWAY_USER"), "app_redacto");
        assert_eq!(value(&migration, "FLYWAY_SCHEMAS"), "app_redacto");
        assert_eq!(value(&core, "REDACTO_DB_URL"), jdbc);
        assert_eq!(value(&core, "REDACTO_DB_USERNAME"), "app_redacto");
        assert_eq!(
            value(&rendering, "REDACTO_CORE_BASE_URL"),
            format!("http://{}:8080", names.core)
        );

        for spec in [&postgres, &migration, &core, &rendering] {
            assert_eq!(spec.network, names.network);
            assert_eq!(spec.labels, labels);
        }
        // The bootstrap SQL creates exactly the role the services log in as.
        assert!(BOOTSTRAP_SQL.contains("CREATE ROLE app_redacto LOGIN PASSWORD 'password'"));
    }

    #[test]
    fn only_rendering_is_published_and_only_from_the_host() {
        let names = Names::for_boot("b");
        let labels = HashMap::new();
        let published = container_specs(&profile(), &names, &Reach::Published, &labels);
        let ports: Vec<_> = published.iter().map(|s| s.publish_ports.clone()).collect();
        assert_eq!(ports, vec![vec![], vec![], vec![], vec![8080]]);

        let in_container = Reach::SessionNetwork {
            self_container: "u2s-server".to_owned(),
        };
        let networked = container_specs(&profile(), &names, &in_container, &labels);
        assert!(networked.iter().all(|s| s.publish_ports.is_empty()));
    }

    #[test]
    fn the_rendering_url_follows_the_reach() {
        assert_eq!(
            rendering_url(&Reach::Published, "r", Some(49001)).unwrap(),
            "http://127.0.0.1:49001"
        );
        assert!(rendering_url(&Reach::Published, "r", None).is_err());
        let reach = Reach::SessionNetwork {
            self_container: "u2s".to_owned(),
        };
        assert_eq!(rendering_url(&reach, "u2s-verify-redacto-rendering-b", None).unwrap(),
            "http://u2s-verify-redacto-rendering-b:8080");
    }

    #[test]
    fn the_rendering_probe_carries_the_credentials_unquoted() {
        let probe = rendering_probe(&("admin".to_owned(), "it's".to_owned()));
        assert_eq!(probe[0], "curl");
        assert!(probe.contains(&"admin:it's".to_owned()), "{probe:?}");
        assert!(probe.contains(&"%{http_code}".to_owned()), "{probe:?}");
    }

    #[test]
    fn rendering_is_ready_once_the_servlet_answers_below_500_but_not_404() {
        let probe = |exit_code, output: &str| ExecOutput {
            exit_code,
            output: output.to_owned(),
        };
        assert!(rendering_is_ready(&probe(0, "405")));
        assert!(rendering_is_ready(&probe(0, "401")));
        assert!(!rendering_is_ready(&probe(0, "404")));
        assert!(!rendering_is_ready(&probe(0, "503")), "Sling still starting");
        assert!(!rendering_is_ready(&probe(0, "000")));
        assert!(!rendering_is_ready(&probe(7, "000")));
        assert!(!rendering_is_ready(&probe(0, "")));
    }
}
