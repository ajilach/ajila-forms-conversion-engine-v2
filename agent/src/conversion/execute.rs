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

/// Why `rule_check` leaves a judged rule unchecked where no judge agent runs:
/// the agent on its own. A pipeline stage's `rule_check` dispatches judges
/// instead (`pipeline::judge`).
const NO_JUDGE: &str = "no judge agent runs here: check the document against this rule's \
                        description (rule_list) yourself";

/// Which rules one `rule_check` covers: the scripted ones (`None` for all of
/// them), and the judged ones.
pub struct RuleCheckPlan {
    scripted: Option<Vec<String>>,
    pub judged: Vec<crate::rules::JudgedRule>,
}

/// Why `inspect` cannot run where no inspector agent runs: the agent on its
/// own. A pipeline stage's `inspect` dispatches inspectors (`pipeline::inspect`).
const NO_INSPECTOR: &str = "no inspector agent runs here: do the work of each brief yourself";

/// A read's remaining work, which owns everything it touches.
pub type ReadWork = std::pin::Pin<Box<dyn std::future::Future<Output = ToolReply> + Send>>;

/// Who makes a call on the agent, which decides whether it counts as the
/// current stage's evidence (see [`super::evidence`]) and whether it may drive
/// the verifier while an inspector walks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// A pipeline stage, or the MCP client: its calls are its evidence.
    Stage,
    /// A judge: it runs on the same agent while the stage that dispatched it
    /// waits, and its calls are not that stage's evidence.
    Judge,
    /// An inspector, by its inspection id: it works on the dispatching stage's
    /// behalf, so its calls are that stage's evidence.
    Inspector(String),
    /// The host outside any stage (a review capture): records nothing.
    Host,
}

impl Caller {
    fn records(&self) -> bool {
        matches!(self, Self::Stage | Self::Inspector(_))
    }
}

impl ConversionAgent {
    /// Starts `name` as a read that runs beside the turn's other calls, when
    /// it is one: everything it needs from the agent is taken now, and the
    /// returned work no longer borrows it, so the caller lets go of the agent
    /// before awaiting it. `None` means the call must go through
    /// [`Self::execute`] instead, holding the agent throughout.
    ///
    /// A read that addresses a live form session (`session`) is not one: it
    /// sees whatever revision the turn's `xfa_set` calls have reached, so it
    /// keeps its place in the call order.
    pub async fn start_read(&mut self, name: &str, input: &Value) -> Option<ReadWork> {
        self.start_read_as(name, input, &Caller::Stage).await
    }

    /// [`Self::start_read`], made by `caller`.
    pub async fn start_read_as(&mut self, name: &str, input: &Value, caller: &Caller) -> Option<ReadWork> {
        if access_of(name) != Access::Read || input.get("session").is_some() {
            return None;
        }
        if let Some(refusal) = self.target_refusal(name) {
            return Some(Box::pin(std::future::ready(ToolReply::Error(refusal))));
        }
        if references_mcp::specs::is_reference_tool(name) {
            let server = self.references.clone();
            let (name, input) = (name.to_string(), input.clone());
            return Some(Box::pin(async move {
                let result = tokio::task::spawn_blocking(move || server.dispatch(&name, &input)).await;
                match result {
                    Ok(Ok(result)) => crate::mcp_reply::reply_from_result(result, None),
                    Ok(Err(e)) => ToolReply::Error(e.to_string()),
                    Err(join) => ToolReply::Error(format!("the reference server failed: {join}")),
                }
            }));
        }
        if caller.records() {
            self.evidence.observe_call(name, input);
        }
        let tools = match self.u2s_tools() {
            Ok(tools) => tools,
            Err(e) => return Some(Box::pin(std::future::ready(ToolReply::Error(e)))),
        };
        Some(match tools.start(name, input, None).await {
            Ok(work) => Box::pin(work),
            Err(refusal) => Box::pin(std::future::ready(refusal)),
        })
    }

