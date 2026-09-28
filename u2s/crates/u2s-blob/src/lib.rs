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
    pub path: PathBuf,
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
}

pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    /// `U2S_BLOB_DIR`, or the system temp dir when unset so standalone use
    /// still works.
    pub fn from_env() -> Self {
        let dir = std::env::var("U2S_BLOB_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("u2s-blobs"));
        Self::new(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Content-addressed: the same bytes written twice yield one file and one
    /// handle.
    pub fn put(
        &self,
        data: &[u8],
        media_type: &str,
        extension: &str,
    ) -> Result<BlobRef, BlobError> {
        std::fs::create_dir_all(&self.dir).map_err(|source| BlobError::CreateDir {
            path: self.dir.display().to_string(),
            source,
        })?;

        let digest = hex(&Sha256::digest(data));
        let handle = format!("{digest}.{extension}");
        let path = self.dir.join(&handle);
        if !path.exists() {
            std::fs::write(&path, data).map_err(|source| BlobError::Write {
                path: path.display().to_string(),
                source,
            })?;
        }

        Ok(BlobRef {
            handle,
            path,
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
        let handle = BlobHandle::parse(raw_handle).map_err(|source| BlobError::InvalidHandle {
            handle: raw_handle.to_owned(),
            source,
        })?;

        let path = self.dir.join(handle.to_string());
        let data = std::fs::read(&path).map_err(|source| BlobError::Read {
            path: path.display().to_string(),
            source,
        })?;

        let actual = hex(&Sha256::digest(&data));
        if actual != handle.digest() {
            return Err(BlobError::DigestMismatch {
                handle: handle.to_string(),
            });
        }
        Ok(data)
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

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
        (BlobStore::new(&dir), dir)
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
        let (store, _dir) = store();
        let put = store.put(b"original", "text/plain", "txt").expect("put");
        std::fs::write(&put.path, b"tampered").expect("overwrite");

        let err = store.get(&put.handle).expect_err("digest must not match");
        assert!(matches!(err, BlobError::DigestMismatch { .. }));
    }

    #[test]
    fn get_of_a_never_written_handle_is_a_read_error_not_a_panic() {
        let (store, _dir) = store();
        let handle = format!("{}.txt", "a".repeat(64));
        assert!(matches!(store.get(&handle), Err(BlobError::Read { .. })));
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
