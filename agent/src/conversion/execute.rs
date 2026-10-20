//! The tool executor: one match over the catalog's tool names.
//!
//! Each arm is the tool's own work; what every edit shares (dropping the stale
//! build, the `AI:` snapshot, the lint) is [`ConversionAgent::edited`].

use super::view::DocumentView;
use super::*;
use crate::rule_board::RuleState;

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

/// A rule an approving review leaves broken, and the Reviewer's reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Waiver {
    rule: String,
    why: String,
}

/// `submit_review`'s `waivers`: absent means none. Each needs the rule's id
/// and a reason that says something.
fn waivers_of(input: &Value) -> Result<Vec<Waiver>, String> {
    let entries = match input.get("waivers") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(entries)) => entries,
        Some(_) => return Err("waivers is a list of {\"rule\": \"<rule id>\", \"why\": \"...\"}".into()),
    };
    entries
        .iter()
        .map(|entry| {
            let rule = entry["rule"].as_str().map(str::trim).unwrap_or_default();
            let why = entry["why"].as_str().map(str::trim).unwrap_or_default();
            if rule.is_empty() {
                return Err(format!("a waiver needs the rule's id in \"rule\": {entry}"));
            }
            if why.is_empty() {
                return Err(format!(
                    "the waiver of rule {rule} gives no reason: say in \"why\" why the rule cannot be kept"
                ));
            }
            Ok(Waiver { rule: rule.to_string(), why: why.to_string() })
        })
        .collect()
}

/// A read's remaining work, which owns everything it touches.
pub type ReadWork = std::pin::Pin<Box<dyn std::future::Future<Output = ToolReply> + Send>>;

/// Who makes a call on the agent, which decides whether it counts as the
/// current stage's evidence (see [`super::evidence`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// A pipeline stage, or the MCP client: its calls are its evidence.
    Stage,
    /// A judge: it runs on the same agent while the stage that dispatched it
    /// waits, and its calls are not that stage's evidence.
    Judge,
    /// The host outside any stage (a review capture): records nothing.
    Host,
}

