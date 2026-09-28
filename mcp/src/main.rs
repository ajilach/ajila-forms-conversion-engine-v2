//! Blueprint MCP server (stdio).
//!
//! Exposes the form-conversion engine's tools over the Model Context Protocol so
//! an external LLM client (Claude Desktop, Claude Code, Cursor, …) can drive a
//! conversion step by step. The client supplies the reasoning; this server
//! supplies the tools, backed by the headless [`agent::ConversionAgent`].
//!
//! Transport is stdio: the client launches this binary as a subprocess and
//! speaks JSON-RPC over stdin/stdout. Register it in the client's MCP config
//! with `command` pointing at the built binary.
//!
//! ## Session model
//!
//! A single implicit conversion is held in the server. Call `start_conversion`
//! (with a `pdf_path` or `pdf_base64`, and an optional `profile`) to load a
//! source PDF; every other tool then operates on that loaded conversion. Each
//! tree change is versioned into the same edit-history SQLite the desktop app
//! uses, so a conversion driven here can later be reviewed in the app.

use std::collections::HashSet;
use std::sync::Arc;

use agent::{ConversionAgent, ToolReply};
use base64::Engine;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServiceExt};
use tokio::sync::Mutex;

/// The MCP server: holds a single implicit conversion session.
#[derive(Clone)]
struct Blueprint {
    agent: Arc<Mutex<Option<ConversionAgent>>>,
}

impl Blueprint {
    fn new() -> Self {
        Self {
            agent: Arc::new(Mutex::new(None)),
        }
    }
}

/// Build a `start_conversion` tool spec in the same Anthropic-style JSON shape
/// `ConversionAgent::tools()` emits, so it maps through the same converter.
fn start_conversion_spec() -> serde_json::Value {
    serde_json::json!({
        "name": "start_conversion",
        "description": "Load the source PDF(s) and begin a conversion. PREFER `pdf_path` (an absolute path on the machine running this server) — efficient, no payload. Use `pdf_paths` for a form that spans several PDFs. Only if the file is NOT reachable on the server's filesystem (e.g. it lives in a sandbox/upload), fall back to `pdf_base64` (raw PDF bytes, base64). Optionally pass a `profile` name (selects the AEM config + reference library). Must be called before any other tool; calling it again starts a fresh conversion.",
        "input_schema": {
            "type": "object",
            "properties": {
                "pdf_path": {"type": "string", "description": "Absolute path to the source PDF, on the server's filesystem. Preferred."},
                "pdf_paths": {"type": "array", "items": {"type": "string"}, "description": "Absolute paths to the source PDFs (use instead of pdf_path for a multi-PDF form)."},
                "pdf_base64": {"type": "string", "description": "Base64-encoded PDF bytes. Fallback for when the file is not on the server's filesystem; large, so prefer a path when possible."},
                "pdf_name": {"type": "string", "description": "Display name for a pdf_base64 source (e.g. \"form.pdf\")."},
                "profile": {"type": "string", "description": "Conversion profile name (for AEM config + references)."},
                "output_target": {"type": "string", "enum": ["aem", "redacto"], "description": "What to produce: \"aem\" (an Adaptive Form package, the default) or \"redacto\" (a text document). Determines which tools are available for the rest of the session."}
            },
            "required": []
        }
    })
}

/// Spec for the MCP-only `write_package` tool that exports the built AEM package
/// (a binary ZIP) to a local file path — binaries are returned by path, not
/// inlined into the transcript.
fn write_package_spec() -> serde_json::Value {
    serde_json::json!({
        "name": "write_package",
        "description": "Write the built AEM FileVault package (ZIP) to a local file path and return the path. Run build_aem_package first. Use this to retrieve the binary result instead of reading it into the conversation.",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to write the .zip package to."}
            },
            "required": ["path"]
        }
    })
}

/// Spec for the MCP-only `validate_aem_package_from_file` tool: validate a
/// FileVault package ZIP that already exists on disk, without a conversion
/// session. For the feedback flow, where a form's `_merged.zip` is edited
/// directly and then re-validated.
fn validate_from_file_spec() -> serde_json::Value {
    serde_json::json!({
        "name": "validate_aem_package_from_file",
        "description": "Validate an AEM FileVault package ZIP from a local file path (same checks as validate_aem_package: required FileVault structure, form and DAM content-XML validation). Operates on an external file — no conversion session or build_aem_package needed.",
        "input_schema": {
            "type": "object",
            "properties": {
                "zip_path": {"type": "string", "description": "Absolute path to the .zip package file to validate."}
            },
            "required": ["zip_path"]
        }
    })
}

