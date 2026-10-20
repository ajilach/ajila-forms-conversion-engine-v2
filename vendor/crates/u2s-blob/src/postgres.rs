//! The Postgres backend: blobs in the `blobs` table the u2s stack migrates
//! (`migrations/`, owned by `u2s-store`).
//!
//! ## Why a synchronous facade over an async pool
//!
//! [`BlobStore`](crate::BlobStore) is synchronous: most of its callers are
//! (an MCP server shaping a page, the agent's blob sink), and the folder
//! backend it replaces blocked on file I/O the same way. sqlx is async, so
//! this backend owns its own one-worker tokio runtime and pool, and every
//! operation is `block_on` that runtime from a scoped helper thread.
//!
//! The helper thread is what makes this safe to call from anywhere: calling
//! `block_on` directly from inside another tokio runtime's task panics, and
//! a caller cannot always know whether it is in one. A plain thread is never
//! inside a runtime. The cost is one short-lived thread per blob operation,
//! which is small next to a database round trip.
//!
//! Queries are runtime-checked (`sqlx::query`), not `query!` macros, so this
//! crate needs no `.sqlx` cache and stays vendorable; the round trip is
//! covered by this module's tests against a migrated database.

use std::str::FromStr;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::{BlobError, BlobHandle};

/// A handful of connections per process: every MCP server opens its own
/// store, and the stack runs several servers per run.
const MAX_CONNECTIONS: u32 = 2;
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct PostgresBlobs {
    /// `Option` only so [`Drop`] can take it: a runtime dropped normally
    /// panics when that happens inside an async context, which is where a
    /// store usually ends its life.
    runtime: Option<tokio::runtime::Runtime>,
    pool: PgPool,
}

impl Drop for PostgresBlobs {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl PostgresBlobs {
    pub(crate) fn connect_lazy(database_url: &str) -> Result<Self, BlobError> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|err| BlobError::Configuration(format!("DATABASE_URL: {err}")))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("u2s-blob-postgres")
            .enable_all()
            .build()
            .map_err(|err| BlobError::Database(format!("starting the blob runtime: {err}")))?;
        // Built inside the runtime, so the pool's connections and timers
        // belong to the runtime every later operation runs on.
        let pool = {
            let _entered = runtime.enter();
            PgPoolOptions::new()
                .max_connections(MAX_CONNECTIONS)
                .acquire_timeout(ACQUIRE_TIMEOUT)
                .connect_lazy_with(options)
        };
        Ok(Self {
            runtime: Some(runtime),
            pool,
        })
    }

    /// Runs one database operation to completion on this backend's runtime.
    /// See the module doc for why it goes through a scoped thread.
    fn run<T, F, Fut>(&self, operation: F) -> Result<T, BlobError>
    where
        T: Send,
        F: FnOnce(PgPool) -> Fut + Send,
        Fut: Future<Output = Result<T, sqlx::Error>>,
    {
        let pool = self.pool.clone();
        let runtime = self
            .runtime
            .as_ref()
            .expect("the runtime is only taken by Drop");
        std::thread::scope(|scope| {
            scope
                .spawn(|| runtime.block_on(operation(pool)))
                .join()
                .map_err(|_| BlobError::Database("a blob operation panicked".to_owned()))?
                .map_err(|err| BlobError::Database(err.to_string()))
        })
    }

    pub(crate) fn put(&self, handle: &str, media_type: &str, data: &[u8]) -> Result<(), BlobError> {
        let byte_len = i64::try_from(data.len())
            .map_err(|_| BlobError::Database(format!("{handle}: too large to store")))?;
        self.run(|pool| async move {
            // Storing content that is already there refreshes its
            // `created_at`: the blob is about to be referenced again, and the
            // garbage collection (`u2s_store::blobs::delete_unreferenced`)
            // must not take an old, so far unreferenced copy in between.
            sqlx::query(
                "INSERT INTO blobs (handle, media_type, byte_len, bytes) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (handle) DO UPDATE SET created_at = now()",
            )
            .bind(handle)
            .bind(media_type)
            .bind(byte_len)
            .bind(data)
            .execute(&pool)
            .await
            .map(|_| ())
        })
    }

    pub(crate) fn get(&self, handle: &BlobHandle) -> Result<Vec<u8>, BlobError> {
        let key = handle.to_string();
        let row: Option<Vec<u8>> = self.run(|pool| {
            let key = key.clone();
            async move {
                sqlx::query_scalar("SELECT bytes FROM blobs WHERE handle = $1")
                    .bind(key)
                    .fetch_optional(&pool)
                    .await
            }
        })?;
        row.ok_or(BlobError::Missing { handle: key })
    }
}

