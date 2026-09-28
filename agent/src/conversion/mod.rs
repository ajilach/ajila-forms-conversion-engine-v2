//! The conversion agent's engine surface — the tool catalog and the executor
//! that drives the form-conversion engine.
//!
//! The agent extracts from the source PDF, builds and edits a **structured**
//! node tree, converts to an **AEM** node tree, edits that, packages it,
//! optionally uploads to AEM and verifies, and can consult reference forms /
//! documentation. Every tree change is snapshotted into an edit-history session
//! ([`crate::db`]) so a UI can review the full history.
//!
//! Tree mutations use a **whole-tree replace** model: the caller reads a tree
//! (`get_*`) and writes the whole tree back (`set_*`); each write is versioned.
//!
//! This type holds no LLM and no UI state: an external loop streams turns,
//! calls [`ConversionAgent::tools`] / [`ConversionAgent::execute`], and surfaces
//! the results. Network tools hit the engine's AEM client.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use blueprint::{
    AemConfig, AemI18nText, AemNode, AemNodeTranslated, AemOptionTranslated,
    Context, OutputTarget, RedactoDump, StructuredNode,
};

/// Error returned by the AEM-tree tools when nothing has been authored yet.
const NO_AEM_TREE: &str = "No AEM tree yet; author it with set_aem_translated.";

/// Error returned by the package tools before [`ConversionAgent::package`] is
/// populated. Public so the MCP server's `write_package` reports the same thing.
pub const NO_PACKAGE: &str = "No package built yet; call build_aem_package.";

/// Error returned by the structured-tree tools when nothing has been authored
/// yet.
const NO_STRUCTURED_TREE: &str = "No structured tree yet; author one with set_structured.";

/// Returned when AEM-only machinery is reached in a run aimed at another target.
/// Should be unreachable through the app (roles are never offered out-of-scope
/// tools) but not through MCP, which serves the flat catalog.
const AEM_ONLY_STATE: &str = "This run targets Redacto; no AEM state exists.";