/// The full advertised catalog: the MCP-only bootstrap/export tools plus the
/// engine tools scoped to an MCP client.
///
/// Scoped by `target` so a session never sees a tool the engine would then
/// refuse — an AEM conversion is not offered `build_redacto_dump`.
fn tool_catalog_for(target: blueprint::OutputTarget) -> Vec<serde_json::Value> {
    let mut specs = vec![
        start_conversion_spec(),
        write_package_spec(),
        validate_from_file_spec(),
    ];
    specs.extend(agent::tools_for(target, agent::scope::MCP));
    specs
}

/// Convert one Anthropic-style tool spec (`{name, description, input_schema}`)
/// into an rmcp [`Tool`]. The raw JSON schema is passed straight through as the
/// MCP `inputSchema`, so no schema derive (and no schemars-version coupling) is
/// involved.
fn to_mcp_tool(spec: &serde_json::Value) -> Option<Tool> {
    let name = spec.get("name")?.as_str()?.to_string();
    let description = spec
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or_default()
        .to_string();
    let input_schema = spec
        .get("input_schema")
        .and_then(|s| s.as_object())
        .cloned()
        .unwrap_or_default();
    Some(Tool::new(name, description, Arc::new(input_schema)))
}

/// Map an engine [`ToolReply`] onto an MCP [`CallToolResult`].
fn reply_to_result(reply: ToolReply) -> CallToolResult {
    match reply {
        ToolReply::Text(text) => CallToolResult::success(vec![Content::text(text)]),
        ToolReply::Blocks(blocks) => CallToolResult::success(
            blocks
                .into_iter()
                .map(|block| match block {
                    agent::ReplyBlock::Text(text) => Content::text(text),
                    agent::ReplyBlock::Image { media_type, data } => {
                        Content::image(data, media_type)
                    }
                })
                .collect(),
        ),
        ToolReply::Error(msg) => CallToolResult::error(vec![Content::text(msg)]),
    }
}

/// One source `start_conversion` was asked to load.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    /// A path on the server's filesystem, read when the conversion starts.
    Path(String),
    /// Inline bytes, for a client that cannot reach the server's filesystem.
    Inline { name: String, bytes: Vec<u8> },
}

/// Parse `start_conversion`'s source arguments: `pdf_path`, then `pdf_paths`,
/// then the `pdf_base64` fallback.
///
/// Paths are de-duplicated while keeping the order the client gave them — a
/// plain [`Vec::dedup`] would only collapse *adjacent* duplicates and silently
/// convert `["a.pdf", "b.pdf", "a.pdf"]` with `a.pdf` loaded twice.
fn collect_sources(args: &serde_json::Value) -> Result<Vec<Source>, String> {
    let mut sources = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    let path_args = args
        .get("pdf_path")
        .into_iter()
        .chain(args.get("pdf_paths").and_then(|v| v.as_array()).into_iter().flatten());
    for path in path_args.filter_map(|v| v.as_str()) {
        if seen.insert(path) {
            sources.push(Source::Path(path.to_string()));
        }
    }

    if let Some(b64) = args.get("pdf_base64").and_then(|v| v.as_str()) {
        let bytes = base64::prelude::BASE64_STANDARD
            .decode(b64)
            .map_err(|e| format!("`pdf_base64` is not valid base64: {e}"))?;
        let name = args
            .get("pdf_name")
            .and_then(|v| v.as_str())
            .unwrap_or("source.pdf")
            .to_string();
        sources.push(Source::Inline { name, bytes });
    }

    if sources.is_empty() {
        return Err(
            "start_conversion requires `pdf_path`, `pdf_paths` or `pdf_base64`.".to_string(),
        );
    }
    Ok(sources)
}

/// Resolve parsed [`Source`]s to named byte buffers, reading paths from disk.
fn load_sources(sources: Vec<Source>) -> Result<Vec<(String, Vec<u8>)>, String> {
    sources
        .into_iter()
        .map(|source| match source {
            Source::Inline { name, bytes } => Ok((name, bytes)),
            Source::Path(path) => {
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("Could not read {path:?}: {e}"))?;
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "source.pdf".to_string());
                Ok((name, bytes))
            }
        })
        .collect()
}

