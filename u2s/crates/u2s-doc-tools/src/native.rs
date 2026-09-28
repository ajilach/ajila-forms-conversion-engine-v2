//! The native JSON tools: the Conversion Agent's whole means of touching the
//! output document.
//!
//! PLAN.md: "The JSON editing tools are native, not a server. The working
//! output document lives in u2s's own database, so routing edits through an
//! external process would mean shipping the document out and syncing
//! revisions back for no benefit." The agent sees the same tool surface as
//! for an MCP tool; dispatch happens in-process against `u2s-jsondoc`.
//!
//! The design is "outline plus path-addressed surgical edits, never
//! whole-tree", because a real UBS form's output is around 253 KB and never
//! fits in context. Two properties follow and both are enforced here rather
//! than trusted:
//!
//! - `json_search` returns **pointers and counts, never bulk values**, so a
//!   search cannot be used to exfiltrate the document a match at a time.
//! - `json_patch` requires an `expected_revision`, so no edit lands on a
//!   version the agent has not seen. A stale revision is a *tool* failure --
//!   content the model reads and retries from -- never a run failure.
//!
//! `json_validate` is the one tool that composes two crates: `u2s-jsondoc`
//! holds the document and `u2s_schema::validate` checks it, so the workspace
//! keeps exactly one validator rather than growing a second one here.
//!
//! `rule_list`/`rule_check` read a different resource: not the working
//! document, but the dataset's active rules -- pre-fetched by the service
//! layer into [`RuleForCheck`] and threaded through `u2s_agent`'s `RunState`, the
//! same way `schema` already is, since `u2s-agent` must never depend on
//! `u2s-store` to fetch them itself.
//!
//! `rule_check` is the one tool here that is **not** dispatched in process.
//! It evaluates LLM-generated JavaScript, and that goes to
//! `u2s-rules-host`'s worker processes -- the same runner `u2s-server`'s
//! deterministic between-rounds pass uses, so a mid-round check and the
//! authoritative one after it can never disagree about what one script
//! says, and neither can take the server down.
//!
//! `rule_propose` is the other tool answered outside this module, and for
//! the opposite reason: it writes rather than reads. It asks the Rule Agent
//! to draft a **new** rule, which needs a model, the dataset's reference
//! corpus and the store -- none of which this crate may reach -- so it goes
//! out through `u2s_agent`'s `RuleProposer`, the same kind of seam
//! `ToolTransport` and `BlobSink` already are. The one thing it cannot do is
//! make a rule enforce: it only ever produces a draft a person must approve,
//! which is why offering it to an agent adds no authority the agent did not
//! already have.
//!
//! [`NativeJsonTool::route`] is what keeps those three exceptions honest --
//! a new tool that forgets to say where it runs is a compile error rather
//! than a silent fall-through into [`dispatch`], which is synchronous and
//! reaches neither the sandbox nor the store.

use serde_json::{Value, json};
use u2s_jsondoc::Document;
use u2s_rules_host::protocol::CheckRequest;
use u2s_rules_host::runner::RuleRunner;

/// One active rule, pre-fetched for `rule_list`/`rule_check` to read.
/// `script_js` is carried only for dispatch to run against -- `rule_list`
/// never puts it in front of the model, matching the retrigger loop's own
/// "rule titles and descriptions only" prompt discipline.
#[derive(Debug, Clone)]
pub struct RuleForCheck {
    pub id: u2s_core::RuleId,
    pub title: String,
    pub description_md: String,
    pub script_js: String,
    /// This rule's mechanical repair, if it has one. `None` is the common
    /// case. Carried the same way `script_js` is -- never put in front of
    /// the model by `rule_list`, only ever run, by `rule_autofix` and the
    /// mid-patch lint's `autofix_available` flag.
    pub fix_js: Option<String>,
    /// This rule's declared facts, resolved for the run's input when the
    /// run started. `Indeterminate` means the check is never run here and
    /// is reported as such; the values themselves are only ever handed to
    /// the sandbox, never shown to the model.
    pub facts: u2s_facts::FactsForCheck,
}

/// Which native tool. A closed enum rather than a name string, so dispatch is
/// exhaustive and adding one is a compile error everywhere it matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeJsonTool {
    Outline,
    Get,
    Search,
    Patch,
    Validate,
    ListRules,
    CheckRules,
    Autofix,
    Propose,
    TryRule,
    ListFacts,
    GetFact,
    ProposeFact,
}

/// Where a native tool's body actually runs.
///
/// A closed enum rather than a pair of booleans, because the three cases are
/// mutually exclusive and two independent predicates could say otherwise.
/// `bridge::execute` matches on it, so a tool added without deciding this is
/// a compile error there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Bounded pure Rust against the working document, in [`dispatch`].
    InProcess,
    /// Evaluates LLM-generated JavaScript, so it must **never** run on a
    /// thread of ours: boa has no interrupt hook and no heap cap (see
    /// `u2s-rules`), so a script that evades the loop and recursion limits
    /// either hangs that thread or aborts the process outright. An abort is
    /// not an unwind, so neither `catch_unwind` nor `spawn_blocking`
    /// contains it. It goes to `u2s-rules-host`'s worker pool, which gives
    /// each script its own process, memory and CPU ceiling, and a deadline
    /// the host can act on.
    Sandbox,
    /// Answered by the application through `u2s_agent`'s `RuleProposer`, because
    /// it needs a model and the store.
    Proposer,
    /// Answered by the application through `u2s_agent`'s `RuleDryRunner`, because
    /// it needs the dataset's reference corpus and the sandbox runner rather
    /// than the working document -- the Rule Agent that calls it has none.
    DryRunner,
    /// Answered by the application through `u2s_agent`'s `FactWorkbench`, because
    /// facts live in the store and extracting one needs a model, the page
    /// renders and the sandbox.
    FactWorkbench,
}

