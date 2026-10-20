//! Content-addressed blob handles — PLAN.md's "large payloads never enter a
//! prompt": rendered pages, tens of megabytes of XFA, a FileVault ZIP all
//! travel as a `{ handle, media_type, byte_len, digest }` the model sees, while
//! u2s dereferences the bytes out of band.
//!
//! Promoted out of `u2s-render-core`, which needed no app-level access to a
//! blob's *contents* — only to produce handles. The app does: the review page
//! serves back a rendered page, and later a FileVault package. A render crate
//! is the wrong place to depend from for that, so this crate holds no
//! rendering knowledge at all, and `u2s-render-core::blob` re-exports it
//! unchanged so the three existing servers need no change.
//!
//! Content-addressing is what makes a handle **self-verifying**: [`get`]
//! re-hashes what it read and refuses a mismatch, so serving a blob over HTTP
//! needs no separate authorization table beyond "this run produced this
//! handle" — a forged or corrupted handle simply does not open.
//!
//! ## Backends
//!
//! A store keeps its blobs in one of two places, chosen once at startup
//! ([`BlobStore::from_env`]):
//!
//! - **Postgres** (the `postgres` feature, `DATABASE_URL`): the `blobs` table
//!   the u2s stack migrates. This is where the stack keeps every file, so a
//!   database backup is a complete backup.
//! - **A folder** (`U2S_BLOB_DIR`): one file per blob. What an MCP server
//!   uses standalone, outside the stack, and what a vendoring consumer that
//!   leaves the `postgres` feature off gets.
//!
//! Tools that can only open a file (pdfium, the XFA renderer) ask for one
//! with [`BlobStore::materialize`]: the folder backend hands back the blob's
//! own file, the Postgres backend writes the bytes once into a cache folder.
//!
//! [`get`]: BlobStore::get

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A `<sha256hex>.<ext>` handle, parsed and validated once so a caller that
/// only has a string (from a tool call, a URL path segment) cannot construct
/// a value this crate would then have to re-validate on every use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHandle {
    digest: String,
    extension: String,
}

/// Why a string is not a valid [`BlobHandle`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandleError {
    #[error("missing a '.<extension>' suffix")]
    NoExtension,
    #[error("extension must not be empty")]
    EmptyExtension,
    #[error("digest must be 64 lowercase hex characters, got {0:?}")]
    NotSha256Hex(String),
    #[error("extension must be alphanumeric, got {0:?}")]
    BadExtension(String),
}

impl BlobHandle {
    /// Parses `"<64 lowercase hex chars>.<ext>"` — exactly what
    /// [`BlobStore::put`] produces, so a value round-trips through
    /// `to_string`/`parse` without loss.
    pub fn parse(raw: &str) -> Result<Self, HandleError> {
        let (digest, extension) = raw.split_once('.').ok_or(HandleError::NoExtension)?;
        if extension.is_empty() {
            return Err(HandleError::EmptyExtension);
        }
        // The extension is validated, not just non-empty, and that is the
        // whole reason `BlobStore::get` can call a parsed handle
        // path-traversal-safe. `split_once` puts *everything* after the
        // first dot here, so without this an extension of
        // `./../../../etc/hosts` parses cleanly and then escapes the blob
        // directory when `get` joins it onto the store path.
        if !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(HandleError::BadExtension(extension.to_owned()));
        }
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(HandleError::NotSha256Hex(digest.to_owned()));
        }
        Ok(Self {
            digest: digest.to_owned(),
            extension: extension.to_owned(),
        })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn extension(&self) -> &str {
        &self.extension
    }
}

impl std::fmt::Display for BlobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.digest, self.extension)
    }
}

/// What [`BlobStore::put`] hands back: the reference a tool result carries,
/// never the bytes themselves.
#[derive(Debug, Clone)]
pub struct BlobRef {
    /// Kept as the plain `<digest>.<ext>` string, not a [`BlobHandle`]: this
    /// is what goes straight into a tool result's JSON, and every existing
    /// caller already treats it as a string.
    pub handle: String,
    pub media_type: String,
    pub byte_len: usize,
    /// Hex sha256 of the contents — identical to the handle's digest, kept as
    /// its own field because that is the shape every existing caller expects.
    pub digest: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("cannot create {path}: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{handle}: not a valid blob handle: {source}")]
    InvalidHandle {
        handle: String,
        #[source]
        source: HandleError,
    },
    /// The file at a handle's path does not hash to that handle's digest —
    /// corruption, or a handle that was never legitimately produced here.
    #[error("{handle}: content does not match the handle's digest")]
    DigestMismatch { handle: String },
    /// No blob is stored under this handle.
    #[error("{handle}: no such blob")]
    Missing { handle: String },
    /// The backend could not be reached or refused the operation.
    #[error("blob database: {0}")]
    Database(String),
    /// The environment names no usable backend, or two at once.
    #[error("blob store configuration: {0}")]
    Configuration(String),
}