impl Blueprint {
    /// Handle the `start_conversion` bootstrap tool: load the PDF(s), create an
    /// edit-history session, and install a fresh [`ConversionAgent`].
    async fn start_conversion(&self, args: &serde_json::Value) -> CallToolResult {
        let pdfs = match collect_sources(args).and_then(load_sources) {
            Ok(pdfs) => pdfs,
            Err(e) => return CallToolResult::error(vec![Content::text(e)]),
        };

        let target = match args.get("output_target").and_then(|v| v.as_str()) {
            None => blueprint::OutputTarget::Aem,
            Some(raw) => match blueprint::OutputTarget::parse(raw) {
                Some(t) => t,
                None => {
                    return CallToolResult::error(vec![Content::text(format!(
                        "Unknown output_target {raw:?}; expected \"aem\" or \"redacto\"."
                    ))]);
                }
            },
        };

        let profile = args
            .get("profile")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let label = pdfs
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        // Verification is not optional, so a conversion whose verifier is not
        // ready is refused before anything is recorded. The settings are the
        // desktop app's (the shared history.db).
        let (aem_verify, redacto_verify) = match agent::u2s::stored_verify_settings() {
            Ok(settings) => settings,
            Err(e) => return CallToolResult::error(vec![Content::text(e)]),
        };
        let readiness = match target {
            blueprint::OutputTarget::Aem => agent::u2s::aem_verify_readiness(&aem_verify).await,
            blueprint::OutputTarget::Redacto => {
                agent::u2s::redacto_verify_readiness(&redacto_verify).await
            }
        };
        let verify_note = match readiness {
            Ok(report) => format!("Verification is ready: {report}"),
            Err(e) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "Verification is not possible, so the conversion was not started:\n{e}\n\
                     Configure it in the desktop app settings; see docker/aem/README.md."
                ))]);
            }
        };

        // Version the conversion into the shared edit-history DB so the desktop
        // app can later review it. Fail loudly when the session cannot be
        // created: a derived id would have no row in `sessions`, so every
        // subsequent edit would be written somewhere the app's session browser
        // cannot find — an unreviewable run, which defeats the point of
        // recording one at all.
        let doc_hash = agent::db::document_hash(&pdfs);
        agent::db::upsert_document(&doc_hash, &label);
        let Some(session) =
            agent::db::create_session(&doc_hash, profile.as_deref(), target.as_str(), &label)
        else {
            return CallToolResult::error(vec![Content::text(
                "Could not create an edit-history session (the shared history.db is unavailable). \
                 A conversion started now would not be reviewable in the desktop app, so it was \
                 not started.",
            )]);
        };
        agent::db::insert_edit(&session, "Initial (empty)", "[]");

        let count = pdfs.len();
        // How many reference forms / docs are available for this profile. The
        // count distinguishes "no references exist" from a profile mismatch
        // returning an empty list.
        let ref_count = agent::references::count(profile.as_deref().unwrap_or_default());
        let new_agent = ConversionAgent::new(profile, pdfs, session.clone(), target);
        let new_agent = match target {
            blueprint::OutputTarget::Aem => new_agent.with_aem_verify(&aem_verify),
            blueprint::OutputTarget::Redacto => new_agent.with_redacto_verify(&redacto_verify),
        };
        let new_agent = match new_agent {
            Ok(agent) => agent,
            Err(e) => {
                // A conversion that never started leaves no session behind.
                agent::db::delete_session(&session);
                return CallToolResult::error(vec![Content::text(e)]);
            }
        };
        let previous = self.agent.lock().await.replace(new_agent);
        let teardown_note = match previous {
            Some(mut previous) => match previous.shutdown_verifiers().await {
                Ok(()) => String::new(),
                Err(e) => format!("\n\nThe previous conversion's verifier could not be torn down: {e}"),
            },
            None => String::new(),
        };

        // The reference forms are AEM packages, so a Redacto session is not
        // told about them.
        let target_notes = match target {
            blueprint::OutputTarget::Redacto => format!("{verify_note}\n\n"),
            blueprint::OutputTarget::Aem => {
                let ref_note = if ref_count > 0 {
                    format!(
                        "{ref_count} reference form(s) are available for this profile — BEFORE \
                         building, consult them (search_references, then get_reference_package / \
                         read_reference_file) and match their structure rather than inventing \
                         your own."
                    )
                } else {
                    "No reference forms are available for this profile.".to_string()
                };
                format!("{ref_note}\n\n{verify_note}\n\n")
            }
        };

        // The workflow guidance is repeated here, not just in the server
        // `instructions`, because many MCP clients drop `instructions` and the
        // tool result is the one surface every client delivers to the model.
        let workflow = match target {
            blueprint::OutputTarget::Aem => agent::SYSTEM_PROMPT,
            blueprint::OutputTarget::Redacto => agent::REDACTO_SYSTEM_PROMPT,
        };
        CallToolResult::success(vec![Content::text(format!(
            "Loaded {count} PDF(s) [{label}] as a {kind} conversion (session {session}).\n\n\
             {workflow}\n\n\
             {target_notes}{MCP_ADDENDUM}{teardown_note}",
            kind = target.label(),
            MCP_ADDENDUM = agent::MCP_ADDENDUM,
        ))])
    }

    /// Handle the MCP-only `write_package` tool: write the built package bytes
    /// to a file path. Binaries leave the server by path, never inlined.
    async fn write_package(&self, args: &serde_json::Value) -> CallToolResult {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return CallToolResult::error(vec![Content::text("write_package requires `path`.")]);
        };
        let guard = self.agent.lock().await;
        let Some(conv) = guard.as_ref() else {
            return CallToolResult::error(vec![Content::text(
                "No conversion loaded. Call `start_conversion` first.",
            )]);
        };
        let Some(pkg) = conv.package() else {
            return CallToolResult::error(vec![Content::text(agent::NO_PACKAGE)]);
        };
        let size = pkg.len();
        match std::fs::write(path, &pkg) {
            Ok(()) => CallToolResult::success(vec![Content::text(format!(
                "Wrote package ({size} bytes) to {path:?}."
            ))]),
            Err(e) => CallToolResult::error(vec![Content::text(format!(
                "Could not write {path:?}: {e}"
            ))]),
        }
    }

    /// Handle the MCP-only `validate_aem_package_from_file` tool: read a ZIP
    /// from disk and run the package validation checks on it. Session-agnostic.
    async fn validate_aem_package_from_file(&self, args: &serde_json::Value) -> CallToolResult {
        let Some(zip_path) = args.get("zip_path").and_then(|v| v.as_str()) else {
            return CallToolResult::error(vec![Content::text(
                "validate_aem_package_from_file requires `zip_path`.",
            )]);
        };
        match std::fs::read(zip_path) {
            Ok(bytes) => match agent::validate_package_bytes(&bytes) {
                Ok(msg) => CallToolResult::success(vec![Content::text(msg)]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            },
            Err(e) => CallToolResult::error(vec![Content::text(format!(
                "Could not read {zip_path:?}: {e}"
            ))]),
        }
    }
}

