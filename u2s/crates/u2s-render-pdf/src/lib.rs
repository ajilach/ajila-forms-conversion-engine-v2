//! True per-page PDF rasterization on pdfium.
//!
//! This crate exists because reconstruction is not rendering: an approach that
//! redraws text runs and rectangles from the content stream drops image
//! XObjects, curves, clipping and transparency. That is adequate input for
//! form-structure analysis and the wrong answer to "show me this page".
//!
//! XFA forms are explicitly **not** this crate's job. pdfium renders them as a
//! static "please update your reader" shim, which looks like success —
//! [`DocumentInfo::form_type`] is the routing signal, and every render of such
//! a document carries a warning naming the correct renderer.

mod error;
mod types;
mod worker;

pub use types::{DocumentInfo, FormType};
pub use worker::Renderer;

// Re-exported so a caller needs one dependency, not two.
pub use u2s_render_core::{
    BlobRef, BlobStore, ImageFormat, Limits, PageGeometry, PageText, Pattern, RectPt, RenderError,
    RenderedPage, RenderedPages, TextMatch, TextSearch,
};
