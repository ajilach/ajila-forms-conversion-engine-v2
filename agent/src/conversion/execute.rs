//! The tool executor: one match over the catalog's tool names.
//!
//! Each arm is the tool's own work; what every edit shares (dropping the stale
//! build, the `AI:` snapshot, the lint) is [`ConversionAgent::edited`].

use super::*;

/// Lines `read_package_file` returns by default, when the caller does not ask
/// for a specific window.
///
/// Unlike `read_reference_file`'s `offset`/`limit` (where 0 means "the whole
/// file"), a package file is not bounded the same way: an authored
/// `.content.xml` reached 250 KB in a run that blew its context window. 500
/// lines is ~31,000 characters at the density observed in real forms (~62
/// bytes/line), well under `pipeline::run::LARGE_TOOL_REPLY_WARN_CHARS`.
pub(super) const DEFAULT_TEXT_WINDOW_LINES: usize = 500;

/// Hard ceiling on a windowed tool's *total* reply, however it was reached.
///
/// `DEFAULT_TEXT_WINDOW_LINES` bounds the default, but an explicit `limit` has
/// no ceiling of its own. This is the backstop that holds regardless: no
/// windowed reply can reach the scale of the incident that prompted it (873,000
/// characters) however large a limit is requested. Comfortably above the
/// default so it only ever engages on a large *explicit* request, never
/// silently on ordinary use.
pub(super) const MAX_TOTAL_REPLY_CHARS: usize = 400_000;

/// A bounded line-range slice of `content`, with a note appended when the
/// window does not reach the end.
///
/// `offset`/`limit` are both in lines; `limit == 0` (absent, or explicitly 0)
/// means [`DEFAULT_TEXT_WINDOW_LINES`], not "everything" — see that constant's
/// doc for why `read_package_file` cannot default to unbounded the way
/// `read_reference_file`'s `offset`/`limit` does. Does not itself
/// apply [`MAX_TOTAL_REPLY_CHARS`]: that is a property of the whole reply a
/// caller assembles, not of one source's window — see [`cap_total`].
///
/// `pub(super)` so it can be unit-tested directly, on synthetic content, rather
/// than only indirectly through a built package.
pub(super) fn windowed_text(content: &str, offset: usize, limit: usize) -> String {
    let limit = if limit == 0 {
        DEFAULT_TEXT_WINDOW_LINES
    } else {
        limit
    };
    let total = content.lines().count();

    // The whole source already fits in one window: return it byte for byte
    // rather than reconstructing through `lines().join("\n")`, which would
    // silently drop a trailing newline or normalize CRLF to LF even though
    // nothing was actually windowed.
    if offset == 0 && limit >= total {
        return content.to_string();
    }
    if offset >= total {
        return format!("[offset {offset} is past the end — this source has only {total} lines]");
    }

    let window: Vec<&str> = content.lines().skip(offset).take(limit).collect();
    let shown = window.len();
    let mut out = window.join("\n");
    if offset + shown < total {
        out.push_str(&format!(
            "\n[showing lines {}-{} of {total} — pass a higher offset to continue, or a \
             higher limit to read more at once]",
            offset + 1,
            offset + shown
        ));
    }
    out
}

/// Truncate `text` to at most [`MAX_TOTAL_REPLY_CHARS`], noting it when it
/// had to. The backstop `windowed_text` cannot provide on its own — see
/// [`MAX_TOTAL_REPLY_CHARS`]'s doc for why one is still needed after windowing.
pub(super) fn cap_total(text: String) -> String {
    if text.len() <= MAX_TOTAL_REPLY_CHARS {
        return text;
    }
    // `text` is UTF-8; cut lands mid-codepoint unless walked back to a
    // boundary.
    let mut cut = MAX_TOTAL_REPLY_CHARS;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n[reply truncated at {MAX_TOTAL_REPLY_CHARS} characters — narrow the request (a \
         smaller limit) instead of reading this much at once]",
        &text[..cut]
    )
}