    /// Resolves `rule_check`'s `rule_ids` (none or empty: every rule) into the
    /// scripted and the judged rules. An id no rule has is an error, not a
    /// silently empty check.
    pub fn rule_check_plan(&self, input: &Value) -> Result<RuleCheckPlan, String> {
        let requested: Vec<String> = match input.get("rule_ids") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(ids)) => ids
                .iter()
                .map(|id| id.as_str().map(str::to_string).ok_or("rule_ids holds only strings"))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("rule_ids is a list of rule ids from rule_list".into()),
        };
        if requested.is_empty() {
            return Ok(RuleCheckPlan { scripted: None, judged: self.judged.clone() });
        }
        let scripted: Vec<String> = self
            .rules
            .iter()
            .map(|r| r.id.to_string())
            .filter(|id| requested.contains(id))
            .collect();
        let judged: Vec<_> = self.judged.iter().filter(|r| requested.contains(&r.id)).cloned().collect();
        let unknown: Vec<&String> = requested
            .iter()
            .filter(|id| !scripted.contains(id) && !judged.iter().any(|r| &r.id == *id))
            .collect();
        if !unknown.is_empty() {
            return Err(format!("no rule has the id {unknown:?}; rule_list lists them"));
        }
        Ok(RuleCheckPlan { scripted: Some(scripted), judged })
    }

    /// The scripted part of a `rule_check`: the scripts' verdicts, and the
    /// package checks of the current build when there is one.
    pub async fn check_scripted(&mut self, plan: &RuleCheckPlan) -> Result<Value, String> {
        let nothing_to_run = self.rules.is_empty() || plan.scripted.as_ref().is_some_and(Vec::is_empty);
        let mut report = if nothing_to_run {
            json!({ "verdicts": [] })
        } else {
            self.runner()?;
            let runner = self.runner.as_ref().expect("started above");
            let input = match &plan.scripted {
                Some(ids) => json!({ "rule_ids": ids }),
                None => json!({}),
            };
            let report =
                u2s_doc_tools::native::check_rules(&input, self.document.value(), Some(&self.schema), &self.rules, runner)
                    .await
                    .map_err(|e| e.to_string())?
                    .value;
            self.record_scripted(&report);
            report
        };
        // The document's build, when it has a current one, is checked too:
        // what the writer made of the document.
        if let (Some(package), Some(object)) = (self.package(), report.as_object_mut()) {
            let findings = match crate::package_checks::check_package(&package) {
                Ok(findings) => json!(findings),
                Err(e) => json!({ "error": e }),
            };
            object.insert("package_findings".into(), findings);
        }
        Ok(report)
    }

    pub async fn execute(&mut self, name: &str, input: &Value) -> ToolReply {
        self.execute_as(name, input, &Caller::Stage).await
    }

    /// [`Self::execute`], made by `caller`.
    pub async fn execute_as(&mut self, name: &str, input: &Value, caller: &Caller) -> ToolReply {
        if let Some(refusal) = self.target_refusal(name) {
            return ToolReply::Error(refusal);
        }
        if let Some(refusal) = self.walk_refusal(name, caller) {
            return ToolReply::Error(refusal);
        }

        match name {
            // §1 source
            "get_source_info" => match self.source_documents(input) {
                Ok(documents) => {
                    let contexts: Vec<&SourceContext> =
                        documents.iter().filter_map(|(_, c, _)| c.as_ref()).collect();
                    let languages = source_languages(&contexts);
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
                let plan = match self.rule_check_plan(input) {
                    Ok(plan) => plan,
                    Err(e) => return ToolReply::Error(e),
                };
                match self.check_scripted(&plan).await {
                    Ok(scripted) => {
                        let judged: Vec<_> = plan
                            .judged
                            .into_iter()
                            .map(|rule| (rule, Err(NO_JUDGE.to_string())))
                            .collect();
                        self.record_judged(&judged, self.revision());
                        ToolReply::Text(crate::rules::merge_rule_report(scripted, &judged).to_string())
                    }
                    Err(e) => ToolReply::Error(e),
                }
            }
            "rule_autofix" => self.autofix(input).await,
            name if DOCUMENT_TOOLS.iter().any(|t| t.name() == name) => {
                let tool = *DOCUMENT_TOOLS.iter().find(|t| t.name() == name).expect("matched");
                if tool == NativeJsonTool::Patch
                    && let Err(e) = self.lint_baseline().await
                {
                    return ToolReply::Error(e);
                }
                match u2s_doc_tools::native::dispatch(
                    tool,
                    input,
                    &mut self.document,
                    Some(&self.schema),
                    &self.rules,
                ) {
                    Ok(outcome) if outcome.mutated => self.edited(name, outcome.value).await,
                    // One list holds every rule, the judged ones too.
                    Ok(outcome) if tool == NativeJsonTool::ListRules => {
                        ToolReply::Text(crate::rules::list_with_judged(outcome.value, &self.judged).to_string())
                    }
                    Ok(outcome) => ToolReply::Text(outcome.value.to_string()),
                    Err(e) => ToolReply::Error(e.to_string()),
                }
            }

            // §3 building
            "build_aem_package" => self.build_aem_package(),
            "build_redacto_dump" => self.build_redacto_dump(),
            "get_package_info" => match self.package() {
                Some(pkg) => {
                    let files = references_mcp::unzip_package(&pkg).unwrap_or_default();
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
                    Some(pkg) => match references_mcp::unzip_package(&pkg) {
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

            "coverage_check" => {
                let language = match input.get("language") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(language)) => Some(language.as_str()),
                    Some(other) => return ToolReply::Error(format!("language must be a string, not {other}")),
                };
                match crate::coverage::check(&self.current_pdfs, self.document.value(), language) {
                    Ok(report) => ToolReply::Text(report.to_string()),
                    Err(e) => ToolReply::Error(e),
                }
            }

            // §7 references: typed arguments, validated by the server itself.
            other if references_mcp::specs::is_reference_tool(other) => {
                match self.references.dispatch(other, input) {
                    Ok(result) => crate::mcp_reply::reply_from_result(result, None),
                    Err(e) => ToolReply::Error(e.to_string()),
                }
            }

            // §8 control
            "submit_rule_verdict" => {
                let Some(judgement) = input["judgement"].as_str() else {
                    return ToolReply::Error("submit_rule_verdict needs the judgement id you were given".into());
                };
                let verdict = match serde_json::from_value::<crate::rules::RuleVerdict>(input.clone()) {
                    Ok(verdict) => verdict,
                    Err(e) => return ToolReply::Error(format!("submit_rule_verdict: {e}")),
                };
                if let Err(e) = verdict.validate() {
                    return ToolReply::Error(format!("submit_rule_verdict: {e}"));
                }
                match self.judgements.get_mut(judgement) {
                    Some(slot @ None) => {
                        let pass = verdict.pass;
                        *slot = Some(verdict);
                        ToolReply::Text(format!("Verdict recorded: {}.", if pass { "kept" } else { "broken" }))
                    }
                    Some(Some(_)) => ToolReply::Error("this judgement already has its verdict".into()),
                    None => ToolReply::Error(format!(
                        "no open judgement {judgement:?}: use the judgement id you were given"
                    )),
                }
            }
            "submit_findings" => {
                let Some(inspection) = input["inspection"].as_str() else {
                    return ToolReply::Error("submit_findings needs the inspection id you were given".into());
                };
                let findings = match crate::findings::Findings::from_input(input) {
                    Ok(findings) => findings,
                    Err(e) => return ToolReply::Error(format!("submit_findings: {e}")),
                };
                match self.inspections.get_mut(inspection) {
                    Some(slot @ None) => {
                        *slot = Some(findings);
                        ToolReply::Text("Findings recorded.".into())
                    }
                    Some(Some(_)) => ToolReply::Error("this inspection already has its findings".into()),
                    None => ToolReply::Error(format!(
                        "no open inspection {inspection:?}: use the inspection id you were given"
                    )),
                }
            }
            "inspect" => ToolReply::Error(NO_INSPECTOR.into()),
            "submit_review" => {
                let approved = input["approved"].as_bool().unwrap_or(false);
                let report = input["report"].as_str().unwrap_or_default().to_string();
                if approved && let Some(refusal) = self.unverified("submit_review(approved=true)") {
                    return refusal;
                }
                self.review = Some(ReviewResult { approved, report });
                ToolReply::Text(if approved {
                    "Review recorded: approved.".into()
                } else {
                    "Review recorded: changes requested, returning to the author.".into()
                })
            }

            "finish_authoring" => {
                if let Some(refusal) = self.unverified("finish_authoring") {
                    return refusal;
                }
                self.finish = Some(input["summary"].as_str().unwrap_or_default().to_string());
                ToolReply::Text("Authoring finished: handing the form to the Reviewer.".into())
            }

            other if crate::u2s::is_u2s_tool(other) => {
                let artifact = if crate::u2s::takes_artifact(other) {
                    self.verify_artifact()
                } else {
                    None
                };
                if caller.records() {
                    self.evidence.observe_call(other, input);
                }
                let reply = match self.u2s_tools() {
                    Ok(tools) => tools.call(other, input, artifact).await,
                    Err(e) => ToolReply::Error(e),
                };
                if caller.records() {
                    self.evidence.observe_reply(other, input, &reply);
                }
                reply
            }
            other => ToolReply::Error(format!("Unknown tool: {other}")),
        }
    }

    /// The refusal of a verifier call while an inspector walks the verifier,
    /// unless that inspector makes it: the verifier holds one form, which one
    /// caller at a time drives.
    fn walk_refusal(&self, name: &str, caller: &Caller) -> Option<String> {
        let walker = self.walker.as_deref()?;
        let verifier = name.starts_with("aem_verify_") || name.starts_with("redacto_verify_");
        let walking = matches!(caller, Caller::Inspector(id) if id == walker);
        (verifier && !walking).then(|| {
            format!("{name} refused: an inspector is walking the verifier; wait for its inspect call to return")
        })
    }

    /// The refusal of a terminal `call` the stage has not earned yet: what
    /// it still has to verify (see [`super::evidence`]).
    fn unverified(&self, call: &str) -> Option<ToolReply> {
        let missing = self.missing_evidence();
        (!missing.is_empty()).then(|| {
            ToolReply::Error(format!(
                "{call} refused: this stage has not verified the form yet. Do these first, then call \
                 it again:\n- {}",
                missing.join("\n- ")
            ))
        })
    }

    /// The tail of every edit to the document: the previous build no longer
    /// describes it, the edit history records it, and the rules report what
    /// the edit changed.
    async fn edited(&mut self, tool: &str, result: Value) -> ToolReply {
        self.set_built(None);
        self.snapshot(&format!("AI: {tool}"));
        match self.lint().await {
            Ok(Some(lint)) => ToolReply::Text(json!({ "result": result, "lint": lint }).to_string()),
            Ok(None) => ToolReply::Text(result.to_string()),
            Err(e) => ToolReply::Text(json!({ "result": result, "lint_error": e }).to_string()),
        }
    }

    /// Every rule's verdict on the document as it stands.
    async fn verdicts(&mut self) -> Result<HashMap<String, (String, Value)>, String> {
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
        self.record_scripted(&outcome.value);
        Ok(outcome.value["verdicts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|v| {
                let id = v["rule_id"].as_str().unwrap_or_default().to_string();
                let verdict = v["verdict"].as_str().unwrap_or_default().to_string();
                (id, (verdict, v))
            })
            .collect())
    }

    /// Puts a scripted check's report (`check_rules`' output) on the rule
    /// board, against the document's current revision.
    fn record_scripted(&mut self, report: &Value) {
        let revision = self.revision();
        let verdicts = report["verdicts"].as_array().map(Vec::as_slice).unwrap_or_default();
        self.rule_board.record_scripted(verdicts, revision);
    }

    /// Brings the rule board's scripted verdicts up to the current document,
    /// when no check has yet: a stage starting on a seeded or resumed document
    /// shows where it stands before its first edit.
    pub async fn refresh_rules(&mut self) -> Result<(), String> {
        if self.rules.is_empty() || self.rule_board.scripted_current(self.revision()) {
            return Ok(());
        }
        // Before the run's first edit this check is also its lint baseline,
        // which then need not be taken again.
        if self.lint.is_none() {
            return self.lint_baseline().await;
        }
        self.verdicts().await.map(|_| ())
    }

    /// Take the lint baseline from the document before this run's first edit.
    async fn lint_baseline(&mut self) -> Result<(), String> {
        if self.rules.is_empty() || self.lint.is_some() {
            return Ok(());
        }
        let verdicts = self.verdicts().await?;
        self.lint = Some(verdicts.into_iter().map(|(id, (v, _))| (id, v)).collect());
        Ok(())
    }

    /// Check every rule and report the rules whose verdict the last edit
    /// changed against the baseline: the findings it introduced, and the ones
    /// it resolved. `None` for a format without rules.
    async fn lint(&mut self) -> Result<Option<Value>, String> {
        if self.rules.is_empty() {
            return Ok(None);
        }
        let verdicts = self.verdicts().await?;
        let before = self.lint.take().unwrap_or_default();
        let mut introduced = Vec::new();
        let mut resolved = Vec::new();
        let mut current = HashMap::new();
        for (id, (now, verdict)) in verdicts {
            let was = before.get(&id).map(String::as_str).unwrap_or("positive");
            if now != "positive" && was == "positive" {
                introduced.push(verdict);
            } else if now == "positive" && was != "positive" {
                resolved.push(json!({ "rule_id": id, "title": verdict["title"] }));
            }
            current.insert(id, now);
        }
        self.lint = Some(current);
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
        if let Err(e) = self.lint_baseline().await.and_then(|()| self.runner().map(|_| ())) {
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
        // A package that fails its own validation is not stored: nothing
        // invalid is verified or exported.
        let valid = match validate_package_bytes(&build.package) {
            Ok(valid) => valid,
            Err(problems) => return ToolReply::Error(format!("Nothing built: {problems}")),
        };
        let findings = crate::package_checks::check_package(&build.package);
        let size = build.package.len();
        let bound = build.bound_package.as_ref().map(Vec::len);
        self.set_built(Some(Built::Aem {
            package: build.package,
            bound_package: build.bound_package,
            xsd: build.xsd,
        }));
        let mut report = format!("Built package ({size} bytes).");
        if let Some(bound) = bound {
            report.push_str(&format!(" With bindRefs: {bound} bytes."));
        }
        report.push_str(&format!(" {valid}"));
        match findings {
            Ok(findings) if findings.is_empty() => {}
            Ok(findings) => {
                report.push_str(
                    "\nENGINE DEFECTS: the package breaks the feedback guard in shapes the templates \
                     and the writer own, so editing the document cannot fix them; report each one:",
                );
                for f in findings {
                    report.push_str(&format!("\n- {} on `{}`: {}", f.problem, f.node, f.detail));
                }
            }
            Err(e) => report.push_str(&format!("\nThe package could not be checked against the feedback guard: {e}")),
        }
        ToolReply::Text(report)
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
        self.set_built(Some(Built::Redacto { dump }));
        ToolReply::Text(report.to_string())
    }
}
