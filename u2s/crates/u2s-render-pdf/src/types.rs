//! PDF-specific wire types. Everything format-agnostic lives in
//! `u2s-render-core`.

use serde::{Deserialize, Serialize};
use u2s_render_core::PageGeometry;

/// pdfium's form-type classification. This is the routing signal: an XFA
/// document rendered here yields the "please update your reader" shim page,
/// which *looks* like a successful render. Callers must send those to the XFA
/// renderer instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormType {
    None,
    Acroform,
    XfaFull,
    XfaForeground,
}

impl FormType {
    /// True when rendering this document with pdfium is likely to produce a
    /// shim page rather than the form.
    pub fn is_xfa(&self) -> bool {
        matches!(self, FormType::XfaFull | FormType::XfaForeground)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentInfo {
    pub page_count: u32,
    pub pages: Vec<PageGeometry>,
    /// True when `pages` was capped; ask for specific pages instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pages_truncated: bool,
    pub form_type: FormType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// Set when the document is an XFA form; names the correct renderer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
