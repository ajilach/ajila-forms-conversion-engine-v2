//! Local SQLite store for application settings and edit history.
//!
//! The data lives in `<config_dir>/blueprint/history.db`.
//!
//! Schema:
//! - `settings(key TEXT PRIMARY KEY, value TEXT)` — key/value store (the whole
//!   `AppSettings` is stored as JSON under the `app` key).
//! - `documents(doc_hash TEXT PRIMARY KEY, label TEXT, created_at TEXT)` — one
//!   row per uploaded document set, keyed by an order-independent content hash.
//! - `sessions(session_id TEXT PRIMARY KEY, doc_hash TEXT, profile TEXT,
//!   label TEXT, created_at TEXT)` — one row per pipeline run / continued edit.
//! - `edits(id INTEGER PRIMARY KEY, session_id TEXT, seq INTEGER, created_at
//!   TEXT, action_label TEXT, structure_json TEXT)` — one snapshot per edit.
//! - `conversations(session_id TEXT, stage TEXT, seq INTEGER, created_at TEXT,
//!   message_json TEXT, PRIMARY KEY(session_id, stage, seq))` — one row per
//!   LLM message, opaque JSON this module never deserializes (that is
//!   `pipeline::memory`'s job, the one place that knows what a rig `Message`
//!   is). Append-only: nothing here ever deletes a row except `clear`.

/// Metadata about a previous editing session, for the "continue editing" list.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub session_id: String,
    pub label: String,
    pub profile: Option<String>,
    /// RFC3339 timestamp of when the session was created.
    pub created_at: String,
    /// Number of edits recorded in the session (including the initial snapshot).
    pub edit_count: usize,
}

/// A single entry in a session's edit history, for the history sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct EditInfo {
    pub seq: usize,
    /// RFC3339 timestamp of when the edit was recorded.
    pub created_at: String,
    pub action_label: String,
}

/// Format an RFC3339 timestamp into a compact `YYYY-MM-DD HH:MM` form.
pub fn format_timestamp(ts: &str) -> String {
    if ts.len() >= 16 {
        ts[..16].replace('T', " ")
    } else {
        ts.to_string()
    }
}

mod imp {
    use super::{EditInfo, SessionInfo};
    use rusqlite::{Connection, OptionalExtension};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    /// How long a writer waits for another writer to commit before giving up.
    ///
    /// Conversions run in parallel and every tool call snapshots its tree, so
    /// write contention is normal rather than exceptional. Without a timeout
    /// SQLite fails the *instant* it finds the database locked, which is how a
    /// snapshot used to go missing without anyone noticing.
    const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

    /// The schema revision this build expects. Bumped when a migration is added.
    const SCHEMA_VERSION: i64 = 4;

    /// Redirects the store to a scratch file. Tests only — see
    /// [`set_db_path_for_test`].
    static DB_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

