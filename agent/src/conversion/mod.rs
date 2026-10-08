//! The conversion agent's engine surface: the tool catalog and the executor.
//!
//! A run authors one JSON document in its target's UBS format
//! ([`u2s_aem_ubs_mcp::UbsAemDocument`] or
//! [`u2s_redacto_ubs_mcp::UbsRedactoDocument`]). The agent reads the source
//! form through the u2s `xfa_*` tools, edits the document with the `json_*`
//! tools, holds it to the format's check rules with the `rule_*` tools, and
//! builds the package or dump through the format's encoder, which the
//! verifiers then check. Every edit is snapshotted into the edit history
//! ([`crate::db`]) under the run's `#document` session.
//!
//! This type holds no LLM and no UI state: an external loop streams turns,
//! calls [`ConversionAgent::execute`], and surfaces the results.

use std::collections::HashMap;

use serde_json::{Value, json};
use u2s_doc_tools::native::{NativeJsonTool, RuleForCheck};
use u2s_jsondoc::Document;
use u2s_rules_host::runner::RuleRunner;

use crate::rule_board::{RuleBoard, RuleView};
use crate::source::SourceContext;

/// Error returned by the package tools before a package is built. Public so the
/// MCP server's `write_package` reports the same thing.
pub const NO_PACKAGE: &str = "No package built yet; call build_aem_package.";

/// The document tools a run offers, in catalog order.
pub(crate) const DOCUMENT_TOOLS: &[NativeJsonTool] = &[
    NativeJsonTool::Outline,
    NativeJsonTool::Get,
    NativeJsonTool::Search,
    NativeJsonTool::Patch,
    NativeJsonTool::Validate,
    NativeJsonTool::ListRules,
    NativeJsonTool::CheckRules,
    NativeJsonTool::Autofix,
];

/// The language UBS masters a form in when the form ships it.
const UBS_MASTER_LANGUAGE: &str = "en";

/// The result of executing one tool call, to be returned to the model as a
/// `tool_result` content block.
#[derive(Debug)]
pub enum ToolReply {
    /// A textual result (JSON, plain text, …).
    Text(String),
    /// Text and images interleaved, in order: what a u2s tool returns, e.g. a
    /// page render next to its metadata. Emitted as one block per entry in a
    /// single `tool_result`.
    Blocks(Vec<ReplyBlock>),
    /// The tool failed; the message is surfaced to the model as an error result.
    Error(String),
}

/// One content block of a [`ToolReply::Blocks`] reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyBlock {
    Text(String),
    /// A base64 image; the media type is whatever the producer said (the
    /// browser sends PNG or JPEG), so it is owned rather than a static str.
    Image {
        media_type: String,
        data: String,
    },
}

/// The outcome of the Reviewer role's `submit_review` call: whether the form is
/// approved, and (if not) a detailed report the controller pins into the Author's
/// next system prompt.
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub approved: bool,
    pub report: String,
}

/// The form's own content XML in an unzipped package: the `cq:Page` under
/// `jcr_root/content/forms/af/`, as `(path, xml)`.
pub(crate) fn form_content_xml(files: &[(String, String)]) -> Option<&(String, String)> {
    files.iter().find(|(p, c)| {
        p.starts_with("jcr_root/content/forms/af/")
            && p.ends_with("/.content.xml")
            && c.contains("\"cq:Page\"")
    })
}

/// Validate FileVault package bytes (session-agnostic).
///
/// The checks `build_aem_package` runs on every build: required FileVault
/// structure, form `.content.xml` (`cq:Page`) validation, and DAM
/// `.content.xml` (`dam:Asset`) validation. Returns `Ok(success message)` when
/// the package is valid, or `Err(problem report)` listing every violation.
pub fn validate_package_bytes(pkg: &[u8]) -> Result<String, String> {
    let files = references_mcp::unzip_package(pkg)
        .map_err(|e| format!("Could not read package: {e}"))?;

    let mut problems: Vec<String> = Vec::new();

    // 1. Required FileVault package structure.
    const REQUIRED: &[&str] = &[
        "META-INF/MANIFEST.MF",
        "META-INF/vault/config.xml",
        "META-INF/vault/nodetypes.cnd",
        "META-INF/vault/filter.xml",
        "META-INF/vault/properties.xml",
        "META-INF/vault/definition/.content.xml",
        "jcr_root/.content.xml",
        "jcr_root/content/.content.xml",
        "jcr_root/content/forms/.content.xml",
        "jcr_root/content/forms/af/.content.xml",
        "jcr_root/content/dam/.content.xml",
        "jcr_root/content/dam/formsanddocuments/.content.xml",
    ];
    for path in REQUIRED {
        if !files.iter().any(|(p, _)| p == path) {
            problems.push(format!("missing required package entry: {path}"));
        }
    }

    // 2. Validate the form content XML (the cq:Page under forms/af).
    match form_content_xml(&files) {
        Some((path, xml)) => {
            if let Err(violations) = u2s_aem_ubs_mcp::aem::validate_aem_form_xml(xml) {
                problems.push(format!(
                    "form {path} failed {} validation check(s):\n    - {}",
                    violations.len(),
                    violations.join("\n    - ")
                ));
            }
        }
        None => problems.push(
            "no form .content.xml (jcr:primaryType cq:Page) found under \
             jcr_root/content/forms/af/"
                .into(),
        ),
    }

    // 3. Validate the DAM content XML (the dam:Asset).
    let dam_xml = files.iter().find(|(p, c)| {
        p.starts_with("jcr_root/content/dam/formsanddocuments/")
            && p.ends_with("/.content.xml")
            && c.contains("\"dam:Asset\"")
    });
    match dam_xml {
        Some((path, xml)) => {
            if let Err(violations) = u2s_aem_ubs_mcp::aem::validate_aem_dam_xml(xml) {
                problems.push(format!(
                    "DAM {path} failed {} validation check(s):\n    - {}",
                    violations.len(),
                    violations.join("\n    - ")
                ));
            }
        }
        None => problems.push(
            "no DAM .content.xml (jcr:primaryType dam:Asset) found under \
             jcr_root/content/dam/formsanddocuments/"
                .into(),
        ),
    }

    if problems.is_empty() {
        Ok(format!(
            "Package valid: {} entries; required FileVault structure present; \
             form and DAM content XML pass AEM validation.",
            files.len()
        ))
    } else {
        Err(format!(
            "Package validation found {} problem(s):\n- {}",
            problems.len(),
            problems.join("\n- ")
        ))
    }
}

// ── The agent ────────────────────────────────────────────────────────────────

/// What the latest successful build produced.
enum Built {
    Aem {
        package: Vec<u8>,
        /// The same form built bound to its schema: every field carries a
        /// `bindRef` and the package ships the XSD. Kept beside the plain one
        /// because they are for different deployments; the plain one is what
        /// UBS installs today.
        bound_package: Option<Vec<u8>>,
        xsd: Option<String>,
    },
    Redacto {
        dump: Vec<u8>,
    },
}

/// A board with every rule of the run unchecked.
fn new_rule_board(scripted: &[RuleForCheck], judged: &[crate::rules::JudgedRule]) -> RuleBoard {
    RuleBoard::new(scripted.iter().map(|r| (r.id.to_string(), r.title.clone())), judged)
}

/// One source PDF: its file name and what it says about itself (`None` for a
/// PDF without XFA).
type SourceDocument = (String, Option<SourceContext>);

