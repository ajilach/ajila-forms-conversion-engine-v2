//! Mapping pdfium failures onto the shared taxonomy.

use std::path::Path;

use u2s_render_core::RenderError;

/// Named so `RenderError::Backend` messages say which engine failed.
pub(crate) const ENGINE: &str = "pdfium";

/// Classify a pdfium load failure. Pdfium reports password protection and
/// malformed files as distinct internal errors, and both are things a caller
/// can act on, so they get structured variants rather than a generic backend
/// error.
pub(crate) fn from_load(path: &Path, err: pdfium_render::prelude::PdfiumError) -> RenderError {
    use pdfium_render::prelude::{PdfiumError, PdfiumInternalError};
    let p = path.display().to_string();
    match err {
        PdfiumError::IoError(source) => RenderError::Io { path: p, source },
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            RenderError::Encrypted { path: p }
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError) => {
            RenderError::UnsupportedInput {
                path: p,
                detail: "pdfium reports a format error".into(),
            }
        }
        other => RenderError::backend(ENGINE, format!("{other:?} loading {p}")),
    }
}