/// Where blobs are kept. See the module doc for the two backends.
pub struct BlobStore {
    backend: Backend,
}

enum Backend {
    Folder(PathBuf),
    /// The pool, and the URL it was opened with, for [`BlobStore::child_env`].
    #[cfg(feature = "postgres")]
    Postgres(postgres::PostgresBlobs, String),
}

/// The environment variable that selects the folder backend.
pub const BLOB_DIR_VAR: &str = "U2S_BLOB_DIR";
/// The environment variable that selects the Postgres backend.
pub const DATABASE_URL_VAR: &str = "DATABASE_URL";

/// Which backend an environment selects, decided by [`backend_choice`]
/// without opening anything, so the rule is testable on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendChoice {
    Folder(PathBuf),
    Postgres(String),
}

/// The backend `DATABASE_URL` and `U2S_BLOB_DIR` select:
///
/// - only `DATABASE_URL`: Postgres (refused when this build has no
///   `postgres` feature, rather than silently writing to a folder);
/// - only `U2S_BLOB_DIR`: that folder;
/// - both: refused, naming both, so the choice is never silent;
/// - neither: a folder under the system temp dir, so standalone use still
///   works with no configuration.
pub fn backend_choice(
    database_url: Option<&str>,
    blob_dir: Option<&str>,
) -> Result<BackendChoice, BlobError> {
    match (database_url, blob_dir) {
        (Some(_), Some(_)) => Err(BlobError::Configuration(format!(
            "both {DATABASE_URL_VAR} and {BLOB_DIR_VAR} are set; set {DATABASE_URL_VAR} to \
             keep blobs in Postgres or {BLOB_DIR_VAR} to keep them in a folder, not both"
        ))),
        (Some(url), None) => Ok(BackendChoice::Postgres(url.to_owned())),
        (None, Some(dir)) => Ok(BackendChoice::Folder(PathBuf::from(dir))),
        (None, None) => Ok(BackendChoice::Folder(
            std::env::temp_dir().join("u2s-blobs"),
        )),
    }
}

impl BlobStore {
    /// A store keeping one file per blob in `dir`.
    pub fn in_folder(dir: impl AsRef<Path>) -> Self {
        Self {
            backend: Backend::Folder(dir.as_ref().to_path_buf()),
        }
    }

    /// A store keeping blobs in the `blobs` table of the database at
    /// `database_url`. Connects lazily: nothing is opened until the first
    /// blob is read or written.
    #[cfg(feature = "postgres")]
    pub fn in_postgres(database_url: &str) -> Result<Self, BlobError> {
        Ok(Self {
            backend: Backend::Postgres(
                postgres::PostgresBlobs::connect_lazy(database_url)?,
                database_url.to_owned(),
            ),
        })
    }

    /// The store [`backend_choice`] selects from `DATABASE_URL` and
    /// `U2S_BLOB_DIR`. An empty variable counts as unset.
    pub fn from_env() -> Result<Self, BlobError> {
        let var = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
        let database_url = var(DATABASE_URL_VAR);
        let blob_dir = var(BLOB_DIR_VAR);
        Self::from_choice(backend_choice(
            database_url.as_deref(),
            blob_dir.as_deref(),
        )?)
    }

    fn from_choice(choice: BackendChoice) -> Result<Self, BlobError> {
        match choice {
            BackendChoice::Folder(dir) => Ok(Self::in_folder(dir)),
            #[cfg(feature = "postgres")]
            BackendChoice::Postgres(url) => Self::in_postgres(&url),
            #[cfg(not(feature = "postgres"))]
            BackendChoice::Postgres(_) => Err(BlobError::Configuration(format!(
                "{DATABASE_URL_VAR} is set, but this build keeps blobs in a folder only (the \
                 `postgres` feature of u2s-blob is off); unset it or set {BLOB_DIR_VAR}"
            ))),
        }
    }

