//! The vendored u2s tool servers (see `u2s/VENDORED.md`), run in-process.
//!
//! Each server keeps its own tool contract: its specs go into the catalog as
//! they are (trimmed to the fields the model API takes), and its calls go
//! through its own `dispatch`. What this module adds is the edge: which files a
//! call may read, where the fonts come from, and turning a `CallToolResult`
//! into a [`ToolReply`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rmcp3::model::{CallToolResult, ContentBlock};
use serde_json::Value;
use u2s_render_core::{BlobStore, Limits};
use u2s_render_pdf_mcp::PdfRenderServer;
use u2s_render_xfa_mcp::XfaRenderServer;
use u2s_xfa_mcp::XfaDataServer;

use crate::conversion::{ReplyBlock, ToolReply};

/// The server a u2s tool belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    XfaData,
    XfaRender,
    PdfRender,
}

/// Every u2s tool spec, as the catalog takes them: `{name, description,
/// input_schema}` and nothing else.
pub(crate) fn tool_specs() -> Vec<Value> {
    families().iter().map(|(spec, _)| spec.clone()).collect()
}

/// Whether `name` is a u2s tool.
pub(crate) fn is_u2s_tool(name: &str) -> bool {
    family_of(name).is_some()
}

fn family_of(name: &str) -> Option<Family> {
    families()
        .iter()
        .find(|(spec, _)| spec["name"] == name)
        .map(|(_, family)| *family)
}

fn families() -> &'static [(Value, Family)] {
    static FAMILIES: OnceLock<Vec<(Value, Family)>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let tagged = |specs: Vec<Value>, family: Family| {
            specs
                .into_iter()
                .map(move |spec| (catalog_spec(&spec), family))
                .collect::<Vec<_>>()
        };
        let mut all = tagged(u2s_xfa_mcp::specs::tool_specs(), Family::XfaData);
        all.extend(tagged(
            u2s_render_xfa_mcp::specs::tool_specs(),
            Family::XfaRender,
        ));
        all.extend(tagged(
            u2s_render_pdf_mcp::specs::tool_specs(),
            Family::PdfRender,
        ));
        all
    })
}

fn catalog_spec(spec: &Value) -> Value {
    serde_json::json!({
        "name": spec["name"],
        "description": spec["description"],
        "input_schema": spec["input_schema"],
    })
}

/// The u2s servers of one conversion agent, plus the documents its calls may
/// open.
pub struct U2sTools {
    /// Holds the source PDFs the tools read and the blob store they write.
    /// Removed with the agent.
    dir: tempfile::TempDir,
    data: XfaDataServer,
    render: XfaRenderServer,
    /// Started on the first `pdf_*` call: it loads pdfium, which the XFA tools
    /// do not need.
    pdf: Option<PdfRenderServer>,
    /// Canonical paths a `doc_path` argument may name.
    documents: HashSet<PathBuf>,
}

impl U2sTools {
    pub fn new() -> Result<Self, String> {
        register_fonts()?;
        let dir = tempfile::Builder::new()
            .prefix("blueprint-u2s-")
            .tempdir()
            .map_err(|e| format!("could not create the u2s working directory: {e}"))?;
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Ok(Self {
            data: XfaDataServer,
            render: XfaRenderServer::with_parts(Limits::default(), blobs),
            pdf: None,
            documents: HashSet::new(),
            dir,
        })
    }

    /// Writes a document the tools may read and returns the path to pass as
    /// `doc_path`. Writing the same name twice under one `group` replaces it.
    pub fn add_document(&mut self, group: &str, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let file_name = Path::new(name)
            .file_name()
            .ok_or_else(|| format!("document name {name:?} has no file name"))?;
        let dir = self.dir.path().join(group);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let path = dir.join(file_name);
        std::fs::write(&path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("could not resolve {}: {e}", path.display()))?;
        self.documents.insert(canonical.clone());
        Ok(canonical)
    }

