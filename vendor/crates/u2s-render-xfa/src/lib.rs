//! Per-page rendering of XFA forms.
//!
//! pdfium cannot render an XFA form — it draws the "please update your reader"
//! shim — so this wraps the ported layout engine instead. The tool surface
//! mirrors the PDF renderer's exactly, because an agent should not have to
//! learn two vocabularies for the same five operations.
//!
//! The engine lays a document out as one tall column that is a stack of whole
//! pages; "pages" are bands of that column, one page tall each, and per-page
//! rendering is a crop. See [`bands`] for the arithmetic that makes them agree.

pub mod bands;
pub mod renderer;
mod session;
pub mod text;
pub mod types;

pub use renderer::{Renderer, Target};
pub use session::{AccessChange, FieldChange, Interaction, InteractionKind};
pub use types::{DocumentInfo, DocumentKind, rect_of};
pub use u2s_xfa::states::ControlPosition;

// Re-exported so a caller needs one dependency, not three.
pub use u2s_render_core::{
    BlobRef, BlobStore, ImageFormat, Limits, PageGeometry, PageText, Pattern, RectPt, RenderError,
    RenderedPage, RenderedPages, TextMatch, TextSearch,
};
pub use u2s_xfa::{Fidelity, XfaNode, extract_xfa_from_pdf_bytes, fonts, states};
