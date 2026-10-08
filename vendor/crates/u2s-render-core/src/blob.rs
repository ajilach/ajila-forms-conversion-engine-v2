//! Blob handles. **Promoted to [`u2s_blob`]** — the app dereferences a blob's
//! contents (the review page serves back a rendered page), which is app-level
//! access no render crate should require depending on. This module re-exports
//! that crate unchanged so `u2s-render-pdf`, `u2s-render-xfa` and the three
//! `-mcp` binaries need no change; [`BlobStore`] here only translates
//! [`u2s_blob::BlobError`] into this crate's [`RenderError`], which is what
//! every renderer's `Result` already is.

pub use u2s_blob::{BlobError, BlobHandle, BlobRef};

use crate::error::RenderError;

pub struct BlobStore(u2s_blob::BlobStore);

impl BlobStore {
    pub fn new(dir: impl AsRef<std::path::Path>) -> Self {
        Self(u2s_blob::BlobStore::new(dir))
    }

    /// `U2S_BLOB_DIR`, or the system temp dir when unset so standalone use
    /// still works.
    pub fn from_env() -> Self {
        Self(u2s_blob::BlobStore::from_env())
    }

    pub fn dir(&self) -> &std::path::Path {
        self.0.dir()
    }

    /// Content-addressed: the same bytes written twice yield one file and one
    /// handle.
    pub fn put(
        &self,
        data: &[u8],
        media_type: &str,
        extension: &str,
    ) -> Result<BlobRef, RenderError> {
        self.0
            .put(data, media_type, extension)
            .map_err(|e| RenderError::Blob(e.to_string()))
    }

    pub fn get(&self, handle: &str) -> Result<Vec<u8>, RenderError> {
        self.0
            .get(handle)
            .map_err(|e| RenderError::Blob(e.to_string()))
    }
}
