//! How a render tool names the document it reads, shared by every document
//! server so they address documents the same way (AGENTS.md).
//!
//! Two ways, never both:
//!
//! - `doc`: the handle of a stored blob. What the u2s stack passes, since it
//!   keeps every file in its blob store (Postgres) rather than on a disk the
//!   tool could open. The blob is materialized to a file for the renderer.
//! - `doc_path`: a path on the server's own machine. What a standalone
//!   client (Claude Code, a script) passes for a local PDF.

use crate::blob::BlobStore;
use crate::error::RenderError;

/// The argument naming a stored document by blob handle.
pub const DOC_ARG: &str = "doc";

/// Which document a call names, read from its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentArg {
    Blob(String),
    Path(String),
}

impl DocumentArg {
    /// The document a call's `doc` and `doc_path` name, or `None` when it
    /// names none (the XFA tools may name a session instead). Naming both is
    /// refused rather than resolved by precedence, since the two could name
    /// different documents.
    pub fn from_parts(
        doc: Option<String>,
        doc_path: Option<String>,
    ) -> Result<Option<DocumentArg>, RenderError> {
        match (doc, doc_path) {
            (Some(_), Some(_)) => Err(RenderError::invalid_argument(
                DOC_ARG,
                "both doc and doc_path were given; supply exactly one",
            )),
            (Some(handle), None) => Ok(Some(DocumentArg::Blob(handle))),
            (None, Some(path)) => Ok(Some(DocumentArg::Path(path))),
            (None, None) => Ok(None),
        }
    }

    /// [`DocumentArg::from_parts`], required: for a tool with no other way
    /// to address a document.
    pub fn required(doc: Option<String>, doc_path: Option<String>) -> Result<DocumentArg, RenderError> {
        Self::from_parts(doc, doc_path)?.ok_or_else(|| {
            RenderError::invalid_argument(
                DOC_ARG,
                "neither doc nor doc_path was given; supply exactly one",
            )
        })
    }

    /// The file a renderer opens: the path as given, or the blob
    /// materialized from `blobs`.
    pub fn path(&self, blobs: &BlobStore) -> Result<String, RenderError> {
        match self {
            DocumentArg::Path(path) => Ok(path.clone()),
            DocumentArg::Blob(handle) => blobs
                .materialize(handle)
                .map(|path| path.display().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_names_its_document_by_handle_or_path_never_both() {
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(
            DocumentArg::from_parts(some("h.pdf"), None).expect("doc"),
            Some(DocumentArg::Blob("h.pdf".to_owned()))
        );
        assert_eq!(
            DocumentArg::from_parts(None, some("/f.pdf")).expect("doc_path"),
            Some(DocumentArg::Path("/f.pdf".to_owned()))
        );
        assert_eq!(DocumentArg::from_parts(None, None).expect("none"), None);
        assert!(DocumentArg::from_parts(some("h.pdf"), some("/f.pdf")).is_err());
        assert!(DocumentArg::required(None, None).is_err());
    }

    #[test]
    fn a_stored_document_is_opened_from_its_materialized_file() {
        let dir = std::env::temp_dir().join(format!("u2s-render-core-doc-{}", std::process::id()));
        let blobs = BlobStore::in_folder(&dir);
        let blob = blobs
            .put(b"%PDF-1.7", "application/pdf", "pdf")
            .expect("put");
        let path = DocumentArg::Blob(blob.handle)
            .path(&blobs)
            .expect("materialize");
        assert_eq!(std::fs::read(path).expect("read"), b"%PDF-1.7");
        let _ = std::fs::remove_dir_all(dir);
    }
}
