//! Importing a dump into a session's own Redacto platform database
//! ([`crate::session`]), so the platform's `rendering` service can render
//! it. The database is reached only through `docker exec psql` inside the
//! session's Postgres container, and holds nothing but what that session
//! imported.
//!
//! **Replace, not append.** `u2s-mapper-redacto` mints every primary key
//! deterministically from the document id, so importing the same document
//! twice would collide; and the platform's `documents.document_id` has no
//! unique constraint, so a document imported under the same business id
//! would silently coexist with ours and the renderer would pick either.
//! Every import therefore first deletes whatever rows that document id
//! already has ([`replace_script`]).
//!
//! **Concurrent callers.** A session's lock already serializes its calls;
//! the import additionally runs in one `psql` session holding an advisory
//! lock on the document id, a guard that costs nothing and keeps the
//! import safe should a platform ever be shared again. The lock is
//! session-scoped: it is released when `psql` exits, also on error.

use std::collections::BTreeMap;

use u2s_verify_core::docker::{DockerError, DockerLifecycle};

/// The platform database and a role allowed to write every table in it --
/// what a session's Postgres is created with.
pub const DB_NAME: &str = "redacto";
pub const DB_USER: &str = "postgres";

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(transparent)]
    Docker(#[from] DockerError),
    #[error("psql exited {exit_code}: {output}")]
    Psql { exit_code: i64, output: String },
    #[error("the dump is not valid UTF-8: {0}")]
    NotUtf8(#[from] std::str::Utf8Error),
}

/// A Redacto business document id (`documents.document_id`), checked once
/// so it can be written into SQL as a literal without quoting concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentId(String);

impl DocumentId {
    /// `[A-Za-z0-9_.-]`, 1 to 50 characters: the column is `varchar(50)`,
    /// and every id the encoder produces fits this alphabet.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let valid = !raw.is_empty()
            && raw.len() <= 50
            && raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if valid {
            Ok(Self(raw.to_owned()))
        } else {
            Err(format!(
                "document id {raw:?} must be 1 to 50 characters of [A-Za-z0-9_.-]"
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `asset_id` behind an ownership or relation `object_id`, which the
/// encoder writes as `<asset_id>-ver-<n>`.
const ASSET_ID_OF_OBJECT: &str = "regexp_replace(object_id, '-ver-[0-9]+$', '')";

/// The whole import, as one `psql` script: take the document's advisory
/// lock, delete its previous rows in one transaction, then run `dump`
/// (which carries its own transaction). Pure function. Unit-tested.
///
/// Deleted: its relations, its ownerships (the asset `ORIGIN` rows and the
/// owner row pointing at the document), every asset only it owned, and the
/// document row. Versions cascade from their parents. An asset another
/// document still owns stays.
pub fn replace_script(document_id: &DocumentId, dump: &str) -> String {
    let id = document_id.as_str();
    format!(
        "SELECT pg_advisory_lock(hashtext('u2s-redacto-verify:{id}'));\n\
         BEGIN;\n\
         CREATE TEMP TABLE u2s_previous_assets ON COMMIT DROP AS\n\
         \x20 SELECT DISTINCT {ASSET_ID_OF_OBJECT} AS asset_id FROM app_redacto.ownerships\n\
         \x20 WHERE owner_id = '{id}' AND ownership_type = 'ORIGIN';\n\
         DELETE FROM app_redacto.relations WHERE relates_to = '{id}';\n\
         DELETE FROM app_redacto.ownerships\n\
         \x20 WHERE owner_id = '{id}' OR (object_type = 'DOCUMENT' AND object_id = '{id}');\n\
         DELETE FROM app_redacto.assets\n\
         \x20 WHERE asset_id IN (SELECT asset_id FROM u2s_previous_assets)\n\
         \x20 AND asset_id NOT IN (\n\
         \x20   SELECT {ASSET_ID_OF_OBJECT} FROM app_redacto.ownerships WHERE ownership_type = 'ORIGIN');\n\
         DELETE FROM app_redacto.documents WHERE document_id = '{id}';\n\
         COMMIT;\n\
         {dump}\n"
    )
}

/// Row counts for one document's rows in each of the six tables, one
/// `name|count` line per table in `psql -t -A -F '|'` output. Pure
/// function. Unit-tested.
pub fn count_query(document_id: &DocumentId) -> String {
    let id = document_id.as_str();
    format!(
        "WITH doc AS (SELECT id FROM app_redacto.documents WHERE document_id = '{id}'),\n\
         owned AS (SELECT DISTINCT {ASSET_ID_OF_OBJECT} AS asset_id FROM app_redacto.ownerships\n\
         \x20 WHERE owner_id = '{id}' AND ownership_type = 'ORIGIN')\n\
         SELECT 'documents', count(*) FROM doc\n\
         UNION ALL SELECT 'document_version', count(*) FROM app_redacto.document_version\n\
         \x20 WHERE document_fk_id IN (SELECT id FROM doc)\n\
         UNION ALL SELECT 'assets', count(*) FROM app_redacto.assets\n\
         \x20 WHERE asset_id IN (SELECT asset_id FROM owned)\n\
         UNION ALL SELECT 'asset_version', count(*) FROM app_redacto.asset_version v\n\
         \x20 JOIN app_redacto.assets a ON v.asset_fk_id = a.id\n\
         \x20 WHERE a.asset_id IN (SELECT asset_id FROM owned)\n\
         UNION ALL SELECT 'ownerships', count(*) FROM app_redacto.ownerships\n\
         \x20 WHERE owner_id = '{id}' OR (object_type = 'DOCUMENT' AND object_id = '{id}')\n\
         UNION ALL SELECT 'relations', count(*) FROM app_redacto.relations WHERE relates_to = '{id}';\n"
    )
}

/// Parses [`count_query`]'s output. Pure function. Unit-tested.
pub fn parse_counts(output: &str) -> BTreeMap<String, i64> {
    output
        .lines()
        .filter_map(|line| {
            let (name, count) = line.trim().split_once('|')?;
            Some((name.trim().to_owned(), count.trim().parse().ok()?))
        })
        .collect()
}

/// One document's rows in the platform database after an import.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportReport {
    pub table_counts: BTreeMap<String, i64>,
}

impl ImportReport {
    pub fn total_rows(&self) -> i64 {
        self.table_counts.values().sum()
    }
}

fn psql_args(extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "psql",
        "-U",
        DB_USER,
        "-d",
        DB_NAME,
        "-v",
        "ON_ERROR_STOP=1",
    ];
    args.extend_from_slice(extra);
    args.into_iter().map(str::to_owned).collect()
}

async fn run_psql(
    docker: &DockerLifecycle,
    container: &str,
    extra: &[&str],
    stdin: Option<&str>,
) -> Result<String, ImportError> {
    let result = docker
        .exec(container, psql_args(extra), stdin.map(str::as_bytes))
        .await?;
    if !result.succeeded() {
        return Err(ImportError::Psql {
            exit_code: result.exit_code,
            output: result.output,
        });
    }
    Ok(result.output)
}

/// Replaces `document_id`'s rows in the platform database with `dump`, and
/// reports that document's row counts afterwards. `container` is the
/// session's Postgres container.
pub async fn import(
    docker: &DockerLifecycle,
    container: &str,
    document_id: &DocumentId,
    dump_bytes: &[u8],
) -> Result<ImportReport, ImportError> {
    let dump = std::str::from_utf8(dump_bytes)?;
    run_psql(
        docker,
        container,
        &[],
        Some(&replace_script(document_id, dump)),
    )
    .await?;
    let counted = run_psql(
        docker,
        container,
        &["-t", "-A", "-F", "|", "-c", &count_query(document_id)],
        None,
    )
    .await?;
    Ok(ImportReport {
        table_counts: parse_counts(&counted),
    })
}

/// Whether the platform database answers a query right now.
pub async fn is_reachable(docker: &DockerLifecycle, container: &str) -> bool {
    run_psql(docker, container, &["-c", "SELECT 1"], None)
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_outside_the_alphabet_are_refused() {
        assert!(DocumentId::parse("aaev_019").is_ok());
        assert!(DocumentId::parse("a.b-c_1").is_ok());
        for bad in ["", "x'; DROP TABLE documents; --", "a b", &"x".repeat(51)] {
            assert!(DocumentId::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_replace_script_locks_deletes_then_imports() {
        let id = DocumentId::parse("aaev_019").unwrap();
        let script = replace_script(&id, "BEGIN;\nINSERT INTO x VALUES (1);\nCOMMIT;");
        let lock = script
            .find("pg_advisory_lock(hashtext('u2s-redacto-verify:aaev_019'))")
            .unwrap();
        let delete_doc = script
            .find("DELETE FROM app_redacto.documents WHERE document_id = 'aaev_019'")
            .unwrap();
        let dump = script.find("INSERT INTO x").unwrap();
        assert!(lock < delete_doc && delete_doc < dump, "{script}");
        // Assets are collected before their ownerships are deleted, and an
        // asset another document still owns is kept.
        let collect = script
            .find("CREATE TEMP TABLE u2s_previous_assets")
            .unwrap();
        let delete_ownerships = script.find("DELETE FROM app_redacto.ownerships").unwrap();
        assert!(collect < delete_ownerships, "{script}");
        assert!(script.contains("AND asset_id NOT IN ("), "{script}");
    }

    #[test]
    fn counts_are_scoped_to_the_document_and_parsed() {
        let id = DocumentId::parse("aaev_019").unwrap();
        let query = count_query(&id);
        assert_eq!(query.matches("'aaev_019'").count(), 5, "{query}");
        let parsed = parse_counts("documents|1\nassets|18\n\nnoise\nrelations|18\n");
        assert_eq!(parsed.get("documents"), Some(&1));
        assert_eq!(parsed.get("assets"), Some(&18));
        assert_eq!(parsed.len(), 3);
        assert_eq!(
            ImportReport {
                table_counts: parsed
            }
            .total_rows(),
            37
        );
    }
}
