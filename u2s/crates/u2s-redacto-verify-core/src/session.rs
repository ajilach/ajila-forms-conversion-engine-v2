//! The per-agent, throwaway Postgres session `verify_run` imports a dump
//! into. **One session per `session_id`, not a shared instance** --
//! AGENTS.md requires every MCP to support multiple agents running at once
//! without interference, and the same reasoning
//! `u2s-aem-verify-core::session::SessionPool`'s own module doc gives
//! applies here at a smaller scale: a single shared Postgres would
//! serialize every caller through one lock and let one agent's dump appear
//! in another's row counts mid-import.
//!
//! **Wiped, not reused, on every import.** [`encode`](u2s_mapper_redacto::encode)
//! mints every id deterministically from `(document_id, ...)` (see that
//! crate's own module doc on why), so re-importing the *same* document a
//! second time into a session's already-populated database would collide
//! on a primary key that never changes. [`SessionPool::import`] therefore
//! drops and recreates the `app_redacto` schema at the start of every call
//! -- cheap, since this database holds nothing durable, and it is what
//! makes a session safe to reuse across repeated `verify_run` calls on an
//! edited-and-re-encoded document instead of only ever working once.
//!
//! **Interior mutability, documented** (mirroring
//! `u2s-aem-verify-core::session`'s own note): each session's container id
//! lives behind its own `Arc<Mutex<Option<SessionState>>>`; [`SessionPool::ensure`]
//! hands the caller an owned guard for the call's whole duration, since only
//! one call may use a *given* session's database at a time. The pool's own
//! outer map is behind a second, short-lived `Mutex` guarding only
//! insertion and lookup.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedMutexGuard};

use u2s_verify_core::docker::{ContainerSpec, DockerError, DockerLifecycle};

/// The `session_id` a caller that omits one gets.
pub const DEFAULT_SESSION_KEY: &str = "default";

const BASELINE_SCHEMA: &str = include_str!("../schema/app_redacto_baseline.sql");

/// How long an idle session's container is kept before [`SessionPool::sweep`]
/// tears it down. Generous: the container is cheap to keep and every import
/// wipes it clean regardless, matching
/// `u2s-aem-verify-core::session`'s own idle-timeout safety net in spirit.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(1800);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Docker(#[from] DockerError),
    #[error("psql exited {exit_code}: {output}")]
    Psql { exit_code: i64, output: String },
    #[error("timed out waiting for postgres to become ready in container {container_id}")]
    NotReady { container_id: String },
    #[error("the dump is not valid UTF-8: {0}")]
    NotUtf8(#[from] std::str::Utf8Error),
}

pub struct SessionState {
    pub container_id: String,
    started_at: Instant,
    last_used: Instant,
}

impl SessionState {
    pub fn age(&self) -> Duration {
        self.started_at.elapsed()
    }
}

/// Row counts across the six tables after an import -- what `verify_run`
/// reports alongside whether the import itself succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportReport {
    pub table_counts: BTreeMap<String, i64>,
}

impl ImportReport {
    pub fn total_rows(&self) -> i64 {
        self.table_counts.values().sum()
    }
}

pub struct SessionPool {
    sessions: Mutex<HashMap<String, Arc<Mutex<Option<SessionState>>>>>,
    postgres_image: String,
}