impl ServerHandler for Blueprint {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("blueprint", env!("CARGO_PKG_VERSION"));
        // Advertise the shared workflow guidance (the same text the desktop app
        // injects as the agent's opening message), wrapped in the MCP-specific
        // bootstrap/teardown the engine tools don't cover. `start_conversion`
        // repeats both, for the clients that drop `instructions`.
        info.instructions = Some(format!(
            "Blueprint form-conversion tools (MCP).\n\n\
             FIRST: call `start_conversion` with a `pdf_path` (or `pdf_paths`) and optional \
             `profile`. It must precede every other tool; every tool then operates on that \
             loaded conversion.\n\n\
             {SYSTEM_PROMPT}\n\n\
             {MCP_ADDENDUM}",
            SYSTEM_PROMPT = agent::SYSTEM_PROMPT,
            MCP_ADDENDUM = agent::MCP_ADDENDUM,
        ));
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // Scope to the loaded conversion's target, so the client is never shown
        // a tool this session would refuse. Before `start_conversion`, advertise
        // the default target's catalog.
        let target = self
            .agent
            .lock()
            .await
            .as_ref()
            .map_or(blueprint::OutputTarget::Aem, |conv| conv.target());
        let tools = tool_catalog_for(target)
            .iter()
            .filter_map(to_mcp_tool)
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let name = request.name.as_ref();
        let input = serde_json::Value::Object(request.arguments.unwrap_or_default());

        // The MCP-only tools are handled here; everything else is an engine tool
        // and needs a loaded conversion.
        match name {
            "start_conversion" => return Ok(self.start_conversion(&input).await),
            "write_package" => return Ok(self.write_package(&input).await),
            "validate_aem_package_from_file" => {
                return Ok(self.validate_aem_package_from_file(&input).await);
            }
            _ => {}
        }

