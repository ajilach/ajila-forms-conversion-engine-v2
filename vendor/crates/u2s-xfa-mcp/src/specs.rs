//! Tool specifications and the `u2s.manifest` resource.
//!
//! Each tool's input schema is generated from its argument type, which is
//! also what its arguments parse into. Tool descriptions stay here; argument
//! descriptions are the doc comments on those fields. Both are prompt surface.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use u2s_render_core::args::{DocumentRef, InfoArgs, ServerArgs};
use u2s_schema::tool_input_schema;

pub const CONTRACT_VERSION: &str = "1.0.0";

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "input_schema": input_schema })
}

/// An xfa data tool's arguments: the PDF it reads next to the tool's own.
pub type XfaDataArgs<T> = ServerArgs<DocumentRef, T>;

fn schema_of<T: JsonSchema>() -> Value {
    tool_input_schema::<XfaDataArgs<T>>()
}

/// `xfa_read`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// A packet name from `xfa_packets` (e.g. "template", "datasets", "xdp" for a single-stream /XFA). Leave out to use the first packet.
    #[schemars(length(min = 1))]
    pub packet: Option<String>,
    /// Character offset; default 0.
    pub offset: Option<usize>,
    /// Characters to return; default 4000.
    #[schemars(range(min = 1))]
    pub limit: Option<usize>,
}

/// `xfa_search`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// A packet name from `xfa_packets` (e.g. "template", "datasets", "xdp" for a single-stream /XFA). Leave out to use the first packet.
    #[schemars(length(min = 1))]
    pub packet: Option<String>,
    /// Substring or regex pattern.
    pub query: String,
    /// Treat query as a regex; default false.
    #[serde(default)]
    pub regex: bool,
    /// Max matches returned; default 50.
    #[schemars(range(min = 1))]
    pub limit: Option<usize>,
}

/// `xfa_outline`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OutlineArgs {
    /// Depth from each packet root; default 10. A leaf value is itself nested 2-3 levels under its field (value/text/character-data), so shallower defaults cut off real content.
    #[schemars(range(min = 1))]
    pub max_depth: Option<usize>,
    /// Max entries returned; default 200.
    #[schemars(range(min = 1))]
    pub limit: Option<usize>,
}

/// `xfa_node`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct NodeArgs {
    /// A path from xfa_outline.
    pub path: String,
}

pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "xfa_packets",
            "List the raw /XFA packets a PDF carries — their declared names and byte \
             lengths, in declaration order. Call this FIRST: it costs nothing (no parse, \
             no layout) and names what `xfa_read` and `xfa_search` can address. A PDF with \
             no XFA form returns an empty list — use pdf_info to confirm form_type.",
            schema_of::<InfoArgs>(),
        ),
        tool(
            "xfa_read",
            "Raw text of one XFA packet, windowed by character offset — for reading the \
             literal XML, not its structure (use `xfa_outline`/`xfa_node` for that). Always \
             reports total_chars and truncated, so a caller can tell a partial read from a \
             whole one. limit defaults to 4000 characters.",
            schema_of::<ReadArgs>(),
        ),
        tool(
            "xfa_search",
            "Search XFA packet text for a literal substring (case-insensitive) or a regex, \
             over a sliding window rather than by line — line-based matching is meaningless \
             on minified XFA, where a whole packet can be one line. Searches every packet by \
             default, tagging each match with its packet; pass `packet` to restrict to one. \
             Each match reports offset and length as character offsets into that packet, so \
             passing them straight to `xfa_read` returns the match itself — search to locate, \
             read to see the surrounding XML. Reports total_matches and truncated honestly \
             rather than silently capping — narrow with `packet` or a more specific query \
             rather than raising limit. A query that could match the empty string is refused, \
             since it would report one hit per character.",
            schema_of::<SearchArgs>(),
        ),
        tool(
            "xfa_outline",
            "Depth- and count-capped structural summary of the parsed XFA — every node's \
             path, kind, a short content excerpt, and how many children it has, without \
             showing the children. Each packet parses to its own root, addressed as \
             template[0], datasets[0], etc. Walk into a path with `xfa_node` rather than \
             raising max_depth or limit.",
            schema_of::<OutlineArgs>(),
        ),
        tool(
            "xfa_node",
            "One node's own info — kind, attributes, a content excerpt — plus the list of \
             its immediate children (never a subtree). `path` is a dot-joined address as \
             returned by `xfa_outline`, e.g. \"template[0].subform2.Field1\".",
            schema_of::<NodeArgs>(),
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
        "server": { "name": "u2s-xfa-mcp", "version": env!("CARGO_PKG_VERSION") },
        // `xfa_outline` opts into the normalizer's `outline` ingest
        // capability -- see `u2s_mcp::manifest::IngestCapability`. It
        // already errors on exactly the documents `xfa_packets` errors on
        // (both go through the same `packets_of`), so `xfa_packets` does
        // not need its own ingest declaration; it stays a registered query
        // tool for agents.
        "tools": [
            { "tool": "xfa_packets", "role": "query", "scope": scope },
            { "tool": "xfa_read",    "role": "query", "scope": scope },
            { "tool": "xfa_search",  "role": "query", "scope": scope },
            { "tool": "xfa_outline", "role": "query", "ingest": "outline", "scope": scope },
            { "tool": "xfa_node",    "role": "query", "scope": scope },
        ],
        "test_vectors": [
            {
                "tool": "xfa_packets",
                "args": { "doc_path": "$FIXTURES/minimal.xfa.pdf" },
                "expect": { "structured": { "count": 1 } }
            },
            {
                "tool": "xfa_outline",
                "args": { "doc_path": "$FIXTURES/minimal.xfa.pdf" },
                "expect": { "structured": { "truncated": false } }
            }
        ]
    })
}
