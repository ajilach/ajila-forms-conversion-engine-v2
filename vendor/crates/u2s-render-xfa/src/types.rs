//! XFA-specific wire types. Everything format-agnostic comes from
//! `u2s-render-core`, so the `xfa_*` tool responses match the `pdf_*` ones
//! field for field wherever they mean the same thing.

use serde::{Deserialize, Serialize};
use u2s_render_core::{PageGeometry, RectPt};
use u2s_xfa::Fidelity;
use u2s_xfa::states::ControlPosition;

/// What a document turned out to be. The mirror of the PDF renderer's
/// `form_type`: each server recognises the other's input and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// An XFA form — this renderer's job.
    Xfa,
    /// A PDF with no XFA. pdfium renders these properly; this engine cannot
    /// render them at all.
    NotXfa,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentInfo {
    pub kind: DocumentKind,
    pub page_count: u32,
    pub pages: Vec<PageGeometry>,
    /// The document's language, as the engine detects it — the declared locale
    /// cross-checked against the actual text.
    pub language: String,
    /// How faithfully the default state could be produced.
    pub fidelity: Fidelity,
    /// The XFA packets present, by name.
    pub packets: Vec<String>,
    /// Set when the caller should be using a different renderer, or when
    /// fidelity had to degrade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// A control's box as [`crate::Renderer::render_region`] takes one. A free
/// function, not `impl From`, since both `ControlPosition` and `RectPt` are
/// foreign to this crate. The page number travels separately — it's
/// `render_region`'s own `page` argument, not part of the rect.
pub fn rect_of(p: &ControlPosition) -> RectPt {
    RectPt {
        x: p.x,
        y: p.y,
        width: p.width,
        height: p.height,
    }
}
