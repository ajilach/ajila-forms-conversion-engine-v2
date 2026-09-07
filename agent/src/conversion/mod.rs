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

use std::collections::HashMap;

use blueprint::{
    AemConfig, AemConnection, AemI18nText, AemNode, AemNodeTranslated, AemOptionTranslated,
    Context, DocumentEnvelope, OutputTarget, RedactoDump, StructuredNode,
};

/// Error returned by the AEM-tree tools when nothing has been authored yet.
const NO_AEM_TREE: &str = "No AEM tree yet; author it with set_aem_translated.";

/// Error returned by the package tools before [`ConversionAgent::package`] is
/// populated. Public so the MCP server's `write_package` reports the same thing.
pub const NO_PACKAGE: &str = "No package built yet; call build_aem_package.";

/// Error returned by the structured-tree tools when nothing has been authored
/// yet.
const NO_STRUCTURED_TREE: &str =
    "No structured tree yet; seed one with seed_structured_from_state.";

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

/// Render scale for on-demand page images.
const RENDER_SCALE: f32 = 1.5;

/// The result of executing one tool call, to be returned to the model as a
/// `tool_result` content block.
#[derive(Debug)]
pub enum ToolReply {
    /// A textual result (JSON, plain text, …).
    Text(String),
    /// One or more images (base64), all sharing one media type — e.g. the pages
    /// of a rendered form. Emitted as multiple image blocks in one `tool_result`.
    Image {
        media_type: &'static str,
        images: Vec<String>,
    },
    /// Text and images interleaved, in order: what an external MCP tool (the
    /// browser) returns: a page snapshot next to a screenshot, a download notice
    /// next to nothing. Emitted as one block per entry in a single `tool_result`.
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

/// Settings key under which the desktop app persists its serialized settings
/// blob in the shared `history.db` (see `app`'s `AppSettings`).
const APP_SETTINGS_KEY: &str = "app";

/// Build an AEM connection from the app settings stored in the shared
/// `history.db`, so a conversion driven headlessly (e.g. over MCP) can
/// upload/verify against the same instance the desktop app is configured for.
///
/// Reads the `aem_host` / `aem_username` / `aem_password` fields out of the
/// settings blob — mirroring `AppSettings::aem_connection` — and returns `None`
/// when no settings are stored or host/username are blank.
pub fn aem_connection_from_settings() -> Option<AemConnection> {
    let json = crate::db::get_setting(APP_SETTINGS_KEY)?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    let host = v
        .get("aem_host")
        .and_then(|h| h.as_str())
        .unwrap_or_default()
        .trim();
    let username = v
        .get("aem_username")
        .and_then(|u| u.as_str())
        .unwrap_or_default()
        .trim();
    if host.is_empty() || username.is_empty() {
        return None;
    }
    let password = v
        .get("aem_password")
        .and_then(|p| p.as_str())
        .unwrap_or_default();
    Some(AemConnection {
        host: host.trim_end_matches('/').to_string(),
        username: username.to_string(),
        password: password.to_string(),
    })
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

struct StateRec {
    label: String,
    pdf_name: String,
    selections: usize,
    state: blueprint::FormState,
    context: Context,
}

/// The engine's view of one input source (the uploaded form, or a reference):
/// discovered states (for listing / rendering / per-state structure), the XFA,
/// and the merged structured tree.
struct Extractor {
    states: Vec<StateRec>,
    xfa: Vec<(String, String)>,
    /// The merged multilingual envelope — content *and* context. The context is
    /// the only carrier of [`Context::header`], which `Blueprint::context()`
    /// never sets (only `merged_structured()` does), so consumers that want the
    /// recovered master-page header must read it from here.
    merged: DocumentEnvelope,
    /// Why the cross-language merge failed, if it did (`merged.content` is empty
    /// in that case). Swallowing this is what produced silently empty Redacto
    /// dumps: a document whose language variants are too dissimilar to merge
    /// looked exactly like a document with no content.
    merge_error: Option<String>,
    /// One context per source PDF, in upload order.
    ///
    /// The language variants do not share a context: each carries its own
    /// master-page header and its own `Footer_Line_*` XFA variables. An output
    /// whose configuration is single-valued (as Redacto's header and footer are)
    /// must therefore pick one deliberately — by master language, not by upload
    /// order. See [`ConversionAgent::source_context`].
    contexts: Vec<Context>,
}

impl Extractor {
    /// `semantic` is the sentence-embedding matcher used to align nodes across
    /// languages when merging the per-PDF (per-language) trees into one
    /// bilingual tree. Without it `merge_translations` can only align nodes
    /// structurally, which duplicates whole sections (one per language, with
    /// colliding field names) whenever the language variants don't line up
    /// node-for-node — so always pass it for a multi-language source.
    fn build(
        pdfs: &[(String, Vec<u8>)],
        semantic: Option<&blueprint::semantic::SemanticMatcher>,
    ) -> Self {
        let multi = pdfs.len() > 1;
        let mut states = Vec::new();
        let mut xfa = Vec::new();
        let mut envelopes: Vec<DocumentEnvelope> = Vec::new();

        for (name, bytes) in pdfs {
            if let Ok(Some(x)) = blueprint::extract_xfa_from_pdf_bytes(bytes) {
                xfa.push((name.clone(), String::from_utf8_lossy(&x).into_owned()));
            }
            if let Ok(mut bp) = blueprint::Blueprint::from_pdf_bytes(bytes) {
                let context = bp.context();
                if let Ok(fs) = bp.states() {
                    for s in fs.iter() {
                        let label = if multi {
                            format!("{name}::{}", s.label)
                        } else {
                            s.label.clone()
                        };
                        let selections = s.selections.len();
                        states.push(StateRec {
                            label,
                            pdf_name: name.clone(),
                            selections,
                            state: s,
                            context: context.clone(),
                        });
                    }
                }
            }
            // Merged structured needs its own Blueprint (states()/merged both &mut).
            if let Ok(mut bp2) = blueprint::Blueprint::from_pdf_bytes(bytes)
                && let Ok(env) = bp2.merged_structured()
            {
                envelopes.push(env);
            }
        }

        // Each single-PDF `merged_structured()` succeeds even when the
        // cross-language merge does not, so keep the first envelope's context as
        // the base: it carries the recovered master-page header, which would
        // otherwise be lost exactly when the merge fails.
        let base_context = envelopes
            .first()
            .map(|e| e.context.clone())
            .unwrap_or_else(|| Context::with_language("en"));
        let empty = |context: Context| DocumentEnvelope {
            context,
            content: Vec::new(),
            state_count: 1,
        };

        let contexts: Vec<Context> = envelopes.iter().map(|e| e.context.clone()).collect();

        let (merged, merge_error) = match envelopes.len() {
            0 => (empty(base_context), None),
            1 => (envelopes.into_iter().next().unwrap(), None),
            _ => match blueprint::merge_translations(envelopes, semantic) {
                Ok(env) => (env, None),
                Err(e) => (empty(base_context), Some(e.to_string())),
            },
        };

        Extractor {
            states,
            xfa,
            merged,
            merge_error,
            contexts,
        }
    }

    fn find(&self, label: &str) -> Option<&StateRec> {
        self.states.iter().find(|s| s.label == label)
    }

    /// The clean, single-language structured tree for one state (one language ×
    /// one configurator selection). Unlike the merged tree this carries no
    /// cross-language/cross-state merge artifacts (no duplicated sections,
    /// colliding field names or mispaired translations) — it's the engine's
    /// faithful read of exactly one rendered variant, suitable as a base to
    /// assemble the working tree from.
    fn state_structured(&self, label: &str) -> Result<Vec<StructuredNode>, String> {
        let rec = self
            .find(label)
            .ok_or_else(|| format!("No state with label '{label}'. Use list_states."))?;
        Ok(rec.state.structured(rec.context.clone()).content)
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
    /// Set once the package has been uploaded + installed on AEM.
    uploaded: bool,
    /// JCR path of the uploaded form on AEM (for the "done" screen).
    form_path: Option<String>,
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
    conn: Option<AemConnection>,
    current_pdfs: Vec<(String, Vec<u8>)>,
    extractors: HashMap<String, Extractor>,

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

    /// Scale for on-demand page renders; see [`ConversionAgent::with_render_scale`].
    render_scale: f32,

    /// The browser the preflight started, when browser verification is on.
    /// Offers its tools to the Author and Reviewer (see `catalog::BROWSER_SCOPES`).
    browser: Option<crate::browser::BrowserSession>,
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
        conn: Option<AemConnection>,
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
            conn,
            current_pdfs: pdfs,
            extractors: HashMap::new(),
            structured: Vec::new(),
            target: target_state,
            structured_session,
            matcher: None,
            render_scale: RENDER_SCALE,
            review: None,
            browser: None,
        };
        // Record the pre-loaded template as the initial AEM edit so it shows in
        // the AEM edit history (no-op when no template was uploaded).
        if agent.aem_tree().is_some() {
            agent.aem_translated_edited("Template (from uploaded package)");
        }
        agent
    }

    /// Where an AEM run will install this form, before any work is done.
    ///
    /// `None` when the run targets no AEM instance, or when the profile has no
    /// AEM config to derive a path from. Deliberately reads the *base* config
    /// rather than [`Self::config`]: only `form_path` and `form_dir` matter
    /// here, and those do not depend on which languages the document turns out
    /// to carry — so the path is knowable up front, which is what lets a
    /// colliding run be refused before it spends a token.
    pub fn planned_jcr_path(&self) -> Option<String> {
        if self.target.target() != OutputTarget::Aem {
            return None;
        }
        let profile = self.profile.as_deref()?;
        let cfg = blueprint::load_aem_config(profile, &self.context).ok()?;
        Some(form_jcr_path(&cfg))
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

    /// Override the scale on-demand page images are rendered at.
    ///
    /// Vision tokens scale with pixel area, so a read-only pass that only has to
    /// *read* a form can halve its image cost. Set at construction rather than
    /// exposed as a tool argument: the model should not be able to spend more by
    /// choosing.
    pub fn with_render_scale(mut self, scale: f32) -> Self {
        self.render_scale = scale;
        self
    }

    /// Attach the browser session the preflight started. From then on the
    /// stages in `catalog::BROWSER_SCOPES` are offered its tools and
    /// `browser_*` calls are forwarded to it.
    pub fn with_browser(mut self, session: crate::browser::BrowserSession) -> Self {
        self.browser = Some(session);
        self
    }

    /// Whether a browser session is attached to this run.
    pub fn has_browser(&self) -> bool {
        self.browser.is_some()
    }

    /// The tools a stage is offered: the catalog's, plus the browser's for the
    /// stages `catalog::BROWSER_SCOPES` names when a session is attached. This
    /// is what the controller hands the model; `tools_for` alone is the
    /// catalog-only view the MCP server and the tests use.
    pub fn tools_for_stage(&self, scopes: scope::Mask) -> Vec<serde_json::Value> {
        let mut tools = tools_for(self.target(), scopes);
        if let Some(browser) = &self.browser
            && self.target() == OutputTarget::Aem
            && scopes & catalog::BROWSER_SCOPES != 0
        {
            tools.extend(browser.tools().iter().cloned());
        }
        tools
    }

    /// Notes the browser session accumulated since the last call (restarts),
    /// for the run's observer.
    pub fn take_warnings(&mut self) -> Vec<String> {
        self.browser
            .as_mut()
            .map(|b| b.take_warnings())
            .unwrap_or_default()
    }

    /// Close the browser, if one is attached. Called by the controller on every
    /// way out of a run, so no headless Chrome outlives it.
    pub async fn shutdown_browser(&mut self) {
        if let Some(browser) = self.browser.take() {
            browser.shutdown().await;
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
    /// Empty on a fresh run: the agent authors the AEM tree directly and only
    /// seeds this when resuming a session. Use
    /// [`source_structured`](Self::source_structured) for the converted source
    /// document.
    pub fn structured(&self) -> &[StructuredNode] {
        &self.structured
    }

    /// The merged structured tree of the current source PDFs — the plain
    /// conversion of the document, identical to what the CLI produces.
    ///
    /// Builds and caches the extractor if the run has not needed it yet, so
    /// this is free once the agent has read the source (the usual case) and a
    /// full conversion otherwise. Returns an empty slice if extraction fails.
    pub fn source_structured(&mut self) -> &[StructuredNode] {
        match self.extractor(&serde_json::json!({})) {
            Ok(extractor) => &extractor.merged.content,
            Err(_) => &[],
        }
    }

    /// The merged source [`DocumentEnvelope`] — [`source_structured`](Self::source_structured)
    /// plus the context it was extracted with.
    ///
    /// Prefer this over pairing `source_structured()` with
    /// [`context`](Self::context) when building an output: only this context
    /// carries [`Context::header`], the master-page header the analysis
    /// recovers. `ConversionAgent::context` is taken from `Blueprint::context()`
    /// before any analysis has run and always has `header: None`.
    pub fn source_envelope(&mut self) -> DocumentEnvelope {
        match self.extractor(&serde_json::json!({})) {
            Ok(extractor) => extractor.merged.clone(),
            Err(_) => DocumentEnvelope {
                context: self.context.clone(),
                content: Vec::new(),
                state_count: 1,
            },
        }
    }

    /// The source context to resolve an output configuration against, preferring
    /// the variant written in `master_language`.
    ///
    /// Each language variant of a document carries its own master-page header
    /// and its own `Footer_Line_*` XFA variables, so a single-valued
    /// configuration (Redacto's `header`/`footer`) takes whichever variant it is
    /// pointed at. Defaulting to upload order made that arbitrary — a document
    /// uploaded SP-first got a Spanish header on an English-master document.
    pub fn source_context(&mut self, master_language: &str) -> Context {
        match self.extractor(&serde_json::json!({})) {
            Ok(ex) => ex
                .contexts
                .iter()
                .find(|c| c.language() == master_language)
                .or_else(|| ex.contexts.first())
                .cloned()
                .unwrap_or_else(|| ex.merged.context.clone()),
            Err(_) => self.context.clone(),
        }
    }

    /// Why the source's cross-language merge failed, if it did.
    ///
    /// A `Some` here means [`source_structured`](Self::source_structured) is
    /// empty for a reason worth reporting rather than because the document has
    /// no content.
    pub fn source_merge_error(&mut self) -> Option<String> {
        match self.extractor(&serde_json::json!({})) {
            Ok(extractor) => extractor.merge_error.clone(),
            Err(_) => None,
        }
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

    /// Whether the package has been uploaded + installed on AEM.
    pub fn aem_uploaded(&self) -> bool {
        self.target.aem().is_some_and(|s| s.uploaded)
    }

    /// The JCR path of the uploaded form, once uploaded.
    pub fn aem_form_path(&self) -> Option<String> {
        self.target.aem().and_then(|s| s.form_path.clone())
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn source_key(input: &serde_json::Value) -> String {
        match input["source"]["reference"].as_str() {
            Some(id) => format!("reference:{id}"),
            None => "current".to_string(),
        }
    }

    /// Get (building+caching if needed) the extractor for the requested source.
    fn extractor(&mut self, input: &serde_json::Value) -> Result<&Extractor, String> {
        let key = Self::source_key(input);
        if !self.extractors.contains_key(&key) {
            let pdfs = match input["source"]["reference"].as_str() {
                Some(id) => {
                    let bytes = crate::references::get_reference_pdf_bytes(id, 0)?;
                    vec![(format!("{id}.pdf"), bytes)]
                }
                None => self.current_pdfs.clone(),
            };
            // A multi-language source must be merged with the semantic matcher
            // (see Extractor::build). Load it best-effort; if it can't load we
            // fall back to None and the structural merge. Single-PDF sources
            // need no cross-language merge, so don't pay the load cost.
            let ex = if pdfs.len() > 1 {
                let _ = self.matcher();
                Extractor::build(&pdfs, self.matcher.as_ref())
            } else {
                Extractor::build(&pdfs, None)
            };
            self.extractors.insert(key.clone(), ex);
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
        } else if let Ok(ex) = self.extractor(&serde_json::Value::Null) {
            blueprint::resolve_aem_languages(&ex.merged.content, &cfg)
        } else {
            cfg
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

    /// Build the Redacto dump for the working structured tree, and cache it.
    ///
    /// The contexts come from the source extraction rather than
    /// [`context`](Self::context) so the recovered master-page header reaches
    /// the profile's `page.header`. All language variants are passed: the page
    /// header and footer are rendered per language, and core picks the
    /// master-language variant for the document's identity.
    fn build_redacto(&mut self) -> Result<(RedactoDump, blueprint::RedactoConfig), String> {
        let profile = self
            .profile
            .clone()
            .ok_or("No profile selected — the Redacto dump needs a profile.")?;
        let contexts = match self.extractor(&serde_json::json!({})) {
            Ok(extractor) if !extractor.contexts.is_empty() => extractor.contexts.clone(),
            _ => vec![self.context.clone()],
        };
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

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::prelude::BASE64_STANDARD.encode(bytes)
}

fn dedup(mut v: Vec<&str>) -> Vec<String> {
    v.sort();
    v.dedup();
    v.into_iter().map(String::from).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

fn line_matches(line: &str, query: &str, regex: bool) -> bool {
    if regex {
        regex_lite::Regex::new(query)
            .map(|re| re.is_match(line))
            .unwrap_or(false)
    } else {
        line.to_lowercase().contains(&query.to_lowercase())
    }
}

/// Concatenate matching lines from every `(name, text)` source, up to
/// `total_cap` characters in total.
///
/// The cap is on the *total*, checked after every line, and the whole search
/// stops the moment it is crossed — one source cannot re-open the budget for
/// the next. A per-source break instead of a per-total one does not multiply
/// the cap by the source count (the accumulator is shared either way), but it
/// still lets every later source append one more line past the cap before its
/// own check re-fires — one extra, unbounded-length line per remaining
/// source, silently, every time. Breaking the whole search instead is what
/// makes the cap a real ceiling rather than "usually close to it."
///
/// Stopping early can leave whole sources unsearched, so the reply says so
/// rather than reading like a complete result: a query for a language-
/// specific phrase that matches heavily in one PDF must not come back
/// claiming the others hold nothing, when they were never actually checked.
fn search_matching_lines(
    sources: &[(String, String)],
    query: &str,
    regex: bool,
    total_cap: usize,
) -> String {
    let mut out = String::new();
    let mut stopped_at = None;
    'sources: for (idx, (n, x)) in sources.iter().enumerate() {
        for line in x.lines().filter(|l| line_matches(l, query, regex)) {
            out.push_str(&format!("{n}: {}\n", line.trim()));
            if out.len() > total_cap {
                stopped_at = Some(idx);
                break 'sources;
            }
        }
    }
    if let Some(idx) = stopped_at {
        let unsearched: Vec<&str> = sources[idx..].iter().map(|(n, _)| n.as_str()).collect();
        out.push_str(&format!(
            "\n[stopped after {total_cap} characters — matches past this point, including any \
             remaining in {}, were not searched; narrow the query or read that source directly]",
            unsearched.join(", ")
        ));
    }
    out
}

/// The form's JCR node path from its AEM config.
fn form_jcr_path(cfg: &AemConfig) -> String {
    join_form_path(&cfg.form_path, &cfg.form_dir)
}

fn join_form_path(form_path: &str, form_dir: &str) -> String {
    format!(
        "/content/forms/af/{}/{}",
        form_path.trim_matches('/'),
        form_dir.trim_matches('/')
    )
}

/// The preview a reviewer opens in a browser: the DAM rendition of the form,
/// outside the editor, in one language. This is what humans use; the form's
/// own `.html` render under `/content/forms/af` tends to 401 outside the
/// editor.
fn form_preview_url(host: &str, cfg: &AemConfig, lang: &str) -> String {
    // The mandator (entity code, e.g. 033 for Italy, 019 for Germany) is not
    // decoration: the UBS runtime reads it for reference data and the
    // submit/DoR backend, so a preview without it renders but cannot submit.
    let mandator = cfg
        .mandator
        .as_deref()
        .map(|m| format!("&mandator={m}"))
        .unwrap_or_default();
    format!(
        "{}/content/dam/formsanddocuments/{}/{}/jcr:content?wcmmode=disabled&afAcceptLang={lang}{mandator}",
        host.trim_end_matches('/'),
        cfg.form_path.trim_matches('/'),
        cfg.form_dir.trim_matches('/')
    )
}

/// The form opened in the AEM Forms editor.
fn form_editor_url(host: &str, cfg: &AemConfig) -> String {
    format!(
        "{}/editor.html{}.html",
        host.trim_end_matches('/'),
        form_jcr_path(cfg)
    )
}

/// The URLs of a deployed form, as the `aem_form_urls` tool and the
/// `upload_to_aem` result report them: the JCR path, one preview per language
/// (master first) and the editor.
fn form_urls_text(host: &str, cfg: &AemConfig) -> String {
    let mut langs: Vec<&str> = vec![cfg.master_language.as_str()];
    for l in &cfg.languages {
        if !langs.contains(&l.as_str()) {
            langs.push(l);
        }
    }
    let mut out = format!("jcr_path: {}\n", form_jcr_path(cfg));
    for lang in langs {
        out.push_str(&format!(
            "preview ({lang}): {}\n",
            form_preview_url(host, cfg, lang)
        ));
    }
    out.push_str(&format!("editor: {}", form_editor_url(host, cfg)));
    out
}

/// Render the DoR PDF to one base64 JPEG per page via the engine.
fn render_pdf_pages(pdf: &[u8]) -> Result<Vec<String>, String> {
    let mut bp =
        blueprint::Blueprint::from_pdf_bytes(pdf).map_err(|e| format!("PDF parse: {e}"))?;
    let states = bp.states().map_err(|e| format!("states: {e}"))?;
    let state = states.iter().next().ok_or("no state in DoR PDF")?;
    let pages = state
        .render_plain_pages(RENDER_SCALE)
        .map_err(|e| format!("render: {e}"))?;
    pages
        .iter()
        .map(|img| {
            crate::image_encode::encode_rgba_to_jpeg(img, 82)
                .map(|jpeg| base64_encode(&jpeg))
                .map_err(|e| format!("encode: {e}"))
        })
        .collect()
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

    /// A fresh agent authors the AEM tree directly and never fills
    /// `structured`, so anything deriving output from the source document must
    /// go through `source_structured` instead. Regression guard: exporting from
    /// `structured()` silently produced an empty document.
    #[test]
    fn source_structured_holds_the_converted_document_while_structured_is_empty() {
        let pdf =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/input/AAEV_019_EN.pdf");
        let bytes = std::fs::read(&pdf).expect("read AAEV_019_EN.pdf");

        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![("AAEV_019_EN.pdf".to_string(), bytes)],
            None,
            "test-source-structured".into(),
            OutputTarget::Aem,
        );

        assert!(
            agent.structured().is_empty(),
            "a fresh agent has no working structured tree"
        );
        assert!(
            !agent.source_structured().is_empty(),
            "the converted source document must be reachable for non-AEM exports"
        );
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
            None,
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
            None,
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

    fn aem_connection() -> AemConnection {
        AemConnection {
            host: "http://localhost:4502".into(),
            username: "admin".into(),
            password: "admin".into(),
        }
    }

    /// The URLs a reviewer opens are derived from the profile's paths: the DAM
    /// preview per language (master first) and the editor. Before an upload
    /// the tool refuses rather than pointing at a form that is not there.
    #[tokio::test]
    async fn aem_form_urls_follow_the_profile_and_need_an_upload() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            Some(aem_connection()),
            "test-form-urls".into(),
            OutputTarget::Aem,
        );
        match agent.execute("aem_form_urls", &serde_json::json!({})).await {
            ToolReply::Error(e) => assert!(e.contains("upload_to_aem"), "{e}"),
            other => panic!("expected an error before upload, got {other:?}"),
        }

        let cfg = agent.config().expect("ubs config");
        let text = form_urls_text("http://localhost:4502/", &cfg);
        let jcr = form_jcr_path(&cfg);
        assert!(text.contains(&format!("jcr_path: {jcr}")), "{text}");
        assert_eq!(
            cfg.mandator.as_deref(),
            Some("019"),
            "AAEV_019 is a Germany form"
        );
        assert!(
            text.contains(&format!(
                "preview ({}): http://localhost:4502/content/dam/formsanddocuments/{}/{}/jcr:content?wcmmode=disabled&afAcceptLang={}&mandator=019",
                cfg.master_language,
                cfg.form_path.trim_matches('/'),
                cfg.form_dir.trim_matches('/'),
                cfg.master_language
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "editor: http://localhost:4502/editor.html{jcr}.html"
            )),
            "{text}"
        );
        // Every source language gets a preview, each once.
        for lang in &cfg.languages {
            assert_eq!(
                text.matches(&format!("preview ({lang}):")).count(),
                1,
                "{text}"
            );
        }
    }

    /// `inspect_pdf` lists the browser's downloads and renders one of them, and
    /// never reaches outside the browser's output directory.
    #[tokio::test]
    async fn inspect_pdf_lists_and_renders_only_the_browser_downloads() {
        // Rendering needs the profile's fonts, which `pipeline::run` loads at
        // the start of a run.
        let _ = blueprint::load_profile_fonts("ubs");
        let dir = std::env::temp_dir().join(format!("blueprint-inspect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (name, bytes) = fixture("AAEV_019_EN.pdf");
        std::fs::write(dir.join("submission.pdf"), &bytes).unwrap();
        std::fs::write(dir.join("notes.txt"), b"not a pdf").unwrap();

        let mut without = ConversionAgent::new(
            None,
            vec![(name.clone(), bytes.clone())],
            None,
            "t".into(),
            OutputTarget::Aem,
        );
        assert!(matches!(
            without.execute("inspect_pdf", &serde_json::json!({})).await,
            ToolReply::Error(e) if e.contains("No browser session")
        ));

        let mut agent = ConversionAgent::new(
            None,
            vec![(name, bytes)],
            None,
            "t".into(),
            OutputTarget::Aem,
        )
        .with_browser(crate::browser::BrowserSession::detached(
            Vec::new(),
            dir.clone(),
        ));

        match agent.execute("inspect_pdf", &serde_json::json!({})).await {
            ToolReply::Text(listing) => {
                assert!(
                    listing.contains("submission.pdf") && listing.contains("notes.txt"),
                    "{listing}"
                );
            }
            other => panic!("expected a listing, got {other:?}"),
        }
        match agent
            .execute(
                "inspect_pdf",
                &serde_json::json!({"path": "submission.pdf"}),
            )
            .await
        {
            ToolReply::Image { media_type, images } => {
                assert_eq!(media_type, "image/jpeg");
                assert!(!images.is_empty(), "at least one rendered page");
            }
            other => panic!("expected page images, got {other:?}"),
        }
        assert!(matches!(
            agent.execute("inspect_pdf", &serde_json::json!({"path": "notes.txt"})).await,
            ToolReply::Error(e) if e.contains("not a PDF")
        ));
        let outside =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/input/AAEV_019_EN.pdf");
        assert!(matches!(
            agent.execute("inspect_pdf", &serde_json::json!({"path": outside.display().to_string()})).await,
            ToolReply::Error(e) if e.contains("outside")
        ));
        assert!(matches!(
            agent
                .execute("inspect_pdf", &serde_json::json!({"path": "../"}))
                .await,
            ToolReply::Error(_)
        ));
        agent.shutdown_browser().await;
        assert!(!dir.exists(), "shutdown removes the output directory");
    }

    /// A single-PDF source keeps the whole merged envelope, not just its
    /// content. Regression guard for the master-page header: `Context::header`
    /// is set only by `merged_structured()`, so an output built from
    /// `agent.context()` (which is `Blueprint::context()`, always
    /// `header: None`) silently loses it.
    #[test]
    fn source_envelope_carries_the_recovered_header() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            None,
            "test-source-envelope".into(),
            OutputTarget::Aem,
        );

        assert!(
            agent.context().header.is_none(),
            "the agent's own context is taken before any analysis has run"
        );
        assert!(
            agent.source_envelope().context.header.is_some(),
            "the merged envelope must carry the header the analysis recovered"
        );
        assert!(
            agent.source_merge_error().is_none(),
            "one PDF needs no merge"
        );
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
            None,
            "test-master-context".into(),
            OutputTarget::Redacto,
        );

        assert_eq!(agent.source_context("en").language(), "en");
        assert_eq!(agent.source_context("es").language(), "es");
        // An unknown language falls back to the first variant rather than
        // failing — better an arbitrary header than none.
        assert_eq!(agent.source_context("fr").language(), "es");
    }

    /// Regression: `Extractor::build` used to swallow a cross-language merge
    /// failure with `unwrap_or_default()`, leaving an empty merged tree that was
    /// indistinguishable from a document with no content. Every output derived
    /// from it — the Redacto dump in particular — then came out silently empty.
    #[test]
    fn extractor_merge_failure_is_reported_not_swallowed() {
        // Two unrelated forms: far below the structural-similarity threshold
        // `merge_translations` requires of language variants of one document.
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAAA_019_DE.pdf"), fixture("AABH_019_EN.pdf")],
            None,
            "test-merge-error".into(),
            OutputTarget::Aem,
        );

        let reason = agent
            .source_merge_error()
            .expect("a failed merge must be reported");
        assert!(
            reason.to_lowercase().contains("similar"),
            "the reason must name the structural-similarity check, got: {reason}"
        );
        assert!(
            agent.source_structured().is_empty(),
            "a failed merge yields no content — which is precisely why it must be reported"
        );
    }

    /// The app never offers an out-of-scope tool to a role, but MCP serves the
    /// flat catalog, so the target split has to refuse them itself — and say why
    /// rather than reporting a missing tree.
    #[test]
    fn aem_tools_are_refused_under_the_redacto_target() {
        let agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            None,
            "test-redacto-guard".into(),
            OutputTarget::Redacto,
        );

        assert_eq!(agent.target(), OutputTarget::Redacto);
        assert!(agent.aem_translated().is_none());
        assert!(agent.package().is_none());
        assert!(!agent.aem_uploaded());
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
            None,
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
                None,
                format!("test-shared-{}", target.as_str()),
                target,
            );
            for tool in [
                "set_structured",
                "get_structured_outline",
                "get_structured_node",
                "set_structured_field",
                "seed_structured_from_state",
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
            None,
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
            ToolReply::Image { .. } | ToolReply::Blocks(_) => panic!("unexpected image reply"),
        }
    }

    /// The whole point of the Redacto target: the agent seeds the engine's clean
    /// per-state tree and the dump is generated from that, so the artefact that
    /// ships is the one it worked on — with the markup, footnotes and multi-column
    /// layout the engine already got right.
    #[tokio::test]
    async fn seeding_from_a_state_yields_a_shippable_redacto_dump() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AAEV_019_EN.pdf")],
            None,
            "test-redacto-seed".into(),
            OutputTarget::Redacto,
        );

        // Nothing authored yet: the dump tool must say so rather than emit an
        // empty document.
        match agent
            .execute("build_redacto_dump", &serde_json::json!({}))
            .await
        {
            ToolReply::Error(e) => assert_eq!(e, NO_STRUCTURED_TREE),
            _ => panic!("an unseeded tree must not build a dump"),
        }

        let states = reply_text(agent.execute("list_states", &serde_json::json!({})).await);
        let label = serde_json::from_str::<serde_json::Value>(&states).unwrap()[0]["label"]
            .as_str()
            .expect("a state label")
            .to_string();

        let seeded = reply_text(
            agent
                .execute(
                    "seed_structured_from_state",
                    &serde_json::json!({"state_label": label}),
                )
                .await,
        );
        assert!(seeded.starts_with("OK"), "{seeded}");
        assert!(
            !agent.structured().is_empty(),
            "seeding must fill the working tree"
        );

        let built = reply_text(
            agent
                .execute("build_redacto_dump", &serde_json::json!({}))
                .await,
        );
        let report: serde_json::Value = serde_json::from_str(&built).unwrap();
        assert_eq!(
            report["problems"].as_array().map(Vec::len),
            Some(0),
            "seeded content must produce a shippable dump: {built}"
        );
        assert!(
            report["assets"].as_u64().unwrap_or(0) > 5,
            "expected a text-heavy document: {built}"
        );
        assert!(
            agent.redacto_dump().is_some(),
            "the dump must be cached for finalize"
        );
    }

    /// The same tools stay reachable under the AEM target — the guard is about
    /// the target, not about the tools.
    #[test]
    fn aem_tools_are_reachable_under_the_aem_target() {
        let agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            None,
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

    #[test]
    fn line_matches_literal_and_regex() {
        assert!(line_matches("Account Holder", "holder", false));
        assert!(!line_matches("Account Holder", "nope", false));
        assert!(line_matches("field_42", r"field_\d+", true));
        assert!(!line_matches("field_x", r"field_\d+", true));
        // invalid regex → no match (not a panic)
        assert!(!line_matches("anything", "(", true));
    }

    /// Only the total accumulator is shared in the old per-source-break code
    /// too, so it does not multiply the cap by the source count the way an
    /// earlier version of this comment claimed — but it is still a real leak:
    /// once the cap is crossed, each later source's own inner loop gets to
    /// append one more line before its check re-fires, every single source,
    /// silently. Ten extra sources is ten extra unbounded-length lines the
    /// cap was supposed to rule out. Breaking the whole search removes that
    /// leak entirely, and the reply says so instead of looking complete.
    #[test]
    fn later_sources_are_not_each_given_one_more_line_past_the_cap() {
        let first = ("de.pdf".to_string(), "hit line\n".repeat(2000));
        let later: Vec<(String, String)> = (0..10)
            .map(|i| (format!("extra-{i}.pdf"), "hit\n".to_string()))
            .collect();

        let mut sources = vec![first.clone()];
        sources.extend(later);

        let cap = execute::SEARCH_XFA_TOTAL_CHARS;
        let capped_at_first = search_matching_lines(std::slice::from_ref(&first), "hit", false, cap);
        let out = search_matching_lines(&sources, "hit", false, cap);

        // The matched lines themselves must be identical either way — the
        // extra sources contribute no additional "name: line" entries.
        let matches_only = |s: &str| s.split("\n[stopped").next().unwrap().to_string();
        assert_eq!(
            matches_only(&out),
            matches_only(&capped_at_first),
            "a source after the one that already spent the budget must \
             contribute no matched lines"
        );

        // The stopping point moved nothing, so the reply must say which
        // sources it never got to — not read as though de.pdf were the whole
        // answer.
        assert!(out.contains("extra-0.pdf") && out.contains("extra-9.pdf"));
        assert!(!capped_at_first.contains("extra-0.pdf"));
    }

    /// The cap is a total, not a per-source rule: several sources that each
    /// match should combine into one bounded reply, not each get a fresh
    /// budget the moment an earlier one crosses it.
    #[test]
    fn matches_across_several_sources_share_one_budget() {
        let a = ("a.pdf".to_string(), "hit\n".repeat(2000));
        let b = ("b.pdf".to_string(), "hit\n".repeat(2000));
        let c = ("c.pdf".to_string(), "hit\n".repeat(2000));

        let cap = execute::SEARCH_XFA_TOTAL_CHARS;
        let out = search_matching_lines(&[a.clone(), b, c], "hit", false, cap);
        let from_a_alone = search_matching_lines(std::slice::from_ref(&a), "hit", false, cap);

        let matches_only = |s: &str| s.split("\n[stopped").next().unwrap().to_string();
        assert_eq!(
            matches_only(&out),
            matches_only(&from_a_alone),
            "the first source alone already spends the whole budget, so b \
             and c must add no matched lines"
        );
        assert!(out.len() > cap, "the sole source must still be over the cap");
        assert!(
            out.contains("b.pdf") && out.contains("c.pdf"),
            "the reply must name the sources it never got to search"
        );
    }

    /// A search that never crosses the cap needs no note at all — it read
    /// everything, and saying otherwise would be exactly the false claim of
    /// completeness the note exists to prevent.
    #[test]
    fn a_search_under_the_cap_carries_no_truncation_note() {
        let out = search_matching_lines(
            &[("only.pdf".to_string(), "hit\n".to_string())],
            "hit",
            false,
            execute::SEARCH_XFA_TOTAL_CHARS,
        );
        assert!(!out.contains("[stopped"));
    }

    /// The pure windowing behind `get_xfa` and `read_package_file`: a source
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

    /// `get_xfa` on real extracted XFA — not synthetic text — applies the same
    /// window and reports the same kind of note. `AABF_019_EN.pdf`'s XFA is
    /// north of ten thousand lines, comfortably past the default window.
    #[tokio::test]
    async fn get_xfa_windows_a_real_forms_xfa_by_default() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![fixture("AABF_019_EN.pdf")],
            None,
            "test-get-xfa-window".into(),
            OutputTarget::Aem,
        );

        let out = match agent.execute("get_xfa", &serde_json::json!({})).await {
            ToolReply::Text(out) => out,
            other => panic!("get_xfa failed: {other:?}"),
        };
        assert!(
            out.contains("showing lines"),
            "a form with thousands of XFA lines must be windowed by default"
        );
        assert!(out.contains("BEGIN XFA") && out.contains("END XFA"));

        let all = match agent
            .execute("get_xfa", &serde_json::json!({"limit": 1_000_000}))
            .await
        {
            ToolReply::Text(out) => out,
            other => panic!("get_xfa failed: {other:?}"),
        };
        assert!(
            !all.contains("showing lines"),
            "a limit past the source's own size must read all of it, unwindowed"
        );
        assert!(
            all.len() > out.len(),
            "the explicit large limit must return more than the default window"
        );
    }

