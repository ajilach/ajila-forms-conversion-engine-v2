//! The agent's face of the reference store, which lives in `references-mcp`:
//! this module connects it to `history.db` through [`crate::db::open`], so the
//! store shares the history's path, busy timeout and schema.

use std::sync::Arc;

pub use references_mcp::{
    ReferenceDocInfo, ReferenceInfo, ReferenceStore, SearchHit, compute_doc_id, compute_ref_id,
    pdf_page_count, unzip_package,
};

/// The reference store over `history.db`.
pub fn store() -> ReferenceStore {
    ReferenceStore::new(Arc::new(|| crate::db::open().map_err(|e| e.to_string())))
}
