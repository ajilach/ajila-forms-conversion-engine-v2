//! The arguments the document render tools share, as types. Every render
//! server flattens these next to its own way of addressing a document
//! ([`ServerArgs`]), so the PDF and XFA servers take the same arguments for
//! the same tool by construction (AGENTS.md: the same toolset), and each
//! type is both the tool's input schema and what its arguments parse into.
//!
//! A `///` on a field is the description the caller reads: prompt text.
//! Every optional argument refuses an empty or zero value in the schema
//! (`length(min = 1)`, `range(min = ..)`): callers, models in particular,
//! fill in every optional field, and an empty value used to be read as
//! "given" in silent ways (`pages: []` rendered nothing, `max_edge_px: 0`
//! gave a 16 px image). `u2s_mcp::conformance` fails a server whose optional
//! arguments do not.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::encode::ImageFormat;
use crate::error::RenderError;
use crate::limits::{MIN_DPI, MIN_EDGE_PX};
use crate::text::Pattern;
use crate::types::RectPt;

/// A tool's arguments: the server's document address `D` next to the
/// tool's own arguments `T`, both flattened into one object.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ServerArgs<D, T> {
    #[serde(flatten)]
    pub document: D,
    #[serde(flatten)]
    pub args: T,
}

/// `ServerArgs` parsed from a call's arguments. Bounds are left to the
/// schema; what is refused here is a wrong shape or type.
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, RenderError> {
    u2s_schema::parse_tool_args(args).map_err(RenderError::Arguments)
}

/// A PDF named by blob handle or by path, exactly one: how a server that
/// takes no other address names the document a tool reads.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DocumentRef {
    /// The PDF as the handle of a stored blob. Exactly one of doc and doc_path is required.
    #[schemars(length(min = 1))]
    pub doc: Option<String>,
    /// Absolute path to the PDF on the machine running this server. Exactly one of doc and doc_path is required.
    #[schemars(length(min = 1))]
    pub doc_path: Option<String>,
}

impl DocumentRef {
    /// The document named, refusing both and neither.
    pub fn into_document(self) -> Result<crate::document::DocumentArg, RenderError> {
        crate::document::DocumentArg::required(self.doc, self.doc_path)
    }
}

/// The arguments of a tool that reads the whole document.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct InfoArgs {}

/// `*_render_page`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RenderPageArgs {
    /// 1-based page number.
    #[schemars(range(min = 1))]
    pub page: u32,
    /// Target resolution; default 144. Leave out for the default.
    #[schemars(range(min = MIN_DPI))]
    pub dpi: Option<f32>,
    /// Default jpeg.
    pub format: Option<ImageFormat>,
    /// Long-edge clamp; default 2576. Leave out for the default.
    #[schemars(range(min = MIN_EDGE_PX))]
    pub max_edge_px: Option<u32>,
}

/// `*_render_pages`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RenderPagesArgs {
    /// Exact pages, in the order you want them. Leave out to use from and limit.
    #[schemars(length(min = 1), inner(range(min = 1)))]
    pub pages: Option<Vec<u32>>,
    /// Window start; default 1.
    #[schemars(range(min = 1))]
    pub from: Option<u32>,
    /// Max images this call.
    #[schemars(range(min = 1))]
    pub limit: Option<usize>,
    /// Target resolution; default 144. Leave out for the default.
    #[schemars(range(min = MIN_DPI))]
    pub dpi: Option<f32>,
    pub format: Option<ImageFormat>,
    /// Long-edge clamp; default 2576. Leave out for the default.
    #[schemars(range(min = MIN_EDGE_PX))]
    pub max_edge_px: Option<u32>,
}

/// `*_render_region`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RenderRegionArgs {
    #[schemars(range(min = 1))]
    pub page: u32,
    /// Region in points, origin top-left of the page.
    pub rect_pt: RectPt,
    /// Target resolution; default 300. Leave out for the default.
    #[schemars(range(min = MIN_DPI))]
    pub dpi: Option<f32>,
    pub format: Option<ImageFormat>,
}

/// `*_page_text`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PageTextArgs {
    #[schemars(range(min = 1))]
    pub page: u32,
    /// Character offset; default 0.
    pub offset: Option<usize>,
    /// Characters to return; default 4000.
    #[schemars(range(min = 1))]
    pub limit: Option<usize>,
}

/// `*_search_text`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchTextArgs {
    /// Substring, or regex pattern when regex is true.
    pub query: String,
    /// Treat query as a regex; default false. A literal is case-insensitive; a regex is not, so write (?i) if you want that.
    #[serde(default)]
    pub regex: bool,
    /// First page to scan; default 1.
    #[schemars(range(min = 1))]
    pub from: Option<u32>,
    /// Max matches returned; default 50.
    #[schemars(range(min = 1))]
    pub limit: Option<usize>,
}

impl SearchTextArgs {
    /// The query compiled once, here at the edge, so nothing downstream can
    /// hold an invalid pattern. A bad regex is the caller's fault, so it is
    /// reported as a bad argument rather than as an engine failure.
    pub fn pattern(&self) -> Result<Pattern, RenderError> {
        Pattern::parse(&self.query, self.regex)
            .map_err(|detail| RenderError::invalid_argument("query", detail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct Doc {
        doc: Option<String>,
    }

    #[test]
    fn a_tools_arguments_are_one_flat_object() {
        let schema = u2s_schema::tool_input_schema::<ServerArgs<Doc, RenderPageArgs>>();
        assert_eq!(
            u2s_schema::tool_schema_violations(&schema),
            Vec::<String>::new()
        );
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["page"]));
        for name in ["doc", "page", "dpi", "format", "max_edge_px"] {
            assert!(schema["properties"].get(name).is_some(), "{name}: {schema}");
        }
        assert_eq!(
            schema["properties"]["format"]["enum"],
            json!(["jpeg", "png"])
        );
    }

    #[test]
    fn arguments_parse_through_both_flattened_halves() {
        let parsed: ServerArgs<Doc, RenderRegionArgs> = parse_args(&json!({
            "doc": "h.pdf",
            "page": 2,
            "rect_pt": { "x": 0, "y": 10, "width": 100.5, "height": 20 },
            "format": "jpg",
            // Injected by the transport; never part of the schema.
            "instance_id": "i"
        }))
        .unwrap();
        assert_eq!(parsed.document.doc.as_deref(), Some("h.pdf"));
        assert_eq!(parsed.args.page, 2);
        assert_eq!(parsed.args.rect_pt.width, 100.5);
        assert_eq!(parsed.args.format, Some(ImageFormat::Jpeg));
    }

    #[test]
    fn a_wrong_type_is_an_argument_error_naming_the_problem() {
        let err =
            parse_args::<ServerArgs<Doc, PageTextArgs>>(&json!({ "page": "one" })).unwrap_err();
        assert!(matches!(err, RenderError::Arguments(_)), "{err:?}");
        let err =
            parse_args::<ServerArgs<Doc, RenderPageArgs>>(&json!({ "page": 1, "format": "gif" }))
                .unwrap_err();
        assert!(err.to_string().contains("gif"), "{err}");
    }
}