    /// The environment a child process needs to use this same store, through
    /// its own [`BlobStore::from_env`]: `DATABASE_URL` for Postgres,
    /// `U2S_BLOB_DIR` for a folder, and the other one set empty, which
    /// `from_env` reads as unset. Set on every MCP server a host starts, so a
    /// handle the server writes is one the host can read, and an inherited
    /// value of the other variable cannot make the child refuse to start.
    pub fn child_env(&self) -> Vec<(String, String)> {
        match &self.backend {
            Backend::Folder(dir) => vec![
                (BLOB_DIR_VAR.to_owned(), dir.display().to_string()),
                (DATABASE_URL_VAR.to_owned(), String::new()),
            ],
            #[cfg(feature = "postgres")]
            Backend::Postgres(_, url) => vec![
                (DATABASE_URL_VAR.to_owned(), url.clone()),
                (BLOB_DIR_VAR.to_owned(), String::new()),
            ],
        }
    }

    /// Content-addressed: the same bytes written twice yield one blob and one
    /// handle.
    pub fn put(
        &self,
        data: &[u8],
        media_type: &str,
        extension: &str,
    ) -> Result<BlobRef, BlobError> {
        let digest = hex(&Sha256::digest(data));
        let handle = format!("{digest}.{extension}");
        // Parsed so an extension that could not be read back (`get` parses)
        // is refused here, at the write, rather than stored unreadable.
        BlobHandle::parse(&handle).map_err(|source| BlobError::InvalidHandle {
            handle: handle.clone(),
            source,
        })?;
        match &self.backend {
            Backend::Folder(dir) => write_into_folder(dir, &handle, data)?,
            #[cfg(feature = "postgres")]
            Backend::Postgres(blobs, _) => blobs.put(&handle, media_type, data)?,
        }
        Ok(BlobRef {
            handle,
            media_type: media_type.to_owned(),
            byte_len: data.len(),
            digest,
        })
    }

    /// Reads a blob back by handle, verifying the bytes actually hash to the
    /// handle's digest before returning them.
    ///
    /// This is the self-verification the module doc promises: without it, a
    /// path-traversal-safe handle (parsed, so no `..` or absolute path can
    /// hide in it) would still let corrupted or substituted bytes through
    /// silently. `parse` happening here too, not just at the HTTP boundary,
    /// means a caller cannot skip validation by holding the raw string a
    /// moment longer.
    pub fn get(&self, raw_handle: &str) -> Result<Vec<u8>, BlobError> {
        let handle = parse_handle(raw_handle)?;
        let data = match &self.backend {
            Backend::Folder(dir) => read_from_folder(dir, &handle)?,
            #[cfg(feature = "postgres")]
            Backend::Postgres(blobs, _) => blobs.get(&handle)?,
        };
        verify(&handle, data)
    }

    /// A file holding the blob's bytes, for a tool that can only open a path
    /// (pdfium, the XFA renderer). The folder backend returns the blob's own
    /// file; the Postgres backend writes the bytes once into a cache folder
    /// under the system temp dir and returns that file. The cache needs no
    /// backup and no cleanup beyond the temp dir's own: a handle names its
    /// content, so a cached file can never go stale.
    pub fn materialize(&self, raw_handle: &str) -> Result<PathBuf, BlobError> {
        let handle = parse_handle(raw_handle)?;
        match &self.backend {
            Backend::Folder(dir) => {
                let path = dir.join(handle.to_string());
                if path.is_file() {
                    Ok(path)
                } else {
                    Err(BlobError::Missing {
                        handle: handle.to_string(),
                    })
                }
            }
            #[cfg(feature = "postgres")]
            Backend::Postgres(blobs, _) => {
                let cache = materialize_cache_dir();
                let path = cache.join(handle.to_string());
                if !path.is_file() {
                    let data = verify(&handle, blobs.get(&handle)?)?;
                    write_into_folder(&cache, &handle.to_string(), &data)?;
                }
                Ok(path)
            }
        }
    }
}

/// Where the Postgres backend materializes blobs. Shared by every process
/// on the machine, which is safe because a file is only ever written whole
/// under its own content's name.
#[cfg(feature = "postgres")]
fn materialize_cache_dir() -> PathBuf {
    std::env::temp_dir().join("u2s-blob-cache")
}

