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
    /// A store keeping one file per blob in `dir`.
    pub fn in_folder(dir: impl AsRef<std::path::Path>) -> Self {
        Self(u2s_blob::BlobStore::in_folder(dir))
    }

    /// The store `DATABASE_URL` or `U2S_BLOB_DIR` selects; see
    /// [`u2s_blob::backend_choice`]. A server refuses to start on an error.
    pub fn from_env() -> Result<Self, RenderError> {
        u2s_blob::BlobStore::from_env()
            .map(Self)
            .map_err(|e| RenderError::Blob(e.to_string()))
    }

    /// Content-addressed: the same bytes written twice yield one blob and
    /// one handle.
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

    /// A file holding the blob's bytes, for a renderer that opens a path.
    pub fn materialize(&self, handle: &str) -> Result<std::path::PathBuf, RenderError> {
        self.0
            .materialize(handle)
            .map_err(|e| RenderError::Blob(e.to_string()))
    }
}
