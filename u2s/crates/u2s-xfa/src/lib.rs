//! XFA form parsing, layout and rasterization.
//!
//! Ported from `ajila-forms-conversion-engine`; see PORTING.md for the upstream
//! revision and every deliberate deviation. The rule is verbatim copy wherever
//! possible, so that a future re-sync has a small diff to reason about.
//!
//! Why this exists at all: pdfium cannot render an XFA form — it draws the
//! "please update your reader" shim. The only way to see the real form without
//! Adobe is to parse the XFA XML, lay it out, and draw it, which is what this
//! engine does.

#![deny(unsafe_code)]
// This crate is a near-verbatim copy of upstream (see PORTING.md), and the rule
// is to keep the diff small so a future re-sync stays tractable. Reformatting
// 16k lines to satisfy style lints would trade that for nothing, so upstream's
// style is allowed rather than corrected. Lints that catch *bugs* stay on.
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::needless_range_loop)]

pub mod barcode;
pub mod corpus;
pub mod exhaustive;
pub mod fidelity;
pub mod flattened;
pub mod fonts;
pub mod query;
pub mod xfa;

pub mod extract;
pub mod states;
mod util;

pub use extract::{
    XfaPacket, extract_xfa_from_pdf, extract_xfa_from_pdf_bytes, extract_xfa_packets,
};
pub use fidelity::{Fidelity, Prepared, prepare_default, prepare_template_only};
pub use flattened::{Flattened, FlattenedNode, FlattenedNodeKind, Page, PageOverrides};
pub use xfa::{Num, XfaNode, XfaNodeKind, num};

/// Errors from parsing and layout. Rendering failures are `String` upstream;
/// the facade crate maps them onto the shared taxonomy.
#[derive(Debug, thiserror::Error)]
pub enum XfaError {
    #[error("cannot read PDF: {0}")]
    Io(#[from] std::io::Error),
    #[error("PDF parse: {0}")]
    PdfParse(String),
    #[error("XFA parse: {0}")]
    XfaParse(String),
    #[error("layout: {0}")]
    Layout(String),
    #[error("render: {0}")]
    Render(String),
    #[error("font: {0}")]
    Font(String),
    /// Building an `XfaForm` failed — used by [`states`] and interaction.
    #[error("form creation: {0}")]
    FormCreation(String),
}

/// Upstream's name for the error type, kept as an alias for the parts of
/// this crate still ported near-verbatim (see PORTING.md).
pub use XfaError as Error;