impl SessionPool {
    pub fn new(postgres_image: impl Into<String>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            postgres_image: postgres_image.into(),
        }
    }

    /// Boots (or reuses) `session_id`'s own container and returns an owned
    /// guard serializing every caller of *this* session; a different
    /// `session_id` never blocks on it.
    pub async fn ensure(
        &self,
        session_id: &str,
        docker: &DockerLifecycle,
    ) -> Result<OwnedMutexGuard<Option<SessionState>>, SessionError> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut guard = entry.lock_owned().await;
        match guard.as_mut() {
            Some(state) => state.last_used = Instant::now(),
            None => {
                let container_id = boot(docker, &self.postgres_image, session_id).await?;
                *guard = Some(SessionState {
                    container_id,
                    started_at: Instant::now(),
                    last_used: Instant::now(),
                });
            }
        }
        Ok(guard)
    }

    /// Tears down and forgets every session whose container has sat idle
    /// past [`IDLE_TIMEOUT`]. Safe to call before every [`Self::ensure`] --
    /// a session currently held by another caller is simply skipped (its
    /// lock is busy), never torn down out from under it.
    pub async fn sweep(&self, docker: &DockerLifecycle) {
        let candidates: Vec<(String, Arc<Mutex<Option<SessionState>>>)> = {
            let sessions = self.sessions.lock().await;
            sessions.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        let mut to_forget = Vec::new();
        for (id, slot) in candidates {
            let Ok(mut guard) = slot.try_lock() else {
                continue;
            };
            let idle = guard.as_ref().map(|s| s.last_used.elapsed() > IDLE_TIMEOUT).unwrap_or(false);
            if idle {
                if let Some(state) = guard.take() {
                    let _ = docker.teardown(&state.container_id).await;
                }
                to_forget.push(id);
            }
        }
        if !to_forget.is_empty() {
            let mut sessions = self.sessions.lock().await;
            for id in to_forget {
                sessions.remove(&id);
            }
        }
    }

    /// Tears down one session unconditionally, regardless of idle time --
    /// used when a caller is done with a session for good, not just between
    /// calls.
    pub async fn teardown(&self, session_id: &str, docker: &DockerLifecycle) {
        let slot = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(session_id)
        };
        if let Some(slot) = slot
            && let Some(state) = slot.lock().await.take()
        {
            let _ = docker.teardown(&state.container_id).await;
        }
    }

    /// Whether `session_id` has a currently-booted container, and for how
    /// long -- `verify_status`'s own read-only check. Never boots one:
    /// unlike [`Self::ensure`], "no session yet" is a legitimate answer
    /// here, not something to fix by starting Docker.
    pub async fn peek(&self, session_id: &str) -> Option<Duration> {
        let slot = self.sessions.lock().await.get(session_id)?.clone();
        let guard = slot.lock().await;
        guard.as_ref().map(SessionState::age)
    }

    /// How many sessions currently exist across every `session_id`,
    /// booted or not yet -- what `verify_status` reports as "sessions
    /// active across every session_id".
    pub async fn active_count(&self) -> usize {
        self.sessions.lock().await.len()
    }
}

async fn boot(docker: &DockerLifecycle, image: &str, session_id: &str) -> Result<String, SessionError> {
    docker.ensure_image(image, "").await?;

    let spec = ContainerSpec {
        name: format!("u2s-redacto-verify-{}", sanitize(session_id)),
        image: image.to_owned(),
        platform: String::new(),
        network: "bridge".to_owned(),
        env: vec![
            ("POSTGRES_PASSWORD".to_owned(), "password".to_owned()),
            ("POSTGRES_DB".to_owned(), "redacto".to_owned()),
        ],
        labels: HashMap::from([("u2s.redacto-verify-session".to_owned(), session_id.to_owned())]),
        publish_ports: vec![],
        binds: vec![],
        extra_hosts: vec![],
        memory_bytes: None,
    };
    let running = docker.run(&spec).await?;
    wait_ready(docker, &running.id).await?;
    Ok(running.id)
}