impl Caller {
    fn records(&self) -> bool {
        matches!(self, Self::Stage)
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
        if DocumentView::serves(name) {
            let view = self.view();
            let (name, input) = (name.to_string(), input.clone());
            return Some(Box::pin(async move {
                match tokio::task::spawn_blocking(move || view.answer(&name, &input)).await {
                    Ok(reply) => reply,
                    Err(join) => ToolReply::Error(format!("the read failed: {join}")),
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

    /// The judged rules, every one of which a rule check hands to a judge
    /// (or answers from the verdict cache).
    pub fn judged_rules(&self) -> Vec<crate::rules::JudgedRule> {
        self.judged.to_vec()
    }

    /// The scripted part of a rule check: every script's verdict, and the
    /// package checks of the current build when there is one.
    pub async fn check_scripted(&mut self) -> Result<Value, String> {
        let mut report = if self.rules.is_empty() {
            json!({ "verdicts": [] })
        } else {
            self.runner()?;
            let runner = self.runner.as_ref().expect("started above");
            let report = u2s_doc_tools::native::check_rules(
                &u2s_doc_tools::native::CheckRulesArgs::default(),
                self.document.value(),
                Some(&self.schema),
                &self.rules,
                runner,
            )
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
        match name {
            // §1 source
            "get_source_info" => match self.source_documents() {
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
            name if DocumentView::serves(name) => self.view().answer(name, input),
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
                    Arc::make_mut(&mut self.document),
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
            "submit_review" => {
                let approved = input["approved"].as_bool().unwrap_or(false);
                let report = input["report"].as_str().unwrap_or_default().trim().to_string();
                let waivers = match waivers_of(input) {
                    Ok(waivers) => waivers,
                    Err(e) => return ToolReply::Error(format!("submit_review refused: {e}")),
                };
                let waived = if approved {
                    match self.unruled(&waivers) {
                        Ok(waived) => waived,
                        Err(refusal) => {
                            return ToolReply::Error(format!("submit_review(approved=true) refused: {refusal}"));
                        }
                    }
                } else {
                    if !waivers.is_empty() {
                        return ToolReply::Error(
                            "waivers come only with approved=true: a review that sends the form back \
                             says in report which rules the author fixes and which it leaves. Call again \
                             without waivers."
                                .into(),
                        );
                    }
                    if report.is_empty() {
                        return ToolReply::Error(
                            "approved=false needs a report: for each rule the author is to fix, the rule \
                             id, the nodes and what to change there."
                                .into(),
                        );
                    }
                    Vec::new()
                };
                let reply = if approved {
                    "Decision recorded: the form is accepted."
                } else {
                    "Decision recorded: the form goes back to the author."
                };
                self.review = Some(ReviewResult { approved, report, waived });
                ToolReply::Text(reply.into())
            }

            "finish_authoring" => {
                if let Some(refusal) = self.unverified("finish_authoring") {
                    return refusal;
                }
                self.finish = Some(input["summary"].as_str().unwrap_or_default().to_string());
                ToolReply::Text(
                    "Authoring finished: every rule is now checked on this document, and a Reviewer \
                     decides on any that is broken."
                        .into(),
                )
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

    /// The rule gate of an approving `submit_review`: every rule needs a
    /// verdict on the document as it stands, and every rule that verdict
    /// leaves broken (a negative, or a check that gave no verdict) needs the
    /// Reviewer's reason in `waivers`. `Ok` carries the waived rules, one line
    /// each, for the run's result; `Err` says what is missing.
    ///
    /// Coverage is read off the rule board, which keeps each rule's latest
    /// verdict and the revision it was given on. The pipeline checks every
    /// rule before a Reviewer runs, and a Reviewer cannot edit, so a rule
    /// without a verdict here is an engine fault, not the Reviewer's to fix.
    fn unruled(&self, waivers: &[Waiver]) -> Result<Vec<String>, String> {
        let board = self.rule_board.snapshot(self.revision());
        let unknown: Vec<&str> = waivers
            .iter()
            .map(|w| w.rule.as_str())
            .filter(|id| !board.iter().any(|r| r.rule_id == *id))
            .collect();
        if !unknown.is_empty() {
            return Err(format!("waivers name rules that do not exist: {unknown:?}; use the rule ids of the rule report"));
        }
        let unchecked: Vec<String> = board
            .iter()
            .filter(|r| r.outdated || r.state == RuleState::NotChecked)
            .map(|r| format!("{} ({})", r.rule_id, r.title))
            .collect();
        let mut waived = Vec::new();
        let mut open = Vec::new();
        for rule in &board {
            let why = match &rule.state {
                RuleState::Fail { violations } => format!("negative, {violations} violation(s)"),
                RuleState::Unchecked { reason } => format!("no verdict: {reason}"),
                RuleState::Pass | RuleState::NotChecked => continue,
            };
            match waivers.iter().find(|w| w.rule == rule.rule_id) {
                Some(waiver) => waived.push(format!("{} ({}, {why}): {}", rule.rule_id, rule.title, waiver.why)),
                None => open.push(format!("{} ({}): {why}", rule.rule_id, rule.title)),
            }
        }
        // A test that waives the evidence gate waives this one too, and still
        // has the waivers it gave recorded.
        #[cfg(any(test, feature = "test-utils"))]
        if self.evidence_waived {
            return Ok(waived);
        }
        if !unchecked.is_empty() {
            return Err(format!(
                "ENGINE DEFECT: {} rule(s) have no verdict on the document under review, although every \
                 rule is checked before a Reviewer runs. Call submit_review with approved=false, so the \
                 rules are checked again after the author's round, and name this engine defect in \
                 report. Not covered:\n- {}",
                unchecked.len(),
                unchecked.join("\n- ")
            ));
        }
        if !open.is_empty() {
            return Err(format!(
                "{} rule(s) are broken on the document under review. Send them back to the author \
                 (approved=false, with what to fix in report), or, where the rule cannot be kept on \
                 this form, say why in waivers: [{{\"rule\": \"<rule id>\", \"why\": \"...\"}}] and \
                 accept again. Not justified:\n- {}",
                open.len(),
                open.join("\n- ")
            ));
        }
        Ok(waived)
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
            &u2s_doc_tools::native::CheckRulesArgs::default(),
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
        let args: u2s_doc_tools::native::AutofixArgs = match serde_json::from_value(input.clone()) {
            Ok(args) => args,
            Err(e) => return ToolReply::Error(format!("rule_autofix's arguments: {e}")),
        };
        let runner = self.runner.as_ref().expect("started above");
        let result = match u2s_doc_tools::native::autofix_rules(
            args.rule_ids.as_deref(),
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
        if let Err(e) = u2s_jsondoc::patch_apply(Arc::make_mut(&mut self.document), &ops, current) {
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
        match self.build_aem_package() {
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
        self.set_built(Some(Built {
            package: build.package.into(),
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
                    "\nENGINE DEFECTS: the package breaks the feedback guard in shapes the writer \
                     owns, so editing the document cannot fix them; report each one:",
                );
                for f in findings {
                    report.push_str(&format!("\n- {} on `{}`: {}", f.problem, f.node, f.detail));
                }
            }
            Err(e) => report.push_str(&format!("\nThe package could not be checked against the feedback guard: {e}")),
        }
        ToolReply::Text(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{JudgedRule, RuleVerdict, Violation};

    fn agent() -> ConversionAgent {
        crate::db::claim_scratch_db_for_test();
        let name = "AAEV_019_EN.pdf";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../forms").join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let session = format!("test-{}", uuid::Uuid::new_v4());
        ConversionAgent::new(vec![(name.to_string(), bytes)], session)
            .expect("the agent starts")
    }

    fn judged(id: &str) -> JudgedRule {
        JudgedRule { id: id.into(), name: id.into(), title: format!("judged {id}"), ..Default::default() }
    }

    fn verdict(pass: bool) -> Result<RuleVerdict, String> {
        let violations = if pass { vec![] } else { vec![Violation { pointer: "/form".into(), message: "m".into() }] };
        Ok(RuleVerdict { pass, violations })
    }

    /// Adds a page, builds it and feeds the evidence gate everything it asks
    /// for, so the Author's hand-over is not refused for want of evidence.
    async fn built_and_verified(agent: &mut ConversionAgent, page: &str) {
        let revision = agent.revision();
        let page = json!({
            "type": "Panel", "uuid": uuid::Uuid::new_v4().to_string(), "name": page,
            "title": {"en": "Details"}, "children": [{
                "type": "TextField", "uuid": uuid::Uuid::new_v4().to_string(),
                "name": format!("TXT_{page}"), "label": {"en": "Last name"}, "mandatory": false,
                "visible": true, "max_chars": null, "colspan": 12, "dor_colspan": null,
                "bind_ref": null, "kind": "Plain"
            }],
            "is_page": true, "visible": true, "is_conditional": false, "dor_num_cols": null,
            "colspan": 12, "dor_colspan": null, "bind_ref": null, "frag_ref": null
        });
        let ops = json!([{ "op": "add", "path": "/form/children/-", "value": page }]);
        let patched = agent.execute("json_patch", &json!({ "ops": ops, "expected_revision": revision })).await;
        assert!(matches!(patched, ToolReply::Text(_)), "{patched:?}");
        let built = agent.execute("build_aem_package", &json!({})).await;
        assert!(matches!(built, ToolReply::Text(_)), "{built:?}");
        let text = |v: Value| ToolReply::Text(v.to_string());
        let e = &mut agent.evidence;
        e.observe_reply(
            "xfa_controls",
            &json!({}),
            &text(json!({ "controls": [], "total": 0, "offset": 0, "next_offset": null, "space_size": 1, "saturated": false })),
        );
        e.observe_call("xfa_render_pages", &json!({ "doc_path": "source.pdf" }));
        e.observe_reply("aem_verify_open", &json!({}), &text(json!({ "session": "s-1", "revision": 0 })));
        e.observe_reply(
            "aem_verify_submit",
            &json!({ "session": "s-1" }),
            &text(json!({ "artefacts": [{ "blob": { "media_type": "application/pdf", "doc_path": "/blobs/dor.pdf" } }] })),
        );
        e.observe_call("pdf_render_pages", &json!({ "doc_path": "/blobs/dor.pdf" }));
        assert!(agent.missing_evidence().is_empty(), "{:?}", agent.missing_evidence());
    }

    /// The rules the board shows broken on the current document: negative, or
    /// without a verdict.
    fn broken(agent: &ConversionAgent) -> Vec<String> {
        agent
            .rule_board()
            .into_iter()
            .filter(|r| matches!(r.state, RuleState::Fail { .. } | RuleState::Unchecked { .. }))
            .map(|r| r.rule_id)
            .collect()
    }

    fn waivers(ids: &[String]) -> Value {
        json!(ids.iter().map(|id| json!({ "rule": id, "why": format!("{id} conflicts with another rule") })).collect::<Vec<_>>())
    }

    /// An approving review, with `waivers`.
    async fn approve(agent: &mut ConversionAgent, waivers: Value) -> Result<String, String> {
        let input = json!({ "approved": true, "report": "", "waivers": waivers });
        match agent.execute("submit_review", &input).await {
            ToolReply::Text(t) => Ok(t),
            ToolReply::Error(e) => Err(e),
            ToolReply::Blocks(_) => Err("blocks".into()),
        }
    }

    /// The Author hands over whatever the rules say: checking them is the
    /// pipeline's, after the hand-over, and only the Reviewer waives a rule.
    #[tokio::test]
    async fn finish_authoring_hands_over_with_rules_broken() {
        let mut agent = agent();
        agent.set_judged_rules(vec![judged("ja")]);
        built_and_verified(&mut agent, "PN_Details").await;
        let revision = agent.revision();
        agent.record_judged(&[(judged("ja"), verdict(false))], revision);

        let reply = agent.execute("finish_authoring", &json!({ "summary": "done" })).await;
        assert!(matches!(&reply, ToolReply::Text(t) if t.contains("every rule is now checked")), "{reply:?}");
        assert_eq!(agent.take_finish().as_deref(), Some("done"));
    }

    /// The false green of 2026-10-09, now at the Reviewer's gate: a rule with
    /// no verdict on the document under review never passes an approval, and
    /// an edit after the check takes the judged verdicts off the document
    /// again.
    #[tokio::test]
    async fn an_approval_needs_every_rule_checked_on_the_document() {
        let mut agent = agent();
        agent.set_judged_rules(vec![judged("ja"), judged("jb")]);
        built_and_verified(&mut agent, "PN_Details").await;

        let revision = agent.revision();
        agent.record_judged(&[(judged("ja"), verdict(true))], revision);
        let all = waivers(&broken(&agent));
        let refused = approve(&mut agent, all).await.unwrap_err();
        assert!(refused.contains("ENGINE DEFECT") && refused.contains("jb"), "{refused}");
        assert!(!refused.contains("- ja "), "ja has a verdict on this revision: {refused}");
        assert!(agent.take_review().is_none(), "a refused approval records nothing");

        agent.record_judged(&[(judged("ja"), verdict(true)), (judged("jb"), verdict(true))], revision);
        let all = waivers(&broken(&agent));
        let accepted = approve(&mut agent, all).await;
        assert!(accepted.is_ok(), "{accepted:?}");
        assert!(agent.take_review().is_some_and(|r| r.approved));

        built_and_verified(&mut agent, "PN_More").await;
        let all = waivers(&broken(&agent));
        let refused = approve(&mut agent, all).await.unwrap_err();
        assert!(refused.contains("ja") && refused.contains("jb"), "{refused}");
    }

    /// Every rule an approval leaves broken needs the Reviewer's reason: one
    /// left out, or one waived without saying why, refuses the approval; with
    /// every reason given it goes through, and the reasons go into the review.
    /// A review that sends the form back takes no waivers.
    #[tokio::test]
    async fn a_negative_verdict_needs_the_reviewers_waiver_with_a_reason() {
        let mut agent = agent();
        agent.set_judged_rules(vec![judged("ja"), judged("jb")]);
        built_and_verified(&mut agent, "PN_Details").await;
        let revision = agent.revision();
        agent.record_judged(&[(judged("ja"), verdict(false)), (judged("jb"), verdict(true))], revision);
        let open = broken(&agent);
        assert!(open.contains(&"ja".to_string()), "{open:?}");

        let others: Vec<String> = open.iter().filter(|id| *id != "ja").cloned().collect();
        let refused = approve(&mut agent, waivers(&others)).await.unwrap_err();
        assert!(refused.contains("ja (judged ja): negative, 1 violation(s)") && refused.contains("waivers"), "{refused}");

        let mut silent = waivers(&others);
        silent.as_array_mut().unwrap().push(json!({ "rule": "ja", "why": "  " }));
        let refused = approve(&mut agent, silent).await.unwrap_err();
        assert!(refused.contains("rule ja gives no reason"), "{refused}");

        let refused = approve(&mut agent, json!([{ "rule": "nope", "why": "x" }])).await.unwrap_err();
        assert!(refused.contains("do not exist") && refused.contains("nope"), "{refused}");

        let rejecting = json!({ "approved": false, "report": "fix ja", "waivers": waivers(&open) });
        assert!(matches!(agent.execute("submit_review", &rejecting).await, ToolReply::Error(e) if e.contains("only with approved=true")));
        let silent = json!({ "approved": false, "report": "  " });
        assert!(matches!(agent.execute("submit_review", &silent).await, ToolReply::Error(e) if e.contains("needs a report")));
        assert!(agent.take_review().is_none(), "a refused decision records nothing");

        let accepted = approve(&mut agent, waivers(&open)).await;
        assert!(accepted.is_ok(), "{accepted:?}");
        let review = agent.take_review().expect("the approval is recorded");
        assert!(
            review.waived.iter().any(|w| w == "ja (judged ja, negative, 1 violation(s)): ja conflicts with another rule"),
            "{:?}",
            review.waived
        );
    }

    /// The Reviewer decides on the rule report and nothing else, so accepting
    /// asks for no verification of its own: a stage that has used no verifier
    /// accepts a form whose every rule passes, while the Author's hand-over
    /// on the same document is refused for want of evidence.
    #[tokio::test]
    async fn accepting_needs_the_rules_but_no_verification() {
        let mut agent = agent();
        agent.set_judged_rules(vec![judged("ja")]);
        built_and_verified(&mut agent, "PN_Details").await;
        let revision = agent.revision();
        agent.record_judged(&[(judged("ja"), verdict(false))], revision);
        agent.begin_stage();
        assert!(!agent.missing_evidence().is_empty(), "a fresh stage has verified nothing");

        let refused = agent.execute("finish_authoring", &json!({ "summary": "done" })).await;
        assert!(matches!(&refused, ToolReply::Error(e) if e.contains("not verified")), "{refused:?}");
        let open = broken(&agent);
        let accepted = approve(&mut agent, waivers(&open)).await;
        assert!(accepted.is_ok(), "{accepted:?}");
        assert!(agent.take_review().is_some_and(|r| r.approved && r.waived.len() == open.len()));

        let sent_back = agent.execute("submit_review", &json!({ "approved": false, "report": "fix ja at /form" })).await;
        assert!(matches!(&sent_back, ToolReply::Text(t) if t.contains("back to the author")), "{sent_back:?}");
        assert!(agent.take_review().is_some_and(|r| !r.approved && r.report == "fix ja at /form"));
    }

    /// Waivers are checked for shape before anything else is said.
    #[test]
    fn waivers_need_a_rule_and_a_reason() {
        assert_eq!(waivers_of(&json!({})), Ok(vec![]));
        assert_eq!(waivers_of(&json!({ "waivers": null })), Ok(vec![]));
        assert!(waivers_of(&json!({ "waivers": "all" })).is_err());
        assert!(waivers_of(&json!({ "waivers": [{ "why": "x" }] })).unwrap_err().contains("rule's id"));
        assert_eq!(
            waivers_of(&json!({ "waivers": [{ "rule": " r ", "why": " because " }] })),
            Ok(vec![Waiver { rule: "r".into(), why: "because".into() }])
        );
    }
}