    /// The regression that matters most: `get_xfa` windows *per PDF*, so a
    /// multi-language form's default reply is the sum of every PDF's window —
    /// this is where a per-PDF default that looked safe for one PDF can still
    /// reproduce the original failure at three. The default for this form's
    /// full three-language source must stay comfortably under the size
    /// warning, not merely under the incident's scale.
    #[tokio::test]
    async fn get_xfas_default_stays_small_across_a_multilingual_forms_full_source() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![
                fixture("AABF_019_DE.pdf"),
                fixture("AABF_019_EN.pdf"),
                fixture("AABF_019_SP.pdf"),
            ],
            None,
            "test-get-xfa-multi-pdf".into(),
            OutputTarget::Aem,
        );

        let out = match agent.execute("get_xfa", &serde_json::json!({})).await {
            ToolReply::Text(out) => out,
            other => panic!("get_xfa failed: {other:?}"),
        };
        // `agent` has no dependency on `pipeline`, so this mirrors
        // `pipeline::run::LARGE_TOOL_REPLY_WARN_CHARS` (200,000) rather than
        // importing it — the number that matters is that this stays well
        // under it.
        const MIRRORS_PIPELINES_WARN_THRESHOLD: usize = 200_000;
        assert!(
            out.len() < MIRRORS_PIPELINES_WARN_THRESHOLD,
            "the default reply across three source PDFs is {} characters — a per-PDF default \
             that looks safe for one PDF can still fail at three",
            out.len()
        );
        // All three PDFs must still be present — this is a size bound, not a
        // dropped-source bug.
        for name in ["AABF_019_DE.pdf", "AABF_019_EN.pdf", "AABF_019_SP.pdf"] {
            assert!(out.contains(name), "{name} missing from {out}");
        }
    }

    /// However large a `limit` is requested, and however many source PDFs the
    /// form has, the assembled reply must never approach the incident's
    /// scale. `windowed_text`'s own per-source window has no ceiling on an
    /// explicit `limit`, so this is `cap_total` doing its job on real,
    /// multi-PDF content rather than on synthetic text.
    #[tokio::test]
    async fn get_xfa_stays_bounded_even_with_an_enormous_explicit_limit_on_every_pdf() {
        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            vec![
                fixture("AABF_019_DE.pdf"),
                fixture("AABF_019_EN.pdf"),
                fixture("AABF_019_SP.pdf"),
            ],
            None,
            "test-get-xfa-hard-cap".into(),
            OutputTarget::Aem,
        );

        let out = match agent
            .execute("get_xfa", &serde_json::json!({"limit": 1_000_000}))
            .await
        {
            ToolReply::Text(out) => out,
            other => panic!("get_xfa failed: {other:?}"),
        };
        assert!(
            out.len() <= execute::MAX_TOTAL_REPLY_CHARS + 500,
            "an explicit limit large enough to read all three PDFs whole still produced {} \
             characters — nowhere near the incident's 873,000, but the hard cap should have \
             engaged well before this",
            out.len()
        );
        assert!(out.contains("truncated"));
    }

    #[test]
    fn config_reflects_languages_in_seeded_structured_tree() {
        use blueprint::{InlineText, ParagraphNode, StructuredNode, TranslatedText};

        let mut agent = ConversionAgent::new(
            Some("ubs".into()),
            Vec::new(),
            None,
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

    #[test]
    fn form_path_trims_slashes() {
        assert_eq!(
            join_form_path("/ubs/all/", "/AF_FORM/"),
            "/content/forms/af/ubs/all/AF_FORM"
        );
        assert_eq!(
            join_form_path("ubs", "AF_FORM"),
            "/content/forms/af/ubs/AF_FORM"
        );
    }
}

mod catalog;
mod execute;
mod prompts;

use catalog::target_mask;
pub use catalog::{ToolSpec, all_tools, catalog, scope, target, tools_for};
pub use prompts::*;