pub struct ConversionAgent {
    target: OutputTarget,
    current_pdfs: Vec<(String, Vec<u8>)>,
    /// Each read source's documents, by source key (see [`Self::source_key`]).
    sources: HashMap<String, Vec<SourceDocument>>,

    /// The run's output document, and the schema and rules it is held to.
    document: Document,
    schema: Value,
    /// The rules a script decides, run on every edit and by `rule_check`.
    rules: Vec<RuleForCheck>,
    /// The rules a judge agent decides (see [`crate::rules`]).
    judged: Vec<crate::rules::JudgedRule>,
    /// The rule sandbox, started on the first rule call.
    runner: Option<RuleRunner>,
    /// Each rule's verdict at the last check, so an edit reports what it
    /// changed rather than every standing finding. `None` until the first edit
    /// of this run takes it from the document as it stood.
    lint: Option<HashMap<String, String>>,
    /// Where every rule stands on the document, for a person watching the run
    /// (see [`crate::rule_board`]). Fed by the checks above, never by a check
    /// of its own.
    rule_board: RuleBoard,

    built: Option<Built>,
    session: String,

    /// The reference tools, scoped to this run's profile. Its sentence-embedding
    /// model loads lazily on the first semantic search.
    references: references_mcp::ReferencesServer,

    /// The Reviewer role's latest `submit_review` outcome, drained by the
    /// controller via [`take_review`](Self::take_review).
    review: Option<ReviewResult>,
    /// The Author role's `finish_authoring` summary, drained by the controller
    /// via [`take_finish`](Self::take_finish).
    finish: Option<String>,
    /// What the current stage has verified, which gates its terminal call
    /// (see [`evidence`]).
    evidence: evidence::StageEvidence,
    /// Whether the evidence gate is off, for a controller test that drives
    /// stages with scripted turns rather than real verification.
    #[cfg(any(test, feature = "test-utils"))]
    evidence_waived: bool,
    /// The judgements `rule_check` has dispatched and not yet taken back, each
    /// with the verdict its judge recorded, if it has. Keyed by a judgement id
    /// of its own, not the rule's: two judges of one rule never collide, and a
    /// verdict for a judgement nobody opened is refused.
    judgements: HashMap<String, Option<crate::rules::RuleVerdict>>,
    /// How many judgements this agent has opened, which numbers the next.
    judgements_opened: u64,

    /// The vendored u2s tool servers, created on the first u2s call or
    /// `get_source_info` (see [`Self::u2s_tools`]).
    u2s: Option<crate::u2s::U2sTools>,
}

impl ConversionAgent {
    /// `files` may mix source PDFs and one UBS AEM package ZIP. The PDFs are
    /// the conversion source. For an AEM run, the package is decoded into the
    /// starting document, a template the agent edits instead of authoring
    /// from scratch; the source's variables and languages replace the
    /// template's own. Otherwise the document starts from what the sources
    /// say about themselves: their variables and languages, and nothing else.
    ///
    /// `session` is the edit-history session the run records into. Starting
    /// records nothing: a revision is recorded by each edit, so a resumed run
    /// keeps its seeded document as the latest until it changes it, and a run
    /// that dies before its first edit leaves nothing behind.
    pub fn new(
        profile: Option<String>,
        files: Vec<(String, Vec<u8>)>,
        session: String,
        target: OutputTarget,
    ) -> Result<Self, String> {
        let pdfs: Vec<(String, Vec<u8>)> = files
            .iter()
            .filter(|(name, _)| is_source_pdf(name))
            .cloned()
            .collect();
        let template = template_of(&files);
        let documents = read_sources(&pdfs)?;
        let contexts: Vec<&SourceContext> = documents.iter().filter_map(|(_, c)| c.as_ref()).collect();
        let document = match target {
            OutputTarget::Aem => starting_aem_document(&contexts, template)?,
            OutputTarget::Redacto => starting_redacto_document(&contexts),
        };
        let schema = match target {
            OutputTarget::Aem => u2s_aem_ubs_mcp::document_schema(),
            OutputTarget::Redacto => u2s_redacto_ubs_mcp::document_schema(),
        };
        let rules = crate::rules::rules_for(target)?;
        let references = references_mcp::ReferencesServer::new(
            crate::references::store(),
            profile.clone().unwrap_or_default(),
        );
        let rule_board = new_rule_board(&rules.scripted, &rules.judged);
        Ok(Self {
            target,
            current_pdfs: pdfs,
            sources: HashMap::from([("current".to_string(), documents)]),
            document: Document::new(document),
            schema,
            rules: rules.scripted,
            judged: rules.judged,
            runner: None,
            lint: None,
            rule_board,
            built: None,
            session,
            references,
            review: None,
            finish: None,
            evidence: evidence::StageEvidence::default(),
            #[cfg(any(test, feature = "test-utils"))]
            evidence_waived: false,
            judgements: HashMap::new(),
            judgements_opened: 0,
            u2s: None,
        })
    }

    /// The output target this run aims at.
    pub fn target(&self) -> OutputTarget {
        self.target
    }

    /// Replace the document with one recorded earlier (a resumed session).
    /// Refused unless it is a document of this run's format.
    pub fn seed_document(&mut self, value: Value) -> Result<(), String> {
        check_document(self.target, &value)?;
        self.document = Document::new(value);
        self.set_built(None);
        self.lint = None;
        self.rule_board = new_rule_board(&self.rules, &self.judged);
        Ok(())
    }

    /// Hand the run a package to inspect as if it had built it: the package
    /// tools then read it (describing a reference form does this).
    pub fn seed_package(&mut self, package: Vec<u8>) {
        self.set_built(Some(Built::Aem {
            package,
            bound_package: None,
            xsd: None,
        }));
    }

    /// Replaces the latest build. Every change of it goes through here: what
    /// the verifier made of the previous build no longer counts as evidence.
    fn set_built(&mut self, built: Option<Built>) {
        self.built = built;
        self.evidence.build_changed();
    }

    /// A pipeline stage begins: it gathers its own evidence for its terminal
    /// call, so an earlier stage's verification never stands in for it.
    pub fn begin_stage(&mut self) {
        self.evidence = evidence::StageEvidence::default();
    }

    /// What the current stage still has to verify before its terminal call
    /// is accepted (empty when nothing).
    pub fn missing_evidence(&self) -> Vec<String> {
        #[cfg(any(test, feature = "test-utils"))]
        if self.evidence_waived {
            return Vec::new();
        }
        self.evidence.missing(self.target, self.built.is_some())
    }

