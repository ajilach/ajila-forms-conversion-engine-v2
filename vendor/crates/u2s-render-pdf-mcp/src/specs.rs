//! Tool specifications and the `u2s.manifest` resource.
//!
//! Schemas are plain `serde_json::Value` rather than `schemars` derives. That
//! decouples them from the rmcp and schemars versions entirely, and it keeps
//! the tool descriptions — which are prompt surface, so a wording change is a
//! behaviour change — visible in one file rather than scattered over derives.

use serde_json::{Value, json};

pub const CONTRACT_VERSION: &str = "1.0.0";

fn tool(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "input_schema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

fn doc_path_prop() -> Value {
    json!({
        "type": "string",
        "description": "Absolute path to the PDF on the machine running this server, \
                        normally inside U2S_BLOB_DIR."
    })
}

pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "pdf_info",
            "Page count, per-page dimensions in points, rotation, and form type. \
             Call this FIRST: when form_type is xfa_full or xfa_foreground the document \
             is an XFA form, and this server can only render its static \
             'please update your reader' shim page — route those to the XFA renderer instead.",
            json!({ "doc_path": doc_path_prop() }),
            json!(["doc_path"]),
        ),
        tool(
            "pdf_render_page",
            "Render one page to an image. Returns the image inline when it is small \
             enough, otherwise a blob handle the caller dereferences out of band. \
             dpi defaults to 144 and is clamped to 36..600; the long edge is clamped \
             to max_edge_px (default 2576) and the effective dpi is reported back.",
            json!({
                "doc_path": doc_path_prop(),
                "page": { "type": "integer", "minimum": 1, "description": "1-based page number." },
                "dpi": { "type": "number", "description": "Target resolution; default 144." },
                "format": { "type": "string", "enum": ["jpeg", "png"], "description": "Default jpeg." },
                "max_edge_px": { "type": "integer", "description": "Long-edge clamp; default 2576." }
            }),
            json!(["doc_path", "page"]),
        ),
        tool(
            "pdf_render_pages",
            "Render several pages with an explicit cursor. Give either `pages` (an exact \
             list) or `from` plus `limit` (a window). The response stops at whichever cap \
             binds first — an image count and a total byte budget — and reports which in \
             `budget_hit`, with `next_from` naming where to continue. Walk `next_from` \
             until it is null rather than asking for a whole document at once.",
            json!({
                "doc_path": doc_path_prop(),
                "pages": {
                    "type": "array", "items": { "type": "integer", "minimum": 1 },
                    "description": "Exact pages, in the order you want them."
                },
                "from": { "type": "integer", "minimum": 1, "description": "Window start; default 1." },
                "limit": { "type": "integer", "minimum": 1, "description": "Max images this call." },
                "dpi": { "type": "number" },
                "format": { "type": "string", "enum": ["jpeg", "png"] }
            }),
            json!(["doc_path"]),
        ),
        tool(
            "pdf_render_region",
            "Render a rectangular region of one page at high resolution — the tool for \
             reading small print. The rect is in PDF points with the origin at the page's \
             TOP-LEFT. dpi defaults to 300.",
            json!({
                "doc_path": doc_path_prop(),
                "page": { "type": "integer", "minimum": 1 },
                "rect_pt": {
                    "type": "object",
                    "description": "Region in points, origin top-left of the page.",
                    "properties": {
                        "x": { "type": "number" }, "y": { "type": "number" },
                        "width": { "type": "number" }, "height": { "type": "number" }
                    },
                    "required": ["x", "y", "width", "height"]
                },
                "dpi": { "type": "number" },
                "format": { "type": "string", "enum": ["jpeg", "png"] }
            }),
            json!(["doc_path", "page", "rect_pt"]),
        ),
        tool(
            "pdf_page_text",
            "Extracted text of one page, windowed by character offset. Always reports \
             total_chars and truncated, so a caller can tell a partial read from a whole \
             one. Use this to quote exactly; use the render tools to see layout.",
            json!({
                "doc_path": doc_path_prop(),
                "page": { "type": "integer", "minimum": 1 },
                "offset": { "type": "integer", "minimum": 0, "description": "Character offset; default 0." },
                "limit": { "type": "integer", "minimum": 1, "description": "Characters to return; default 4000." }
            }),
            json!(["doc_path", "page"]),
        ),
        tool(
            "pdf_search_text",
            "Find where something is said, across pages, without reading each one. Greps \
             extracted page text for a literal substring (case-insensitive) or a regex, and \
             tags every match with its page plus the offset and length of the match in that \
             page's text — pass those three straight to pdf_page_text to read the hit in \
             context. Scans from `from` onwards and stops at whichever cap binds first, a \
             match count and a page count, reporting which in `budget_hit` with `next_from` \
             naming where to continue; walk `next_from` until it is null. A page is always \
             scanned whole, so a walk never repeats a match. total_matches counts the pages \
             THIS call scanned, and truncated says the match limit dropped entries — narrow \
             the query rather than raising limit. A query that could match the empty string \
             is refused, since it would report one hit per character.",
            json!({
                "doc_path": doc_path_prop(),
                "query": { "type": "string", "description": "Substring, or regex pattern when regex is true." },
                "regex": { "type": "boolean", "description": "Treat query as a regex; default false. A literal is case-insensitive; a regex is not, so write (?i) if you want that." },
                "from": { "type": "integer", "minimum": 1, "description": "First page to scan; default 1." },
                "limit": { "type": "integer", "minimum": 1, "description": "Max matches returned; default 50." }
            }),
            json!(["doc_path", "query"]),
        ),
    ]
}