        let mut guard = self.agent.lock().await;
        let Some(conv) = guard.as_mut() else {
            return Ok(CallToolResult::error(vec![Content::text(
                "No conversion loaded. Call `start_conversion` first.",
            )]));
        };
        let reply = conv.execute(name, &input).await;
        Ok(reply_to_result(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let transport = rmcp::transport::stdio();
    let server = Blueprint::new();
    let agent = server.agent.clone();
    let service = server.serve(transport).await?;
    service.waiting().await?;
    // The client is gone: remove the containers the last conversion started.
    if let Some(mut last) = agent.lock().await.take()
        && let Err(e) = last.shutdown_verifiers().await
    {
        eprintln!("blueprint: the verifier could not be torn down: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path(p: &str) -> Source {
        Source::Path(p.to_string())
    }

    #[test]
    fn a_single_pdf_path_is_collected() {
        assert_eq!(
            collect_sources(&json!({"pdf_path": "/tmp/a.pdf"})).unwrap(),
            vec![path("/tmp/a.pdf")]
        );
    }

    #[test]
    fn pdf_path_and_pdf_paths_are_concatenated_in_order() {
        let args = json!({"pdf_path": "/tmp/a.pdf", "pdf_paths": ["/tmp/b.pdf", "/tmp/c.pdf"]});
        assert_eq!(
            collect_sources(&args).unwrap(),
            vec![path("/tmp/a.pdf"), path("/tmp/b.pdf"), path("/tmp/c.pdf")]
        );
    }

    /// Regression: `Vec::dedup` only collapses *adjacent* duplicates, so a form
    /// passed as a.pdf, b.pdf, a.pdf used to be converted with a.pdf loaded
    /// twice — doubling its states and its cost.
    #[test]
    fn duplicate_paths_are_removed_even_when_not_adjacent() {
        let args = json!({"pdf_paths": ["/tmp/a.pdf", "/tmp/b.pdf", "/tmp/a.pdf"]});
        assert_eq!(
            collect_sources(&args).unwrap(),
            vec![path("/tmp/a.pdf"), path("/tmp/b.pdf")]
        );
    }

    #[test]
    fn pdf_path_repeated_in_pdf_paths_is_collected_once() {
        let args = json!({"pdf_path": "/tmp/a.pdf", "pdf_paths": ["/tmp/a.pdf"]});
        assert_eq!(collect_sources(&args).unwrap(), vec![path("/tmp/a.pdf")]);
    }

    /// Regression: the schema advertised `pdf_base64` and the description told
    /// the model to fall back to it, but the handler only ever read the path
    /// arguments and then errored.
    #[test]
    fn pdf_base64_is_decoded_with_its_name() {
        let args = json!({
            "pdf_base64": base64::prelude::BASE64_STANDARD.encode(b"%PDF-1.7"),
            "pdf_name": "form.pdf",
        });
        assert_eq!(
            collect_sources(&args).unwrap(),
            vec![Source::Inline {
                name: "form.pdf".to_string(),
                bytes: b"%PDF-1.7".to_vec(),
            }]
        );
    }

    #[test]
    fn pdf_base64_without_a_name_falls_back_to_a_default() {
        let args = json!({"pdf_base64": base64::prelude::BASE64_STANDARD.encode(b"x")});
        assert!(matches!(
            collect_sources(&args).unwrap().as_slice(),
            [Source::Inline { name, .. }] if name == "source.pdf"
        ));
    }

    #[test]
    fn malformed_pdf_base64_is_reported_rather_than_silently_skipped() {
        let err = collect_sources(&json!({"pdf_base64": "not base64!"})).unwrap_err();
        assert!(err.contains("pdf_base64"), "{err}");
    }

    #[test]
    fn no_source_at_all_is_an_error_naming_every_accepted_argument() {
        let err = collect_sources(&json!({"profile": "ubs"})).unwrap_err();
        for arg in ["pdf_path", "pdf_paths", "pdf_base64"] {
            assert!(err.contains(arg), "{err} should mention {arg}");
        }
    }

    /// Every property `start_conversion` advertises must be one the handler
    /// actually reads — the `pdf_base64` divergence above went unnoticed
    /// because nothing checked the schema against the code.
    #[test]
    fn every_advertised_start_conversion_property_is_honoured() {
        let spec = start_conversion_spec();
        let props = spec["input_schema"]["properties"].as_object().unwrap();

        let sample = json!({
            "pdf_path": "/tmp/a.pdf",
            "pdf_paths": ["/tmp/b.pdf"],
            "pdf_base64": base64::prelude::BASE64_STANDARD.encode(b"x"),
            "pdf_name": "named.pdf",
        });
        let sources = collect_sources(&sample).unwrap();

        assert!(props.contains_key("profile"), "profile is read at load time");
        assert_eq!(
            props["output_target"]["enum"],
            json!(["aem", "redacto"]),
            "the advertised targets must be the ones OutputTarget::parse accepts"
        );
        for advertised in props["output_target"]["enum"].as_array().unwrap() {
            let raw = advertised.as_str().unwrap();
            assert!(
                blueprint::OutputTarget::parse(raw).is_some(),
                "start_conversion advertises output_target {raw:?}, which it cannot parse"
            );
        }
        assert!(
            sources.contains(&path("/tmp/a.pdf")) && sources.contains(&path("/tmp/b.pdf")),
            "pdf_path and pdf_paths must both be honoured: {sources:?}"
        );
        assert!(
            sources.iter().any(|s| matches!(s, Source::Inline { name, .. } if name == "named.pdf")),
            "pdf_base64 and pdf_name must both be honoured: {sources:?}"
        );
    }

    #[test]
    fn load_sources_passes_inline_bytes_through_untouched() {
        let loaded = load_sources(vec![Source::Inline {
            name: "a.pdf".to_string(),
            bytes: vec![1, 2, 3],
        }])
        .unwrap();
        assert_eq!(loaded, vec![("a.pdf".to_string(), vec![1, 2, 3])]);
    }

    #[test]
    fn load_sources_reports_the_path_it_could_not_read() {
        let err = load_sources(vec![path("/nonexistent/nope.pdf")]).unwrap_err();
        assert!(err.contains("nope.pdf"), "{err}");
    }

    fn catalog_names(target: blueprint::OutputTarget) -> Vec<String> {
        tool_catalog_for(target)
            .iter()
            .filter_map(|s| s["name"].as_str().map(String::from))
            .collect()
    }

    #[test]
    fn the_catalog_exposes_the_mcp_only_tools_alongside_the_engine_tools() {
        let names = catalog_names(blueprint::OutputTarget::Aem);
        for mcp_only in [
            "start_conversion",
            "write_package",
            "validate_aem_package_from_file",
        ] {
            assert!(
                names.iter().any(|n| n == mcp_only),
                "{mcp_only} missing from catalog"
            );
        }
        assert!(
            names.iter().any(|n| n == "build_aem_package"),
            "engine tools missing"
        );
    }

    /// Regression: the catalog used to be target-blind while every MCP session
    /// was hardcoded to AEM, so `build_redacto_dump` was advertised on every
    /// session and then refused on every call.
    #[test]
    fn the_catalog_never_advertises_a_tool_this_target_would_refuse() {
        let aem = catalog_names(blueprint::OutputTarget::Aem);
        assert!(!aem.iter().any(|n| n == "build_redacto_dump"), "{aem:?}");
        assert!(!aem.iter().any(|n| n == "review_redacto_output"), "{aem:?}");

        let redacto = catalog_names(blueprint::OutputTarget::Redacto);
        assert!(redacto.iter().any(|n| n == "build_redacto_dump"));
        assert!(!redacto.iter().any(|n| n == "set_aem_translated"), "{redacto:?}");
        assert!(!redacto.iter().any(|n| n == "build_aem_package"), "{redacto:?}");

        // The MCP-only bootstrap tools are offered whatever the target.
        for names in [&aem, &redacto] {
            assert!(names.iter().any(|n| n == "start_conversion"));
        }
    }

    #[test]
    fn to_mcp_tool_passes_the_raw_input_schema_through() {
        let spec = write_package_spec();
        let tool = to_mcp_tool(&spec).expect("spec converts");
        assert_eq!(tool.name, "write_package");
        assert_eq!(
            serde_json::Value::Object((*tool.input_schema).clone()),
            spec["input_schema"]
        );
    }

    /// The MCP addendum is deliberately emitted twice (many clients drop the
    /// server `instructions`), so it has to come from one constant.
    #[test]
    fn the_mcp_addendum_is_shared_not_restated() {
        let instructions = Blueprint::new().get_info().instructions.unwrap();
        assert!(instructions.contains(agent::MCP_ADDENDUM));
        assert!(instructions.contains(agent::SYSTEM_PROMPT));
    }
}
