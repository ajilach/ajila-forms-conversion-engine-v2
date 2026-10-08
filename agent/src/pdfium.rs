//! The pdfium library, embedded in the binary by `build.rs`.
//!
//! pdfium can only be loaded from a file, so the first renderer of a process
//! writes the embedded copy to the user's cache directory, under a directory
//! named after its hash, and points the vendored renderer at it. Every build of
//! the same library shares that one file; a different build gets its own.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use u2s_render_pdf::{BlobStore, Limits, Renderer};
use u2s_render_pdf_mcp::PdfRenderServer;

static LIBRARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pdfium.lib"));
const LIBRARY_NAME: &str = env!("PDFIUM_LIBRARY_NAME");
const LIBRARY_SHA256: &str = env!("PDFIUM_LIBRARY_SHA256");

/// The `pdf_*` tool server, on the embedded pdfium.
pub(crate) fn server(blobs: BlobStore) -> Result<PdfRenderServer, String> {
    ensure()?;
    PdfRenderServer::with_parts(Limits::default(), blobs).map_err(unavailable)
}

/// A bare pdfium renderer, on the embedded pdfium.
pub(crate) fn renderer(limits: Limits) -> Result<Renderer, String> {
    ensure()?;
    Renderer::start(limits).map_err(unavailable)
}

fn unavailable(e: impl std::fmt::Display) -> String {
    format!("the PDF renderer cannot load pdfium ({e})")
}

/// Makes the embedded pdfium the one the renderer binds, once per process.
/// An operator's `PDFIUM_LIB_PATH` still wins: the renderer reads it first,
/// and then nothing is extracted.
fn ensure() -> Result<(), String> {
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            if std::env::var_os("PDFIUM_LIB_PATH").is_some() {
                return Ok(());
            }
            let cache = dirs::cache_dir()
                .ok_or("the user cache directory is unknown, so pdfium cannot be unpacked")?
                .join("ajila-forms-conversion-engine/pdfium")
                .join(LIBRARY_SHA256);
            let path = install(&cache, LIBRARY_NAME, LIBRARY)?;
            u2s_render_pdf::set_library_path(path);
            Ok(())
        })
        .clone()
}

/// Writes `bytes` to `dir/name` unless an identical file is already there, and
/// returns its path. The file appears complete or not at all: it is written
/// beside its final name and renamed into place, so a concurrent process never
/// loads half a library.
fn install(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let path = dir.join(name);
    if fs::read(&path).is_ok_and(|existing| existing == bytes) {
        return Ok(path);
    }
    let fail = |e: std::io::Error| format!("cannot unpack pdfium into {}: {e}", dir.display());
    fs::create_dir_all(dir).map_err(fail)?;
    let mut partial = tempfile::NamedTempFile::new_in(dir).map_err(fail)?;
    partial.write_all(bytes).map_err(fail)?;
    partial.persist(&path).map_err(|e| fail(e.error))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_then_reuses_then_repairs() {
        let dir = tempfile::tempdir().unwrap();
        let path = install(dir.path(), "lib", b"pdfium").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"pdfium");

        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(install(dir.path(), "lib", b"pdfium").unwrap(), path);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);

        fs::write(&path, b"truncat").unwrap();
        install(dir.path(), "lib", b"pdfium").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"pdfium");
    }

    #[test]
    fn embedded_library_is_the_pinned_build() {
        assert!(LIBRARY.len() > 1_000_000, "the embedded pdfium is suspiciously small");
        assert_eq!(LIBRARY_SHA256.len(), 64);
    }
}
