//! The document reads: what they answer from, and the one body each has,
//! whichever path the call takes.
//!
//! [`ConversionAgent::start_read_as`] takes a [`DocumentView`] under the
//! agent's lock and answers from it without the lock;
//! [`ConversionAgent::execute_as`] answers from one while holding it.

use std::sync::Arc;

use serde_json::Value;
use u2s_doc_tools::native::{NativeJsonTool, RuleForCheck};
use u2s_jsondoc::Document;

use super::execute::{cap_total, windowed_text};
use super::{ConversionAgent, DOCUMENT_TOOLS, NO_PACKAGE, ToolReply};

/// The tools a [`DocumentView`] answers: the reads of the document, the
/// rules, the build and the source texts the coverage check compares.
const VIEW_TOOLS: &[&str] = &[
    "json_outline",
    "json_get",
    "json_search",
    "json_validate",
    "rule_list",
    "rule_get",
    "get_package_info",
    "read_package_file",
    "coverage_check",
];

/// What the document reads see: the agent as it stood when the view was
/// taken. Every part is shared, so taking one copies nothing, and a later edit
/// leaves it as it was.
pub(super) struct DocumentView {
    document: Arc<Document>,
    schema: Arc<Value>,
    rules: Arc<[RuleForCheck]>,
    judged: Arc<[crate::rules::JudgedRule]>,
    package: Option<Arc<[u8]>>,
    sources: Arc<[(String, Vec<u8>)]>,
}

impl ConversionAgent {
    /// The agent as the document reads see it now.
    pub(super) fn view(&self) -> DocumentView {
        DocumentView {
            document: self.document.clone(),
            schema: self.schema.clone(),
            rules: self.rules.clone(),
            judged: self.judged.clone(),
            package: self.built.as_ref().map(|built| built.package.clone()),
            sources: self.current_pdfs.clone(),
        }
    }
}

impl DocumentView {
    /// Whether `name` is answered by a view.
    pub(super) fn serves(name: &str) -> bool {
        VIEW_TOOLS.contains(&name)
    }

    /// `name`'s answer to `input`. Only for a tool it [`serves`](Self::serves).
    pub(super) fn answer(&self, name: &str, input: &Value) -> ToolReply {
        match name {
            "rule_get" => match crate::rules::rule_get(&self.rules, &self.judged, input) {
                Ok(rules) => ToolReply::Text(rules.to_string()),
                Err(e) => ToolReply::Error(e),
            },
            "get_package_info" => match &self.package {
                Some(pkg) => {
                    let files = references_mcp::unzip_package(pkg).unwrap_or_default();
                    let paths: Vec<&String> = files.iter().map(|(p, _)| p).collect();
                    ToolReply::Text(format!(
                        "size: {} bytes\nfiles:\n{}",
                        pkg.len(),
                        serde_json::to_string_pretty(&paths).unwrap_or_default()
                    ))
                }
                None => ToolReply::Error(NO_PACKAGE.into()),
            },
            "read_package_file" => {
                let path = input["path"].as_str().unwrap_or_default();
                // See `windowed_text`: an authored .content.xml reached 250 KB
                // in the run that overflowed the window, so the default here
                // is a bounded window, not the whole file.
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let limit = input["limit"].as_u64().unwrap_or(0) as usize;
                match &self.package {
                    Some(pkg) => match references_mcp::unzip_package(pkg) {
                        Ok(files) => match files.iter().find(|(p, _)| p == path) {
                            Some((_, content)) => ToolReply::Text(cap_total(windowed_text(content, offset, limit))),
                            None => ToolReply::Error(format!("No such file: {path:?}")),
                        },
                        Err(e) => ToolReply::Error(e),
                    },
                    None => ToolReply::Error(NO_PACKAGE.into()),
                }
            }
            "coverage_check" => {
                let language = match input.get("language") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(language)) => Some(language.as_str()),
                    Some(other) => return ToolReply::Error(format!("language must be a string, not {other}")),
                };
                match crate::coverage::check(&self.sources, self.document.value(), language) {
                    Ok(report) => ToolReply::Text(report.to_string()),
                    Err(e) => ToolReply::Error(e),
                }
            }
            name => {
                let tool = *DOCUMENT_TOOLS
                    .iter()
                    .find(|t| t.name() == name)
                    .unwrap_or_else(|| panic!("{name} is not a tool a document view answers"));
                // The vendored dispatch takes the document mutably whatever
                // the tool, so it gets a copy of its own: the view's stays as
                // it was taken.
                let mut document = Document::clone(&self.document);
                match u2s_doc_tools::native::dispatch(tool, input, &mut document, Some(&self.schema), &self.rules) {
                    Ok(outcome) => {
                        assert!(!outcome.mutated, "{name} changed the document, but it is a read");
                        match tool {
                            // One list holds every rule, the judged ones too.
                            NativeJsonTool::ListRules => {
                                ToolReply::Text(crate::rules::list_with_judged(outcome.value, &self.judged).to_string())
                            }
                            _ => ToolReply::Text(outcome.value.to_string()),
                        }
                    }
                    Err(e) => ToolReply::Error(e.to_string()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Access, access_of};

    /// Every tool a view answers is a read in the catalog, so it is answered
    /// off the lock.
    #[test]
    fn every_tool_a_view_answers_is_a_read() {
        for name in VIEW_TOOLS {
            assert_eq!(access_of(name), Access::Read, "{name}");
        }
    }
}
