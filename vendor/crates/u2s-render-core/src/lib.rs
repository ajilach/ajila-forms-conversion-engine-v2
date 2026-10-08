//! Format-agnostic pieces shared by every u2s page renderer.
//!
//! The `pdf_*` and `xfa_*` tool surfaces are deliberately identical in shape,
//! so everything that does not depend on *how* a page is produced lives here:
//! the clamps and budgets, image encoding, the blob store, the cursor
//! pagination contract, and the wire types.

pub mod batch;
pub mod blob;
pub mod encode;
pub mod error;
pub mod limits;
pub mod text;
pub mod types;

pub use batch::{BatchLimits, render_batch};
pub use blob::{BlobRef, BlobStore};
pub use encode::ImageFormat;
pub use error::RenderError;
pub use limits::Limits;
pub use text::{Grep, GrepMatch, Pattern, Windowed, window_chars};
pub use types::{
    PageGeometry, PageText, RectPt, RenderedPage, RenderedPages, TextMatch, TextSearch,
};