impl ConversionAgent {
    pub async fn execute(&mut self, name: &str, input: &Value) -> ToolReply {
        if let Some(refusal) = self.target_refusal(name) {
            return ToolReply::Error(refusal);
        }

        match name {
            // §1 source
            "get_source_info" => match self.source_documents(input) {
                Ok(documents) => {
                    let mut languages: Vec<&str> = documents
                        .iter()
                        .filter_map(|(_, c, _)| c.as_ref().map(|c| c.language.as_str()))
                        .collect();
                    languages.dedup();
                    let documents: Vec<Value> = documents
                        .iter()
                        .map(|(name, context, path)| match context {
                            Some(context) => json!({
                                "name": name,
                                "language": context.language,
                                "variables": context.variables,
                                "doc_path": path.display().to_string(),
                            }),
                            None => json!({
                                "name": name,
                                "xfa": false,
                                "doc_path": path.display().to_string(),
                            }),
                        })
                        .collect();
                    ToolReply::Text(json!({ "languages": languages, "documents": documents }).to_string())
                }
                Err(e) => ToolReply::Error(e),
            },

            // §2 the document
            "rule_check" => {
                if let Err(e) = self.runner() {
                    return ToolReply::Error(e);
                }
                let runner = self.runner.as_ref().expect("started above");
                match u2s_doc_tools::native::check_rules(
                    input,
                    self.document.value(),
                    Some(&self.schema),
                    &self.rules,
                    runner,
                )
                .await
                {
                    Ok(outcome) => ToolReply::Text(outcome.value.to_string()),
                    Err(e) => ToolReply::Error(e.to_string()),
                }
            }
            "rule_autofix" => self.autofix(input).await,
            name if DOCUMENT_TOOLS.iter().any(|t| t.name() == name) => {
                let tool = *DOCUMENT_TOOLS.iter().find(|t| t.name() == name).expect("matched");
                match u2s_doc_tools::native::dispatch(
                    tool,
                    input,
                    &mut self.document,
                    Some(&self.schema),
                    &self.rules,
                ) {
                    Ok(outcome) if outcome.mutated => self.edited(name, outcome.value).await,
                    Ok(outcome) => ToolReply::Text(outcome.value.to_string()),
                    Err(e) => ToolReply::Error(e.to_string()),
                }
            }

            // §3 building
            "build_aem_package" => self.build_aem_package(),
            "build_redacto_dump" => self.build_redacto_dump(),
            "get_package_info" => match self.package() {
                Some(pkg) => {
                    let files = crate::references::unzip_package(&pkg).unwrap_or_default();
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
                match self.package() {
                    Some(pkg) => match crate::references::unzip_package(&pkg) {
                        Ok(files) => match files.iter().find(|(p, _)| p == path) {
                            Some((_, content)) => {
                                ToolReply::Text(cap_total(windowed_text(content, offset, limit)))
                            }
                            None => ToolReply::Error(format!("No such file: {path:?}")),
                        },
                        Err(e) => ToolReply::Error(e),
                    },
                    None => ToolReply::Error(NO_PACKAGE.into()),
                }
            }

            // §7 references
            "list_reference_forms" => {
                let profile = self.profile.clone().unwrap_or_default();
                let list: Vec<_> = crate::references::list_references(&profile)
                    .into_iter()
                    .map(|r| serde_json::json!({"ref_id": r.ref_id, "label": r.label, "description": r.description, "pdf_count": r.pdf_count, "files": r.files}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&list).unwrap_or_default())
            }
            "search_references" => {
                let profile = self.profile.clone().unwrap_or_default();
                let query = input["query"].as_str().unwrap_or_default().to_string();
                if query.trim().is_empty() {
                    return ToolReply::Error(
                        "search_references requires a non-empty query — pass a description of the \
                         input form/section, not an empty string."
                            .into(),
                    );
                }
                let top_k = input["top_k"].as_u64().unwrap_or(3).max(1) as usize;
                let matcher = match self.matcher() {
                    Ok(m) => m,
                    Err(e) => return ToolReply::Error(e),
                };
                let hits: Vec<_> =
                    crate::references::search_references(&profile, &query, matcher, top_k)
                        .into_iter()
                        .map(|h| serde_json::json!({"ref_id": h.ref_id, "label": h.label, "where": h.location, "matched": h.matched, "score": h.score, "snippet": h.snippet}))
                        .collect();
                ToolReply::Text(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "grep_references" => {
                let profile = self.profile.clone().unwrap_or_default();
                let query = input["query"].as_str().unwrap_or_default();
                let regex = input["regex"].as_bool().unwrap_or(false);
                let hits: Vec<_> = crate::references::grep_references(&profile, query, regex)
                    .into_iter()
                    .map(|h| serde_json::json!({"ref_id": h.ref_id, "label": h.label, "where": h.location, "snippet": h.snippet}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "read_reference_file" => {
                let ref_id = input["ref_id"].as_str().unwrap_or_default();
                let path = input["path"].as_str().unwrap_or_default();
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let limit = input["limit"].as_u64().unwrap_or(0) as usize;
                match crate::references::read_reference_file(ref_id, path, offset, limit) {
                    Ok(t) => ToolReply::Text(t),
                    Err(e) => ToolReply::Error(e),
                }
            }
            "get_reference_package" => {
                let ref_id = input["ref_id"].as_str().unwrap_or_default();
                let files = crate::references::get_reference_package_files(ref_id);
                let paths: Vec<&String> = files.iter().map(|(p, _)| p).collect();
                ToolReply::Text(serde_json::to_string_pretty(&paths).unwrap_or_default())
            }
            "list_reference_docs" => {
                let profile = self.profile.clone().unwrap_or_default();
                let list: Vec<_> = crate::references::list_docs(&profile)
                    .into_iter()
                    .map(|d| serde_json::json!({"doc_id": d.doc_id, "label": d.label}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&list).unwrap_or_default())
            }
            "read_reference_doc" => {
                let doc_id = input["doc_id"].as_str().unwrap_or_default();
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let limit = input["limit"].as_u64().unwrap_or(0) as usize;
                match crate::references::read_doc(doc_id, offset, limit) {
                    Ok(t) => ToolReply::Text(t),
                    Err(e) => ToolReply::Error(e),
                }
            }
            "grep_reference_docs" => {
                let profile = self.profile.clone().unwrap_or_default();
                let query = input["query"].as_str().unwrap_or_default();
                let regex = input["regex"].as_bool().unwrap_or(false);
                let hits: Vec<_> = crate::references::grep_docs(&profile, query, regex)
                    .into_iter()
                    .map(|(doc_id, label, snippet)| serde_json::json!({"doc_id": doc_id, "label": label, "snippet": snippet}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }

            // §8 control
            "submit_review" => {
                let approved = input["approved"].as_bool().unwrap_or(false);
                let report = input["report"].as_str().unwrap_or_default().to_string();
                self.review = Some(ReviewResult { approved, report });
                ToolReply::Text(if approved {
                    "Review recorded: approved.".into()
                } else {
                    "Review recorded: changes requested, returning to the author.".into()
                })
            }

            other if crate::u2s::is_u2s_tool(other) => {
                let artifact = if crate::u2s::takes_artifact(other) {
                    self.verify_artifact()
                } else {
                    None
                };
                match self.u2s_tools() {
                    Ok(tools) => tools.call(other, input, artifact).await,
                    Err(e) => ToolReply::Error(e),
                }
            }
            other => ToolReply::Error(format!("Unknown tool: {other}")),
        }
    }

    /// The tail of every edit to the document: the previous build no longer
    /// describes it, the edit history records it, and the rules report what
    /// the edit changed.
    async fn edited(&mut self, tool: &str, result: Value) -> ToolReply {
        self.built = None;
        self.snapshot(&format!("AI: {tool}"));
        match self.lint().await {
            Ok(Some(lint)) => ToolReply::Text(json!({ "result": result, "lint": lint }).to_string()),
            Ok(None) => ToolReply::Text(result.to_string()),
            Err(e) => ToolReply::Text(json!({ "result": result, "lint_error": e }).to_string()),
        }
    }

    /// Check every rule and report the rules whose verdict the last edit
    /// changed: the findings it introduced, and the ones it resolved. `None`
    /// for a format without rules.
    async fn lint(&mut self) -> Result<Option<Value>, String> {
        if self.rules.is_empty() {
            return Ok(None);
        }
        self.runner()?;
        let runner = self.runner.as_ref().expect("started above");
        let outcome = u2s_doc_tools::native::check_rules(
            &json!({}),
            self.document.value(),
            Some(&self.schema),
            &self.rules,
            runner,
        )
        .await
        .map_err(|e| e.to_string())?;
        let verdicts = outcome.value["verdicts"].as_array().cloned().unwrap_or_default();
        let mut introduced = Vec::new();
        let mut resolved = Vec::new();
        let mut current = HashMap::new();
        for verdict in &verdicts {
            let id = verdict["rule_id"].as_str().unwrap_or_default().to_string();
            let now = verdict["verdict"].as_str().unwrap_or_default().to_string();
            let before = self.lint.get(&id).map(String::as_str).unwrap_or("positive");
            if now != "positive" && before == "positive" {
                introduced.push(verdict.clone());
            } else if now == "positive" && before != "positive" {
                resolved.push(json!({ "rule_id": id, "title": verdict["title"] }));
            }
            current.insert(id, now);
        }
        self.lint = current;
        Ok(Some(json!({ "introduced": introduced, "resolved": resolved })))
    }

    /// `rule_autofix`: every applicable fix, applied as one edit.
    async fn autofix(&mut self, input: &Value) -> ToolReply {
        let expected = input["expected_revision"].as_u64();
        if expected != Some(self.document.revision().get()) {
            return ToolReply::Error(format!(
                "expected_revision {} does not match the document's revision {}; read it again",
                expected.map_or("(missing)".to_string(), |r| r.to_string()),
                self.document.revision().get()
            ));
        }
        if let Err(e) = self.runner() {
            return ToolReply::Error(e);
        }
        let runner = self.runner.as_ref().expect("started above");
        let result = match u2s_doc_tools::native::autofix_rules(
            input,
            self.document.value(),
            Some(&self.schema),
            &self.rules,
            runner,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => return ToolReply::Error(e.to_string()),
        };
        if !result.changed {
            return ToolReply::Text(
                json!({ "results": result.results, "revision": self.document.revision().get() }).to_string(),
            );
        }
        let ops = json!([{ "op": "replace", "path": "", "value": result.final_output }]);
        let current = self.document.revision();
        if let Err(e) = u2s_jsondoc::patch_apply(&mut self.document, &ops, current) {
            return ToolReply::Error(format!("the fixes could not be applied: {e}"));
        }
        let reply = json!({ "results": result.results, "revision": self.document.revision().get() });
        self.edited("rule_autofix", reply).await
    }

    /// Build the document now if the latest build no longer describes it.
    pub fn ensure_built(&mut self) -> Result<(), String> {
        if self.built.is_some() {
            return Ok(());
        }
        let reply = match self.target {
            OutputTarget::Aem => self.build_aem_package(),
            OutputTarget::Redacto => self.build_redacto_dump(),
        };
        match reply {
            ToolReply::Error(e) => Err(e),
            _ => Ok(()),
        }
    }

    fn build_aem_package(&mut self) -> ToolReply {
        let doc = match u2s_aem_ubs_mcp::UbsAemDocument::from_json(self.document.value()) {
            Ok(doc) => doc,
            Err(e) => return ToolReply::Error(format!("Nothing built: {e}")),
        };
        let build = match u2s_aem_ubs_mcp::encode(&doc) {
            Ok(build) => build,
            Err(e) => return ToolReply::Error(format!("Nothing built: {e}")),
        };
        let validation = validate_package_bytes(&build.package);
        let size = build.package.len();
        let bound = build.bound_package.as_ref().map(Vec::len);
        self.built = Some(Built::Aem {
            package: build.package,
            bound_package: build.bound_package,
            xsd: build.xsd,
        });
        let mut report = format!("Built package ({size} bytes).");
        if let Some(bound) = bound {
            report.push_str(&format!(" With bindRefs: {bound} bytes."));
        }
        match validation {
            Ok(ok) => ToolReply::Text(format!("{report} {ok}")),
            Err(problems) => ToolReply::Text(format!("{report} {problems}")),
        }
    }

    fn build_redacto_dump(&mut self) -> ToolReply {
        let doc: u2s_redacto_ubs_mcp::UbsRedactoDocument =
            match serde_json::from_value(self.document.value().clone()) {
                Ok(doc) => doc,
                Err(e) => return ToolReply::Error(format!("Nothing built: not a UBS Redacto document: {e}")),
            };
        let redacto = match u2s_redacto_ubs_mcp::to_redacto(&doc) {
            Ok(redacto) => redacto,
            Err(e) => return ToolReply::Error(format!("Nothing built: {e}")),
        };
        let dump = match u2s_mapper_redacto::encode(&redacto) {
            Ok(dump) => dump.bytes,
            Err(e) => return ToolReply::Error(format!("Nothing built: {e}")),
        };
        let meta = &redacto.document().metadata;
        let report = json!({
            "document_id": meta.document_id.as_str(),
            "languages": meta.languages.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
            "master_language": meta.master_language.as_str(),
            "assets": redacto.document().assets.len(),
            "has_header": !redacto.document().header.is_empty(),
            "has_footer": !redacto.document().footer.is_empty(),
            "bytes": dump.len(),
        });
        self.built = Some(Built::Redacto { dump });
        ToolReply::Text(report.to_string())
    }

    fn matcher(&mut self) -> Result<&crate::semantic::SemanticMatcher, String> {
        if self.matcher.is_none() {
            self.matcher = Some(crate::semantic::SemanticMatcher::new().map_err(|e| e.to_string())?);
        }
        Ok(self.matcher.as_ref().expect("just loaded"))
    }
}