/// Docker container names allow only `[a-zA-Z0-9_.-]`; a `session_id` in
/// real use is a UUID-shaped run id, but this normalises defensively rather
/// than trusting the caller's own format.
fn sanitize(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Confirmed live: `pg_isready -d redacto` reports success against the
/// official `postgres` image's own short-lived *first-boot* server, before
/// its init scripts have finished creating the `POSTGRES_DB`-named
/// database -- a later `psql -d redacto` in that same window then fails
/// with "database \"redacto\" does not exist", not a connection error.
/// `pg_isready` only ever checks that *a* postmaster answers, never that a
/// specific database exists, so this polls with a real `psql` connection
/// to the actual database instead and treats "does not exist" as "not
/// ready yet", the same as a refused connection.
async fn wait_ready(docker: &DockerLifecycle, container_id: &str) -> Result<(), SessionError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ready = docker
            .exec(
                container_id,
                vec![
                    "psql".to_owned(),
                    "-U".to_owned(),
                    "postgres".to_owned(),
                    "-d".to_owned(),
                    "redacto".to_owned(),
                    "-c".to_owned(),
                    "SELECT 1".to_owned(),
                ],
                None,
            )
            .await
            .map(|r| r.succeeded())
            .unwrap_or(false);
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SessionError::NotReady {
                container_id: container_id.to_owned(),
            });
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn run_psql(docker: &DockerLifecycle, container_id: &str, sql: &str) -> Result<String, SessionError> {
    let result = docker
        .exec(
            container_id,
            vec![
                "psql".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-d".to_owned(),
                "redacto".to_owned(),
                "-v".to_owned(),
                "ON_ERROR_STOP=1".to_owned(),
            ],
            Some(sql.as_bytes()),
        )
        .await?;
    if !result.succeeded() {
        return Err(SessionError::Psql {
            exit_code: result.exit_code,
            output: result.output,
        });
    }
    Ok(result.output)
}

const ROW_COUNT_QUERY: &str = "\
SELECT 'assets', count(*) FROM app_redacto.assets
UNION ALL SELECT 'asset_version', count(*) FROM app_redacto.asset_version
UNION ALL SELECT 'documents', count(*) FROM app_redacto.documents
UNION ALL SELECT 'document_version', count(*) FROM app_redacto.document_version
UNION ALL SELECT 'ownerships', count(*) FROM app_redacto.ownerships
UNION ALL SELECT 'relations', count(*) FROM app_redacto.relations;\n";

/// Wipes `container_id`'s own `app_redacto` schema, reapplies the baseline
/// DDL, imports `dump_bytes`, and reports row counts -- proof the dump is
/// not just parseable JSON-and-SQL-shaped text but actually importable
/// against the platform's own real schema, foreign keys and all.
pub async fn import(
    docker: &DockerLifecycle,
    container_id: &str,
    dump_bytes: &[u8],
) -> Result<ImportReport, SessionError> {
    run_psql(docker, container_id, "DROP SCHEMA IF EXISTS app_redacto CASCADE;\nCREATE SCHEMA app_redacto;\n").await?;
    run_psql(
        docker,
        container_id,
        &format!("SET search_path TO app_redacto;\n{BASELINE_SCHEMA}"),
    )
    .await?;

    let dump_text = std::str::from_utf8(dump_bytes)?;
    run_psql(docker, container_id, dump_text).await?;

    // `-t -A -F '|'`: unaligned, tuples-only, pipe-separated -- a
    // machine-readable row per `UNION ALL` branch, not the aligned table
    // `psql` prints by default (fragile to parse across locale/width).
    let counted = docker
        .exec(
            container_id,
            vec![
                "psql".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-d".to_owned(),
                "redacto".to_owned(),
                "-t".to_owned(),
                "-A".to_owned(),
                "-F".to_owned(),
                "|".to_owned(),
                "-c".to_owned(),
                ROW_COUNT_QUERY.to_owned(),
            ],
            None,
        )
        .await?;
    if !counted.succeeded() {
        return Err(SessionError::Psql {
            exit_code: counted.exit_code,
            output: counted.output,
        });
    }

    let mut table_counts = BTreeMap::new();
    for line in counted.output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((name, count)) = line.split_once('|')
            && let Ok(n) = count.trim().parse::<i64>()
        {
            table_counts.insert(name.trim().to_owned(), n);
        }
    }
    Ok(ImportReport { table_counts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_unsafe_characters() {
        assert_eq!(sanitize("run/123:abc"), "run_123_abc");
        assert_eq!(sanitize("abc-123_XYZ"), "abc-123_XYZ");
    }

    #[test]
    fn import_report_sums_every_table() {
        let report = ImportReport {
            table_counts: BTreeMap::from([
                ("assets".to_owned(), 3),
                ("relations".to_owned(), 3),
            ]),
        };
        assert_eq!(report.total_rows(), 6);
    }
}