#[cfg(test)]
mod tests {
    use crate::{BlobError, BlobStore};

    /// A store on the test's own migrated database: `DATABASE_URL` (which
    /// `sqlx::test` requires anyway, and which carries the password) with
    /// the database name swapped for the one `sqlx::test` created.
    fn store_for(pool: &sqlx::PgPool) -> BlobStore {
        let base = std::env::var("DATABASE_URL").expect("sqlx::test needs DATABASE_URL");
        let mut url = url::Url::parse(&base).expect("DATABASE_URL parses");
        let options = pool.connect_options();
        url.set_path(
            options
                .get_database()
                .expect("sqlx::test names its database"),
        );
        BlobStore::in_postgres(url.as_str()).expect("store")
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_blob_round_trips_through_postgres(pool: sqlx::PgPool) {
        let store = store_for(&pool);
        let put = store
            .put(b"%PDF-1.7", "application/pdf", "pdf")
            .expect("put");
        let again = store
            .put(b"%PDF-1.7", "application/pdf", "pdf")
            .expect("put twice");
        assert_eq!(put.handle, again.handle, "content-addressed");
        assert_eq!(store.get(&put.handle).expect("get"), b"%PDF-1.7");

        let (count, media_type): (i64, String) =
            sqlx::query_as("SELECT count(*), max(media_type) FROM blobs")
                .fetch_one(&pool)
                .await
                .expect("count");
        assert_eq!((count, media_type.as_str()), (1, "application/pdf"));

        let path = store.materialize(&put.handle).expect("materialize");
        assert_eq!(std::fs::read(path).expect("read"), b"%PDF-1.7");
    }

    /// An old copy stored again is young again, so the garbage collection's
    /// grace period protects it until its new reference is inserted.
    #[sqlx::test(migrations = "../../migrations")]
    async fn storing_existing_content_again_refreshes_its_age(pool: sqlx::PgPool) {
        let store = store_for(&pool);
        let put = store.put(b"reused", "text/plain", "txt").expect("put");
        sqlx::query("UPDATE blobs SET created_at = now() - interval '3 days' WHERE handle = $1")
            .bind(&put.handle)
            .execute(&pool)
            .await
            .expect("age");
        store
            .put(b"reused", "text/plain", "txt")
            .expect("put again");
        let young: bool = sqlx::query_scalar(
            "SELECT created_at > now() - interval '1 minute' FROM blobs WHERE handle = $1",
        )
        .bind(&put.handle)
        .fetch_one(&pool)
        .await
        .expect("age");
        assert!(young);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_missing_or_tampered_blob_is_refused(pool: sqlx::PgPool) {
        let store = store_for(&pool);
        let missing = format!("{}.pdf", "a".repeat(64));
        assert!(matches!(
            store.get(&missing),
            Err(BlobError::Missing { .. })
        ));

        let put = store.put(b"original", "text/plain", "txt").expect("put");
        sqlx::query("UPDATE blobs SET bytes = $1, byte_len = 8 WHERE handle = $2")
            .bind(b"tampered".as_slice())
            .bind(&put.handle)
            .execute(&pool)
            .await
            .expect("tamper");
        assert!(matches!(
            store.get(&put.handle),
            Err(BlobError::DigestMismatch { .. })
        ));
    }
}