/// All language codes appearing in any text field of a working tree (used to
/// keep a pre-loaded template's languages alive through lowering, and to pick
/// the languages a restored tree is lowered with — see [`crate::session`]).
pub(crate) fn collect_translated_languages(
    tree: &AemNodeTranslated,
) -> std::collections::BTreeSet<String> {
    fn add(text: &AemI18nText, out: &mut std::collections::BTreeSet<String>) {
        out.extend(text.languages().map(String::from));
    }
    fn add_opts(opts: &[AemOptionTranslated], out: &mut std::collections::BTreeSet<String>) {
        for o in opts {
            add(&o.label, out);
        }
    }
    fn walk(node: &AemNodeTranslated, out: &mut std::collections::BTreeSet<String>) {
        match node {
            AemNodeTranslated::Root { title, children } => {
                add(title, out);
                children.iter().for_each(|c| walk(c, out));
            }
            AemNodeTranslated::Panel {
                title, children, ..
            } => {
                add(title, out);
                children.iter().for_each(|c| walk(c, out));
            }
            AemNodeTranslated::Repeatable {
                title, children, ..
            } => {
                add(title, out);
                children.iter().for_each(|c| walk(c, out));
            }
            AemNodeTranslated::TextField { label, .. }
            | AemNodeTranslated::NumberField { label, .. }
            | AemNodeTranslated::DatePicker { label, .. } => add(label, out),
            AemNodeTranslated::Dropdown { label, options, .. }
            | AemNodeTranslated::Checkbox { label, options, .. }
            | AemNodeTranslated::RadioButton { label, options, .. }
            | AemNodeTranslated::Custom { label, options, .. } => {
                add(label, out);
                add_opts(options, out);
            }
            AemNodeTranslated::TextDraw { content, .. }
            | AemNodeTranslated::MessageBox { content, .. }
            | AemNodeTranslated::TitleDraw { content, .. }
            | AemNodeTranslated::HtmlDisplayer { content, .. } => add(content, out),
            AemNodeTranslated::Fragment { .. }
            | AemNodeTranslated::Preface { .. }
            | AemNodeTranslated::Appendix { .. }
            | AemNodeTranslated::FootnotePlaceholder { .. } => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(tree, &mut out);
    out
}

/// The package writer's translation dictionary: master text → { lang → text }.
type I18nDict = std::collections::HashMap<String, std::collections::HashMap<String, String>>;

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

/// Validate FileVault package bytes (session-agnostic).
///
/// Runs the same checks as the `validate_aem_package` tool: required FileVault
/// structure, form `.content.xml` (`cq:Page`) validation, and DAM
/// `.content.xml` (`dam:Asset`) validation. Returns `Ok(success message)` when
/// the package is valid, or `Err(problem report)` listing every violation.
pub fn validate_package_bytes(pkg: &[u8]) -> Result<String, String> {
    let files = crate::references::unzip_package(pkg)
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
    let form_xml = files.iter().find(|(p, c)| {
        p.starts_with("jcr_root/content/forms/af/")
            && p.ends_with("/.content.xml")
            && c.contains("\"cq:Page\"")
    });
    match form_xml {
        Some((path, xml)) => {
            if let Err(violations) = blueprint::validate_aem_form_xml(xml) {
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
            if let Err(violations) = blueprint::validate_aem_dam_xml(xml) {
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
            "✓ Package valid: {} entries; required FileVault structure present; \
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

// ── Per-source extraction (sync; cached) ─────────────────────────────────────

/// The engine's reading of one input source (the uploaded form, or a
/// reference): each source PDF's name and context, in upload order.
///
/// A context carries its PDF's language and XFA variables. The language
/// variants do not share one, so an output with single-valued configuration
/// must pick one deliberately (see [`ConversionAgent::source_context`]).
/// `None` for a PDF the engine could not read.
struct Extractor {
    documents: Vec<(String, Option<Context>)>,
}

impl Extractor {
    fn build(pdfs: &[(String, Vec<u8>)]) -> Self {
        let documents = pdfs
            .iter()
            .map(|(name, bytes)| {
                let context = blueprint::Blueprint::from_pdf_bytes(bytes)
                    .ok()
                    .map(|bp| bp.context());
                (name.clone(), context)
            })
            .collect();
        Extractor { documents }
    }

    fn contexts(&self) -> impl Iterator<Item = &Context> {
        self.documents.iter().filter_map(|(_, c)| c.as_ref())
    }
}

// ── The agent ────────────────────────────────────────────────────────────────

/// Everything a run aimed at [`OutputTarget::Aem`] accumulates.
#[derive(Default)]
struct AemState {
    config: Option<AemConfig>,
    /// The working multilingual AEM tree the agent authors directly. Lowered to
    /// `(AemNode, translations)` at build/review time.
    tree: Option<AemNodeTranslated>,
    package: Option<Vec<u8>>,
    /// The same form, built with `bind_to_xsd` on: every field carries a
    /// `bindRef` and the schema those paths belong to is bundled.
    ///
    /// Kept alongside the plain package rather than replacing it, because the two
    /// are for different deployments — the plain one is what UBS installs today.
    package_bound: Option<Vec<u8>>,
    /// The derived `#aem` edit-history session id, once anything is snapshotted.
    session: Option<String>,
}

/// Everything a run aimed at [`OutputTarget::Redacto`] accumulates.
///
/// The authored document itself lives in [`ConversionAgent::structured`], which
/// both targets share; this is only what building the dump produces.
#[derive(Default)]
struct RedactoState {
    /// The dump from the most recent `build_redacto_dump`, reused by `finalize`
    /// so the shipped SQL is the one the agent last saw validated.
    dump: Option<RedactoDump>,
}

/// The per-target half of the agent's state.
///
/// Splitting it makes an AEM tool structurally unreachable in a Redacto run
/// rather than merely un-offered: the app filters tools by role name, but MCP
/// serves the flat catalog, so the guarantee has to live here.
enum TargetState {
    Aem(Box<AemState>),
    Redacto(RedactoState),
}

impl TargetState {
    fn new(target: OutputTarget) -> Self {
        match target {
            OutputTarget::Aem => TargetState::Aem(Box::default()),
            OutputTarget::Redacto => TargetState::Redacto(RedactoState::default()),
        }
    }

    fn target(&self) -> OutputTarget {
        match self {
            TargetState::Aem(_) => OutputTarget::Aem,
            TargetState::Redacto(_) => OutputTarget::Redacto,
        }
    }

    fn aem(&self) -> Option<&AemState> {
        match self {
            TargetState::Aem(state) => Some(state),
            TargetState::Redacto(_) => None,
        }
    }

    fn aem_mut(&mut self) -> Option<&mut AemState> {
        match self {
            TargetState::Aem(state) => Some(state),
            TargetState::Redacto(_) => None,
        }
    }

    fn redacto_mut(&mut self) -> Option<&mut RedactoState> {
        match self {
            TargetState::Redacto(state) => Some(state),
            TargetState::Aem(_) => None,
        }
    }
}

pub struct ConversionAgent {
    profile: Option<String>,
    context: Context,
    current_pdfs: Vec<(String, Vec<u8>)>,
    extractors: HashMap<String, Extractor>,

    /// The page header per language, as the agent read it off the source's
    /// master page and set it with `set_structured`. Output targets with a
    /// page-header slot (Redacto's `page.header`) render it; recorded under the
    /// session's `#headers` sibling so a resumed run keeps it.
    headers: BTreeMap<String, String>,

    /// The working structured tree. Under [`OutputTarget::Redacto`] this is what
    /// the agent authors and the dump is generated from; under
    /// [`OutputTarget::Aem`] it stays empty (the agent authors the AEM tree
    /// directly) and only feeds `config()`'s language detection when a resumed
    /// session seeded it.
    structured: Vec<StructuredNode>,

    /// State belonging to the output target this run aims at.
    target: TargetState,

    structured_session: String,

    /// Sentence-embedding model backing semantic `search_references`. Loaded
    /// lazily on first use (~200ms) and reused for the rest of the run.
    matcher: Option<blueprint::semantic::SemanticMatcher>,

    /// The Reviewer role's latest `submit_review` outcome, drained by the
    /// controller via [`take_review`](Self::take_review).
    review: Option<ReviewResult>,

    /// The vendored u2s tool servers, created on the first u2s call or
    /// `get_source_info` (see [`Self::u2s_tools`]).
    u2s: Option<crate::u2s::U2sTools>,
}

impl ConversionAgent {
    /// `files` may mix source PDFs and a single AEM content-package ZIP. The
    /// PDFs are the conversion source; the ZIP (if any) is parsed into an
    /// `AemNodeTranslated` and pre-loaded as the working tree, acting as an
    /// editable template the agent modifies instead of authoring from scratch.
    ///
    /// `target` fixes what the run produces, and with it which half of the
    /// agent's state exists at all: an uploaded template is only meaningful for
    /// [`OutputTarget::Aem`] and is ignored otherwise.
    pub fn new(
        profile: Option<String>,
        files: Vec<(String, Vec<u8>)>,
        structured_session: String,
        target: OutputTarget,
    ) -> Self {
        let pdfs: Vec<(String, Vec<u8>)> = files
            .iter()
            .filter(|(name, _)| name.to_ascii_lowercase().ends_with(".pdf"))
            .cloned()
            .collect();

        // First AEM content-package ZIP, parsed once for both the template tree
        // and (for template-only runs) the document context/language.
        let template_bp = files
            .iter()
            .find(|(_, b)| blueprint::aem::detect_aem_zip(b))
            .and_then(|(_, b)| blueprint::Blueprint::from_aem_zip(b).ok());

        let context = pdfs
            .iter()
            .find_map(|(_, b)| {
                blueprint::Blueprint::from_pdf_bytes(b)
                    .ok()
                    .map(|bp| bp.context())
            })
            .or_else(|| template_bp.as_ref().map(|bp| bp.context()))
            .unwrap_or_else(|| Context::with_language("en"));

        let mut target_state = TargetState::new(target);
        if let Some(aem) = target_state.aem_mut() {
            aem.tree = template_bp.as_ref().and_then(|bp| bp.aem_translated());
        }

        let mut agent = Self {
            profile,
            context,
            current_pdfs: pdfs,
            extractors: HashMap::new(),
            structured: Vec::new(),
            headers: BTreeMap::new(),
            target: target_state,
            structured_session,
            matcher: None,
            review: None,
            u2s: None,
        };
        // Record the pre-loaded template as the initial AEM edit so it shows in
        // the AEM edit history (no-op when no template was uploaded).
        if agent.aem_tree().is_some() {
            agent.aem_translated_edited("Template (from uploaded package)");
        }
        agent
    }

    // ── Target-state access ──────────────────────────────────────────────────

    /// The output target this run aims at.
    pub fn target(&self) -> OutputTarget {
        self.target.target()
    }

    /// The working AEM tree, if this is an AEM run that has one.
    fn aem_tree(&self) -> Option<&AemNodeTranslated> {
        self.target.aem().and_then(|s| s.tree.as_ref())
    }

    /// Mutable access to the working AEM tree, if this is an AEM run with one.
    fn aem_tree_mut(&mut self) -> Option<&mut AemNodeTranslated> {
        self.target.aem_mut().and_then(|s| s.tree.as_mut())
    }

    /// Lazily load (and cache) the sentence-embedding model used by semantic
    /// `search_references`.
    fn matcher(&mut self) -> Result<&blueprint::semantic::SemanticMatcher, String> {
        if self.matcher.is_none() {
            self.matcher =
                Some(blueprint::semantic::SemanticMatcher::new().map_err(|e| e.to_string())?);
        }
        Ok(self.matcher.as_ref().unwrap())
    }

    /// Seed the working structured tree (used when resuming a session to apply
    /// user feedback to a prior result).
    pub fn seed_structured(&mut self, nodes: Vec<StructuredNode>) {
        self.structured = nodes;
    }

    /// Seed the working AEM tree from a restored session, so feedback is applied
    /// to the tree the previous run actually authored instead of re-deriving one
    /// from the source.
    ///
    /// Deliberately does *not* snapshot: the tree came out of the history, and
    /// re-recording it would add a no-op entry to every resumed session.
    pub fn seed_aem_translated(&mut self, tree: AemNodeTranslated) {
        if let Some(aem) = self.target.aem_mut() {
            aem.tree = Some(tree);
            aem.package = None;
        }
    }

    /// Point the package tools at an existing FileVault ZIP instead of one this
    /// agent built. Lets a read-only caller — the describe-a-reference step —
    /// inspect an uploaded package with `get_package_info` / `read_package_file`.
    pub fn seed_package(&mut self, zip: Vec<u8>) {
        if let Some(aem) = self.target.aem_mut() {
            aem.package = Some(zip);
        }
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
    /// `redacto_verify_*` tools import the built dump into a throwaway
    /// Postgres. Run [`crate::u2s::redacto_verify_readiness`] first.
    pub fn with_redacto_verify(
        mut self,
        settings: &crate::u2s::RedactoVerifySettings,
    ) -> Result<Self, String> {
        self.u2s_tools()?.attach_redacto_verify(settings);
        Ok(self)
    }

    /// Whether a verifier is attached (and not yet torn down).
    pub fn has_verifier(&self) -> bool {
        self.u2s.as_ref().is_some_and(|t| t.has_verifier())
    }

    /// The tools a stage is offered.
    pub fn tools_for_stage(&self, scopes: scope::Mask) -> Vec<serde_json::Value> {
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
        match self.target() {
            OutputTarget::Aem => self.package().map(|bytes| crate::u2s::Artifact {
                file_name: "package.zip",
                bytes,
            }),
            OutputTarget::Redacto => self.redacto_dump().map(|dump| crate::u2s::Artifact {
                file_name: "dump.sql",
                bytes: dump.to_sql().into_bytes(),
            }),
        }
    }

    // ── Public accessors (for the driving loop's result finalization) ─────────

    /// Drain the Reviewer role's latest `submit_review` outcome (the controller
    /// reads this after running the Reviewer stage).
    pub fn take_review(&mut self) -> Option<ReviewResult> {
        self.review.take()
    }

    /// The detected document context (language, …).
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// The current working structured tree.
    ///
    /// Empty on a fresh AEM run: the agent authors the AEM tree directly and
    /// only seeds this when resuming a session.
    pub fn structured(&self) -> &[StructuredNode] {
        &self.structured
    }

    /// The page headers the agent authored, by language.
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    /// Restore page headers recorded by an earlier run of this session.
    pub fn seed_headers(&mut self, headers: BTreeMap<String, String>) {
        self.headers = headers;
    }

    /// One context per readable source PDF, in upload order, each carrying the
    /// page header the agent authored for its language.
    ///
    /// Falls back to [`context`](Self::context) when no source PDF is readable
    /// (a template-only run).
    fn source_contexts(&mut self) -> Vec<Context> {
        let mut contexts: Vec<Context> = match self.extractor(&serde_json::json!({})) {
            Ok(ex) => ex.contexts().cloned().collect(),
            Err(_) => Vec::new(),
        };
        if contexts.is_empty() {
            contexts.push(self.context.clone());
        }
        for context in &mut contexts {
            context.header = self.headers.get(context.language()).cloned();
        }
        contexts
    }

    /// The source context to resolve an output configuration against, preferring
    /// the variant written in `master_language`.
    ///
    /// Each language variant of a document carries its own page header and its
    /// own `Footer_Line_*` XFA variables, so a single-valued configuration
    /// (Redacto's `header`/`footer`) takes whichever variant it is pointed at.
    /// Defaulting to upload order made that arbitrary — a document uploaded
    /// SP-first got a Spanish header on an English-master document.
    pub fn source_context(&mut self, master_language: &str) -> Context {
        let contexts = self.source_contexts();
        contexts
            .iter()
            .find(|c| c.language() == master_language)
            .unwrap_or(&contexts[0])
            .clone()
    }

    /// The working AEM (translated) tree — what the agent actually authored.
    ///
    /// This is the run's real product: the structured tree stays empty, so any
    /// consumer that wants the authored document (the editors, the recorded
    /// snapshot) must go through here rather than [`structured`](Self::structured).
    pub fn aem_translated(&self) -> Option<&AemNodeTranslated> {
        self.aem_tree()
    }

    /// The most recently built AEM package (ZIP), if any.
    pub fn package(&self) -> Option<Vec<u8>> {
        self.target.aem().and_then(|s| s.package.clone())
    }

    /// The package built with `bind_to_xsd` on, if one was built.
    pub fn package_bound(&self) -> Option<Vec<u8>> {
        self.target.aem().and_then(|s| s.package_bound.clone())
    }

    /// The resolved form code, if the AEM config has been loaded.
    pub fn form_code(&self) -> Option<String> {
        self.target
            .aem()
            .and_then(|s| s.config.as_ref())
            .map(|c| c.form_code.clone())
    }

    /// The derived AEM edit-history session id, if any AEM snapshot was taken.
    pub fn aem_session(&self) -> Option<String> {
        self.target.aem().and_then(|s| s.session.clone())
    }

    /// The edit-history session this agent was constructed with. Empty for a
    /// throwaway agent with no session of its own (`describe_reference`'s
    /// one-shot pass) — a stage's conversation is only persisted when this is
    /// non-empty.
    pub fn session_id(&self) -> &str {
        &self.structured_session
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn source_key(input: &serde_json::Value) -> String {
        match input["source"]["reference"].as_str() {
            Some(id) => format!("reference:{id}"),
            None => "current".to_string(),
        }
    }

    /// The PDFs of the requested source: the uploaded form, or a reference's
    /// input.
    fn source_pdfs(&self, input: &serde_json::Value) -> Result<Vec<(String, Vec<u8>)>, String> {
        match input["source"]["reference"].as_str() {
            Some(id) => {
                let bytes = crate::references::get_reference_pdf_bytes(id, 0)?;
                Ok(vec![(format!("{id}.pdf"), bytes)])
            }
            None => Ok(self.current_pdfs.clone()),
        }
    }

    /// The u2s tool servers, created on first use.
    fn u2s_tools(&mut self) -> Result<&mut crate::u2s::U2sTools, String> {
        let tools = match self.u2s.take() {
            Some(tools) => tools,
            None => crate::u2s::U2sTools::new()?,
        };
        Ok(self.u2s.insert(tools))
    }

    /// The requested source's PDFs, written where the u2s tools may read them:
    /// (file name, language, `doc_path`).
    fn source_documents(
        &mut self,
        input: &serde_json::Value,
    ) -> Result<Vec<(String, String, std::path::PathBuf)>, String> {
        let pdfs = self.source_pdfs(input)?;
        let languages: Vec<String> = self
            .extractor(input)?
            .documents
            .iter()
            .map(|(_, c)| c.as_ref().map_or("unknown", |c| c.language()).to_string())
            .collect();
        let group = Self::source_key(input).replace(':', "-");
        let tools = self.u2s_tools()?;
        pdfs.iter()
            .zip(languages)
            .map(|((name, bytes), language)| {
                let path = tools.add_document(&group, name, bytes)?;
                Ok((name.clone(), language, path))
            })
            .collect()
    }

    /// Get (building+caching if needed) the extractor for the requested source.
    fn extractor(&mut self, input: &serde_json::Value) -> Result<&Extractor, String> {
        let key = Self::source_key(input);
        if !self.extractors.contains_key(&key) {
            let pdfs = self.source_pdfs(input)?;
            self.extractors.insert(key.clone(), Extractor::build(&pdfs));
        }
        Ok(self.extractors.get(&key).unwrap())
    }

    /// The resolved AEM configuration. AEM-only by construction: a Redacto run
    /// has no `AemState` to cache it on, and its language resolution (which
    /// prefers the AEM profile's list) is the wrong answer for a Redacto
    /// document — that uses `resolve_redacto_languages` instead.
    fn config(&mut self) -> Result<AemConfig, String> {
        let cached = self.target.aem().ok_or(AEM_ONLY_STATE)?.config.clone();
        let cfg = match cached {
            Some(cfg) => cfg,
            None => {
                let p = self
                    .profile
                    .clone()
                    .ok_or("No profile selected — AEM conversion needs a profile.")?;
                let loaded = blueprint::load_aem_config(&p, &self.context)?;
                if let Some(aem) = self.target.aem_mut() {
                    aem.config = Some(loaded.clone());
                }
                loaded
            }
        };
        // Reflect the languages actually present in the document so
        // get_profile_info and the package builder never misreport a
        // multilingual form as en-only. `resolve_aem_languages` only overrides
        // when it detects ≥1 language, so monolingual flows keep the default.
        // Resolved per-call (not cached) because set_structured mutates
        // self.structured without touching the cached config. Prefer the working
        // tree once seeded; otherwise fall back to the merged source extraction
        // so the languages are reported even before the tree is seeded.
        let mut cfg = if !self.structured.is_empty() {
            blueprint::resolve_aem_languages(&self.structured, &cfg)
        } else {
            self.with_source_languages(cfg)
        };
        // Carry any languages present in the working tree (e.g. a pre-loaded
        // template) into the config so they survive lowering — important for
        // template-only runs where there is no PDF source to detect them from.
        if let Some(tree) = self.aem_tree() {
            for lang in collect_translated_languages(tree) {
                if !cfg.languages.contains(&lang) {
                    cfg.languages.push(lang);
                }
            }
        }
        Ok(cfg)
    }

    /// `cfg` with its languages replaced by the source PDFs' own, when there is
    /// at least one readable source PDF (the same rule `resolve_aem_languages`
    /// applies to a tree).
    fn with_source_languages(&mut self, mut cfg: AemConfig) -> AemConfig {
        let Ok(ex) = self.extractor(&serde_json::json!({})) else {
            return cfg;
        };
        let languages: BTreeSet<String> = ex.contexts().map(|c| c.language().to_string()).collect();
        if !languages.is_empty() {
            cfg.languages = languages.into_iter().collect();
        }
        cfg
    }

    /// Snapshot the working AEM (translated) tree for versioning.
    fn snapshot_aem_translated(&mut self, label: &str) {
        let derived_session = format!("{}#aem", self.structured_session);
        let Some(aem) = self.target.aem_mut() else {
            return;
        };
        let Some(ref tree) = aem.tree else {
            return;
        };
        let Ok(json) = serde_json::to_string(tree) else {
            return;
        };
        let sid = aem.session.get_or_insert(derived_session).clone();
        crate::db::insert_edit(&sid, label, &json);
    }

    /// Common tail of every structured-tree edit: invalidate the built dump,
    /// then snapshot the tree into the edit history.
    ///
    /// Recording into `structured_session` (the primary session, not the derived
    /// `#aem` one) is what makes a Redacto run reopenable: `session::restore`
    /// already prefers a non-empty structured snapshot, so it needs no changes.
    fn structured_edited(&mut self, label: &str) {
        if let Some(redacto) = self.target.redacto_mut() {
            redacto.dump = None;
        }
        if let Ok(json) = serde_json::to_string(&self.structured) {
            crate::db::insert_edit(&self.structured_session, label, &json);
        }
    }

    /// Replace the page headers and record them under the `#headers` sibling
    /// session, so a resumed run restores them.
    fn set_headers(&mut self, headers: BTreeMap<String, String>) {
        if let Some(redacto) = self.target.redacto_mut() {
            redacto.dump = None;
        }
        self.headers = headers;
        if let Ok(json) = serde_json::to_string(&self.headers) {
            crate::db::insert_edit(
                &format!("{}#headers", self.structured_session),
                "AI: set page headers",
                &json,
            );
        }
    }

    /// Build the Redacto dump for the working structured tree, and cache it.
    ///
    /// The contexts are [`source_contexts`](Self::source_contexts), so the page
    /// header the agent authored reaches the profile's `page.header`. All
    /// language variants are passed: the page header and footer are rendered
    /// per language, and core picks the master-language variant for the
    /// document's identity.
    fn build_redacto(&mut self) -> Result<(RedactoDump, blueprint::RedactoConfig), String> {
        let profile = self
            .profile
            .clone()
            .ok_or("No profile selected — the Redacto dump needs a profile.")?;
        let contexts = self.source_contexts();
        let (dump, config) =
            blueprint::to_redacto_dump_for_profile(&profile, &contexts, &self.structured)?;
        if let Some(redacto) = self.target.redacto_mut() {
            redacto.dump = Some(dump.clone());
        }
        Ok((dump, config))
    }

    /// The dump from the most recent `build_redacto_dump`, if one succeeded.
    pub fn redacto_dump(&self) -> Option<&RedactoDump> {
        match &self.target {
            TargetState::Redacto(state) => state.dump.as_ref(),
            TargetState::Aem(_) => None,
        }
    }

    /// Common tail of every AEM-tree edit: invalidate the package, then snapshot.
    fn aem_translated_edited(&mut self, label: &str) {
        if let Some(aem) = self.target.aem_mut() {
            aem.package = None;
        }
        self.snapshot_aem_translated(label);
    }

    // ── Edit-arm plumbing ──────────────────────────────────────────────────────
    //
    // The nine structured/AEM editing tools differ only in which editor function
    // they call and how they label the snapshot. These three helpers own
    // everything else: the `AI:` label prefix, the "no tree yet" guard, and the
    // Ok/Err → ToolReply mapping.

    /// Run a structured-tree edit and record it: on success snapshot the tree
    /// under `AI: <label>` and report the editor's message, on failure surface
    /// the error unchanged.
    fn edit_structured(
        &mut self,
        label: std::fmt::Arguments<'_>,
        result: Result<String, String>,
    ) -> ToolReply {
        match result {
            Ok(msg) => {
                self.structured_edited(&format!("AI: {label}"));
                ToolReply::Text(msg)
            }
            Err(e) => ToolReply::Error(e),
        }
    }

    /// Run an AEM-tree edit against the working tree and record it.
    ///
    /// Takes a closure rather than a `&mut AemNodeTranslated` because
    /// [`aem_tree_mut`](Self::aem_tree_mut) and
    /// [`aem_translated_edited`](Self::aem_translated_edited) both borrow `self`
    /// mutably: the tree borrow has to end before the snapshot is taken.
    fn edit_aem(
        &mut self,
        label: std::fmt::Arguments<'_>,
        edit: impl FnOnce(&mut AemNodeTranslated) -> Result<String, String>,
    ) -> ToolReply {
        let result = match self.aem_tree_mut() {
            Some(root) => edit(root),
            None => return ToolReply::Error(NO_AEM_TREE.into()),
        };
        match result {
            Ok(msg) => {
                self.aem_translated_edited(&format!("AI: {label}"));
                ToolReply::Text(msg)
            }
            Err(e) => ToolReply::Error(e),
        }
    }

    /// The structured content the derived outputs (XSD, HTML) render from.
    ///
    /// A Redacto run authors [`structured`](Self::structured) directly. An AEM
    /// run leaves it empty and authors the AEM tree instead, so the tree is
    /// lifted back to structured content — the same conversion the app's
    /// finalization does. Without this, both tools silently rendered an empty
    /// document on every AEM run.
    fn derived_output_content(&mut self) -> Result<Vec<StructuredNode>, String> {
        if !self.structured.is_empty() {
            return Ok(self.structured.clone());
        }
        let profile = self.profile.clone();
        let tree = self.aem_tree().ok_or(
            "Nothing to render yet: no structured content, and no AEM tree to derive it from.",
        )?;
        let content = crate::session::structured_from_aem_tree(tree, profile.as_deref());
        if content.is_empty() {
            return Err("The AEM tree produced no structured content to render.".into());
        }
        Ok(content)
    }

    /// Read from the working AEM tree, with the same "no tree yet" guard the
    /// editing tools use.
    fn read_aem(&mut self, read: impl FnOnce(&mut AemNodeTranslated) -> ToolReply) -> ToolReply {
        match self.aem_tree_mut() {
            Some(root) => read(root),
            None => ToolReply::Error(NO_AEM_TREE.into()),
        }
    }

    /// Lower the working multilingual tree to the single-language `AemNode` plus
    /// the master-text-keyed translation dictionary the package writer consumes.
    fn lower_aem_translated(&mut self) -> Result<(AemNode, I18nDict), String> {
        let cfg = self.config()?;
        self.lower_aem_translated_with(&cfg)
    }

    /// Lower the working tree against `cfg` rather than the run's own config, so
    /// a bound build derives bound `bindRef`s from the same tree.
    fn lower_aem_translated_with(&self, cfg: &AemConfig) -> Result<(AemNode, I18nDict), String> {
        let tree = self.aem_tree().ok_or(NO_AEM_TREE)?;
        let (mut node, dict) = tree.lower(&cfg.master_language, &cfg.languages);

        // Re-derive the bindRefs from the lowered tree.
        //
        // The agent edits the working tree freely — moving a field between
        // sections changes its bind path — and the package writer generates the
        // schema from this same tree. Without this the two would drift: the
        // shipped `.content.xml` would carry bindRefs from whenever the tree was
        // last built while the bundled XSD described the tree as it is now.
        if let Some(xsd_config) = cfg.xsd_config.as_ref().filter(|_| cfg.bind_to_xsd) {
            let result = blueprint::generate_xsd_from_aem(&node, xsd_config, &cfg.fragments);
            blueprint::apply_bind_refs(&mut node, &result.bind_refs);
        }
        Ok((node, dict))
    }

    /// Lower the working AEM tree without needing a fully-resolved profile.
    ///
    /// Rendering the profile's templates needs variables that only exist once a
    /// source document has been ingested. Read-only derivations such as
    /// `generate_xsd` should still work on an authored tree before that, so fall
    /// back to the run's own language when the config cannot be built.
    fn lower_aem_translated_lenient(&mut self) -> Result<(AemNode, I18nDict), String> {
        if let Ok(cfg) = self.config() {
            let tree = self.aem_tree().ok_or(NO_AEM_TREE)?;
            return Ok(tree.lower(&cfg.master_language, &cfg.languages));
        }
        let master = self.context().language().to_string();
        let tree = self.aem_tree().ok_or(NO_AEM_TREE)?;
        Ok(tree.lower(&master, std::slice::from_ref(&master)))
    }

    // ── Tool execution (async: some tools hit the network) ──────────────────────

    /// Why `name` cannot run under this run's output target, if it cannot.
    ///
    /// One guard for the whole AEM family, so a mis-targeted call says what is
    /// actually wrong instead of failing deeper down with something misleading
    /// like "No AEM tree yet". Derived from the catalog, so a tool is scoped in
    /// exactly one place.
    fn target_refusal(&self, name: &str) -> Option<String> {
        let target = self.target.target();
        let scoped_out = catalog()
            .iter()
            .find(|t| t.name() == name)
            .is_some_and(|t| t.targets & target_mask(target) == 0);
        scoped_out.then(|| {
            format!(
                "{name} is not available for the {} output target.",
                target.label()
            )
        })
    }
}

// ── Small helpers ────────────────────────────────────────────────────────────

fn dedup(mut v: Vec<&str>) -> Vec<String> {
    v.sort();
    v.dedup();
    v.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tools the catalog scopes to exactly one output target.
    fn tools_only_for(mask: target::Mask) -> Vec<&'static str> {
        catalog()
            .iter()
            .filter(|t| t.targets == mask)
            .map(|t| t.name())
            .collect()
    }

    /// A minimal bilingual AEM tree: one panel holding one labelled text field.
    fn small_aem_tree() -> serde_json::Value {
        serde_json::json!({
            "type": "Root",
            "title": {"de": "Formular", "en": "Form"},
            "children": [{
                "type": "Panel",
                "uuid": "00000000-0000-0000-0000-000000000001",
                "name": "p1",
                "title": {"de": "Angaben", "en": "Details"},
                "children": [{
                    "type": "TextField",
                    "uuid": "00000000-0000-0000-0000-000000000002",
                    "name": "lastName",
                    "label": {"de": "Nachname", "en": "Last name"},
                    "mandatory": false,
                    "visible": true,
                    "max_chars": null,
                    "colspan": 12,
                    "dor_colspan": null,
                    "bind_ref": null
                }],
                "is_page": false,
                "dor_exclude": false,
                "visible": true,
                "is_conditional": false,
                "dor_num_cols": null,
                "colspan": 12,
                "dor_colspan": null
            }]
        })
    }

    /// Regression: `generate_xsd` and `generate_html` render
    /// [`ConversionAgent::structured`], which an AEM run never fills — yet both
    /// were offered to the AEM Author. They silently emitted an empty document
    /// instead of the form the agent had just authored.
    #[tokio::test]
    async fn derived_outputs_render_the_aem_tree_on_an_aem_run() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-derived-outputs".into(),
            OutputTarget::Aem,
        );
        assert!(
            agent.structured().is_empty(),
            "an AEM run authors the AEM tree, not the structured one"
        );

        // With no tree at all, say so rather than rendering nothing.
        for tool in ["generate_xsd", "generate_html"] {
            let reply = agent.execute(tool, &serde_json::json!({})).await;
            assert!(
                matches!(reply, ToolReply::Error(_)),
                "{tool} must report that there is nothing to render"
            );
        }

        let set = agent
            .execute(
                "set_aem_translated",
                &serde_json::json!({"root": small_aem_tree()}),
            )
            .await;
        assert!(matches!(set, ToolReply::Text(_)), "{set:?}");

        // The field the tree carries has to reach both outputs. The XSD names
        // elements after the label, the HTML renders the label itself.
        for (tool, expected) in [("generate_xsd", "LastName"), ("generate_html", "Last name")] {
            match agent.execute(tool, &serde_json::json!({})).await {
                ToolReply::Text(out) => assert!(
                    out.contains(expected),
                    "{tool} rendered nothing from the authored tree (no {expected:?}): {out}"
                ),
                other => panic!("{tool} failed: {other:?}"),
            }
        }
    }

    /// Regression: `generate_html`'s reply once cost a stage over 800,000
    /// prompt tokens in a single call, because the profile's logo and fonts
    /// were inlined as base64 data URIs — invisible to a text model, and the
    /// direct cause of a run failing with "prompt is too long". The agent
    /// compares the render against the source page images; it never needed
    /// brand-accurate assets for that, only the "ubs" profile that actually
    /// carries them (`profiles/ubs/html/logo.png` plus the Frutiger fonts).
    #[tokio::test]
    async fn generate_html_does_not_inline_profile_assets() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-generate-html-size".into(),
            OutputTarget::Aem,
        );
        let set = agent
            .execute(
                "set_aem_translated",
                &serde_json::json!({"root": small_aem_tree()}),
            )
            .await;
        assert!(matches!(set, ToolReply::Text(_)), "{set:?}");

        let out = match agent.execute("generate_html", &serde_json::json!({})).await {
            ToolReply::Text(out) => out,
            other => panic!("generate_html failed: {other:?}"),
        };
        assert!(
            !out.contains("base64"),
            "the reply inlined an asset as base64, which is what blew the context window"
        );
        assert!(!out.contains("data:"), "the reply carries a data URI");
        assert!(
            out.len() < 20_000,
            "reply is {} bytes — the profile's assets are back", out.len()
        );
    }

    fn fixture(name: &str) -> (String, Vec<u8>) {
        let pdf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../core/input")
            .join(name);
        let bytes = std::fs::read(&pdf).unwrap_or_else(|e| panic!("read {name}: {e}"));
        (name.to_string(), bytes)
    }

    /// The page header is document furniture the agent reads off the source
    /// and sets itself: it must reach the context of its own language, and only
    /// that one.
    #[tokio::test]
    async fn authored_headers_reach_the_context_of_their_language() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAAL_019_SP.pdf"), fixture("AAAL_019_EN.pdf")],
            String::new(),
            OutputTarget::Redacto,
        );
        assert!(agent.source_context("en").header.is_none());

        let reply = agent
            .execute(
                "set_structured",
                &serde_json::json!({
                    "nodes": [],
                    "headers": { "en": "UBS Europe SE", "es": "UBS Europe SE, Sucursal en España" }
                }),
            )
            .await;
        assert!(reply_text(reply).starts_with("OK"));

        assert_eq!(agent.source_context("en").header.as_deref(), Some("UBS Europe SE"));
        assert_eq!(
            agent.source_context("es").header.as_deref(),
            Some("UBS Europe SE, Sucursal en España")
        );

        // A key that is not one of the source's languages would never render.
        match agent
            .execute(
                "set_structured",
                &serde_json::json!({ "nodes": [], "headers": { "EN": "UBS Europe SE" } }),
            )
            .await
        {
            ToolReply::Error(e) => assert!(e.contains("source's languages"), "{e}"),
            _ => panic!("an unknown language key must be refused"),
        }

        // Omitting `headers` keeps the ones already set.
        let reply = agent
            .execute("set_structured", &serde_json::json!({ "nodes": [] }))
            .await;
        assert!(reply_text(reply).starts_with("OK"));
        assert_eq!(agent.headers().len(), 2);
    }

    /// A resumed run restores the page headers the earlier run set, from the
    /// session's `#headers` sibling.
    #[tokio::test]
    async fn page_headers_survive_a_resume() {
        use blueprint::{InlineText, ParagraphNode, TranslatedText};

        let session = format!("test-headers-resume-{}", uuid::Uuid::new_v4());
        let mut content = TranslatedText::empty();
        content.insert("en", InlineText::plain("Body"));
        let nodes = vec![StructuredNode::Paragraph(ParagraphNode {
            content,
            som_path: None,
            source_name: None,
        })];
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            session.clone(),
            OutputTarget::Redacto,
        );
        let reply = agent
            .execute(
                "set_structured",
                &serde_json::json!({
                    "nodes": serde_json::to_value(&nodes).unwrap(),
                    "headers": { "en": "UBS Switzerland AG" }
                }),
            )
            .await;
        assert!(reply_text(reply).starts_with("OK"));

        let restored = crate::session::restore(&session, Some("ubs"));
        crate::db::delete_session(&session);
        let restored = restored.map(|r| r.headers).unwrap_or_default();
        assert_eq!(
            restored.get("en").map(String::as_str),
            Some("UBS Switzerland AG"),
            "{restored:?}"
        );
    }

    #[tokio::test]
    async fn headers_that_are_not_a_language_to_text_map_are_refused() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            String::new(),
            OutputTarget::Redacto,
        );
        match agent
            .execute(
                "set_structured",
                &serde_json::json!({ "nodes": [], "headers": ["UBS Europe SE"] }),
            )
            .await
        {
            ToolReply::Error(e) => assert!(e.contains("headers"), "{e}"),
            _ => panic!("a malformed headers value must be refused"),
        }
        assert!(agent.headers().is_empty());
    }

    /// Each language variant carries its own master-page header and its own
    /// `Footer_Line_*` variables, and the Redacto configuration holds one of
    /// each. Regression: it took whichever PDF was uploaded first, so a
    /// SP-first upload gave an English-master document a Spanish header.
    #[test]
    fn source_context_prefers_the_master_language_variant() {
        // Deliberately upload the non-master language first.
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAAL_019_SP.pdf"), fixture("AAAL_019_EN.pdf")],
            "test-master-context".into(),
            OutputTarget::Redacto,
        );

        assert_eq!(agent.source_context("en").language(), "en");
        assert_eq!(agent.source_context("es").language(), "es");
        // An unknown language falls back to the first variant rather than
        // failing — better an arbitrary header than none.
        assert_eq!(agent.source_context("fr").language(), "es");
    }

    /// The app never offers an out-of-scope tool to a role, but MCP serves the
    /// flat catalog, so the target split has to refuse them itself — and say why
    /// rather than reporting a missing tree.
    #[test]
    fn aem_tools_are_refused_under_the_redacto_target() {
        let agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-redacto-guard".into(),
            OutputTarget::Redacto,
        );

        assert_eq!(agent.target(), OutputTarget::Redacto);
        assert!(agent.aem_translated().is_none());
        assert!(agent.package().is_none());
        assert!(agent.aem_session().is_none());
        assert!(agent.form_code().is_none());

        for tool in tools_only_for(target::AEM) {
            let refusal = agent
                .target_refusal(tool)
                .unwrap_or_else(|| panic!("{tool} must be refused under the Redacto target"));
            assert!(
                refusal.contains("not available for the Redacto"),
                "the refusal must name the target, got: {refusal}"
            );
        }
    }

    /// The guard is symmetric: building a Redacto dump makes no sense in a run
    /// that is authoring an AEM form.
    #[test]
    fn redacto_tools_are_refused_under_the_aem_target() {
        let agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-aem-only-guard".into(),
            OutputTarget::Aem,
        );

        for tool in tools_only_for(target::REDACTO) {
            let refusal = agent
                .target_refusal(tool)
                .unwrap_or_else(|| panic!("{tool} must be refused under the AEM target"));
            assert!(refusal.contains("not available for the AEM"), "{refusal}");
        }
    }

    /// The structured editors belong to neither target exclusively: a resumed
    /// AEM session seeds the same tree.
    #[test]
    fn structured_editors_are_available_under_both_targets() {
        for target in [OutputTarget::Aem, OutputTarget::Redacto] {
            let agent = ConversionAgent::new(
                Some("ubs".into()),
                Vec::new(),
                format!("test-shared-{}", target.as_str()),
                target,
            );
            for tool in [
                "set_structured",
                "get_structured_outline",
                "get_structured_node",
                "set_structured_field",
            ] {
                assert!(
                    agent.target_refusal(tool).is_none(),
                    "{tool} must be available under {target:?}"
                );
            }
        }
    }

    /// `get_schema` declared a `kind` argument but ignored it, so the structured
    /// schema was unreachable even though `blueprint::structured_schema()` had
    /// always been there.
    #[tokio::test]
    async fn get_schema_dispatches_on_kind() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-schema".into(),
            OutputTarget::Redacto,
        );

        let structured = reply_text(
            agent
                .execute("get_schema", &serde_json::json!({"kind": "structured"}))
                .await,
        );
        assert!(
            structured.contains("StructuredNode"),
            "expected the structured schema, got: {}",
            &structured[..200.min(structured.len())]
        );

        // Absent or unknown `kind` keeps the historical AEM answer.
        for input in [
            serde_json::json!({}),
            serde_json::json!({"kind": "nonsense"}),
        ] {
            let aem = reply_text(agent.execute("get_schema", &input).await);
            assert!(
                aem.contains("AemNodeTranslated"),
                "got: {}",
                &aem[..200.min(aem.len())]
            );
        }
    }

    fn reply_text(reply: ToolReply) -> String {
        match reply {
            ToolReply::Text(t) => t,
            ToolReply::Error(e) => panic!("unexpected tool error: {e}"),
            ToolReply::Blocks(_) => panic!("unexpected image reply"),
        }
    }

    /// The whole point of the Redacto target: the Author authors the structured
    /// tree itself, and the dump is generated from exactly that tree, in the
    /// format the vendored u2s verifier decodes.
    #[tokio::test]
    async fn a_tree_authored_with_set_structured_yields_a_shippable_redacto_dump() {
        use blueprint::{InlineText, ParagraphNode, TranslatedText};

        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            "test-redacto-authored".into(),
            OutputTarget::Redacto,
        );

        // Nothing authored yet: the dump tool must say so rather than emit an
        // empty document.
        match agent
            .execute("build_redacto_dump", &serde_json::json!({}))
            .await
        {
            ToolReply::Error(e) => assert_eq!(e, NO_STRUCTURED_TREE),
            _ => panic!("an empty tree must not build a dump"),
        }

        let paragraph = |text: &str| {
            let mut content = TranslatedText::empty();
            content.insert("en", InlineText::plain(text));
            StructuredNode::Paragraph(ParagraphNode {
                content,
                som_path: None,
                source_name: None,
            })
        };
        let nodes: Vec<StructuredNode> = (1..=8)
            .map(|i| paragraph(&format!("Paragraph number {i} of the authored document.")))
            .collect();
        let reply = agent
            .execute(
                "set_structured",
                &serde_json::json!({
                    "nodes": serde_json::to_value(&nodes).unwrap(),
                    "headers": { "en": "UBS Switzerland AG" }
                }),
            )
            .await;
        assert!(reply_text(reply).starts_with("OK"));

        let built = reply_text(
            agent
                .execute("build_redacto_dump", &serde_json::json!({}))
                .await,
        );
        let report: serde_json::Value = serde_json::from_str(&built).unwrap();
        assert_eq!(
            report["problems"].as_array().map(Vec::len),
            Some(0),
            "an authored tree must produce a shippable dump: {built}"
        );
        assert!(
            report["assets"].as_u64().unwrap_or(0) >= 8,
            "every authored paragraph must become an asset: {built}"
        );

        let dump = agent.redacto_dump().expect("the dump must be cached for finalize");
        let sql = dump.to_sql();
        assert!(sql.contains("UBS Switzerland AG"), "the authored header must ship");
        if let Err(e) = u2s_mapper_redacto::decode::decode(sql.as_bytes()) {
            panic!("the u2s Redacto verifier must accept the engine's dump: {e}");
        }
    }

    /// The same tools stay reachable under the AEM target — the guard is about
    /// the target, not about the tools.
    #[test]
    fn aem_tools_are_reachable_under_the_aem_target() {
        let agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-aem-guard".into(),
            OutputTarget::Aem,
        );

        assert_eq!(agent.target(), OutputTarget::Aem);
        for tool in tools_only_for(target::AEM) {
            assert!(
                agent.target_refusal(tool).is_none(),
                "{tool} must be available under the AEM target"
            );
        }
    }

    #[test]
    fn source_key_defaults_to_current() {
        assert_eq!(
            ConversionAgent::source_key(&serde_json::json!({})),
            "current"
        );
        assert_eq!(
            ConversionAgent::source_key(&serde_json::json!({"source": {"reference": "abc"}})),
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

    #[test]
    fn config_reflects_languages_in_seeded_structured_tree() {
        use blueprint::{InlineText, ParagraphNode, StructuredNode, TranslatedText};

        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            "test-config-languages".into(),
            OutputTarget::Aem,
        );
        // The ubs profile templates reference a couple of xfa vars; supply the
        // minimal context so load_aem_config succeeds without a real PDF.
        let mut vars = HashMap::new();
        vars.insert("formrange_code".to_string(), "TESTFORM".to_string());
        vars.insert("formrange_entity".to_string(), "TEST".to_string());
        agent.context = blueprint::Context::new("en".to_string(), vars);

        // With no content the config falls back to the profile default.
        let before = agent.config().expect("config loads for ubs profile");
        assert_eq!(before.languages, vec!["en".to_string()]);

        // Seed a bilingual (de + en) working tree.
        let mut content = TranslatedText::empty();
        content.insert("en", InlineText::plain("Hello"));
        content.insert("de", InlineText::plain("Hallo"));
        agent.seed_structured(vec![StructuredNode::Paragraph(ParagraphNode {
            content,
            som_path: None,
            source_name: None,
        })]);

        // config() must now reflect the languages present in the tree so
        // get_profile_info and the package builder treat the form as
        // multilingual instead of collapsing it to the en-only default.
        let after = agent.config().expect("config loads");
        assert!(after.languages.contains(&"en".to_string()));
        assert!(
            after.languages.contains(&"de".to_string()),
            "config.languages must include every language in the seeded tree, got {:?}",
            after.languages
        );
    }

    /// Before any tree exists, the AEM configuration takes its languages from
    /// the source PDFs themselves.
    #[test]
    fn config_takes_the_source_languages_before_a_tree_exists() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAAL_019_SP.pdf"), fixture("AAAL_019_EN.pdf")],
            String::new(),
            OutputTarget::Aem,
        );
        let config = agent.config().expect("config loads for the ubs profile");
        assert_eq!(config.languages, vec!["en".to_string(), "es".to_string()]);
    }

    /// The source documents `get_source_info` lists, parsed.
    async fn source_documents_of(agent: &mut ConversionAgent) -> Vec<serde_json::Value> {
        let info = reply_text(agent.execute("get_source_info", &serde_json::json!({})).await);
        let info: serde_json::Value = serde_json::from_str(&info).unwrap();
        info["documents"].as_array().cloned().unwrap_or_default()
    }

    #[tokio::test]
    async fn get_source_info_hands_out_the_doc_path_the_xfa_tools_read() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAOS_033_IT.pdf")],
            "test-u2s-read".into(),
            OutputTarget::Aem,
        );
        let documents = source_documents_of(&mut agent).await;
        assert_eq!(documents.len(), 1, "{documents:?}");
        assert_eq!(documents[0]["language"], "it");
        let doc_path = documents[0]["doc_path"].as_str().unwrap().to_string();

        let packets = reply_text(
            agent
                .execute("xfa_packets", &serde_json::json!({ "doc_path": doc_path }))
                .await,
        );
        assert!(packets.contains("\"template\""), "{packets}");

        let read = reply_text(
            agent
                .execute(
                    "xfa_read",
                    &serde_json::json!({ "doc_path": doc_path, "packet": "template", "limit": 200 }),
                )
                .await,
        );
        assert!(read.contains("<template"), "{read}");
    }

    #[tokio::test]
    async fn a_doc_path_outside_the_runs_documents_is_refused() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAOS_033_IT.pdf")],
            "test-u2s-refuse".into(),
            OutputTarget::Aem,
        );
        let outside = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../core/input/AAEV_019_EN.pdf")
            .display()
            .to_string();
        for tool in ["xfa_packets", "xfa_info", "pdf_info"] {
            match agent
                .execute(tool, &serde_json::json!({ "doc_path": outside }))
                .await
            {
                ToolReply::Error(e) => assert!(e.contains("not one of this run's documents"), "{e}"),
                other => panic!("{tool} read a file outside the run: {:?}", reply_kind(&other)),
            }
        }
    }

    #[tokio::test]
    async fn xfa_render_page_replies_with_the_page_image() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAOS_033_IT.pdf")],
            "test-u2s-render".into(),
            OutputTarget::Redacto,
        );
        let doc_path = source_documents_of(&mut agent).await[0]["doc_path"]
            .as_str()
            .unwrap()
            .to_string();
        let reply = agent
            .execute(
                "xfa_render_page",
                &serde_json::json!({ "doc_path": doc_path, "page": 1, "dpi": 72 }),
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

    /// Settings that pass validation but point at no real image: enough to
    /// attach the verifier, whose offline checks never touch Docker.
    fn offline_aem_verify() -> crate::u2s::AemVerifySettings {
        crate::u2s::AemVerifySettings {
            image: "blueprint-test/aem:unused".into(),
            ..Default::default()
        }
    }

    /// The verifier checks the run's own latest build, whatever path the model
    /// passes, and says what to build when there is nothing yet.
    #[tokio::test]
    async fn aem_verify_package_check_checks_the_runs_own_build() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAOS_033_IT.pdf")],
            "test-verify-package".into(),
            OutputTarget::Aem,
        )
        .with_aem_verify(&offline_aem_verify())
        .expect("complete settings attach the verifier");

        match agent
            .execute("aem_verify_package_check", &serde_json::json!({}))
            .await
        {
            ToolReply::Error(e) => assert!(e.contains("build_aem_package"), "{e}"),
            other => panic!("nothing is built yet: {}", reply_kind(&other)),
        }

        let set = agent
            .execute("set_aem_translated", &serde_json::json!({"root": small_aem_tree()}))
            .await;
        assert!(!matches!(set, ToolReply::Error(_)), "{}", reply_kind(&set));
        let built = agent.execute("build_aem_package", &serde_json::json!({})).await;
        assert!(!matches!(built, ToolReply::Error(_)), "{}", reply_kind(&built));

        let checked = reply_text(
            agent
                .execute(
                    "aem_verify_package_check",
                    &serde_json::json!({ "package_path": "/nonexistent/other.zip" }),
                )
                .await,
        );
        assert!(checked.contains("form_jcr_path"), "{checked}");
        assert!(checked.contains("/content/forms/af/"), "{checked}");
    }

    /// A verifier tool without a verifier attached says so rather than failing
    /// obscurely, and the other target's verifier tools are refused outright.
    #[tokio::test]
    async fn a_verifier_tool_needs_its_verifier() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            String::new(),
            OutputTarget::Aem,
        );
        agent.seed_package(vec![0u8; 4]);
        match agent.execute("aem_verify_package_check", &serde_json::json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("verifier was not started"), "{e}"),
            other => panic!("{}", reply_kind(&other)),
        }
        match agent.execute("redacto_verify_status", &serde_json::json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("not available for the"), "{e}"),
            other => panic!("{}", reply_kind(&other)),
        }
    }

    /// The Redacto verifier decodes the run's own dump, offline.
    #[tokio::test]
    async fn redacto_verify_dump_check_checks_the_runs_own_dump() {
        use blueprint::{InlineText, ParagraphNode, TranslatedText};

        let settings = crate::u2s::RedactoVerifySettings::default();
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            "test-verify-dump".into(),
            OutputTarget::Redacto,
        )
        .with_redacto_verify(&settings)
        .expect("the Redacto verifier attaches without Docker");

        let mut content = TranslatedText::empty();
        content.insert("en", InlineText::plain("A paragraph the dump must carry."));
        let nodes = vec![StructuredNode::Paragraph(ParagraphNode {
            content,
            som_path: None,
            source_name: None,
        })];
        let set = agent
            .execute("set_structured", &serde_json::json!({ "nodes": serde_json::to_value(&nodes).unwrap() }))
            .await;
        assert!(reply_text(set).starts_with("OK"));
        let built = agent.execute("build_redacto_dump", &serde_json::json!({})).await;
        assert!(!matches!(built, ToolReply::Error(_)), "{}", reply_kind(&built));

        let checked = reply_text(
            agent
                .execute("redacto_verify_dump_check", &serde_json::json!({}))
                .await,
        );
        assert!(checked.contains("\"ok\":true") || checked.contains("\"ok\": true"), "{checked}");
    }

    fn reply_kind(reply: &ToolReply) -> String {
        match reply {
            ToolReply::Text(t) => format!("text: {t}"),
            ToolReply::Error(e) => format!("error: {e}"),
            ToolReply::Blocks(b) => format!("{} blocks", b.len()),
        }
    }

}

mod catalog;
mod execute;
mod prompts;

use catalog::target_mask;
pub use catalog::{ToolSpec, all_tools, catalog, scope, target, tools_for};
pub use prompts::*;