    pub async fn call(&mut self, name: &str, input: &Value) -> ToolReply {
        let Some(family) = family_of(name) else {
            return ToolReply::Error(format!("Unknown tool: {name}"));
        };
        if let Err(e) = self.check_document(input) {
            return ToolReply::Error(e);
        }
        let name_owned = name.to_string();
        let input_owned = input.clone();
        let result = match family {
            Family::XfaData => {
                let server = self.data.clone();
                tokio::task::spawn_blocking(move || {
                    server
                        .dispatch(&name_owned, &input_owned)
                        .map_err(|e| e.to_string())
                })
                .await
            }
            Family::XfaRender => {
                let server = self.render.clone();
                tokio::task::spawn_blocking(move || {
                    server
                        .dispatch(&name_owned, &input_owned)
                        .map_err(|e| e.to_string())
                })
                .await
            }
            Family::PdfRender => {
                let server = match self.pdf_server() {
                    Ok(server) => server.clone(),
                    Err(e) => return ToolReply::Error(e),
                };
                tokio::task::spawn_blocking(move || {
                    server
                        .dispatch(&name_owned, &input_owned)
                        .map_err(|e| e.to_string())
                })
                .await
            }
        };
        match result {
            Ok(Ok(result)) => reply_from_result(result),
            Ok(Err(message)) => ToolReply::Error(message),
            Err(join) => ToolReply::Error(format!("{name} failed inside the tool server: {join}")),
        }
    }

    fn pdf_server(&mut self) -> Result<&PdfRenderServer, String> {
        let server = match self.pdf.take() {
            Some(server) => server,
            None => {
                let blobs = BlobStore::new(self.dir.path().join("blobs"));
                PdfRenderServer::with_parts(Limits::default(), blobs).map_err(|e| {
                    format!("the PDF renderer is unavailable ({e}); run scripts/fetch-pdfium.sh")
                })?
            }
        };
        Ok(self.pdf.insert(server))
    }

    /// A `doc_path` must name a document this agent wrote.
    fn check_document(&self, input: &Value) -> Result<(), String> {
        let Some(doc_path) = input.get("doc_path") else {
            return Ok(());
        };
        let raw = doc_path
            .as_str()
            .ok_or("doc_path must be a string path from get_source_info")?;
        let allowed = Path::new(raw)
            .canonicalize()
            .is_ok_and(|p| self.documents.contains(&p));
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "doc_path {raw:?} is not one of this run's documents; use a path from get_source_info"
            ))
        }
    }
}

/// Registers every profile's parser fonts with the u2s font manager, once per
/// process (the manager is process-global).
fn register_fonts() -> Result<(), String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            use u2s_xfa::xfa::font_manager::{get_font_manager, register_profile_font_data};

            let mut fonts = Vec::new();
            for profile in blueprint::list_profiles() {
                if let Ok(files) = blueprint::profile_font_files(&profile) {
                    fonts.extend(files);
                }
            }
            let fallback = fallback_font(&fonts)
                .ok_or("no profile ships parser fonts; the u2s renderer cannot lay out text")?;
            let manager = get_font_manager();
            let mut manager = manager
                .lock()
                .map_err(|e| format!("u2s font manager lock: {e}"))?;
            for (_, data) in &fonts {
                register_profile_font_data(&mut manager, data);
            }
            manager.set_fallback(fallback);
            Ok(())
        })
        .clone()
}

/// The face every unresolvable typeface falls back to: the first upright,
/// normal-weight one, else the first light one, else the first of all. A bold
/// or italic fallback would restyle every such run of text.
fn fallback_font(fonts: &[(String, &'static [u8])]) -> Option<&'static [u8]> {
    let stem = |s: &str| s.to_ascii_lowercase();
    let styled = |s: &str| ["bold", "italic", "oblique", "black"].iter().any(|w| stem(s).contains(w));
    fonts
        .iter()
        .find(|(s, _)| !styled(s) && !stem(s).contains("light"))
        .or_else(|| fonts.iter().find(|(s, _)| !styled(s)))
        .or_else(|| fonts.first())
        .map(|(_, data)| *data)
}

fn reply_from_result(result: CallToolResult) -> ToolReply {
    let blocks: Vec<ReplyBlock> = result
        .content
        .into_iter()
        .map(|content| match content {
            ContentBlock::Text(t) => ReplyBlock::Text(t.text),
            ContentBlock::Image(i) => ReplyBlock::Image {
                media_type: i.mime_type,
                data: i.data,
            },
            ContentBlock::ResourceLink(r) => ReplyBlock::Text(format!("[resource link: {}]", r.uri)),
            _ => ReplyBlock::Text("[non-text content omitted]".into()),
        })
        .collect();
    let text = || {
        blocks
            .iter()
            .filter_map(|b| match b {
                ReplyBlock::Text(t) => Some(t.as_str()),
                ReplyBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if result.is_error == Some(true) {
        let message = text();
        return ToolReply::Error(if message.is_empty() {
            "the tool reported an error".into()
        } else {
            message
        });
    }
    if blocks.iter().all(|b| matches!(b, ReplyBlock::Text(_))) {
        ToolReply::Text(text())
    } else {
        ToolReply::Blocks(blocks)
    }
}