/// The `u2s.manifest` resource: what u2s reads at registration to learn each
/// tool's role, format scope and conformance vectors.
///
/// `$FIXTURES` is resolved by the conformance runner against the fixture
/// directory this server ships.
pub fn manifest() -> Value {
    let scope = json!({ "output_formats": [] });
    json!({
        "contract": CONTRACT_VERSION,
        "server": { "name": "u2s-render-pdf-mcp", "version": env!("CARGO_PKG_VERSION") },
        // `pdf_info`/`pdf_page_text`/`pdf_render_pages` opt into the
        // normalizer's format-agnostic ingest capabilities -- see
        // `u2s_mcp::manifest::IngestCapability`. The other three tools serve
        // agents directly (page-by-page rendering, region zoom, cross-page
        // search) and are not part of ingest.
        "tools": [
            { "tool": "pdf_info",          "role": "query", "ingest": "info",   "scope": scope },
            { "tool": "pdf_render_page",   "role": "query", "scope": scope },
            { "tool": "pdf_render_pages",  "role": "query", "ingest": "render", "scope": scope },
            { "tool": "pdf_render_region", "role": "query", "scope": scope },
            { "tool": "pdf_page_text",     "role": "query", "ingest": "text",   "scope": scope },
            { "tool": "pdf_search_text",   "role": "query", "scope": scope },
        ],
        "test_vectors": [
            {
                "tool": "pdf_info",
                "args": { "doc_path": "$FIXTURES/ten-pages.pdf" },
                "expect": { "structured": { "page_count": 10, "form_type": "none" } }
            },
            {
                "tool": "pdf_render_page",
                "args": { "doc_path": "$FIXTURES/ten-pages.pdf", "page": 1, "dpi": 72, "format": "png" },
                // Determinism is same-process byte equality; across pdfium builds
                // antialiasing shifts pixels, so geometry is asserted exactly and
                // similarity by perceptual hash.
                "expect": { "structured": { "width_px": 595, "height_px": 842 } }
            },
            {
                "tool": "pdf_page_text",
                "args": { "doc_path": "$FIXTURES/unicode-text.pdf", "page": 1 },
                "expect": { "structured": { "truncated": false } }
            },
            {
                "tool": "pdf_search_text",
                "args": { "doc_path": "$FIXTURES/unicode-text.pdf", "query": "Hello" },
                "expect": { "structured": { "total_matches": 1, "truncated": false } }
            }
        ]
    })
}