impl NativeJsonTool {
    pub const ALL: &'static [NativeJsonTool] = &[
        NativeJsonTool::Outline,
        NativeJsonTool::Get,
        NativeJsonTool::Search,
        NativeJsonTool::Patch,
        NativeJsonTool::Validate,
        NativeJsonTool::ListRules,
        NativeJsonTool::CheckRules,
        NativeJsonTool::Autofix,
        NativeJsonTool::Propose,
        NativeJsonTool::TryRule,
        NativeJsonTool::ListFacts,
        NativeJsonTool::GetFact,
        NativeJsonTool::ProposeFact,
    ];

    /// What a conversion's agents may be offered: every tool except the
    /// fact tools. Facts exist to check a conversion independently of it, so
    /// no agent taking part in one may read or propose them -- enforced by
    /// the offer itself, not by a wiring that happens to leave the
    /// workbench out.
    pub const OFFERED_TO_RUNS: &'static [NativeJsonTool] = &[
        NativeJsonTool::Outline,
        NativeJsonTool::Get,
        NativeJsonTool::Search,
        NativeJsonTool::Patch,
        NativeJsonTool::Validate,
        NativeJsonTool::ListRules,
        NativeJsonTool::CheckRules,
        NativeJsonTool::Autofix,
        NativeJsonTool::Propose,
        NativeJsonTool::TryRule,
    ];

    pub fn name(self) -> &'static str {
        match self {
            NativeJsonTool::Outline => "json_outline",
            NativeJsonTool::Get => "json_get",
            NativeJsonTool::Search => "json_search",
            NativeJsonTool::Patch => "json_patch",
            NativeJsonTool::Validate => "json_validate",
            NativeJsonTool::ListRules => "rule_list",
            NativeJsonTool::CheckRules => "rule_check",
            NativeJsonTool::Autofix => "rule_autofix",
            NativeJsonTool::Propose => "rule_propose",
            NativeJsonTool::TryRule => "rule_try",
            NativeJsonTool::ListFacts => "fact_list",
            NativeJsonTool::GetFact => "fact_get",
            NativeJsonTool::ProposeFact => "fact_propose",
        }
    }

    /// Prompt surface. Each says what the tool will *not* do, because the
    /// caps are the part a model needs to plan around.
    pub fn description(self) -> &'static str {
        match self {
            NativeJsonTool::Outline => {
                "Structure of the output document at a JSON Pointer: one line per node with its \
                 path, type and a short excerpt. Depth- and count-capped, so it never returns the \
                 whole document. Start here, then json_get the part you need."
            }
            NativeJsonTool::Get => {
                "Read a subtree at a JSON Pointer. Depth-capped (deeper nodes are elided) and then \
                 character-windowed, and it reports when it truncated. Use offset to continue."
            }
            NativeJsonTool::Search => {
                "Find where a string occurs in the document. Returns JSON Pointers and counts with \
                 a short context snippet -- never the matched values themselves. Follow up with \
                 json_get on a pointer you care about."
            }
            NativeJsonTool::Patch => {
                "Apply RFC 6902 operations atomically. Requires expected_revision: if the document \
                 has moved on, the patch is refused and you must re-read before retrying. Include \
                 'test' operations to make a conditional write safe."
            }
            NativeJsonTool::Validate => {
                "Validate the whole output document against the output format's JSON Schema. \
                 Returns violations as JSON Pointers. Read-only."
            }
            NativeJsonTool::ListRules => {
                "List this dataset's active rules by id, title and description. Read-only. Use \
                 this to see what a rule_check verdict is checking before you call it."
            }
            NativeJsonTool::CheckRules => {
                "Run the current document through active rules' check scripts, the same checks \
                 that run after you finish. Omit rule_ids to check every active rule, or name a \
                 subset. Returns each rule's verdict and violations. Read-only -- this never \
                 replaces the deterministic pass that actually records the run's verdicts. Every \
                 json_patch result already tells you what changed, so you rarely need to call \
                 this yourself -- it is here for a check you want without editing anything."
            }
            NativeJsonTool::Autofix => {
                "Apply the mechanical repair of every failing rule that has one. Requires \
                 expected_revision, the same as json_patch, and is refused on the same terms if \
                 the document has moved on. Only ever touches a rule marked \
                 autofix_available in a json_patch result or a rule_check verdict -- a rule with \
                 no mechanical repair, or whose repair does not actually resolve it, is reported \
                 and left untouched, never guessed at. Omit rule_ids to attempt every fixable \
                 rule, or name a subset."
            }
            NativeJsonTool::Propose => {
                "Propose a NEW rule for this dataset, described in prose. A person reviews and \
                 approves it before it ever constrains anything, so this changes nothing about \
                 the document you are reviewing and nothing about this run -- it does not \
                 activate a rule, and the rule will not be checked here. Use it for a mistake \
                 you expect to recur across documents, not for a one-off you should simply \
                 report as a finding. Describe the condition and when it should fail; the script \
                 is written for you. Proposing the same rule twice creates two proposals \
                 somebody then has to read, and the number of proposals one run may make is \
                 capped."
            }
            NativeJsonTool::TryRule => {
                "Try a candidate check script, and optionally a fix script, against every \
                 reference output in this dataset. Nothing is saved and no document is changed: \
                 the fix is proved against a throwaway copy and its operations are reported, \
                 never applied. Returns each reference's verdict, the violations your check \
                 reported, why it broke if it broke, and whether your fix actually repaired what \
                 it found. Call this before you answer, and call it again after you change the \
                 script."
            }
            NativeJsonTool::ListFacts => {
                "List this dataset's facts: named questions about an input, with a strict answer \
                 schema, that an extrinsic check reads as ctx.facts.<key> after declaring it in \
                 `const requires = [\"<key>\"]`. Returns each fact's key, question, schema, \
                 source and how many rules read it -- never its values. Look here before \
                 proposing a fact: reuse one that already answers your question."
            }
            NativeJsonTool::GetFact => {
                "One fact's definition and a few of its extracted values on reference inputs, so \
                 you can see what ctx.facts.<key> will hold before relying on it."
            }
            NativeJsonTool::ProposeFact => {
                "Propose a new fact, or a revised question for an existing one, for the rule you \
                 are drafting. It is extracted right away on a sample of reference inputs and \
                 the values (or why extraction failed) are returned; nothing is saved for other \
                 rules until this rule is saved. Ask for lists in document order, not counts. \
                 Use source \"ingest_script\" with an `extract(ingest)` script when the input's \
                 outline can answer mechanically; use \"llm\" otherwise. An LLM fact must give \
                 the same answer every time: if two extractions of a sample input disagree, the \
                 rule cannot be saved."
            }
        }
    }

    /// Everything but a mutation. The duplicate-call guard uses this: a
    /// repeated read is a wasted turn worth deflecting, while a repeated
    /// patch, autofix or proposal may be a legitimate second one.
    ///
    /// `rule_propose` counts as a mutation even though it never touches the
    /// document: a second identical call is a second proposal somebody has
    /// to read, not a repeat of an answer that already stands.
    pub fn is_idempotent(self) -> bool {
        !matches!(
            self,
            NativeJsonTool::Patch
                | NativeJsonTool::Autofix
                | NativeJsonTool::Propose
                | NativeJsonTool::ProposeFact
        )
    }

    /// Where this tool's body runs. See [`Route`] for why each case is not
    /// simply [`dispatch`], which is synchronous and touches only the
    /// working document.
    pub fn route(self) -> Route {
        match self {
            NativeJsonTool::CheckRules | NativeJsonTool::Autofix => Route::Sandbox,
            NativeJsonTool::Propose => Route::Proposer,
            NativeJsonTool::TryRule => Route::DryRunner,
            NativeJsonTool::ListFacts | NativeJsonTool::GetFact | NativeJsonTool::ProposeFact => {
                Route::FactWorkbench
            }
            NativeJsonTool::Outline
            | NativeJsonTool::Get
            | NativeJsonTool::Search
            | NativeJsonTool::Patch
            | NativeJsonTool::Validate
            | NativeJsonTool::ListRules => Route::InProcess,
        }
    }

    /// Hand-written `Value` schemas, matching the convention the three MCP
    /// servers already follow: schemas as plain JSON rather than `schemars`
    /// derives, so they are readable in one place and decoupled from any
    /// derive-macro version.
    pub fn input_schema(self) -> Value {
        match self {
            NativeJsonTool::Outline => json!({
                "type": "object",
                "properties": {
                    "pointer": { "type": "string", "description": "RFC 6901 pointer; \"\" is the root" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 6, "default": 2 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
                },
                "required": ["pointer"],
                "additionalProperties": false
            }),
            NativeJsonTool::Get => json!({
                "type": "object",
                "properties": {
                    "pointer": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 8, "default": 4 },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20000, "default": 4000 }
                },
                "required": ["pointer"],
                "additionalProperties": false
            }),
            NativeJsonTool::Search => json!({
                "type": "object",
                "properties": {
                    "pointer": { "type": "string", "description": "subtree to search; \"\" is the root" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
                },
                "required": ["pointer", "query"],
                "additionalProperties": false
            }),
            NativeJsonTool::Patch => json!({
                "type": "object",
                "properties": {
                    "ops": {
                        "type": "array",
                        "description": "RFC 6902 operations, applied atomically",
                        "items": { "type": "object" }
                    },
                    "expected_revision": { "type": "integer", "minimum": 0 }
                },
                "required": ["ops", "expected_revision"],
                "additionalProperties": false
            }),
            NativeJsonTool::Validate => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            NativeJsonTool::ListRules => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            NativeJsonTool::CheckRules => json!({
                "type": "object",
                "properties": {
                    "rule_ids": {
                        "type": "array",
                        "description": "Subset of rule ids to check; omit or empty for every active rule",
                        "items": { "type": "string" }
                    }
                },
                "additionalProperties": false
            }),
            NativeJsonTool::Autofix => json!({
                "type": "object",
                "properties": {
                    "rule_ids": {
                        "type": "array",
                        "description": "Subset of rule ids to autofix; omit or empty for every fixable rule",
                        "items": { "type": "string" }
                    },
                    "expected_revision": { "type": "integer", "minimum": 0 }
                },
                "required": ["expected_revision"],
                "additionalProperties": false
            }),
            NativeJsonTool::Propose => json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "A short name for the rule, as a reviewer will see it in a list"
                    },
                    "description_md": {
                        "type": "string",
                        "description": "What the rule must check and when it should fail, in prose. \
                                        Name the JSON Pointers involved and give a concrete example \
                                        of output that should fail -- this is the whole brief the \
                                        script is written from, and a reviewer reads it too."
                    }
                },
                "required": ["title", "description_md"],
                "additionalProperties": false
            }),
            NativeJsonTool::TryRule => json!({
                "type": "object",
                "properties": {
                    "check_js": { "type": "string" },
                    "fix_js": { "type": ["string", "null"] }
                },
                "required": ["check_js"],
                "additionalProperties": false
            }),
            NativeJsonTool::ListFacts => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            NativeJsonTool::GetFact => json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" }
                },
                "required": ["key"],
                "additionalProperties": false
            }),
            NativeJsonTool::ProposeFact => json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Lowercase snake case, e.g. source_fields. The name the \
                                        check reads it under."
                    },
                    "question_md": {
                        "type": "string",
                        "description": "The question, answerable from the input alone."
                    },
                    "answer_schema": {
                        "type": "object",
                        "description": "A strict JSON Schema every answer must satisfy."
                    },
                    "source": { "type": "string", "enum": ["llm", "ingest_script"] },
                    "script_js": {
                        "type": ["string", "null"],
                        "description": "Required for ingest_script: `function extract(ingest)` \
                                        over ingest.files[i].{filename, page_count, outline, \
                                        text}, where outline is [{path, kind}] or null."
                    }
                },
                "required": ["key", "question_md", "answer_schema", "source"],
                "additionalProperties": false
            }),
        }
    }
}

