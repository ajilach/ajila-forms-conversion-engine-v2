//! Stdio MCP server over the reference store.
//!
//! `references-mcp --db <history.db> [--profile <name>]`, or the environment
//! variables `REFERENCES_DB` and `REFERENCES_PROFILE`. The profile is fixed
//! for the life of the server: it is the host's choice, not a tool argument.

use std::sync::Arc;
use std::time::Duration;

use references_mcp::reference_db::SCHEMA_SQL;
use references_mcp::{Opener, ReferenceStore, ReferencesServer};
use rmcp::service::ServiceExt;
use rmcp::transport::stdio;
use rusqlite::Connection;

/// How long a writer waits for another writer to commit before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// `--name value` from the arguments, else the environment variable.
fn setting(args: &[String], flag: &str, env: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| std::env::var(env).ok())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = setting(&args, "--db", "REFERENCES_DB")
        .ok_or("the history database is required: pass --db <path> or set REFERENCES_DB")?;
    let profile = setting(&args, "--profile", "REFERENCES_PROFILE").unwrap_or_default();

    // The schema is applied once here, so every later connection is a plain open.
    Connection::open(&db)?.execute_batch(SCHEMA_SQL)?;
    let open: Opener = Arc::new(move || {
        let conn = Connection::open(&db).map_err(|e| e.to_string())?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| e.to_string())?;
        Ok(conn)
    });

    let server = ReferencesServer::new(ReferenceStore::new(open), profile);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
