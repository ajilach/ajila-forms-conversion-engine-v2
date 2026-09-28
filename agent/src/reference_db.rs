//! Shared schema and helpers for the **reference-form database**.
//!
//! Reference forms (a worked example: the original input PDF + the final AEM
//! package + an LLM-written description) are stored in SQLite. Two crates touch
//! the same tables and must never drift:
//!
//! * the **app** (`blueprint-app`) — stores references in its `history.db`,
//!   searches them on behalf of the LLM, and imports/exports dataset files;
//! * the **`reference-builder`** crate — produces a distributable dataset file
//!   offline.
//!
//! To keep them in lockstep this module owns the single source of truth: the
//! `CREATE TABLE` text ([`SCHEMA_SQL`]) and the `f32`↔BLOB conversions used for
//! the description embeddings ([`vec_to_blob`] / [`blob_to_vec`]).
//!
//! Schema (one row per reference form, keyed by the input-file hash):
//! - `references_(ref_id PK, profile, label, description, description_embedding,
//!   model_version, created_at)` — `ref_id` is `document_hash(input PDF)`.
//!   `profile` is an attribute (indexed), not part of the key, so a given form
//!   is one reference deduplicated by content.
//! - `reference_pdfs(ref_id, pdf_index, page_count, pdf_bytes)` — the source
//!   PDF(s), keyed by `ref_id` only.
//! - `reference_files(ref_id, path, content)` — the unzipped AEM package text
//!   files (the `.content.xml` etc.), for literal search and reading.
//! - `reference_docs(doc_id PK, profile, label, content, created_at)` — plain
//!   reference documentation (`.txt`/`.md`), keyed by a content hash. Stored and
//!   imported/exported alongside reference forms, but otherwise independent.

/// The complete schema for the reference tables. Idempotent (`IF NOT EXISTS`),
/// so it can be applied to a fresh export file or merged into an existing DB.
pub const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS references_ (
    ref_id                TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    label                 TEXT NOT NULL,
    description           TEXT NOT NULL,
    description_embedding BLOB NOT NULL,
    model_version         TEXT NOT NULL,
    created_at            TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_references_profile ON references_(profile);
CREATE TABLE IF NOT EXISTS reference_pdfs (
    ref_id     TEXT NOT NULL,
    pdf_index  INTEGER NOT NULL,
    page_count INTEGER NOT NULL,
    pdf_bytes  BLOB NOT NULL,
    PRIMARY KEY (ref_id, pdf_index)
);
CREATE TABLE IF NOT EXISTS reference_files (
    ref_id  TEXT NOT NULL,
    path    TEXT NOT NULL,
    content TEXT NOT NULL,
    PRIMARY KEY (ref_id, path)
);
CREATE TABLE IF NOT EXISTS reference_docs (
    doc_id     TEXT PRIMARY KEY,
    profile    TEXT NOT NULL,
    label      TEXT NOT NULL,
    content    TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reference_docs_profile ON reference_docs(profile);";

/// Identifier for the embedding model that produced `description_embedding`.
///
/// Stored per reference so the app can detect a model change on import and
/// recompute embeddings instead of comparing vectors from different models.
/// Bump this whenever the embedded model in [`crate::semantic`] changes.
pub const EMBEDDING_MODEL_VERSION: &str = "paraphrase-multilingual-MiniLM-L12-v2";

/// Encode an embedding as little-endian `f32` bytes for a SQLite BLOB.
pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian `f32` BLOB back into an embedding. Trailing bytes
/// that don't form a full `f32` are ignored (defensive against truncation).
pub fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trip() {
        let v = vec![0.0_f32, 1.5, -2.25, 3.125, f32::MIN, f32::MAX];
        let blob = vec_to_blob(&v);
        assert_eq!(blob.len(), v.len() * 4);
        assert_eq!(blob_to_vec(&blob), v);
    }

    #[test]
    fn blob_to_vec_ignores_trailing_partial() {
        let mut blob = vec_to_blob(&[1.0, 2.0]);
        blob.push(0x42); // stray byte
        assert_eq!(blob_to_vec(&blob), vec![1.0, 2.0]);
    }
}