fn parse_handle(raw_handle: &str) -> Result<BlobHandle, BlobError> {
    BlobHandle::parse(raw_handle).map_err(|source| BlobError::InvalidHandle {
        handle: raw_handle.to_owned(),
        source,
    })
}

fn verify(handle: &BlobHandle, data: Vec<u8>) -> Result<Vec<u8>, BlobError> {
    if hex(&Sha256::digest(&data)) != handle.digest() {
        return Err(BlobError::DigestMismatch {
            handle: handle.to_string(),
        });
    }
    Ok(data)
}

/// Makes each staging file name unique within the process; the pid makes it
/// unique across processes sharing the directory.
static STAGING_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `data` to `dir/handle` unless that file already exists.
fn write_into_folder(dir: &Path, handle: &str, data: &[u8]) -> Result<(), BlobError> {
    std::fs::create_dir_all(dir).map_err(|source| BlobError::CreateDir {
        path: dir.display().to_string(),
        source,
    })?;
    let path = dir.join(handle);
    if path.exists() {
        return Ok(());
    }
    // Written aside and renamed into place, so the handle's path only ever
    // holds the whole blob. Written in place, a crash mid-write would leave a
    // truncated file that every later `put` of the same bytes skips, since
    // the path then exists, and a concurrent `get` could read half a blob.
    let staging = dir.join(format!(
        ".{handle}.{}.{}.tmp",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let write_err = |source| BlobError::Write {
        path: path.display().to_string(),
        source,
    };
    std::fs::write(&staging, data).map_err(write_err)?;
    if let Err(source) = std::fs::rename(&staging, &path) {
        let _ = std::fs::remove_file(&staging);
        return Err(write_err(source));
    }
    Ok(())
}

fn read_from_folder(dir: &Path, handle: &BlobHandle) -> Result<Vec<u8>, BlobError> {
    let path = dir.join(handle.to_string());
    std::fs::read(&path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => BlobError::Missing {
            handle: handle.to_string(),
        },
        _ => BlobError::Read {
            path: path.display().to_string(),
            source,
        },
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(feature = "postgres")]
mod postgres;
mod source;
pub use source::{BlobOrPath, Source, SourceArgs};

#[cfg(test)]
mod tests {

    /// `BlobStore::get`'s docs call a parsed handle path-traversal-safe.
    /// That is only true because the extension is validated: everything
    /// after the first dot lands there, so a handle whose "extension" is a
    /// relative path would escape the store directory when `get` joins it.
    #[test]
    fn a_traversal_hidden_in_the_extension_is_refused() {
        for raw in [
            "0000000000000000000000000000000000000000000000000000000000000000./../../etc/hosts",
            "0000000000000000000000000000000000000000000000000000000000000000.pdf/../../x",
            "0000000000000000000000000000000000000000000000000000000000000000.a\\b",
        ] {
            assert!(
                BlobHandle::parse(raw).is_err(),
                "must refuse a traversal in the extension: {raw:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_extension_still_parses() {
        let raw = "0000000000000000000000000000000000000000000000000000000000000000.pdf";
        let handle = BlobHandle::parse(raw).expect("a real handle must parse");
        assert_eq!(handle.extension(), "pdf");
        assert_eq!(handle.to_string(), raw);
    }
    use super::*;

    fn store() -> (BlobStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "u2s-blob-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (BlobStore::in_folder(&dir), dir)
    }

    #[test]
    fn put_is_content_addressed() {
        let (store, _dir) = store();
        let a = store.put(b"hello", "text/plain", "txt").expect("put");
        let b = store.put(b"hello", "text/plain", "txt").expect("put");
        assert_eq!(a.handle, b.handle, "identical bytes yield one handle");
        assert_eq!(a.digest, b.digest);
    }

    #[test]
    fn different_bytes_yield_different_handles() {
        let (store, _dir) = store();
        let a = store.put(b"hello", "text/plain", "txt").expect("put");
        let b = store.put(b"world", "text/plain", "txt").expect("put");
        assert_ne!(a.handle, b.handle);
    }

    #[test]
    fn a_put_blob_reads_back_identical() {
        let (store, _dir) = store();
        let put = store.put(b"round trip", "text/plain", "txt").expect("put");
        let read = store.get(&put.handle).expect("get");
        assert_eq!(read, b"round trip");
    }

    #[test]
    fn get_refuses_a_malformed_handle_without_touching_disk() {
        let (store, _dir) = store();
        assert!(matches!(
            store.get("not-a-handle"),
            Err(BlobError::InvalidHandle { .. })
        ));
        assert!(matches!(
            store.get("../../etc/passwd.txt"),
            Err(BlobError::InvalidHandle { .. })
        ));
    }

    #[test]
    fn get_refuses_content_that_does_not_match_the_digest() {
        let (store, dir) = store();
        let put = store.put(b"original", "text/plain", "txt").expect("put");
        std::fs::write(dir.join(&put.handle), b"tampered").expect("overwrite");

        let err = store.get(&put.handle).expect_err("digest must not match");
        assert!(matches!(err, BlobError::DigestMismatch { .. }));
    }

    #[test]
    fn get_of_a_never_written_handle_is_missing_not_a_panic() {
        let (store, _dir) = store();
        let handle = format!("{}.txt", "a".repeat(64));
        assert!(matches!(store.get(&handle), Err(BlobError::Missing { .. })));
        assert!(matches!(
            store.materialize(&handle),
            Err(BlobError::Missing { .. })
        ));
    }

    #[test]
    fn a_folder_blob_materializes_as_its_own_file() {
        let (store, dir) = store();
        let put = store
            .put(b"%PDF-1.7", "application/pdf", "pdf")
            .expect("put");
        let path = store.materialize(&put.handle).expect("materialize");
        assert_eq!(path, dir.join(&put.handle));
        assert_eq!(std::fs::read(path).expect("read"), b"%PDF-1.7");
    }

    #[test]
    fn an_extension_that_could_not_be_read_back_is_refused_at_the_write() {
        let (store, _dir) = store();
        assert!(matches!(
            store.put(b"x", "text/plain", "t/xt"),
            Err(BlobError::InvalidHandle { .. })
        ));
    }

    /// The backend is chosen by which variable is set, and two at once is
    /// refused rather than resolved by precedence.
    #[test]
    fn the_environment_selects_exactly_one_backend() {
        assert_eq!(
            backend_choice(Some("postgres://db"), None).expect("postgres"),
            BackendChoice::Postgres("postgres://db".to_owned())
        );
        assert_eq!(
            backend_choice(None, Some("/blobs")).expect("folder"),
            BackendChoice::Folder(PathBuf::from("/blobs"))
        );
        let both =
            backend_choice(Some("postgres://db"), Some("/blobs")).expect_err("both is ambiguous");
        assert!(
            both.to_string().contains(DATABASE_URL_VAR) && both.to_string().contains(BLOB_DIR_VAR),
            "{both}"
        );
        let child = BlobStore::in_folder("/blobs").child_env();
        assert_eq!(
            child,
            vec![
                (BLOB_DIR_VAR.to_owned(), "/blobs".to_owned()),
                (DATABASE_URL_VAR.to_owned(), String::new()),
            ],
            "a child is told the store and has the other variable emptied"
        );
        assert_eq!(
            backend_choice(None, None).expect("default"),
            BackendChoice::Folder(std::env::temp_dir().join("u2s-blobs"))
        );
    }

    #[test]
    fn handle_parses_and_round_trips_through_display() {
        let raw = format!("{}.jpg", "f".repeat(64));
        let handle = BlobHandle::parse(&raw).expect("parses");
        assert_eq!(handle.to_string(), raw);
        assert_eq!(handle.extension(), "jpg");
    }

    #[test]
    fn handle_rejects_wrong_length_and_uppercase_and_missing_extension() {
        assert!(matches!(
            BlobHandle::parse("deadbeef.txt"),
            Err(HandleError::NotSha256Hex(_))
        ));
        assert!(matches!(
            BlobHandle::parse(&format!("{}.txt", "A".repeat(64))),
            Err(HandleError::NotSha256Hex(_))
        ));
        assert!(matches!(
            BlobHandle::parse(&"a".repeat(64)),
            Err(HandleError::NoExtension)
        ));
        assert!(matches!(
            BlobHandle::parse(&format!("{}.", "a".repeat(64))),
            Err(HandleError::EmptyExtension)
        ));
    }
}