    /// Turns the evidence gate off, for a controller test whose scripted
    /// stages end with a terminal call they did nothing to earn.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn waive_evidence(&mut self) {
        self.evidence_waived = true;
    }

    /// The document as it stands.
    pub fn document(&self) -> &Value {
        self.document.value()
    }

    /// Start the UBS AEM verifier for this run: from then on the
    /// `aem_verify_*` tools check the built package against a Docker AEM.
    /// Run [`crate::u2s::aem_verify_readiness`] first; this only validates the
    /// settings.
    pub fn with_aem_verify(mut self, settings: &crate::u2s::AemVerifySettings) -> Result<Self, String> {
        self.u2s_tools()?.attach_aem_verify(settings)?;
        Ok(self)
    }

    /// Start the UBS Redacto verifier for this run: from then on the
    /// `redacto_verify_*` tools import the built dump into a Redacto platform
    /// of the run's own and render it there. Run
    /// [`crate::u2s::redacto_verify_readiness`] first.
    pub fn with_redacto_verify(
        mut self,
        settings: &crate::u2s::RedactoVerifySettings,
    ) -> Result<Self, String> {
        self.u2s_tools()?.attach_redacto_verify(settings)?;
        Ok(self)
    }

    /// Whether a verifier is attached (and not yet torn down).
    pub fn has_verifier(&self) -> bool {
        self.u2s.as_ref().is_some_and(|t| t.has_verifier())
    }

    /// The tools a stage is offered.
    pub fn tools_for_stage(&self, scopes: scope::Mask) -> Vec<Value> {
        tools_for(self.target(), scopes)
    }

    /// Tear down the containers the verifier started, if any. Called by the
    /// controller on every way out of a run, so no AEM or Postgres container
    /// outlives it.
    pub async fn shutdown_verifiers(&mut self) -> Result<(), String> {
        match self.u2s.as_mut() {
            Some(tools) => tools.shutdown().await,
            None => Ok(()),
        }
    }

    /// What a verifier tool checks: the latest package or dump this run built.
    fn verify_artifact(&self) -> Option<crate::u2s::Artifact> {
        match self.built.as_ref()? {
            Built::Aem { package, .. } => Some(crate::u2s::Artifact {
                file_name: "package.zip",
                bytes: package.clone(),
            }),
            Built::Redacto { dump } => Some(crate::u2s::Artifact {
                file_name: "dump.sql",
                bytes: dump.clone(),
            }),
        }
    }

    // ── Public accessors (for the driving loop's result finalization) ─────────

    /// Drain the Reviewer role's latest `submit_review` outcome (the controller
    /// reads this after running the Reviewer stage).
    pub fn take_review(&mut self) -> Option<ReviewResult> {
        self.review.take()
    }

    /// Drain the Author role's `finish_authoring` summary (the controller
    /// reads this after running an Author stage; `None` when it ended without).
    pub fn take_finish(&mut self) -> Option<String> {
        self.finish.take()
    }

    /// Replaces the judged rules, for a test of what dispatches judges.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_judged_rules(&mut self, rules: Vec<crate::rules::JudgedRule>) {
        self.judged = rules;
        self.rule_board = new_rule_board(&self.rules, &self.judged);
    }

    /// Opens a judgement for one judge to record its verdict under, and
    /// returns its id.
    pub fn open_judgement(&mut self) -> String {
        self.judgements_opened += 1;
        let id = format!("judgement-{}", self.judgements_opened);
        self.judgements.insert(id.clone(), None);
        id
    }

    /// Closes the judgement `id` and returns the verdict recorded under it, if
    /// its judge recorded one.
    pub fn take_judgement(&mut self, id: &str) -> Option<crate::rules::RuleVerdict> {
        self.judgements.remove(id).flatten()
    }

    /// The document's revision, which a check reports against.
    pub fn revision(&self) -> u64 {
        self.document.revision().get()
    }

    /// Where every rule stands on the document as it is now.
    pub fn rule_board(&self) -> Vec<RuleView> {
        self.rule_board.snapshot(self.revision())
    }

    /// Records the judges' outcomes of one `rule_check`, dispatched on
    /// `revision`.
    pub fn record_judged(
        &mut self,
        outcomes: &[(crate::rules::JudgedRule, Result<crate::rules::RuleVerdict, String>)],
        revision: u64,
    ) {
        self.rule_board.record_judged(outcomes, revision);
    }

    /// The latest built AEM package.
    pub fn package(&self) -> Option<Vec<u8>> {
        match self.built.as_ref()? {
            Built::Aem { package, .. } => Some(package.clone()),
            Built::Redacto { .. } => None,
        }
    }

    /// The latest built AEM package bound to its schema.
    pub fn package_bound(&self) -> Option<Vec<u8>> {
        match self.built.as_ref()? {
            Built::Aem { bound_package, .. } => bound_package.clone(),
            Built::Redacto { .. } => None,
        }
    }

    /// The schema of the latest built AEM package.
    pub fn xsd(&self) -> Option<String> {
        match self.built.as_ref()? {
            Built::Aem { xsd, .. } => xsd.clone(),
            Built::Redacto { .. } => None,
        }
    }

    /// The latest built Redacto dump.
    pub fn redacto_dump(&self) -> Option<Vec<u8>> {
        match self.built.as_ref()? {
            Built::Redacto { dump } => Some(dump.clone()),
            Built::Aem { .. } => None,
        }
    }

    /// The form code the document is named by (`formrange_code`), for file names.
    pub fn form_code(&self) -> Option<String> {
        let variables = match self.target {
            OutputTarget::Aem => &self.document.value()["variables"],
            OutputTarget::Redacto => self.document.value()["sources"]
                .as_object()
                .and_then(|sources| sources.values().next())
                .map(|source| &source["variables"])?,
        };
        variables["formrange_code"].as_str().map(String::from)
    }

    /// The edit-history session this run records into.
    pub fn session_id(&self) -> &str {
        &self.session
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Record the document as it stands into the edit history.
    fn snapshot(&self, label: &str) {
        let json = self.document.value().to_string();
        crate::db::insert_edit(&crate::session::document_session(&self.session), label, &json);
    }

    fn source_key(input: &Value) -> String {
        match input["source"]["reference"].as_str() {
            Some(id) => format!("reference:{id}"),
            None => "current".to_string(),
        }
    }

    /// The PDFs of the requested source: the uploaded form, or a reference's
    /// input.
    fn source_pdfs(&self, input: &Value) -> Result<Vec<(String, Vec<u8>)>, String> {
        match input["source"]["reference"].as_str() {
            Some(id) => {
                let bytes = crate::references::store().get_reference_pdf_bytes(id, 0)?;
                Ok(vec![(format!("{id}.pdf"), bytes)])
            }
            None => Ok(self.current_pdfs.clone()),
        }
    }

    /// What the requested source's PDFs say about themselves, read once.
    fn source_contexts(&mut self, input: &Value) -> Result<&[SourceDocument], String> {
        let key = Self::source_key(input);
        if !self.sources.contains_key(&key) {
            let documents = read_sources(&self.source_pdfs(input)?)?;
            self.sources.insert(key.clone(), documents);
        }
        Ok(&self.sources[&key])
    }

    /// The u2s tool servers, created on first use.
    fn u2s_tools(&mut self) -> Result<&mut crate::u2s::U2sTools, String> {
        let tools = match self.u2s.take() {
            Some(tools) => tools,
            None => crate::u2s::U2sTools::new()?,
        };
        Ok(self.u2s.insert(tools))
    }

    /// The requested source's PDFs, written where the u2s tools may read them,
    /// with what each says about itself: (file name, context, `doc_path`).
    fn source_documents(
        &mut self,
        input: &Value,
    ) -> Result<Vec<(String, Option<SourceContext>, std::path::PathBuf)>, String> {
        let pdfs = self.source_pdfs(input)?;
        let contexts: Vec<Option<SourceContext>> =
            self.source_contexts(input)?.iter().map(|(_, c)| c.clone()).collect();
        let group = Self::source_key(input).replace(':', "-");
        let tools = self.u2s_tools()?;
        pdfs.iter()
            .zip(contexts)
            .map(|((name, bytes), context)| {
                let path = tools.add_document(&group, name, bytes)?;
                Ok((name.clone(), context, path))
            })
            .collect()
    }

    /// The rule sandbox, started on first use.
    fn runner(&mut self) -> Result<&RuleRunner, String> {
        if self.runner.is_none() {
            self.runner = Some(crate::rules::runner()?);
        }
        Ok(self.runner.as_ref().expect("just started"))
    }

    /// Why `name` cannot run under this run's output target, if it cannot.
    /// Derived from the catalog, so a tool is scoped in exactly one place.
    fn target_refusal(&self, name: &str) -> Option<String> {
        let scoped_out = catalog()
            .iter()
            .find(|t| t.name() == name)
            .is_some_and(|t| t.targets & target_mask(self.target) == 0);
        scoped_out.then(|| {
            format!(
                "{name} is not available for the {} output target.",
                self.target.label()
            )
        })
    }
}