    /// Databases this process has already put through schema + migration, so the
    /// DDL runs once rather than on every connection.
    static INITIALIZED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    fn db_path() -> PathBuf {
        if let Some(path) = DB_PATH_OVERRIDE.get() {
            return path.clone();
        }
        let base = dirs::config_dir().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        });
        base.join("blueprint").join("history.db")
    }

    /// The directory the store lives in, for other per-user state that has to
    /// be shared between this program's processes (the verifier locks).
    pub fn state_dir() -> PathBuf {
        db_path()
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Point the store at `path` instead of the user's config directory.
    ///
    /// Only ever takes effect once, and only before the first [`open`] — the
    /// concurrency tests need a real file on disk (an in-memory database is
    /// private to its connection, so it cannot show contention at all) without
    /// writing over the developer's own history.
    ///
    /// Also reachable from another crate's own tests via the `test-utils`
    /// feature (`pipeline`'s `SqliteConversationMemory` tests need it, since
    /// `#[cfg(test)]` alone only compiles this in for *this* crate's tests).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_db_path_for_test(path: PathBuf) {
        let _ = DB_PATH_OVERRIDE.set(path);
    }

    /// Point the store at a scratch file, for a test that needs a real one: an
    /// in-memory database is private to its own connection and cannot show
    /// contention at all.
    ///
    /// `set_db_path_for_test` is a one-shot `OnceLock`: whichever test in this
    /// binary calls this first wins, and every other caller shares that same
    /// path. That is fine: every caller only ever needs *a* scratch file, never
    /// a specific one, so sharing is safe as long as tests that write to the
    /// same table use keys (session ids) that cannot collide with each other's.
    #[cfg(test)]
    pub(crate) fn claim_scratch_db_for_test() {
        let dir = std::env::temp_dir().join(format!("blueprint-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        set_db_path_for_test(dir.join("history.db"));
    }

    /// The suffix of the sibling session a run's document is recorded under.
    pub const DOCUMENT_SUFFIX: &str = "#document";

    /// Report a database error instead of discarding it.
    ///
    /// The store is best-effort by design: a failed settings write must not take
    /// the conversion down with it. But "best effort" used to mean "silent", and
    /// a lost edit-history snapshot is invisible until someone tries to reopen
    /// the session. The workspace has no logging framework, so this goes to
    /// stderr — which the CLI shows directly, and where the desktop app's own
    /// warning banner (see `insert_edit`'s callers) is the user-facing half.
    fn warn_db<T>(result: rusqlite::Result<T>, what: &str) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                eprintln!("history store: {what} failed: {e}");
                None
            }
        }
    }

    /// Open the database connection, creating the file and schema if needed.
    ///
    /// Public so the reference store ([`crate::references`]) can share the same
    /// single `history.db` connection rather than opening a second database.
    ///
    /// Every caller gets its own connection. That is deliberate: in WAL mode any
    /// number of readers run concurrently with one writer, so a reference import
    /// no longer blocks a running conversion's snapshots. Serializing everything
    /// behind one shared connection would have thrown that away.
    pub fn open() -> rusqlite::Result<Connection> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&path)?;

        // Per-connection, so they belong here rather than in the one-time init.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // The standard pairing with WAL: a power cut can cost the most recent
        // commits but cannot corrupt the file. An edit-history snapshot is worth
        // that trade; a synchronous fsync per tool call is not.
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        initialize_once(&path, &conn)?;
        Ok(conn)
    }

    /// Run the schema and migrations for `path` the first time it is opened.
    fn initialize_once(path: &Path, conn: &Connection) -> rusqlite::Result<()> {
        let mut guard = INITIALIZED.lock().unwrap_or_else(|e| e.into_inner());
        let seen = guard.get_or_insert_with(HashSet::new);
        if seen.contains(path) {
            return Ok(());
        }

        // Persistent property of the file, so it only has to be set once — but
        // it reports the resulting mode as a row, which `execute` rejects.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        ensure_schema(conn)?;
        migrate(conn)?;

        seen.insert(path.to_path_buf());
        Ok(())
    }

    /// Bring an existing database up to [`SCHEMA_VERSION`].
    ///
    /// Split from [`ensure_schema`] because `CREATE TABLE IF NOT EXISTS` cannot
    /// alter a table that already exists, and because the de-duplication below
    /// is a full scan that should run once rather than on every connection.
    fn migrate(conn: &Connection) -> rusqlite::Result<()> {
        let version: i64 = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        if version < 1 {
            // Until the unique index below existed, two concurrent writers could
            // both read the same `MAX(seq) + 1` and both insert it. A duplicate
            // is silent corruption: `snapshot_at` takes whichever row comes
            // first, so one run's tree could be restored into another's session.
            // Keep the earliest row for each `(session_id, seq)` and drop the
            // rest, or the index cannot be created.
            conn.execute(
                "DELETE FROM edits WHERE id NOT IN (
                     SELECT MIN(id) FROM edits GROUP BY session_id, seq
                 )",
                [],
            )?;
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_edits_session_seq
                     ON edits(session_id, seq);
                 -- Exactly the columns the unique index now covers.
                 DROP INDEX IF EXISTS idx_edits_session;",
            )?;
        }

        if version < 2 {
            // What a run authors (an AEM form or a Redacto document) is a
            // property of the session, not of the view onto it: resuming with
            // the wrong target gives a run that cannot see its own prior work.
            // `profile` is already a column for the same reason.
            //
            // `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that
            // already exists, and there is no `ADD COLUMN IF NOT EXISTS`, so an
            // error here means the column is already present.
            let _ = conn.execute("ALTER TABLE sessions ADD COLUMN target TEXT", []);
        }

        if version < 4 {
            // What a session has cost so far — across every run it has been
            // resumed for, not just its latest one — is the same kind of
            // per-session property `target` is: something a later run needs
            // to carry forward, not derive fresh. Stored as opaque JSON
            // (a serialized `pipeline::Spend`) rather than one column per
            // field: this crate has no reason to know that struct's shape,
            // only to hold it for whoever does.
            let _ = conn.execute("ALTER TABLE sessions ADD COLUMN spend_json TEXT", []);
        }

        if version < SCHEMA_VERSION {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    }

    fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS documents (
                doc_hash   TEXT PRIMARY KEY,
                label      TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                doc_hash   TEXT NOT NULL,
                profile    TEXT,
                label      TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edits (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id     TEXT NOT NULL,
                seq            INTEGER NOT NULL,
                created_at     TEXT NOT NULL,
                action_label   TEXT NOT NULL,
                structure_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_doc ON sessions(doc_hash, created_at);
            -- The bytes a session was converted from. Keyed by the content hash
            -- rather than the session, so re-uploading the same document, or
            -- converting it in several tabs, shares one copy. Without these a
            -- reopened session could be read but never continued: resuming
            -- replays the sources through the agent.
            CREATE TABLE IF NOT EXISTS session_sources (
                doc_hash   TEXT NOT NULL,
                file_index INTEGER NOT NULL,
                name       TEXT NOT NULL,
                bytes      BLOB NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (doc_hash, file_index)
            );
            -- A stage's LLM conversation, one row per message. New in schema
            -- version 3; a brand new table needs no `migrate()` entry of its
            -- own, `CREATE TABLE IF NOT EXISTS` here is enough to add it to an
            -- existing v2 database with nothing to backfill.
            CREATE TABLE IF NOT EXISTS conversations (
                session_id   TEXT NOT NULL,
                stage        TEXT NOT NULL,
                seq          INTEGER NOT NULL,
                created_at   TEXT NOT NULL,
                message_json TEXT NOT NULL,
                PRIMARY KEY (session_id, stage, seq)
            );",
        )?;
        // Reference-form tables (shared schema with the `reference-builder`
        // crate, so dataset exports import without drift). Stored in the same
        // `history.db`; only these tables are written by reference import/export.
        conn.execute_batch(references_mcp::reference_db::SCHEMA_SQL)?;
        Ok(())
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    // ── Settings ────────────────────────────────────────────────────────────

    pub fn get_setting(key: &str) -> Option<String> {
        let conn = open().ok()?;
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
    }

    pub fn set_setting(key: &str, value: &str) {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return;
        };
        warn_db(
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            ),
            "saving a setting",
        );
    }

    // ── Documents & sessions ────────────────────────────────────────────────

    /// The key a document's edit history is stored under; the same
    /// content hash the reference store keys its references by.
    pub fn document_hash(files: &[(String, Vec<u8>)]) -> String {
        references_mcp::document_hash(files)
    }

    pub fn upsert_document(doc_hash: &str, label: &str) {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return;
        };
        warn_db(
            conn.execute(
                "INSERT INTO documents (doc_hash, label, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(doc_hash) DO UPDATE SET label = excluded.label",
                rusqlite::params![doc_hash, label, now()],
            ),
            "recording a document",
        );
    }

    pub fn create_session(
        doc_hash: &str,
        profile: Option<&str>,
        target: &str,
        label: &str,
    ) -> Option<String> {
        let conn = warn_db(open(), "opening the store")?;
        let session_id = uuid::Uuid::new_v4().to_string();
        warn_db(
            conn.execute(
                "INSERT INTO sessions (session_id, doc_hash, profile, target, label, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![session_id, doc_hash, profile, target, label, now()],
            ),
            "creating a session",
        )?;
        Some(session_id)
    }

    /// The output target recorded for a session, if any.
    ///
    /// `None` for sessions written before the column existed; the caller falls
    /// back to whatever the tab remembers.
    pub fn session_target(session_id: &str) -> Option<String> {
        let conn = warn_db(open(), "opening the store")?;
        conn.query_row(
            "SELECT target FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// The running spend recorded for a session, as opaque JSON — `None` for
    /// a session that has not billed anything yet, or one written before the
    /// column existed.
    ///
    /// Opaque because this crate carries no `pipeline::Spend` type of its own
    /// (`pipeline` already depends on `agent`, so the reverse would be
    /// circular) — the caller, which does depend on both, is what
    /// serializes and parses it.
    pub fn session_spend_json(session_id: &str) -> Option<String> {
        let conn = warn_db(open(), "opening the store")?;
        conn.query_row(
            "SELECT spend_json FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// Record a session's running spend, as opaque JSON — see
    /// [`session_spend_json`] for why this crate never parses it.
    pub fn set_session_spend_json(session_id: &str, spend_json: &str) {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return;
        };
        warn_db(
            conn.execute(
                "UPDATE sessions SET spend_json = ?1 WHERE session_id = ?2",
                rusqlite::params![spend_json, session_id],
            ),
            "recording a session's spend",
        );
    }

    /// Look up the conversion profile stored for a session, if any.
    pub fn session_profile(session_id: &str) -> Option<String> {
        let conn = open().ok()?;
        conn.query_row(
            "SELECT profile FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    pub fn list_sessions(doc_hash: &str) -> Vec<SessionInfo> {
        let Ok(conn) = open() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT s.session_id, s.label, s.profile, s.created_at,
                    (SELECT COUNT(*) FROM edits e
                      WHERE e.session_id IN (s.session_id, s.session_id || '#document'))
             FROM sessions s
             WHERE s.doc_hash = ?1
             ORDER BY s.created_at DESC",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([doc_hash], |row| {
            Ok(SessionInfo {
                session_id: row.get(0)?,
                label: row.get(1)?,
                profile: row.get(2)?,
                created_at: row.get(3)?,
                edit_count: row.get::<_, i64>(4)? as usize,
            })
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// List every editing session across all documents, newest first. Used by
    /// the "load previous session" browser shown before an upload.
    pub fn list_all_sessions() -> Vec<SessionInfo> {
        let Ok(conn) = open() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT s.session_id, s.label, s.profile, s.created_at,
                    (SELECT COUNT(*) FROM edits e
                      WHERE e.session_id IN (s.session_id, s.session_id || '#document'))
             FROM sessions s
             ORDER BY s.created_at DESC",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |row| {
            Ok(SessionInfo {
                session_id: row.get(0)?,
                label: row.get(1)?,
                profile: row.get(2)?,
                created_at: row.get(3)?,
                edit_count: row.get::<_, i64>(4)? as usize,
            })
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The document hashes of the `limit` most recent sessions.
    ///
    /// What the source-document prune keeps beyond the open tabs: a session the
    /// operator might still reopen has to keep the bytes that would let them
    /// continue it.
    pub fn recent_doc_hashes(limit: usize) -> Vec<String> {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return Vec::new();
        };
        let Some(mut stmt) = warn_db(
            conn.prepare(
                "SELECT DISTINCT doc_hash FROM sessions ORDER BY created_at DESC LIMIT ?1",
            ),
            "listing recent sessions",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([limit as i64], |row| row.get::<_, String>(0));
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Permanently delete an editing session and all of its edits.
    ///
    /// One transaction, so a failure between the two statements cannot leave the
    /// edits behind as orphans that no session lists any more.
    pub fn delete_session(session_id: &str) {
        let Some(mut conn) = warn_db(open(), "opening the store") else {
            return;
        };
        warn_db(delete_session_conn(&mut conn, session_id), "deleting a session");
    }

    fn delete_session_conn(conn: &mut Connection, session_id: &str) -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        // A run's document, and the AEM tree and page headers older sessions
        // recorded, live under sibling ids, so they have to go too.
        let document_session = format!("{session_id}{DOCUMENT_SUFFIX}");
        let aem_session = format!("{session_id}#aem");
        let headers_session = format!("{session_id}#headers");
        tx.execute(
            "DELETE FROM edits WHERE session_id IN (?1, ?2, ?3, ?4)",
            [session_id, &document_session, &aem_session, &headers_session],
        )?;
        tx.execute("DELETE FROM sessions WHERE session_id = ?1", [session_id])?;
        tx.commit()
    }

    // ── Source documents ────────────────────────────────────────────────────

    /// Store the bytes a document set was converted from.
    ///
    /// Content-addressed, so storing the same document twice is a no-op rather
    /// than a second copy. One transaction: a half-written source set would look
    /// present but reload short.
    pub fn store_sources(doc_hash: &str, files: &[(String, Vec<u8>)]) {
        let Some(mut conn) = warn_db(open(), "opening the store") else {
            return;
        };
        let Some(tx) = warn_db(conn.transaction(), "starting a source write") else {
            return;
        };
        let stored = (|| -> rusqlite::Result<()> {
            let now = now();
            for (index, (name, bytes)) in files.iter().enumerate() {
                tx.execute(
                    "INSERT INTO session_sources (doc_hash, file_index, name, bytes, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(doc_hash, file_index) DO NOTHING",
                    rusqlite::params![doc_hash, index as i64, name, bytes, now],
                )?;
            }
            Ok(())
        })();
        if warn_db(stored, "storing source documents").is_some() {
            warn_db(tx.commit(), "committing source documents");
        }
    }

    /// Load the sources stored for `doc_hash`, in their original order.
    pub fn load_sources(doc_hash: &str) -> Vec<(String, Vec<u8>)> {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return Vec::new();
        };
        let Some(mut stmt) = warn_db(
            conn.prepare(
                "SELECT name, bytes FROM session_sources
                 WHERE doc_hash = ?1 ORDER BY file_index ASC",
            ),
            "reading source documents",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([doc_hash], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(e) => {
                eprintln!("history store: reading source documents failed: {e}");
                Vec::new()
            }
        }
    }

    /// Whether any bytes are stored for `doc_hash`.
    ///
    /// A cheap existence check, so deciding what a restored tab can do does not
    /// pull megabytes off disk.
    pub fn has_sources(doc_hash: &str) -> bool {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return false;
        };
        conn.query_row(
            "SELECT 1 FROM session_sources WHERE doc_hash = ?1 LIMIT 1",
            [doc_hash],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    /// Total size of the stored source documents, in bytes.
    pub fn sources_size() -> u64 {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return 0;
        };
        conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(bytes)), 0) FROM session_sources",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
        .unwrap_or(0)
    }

    /// Every hash the source store holds.
    pub fn stored_source_hashes() -> Vec<String> {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return Vec::new();
        };
        let Some(mut stmt) = warn_db(
            conn.prepare("SELECT DISTINCT doc_hash FROM session_sources"),
            "listing source documents",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |row| row.get::<_, String>(0));
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Which stored hashes nothing refers to any more.
    ///
    /// Pure, so the pruning rule is testable without a database: `keep` is the
    /// set still reachable from an open tab or a recent session.
    pub fn orphan_hashes(stored: &[String], keep: &[String]) -> Vec<String> {
        let keep: std::collections::HashSet<&str> = keep.iter().map(String::as_str).collect();
        stored
            .iter()
            .filter(|hash| !keep.contains(hash.as_str()))
            .cloned()
            .collect()
    }

    /// Delete the stored sources for hashes nothing refers to any more.
    ///
    /// Returns how many documents were dropped. `VACUUM` is left to the caller
    /// and only worth running when something actually went — SQLite does not
    /// shrink the file on its own, but vacuuming rewrites the whole database.
    pub fn prune_sources(keep: &[String]) -> usize {
        let orphans = orphan_hashes(&stored_source_hashes(), keep);
        if orphans.is_empty() {
            return 0;
        }
        let Some(mut conn) = warn_db(open(), "opening the store") else {
            return 0;
        };
        let Some(tx) = warn_db(conn.transaction(), "starting a source prune") else {
            return 0;
        };
        let pruned = (|| -> rusqlite::Result<()> {
            for hash in &orphans {
                tx.execute("DELETE FROM session_sources WHERE doc_hash = ?1", [hash])?;
            }
            Ok(())
        })();
        if warn_db(pruned, "pruning source documents").is_none() {
            return 0;
        }
        if warn_db(tx.commit(), "committing a source prune").is_none() {
            return 0;
        }
        orphans.len()
    }

    /// Reclaim the space a prune freed. Only worth calling after one.
    pub fn vacuum() {
        if let Some(conn) = warn_db(open(), "opening the store") {
            warn_db(conn.execute_batch("VACUUM"), "reclaiming space");
        }
    }

    // ── Edits ───────────────────────────────────────────────────────────────

    /// Append a snapshot, choosing its sequence number in the same statement.
    ///
    /// Reading `MAX(seq) + 1` and then inserting it as two statements is a
    /// lost-update race: two parallel conversions both read the same number and
    /// both write it. One statement cannot be interleaved — SQLite holds the
    /// write lock for its whole duration — and `idx_edits_session_seq` turns any
    /// future regression into a loud error instead of a duplicate row.
    ///
    /// The aggregate has no `GROUP BY`, so it yields exactly one row even for a
    /// session with no edits yet, and `COALESCE` starts that session at 0.
    fn insert_edit_conn(
        conn: &Connection,
        session_id: &str,
        action_label: &str,
        structure_json: &str,
    ) -> Option<usize> {
        let seq = conn.query_row(
            "INSERT INTO edits (session_id, seq, created_at, action_label, structure_json)
             SELECT ?1, COALESCE(MAX(seq) + 1, 0), ?2, ?3, ?4 FROM edits WHERE session_id = ?1
             RETURNING seq",
            rusqlite::params![session_id, now(), action_label, structure_json],
            |row| row.get::<_, i64>(0),
        );
        warn_db(seq, "recording an edit-history snapshot").map(|n| n as usize)
    }

    fn snapshot_at_conn(conn: &Connection, session_id: &str, seq: usize) -> Option<String> {
        conn.query_row(
            "SELECT structure_json FROM edits WHERE session_id = ?1 AND seq = ?2",
            rusqlite::params![session_id, seq as i64],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    fn latest_seq_conn(conn: &Connection, session_id: &str) -> Option<usize> {
        conn.query_row(
            "SELECT MAX(seq) FROM edits WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
        .map(|n| n as usize)
    }

    fn list_edits_conn(conn: &Connection, session_id: &str) -> Vec<EditInfo> {
        let Ok(mut stmt) = conn.prepare(
            "SELECT seq, created_at, action_label FROM edits
             WHERE session_id = ?1 ORDER BY seq ASC",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([session_id], |row| {
            Ok(EditInfo {
                seq: row.get::<_, i64>(0)? as usize,
                created_at: row.get(1)?,
                action_label: row.get(2)?,
            })
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Append a snapshot at the next sequence number. Returns the new seq.
    pub fn insert_edit(
        session_id: &str,
        action_label: &str,
        structure_json: &str,
    ) -> Option<usize> {
        let conn = open().ok()?;
        insert_edit_conn(&conn, session_id, action_label, structure_json)
    }

    pub fn snapshot_at(session_id: &str, seq: usize) -> Option<String> {
        let conn = open().ok()?;
        snapshot_at_conn(&conn, session_id, seq)
    }

    pub fn latest_seq(session_id: &str) -> Option<usize> {
        let conn = open().ok()?;
        latest_seq_conn(&conn, session_id)
    }

    pub fn list_edits(session_id: &str) -> Vec<EditInfo> {
        let Ok(conn) = open() else {
            return Vec::new();
        };
        list_edits_conn(&conn, session_id)
    }

    // ── Stage conversations ──────────────────────────────────────────────────
    //
    // One row per LLM message, opaque JSON. This module never deserializes it
    // — `pipeline::memory::SqliteConversationMemory` is the one place that
    // knows what a rig `Message` is and does the (de)serializing.

    fn load_conversation_conn(conn: &Connection, session_id: &str, stage: &str) -> Vec<String> {
        let Some(mut stmt) = warn_db(
            conn.prepare(
                "SELECT message_json FROM conversations
                 WHERE session_id = ?1 AND stage = ?2 ORDER BY seq ASC",
            ),
            "preparing a conversation load",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map(rusqlite::params![session_id, stage], |row| {
            row.get::<_, String>(0)
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Load a stage's stored conversation, oldest first. Empty both when
    /// nothing was ever stored and when the read failed — best-effort, like
    /// every other reader in this module; a caller that needs to tell those
    /// apart has no way to here, matching `list_edits`/`load_sources`.
    pub fn load_conversation(session_id: &str, stage: &str) -> Vec<String> {
        let Ok(conn) = open() else {
            return Vec::new();
        };
        load_conversation_conn(&conn, session_id, stage)
    }

    fn append_conversation_conn(
        conn: &mut Connection,
        session_id: &str,
        stage: &str,
        messages: &[String],
    ) {
        if messages.is_empty() {
            return;
        }
        let Some(tx) = warn_db(conn.transaction(), "starting a conversation append") else {
            return;
        };
        let appended = (|| -> rusqlite::Result<()> {
            let now = now();
            for message_json in messages {
                // One atomic statement per message, exactly like
                // `insert_edit_conn`'s own `COALESCE(MAX(seq) + 1, 0)`:
                // reading the next `seq` and inserting it as two separate
                // statements (a prior version of this function did) is a
                // lost-update race — two concurrent appends to the same
                // `(session_id, stage)` can both read the same `MAX(seq)`
                // before either has written, and the second write then fails
                // (or, without a unique index, silently duplicates a seq).
                // Folding the read into the `INSERT`'s own `SELECT` makes the
                // read-then-write indivisible.
                tx.execute(
                    "INSERT INTO conversations (session_id, stage, seq, created_at, message_json)
                     SELECT ?1, ?2, COALESCE(MAX(seq) + 1, 0), ?3, ?4
                     FROM conversations WHERE session_id = ?1 AND stage = ?2",
                    rusqlite::params![session_id, stage, now, message_json],
                )?;
            }
            Ok(())
        })();
        if warn_db(appended, "appending a conversation").is_some() {
            warn_db(tx.commit(), "committing a conversation append");
        }
    }

    /// Append `messages` (each already serialized to JSON) at the next
    /// sequence numbers, in one transaction — a half-written turn would look
    /// present but reload short, same reasoning as [`store_sources`].
    pub fn append_conversation(session_id: &str, stage: &str, messages: &[String]) {
        let Some(mut conn) = warn_db(open(), "opening the store") else {
            return;
        };
        append_conversation_conn(&mut conn, session_id, stage, messages);
    }

    fn clear_conversation_conn(conn: &Connection, session_id: &str, stage: &str) {
        warn_db(
            conn.execute(
                "DELETE FROM conversations WHERE session_id = ?1 AND stage = ?2",
                rusqlite::params![session_id, stage],
            ),
            "clearing a conversation",
        );
    }

    /// Drop a stage's stored conversation entirely.
    pub fn clear_conversation(session_id: &str, stage: &str) {
        let Some(conn) = warn_db(open(), "opening the store") else {
            return;
        };
        clear_conversation_conn(&conn, session_id, stage);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A scratch database carrying the same schema and indexes a real one
        /// gets, so the unique constraint is under test rather than bypassed.
        fn mem() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            ensure_schema(&conn).unwrap();
            migrate(&conn).unwrap();
            conn
        }

        #[test]
        fn document_hash_is_order_independent() {
            let a = ("a.xml".to_string(), b"alpha".to_vec());
            let b = ("b.xml".to_string(), b"beta".to_vec());
            let forward = document_hash(&[a.clone(), b.clone()]);
            let reversed = document_hash(&[b, a]);
            assert_eq!(forward, reversed);
        }

        #[test]
        fn document_hash_differs_on_content() {
            let one = document_hash(&[("f".into(), b"one".to_vec())]);
            let two = document_hash(&[("f".into(), b"two".to_vec())]);
            assert_ne!(one, two);
        }

        #[test]
        fn edits_append_with_increasing_seq() {
            let conn = mem();
            assert_eq!(insert_edit_conn(&conn, "s", "first", "{}"), Some(0));
            assert_eq!(insert_edit_conn(&conn, "s", "second", "{}"), Some(1));
            assert_eq!(latest_seq_conn(&conn, "s"), Some(1));
        }

        #[test]
        fn snapshot_round_trips() {
            let conn = mem();
            insert_edit_conn(&conn, "s", "first", "{\"v\":1}");
            insert_edit_conn(&conn, "s", "second", "{\"v\":2}");
            assert_eq!(
                snapshot_at_conn(&conn, "s", 0).as_deref(),
                Some("{\"v\":1}")
            );
            assert_eq!(
                snapshot_at_conn(&conn, "s", 1).as_deref(),
                Some("{\"v\":2}")
            );
        }

        #[test]
        fn list_edits_is_ordered_by_seq() {
            let conn = mem();
            insert_edit_conn(&conn, "s", "a", "0");
            insert_edit_conn(&conn, "s", "b", "1");
            let edits = list_edits_conn(&conn, "s");
            assert_eq!(edits.len(), 2);
            assert_eq!(edits[0].seq, 0);
            assert_eq!(edits[0].action_label, "a");
            assert_eq!(edits[1].seq, 1);
            assert_eq!(edits[1].action_label, "b");
        }

        /// A conversation round-trips in order, and appending twice extends it
        /// rather than overwriting it — the shape `pipeline`'s memory backend
        /// relies on: every turn's messages land after the ones before them.
        #[test]
        fn a_conversation_appends_and_loads_in_order() {
            let mut conn = mem();
            append_conversation_conn(&mut conn, "s", "Author", &["\"m0\"".into(), "\"m1\"".into()]);
            append_conversation_conn(&mut conn, "s", "Author", &["\"m2\"".into()]);

            let loaded = load_conversation_conn(&conn, "s", "Author");
            assert_eq!(loaded, vec!["\"m0\"", "\"m1\"", "\"m2\""]);
        }

        /// Different stages of the same session, and the same stage name in a
        /// different session, must not see each other's messages — the
        /// `(session_id, stage)` key is the whole point of keying by both.
        #[test]
        fn conversations_are_isolated_by_session_and_stage() {
            let mut conn = mem();
            append_conversation_conn(&mut conn, "s", "Author", &["\"author\"".into()]);
            append_conversation_conn(&mut conn, "s", "Reviewer", &["\"reviewer\"".into()]);
            append_conversation_conn(&mut conn, "other-session", "Author", &["\"other\"".into()]);

            assert_eq!(load_conversation_conn(&conn, "s", "Author"), vec!["\"author\""]);
            assert_eq!(load_conversation_conn(&conn, "s", "Reviewer"), vec!["\"reviewer\""]);
            assert_eq!(
                load_conversation_conn(&conn, "other-session", "Author"),
                vec!["\"other\""]
            );
        }

        /// A conversation nobody ever appended to loads empty rather than
        /// erroring — the common case, every fresh stage's first attempt.
        #[test]
        fn an_untouched_conversation_loads_empty() {
            let conn = mem();
            assert!(load_conversation_conn(&conn, "s", "Author").is_empty());
        }

        /// Clearing one stage's conversation must not touch a sibling stage's,
        /// the same isolation `clear_conversation` promises the caller.
        #[test]
        fn clearing_a_conversation_leaves_other_stages_alone() {
            let mut conn = mem();
            append_conversation_conn(&mut conn, "s", "Author", &["\"a\"".into()]);
            append_conversation_conn(&mut conn, "s", "Reviewer", &["\"r\"".into()]);

            clear_conversation_conn(&conn, "s", "Author");

            assert!(load_conversation_conn(&conn, "s", "Author").is_empty());
            assert_eq!(load_conversation_conn(&conn, "s", "Reviewer"), vec!["\"r\""]);
        }

        /// Appending nothing must not touch the sequence counter — the next
        /// real append still starts at 0, not 1.
        #[test]
        fn appending_no_messages_is_a_no_op() {
            let mut conn = mem();
            append_conversation_conn(&mut conn, "s", "Author", &[]);
            append_conversation_conn(&mut conn, "s", "Author", &["\"first\"".into()]);
            assert_eq!(load_conversation_conn(&conn, "s", "Author"), vec!["\"first\""]);
        }

        /// Concurrent appends to the *same* `(session_id, stage)` must not race
        /// on the next `seq`. An earlier version of `append_conversation_conn`
        /// read `MAX(seq) + 1` as a separate `SELECT` before inserting — two
        /// concurrent transactions could both read the same value before
        /// either wrote, and the loser's `INSERT` then failed the
        /// `(session_id, stage, seq)` primary key and rolled back silently
        /// (`warn_db` only logs to stderr; the public `append_conversation`
        /// has no error return at all). Folding the read into the `INSERT`'s
        /// own `SELECT` — one atomic statement, [`insert_edit_conn`]'s own
        /// pattern — is what this test is pinning.
        #[test]
        fn concurrent_appends_to_one_conversation_lose_nothing() {
            const THREADS: usize = 8;
            const MESSAGES_PER_THREAD: usize = 20;

            claim_scratch_db_for_test();

            std::thread::scope(|scope| {
                for thread in 0..THREADS {
                    scope.spawn(move || {
                        for i in 0..MESSAGES_PER_THREAD {
                            append_conversation(
                                "concurrent-session",
                                "Author",
                                &[format!("\"{thread}:{i}\"")],
                            );
                        }
                    });
                }
            });

            let loaded = load_conversation("concurrent-session", "Author");
            assert_eq!(
                loaded.len(),
                THREADS * MESSAGES_PER_THREAD,
                "some concurrent appends were silently lost"
            );
            // No `seq` collision means no message was ever dropped in favour
            // of another with the same slot, so every payload is distinct.
            let unique: std::collections::HashSet<_> = loaded.iter().collect();
            assert_eq!(unique.len(), loaded.len(), "a message was overwritten or duplicated");
        }

        #[test]
        fn sessions_listed_newest_first() {
            let conn = mem();
            // created_at is generated via now(); insert with explicit ordering.
            conn.execute(
                "INSERT INTO sessions (session_id, doc_hash, profile, label, created_at)
                 VALUES ('old', 'h', NULL, 'old', '2020-01-01T00:00:00Z'),
                        ('new', 'h', NULL, 'new', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT session_id FROM sessions WHERE doc_hash = ?1 ORDER BY created_at DESC",
                )
                .unwrap();
            let ids: Vec<String> = stmt
                .query_map(["h"], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            assert_eq!(ids, vec!["new".to_string(), "old".to_string()]);
        }

        /// Two conversions running at once both snapshot after every tool call.
        /// Choosing the sequence number in a separate statement let them both
        /// read the same number and both write it, and a duplicate is not an
        /// error anyone sees — `snapshot_at` just starts returning the wrong
        /// tree. This is the assertion that the store survives parallel runs.
        #[test]
        fn parallel_writers_keep_every_snapshot_distinct() {
            const THREADS_PER_SESSION: usize = 4;
            const EDITS_PER_THREAD: usize = 50;
            const PER_SESSION: usize = THREADS_PER_SESSION * EDITS_PER_THREAD;

            claim_scratch_db_for_test();

            let recorded: Vec<(usize, String, usize)> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..THREADS_PER_SESSION * 2)
                    .map(|thread| {
                        scope.spawn(move || {
                            let session = if thread % 2 == 0 { "sess-a" } else { "sess-b" };
                            (0..EDITS_PER_THREAD)
                                .map(|i| {
                                    let payload = format!("{thread}:{i}");
                                    let seq = insert_edit(session, "edit", &payload)
                                        .expect("every write has to be recorded");
                                    (thread, payload, seq)
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap())
                    .collect()
            });

            assert_eq!(recorded.len(), PER_SESSION * 2);

            for session in ["sess-a", "sess-b"] {
                let seqs: Vec<usize> = list_edits(session).into_iter().map(|e| e.seq).collect();
                assert_eq!(
                    seqs,
                    (0..PER_SESSION).collect::<Vec<_>>(),
                    "{session} has to hold every sequence number exactly once"
                );
            }

            // The numbering being intact is not enough: each snapshot has to
            // still be readable under the number it was given.
            for (thread, payload, seq) in &recorded {
                let session = if thread % 2 == 0 { "sess-a" } else { "sess-b" };
                assert_eq!(
                    snapshot_at(session, *seq).as_deref(),
                    Some(payload.as_str()),
                    "{session} seq {seq} came back as another writer's snapshot"
                );
            }
        }

        /// The index is what turns a future regression into a loud failure
        /// rather than a silently duplicated snapshot.
        #[test]
        fn a_duplicate_sequence_number_is_refused() {
            let conn = mem();
            insert_edit_conn(&conn, "s", "first", "{}");
            let duplicate = conn.execute(
                "INSERT INTO edits (session_id, seq, created_at, action_label, structure_json)
                 VALUES ('s', 0, '2024-01-01T00:00:00Z', 'forced', '{}')",
                [],
            );
            assert!(duplicate.is_err(), "the store accepted a duplicate seq");
        }

        /// Databases written before the unique index existed may already hold
        /// duplicates, and the index cannot be created over them.
        #[test]
        fn migration_clears_duplicates_left_by_the_old_writer() {
            let conn = Connection::open_in_memory().unwrap();
            ensure_schema(&conn).unwrap();
            conn.execute(
                "INSERT INTO edits (session_id, seq, created_at, action_label, structure_json)
                 VALUES ('s', 0, '2024-01-01T00:00:00Z', 'kept', 'first'),
                        ('s', 0, '2024-01-01T00:00:01Z', 'lost', 'second'),
                        ('s', 1, '2024-01-01T00:00:02Z', 'kept', 'third')",
                [],
            )
            .unwrap();

            migrate(&conn).unwrap();

            let edits = list_edits_conn(&conn, "s");
            assert_eq!(edits.len(), 2, "the duplicate should have been dropped");
            // The earliest row wins, so the surviving history stays in order.
            assert_eq!(snapshot_at_conn(&conn, "s", 0).as_deref(), Some("first"));
            assert_eq!(snapshot_at_conn(&conn, "s", 1).as_deref(), Some("third"));
        }

        /// A database left at schema version 2 (before the `conversations`
        /// table existed) has to reach the latest version with its existing
        /// edit history intact and the new table usable — the new table is
        /// added by `ensure_schema`'s own `CREATE TABLE IF NOT EXISTS`, so
        /// this pins that running it against an *existing* v2 database (not
        /// just a fresh one) actually adds it, and that migrating changes
        /// nothing about the data that was already there.
        #[test]
        fn a_v2_database_migrates_to_the_latest_schema_without_losing_edits() {
            let mut conn = Connection::open_in_memory().unwrap();
            ensure_schema(&conn).unwrap();
            insert_edit_conn(&conn, "s", "before the migration", "{}");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('schema_version', '2')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .unwrap();

            migrate(&conn).unwrap();

            let version: String = conn
                .query_row(
                    "SELECT value FROM settings WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION.to_string());
            assert_eq!(list_edits_conn(&conn, "s").len(), 1, "prior edits must survive");

            // The new table has to be usable on this migrated database, not
            // just on a freshly created one.
            append_conversation_conn(&mut conn, "s", "Author", &["\"hi\"".into()]);
            assert_eq!(load_conversation_conn(&conn, "s", "Author"), vec!["\"hi\""]);
        }

        /// Resuming a session replays its sources, so the bytes have to come
        /// back exactly — and in order, because the language variants are
        /// positional to the caller.
        #[test]
        fn stored_sources_round_trip_in_order() {
            let conn = mem();
            let files = [
                ("AAOV_033_DE.pdf".to_string(), vec![1u8, 2, 3]),
                ("AAOV_033_FR.pdf".to_string(), vec![4u8, 5]),
            ];
            let now = now();
            for (i, (name, bytes)) in files.iter().enumerate() {
                conn.execute(
                    "INSERT INTO session_sources (doc_hash, file_index, name, bytes, created_at)
                     VALUES ('h', ?1, ?2, ?3, ?4)",
                    rusqlite::params![i as i64, name, bytes, now],
                )
                .unwrap();
            }

            let mut stmt = conn
                .prepare(
                    "SELECT name, bytes FROM session_sources
                     WHERE doc_hash = 'h' ORDER BY file_index ASC",
                )
                .unwrap();
            let read: Vec<(String, Vec<u8>)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            assert_eq!(read, files);
        }

        /// Content-addressed: the same document converted in three tabs is one
        /// copy on disk, not three.
        #[test]
        fn storing_the_same_document_twice_keeps_one_copy() {
            let conn = mem();
            for _ in 0..2 {
                conn.execute(
                    "INSERT INTO session_sources (doc_hash, file_index, name, bytes, created_at)
                     VALUES ('h', 0, 'a.pdf', X'0102', '2024-01-01T00:00:00Z')
                     ON CONFLICT(doc_hash, file_index) DO NOTHING",
                    [],
                )
                .unwrap();
            }
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM session_sources", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1);
        }

        /// The prune must only ever drop what nothing points at any more.
        #[test]
        fn only_unreferenced_sources_are_pruned() {
            let stored = vec!["a".to_string(), "b".to_string(), "c".to_string()];
            let keep = vec!["b".to_string(), "missing".to_string()];
            assert_eq!(orphan_hashes(&stored, &keep), ["a", "c"]);
            // Nothing stored, or everything referenced, is a no-op.
            assert!(orphan_hashes(&[], &keep).is_empty());
            assert!(orphan_hashes(&stored, &stored).is_empty());
        }

        /// Resuming with the wrong target gives a run that cannot see its own
        /// prior work, so the session has to remember what it authored.
        #[test]
        fn a_session_remembers_its_target() {
            let conn = mem();
            conn.execute(
                "INSERT INTO sessions (session_id, doc_hash, profile, target, label, created_at)
                 VALUES ('s', 'h', 'ubs', 'redacto', 'l', '2024-01-01T00:00:00Z'),
                        ('old', 'h', 'ubs', NULL, 'l', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

            let read = |id: &str| {
                conn.query_row(
                    "SELECT target FROM sessions WHERE session_id = ?1",
                    [id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .unwrap()
            };
            assert_eq!(read("s").as_deref(), Some("redacto"));
            // Written before the column existed: the caller falls back to
            // whatever the tab remembers.
            assert_eq!(read("old"), None);
        }

        /// A form's cost has to survive being resumed: a fresh session has
        /// none recorded yet, and a later run's `set_session_spend_json` has
        /// to be what a subsequent read sees, not what a *different* session
        /// happens to hold.
        #[test]
        fn a_sessions_spend_round_trips_and_is_isolated_from_others() {
            claim_scratch_db_for_test();
            let a = create_session("h", None, "redacto", "l").expect("session a");
            let b = create_session("h", None, "redacto", "l").expect("session b");

            assert_eq!(session_spend_json(&a), None, "a fresh session has billed nothing yet");

            set_session_spend_json(&a, r#"{"cost_usd":1.5}"#);

            assert_eq!(session_spend_json(&a).as_deref(), Some(r#"{"cost_usd":1.5}"#));
            assert_eq!(session_spend_json(&b), None, "one session's spend must not leak into another's");
        }

        /// A run's document, and the AEM tree and page headers older sessions
        /// recorded, live under sibling ids, so deleting a session has to take
        /// them too, otherwise those rows outlive every reference to them.
        #[test]
        fn deleting_a_session_leaves_no_orphan_snapshots() {
            let mut conn = mem();
            insert_edit_conn(&conn, "s", "structured", "{}");
            insert_edit_conn(&conn, "s#aem", "tree", "{}");
            insert_edit_conn(&conn, "s#headers", "headers", "{}");
            insert_edit_conn(&conn, "s#document", "AI: json_patch", "{}");
            conn.execute(
                "INSERT INTO sessions (session_id, doc_hash, profile, label, created_at)
                 VALUES ('s', 'h', NULL, 'l', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

            delete_session_conn(&mut conn, "s").unwrap();

            assert!(list_edits_conn(&conn, "s").is_empty());
            assert!(
                list_edits_conn(&conn, "s#aem").is_empty(),
                "the AEM tree outlived the session it belonged to"
            );
            assert!(
                list_edits_conn(&conn, "s#headers").is_empty(),
                "the page headers outlived the session they belonged to"
            );
            assert!(
                list_edits_conn(&conn, "s#document").is_empty(),
                "the document outlived the session it belonged to"
            );
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

pub use imp::*;