/// A native tool call that could not be carried out.
///
/// Every variant is **content the model reads**, not a run failure: a bad
/// pointer, a stale revision and a rejected patch are all things a model can
/// recover from by reading and retrying, and failing the conversion for them
/// would throw away a working run over a recoverable mistake.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeToolError {
    #[error("argument {field:?} is missing or the wrong type")]
    BadArgument { field: &'static str },
    #[error("pointer {pointer:?} does not resolve in the document")]
    BadPointer { pointer: String },
    #[error(
        "the document is at revision {actual}, not {expected}; \
         re-read the part you are editing and retry with the current revision"
    )]
    StaleRevision { expected: u64, actual: u64 },
    #[error("the patch was rejected and nothing was changed: {detail}")]
    PatchRejected { detail: String },
    #[error("no output schema is available, so validation cannot run")]
    NoSchema,
    #[error(
        "rule_propose is unavailable because this agent has no rule proposer wired; \
         nothing was proposed"
    )]
    NoProposer,
    #[error("the rule was not proposed: {detail}")]
    ProposalRefused { detail: String },
    #[error(
        "rule_try is unavailable because this agent has no dry runner wired; nothing was tried"
    )]
    NoDryRunner,
    #[error("the script could not be tried: {detail}")]
    DryRunFailed { detail: String },
    #[error(
        "the fact tools are unavailable because this agent has no fact workbench wired; \
         nothing was read or proposed"
    )]
    NoFactWorkbench,
    #[error("{detail}")]
    FactToolFailed { detail: String },
}

/// A tool call's result plus whether it changed the document, so a caller
/// knows when to bump a revision without inspecting the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeOutcome {
    pub value: Value,
    pub mutated: bool,
}

fn pointer_arg(args: &Value) -> Result<String, NativeToolError> {
    args.get("pointer")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(NativeToolError::BadArgument { field: "pointer" })
}

fn usize_arg(args: &Value, field: &'static str, default: usize) -> usize {
    args.get(field)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(default)
}

/// Dispatches one native tool call against `doc`.
///
/// Takes `&mut Document` because [`NativeJsonTool::Patch`] mutates it in
/// place. That is the point of the design: the document is never returned to
/// the model, so it never has to fit in a context window.
pub fn dispatch(
    tool: NativeJsonTool,
    args: &Value,
    doc: &mut Document,
    schema: Option<&Value>,
    rules: &[RuleForCheck],
) -> Result<NativeOutcome, NativeToolError> {
    match tool {
        NativeJsonTool::Outline => {
            let pointer = pointer_arg(args)?;
            let outline = u2s_jsondoc::outline(
                doc.value(),
                &pointer,
                usize_arg(args, "depth", 2),
                usize_arg(args, "limit", 100),
            )
            .map_err(|_| NativeToolError::BadPointer {
                pointer: pointer.clone(),
            })?;
            Ok(NativeOutcome {
                value: serde_json::to_value(&outline).expect("an outline is plain data"),
                mutated: false,
            })
        }
        NativeJsonTool::Get => {
            let pointer = pointer_arg(args)?;
            let got = u2s_jsondoc::get(
                doc.value(),
                &pointer,
                usize_arg(args, "depth", 4),
                usize_arg(args, "offset", 0),
                usize_arg(args, "limit", 4_000),
            )
            .map_err(|_| NativeToolError::BadPointer {
                pointer: pointer.clone(),
            })?;
            Ok(NativeOutcome {
                value: serde_json::to_value(&got).expect("a get result is plain data"),
                mutated: false,
            })
        }
        NativeJsonTool::Search => {
            let pointer = pointer_arg(args)?;
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or(NativeToolError::BadArgument { field: "query" })?;
            let found =
                u2s_jsondoc::search(doc.value(), &pointer, query, usize_arg(args, "limit", 50))
                    .map_err(|_| NativeToolError::BadPointer {
                        pointer: pointer.clone(),
                    })?;
            Ok(NativeOutcome {
                value: serde_json::to_value(&found).expect("a search result is plain data"),
                mutated: false,
            })
        }
        NativeJsonTool::Patch => {
            let ops = args
                .get("ops")
                .filter(|ops| ops.is_array())
                .ok_or(NativeToolError::BadArgument { field: "ops" })?;
            let expected = args
                .get("expected_revision")
                .and_then(Value::as_u64)
                .ok_or(NativeToolError::BadArgument {
                    field: "expected_revision",
                })?;

            // The staleness check is ours, deliberately. `u2s_jsondoc`'s
            // `Revision` has no `From<u64>` and must not grow one: its
            // invariant is that a revision always traces back to an actual
            // read of the document, which is exactly what stops a caller
            // from inventing one. So compare the model-supplied integer
            // here, and hand `patch_apply` the document's own revision only
            // once it matches.
            let actual = doc.revision().get();
            if expected != actual {
                return Err(NativeToolError::StaleRevision { expected, actual });
            }

            let current = doc.revision();
            u2s_jsondoc::patch_apply(doc, ops, current).map_err(|err| {
                NativeToolError::PatchRejected {
                    detail: err.to_string(),
                }
            })?;

            Ok(NativeOutcome {
                value: json!({
                    "applied": true,
                    "revision": doc.revision().get(),
                    "ops": ops.as_array().map(Vec::len).unwrap_or_default(),
                }),
                mutated: true,
            })
        }
        NativeJsonTool::Validate => {
            let schema = schema.ok_or(NativeToolError::NoSchema)?;
            // The workspace's one validator. Composing it here rather than
            // reimplementing anything is the whole reason `u2s-jsondoc` has
            // no schema knowledge of its own.
            let violations = u2s_schema::validate(schema, doc.value()).map_err(|err| {
                NativeToolError::PatchRejected {
                    detail: format!("the output schema is unusable: {err}"),
                }
            })?;
            Ok(NativeOutcome {
                value: json!({
                    "valid": violations.is_empty(),
                    "violation_count": violations.len(),
                    "violations": serde_json::to_value(&violations)
                        .expect("violations are plain data"),
                }),
                mutated: false,
            })
        }
        NativeJsonTool::ListRules => {
            // Titles and descriptions only -- never `script_js`. The
            // retrigger loop already quotes rules to the model the same
            // way (`RuleBrief`), and this is the same discipline applied
            // to a tool result instead of a prompt section.
            let listed: Vec<Value> = rules
                .iter()
                .map(|rule| {
                    json!({
                        "id": rule.id.to_string(),
                        "title": rule.title,
                        "description": rule.description_md,
                    })
                })
                .collect();
            Ok(NativeOutcome {
                value: json!({ "rules": listed }),
                mutated: false,
            })
        }
        // Not dispatched here, and [`NativeJsonTool::route`] says so:
        // `rule_check` and `rule_autofix` hand a script to a worker process
        // rather than running it on this thread, and `rule_propose` needs a
        // model and the store. All three are async; this function is not.
        // Saying so rather than pretending otherwise keeps the exhaustive
        // match honest and makes a mis-wiring loud instead of silent.
        NativeJsonTool::CheckRules => Err(NativeToolError::PatchRejected {
            detail: "rule_check is dispatched through the sandbox runner, not here".to_owned(),
        }),
        NativeJsonTool::Autofix => Err(NativeToolError::PatchRejected {
            detail: "rule_autofix is dispatched through the sandbox runner, not here".to_owned(),
        }),
        NativeJsonTool::Propose => Err(NativeToolError::PatchRejected {
            detail: "rule_propose is dispatched through the rule proposer, not here".to_owned(),
        }),
        NativeJsonTool::TryRule => Err(NativeToolError::PatchRejected {
            detail: "rule_try is dispatched through the dry runner, not here".to_owned(),
        }),
        NativeJsonTool::ListFacts | NativeJsonTool::GetFact | NativeJsonTool::ProposeFact => {
            Err(NativeToolError::PatchRejected {
                detail: "the fact tools are dispatched through the fact workbench, not here"
                    .to_owned(),
            })
        }
    }
}

