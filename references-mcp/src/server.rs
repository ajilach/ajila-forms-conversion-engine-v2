//! The reference tools as an MCP server: [`ReferencesServer::dispatch`] for
//! hosts that run it in-process, and an rmcp [`ServerHandler`] for the stdio
//! binary.
//!
//! Every failure a caller could act on is an error whose text is the message
//! the model reads; the handler turns it into a tool error, not a protocol
//! error.

use std::sync::{Arc, OnceLock};

use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::args;
use crate::semantic::SemanticMatcher;
use crate::specs;
use crate::store::ReferenceStore;

/// A failed tool call, in the words the model reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// The matcher, loaded on first use. The load result is kept either way: the
/// model is embedded in the binary, so a failure would repeat.
///
/// Interior mutability: the `OnceLock` is written once, by whichever call
/// needs the matcher first, and only read afterwards. It is shared between the
/// clones of a server (the stdio handler clones per call).
type LazyMatcher = Arc<OnceLock<Result<SemanticMatcher, String>>>;

#[derive(Clone)]
pub struct ReferencesServer {
    store: ReferenceStore,
    profile: String,
    matcher: LazyMatcher,
}

impl ReferencesServer {
    /// `profile` scopes every listing and search; the host supplies it, the
    /// model never does.
    pub fn new(store: ReferenceStore, profile: String) -> Self {
        Self {
            store,
            profile,
            matcher: Arc::default(),
        }
    }

    fn matcher(&self) -> Result<&SemanticMatcher, Error> {
        self.matcher
            .get_or_init(|| SemanticMatcher::new().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| Error(e.clone()))
    }

    pub fn dispatch(&self, name: &str, args: &Value) -> Result<CallToolResult, Error> {
        match name {
            "list_reference_forms" => {
                let args::ListReferenceForms {} = parse(name, args)?;
                let list: Vec<_> = self
                    .store
                    .list_references(&self.profile)
                    .into_iter()
                    .map(|r| json!({"ref_id": r.ref_id, "label": r.label, "description": r.description, "pdf_count": r.pdf_count, "files": r.files}))
                    .collect();
                pretty(&list)
            }
            "search_references" => {
                let a: args::SearchReferences = parse(name, args)?;
                if a.query.trim().is_empty() {
                    return Err(Error(
                        "search_references requires a non-empty query — pass a description of the \
                         input form/section, not an empty string."
                            .into(),
                    ));
                }
                let top_k = a.top_k.unwrap_or(3).max(1);
                let hits: Vec<_> = self
                    .store
                    .search_references(&self.profile, &a.query, self.matcher()?, top_k)
                    .into_iter()
                    .map(|h| json!({"ref_id": h.ref_id, "label": h.label, "where": h.location, "matched": h.matched, "score": h.score, "snippet": h.snippet}))
                    .collect();
                pretty(&hits)
            }
            "grep_references" => {
                let a: args::GrepReferences = parse(name, args)?;
                let hits: Vec<_> = self
                    .store
                    .grep_references(&self.profile, &a.query, a.regex.unwrap_or(false))
                    .into_iter()
                    .map(|h| json!({"ref_id": h.ref_id, "label": h.label, "where": h.location, "snippet": h.snippet}))
                    .collect();
                pretty(&hits)
            }
            "read_reference_file" => {
                let a: args::ReadReferenceFile = parse(name, args)?;
                let text = self
                    .store
                    .read_reference_file(&a.ref_id, &a.path, a.offset.unwrap_or(0), a.limit.unwrap_or(0))?;
                Ok(text_result(text))
            }
            "get_reference_package" => {
                let a: args::GetReferencePackage = parse(name, args)?;
                let files = self.store.get_reference_package_files(&a.ref_id);
                let paths: Vec<&String> = files.iter().map(|(p, _)| p).collect();
                pretty(&paths)
            }
            "list_reference_docs" => {
                let args::ListReferenceDocs {} = parse(name, args)?;
                let list: Vec<_> = self
                    .store
                    .list_docs(&self.profile)
                    .into_iter()
                    .map(|d| json!({"doc_id": d.doc_id, "label": d.label}))
                    .collect();
                pretty(&list)
            }
            "read_reference_doc" => {
                let a: args::ReadReferenceDoc = parse(name, args)?;
                Ok(text_result(self.store.read_doc(&a.doc_id, a.offset.unwrap_or(0), a.limit.unwrap_or(0))?))
            }
            "grep_reference_docs" => {
                let a: args::GrepReferenceDocs = parse(name, args)?;
                let hits: Vec<_> = self
                    .store
                    .grep_docs(&self.profile, &a.query, a.regex.unwrap_or(false))
                    .into_iter()
                    .map(|(doc_id, label, snippet)| json!({"doc_id": doc_id, "label": label, "snippet": snippet}))
                    .collect();
                pretty(&hits)
            }
            other => Err(Error(format!("unknown tool {other}"))),
        }
    }
}

/// The tool's arguments, converted once at the edge: a missing field, a wrong
/// type or an unknown field is an error naming the tool and the problem.
fn parse<T: DeserializeOwned>(tool: &str, args: &Value) -> Result<T, Error> {
    // A call without arguments arrives as `null`; it is an empty object.
    let args = if args.is_null() { json!({}) } else { args.clone() };
    serde_json::from_value(args).map_err(|e| Error(format!("invalid arguments for {tool}: {e}")))
}

fn text_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// A JSON reply as pretty-printed text, the shape the model has always read.
fn pretty<T: Serialize>(value: &T) -> Result<CallToolResult, Error> {
    serde_json::to_string_pretty(value)
        .map(text_result)
        .map_err(|e| Error(format!("serialize reply: {e}")))
}

fn to_mcp_tool(spec: &Value) -> Option<Tool> {
    let name = spec.get("name")?.as_str()?.to_string();
    let description = spec
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The spec files say `input_schema`; MCP wants `inputSchema`. The rename
    // happens here and only here.
    let schema = spec.get("input_schema")?.as_object()?.clone();
    Some(Tool::new(name, description, Arc::new(schema)))
}

impl ServerHandler for ReferencesServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("references-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Worked-example reference forms and reference documentation for one customer \
             profile. `search_references` finds precedents by meaning, `grep_references` and \
             `grep_reference_docs` find an exact string, the rest list and read."
                .to_string(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = specs::tool_specs().iter().filter_map(to_mcp_tool).collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());

        // Database reads and the first model load are blocking work.
        let this = self.clone();
        let result = tokio::task::spawn_blocking(move || this.dispatch(&name, &args))
            .await
            .map_err(|e| McpError::internal_error(format!("task panicked: {e}"), None))?;

        Ok(match result {
            Ok(ok) => ok.into(),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into(),
        })
    }
}