/// Whether an uploaded file is a source PDF, by its name. The one rule both
/// the agent and its hosts decide it by.
pub fn is_source_pdf(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".pdf")
}

/// The uploaded AEM package a run starts from, if `files` has one: the first
/// `.zip`. The one rule both the agent and its hosts decide it by.
pub fn template_of(files: &[(String, Vec<u8>)]) -> Option<&[u8]> {
    files
        .iter()
        .find(|(name, _)| name.to_ascii_lowercase().ends_with(".zip"))
        .map(|(_, bytes)| bytes.as_slice())
}

/// What each PDF says about itself; a PDF that cannot be read at all is an
/// error, one without XFA is `None`.
fn read_sources(pdfs: &[(String, Vec<u8>)]) -> Result<Vec<SourceDocument>, String> {
    pdfs.iter()
        .map(|(name, bytes)| {
            crate::source::read(bytes)
                .map(|context| (name.clone(), context))
                .map_err(|e| format!("{name}: {e}"))
        })
        .collect()
}

/// The sources' languages, first appearance first.
fn source_languages(contexts: &[&SourceContext]) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for context in contexts {
        if !languages.contains(&context.language) {
            languages.push(context.language.clone());
        }
    }
    languages
}

/// The source UBS masters the form in: the English one when there is one,
/// otherwise the first. Its variables name the form.
fn master_source<'a>(contexts: &[&'a SourceContext]) -> Option<&'a SourceContext> {
    contexts
        .iter()
        .find(|c| c.language == UBS_MASTER_LANGUAGE)
        .or(contexts.first())
        .copied()
}

fn starting_aem_document(contexts: &[&SourceContext], template: Option<&[u8]>) -> Result<Value, String> {
    let mut document = match template {
        Some(package) => {
            let decoded = u2s_aem_ubs_mcp::decode(package).map_err(|e| format!("the template: {e}"))?;
            serde_json::to_value(decoded).expect("a document serializes")
        }
        None => json!({
            "variables": {},
            "languages": [],
            "form": { "type": "Root", "title": {}, "children": [] }
        }),
    };
    if let Some(master) = master_source(contexts) {
        document["variables"] = json!(master.variables);
        let mut languages = source_languages(contexts);
        // A template's texts may carry languages the sources do not: keep them
        // listed, so the document stays valid until the agent decides.
        for language in document["languages"].as_array().cloned().unwrap_or_default() {
            if let Some(language) = language.as_str()
                && !languages.iter().any(|l| l == language)
            {
                languages.push(language.to_string());
            }
        }
        document["languages"] = json!(languages);
    }
    Ok(document)
}

fn starting_redacto_document(contexts: &[&SourceContext]) -> Value {
    let mut sources = serde_json::Map::new();
    for context in contexts {
        sources
            .entry(context.language.clone())
            .or_insert_with(|| json!({ "variables": context.variables }));
    }
    json!({ "sources": sources, "assets": [], "body": [] })
}