/// `rule_check`'s body, split out of [`dispatch`] because **it evaluates
/// LLM-generated JavaScript** and every other native tool does not.
///
/// Async, and dispatched to [`RuleRunner`]'s worker processes rather than
/// evaluated here: boa has no interrupt hook and no heap cap, so a script
/// that evades the loop and recursion limits either hangs the thread it
/// runs on or aborts the process outright. Neither is survivable in-process
/// -- an abort is not an unwind, so no `catch_unwind` and no
/// `spawn_blocking` contains it. The runner gives each script its own
/// process, its own memory and CPU ceiling, and a deadline the host can
/// actually act on. [`NativeJsonTool::runs_untrusted_scripts`] is how a
/// caller knows this tool is the one that needs it.
///
/// Every selected rule is checked **in parallel**, which is safe for the
/// same reason: a runaway script now costs a process the runner reclaims,
/// not a thread nothing can.
pub async fn check_rules(
    args: &Value,
    output: &Value,
    schema: Option<&Value>,
    rules: &[RuleForCheck],
    runner: &RuleRunner,
) -> Result<NativeOutcome, NativeToolError> {
    let schema = schema.ok_or(NativeToolError::NoSchema)?;
    let requested: Option<Vec<&str>> = args
        .get("rule_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>());

    let budget = u2s_rules::ScriptBudget::default();
    let selected: Vec<&RuleForCheck> = rules
        .iter()
        .filter(|rule| match &requested {
            Some(ids) => ids.contains(&rule.id.to_string().as_str()),
            None => true,
        })
        .collect();

    // Keyed by position rather than by `RuleId` so the answer can be put
    // back in the order the rules were offered -- a stable order matters
    // because this text goes into a prompt.
    // An indeterminate rule has no job: its facts are missing, so running
    // the script would be a guess (see `u2s_facts::resolve_facts`).
    let jobs: Vec<(usize, CheckRequest)> = selected
        .iter()
        .enumerate()
        .filter_map(|(idx, rule)| {
            let facts = rule.facts.ready()?;
            Some((
                idx,
                CheckRequest::new(
                    rule.script_js.clone(),
                    // `rule_check` answers without editing, so a fix is
                    // never attempted here -- only `rule_autofix` and the
                    // save gate ever set `fix_js`.
                    None,
                    output.clone(),
                    schema.clone(),
                    facts.clone(),
                    &budget,
                ),
            ))
        })
        .collect();

    let mut outcomes = u2s_rules_host::runner::into_map(runner.run_batch(jobs).await);

    let verdicts: Vec<Value> = selected
        .iter()
        .enumerate()
        .map(|(idx, rule)| {
            if let u2s_facts::FactsForCheck::Indeterminate(reason) = &rule.facts {
                return json!({
                    "rule_id": rule.id.to_string(),
                    "title": rule.title,
                    "verdict": "indeterminate",
                    "violations": [],
                    // Names the facts that are missing, never their values.
                    "indeterminate_reason": reason,
                    "autofix_available": false,
                });
            }
            let outcome = outcomes.remove(&idx).unwrap_or_else(|| {
                // The runner is total, so this is unreachable; saying so as
                // a verdict rather than a panic keeps the tool answering.
                u2s_rules_host::protocol::CheckResponse::broken(
                    "the sandbox runner returned no verdict for this rule",
                )
                .into()
            });
            json!({
                "rule_id": rule.id.to_string(),
                "title": rule.title,
                "verdict": match outcome.verdict {
                    u2s_rules::CheckVerdict::Positive => "positive",
                    u2s_rules::CheckVerdict::Negative => "negative",
                    u2s_rules::CheckVerdict::Broken => "broken",
                },
                "violations": outcome.violations,
                // Carried through rather than dropped: a bare "broken" with
                // an empty violation list tells the agent a rule failed and
                // nothing it can act on. The deterministic pass records this
                // same reason; the model should see the same thing.
                "broken_reason": outcome.broken_reason,
                // Known without evaluating anything -- `rule_check` never
                // runs a fix script itself (see `autofix_rules`), it only
                // says whether one exists to try.
                "autofix_available": rule.fix_js.is_some(),
            })
        })
        .collect();

    Ok(NativeOutcome {
        value: json!({ "verdicts": verdicts }),
        // Read-only -- checking never edits the document, and never
        // substitutes for the deterministic pass that actually records
        // `rule_references` after the round ends.
        mutated: false,
    })
}

/// What one rule's autofix attempt produced, before anything is applied to
/// the live document.
///
/// `final_output` is the whole document after every accepted fix, not a
/// diff -- the caller (`bridge::execute`) applies it as one whole-document
/// replace through [`u2s_jsondoc::patch_apply`], so an autofix call is one
/// revision step regardless of how many rules it fixed, the same "one call
/// is one edit" convention `json_patch` itself already holds.
#[derive(Debug, Clone, PartialEq)]
pub struct AutofixResult {
    /// Per-rule report: `{ rule_id, title, outcome, ... }`. See
    /// [`autofix_rules`] for the `outcome` values.
    pub results: Value,
    pub final_output: Value,
    /// Whether `final_output` actually differs from what was passed in --
    /// `false` when every selected rule already passed, was already
    /// broken, or had no working fix. The caller uses this to decide
    /// whether there is anything to apply at all.
    pub changed: bool,
}

/// `rule_autofix`'s body, split out of [`dispatch`] for the same reason
/// [`check_rules`] is: it evaluates LLM-generated JavaScript, so it must
/// never run on a thread of ours (see [`NativeJsonTool::runs_untrusted_scripts`]).
///
/// Every selected rule is checked and, if it fails, fixed **sequentially**,
/// never in parallel the way [`check_rules`]'s batch is: rule 2's fix must
/// be computed and verified against the document *after* rule 1's fix has
/// already been applied to it, or its ops could target a pointer rule 1's
/// fix just moved or removed. This is the same reason `json_patch` itself
/// carries a revision check, applied within one tool call instead of
/// across two.
///
/// A rule with no `fix_js` is skipped before it ever reaches the sandbox --
/// there is nothing to try, so evaluating its check here would only spend
/// budget to report the same "no fix" a reader can already see on
/// [`RuleForCheck::fix_js`].
pub async fn autofix_rules(
    args: &Value,
    output: &Value,
    schema: Option<&Value>,
    rules: &[RuleForCheck],
    runner: &RuleRunner,
) -> Result<AutofixResult, NativeToolError> {
    let schema = schema.ok_or(NativeToolError::NoSchema)?;
    let requested: Option<Vec<&str>> = args
        .get("rule_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>());

    let budget = u2s_rules::ScriptBudget::default();
    let selected: Vec<&RuleForCheck> = rules
        .iter()
        .filter(|rule| rule.fix_js.is_some())
        .filter(|rule| match &requested {
            Some(ids) => ids.contains(&rule.id.to_string().as_str()),
            None => true,
        })
        .collect();

    let mut current = output.clone();
    let mut changed = false;
    let mut results = Vec::with_capacity(selected.len());

    for rule in selected {
        let facts = match &rule.facts {
            u2s_facts::FactsForCheck::Ready(facts) => facts,
            u2s_facts::FactsForCheck::Indeterminate(reason) => {
                results.push(json!({
                    "rule_id": rule.id.to_string(),
                    "title": rule.title,
                    "outcome": "indeterminate",
                    "reason": reason,
                }));
                continue;
            }
        };
        let outcome = runner
            .run_batch(vec![(
                (),
                CheckRequest::new(
                    rule.script_js.clone(),
                    rule.fix_js.clone(),
                    current.clone(),
                    schema.clone(),
                    facts.clone(),
                    &budget,
                ),
            )])
            .await
            .into_iter()
            .next()
            .map(|(_, outcome)| outcome)
            .unwrap_or_else(|| {
                // The runner is total; unreachable in practice, but saying
                // so as a per-rule result rather than a panic keeps every
                // other selected rule's own attempt from being lost with it.
                u2s_rules_host::protocol::CheckResponse::broken(
                    "the sandbox runner returned no verdict for this rule",
                )
                .into()
            });

        let entry = match outcome.verdict {
            u2s_rules::CheckVerdict::Positive => json!({
                "rule_id": rule.id.to_string(),
                "title": rule.title,
                "outcome": "not_needed",
            }),
            u2s_rules::CheckVerdict::Broken => json!({
                "rule_id": rule.id.to_string(),
                "title": rule.title,
                "outcome": "check_broken",
                "reason": outcome.broken_reason,
            }),
            u2s_rules::CheckVerdict::Negative => match outcome.fix {
                Some(u2s_rules::FixOutcome::Broken(reason)) => json!({
                    "rule_id": rule.id.to_string(),
                    "title": rule.title,
                    "outcome": "fix_broken",
                    "reason": reason,
                }),
                Some(u2s_rules::FixOutcome::Ops(ops)) => {
                    match apply_and_validate(&current, &ops, schema) {
                        Ok(fixed) => {
                            current = fixed;
                            changed = true;
                            json!({
                                "rule_id": rule.id.to_string(),
                                "title": rule.title,
                                "outcome": "fixed",
                                "ops": ops,
                            })
                        }
                        Err(detail) => json!({
                            "rule_id": rule.id.to_string(),
                            "title": rule.title,
                            "outcome": "rejected",
                            "reason": detail,
                        }),
                    }
                }
                // `classify_check_and_fix` always produces a `fix` when the
                // check is `Negative` and `fix_js` was given (which it was,
                // by construction above) -- unreachable, but a plain result
                // keeps this arm from being a panic.
                None => json!({
                    "rule_id": rule.id.to_string(),
                    "title": rule.title,
                    "outcome": "fix_broken",
                    "reason": "no fix outcome was returned for a failing check",
                }),
            },
        };
        results.push(entry);
    }

    Ok(AutofixResult {
        results: Value::Array(results),
        final_output: current,
        changed,
    })
}

/// Applies `ops` to a clone of `current` and refuses the result if it
/// introduces a schema violation the document did not already have -- a
/// fix must not trade a rule violation for a schema violation, since the
/// round-level check (`convert.rs`, over `u2s_schema::validate`) would then
/// fail the *whole run* over a repair nobody reviewed, rather than leaving
/// the violation for the agent to see and act on.
fn apply_and_validate(current: &Value, ops: &Value, schema: &Value) -> Result<Value, String> {
    let mut fixed = current.clone();
    let parsed_ops: Vec<u2s_jsondoc::json_patch::PatchOperation> =
        serde_json::from_value(ops.clone())
            .map_err(|err| format!("the fix's own operations no longer parse: {err}"))?;
    u2s_jsondoc::json_patch::patch(&mut fixed, &parsed_ops)
        .map_err(|err| format!("the fix's operations were rejected: {err}"))?;

    let before = u2s_schema::validate(schema, current)
        .map_err(|err| format!("the output schema is unusable: {err}"))?;
    let after = u2s_schema::validate(schema, &fixed)
        .map_err(|err| format!("the output schema is unusable: {err}"))?;
    let introduced: Vec<&u2s_schema::Violation> =
        after.iter().filter(|v| !before.contains(v)).collect();
    if !introduced.is_empty() {
        let detail: Vec<String> = introduced
            .iter()
            .map(|v| format!("{}: {}", v.pointer, v.message))
            .collect();
        return Err(format!(
            "the fix would introduce new schema violation(s): {}",
            detail.join("; ")
        ));
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_document() -> Document {
        Document::new(json!({
            "title": "Account opening",
            "sections": [
                { "name": "Personal", "fields": [{ "id": "first_name" }, { "id": "last_name" }] },
                { "name": "Address", "fields": [{ "id": "street" }] }
            ]
        }))
    }

    #[test]
    fn every_tool_has_a_distinct_name_and_a_non_empty_description() {
        let mut names: Vec<&str> = NativeJsonTool::ALL.iter().map(|t| t.name()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        for tool in NativeJsonTool::ALL {
            assert!(
                tool.description().len() > 40,
                "{} needs a real description: it is prompt surface",
                tool.name()
            );
            assert_eq!(tool.input_schema()["type"], "object");
        }
    }

    #[test]
    fn a_conversion_is_never_offered_a_fact_tool() {
        for tool in NativeJsonTool::OFFERED_TO_RUNS {
            assert_ne!(tool.route(), Route::FactWorkbench, "{}", tool.name());
        }
        // Everything else is offered: only the fact tools are held back.
        for tool in NativeJsonTool::ALL {
            assert_eq!(
                NativeJsonTool::OFFERED_TO_RUNS.contains(tool),
                tool.route() != Route::FactWorkbench,
                "{}",
                tool.name()
            );
        }
    }

    #[test]
    fn only_the_mutating_tools_are_non_idempotent() {
        for tool in NativeJsonTool::ALL {
            let mutates = matches!(
                tool,
                NativeJsonTool::Patch
                    | NativeJsonTool::Autofix
                    | NativeJsonTool::Propose
                    | NativeJsonTool::ProposeFact
            );
            assert_eq!(tool.is_idempotent(), !mutates, "{}", tool.name());
        }
    }

    /// The three tools `dispatch` cannot answer are exactly the three that
    /// route away from it. Asserted as an equality rather than checked in
    /// two places, because a tool routed to the sandbox or the proposer
    /// while `dispatch` still claims to handle it would be answered by the
    /// synchronous path that cannot actually do the work.
    #[test]
    fn only_the_tools_dispatch_cannot_answer_route_away_from_it() {
        for tool in NativeJsonTool::ALL {
            let routed_away = tool.route() != Route::InProcess;
            let mut doc = a_document();
            let refused = dispatch(*tool, &json!({}), &mut doc, Some(&json!({})), &[])
                .is_err_and(|err| err.to_string().contains("not here"));
            assert_eq!(
                routed_away,
                refused,
                "{} routes to {:?} but dispatch says otherwise",
                tool.name(),
                tool.route()
            );
        }
    }

    #[test]
    fn rule_propose_is_answered_by_the_proposer_and_never_in_process() {
        assert_eq!(NativeJsonTool::Propose.route(), Route::Proposer);
        assert_eq!(NativeJsonTool::CheckRules.route(), Route::Sandbox);
        assert_eq!(NativeJsonTool::Autofix.route(), Route::Sandbox);
        assert_eq!(NativeJsonTool::Outline.route(), Route::InProcess);
    }

    /// The description is the only thing telling the model that proposing a
    /// rule does not change the run it is in. If that ever drops out, an
    /// agent may well start proposing rules expecting them to take effect.
    #[test]
    fn rule_propose_says_it_neither_activates_nor_affects_this_run() {
        let description = NativeJsonTool::Propose.description();
        assert!(description.contains("approves"), "{description}");
        assert!(description.contains("this run"), "{description}");
        assert!(description.contains("does not activate"), "{description}");
    }

    #[test]
    fn outline_describes_structure_without_returning_the_document() {
        let mut doc = a_document();
        let out = dispatch(
            NativeJsonTool::Outline,
            &json!({ "pointer": "", "depth": 1 }),
            &mut doc,
            None,
            &[],
        )
        .expect("outline");
        assert!(!out.mutated);
        let rendered = serde_json::to_string(&out.value).expect("serialize");
        assert!(
            !rendered.contains("first_name"),
            "a depth-1 outline must not reach the leaves: {rendered}"
        );
    }

    /// PLAN.md: search returns "pointers and counts, never bulk values". So
    /// a search for a field name must not hand back the field's contents.
    #[test]
    fn search_returns_pointers_and_never_the_matched_values() {
        let mut doc = Document::new(json!({
            "notes": "a very long secret note that must not be echoed back wholesale",
            "label": "note"
        }));
        let out = dispatch(
            NativeJsonTool::Search,
            &json!({ "pointer": "", "query": "note" }),
            &mut doc,
            None,
            &[],
        )
        .expect("search");
        let rendered = serde_json::to_string(&out.value).expect("serialize");
        assert!(
            !rendered.contains("must not be echoed back wholesale"),
            "search leaked a value: {rendered}"
        );
        assert!(
            rendered.contains("pointer"),
            "it must say where: {rendered}"
        );
    }

    #[test]
    fn a_patch_applies_and_bumps_the_revision() {
        let mut doc = a_document();
        let before = doc.revision().get();
        let out = dispatch(
            NativeJsonTool::Patch,
            &json!({
                "ops": [{ "op": "replace", "path": "/title", "value": "Account opening (revised)" }],
                "expected_revision": before
            }),
            &mut doc,
            None,
        &[],
        )
        .expect("patch");
        assert!(out.mutated);
        assert_eq!(out.value["applied"], true);
        assert!(doc.revision().get() > before);
        assert_eq!(doc.value()["title"], "Account opening (revised)");
    }

    /// A stale revision must be refused, and the refusal must tell the model
    /// what to do -- it is content, not a run failure.
    #[test]
    fn a_stale_revision_is_refused_with_the_current_one_named() {
        let mut doc = a_document();
        let stale = doc.revision().get();
        dispatch(
            NativeJsonTool::Patch,
            &json!({
                "ops": [{ "op": "replace", "path": "/title", "value": "one" }],
                "expected_revision": stale
            }),
            &mut doc,
            None,
            &[],
        )
        .expect("first patch");

        let err = dispatch(
            NativeJsonTool::Patch,
            &json!({
                "ops": [{ "op": "replace", "path": "/title", "value": "two" }],
                "expected_revision": stale
            }),
            &mut doc,
            None,
            &[],
        )
        .expect_err("a second patch on the old revision must be refused");

        match err {
            NativeToolError::StaleRevision { expected, actual } => {
                assert_eq!(expected, stale);
                assert!(actual > expected);
            }
            other => panic!("expected StaleRevision, got {other:?}"),
        }
        assert!(
            err.to_string().contains("re-read"),
            "the message must say how to recover: {err}"
        );
        assert_eq!(
            doc.value()["title"],
            "one",
            "a refused patch must change nothing"
        );
    }

    /// A `test` op is how a conditional write is made safe, and a failing one
    /// must leave the document untouched -- RFC 6902 atomicity, which
    /// `json-patch` provides and this asserts we actually get.
    #[test]
    fn a_failing_test_op_prevents_the_whole_patch() {
        let mut doc = a_document();
        let rev = doc.revision().get();
        let err = dispatch(
            NativeJsonTool::Patch,
            &json!({
                "ops": [
                    { "op": "test", "path": "/title", "value": "something else entirely" },
                    { "op": "replace", "path": "/title", "value": "must not land" }
                ],
                "expected_revision": rev
            }),
            &mut doc,
            None,
            &[],
        )
        .expect_err("the test op fails, so the patch must be rejected");
        assert!(
            matches!(err, NativeToolError::PatchRejected { .. }),
            "{err:?}"
        );
        assert_eq!(doc.value()["title"], "Account opening");
    }

    #[test]
    fn a_bad_pointer_names_itself_rather_than_failing_opaquely() {
        let mut doc = a_document();
        let err = dispatch(
            NativeJsonTool::Get,
            &json!({ "pointer": "/nope/deeper" }),
            &mut doc,
            None,
            &[],
        )
        .expect_err("must fail");
        assert_eq!(
            err,
            NativeToolError::BadPointer {
                pointer: "/nope/deeper".to_owned()
            }
        );
    }

    #[test]
    fn a_missing_argument_names_the_field() {
        let mut doc = a_document();
        assert_eq!(
            dispatch(NativeJsonTool::Get, &json!({}), &mut doc, None, &[]).expect_err("must fail"),
            NativeToolError::BadArgument { field: "pointer" }
        );
        assert_eq!(
            dispatch(
                NativeJsonTool::Patch,
                &json!({ "ops": [] }),
                &mut doc,
                None,
                &[]
            )
            .expect_err("must fail"),
            NativeToolError::BadArgument {
                field: "expected_revision"
            }
        );
        assert_eq!(
            dispatch(
                NativeJsonTool::Search,
                &json!({ "pointer": "" }),
                &mut doc,
                None,
                &[],
            )
            .expect_err("must fail"),
            NativeToolError::BadArgument { field: "query" }
        );
    }

    /// `json_validate` composes `u2s-jsondoc` with `u2s_schema::validate`, so
    /// the workspace keeps one validator. This asserts the composition, not
    /// the validator.
    #[test]
    fn validate_reports_violations_as_pointers_through_the_one_validator() {
        let schema = json!({
            "type": "object",
            "properties": { "title": { "type": "string" }, "count": { "type": "integer" } },
            "required": ["title", "count"],
            "additionalProperties": false
        });

        let mut good = Document::new(json!({ "title": "ok", "count": 1 }));
        let out = dispatch(
            NativeJsonTool::Validate,
            &json!({}),
            &mut good,
            Some(&schema),
            &[],
        )
        .expect("validate");
        assert_eq!(out.value["valid"], true);
        assert_eq!(out.value["violation_count"], 0);
        assert!(!out.mutated);

        let mut bad = Document::new(json!({ "title": "ok" }));
        let out = dispatch(
            NativeJsonTool::Validate,
            &json!({}),
            &mut bad,
            Some(&schema),
            &[],
        )
        .expect("validate");
        assert_eq!(out.value["valid"], false);
        assert!(out.value["violation_count"].as_u64().expect("a count") >= 1);
    }

    #[test]
    fn validate_without_a_schema_says_so_rather_than_passing_vacuously() {
        let mut doc = a_document();
        assert_eq!(
            dispatch(NativeJsonTool::Validate, &json!({}), &mut doc, None, &[])
                .expect_err("must not silently succeed"),
            NativeToolError::NoSchema
        );
    }

    fn a_rule(title: &str, script_js: &str) -> RuleForCheck {
        RuleForCheck {
            facts: u2s_facts::FactsForCheck::Ready(serde_json::Map::new()),
            id: u2s_core::RuleId::generate(),
            title: title.to_owned(),
            description_md: format!("{title} description"),
            script_js: script_js.to_owned(),
            fix_js: None,
        }
    }

    fn a_rule_with_fix(title: &str, script_js: &str, fix_js: &str) -> RuleForCheck {
        RuleForCheck {
            facts: u2s_facts::FactsForCheck::Ready(serde_json::Map::new()),
            fix_js: Some(fix_js.to_owned()),
            ..a_rule(title, script_js)
        }
    }

    /// PLAN.md: the model must see a rule's title and description, never
    /// its script -- the same discipline the retrigger loop's own
    /// `RuleBrief` already holds, applied here to a tool result instead of
    /// a prompt section.
    #[test]
    fn rule_list_never_includes_the_script() {
        let secret_marker = "SCRIPT_BODY_MUST_NOT_LEAK";
        let rules = vec![a_rule(
            "Amount must be positive",
            &format!(
                "function check(output, ctx) {{ /* {secret_marker} */ return {{ pass: true, violations: [] }}; }}"
            ),
        )];
        let mut doc = a_document();
        let out = dispatch(
            NativeJsonTool::ListRules,
            &json!({}),
            &mut doc,
            None,
            &rules,
        )
        .expect("list");
        let rendered = serde_json::to_string(&out.value).expect("serialize");
        assert!(
            !rendered.contains(secret_marker),
            "rule_list leaked a script: {rendered}"
        );
        assert!(rendered.contains("Amount must be positive"));
        assert!(!out.mutated);
    }

    fn test_runner() -> u2s_rules_host::runner::RuleRunner {
        u2s_rules_host::runner::test_support::runner()
    }

    #[tokio::test]
    async fn rule_check_reports_each_rules_verdict_against_the_current_document() {
        let rules = vec![
            a_rule(
                "always positive",
                "function check(output, ctx) { return { pass: true, violations: [] }; }",
            ),
            a_rule(
                "reject empty title",
                r#"function check(output, ctx) {
                    if (!output.title) {
                        return { pass: false, violations: [{ pointer: "", message: "title required" }] };
                    }
                    return { pass: true, violations: [] };
                }"#,
            ),
        ];
        let doc = a_document(); // has a "title" field, so both rules pass
        let out = check_rules(
            &json!({}),
            doc.value(),
            Some(&json!({})),
            &rules,
            &test_runner(),
        )
        .await
        .expect("check");
        let verdicts = out.value["verdicts"].as_array().expect("an array");
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts.iter().all(|v| v["verdict"] == "positive"));
        assert!(!out.mutated, "checking must never edit the document");
    }

    /// The verdicts must come back in the order the rules were offered,
    /// even though they are evaluated concurrently -- this text goes into a
    /// prompt, and a prompt that reorders between runs is a prompt nobody
    /// can diff.
    #[tokio::test]
    async fn rule_check_returns_verdicts_in_the_order_the_rules_were_given() {
        let rules = vec![
            a_rule(
                "first",
                "function check(output, ctx) { return { pass: true, violations: [] }; }",
            ),
            a_rule(
                "second",
                "function check(output, ctx) { throw new Error('boom'); }",
            ),
            a_rule(
                "third",
                "function check(output, ctx) { return { pass: true, violations: [] }; }",
            ),
        ];
        let doc = a_document();
        let out = check_rules(
            &json!({}),
            doc.value(),
            Some(&json!({})),
            &rules,
            &test_runner(),
        )
        .await
        .expect("check");
        let verdicts = out.value["verdicts"].as_array().expect("an array");
        let titles: Vec<&str> = verdicts
            .iter()
            .map(|v| v["title"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(titles, vec!["first", "second", "third"]);
        assert_eq!(verdicts[1]["verdict"], "broken");
    }

    #[tokio::test]
    async fn rule_check_honours_a_requested_subset_of_rule_ids() {
        let broken = a_rule(
            "throws",
            "function check(output, ctx) { throw new Error('boom'); }",
        );
        let fine = a_rule(
            "always positive",
            "function check(output, ctx) { return { pass: true, violations: [] }; }",
        );
        let fine_id = fine.id.to_string();
        let rules = vec![broken, fine];
        let doc = a_document();
        let out = check_rules(
            &json!({ "rule_ids": [fine_id] }),
            doc.value(),
            Some(&json!({})),
            &rules,
            &test_runner(),
        )
        .await
        .expect("check");
        let verdicts = out.value["verdicts"].as_array().expect("an array");
        assert_eq!(
            verdicts.len(),
            1,
            "only the requested rule must be checked: {verdicts:?}"
        );
        assert_eq!(verdicts[0]["verdict"], "positive");
    }

    #[tokio::test]
    async fn rule_check_reports_an_indeterminate_rule_without_running_it_or_leaking_facts() {
        let doc = a_document();
        let throwing = "function check() { throw new Error('must not run'); }";
        let rule = RuleForCheck {
            facts: u2s_facts::FactsForCheck::Indeterminate(
                "fact `source_fields` not extracted for this input".to_owned(),
            ),
            ..a_rule("extrinsic", throwing)
        };
        let outcome = check_rules(
            &json!({}),
            doc.value(),
            Some(&json!({})),
            &[rule],
            &test_runner(),
        )
        .await
        .unwrap();
        let verdict = &outcome.value["verdicts"][0];
        assert_eq!(verdict["verdict"], "indeterminate");
        assert!(
            verdict["indeterminate_reason"]
                .as_str()
                .unwrap()
                .contains("source_fields")
        );
    }

    #[tokio::test]
    async fn rule_check_hands_ready_facts_to_the_script() {
        let doc = a_document();
        let script = r#"
            const requires = ["must_fail"];
            function check(output, ctx) {
                return ctx.facts.must_fail
                    ? { pass: false, violations: [{ pointer: "", message: "failed by fact" }] }
                    : { pass: true, violations: [] };
            }
        "#;
        let mut facts = serde_json::Map::new();
        facts.insert("must_fail".to_owned(), json!(true));
        let rule = RuleForCheck {
            facts: u2s_facts::FactsForCheck::Ready(facts),
            ..a_rule("extrinsic", script)
        };
        let outcome = check_rules(
            &json!({}),
            doc.value(),
            Some(&json!({})),
            &[rule],
            &test_runner(),
        )
        .await
        .unwrap();
        let verdict = &outcome.value["verdicts"][0];
        assert_eq!(verdict["verdict"], "negative", "{verdict}");
        // The value itself never reaches the tool result.
        assert!(!outcome.value.to_string().contains("\"must_fail\":true"));
    }

    #[tokio::test]
    async fn rule_check_without_a_schema_says_so_rather_than_passing_vacuously() {
        let doc = a_document();
        assert_eq!(
            check_rules(&json!({}), doc.value(), None, &[], &test_runner())
                .await
                .expect_err("must not silently succeed"),
            NativeToolError::NoSchema
        );
    }

    /// `dispatch` is synchronous, so it can neither run a script nor reach
    /// the store -- and rather than pretend otherwise it refuses, naming
    /// where the call should have gone.
    #[test]
    fn dispatch_refuses_the_async_tools_and_says_where_they_belong() {
        let mut doc = a_document();
        let err = dispatch(
            NativeJsonTool::CheckRules,
            &json!({}),
            &mut doc,
            Some(&json!({})),
            &[],
        )
        .expect_err("dispatch cannot run scripts");
        assert!(format!("{err}").contains("sandbox runner"), "{err}");

        let err = dispatch(
            NativeJsonTool::Propose,
            &json!({ "title": "t", "description_md": "d" }),
            &mut doc,
            Some(&json!({})),
            &[],
        )
        .expect_err("dispatch cannot reach the store");
        assert!(format!("{err}").contains("rule proposer"), "{err}");
    }

    /// The check must fail before a fix is ever attempted; here it does not,
    /// so the rule is reported "not needed" and nothing changes.
    const NO_TITLE_ALLOWED_CHECK: &str = r#"
        function check(output, ctx) {
            if (output.title) {
                return { pass: false, violations: [{ pointer: "/title", message: "no title allowed" }] };
            }
            return { pass: true, violations: [] };
        }
    "#;
    const REMOVE_VIOLATION_POINTERS_FIX: &str = r#"
        function fix(output, ctx) {
            return ctx.violations.map(v => ({ op: "remove", path: v.pointer }));
        }
    "#;

    #[tokio::test]
    async fn autofix_applies_a_working_fix_and_reports_it_fixed() {
        let rule = a_rule_with_fix(
            "no title",
            NO_TITLE_ALLOWED_CHECK,
            REMOVE_VIOLATION_POINTERS_FIX,
        );
        let output = json!({ "title": "nope" });
        let result = autofix_rules(
            &json!({}),
            &output,
            Some(&json!({})),
            &[rule],
            &test_runner(),
        )
        .await
        .expect("autofix runs");

        assert!(result.changed);
        assert_eq!(result.final_output, json!({}));
        assert_eq!(result.results[0]["outcome"], "fixed");
    }

    #[tokio::test]
    async fn autofix_reports_not_needed_when_the_check_already_passes() {
        let rule = a_rule_with_fix(
            "no title",
            NO_TITLE_ALLOWED_CHECK,
            REMOVE_VIOLATION_POINTERS_FIX,
        );
        let output = json!({}); // no title, so the check already passes
        let result = autofix_rules(
            &json!({}),
            &output,
            Some(&json!({})),
            &[rule],
            &test_runner(),
        )
        .await
        .expect("autofix runs");

        assert!(!result.changed);
        assert_eq!(result.final_output, output);
        assert_eq!(result.results[0]["outcome"], "not_needed");
    }

    #[tokio::test]
    async fn autofix_reports_fix_broken_when_the_fix_does_not_resolve_it() {
        let rule = a_rule_with_fix(
            "no title",
            NO_TITLE_ALLOWED_CHECK,
            "function fix(output, ctx) { return []; }",
        );
        let output = json!({ "title": "nope" });
        let result = autofix_rules(
            &json!({}),
            &output,
            Some(&json!({})),
            &[rule],
            &test_runner(),
        )
        .await
        .expect("autofix runs");

        assert!(!result.changed, "a broken fix must not change the document");
        assert_eq!(result.final_output, output);
        assert_eq!(result.results[0]["outcome"], "fix_broken");
    }

    /// A rule with no `fix_js` at all is skipped before it ever reaches the
    /// sandbox -- it must not appear in the results, fixed or otherwise.
    #[tokio::test]
    async fn autofix_skips_rules_with_no_fix_script_entirely() {
        let fixable = a_rule_with_fix(
            "no title",
            NO_TITLE_ALLOWED_CHECK,
            REMOVE_VIOLATION_POINTERS_FIX,
        );
        let unfixable = a_rule(
            "unrelated",
            "function check(output, ctx) { return { pass: true, violations: [] }; }",
        );
        let output = json!({ "title": "nope" });
        let result = autofix_rules(
            &json!({}),
            &output,
            Some(&json!({})),
            &[unfixable, fixable],
            &test_runner(),
        )
        .await
        .expect("autofix runs");

        let results = result.results.as_array().expect("an array");
        assert_eq!(results.len(), 1, "the rule with no fix_js must not appear");
        assert_eq!(results[0]["outcome"], "fixed");
    }

    /// The property that makes sequential application correct: the second
    /// rule's fix is computed and verified against the document *after*
    /// the first rule's fix already ran, not against the original
    /// snapshot. If this ran the two in parallel against the same
    /// snapshot instead, the second rule would still see `/a` present and
    /// would try (and fail) to remove it a second time.
    #[tokio::test]
    async fn autofix_computes_each_fix_against_the_document_the_previous_fix_left() {
        let check_js = r#"
            function check(output, ctx) {
                if (output.a !== undefined) {
                    return { pass: false, violations: [{ pointer: "/a", message: "no a allowed" }] };
                }
                return { pass: true, violations: [] };
            }
        "#;
        let first = a_rule_with_fix("first", check_js, REMOVE_VIOLATION_POINTERS_FIX);
        let second = a_rule_with_fix("second", check_js, REMOVE_VIOLATION_POINTERS_FIX);
        let output = json!({ "a": 1 });

        let result = autofix_rules(
            &json!({}),
            &output,
            Some(&json!({})),
            &[first, second],
            &test_runner(),
        )
        .await
        .expect("autofix runs");

        assert_eq!(result.final_output, json!({}));
        assert_eq!(result.results[0]["outcome"], "fixed");
        assert_eq!(
            result.results[1]["outcome"], "not_needed",
            "the second rule must see the document the first rule already fixed"
        );
    }

    #[tokio::test]
    async fn autofix_refuses_a_fix_that_would_introduce_a_schema_violation() {
        // `title` is required by the schema. The fix resolves the check's
        // own violation (removing `/extra`, which is all `verify_fix`
        // checks for) but also removes `/title` as a side effect -- that
        // second removal is what must be caught and refused, since the fix
        // *did* satisfy the check it was computed for.
        let schema = json!({
            "type": "object",
            "required": ["title"],
            "properties": { "title": { "type": "string" } }
        });
        let rule = a_rule_with_fix(
            "flag extra",
            r#"function check(output, ctx) {
                if (output.extra) {
                    return { pass: false, violations: [{ pointer: "/extra", message: "no extra" }] };
                }
                return { pass: true, violations: [] };
            }"#,
            r#"function fix(output, ctx) {
                return [
                    { op: "remove", path: "/extra" },
                    { op: "remove", path: "/title" }
                ];
            }"#,
        );
        let output = json!({ "title": "Account opening", "extra": true });
        let result = autofix_rules(&json!({}), &output, Some(&schema), &[rule], &test_runner())
            .await
            .expect("autofix runs");

        assert!(
            !result.changed,
            "a schema-violating fix must not be applied"
        );
        assert_eq!(result.final_output, output);
        assert_eq!(result.results[0]["outcome"], "rejected");
    }

    #[tokio::test]
    async fn autofix_without_a_schema_says_so_rather_than_passing_vacuously() {
        assert_eq!(
            autofix_rules(
                &json!({}),
                &a_document().into_value(),
                None,
                &[],
                &test_runner()
            )
            .await
            .expect_err("must not silently succeed"),
            NativeToolError::NoSchema
        );
    }
}