/// Whether `value` is a document of `target`'s format: the one check every
/// document entering a run passes.
pub fn check_document(target: OutputTarget, value: &Value) -> Result<(), String> {
    match target {
        OutputTarget::Aem => u2s_aem_ubs_mcp::UbsAemDocument::from_json(value)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        OutputTarget::Redacto => serde_json::from_value::<u2s_redacto_ubs_mcp::UbsRedactoDocument>(value.clone())
            .map(|_| ())
            .map_err(|e| format!("not a UBS Redacto document: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_board::{RuleKind, RuleState};

    /// Where the source forms the tests convert live.
    const SOURCE_FORMS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../forms");

    fn fixture(name: &str) -> (String, Vec<u8>) {
        let path = std::path::Path::new(SOURCE_FORMS).join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        (name.to_string(), bytes)
    }

    fn agent_for(target: OutputTarget, files: Vec<(String, Vec<u8>)>) -> ConversionAgent {
        crate::db::claim_scratch_db_for_test();
        let session = format!("test-{}", uuid::Uuid::new_v4());
        ConversionAgent::new(Some("ubs".into()), files, session, target).expect("the agent starts")
    }

    /// The tools the catalog scopes to exactly one output target.
    fn tools_only_for(mask: target::Mask) -> Vec<&'static str> {
        catalog()
            .iter()
            .filter(|t| t.targets == mask)
            .map(|t| t.name())
            .collect()
    }

    fn reply_text(reply: ToolReply) -> String {
        match reply {
            ToolReply::Text(t) => t,
            ToolReply::Error(e) => panic!("tool failed: {e}"),
            ToolReply::Blocks(b) => panic!("expected text, got {} blocks", b.len()),
        }
    }

    fn reply_kind(reply: &ToolReply) -> String {
        match reply {
            ToolReply::Text(t) => format!("text: {t}"),
            ToolReply::Error(e) => format!("error: {e}"),
            ToolReply::Blocks(b) => format!("{} blocks", b.len()),
        }
    }

    async fn patch(agent: &mut ConversionAgent, ops: Value) -> Value {
        let revision = agent.document.revision().get();
        let reply = reply_text(
            agent
                .execute("json_patch", &json!({ "ops": ops, "expected_revision": revision }))
                .await,
        );
        serde_json::from_str(&reply).unwrap()
    }

    /// One page holding one bilingual text field.
    fn page(name: &str) -> Value {
        json!({
            "type": "Panel", "uuid": "6a9f2f5e-8c8e-4a8e-9b0e-1f2d3c4b5a61", "name": name,
            "title": {"en": "Details"}, "children": [{
                "type": "TextField", "uuid": "6a9f2f5e-8c8e-4a8e-9b0e-1f2d3c4b5a62",
                "name": "TXT_LastName", "label": {"en": "Last name"}, "mandatory": false,
                "visible": true, "max_chars": null, "colspan": 12, "dor_colspan": null,
                "bind_ref": null, "kind": "Plain"
            }],
            "is_page": true, "visible": true, "is_conditional": false, "dor_num_cols": null,
            "colspan": 12, "dor_colspan": null, "bind_ref": null, "frag_ref": null
        })
    }

    /// An AEM run starts from what the source says about itself: its variables
    /// name the form, and its languages are the form's.
    #[test]
    fn an_aem_document_starts_from_the_sources_variables_and_languages() {
        let agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let doc = agent.document();
        assert_eq!(doc["variables"]["formrange_code"], "AAEV");
        assert_eq!(doc["languages"], json!(["en"]));
        assert_eq!(doc["form"]["type"], "Root");
        assert_eq!(agent.form_code().as_deref(), Some("AAEV"));
    }

    /// A bilingual AEM run is named by its English source, whichever order the
    /// sources come in.
    #[test]
    fn a_bilingual_aem_document_takes_the_english_variables() {
        let agent = agent_for(
            OutputTarget::Aem,
            vec![fixture("AABF_019_DE.pdf"), fixture("AABF_019_EN.pdf")],
        );
        let doc = agent.document();
        assert_eq!(doc["variables"]["formrange_language"], "EN", "{}", doc["variables"]);
        assert_eq!(doc["languages"], json!(["de", "en"]));
    }

    /// A Redacto run starts with one source entry per language.
    #[test]
    fn a_redacto_document_starts_with_one_source_per_language() {
        let agent = agent_for(
            OutputTarget::Redacto,
            vec![fixture("AABF_019_DE.pdf"), fixture("AABF_019_EN.pdf")],
        );
        let sources = agent.document()["sources"].as_object().unwrap();
        assert_eq!(sources.keys().collect::<Vec<_>>(), vec!["de", "en"]);
        assert_eq!(sources["de"]["variables"]["formrange_code"], "AABF");
    }

    /// An uploaded package is decoded into the starting document, with the
    /// source's variables in place of the template's own.
    #[test]
    fn a_template_package_becomes_the_starting_form() {
        let package = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../u2s/crates/u2s-aem-ubs-mcp/tests/fixtures/golden/AABF_019/package.zip"
        ))
        .unwrap();
        let agent = agent_for(
            OutputTarget::Aem,
            vec![fixture("AAEV_019_EN.pdf"), ("template.zip".into(), package)],
        );
        let doc = agent.document();
        assert!(!doc["form"]["children"].as_array().unwrap().is_empty());
        assert_eq!(doc["variables"]["formrange_code"], "AAEV");
        // The template's texts are in languages the source does not have, and
        // stay listed until the agent decides.
        let languages = doc["languages"].as_array().unwrap();
        assert!(languages.contains(&json!("en")) && languages.contains(&json!("de")));
    }

    /// Exactly the catalog's reads take the read path, and a read that
    /// addresses a live form session does not, since it has to see the turn's
    /// `xfa_set` calls in order.
    #[tokio::test]
    async fn only_the_catalogs_reads_run_beside_other_calls() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        for tool in catalog() {
            let started = agent.start_read(tool.name(), &json!({})).await.is_some();
            assert_eq!(started, tool.access == Access::Read, "{}", tool.name());
        }
        assert!(
            agent
                .start_read("xfa_render_pages", &json!({"session": "s", "revision": 0}))
                .await
                .is_none()
        );
    }

    /// A read run on the read path answers exactly what the same call answers
    /// through `execute`: it is the same server work, done without the agent.
    #[tokio::test]
    async fn a_read_answers_the_same_on_either_path() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let info: Value = serde_json::from_str(&reply_text(agent.execute("get_source_info", &json!({})).await)).unwrap();
        let input = json!({"doc_path": info["documents"][0]["doc_path"]});

        let work = agent.start_read("xfa_packets", &input).await.expect("a read");
        let beside = reply_text(work.await);
        let through = reply_text(agent.execute("xfa_packets", &input).await);
        assert_eq!(beside, through);
        assert!(beside.contains("template"), "{beside}");
    }

    /// rule_list holds every rule and says which kind each is; rule_check
    /// runs the scripts and, with no judge agent here, reports a judged rule
    /// unchecked; an id no rule has is refused.
    #[tokio::test]
    async fn rule_tools_cover_scripted_and_judged_rules() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let judged = crate::rules::JudgedRule {
            id: "judged-id".into(),
            name: "ubs-aem-test".into(),
            title: "A judged rule".into(),
            description: "Judge me.".into(),
        };
        agent.set_judged_rules(vec![judged.clone()]);

        let listed: Value = serde_json::from_str(&reply_text(agent.execute("rule_list", &json!({})).await)).unwrap();
        let rules = listed["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["check"] == "script"));
        assert!(rules.iter().any(|r| r["id"] == "judged-id" && r["check"] == "agent"));

        let checked: Value = serde_json::from_str(&reply_text(agent.execute("rule_check", &json!({})).await)).unwrap();
        let verdicts = checked["verdicts"].as_array().unwrap();
        assert!(verdicts.iter().any(|v| v["check"] == "script"));
        let unjudged = verdicts.iter().find(|v| v["rule_id"] == "judged-id").expect("the judged rule is reported");
        assert_eq!(unjudged["verdict"], "unchecked");

        let only: Value =
            serde_json::from_str(&reply_text(agent.execute("rule_check", &json!({"rule_ids": ["judged-id"]})).await))
                .unwrap();
        assert_eq!(only["verdicts"].as_array().unwrap().len(), 1);

        let refused = agent.execute("rule_check", &json!({"rule_ids": ["nope"]})).await;
        assert!(matches!(refused, ToolReply::Error(e) if e.contains("nope")));
    }

    /// coverage_check compares the run's own source PDFs with its document: a
    /// text patched in stops being missing, and an unknown language is refused.
    #[tokio::test]
    async fn coverage_check_follows_the_document() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let missing = |reply: ToolReply| -> Vec<String> {
            let report: Value = serde_json::from_str(&reply_text(reply)).unwrap();
            report["sources"][0]["missing"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect()
        };
        let text = "The QI must withhold tax at the highest rate applicable to any partner, beneficiary, or owner.";
        let before = missing(agent.execute("coverage_check", &json!({})).await);
        assert!(before.iter().any(|t| t == text), "{before:?}");

        let mut page = page("PN_Details");
        page["children"] = json!([{
            "type": "TextDraw", "uuid": "00000000-0000-4000-8000-000000000001", "name": "ST_Withhold",
            "content": {"en": format!("<p>{text}</p>")}, "visible": true, "colspan": 12, "dor_colspan": null
        }]);
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page }])).await;
        let after = missing(agent.execute("coverage_check", &json!({"language": "en"})).await);
        assert!(!after.iter().any(|t| t == text), "{after:?}");

        let refused = agent.execute("coverage_check", &json!({"language": "xx"})).await;
        assert!(matches!(refused, ToolReply::Error(e) if e.contains("xx")));
    }

    /// The whole AEM path: patch a page in, build the package, and the build is
    /// what the verifier checks.
    #[tokio::test]
    async fn a_patched_document_builds_a_valid_package() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;

        let built = reply_text(agent.execute("build_aem_package", &json!({})).await);
        assert!(built.contains("Built package") && built.contains("Package valid"), "{built}");
        // The writer's package is checked against the guard on every build, and
        // rule_check reports the same for the current build.
        assert!(!built.contains("ENGINE DEFECTS"), "{built}");
        let checked: Value = serde_json::from_str(&reply_text(agent.execute("rule_check", &json!({})).await)).unwrap();
        assert_eq!(checked["package_findings"], json!([]), "{checked}");
        let files = references_mcp::unzip_package(&agent.package().unwrap()).unwrap();
        assert!(files.iter().any(|(_, c)| c.contains("TXT_LastName")));
        assert!(agent.xsd().is_some() && agent.package_bound().is_some());
    }

    /// Feeds the agent's evidence everything the AEM gate asks for, as the
    /// verifier and renderer replies would.
    fn verify_everything(agent: &mut ConversionAgent) {
        let text = |v: Value| ToolReply::Text(v.to_string());
        let e = &mut agent.evidence;
        e.observe_reply("xfa_controls", &json!({}), &text(json!({ "controls": [] })));
        e.observe_call("xfa_render_pages", &json!({ "doc_path": "source.pdf" }));
        e.observe_reply("aem_verify_open", &json!({}), &text(json!({})));
        e.observe_reply(
            "aem_verify_submit",
            &json!({}),
            &text(json!({ "artefacts": [{ "blob": { "media_type": "application/pdf", "doc_path": "/blobs/dor.pdf" } }] })),
        );
        e.observe_call("pdf_render_pages", &json!({ "doc_path": "/blobs/dor.pdf" }));
    }

    /// The terminal calls are gated on the stage's own verification of the
    /// current build: refused before it, accepted after it, refused again once
    /// an edit or a new stage voids it. A rejecting review is never gated.
    #[tokio::test]
    async fn terminal_calls_wait_for_the_stages_own_verification() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;

        let approve = json!({ "approved": true, "report": "" });
        let refused = agent.execute("submit_review", &approve).await;
        assert!(matches!(&refused, ToolReply::Error(e) if e.contains("aem_verify_submit") && e.contains("build_aem_package")), "{refused:?}");
        assert!(agent.take_review().is_none(), "a refused approval records nothing");
        assert!(matches!(agent.execute("finish_authoring", &json!({ "summary": "done" })).await, ToolReply::Error(_)));

        let rejected = agent.execute("submit_review", &json!({ "approved": false, "report": "footer" })).await;
        assert!(matches!(rejected, ToolReply::Text(_)));
        assert!(agent.take_review().is_some_and(|r| !r.approved && r.report == "footer"));

        reply_text(agent.execute("build_aem_package", &json!({})).await);
        verify_everything(&mut agent);
        reply_text(agent.execute("submit_review", &approve).await);
        assert!(agent.take_review().is_some_and(|r| r.approved));
        reply_text(agent.execute("finish_authoring", &json!({ "summary": "done" })).await);
        assert_eq!(agent.take_finish().as_deref(), Some("done"));

        // A new stage earns its own.
        agent.begin_stage();
        assert!(matches!(agent.execute("submit_review", &approve).await, ToolReply::Error(_)));

        // An edit voids the verification of the build it replaced.
        verify_everything(&mut agent);
        assert!(agent.missing_evidence().is_empty());
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_More") }])).await;
        let missing = agent.missing_evidence().join("\n");
        assert!(missing.contains("build_aem_package") && missing.contains("aem_verify_submit"), "{missing}");
    }

    /// A judge's calls run on the same agent while the stage waits for its
    /// verdict, but they are not the stage's own verification.
    #[tokio::test]
    async fn an_unrecorded_call_is_no_evidence() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let render = json!({ "doc_path": "/nowhere/source.pdf", "page": 1 });
        let names_render = |agent: &ConversionAgent| agent.missing_evidence().join("\n").contains("xfa_render_pages");

        agent.execute_as("xfa_render_pages", &render, false).await;
        assert!(agent.start_read_as("xfa_render_pages", &render, false).await.is_some());
        assert!(names_render(&agent), "a judge's render counted for the stage");
        agent.execute("xfa_render_pages", &render).await;
        assert!(!names_render(&agent), "the stage's own render did not count");
    }

    /// The authored header is what the banking-relationship preface prints in
    /// the DoR header slot, without its validity line.
    #[tokio::test]
    async fn the_header_reaches_the_packages_dor_slot() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let mut first = page("PN_Details");
        first["children"].as_array_mut().unwrap().insert(
            0,
            json!({ "type": "Preface", "uuid": "6a9f2f5e-8c8e-4a8e-9b0e-1f2d3c4b5a63", "name": "PN_BR" }),
        );
        patch(
            &mut agent,
            json!([
                { "op": "add", "path": "/form/children/-", "value": first },
                { "op": "add", "path": "/header", "value": "Valid from 02.01.2018\nUBS Europe SE (Succursale Italia)" }
            ]),
        )
        .await;
        reply_text(agent.execute("build_aem_package", &json!({})).await);
        let files = references_mcp::unzip_package(&agent.package().unwrap()).unwrap();
        assert!(
            files.iter().any(|(_, c)| c.contains("UBS Europe SE") && c.contains("(Succursale Italia)")),
            "the header is in no file of the package"
        );
        assert!(!files.iter().any(|(_, c)| c.contains("Valid from 02.01.2018")));
    }

    /// A package that fails its own validation is not a build: nothing is
    /// stored, so nothing invalid is verified or exported.
    #[tokio::test]
    async fn a_package_that_fails_validation_builds_nothing() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let mut broken = page("PN_Details");
        broken["passthrough"] = json!({ "raw_children": ["<unclosed>"] });
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": broken }])).await;
        match agent.execute("build_aem_package", &json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("Nothing built"), "{e}"),
            other => panic!("an invalid package was built: {}", reply_kind(&other)),
        }
        assert!(agent.package().is_none());
        let outputs = crate::outputs::build(&mut agent);
        assert!(outputs.package.is_none() && !outputs.warnings.is_empty());
    }

    /// A patch reports what it changed about the rules: here a page named
    /// without its prefix, and then the rename that fixes it.
    #[tokio::test]
    async fn a_patch_reports_the_findings_it_introduced_and_resolved() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let added = patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("Details") }])).await;
        let introduced = added["lint"]["introduced"].as_array().unwrap();
        assert!(
            introduced.iter().any(|v| v["title"].as_str().unwrap().contains("prefix")),
            "{added}"
        );

        let renamed = patch(&mut agent, json!([{ "op": "replace", "path": "/form/children/0/name", "value": "PN_Details" }])).await;
        let resolved = renamed["lint"]["resolved"].as_array().unwrap();
        assert!(resolved.iter().any(|v| v["title"].as_str().unwrap().contains("prefix")), "{renamed}");
    }

    /// The rule board follows the edits: every scripted rule gets a verdict on
    /// the edited revision, the prefix rule fails and then passes with its
    /// fix, and the judged rules stay unchecked, since no judge ran.
    #[tokio::test]
    async fn the_rule_board_follows_the_edits() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let board = agent.rule_board();
        assert!(board.iter().all(|r| r.state == RuleState::NotChecked), "{board:?}");
        assert!(board.iter().any(|r| r.kind == RuleKind::Judge));

        let prefix = |agent: &ConversionAgent| {
            agent.rule_board().into_iter().find(|r| r.title.contains("prefix")).expect("the prefix rule")
        };
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("Details") }])).await;
        let board = agent.rule_board();
        assert!(
            board.iter().filter(|r| r.kind == RuleKind::Script).all(|r| r.state != RuleState::NotChecked && !r.outdated),
            "{board:?}"
        );
        assert!(board.iter().filter(|r| r.kind == RuleKind::Judge).all(|r| r.state == RuleState::NotChecked));
        assert!(matches!(prefix(&agent).state, RuleState::Fail { .. }), "{:?}", prefix(&agent));

        patch(&mut agent, json!([{ "op": "replace", "path": "/form/children/0/name", "value": "PN_Details" }])).await;
        assert_eq!(prefix(&agent).state, RuleState::Pass);
    }

    /// A seeded document's scripted verdicts are taken once, before any edit,
    /// so a stage starting on it shows where it stands.
    #[tokio::test]
    async fn refreshing_the_rules_checks_a_seeded_document() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let mut seeded = agent.document().clone();
        seeded["form"]["children"] = json!([page("Details")]);
        agent.seed_document(seeded).unwrap();
        agent.refresh_rules().await.unwrap();
        let board = agent.rule_board();
        assert!(board.iter().filter(|r| r.kind == RuleKind::Script).all(|r| r.state != RuleState::NotChecked));
    }

    /// An edit reports what it changed, not what the document already had: a
    /// seeded document with a standing finding does not blame it on the first
    /// unrelated edit.
    #[tokio::test]
    async fn the_first_edit_of_a_seeded_document_reports_only_its_own_findings() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let mut seeded = agent.document().clone();
        seeded["form"]["children"] = json!([page("Details")]);
        agent.seed_document(seeded).unwrap();
        let reply = patch(&mut agent, json!([{ "op": "add", "path": "/header", "value": "UBS Europe SE" }])).await;
        assert_eq!(reply["lint"]["introduced"], json!([]), "{reply}");
        assert_eq!(reply["lint"]["resolved"], json!([]), "{reply}");
    }

    /// An edit makes the last build stale: nothing is verified against a
    /// package that no longer describes the document.
    #[tokio::test]
    async fn an_edit_drops_the_stale_build() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;
        reply_text(agent.execute("build_aem_package", &json!({})).await);
        assert!(agent.package().is_some());
        patch(&mut agent, json!([{ "op": "replace", "path": "/form/children/0/title/en", "value": "Personal details" }])).await;
        assert!(agent.package().is_none());
        // Finishing builds it again, so what ships is the final document.
        let outputs = crate::outputs::build(&mut agent);
        assert!(outputs.package.is_some() && outputs.warnings.is_empty(), "{:?}", outputs.warnings);
    }

    /// A patch against an outdated revision is refused rather than applied over
    /// an edit the agent has not seen.
    #[tokio::test]
    async fn a_patch_against_a_stale_revision_is_refused() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        patch(&mut agent, json!([{ "op": "add", "path": "/header", "value": "UBS Europe SE" }])).await;
        let stale = agent
            .execute("json_patch", &json!({ "ops": [{ "op": "remove", "path": "/header" }], "expected_revision": 0 }))
            .await;
        assert!(matches!(stale, ToolReply::Error(_)), "{}", reply_kind(&stale));
    }

    /// Every edit is recorded, so a resumed session picks up the latest document.
    #[tokio::test]
    async fn every_edit_is_recorded_for_a_resume() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;
        match crate::session::restore(agent.session_id(), OutputTarget::Aem).unwrap() {
            crate::session::Restored::Document(doc) => assert_eq!(&doc, agent.document()),
            other => panic!("{other:?}"),
        }
        let mut resumed = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        resumed.seed_document(agent.document().clone()).unwrap();
        assert_eq!(resumed.document(), agent.document());
        assert!(resumed.seed_document(json!({"sources": {}, "assets": [], "body": []})).is_err());
    }

    /// Resuming must not bury the document it resumes: starting an agent
    /// records nothing, so until the resumed run edits, the latest recorded
    /// revision is still the one it was seeded with. A run that died before its
    /// first edit leaves no revision at all.
    #[tokio::test]
    async fn starting_or_resuming_records_nothing_until_an_edit() {
        let mut first = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        let session = first.session_id().to_string();
        assert!(matches!(
            crate::session::restore(&session, OutputTarget::Aem).unwrap(),
            crate::session::Restored::Nothing
        ));
        patch(&mut first, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;
        let authored = first.document().clone();

        let mut resumed = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            session.clone(),
            OutputTarget::Aem,
        )
        .unwrap();
        resumed.seed_document(authored.clone()).unwrap();
        match crate::session::restore(&session, OutputTarget::Aem).unwrap() {
            crate::session::Restored::Document(doc) => assert_eq!(doc, authored),
            other => panic!("{other:?}"),
        }
    }

    /// The whole Redacto path: an asset and its place in the body, the page
    /// header, the dump, and the verifier's offline check of it.
    #[tokio::test]
    async fn a_patched_redacto_document_builds_a_dump_the_verifier_reads() {
        let mut agent = agent_for(OutputTarget::Redacto, vec![fixture("AAEV_019_EN.pdf")])
            .with_redacto_verify(&crate::u2s::RedactoVerifySettings::default())
            .expect("the Redacto verifier attaches without Docker");
        patch(
            &mut agent,
            json!([
                { "op": "add", "path": "/assets/-", "value": { "key": "intro", "kind": "text", "content": { "en": "<p>A paragraph the dump must carry.</p>" } } },
                { "op": "add", "path": "/body/-", "value": { "type": "assetContainer", "assets": ["intro"] } },
                { "op": "add", "path": "/sources/en/header", "value": "Valid from 02.01.2018\nUBS Europe SE" }
            ]),
        )
        .await;
        let built: Value = serde_json::from_str(&reply_text(agent.execute("build_redacto_dump", &json!({})).await)).unwrap();
        assert_eq!(built["document_id"], "aaev_019");
        assert_eq!(built["has_header"], true);
        assert_eq!(built["has_footer"], true);

        let checked = reply_text(agent.execute("redacto_verify_dump_check", &json!({})).await);
        assert!(checked.contains("\"ok\":true") || checked.contains("\"ok\": true"), "{checked}");
    }

    /// A Redacto document whose sources do not name the form builds nothing:
    /// the dump's identity comes from those variables.
    #[tokio::test]
    async fn a_redacto_document_without_its_identity_builds_nothing() {
        let mut agent = agent_for(OutputTarget::Redacto, vec![fixture("AAEV_019_EN.pdf")]);
        patch(
            &mut agent,
            json!([
                { "op": "replace", "path": "/sources/en/variables", "value": {} },
                { "op": "add", "path": "/assets/-", "value": { "key": "intro", "kind": "text", "content": { "en": "<p>Text.</p>" } } },
                { "op": "add", "path": "/body/-", "value": { "type": "assetContainer", "assets": ["intro"] } }
            ]),
        )
        .await;
        match agent.execute("build_redacto_dump", &json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("Nothing built"), "{e}"),
            other => panic!("a dump without its identity was built: {}", reply_kind(&other)),
        }
    }

    /// A document the Redacto model refuses builds nothing and says why.
    #[tokio::test]
    async fn an_empty_redacto_body_builds_nothing() {
        let mut agent = agent_for(OutputTarget::Redacto, vec![fixture("AAEV_019_EN.pdf")]);
        match agent.execute("build_redacto_dump", &json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("Nothing built") && e.contains("body"), "{e}"),
            other => panic!("{}", reply_kind(&other)),
        }
    }

    #[tokio::test]
    async fn get_source_info_names_each_documents_language_and_variables() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAOS_033_IT.pdf")]);
        let info: Value = serde_json::from_str(&reply_text(agent.execute("get_source_info", &json!({})).await)).unwrap();
        assert_eq!(info["languages"], json!(["it"]));
        let document = &info["documents"][0];
        assert_eq!(document["variables"]["formrange_code"], "AAOS");
        let doc_path = document["doc_path"].as_str().unwrap();
        let packets = reply_text(agent.execute("xfa_packets", &json!({ "doc_path": doc_path })).await);
        assert!(packets.contains("\"template\""), "{packets}");
    }

    #[test]
    fn each_targets_tools_are_refused_under_the_other() {
        let aem = agent_for(OutputTarget::Aem, Vec::new());
        let redacto = agent_for(OutputTarget::Redacto, Vec::new());
        for tool in tools_only_for(target::AEM) {
            let refusal = redacto.target_refusal(tool).unwrap_or_else(|| panic!("{tool}"));
            assert!(refusal.contains("not available for the Redacto"), "{refusal}");
        }
        for tool in tools_only_for(target::REDACTO) {
            let refusal = aem.target_refusal(tool).unwrap_or_else(|| panic!("{tool}"));
            assert!(refusal.contains("not available for the AEM"), "{refusal}");
        }
    }

    /// A verifier tool without a verifier attached says so rather than failing
    /// obscurely.
    #[tokio::test]
    async fn a_verifier_tool_needs_its_verifier() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")]);
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;
        reply_text(agent.execute("build_aem_package", &json!({})).await);
        match agent.execute("aem_verify_package_check", &json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("verifier was not started"), "{e}"),
            other => panic!("{}", reply_kind(&other)),
        }
    }

    /// The verifier checks the run's own latest build, whatever path the model
    /// passes, and says what to build when there is nothing yet.
    #[tokio::test]
    async fn aem_verify_package_check_checks_the_runs_own_build() {
        let settings = crate::u2s::AemVerifySettings {
            image: "blueprint-test/aem:unused".into(),
            ..Default::default()
        };
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAEV_019_EN.pdf")])
            .with_aem_verify(&settings)
            .expect("complete settings attach the verifier");
        match agent.execute("aem_verify_package_check", &json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("build_aem_package"), "{e}"),
            other => panic!("nothing is built yet: {}", reply_kind(&other)),
        }
        patch(&mut agent, json!([{ "op": "add", "path": "/form/children/-", "value": page("PN_Details") }])).await;
        reply_text(agent.execute("build_aem_package", &json!({})).await);
        let checked = reply_text(
            agent
                .execute("aem_verify_package_check", &json!({ "package_path": "/nonexistent/other.zip" }))
                .await,
        );
        assert!(checked.contains("/content/forms/af/"), "{checked}");
    }

    #[test]
    fn source_key_defaults_to_current() {
        assert_eq!(
            ConversionAgent::source_key(&json!({})),
            "current"
        );
        assert_eq!(
            ConversionAgent::source_key(&json!({"source": {"reference": "abc"}})),
            "reference:abc"
        );
    }

    /// The pure windowing behind `read_package_file`: a source
    /// under the window is returned whole and silently, one that reaches past
    /// it says so and names where to continue.
    #[test]
    fn windowed_text_is_silent_under_the_window_and_says_so_over_it() {
        let short = "line1\nline2\nline3";
        assert_eq!(execute::windowed_text(short, 0, 10), short);
        assert!(
            !execute::windowed_text(short, 0, 10).contains("showing lines"),
            "a source that fits must not carry a truncation note"
        );

        let long: String = (0..5000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let windowed = execute::windowed_text(&long, 0, 100);
        assert_eq!(windowed.lines().count(), 101, "100 lines plus the note");
        assert!(windowed.starts_with("line0\nline1"));
        assert!(!windowed.contains("line100"), "the window must stop at the limit");
        assert!(
            windowed.contains("showing lines 1-100 of 5000"),
            "the note must say what was shown and how much more there is: {windowed}"
        );
    }

    /// A source that fits in one window comes back byte for byte — not
    /// reconstructed through `lines().join("\n")`, which would silently drop
    /// a trailing newline or turn CRLF into LF even though nothing was cut.
    #[test]
    fn a_source_that_fits_is_returned_byte_for_byte() {
        let with_trailing_newline = "a\r\nb\r\nc\r\n";
        assert_eq!(
            execute::windowed_text(with_trailing_newline, 0, 10),
            with_trailing_newline,
            "no reconstruction may happen when nothing was windowed"
        );
    }

    /// An offset past the end is a distinct case from "this source is empty":
    /// the caller paged past what exists, and needs to be told so rather than
    /// reading an empty reply as "no more content of any kind here."
    #[test]
    fn an_offset_past_the_end_says_so_rather_than_returning_silently_empty() {
        let short = "line1\nline2\nline3";
        let out = execute::windowed_text(short, 100, 10);
        assert!(
            out.contains("past the end") && out.contains('3'),
            "must name both that it is past the end and the source's real line count: {out}"
        );
    }

    /// `limit == 0`, whether omitted or given explicitly, means the default
    /// window — not "everything", which is `read_reference_file`'s convention
    /// but not safe here (see `windowed_text`'s doc for the form that would
    /// have overflowed the window under that convention).
    #[test]
    fn zero_limit_means_the_default_window_not_everything() {
        let long: String = (0..5000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let default = execute::windowed_text(&long, 0, 0);
        assert_eq!(
            default.lines().count() - 1, // minus the note line
            execute::DEFAULT_TEXT_WINDOW_LINES,
            "limit 0 must apply the default window, not read the whole 5000-line source"
        );
    }

    /// However large a `limit` is requested, and however many sources a caller
    /// joins into one reply, the assembled total must never reach the scale
    /// that caused the incident — this is what actually enforces that, since
    /// `windowed_text` alone only bounds one source's own window.
    #[test]
    fn cap_total_bounds_an_explicit_request_regardless_of_size() {
        let huge = "x".repeat(execute::MAX_TOTAL_REPLY_CHARS * 3);
        let capped = execute::cap_total(huge);

        assert!(capped.len() <= execute::MAX_TOTAL_REPLY_CHARS + 300);
        assert!(capped.contains("truncated"));
    }

    /// A reply already under the ceiling must come back untouched — the cap is
    /// a backstop, not a rewrite of every reply.
    #[test]
    fn cap_total_leaves_a_small_reply_alone() {
        let small = "hello".to_string();
        assert_eq!(execute::cap_total(small.clone()), small);
    }

    /// `offset` actually pages: two windows starting at different offsets
    /// cover different content, and paging from where the first window left
    /// off reaches lines the first window did not show.
    #[test]
    fn offset_pages_through_a_source_larger_than_one_window() {
        let long: String = (0..5000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let first = execute::windowed_text(&long, 0, 100);
        let second = execute::windowed_text(&long, 100, 100);

        assert_ne!(first, second);
        assert!(second.starts_with("line100\nline101"));
        assert!(!first.contains("line100"), "the first window must not reach into the second");
    }

    #[tokio::test]
    async fn xfa_render_page_replies_with_the_page_image() {
        let mut agent = agent_for(OutputTarget::Redacto, vec![fixture("AAOS_033_IT.pdf")]);
        let info: Value = serde_json::from_str(&reply_text(agent.execute("get_source_info", &json!({})).await)).unwrap();
        let doc_path = info["documents"][0]["doc_path"]
            .as_str()
            .unwrap()
            .to_string();
        let reply = agent
            .execute(
                "xfa_render_page",
                &json!({ "doc_path": doc_path, "page": 1, "dpi": 72 }),
            )
            .await;
        match reply {
            ToolReply::Blocks(blocks) => assert!(
                blocks
                    .iter()
                    .any(|b| matches!(b, ReplyBlock::Image { media_type, .. } if media_type.starts_with("image/"))),
                "no image block in the render reply"
            ),
            other => panic!("expected image blocks, got {:?}", reply_kind(&other)),
        }
    }

    #[tokio::test]
    async fn a_doc_path_outside_the_runs_documents_is_refused() {
        let mut agent = agent_for(OutputTarget::Aem, vec![fixture("AAOS_033_IT.pdf")]);
        let outside = std::path::Path::new(SOURCE_FORMS)
            .join("AAEV_019_EN.pdf")
            .display()
            .to_string();
        for tool in ["xfa_packets", "xfa_info", "pdf_info"] {
            match agent
                .execute(tool, &json!({ "doc_path": outside }))
                .await
            {
                ToolReply::Error(e) => assert!(e.contains("not one of this run's documents"), "{e}"),
                other => panic!("{tool} read a file outside the run: {:?}", reply_kind(&other)),
            }
        }
    }

}

mod catalog;
mod evidence;
mod execute;
mod prompts;

use catalog::target_mask;
pub use catalog::{Access, ToolSpec, access_of, all_tools, catalog, scope, target, tools_for};
pub(crate) use evidence::json_of;
pub use execute::ReadWork;
pub use prompts::*;

use crate::OutputTarget;
