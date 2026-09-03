//! The tool catalog as data: every tool's spec plus the targets that may run
//! it and the stages it is offered to.
//!
//! Scoping lives here and nowhere else — adding a tool means adding a
//! [`SCOPING`] row, and `scoping_covers_exactly_the_catalog` proves the table
//! and the catalog stay in step.

use blueprint::OutputTarget;

// ── Tool catalog ─────────────────────────────────────────────────────────────

/// Which output targets a tool may run under.
pub mod target {
    /// A set of [`blueprint::OutputTarget`]s, as a bitmask.
    pub type Mask = u8;
    pub const AEM: Mask = 1 << 0;
    pub const REDACTO: Mask = 1 << 1;
    pub const BOTH: Mask = AEM | REDACTO;
}

/// Which callers a tool is *offered* to.
///
/// Distinct from [`target`]: a tool can be executable under both targets while
/// only ever being offered to one target's stages. The structured-tree editors
/// are the case in point — an AEM run can execute them (a resumed session seeds
/// the structured tree), but no AEM stage is given them.
pub mod scope {
    /// A set of pipeline stages, as a bitmask.
    pub type Mask = u8;
    pub const AEM_ANALYST: Mask = 1 << 0;
    pub const AEM_AUTHOR: Mask = 1 << 1;
    pub const AEM_REVIEWER: Mask = 1 << 2;
    pub const REDACTO_ANALYST: Mask = 1 << 3;
    pub const REDACTO_AUTHOR: Mask = 1 << 4;
    pub const REDACTO_REVIEWER: Mask = 1 << 5;
    /// An external MCP client, which drives the tools itself.
    pub const MCP: Mask = 1 << 6;
    /// The read-only pass that writes a reference form's description. Sees the
    /// source and the package; edits nothing.
    pub const DESCRIBE: Mask = 1 << 7;

    pub const AEM_STAGES: Mask = AEM_ANALYST | AEM_AUTHOR | AEM_REVIEWER;
    pub const REDACTO_STAGES: Mask = REDACTO_ANALYST | REDACTO_AUTHOR | REDACTO_REVIEWER;
    pub const ALL_STAGES: Mask = AEM_STAGES | REDACTO_STAGES;
    /// Every caller, the read-only describe pass included.
    pub const EVERYWHERE: Mask = ALL_STAGES | MCP | DESCRIBE;
}

/// One entry in the tool catalog: the Anthropic-style JSON spec plus the scopes
/// it belongs to.
pub struct ToolSpec {
    /// `{name, description, input_schema}`, passed to the model verbatim.
    pub spec: serde_json::Value,
    /// Output targets whose runs may *execute* this tool.
    pub targets: target::Mask,
    /// Stages this tool is *offered* to.
    pub scopes: scope::Mask,
}

impl ToolSpec {
    pub fn name(&self) -> &str {
        self.spec["name"].as_str().unwrap_or_default()
    }
}

pub(super) fn target_mask(target: OutputTarget) -> target::Mask {
    match target {
        OutputTarget::Aem => target::AEM,
        OutputTarget::Redacto => target::REDACTO,
    }
}

/// The whole tool catalog. Built once — nothing in it depends on run state.
pub fn catalog() -> &'static [ToolSpec] {
    static CATALOG: std::sync::OnceLock<Vec<ToolSpec>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(build_catalog)
}

/// Every tool spec, unfiltered. For consumers that present the flat catalog.
pub fn all_tools() -> Vec<serde_json::Value> {
    catalog().iter().map(|t| t.spec.clone()).collect()
}

/// The tool specs offered to `scopes` in a run targeting `target`.
///
/// This is the single place a caller's tool set is decided: the app's pipeline
/// stages and the MCP server both go through it, so a tool is scoped once, in
/// [`SCOPING`], rather than in a list per consumer.
pub fn tools_for(target: OutputTarget, scopes: scope::Mask) -> Vec<serde_json::Value> {
    let target = target_mask(target);
    catalog()
        .iter()
        .filter(|t| t.targets & target != 0 && t.scopes & scopes != 0)
        .map(|t| t.spec.clone())
        .collect()
}

fn build_catalog() -> Vec<ToolSpec> {
    tool_specs()
        .into_iter()
        .map(|spec| {
            let name = spec["name"].as_str().unwrap_or_default();
            let (_, targets, scopes) = SCOPING
                .iter()
                .find(|(n, _, _)| *n == name)
                .unwrap_or_else(|| panic!("tool {name:?} has no row in SCOPING"));
            ToolSpec {
                targets: *targets,
                scopes: *scopes,
                spec,
            }
        })
        .collect()
}

/// Which target and which stages each tool belongs to.
///
/// One row per catalog entry — `scoping_covers_exactly_the_catalog` proves the
/// two stay in step, and [`build_catalog`] panics on a missing row, so a new
/// tool cannot be added without deciding who gets it.
#[rustfmt::skip]
const SCOPING: &[(&str, target::Mask, scope::Mask)] = {
    use scope::*;
    &[
        // §1 extraction. The AEM Reviewer is the one stage without
        // get_source_info: it reviews the built package against the tree, and
        // the Redacto Reviewer needs it only because languages are the thing it
        // checks. Preserved as-is rather than quietly widened.
        ("get_source_info",                   target::BOTH,    AEM_ANALYST | AEM_AUTHOR | REDACTO_STAGES | MCP | DESCRIBE),
        ("list_states",                       target::BOTH,    AEM_ANALYST | AEM_AUTHOR | REDACTO_ANALYST | REDACTO_AUTHOR | MCP | DESCRIBE),
        ("get_xfa",                           target::BOTH,    AEM_ANALYST | AEM_AUTHOR | REDACTO_ANALYST | REDACTO_AUTHOR | MCP | DESCRIBE),
        ("search_xfa",                        target::BOTH,    EVERYWHERE),
        ("get_plain_state_image",             target::BOTH,    EVERYWHERE),
        ("get_annotated_state_image",         target::BOTH,    EVERYWHERE),
        ("get_flattened_structure_for_state", target::BOTH,    AEM_ANALYST | AEM_AUTHOR | REDACTO_ANALYST | REDACTO_AUTHOR | MCP | DESCRIBE),

        // §2a structured tree — executable under both targets (a resumed AEM
        // session seeds it), but only ever offered to the Redacto stages.
        ("seed_structured_from_state",        target::BOTH,    REDACTO_AUTHOR | MCP),
        ("set_structured",                    target::BOTH,    MCP),
        ("get_structured_outline",            target::BOTH,    REDACTO_AUTHOR | REDACTO_REVIEWER | MCP),
        ("get_structured_node",               target::BOTH,    REDACTO_AUTHOR | REDACTO_REVIEWER | MCP),
        ("set_structured_field",              target::BOTH,    REDACTO_AUTHOR | MCP),
        ("set_structured_fields",             target::BOTH,    REDACTO_AUTHOR | MCP),
        ("replace_structured_node",           target::BOTH,    REDACTO_AUTHOR | MCP),
        ("insert_structured_node",            target::BOTH,    REDACTO_AUTHOR | MCP),
        ("remove_structured_node",            target::BOTH,    REDACTO_AUTHOR | MCP),

        // §2b Redacto output.
        ("build_redacto_dump",                target::REDACTO, REDACTO_AUTHOR | REDACTO_REVIEWER | MCP),
        ("review_redacto_output",             target::REDACTO, REDACTO_AUTHOR | REDACTO_REVIEWER | MCP),

        // §3 AEM tree.
        ("set_aem_translated",                target::AEM,     AEM_AUTHOR | MCP),
        ("get_aem_translated",                target::AEM,     MCP),
        ("get_aem_translated_outline",        target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("get_aem_translated_node",           target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("set_aem_translated_field",          target::AEM,     AEM_AUTHOR | MCP),
        ("replace_aem_translated_node",       target::AEM,     AEM_AUTHOR | MCP),
        ("insert_aem_translated_node",        target::AEM,     AEM_AUTHOR | MCP),
        ("remove_aem_translated_node",        target::AEM,     AEM_AUTHOR | MCP),

        // §4 AEM package.
        ("build_aem_package",                 target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("get_package_info",                  target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP | DESCRIBE),
        ("read_package_file",                 target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP | DESCRIBE),
        ("validate_aem_package",              target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("review_output",                     target::AEM,     AEM_REVIEWER | MCP),

        // §5 derived output.
        ("generate_xsd",                      target::BOTH,    AEM_AUTHOR | MCP),
        ("generate_html",                     target::BOTH,    AEM_AUTHOR | AEM_REVIEWER | MCP),

        // §6 live AEM.
        ("upload_to_aem",                     target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("fetch_aem_form_html",               target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("fetch_aem_dor_pdf",                 target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("aem_form_urls",                     target::AEM,     AEM_AUTHOR | AEM_REVIEWER | MCP),
        ("inspect_pdf",                       target::AEM,     AEM_AUTHOR | AEM_REVIEWER),

        // §7 references. The reference *forms* are AEM packages, so they are
        // pure token cost for a text-only Redacto document; only the reference
        // documentation is offered there.
        ("list_reference_forms",              target::BOTH,    AEM_ANALYST | MCP),
        ("search_references",                 target::BOTH,    AEM_ANALYST | AEM_AUTHOR | MCP),
        ("grep_references",                   target::BOTH,    AEM_ANALYST | AEM_AUTHOR | MCP),
        ("read_reference_file",               target::BOTH,    AEM_ANALYST | AEM_AUTHOR | MCP),
        ("get_reference_package",             target::BOTH,    AEM_ANALYST | AEM_AUTHOR | MCP),
        ("list_reference_docs",               target::BOTH,    AEM_ANALYST | REDACTO_ANALYST | MCP),
        ("read_reference_doc",                target::BOTH,    AEM_ANALYST | AEM_AUTHOR | REDACTO_ANALYST | REDACTO_AUTHOR | MCP),
        ("grep_reference_docs",               target::BOTH,    AEM_ANALYST | AEM_AUTHOR | REDACTO_ANALYST | REDACTO_AUTHOR | MCP),

        // §8 meta. get_profile_info reports the AEM configuration, which would
        // mislead a Redacto stage; get_source_info is the authority on languages.
        ("get_schema",                        target::BOTH,    AEM_AUTHOR | REDACTO_AUTHOR | MCP),
        ("get_profile_info",                  target::AEM,     AEM_ANALYST | AEM_AUTHOR | MCP),
        ("submit_review",                     target::BOTH,    AEM_REVIEWER | REDACTO_REVIEWER | MCP),
    ]
};

/// The single scoping decision for the externally discovered browser tool
/// family (Playwright MCP, see `crate::browser`). Its tool names are only known
/// at runtime, so this is one row for the whole family rather than one per
/// tool: the Author and Reviewer of an AEM run, nobody else. An external MCP
/// client brings its own browser, the Analyst and the describe pass only read,
/// and a Redacto document has nothing to click through.
pub const BROWSER_SCOPES: scope::Mask = scope::AEM_AUTHOR | scope::AEM_REVIEWER;

fn tool_specs() -> Vec<serde_json::Value> {
    {
        let source = serde_json::json!({
            "source": {
                "type": "object",
                "description": "Optional: which input to read. Omit for the uploaded form, or {\"reference\": \"<ref_id>\"} to run the engine on a reference's input.",
                "properties": { "reference": { "type": "string" } }
            }
        });
        let with_source = |props: serde_json::Value| {
            let mut m = props.as_object().cloned().unwrap_or_default();
            m.insert("source".to_string(), source["source"].clone());
            serde_json::Value::Object(m)
        };
        let t = |name: &str, desc: &str, props: serde_json::Value, required: serde_json::Value| {
            serde_json::json!({
                "name": name, "description": desc,
                "input_schema": { "type": "object", "properties": props, "required": required }
            })
        };
        let state_label = serde_json::json!({ "state_label": {"type": "string", "description": "A label from list_states."} });

        vec![
            // §1 extraction (source-parameterized)
            t(
                "get_source_info",
                "Info about the source PDFs (name, language, state count).",
                with_source(serde_json::json!({})),
                serde_json::json!([]),
            ),
            t(
                "list_states",
                "List discovered form states (label, pdf, selection count).",
                with_source(serde_json::json!({})),
                serde_json::json!([]),
            ),
            t(
                "get_xfa",
                "Return the source's authoritative XFA XML (all PDFs concatenated).",
                with_source(serde_json::json!({})),
                serde_json::json!([]),
            ),
            t(
                "search_xfa",
                "Regex/substring search within the source's XFA; returns matching snippets.",
                with_source(
                    serde_json::json!({"query": {"type":"string"}, "regex": {"type":"boolean"}}),
                ),
                serde_json::json!(["query"]),
            ),
            t(
                "get_plain_state_image",
                "Render a state's page image (plain).",
                with_source(state_label.clone()),
                serde_json::json!(["state_label"]),
            ),
            t(
                "get_annotated_state_image",
                "Render a state's page image with field-name overlays.",
                with_source(state_label.clone()),
                serde_json::json!(["state_label"]),
            ),
            t(
                "get_flattened_structure_for_state",
                "The engine's clean structured tree for ONE state (one language × one configurator selection). Carries no merge artifacts — no duplicated sections, colliding field names or mispaired translations. This is the building block you assemble the working tree from: inspect each state, compare against its page image and XFA, then seed from one and layer in the rest.",
                with_source(state_label.clone()),
                serde_json::json!(["state_label"]),
            ),
            // §2a structured tree (Redacto target) — seeded, then refined.
            t(
                "seed_structured_from_state",
                "Load the engine's clean structured tree for ONE state as the working tree, replacing whatever is there. START HERE: the engine already got the block structure, the inline markup, the list nesting, the footnote markers and the multi-column sections right for that state — you only have to add the OTHER languages to each node. Far cheaper and far more faithful than emitting the tree yourself with set_structured. Pick the state in the master language, then layer in the rest with set_structured_field.",
                state_label.clone(),
                serde_json::json!(["state_label"]),
            ),
            t(
                "set_structured",
                "Set the WHOLE working structured tree as a JSON array of StructuredNode (call get_schema('structured') for the exact shape). Rarely needed: prefer seed_structured_from_state followed by targeted edits, which cannot silently drop a node or a language.",
                serde_json::json!({"nodes": {"type":"array"}}),
                serde_json::json!(["nodes"]),
            ),
            t(
                "get_structured_outline",
                "Map the working structured tree: one line per node — `<path>  <type> <summary>  <flags>`. Flags: `⚠ text?` / `⚠ label?` (missing or placeholder text), `⚠ no-options` (empty choice list), `⚠ unsupported` (a node the Redacto output cannot represent: fields, images, conditionals, repeatables). Paths are `/`-separated walks from the top level, e.g. `0/children/2`, `5/rows/0/cells/1`.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "get_structured_node",
                "Return the node (its whole subtree) at `path` as JSON. Inspect it before editing to see the exact field shapes — in particular that every text is a per-language map like {\"de\":[…],\"en\":[…]}.",
                serde_json::json!({"path": {"type":"string"}}),
                serde_json::json!(["path"]),
            ),
            t(
                "set_structured_field",
                "Set one field of the node at `path`. `field` is a node key such as `content`, `level`, `label`, `items`, `columnFlow`; `value` is the raw JSON for it (match the shape from get_structured_node). This is how you add a language: read the node, then write back its `content` map with every language present. Validated by round-trip; a bad value is rejected and the tree left unchanged. Cannot change a node's `type` (use replace_structured_node).",
                serde_json::json!({"path": {"type":"string"}, "field": {"type":"string"}, "value": {}}),
                serde_json::json!(["path", "field", "value"]),
            ),
            t(
                "set_structured_fields",
                "Apply MANY set_structured_field edits in ONE call: `edits` is an array of {path, field, value}. This is how you add a language — read the outline, then write every node's `content` map back in a single call. All-or-nothing: if any edit is invalid none are applied, and the error names the offending one. Use this instead of re-emitting the whole tree, which would discard the grouping, multi-column sections and heading levels the seed carried.",
                serde_json::json!({"edits": {"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"field":{"type":"string"},"value":{}},"required":["path","field","value"]}}}),
                serde_json::json!(["edits"]),
            ),
            t(
                "replace_structured_node",
                "Replace the whole node at `path` with `node`, a JSON object parseable as a StructuredNode (must include its `type`). Use to change a node's type or rebuild it.",
                serde_json::json!({"path": {"type":"string"}, "node": {"type":"object"}}),
                serde_json::json!(["path", "node"]),
            ),
            t(
                "insert_structured_node",
                "Insert `node` (a StructuredNode JSON object) into a child list. `parent_path` is empty/\"root\" for the top level, or the path of a Group. `position` is \"first\", \"last\", {\"before\":<i>} or {\"after\":<i>}.",
                serde_json::json!({"parent_path": {"type":"string"}, "node": {"type":"object"}, "position": {"type":["string","object"]}}),
                serde_json::json!(["parent_path", "node", "position"]),
            ),
            t(
                "remove_structured_node",
                "Remove the node at `path` from its list (top-level nodes and Group children only).",
                serde_json::json!({"path": {"type":"string"}}),
                serde_json::json!(["path"]),
            ),
            t(
                "build_redacto_dump",
                "Build the Redacto PostgreSQL dump from the working structured tree and report what it contains: languages, document id, per-table row counts, the per-language page `headers` and `footers` with their `header_assets`/`footer_assets` counts, the component shape, `problems` and `warnings`. Run it after every substantive change. A `problem` means the dump is not shippable (no text assets at all, an empty body section, a language missing its variants); a `warning` means content was dropped in translation to the Redacto model. Resolve every problem before you stop.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "review_redacto_output",
                "Fidelity review: compare the engine's parse of the source against the text that actually reaches the generated dump, and report input text with no match, plus a coverage score. Compares the master language only. Reviews the DUMP, not the working tree — that is the artefact that ships.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            // §2 multilingual AEM tree (AemNodeTranslated) — authored directly.
            t(
                "set_aem_translated",
                "Set the WHOLE working AEM tree as an AemNodeTranslated JSON object (call get_schema('aem_translated') for the exact shape). Use this for the initial authoring of the form; for small fixes afterwards use the targeted editors below. Text fields (title/label/content and option labels) are per-language maps like {\"de\":\"…\",\"en\":\"…\"}; include EVERY source language. Invalidates the package.",
                serde_json::json!({"root": {"type":"object"}}),
                serde_json::json!(["root"]),
            ),
            t(
                "get_aem_translated",
                "Dump the WHOLE working AemNodeTranslated tree as JSON. Expensive on a real form — prefer get_aem_translated_outline to find the path, then get_aem_translated_node to read just that subtree.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "get_aem_translated_outline",
                "Map the working AEM tree: one line per node — `<path>  <Type>  [langs] \"excerpt\"  <flags>`. Flags: `⚠ empty` (text-bearing node with no text), `⚠ 1 lang` (only one language present — likely a missing translation). Use it to find the path to fix, then call the set/replace/insert/remove tools. Paths are `/`-separated child indices from the root (e.g. 2/0/3); `root`/empty addresses the root node.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "get_aem_translated_node",
                "Return just the node (its whole subtree) at `path` as JSON. Inspect it before editing to see the exact field shapes (e.g. how `label`/`options` are structured).",
                serde_json::json!({"path": {"type":"string"}}),
                serde_json::json!(["path"]),
            ),
            t(
                "set_aem_translated_field",
                "Set one field of the node at `path`. `field` is a node key such as `label`, `title`, `content`, `options`, `visible`, `mandatory`, `colspan`, `bind_ref`; `value` is the raw JSON for it (match the shape from get_aem_translated_node — text fields are per-language maps). Validated by round-trip; a bad value is rejected and the tree left unchanged. Cannot change a node's `type` (use replace_aem_translated_node). Invalidates the package.",
                serde_json::json!({"path": {"type":"string"}, "field": {"type":"string"}, "value": {}}),
                serde_json::json!(["path", "field", "value"]),
            ),
            t(
                "replace_aem_translated_node",
                "Replace the whole node at `path` with `node`, a JSON object parseable as an AemNodeTranslated (must include its `type`). Use to change a node's type or rebuild it. Invalidates the package.",
                serde_json::json!({"path": {"type":"string"}, "node": {"type":"object"}}),
                serde_json::json!(["path", "node"]),
            ),
            t(
                "insert_aem_translated_node",
                "Insert `node` (an AemNodeTranslated JSON object) into a child list. `parent_path` is empty/\"root\" for the root, or the path of a Panel or Repeatable (only those hold children). `position` is \"first\", \"last\", {\"before\":<i>} or {\"after\":<i>} (i = child index). Invalidates the package.",
                serde_json::json!({"parent_path": {"type":"string"}, "node": {"type":"object"}, "position": {"type":["string","object"]}}),
                serde_json::json!(["parent_path", "node", "position"]),
            ),
            t(
                "remove_aem_translated_node",
                "Remove the node at `path` from its parent's child list (the root cannot be removed). Invalidates the package.",
                serde_json::json!({"path": {"type":"string"}}),
                serde_json::json!(["path"]),
            ),
            // §5 output
            t(
                "build_aem_package",
                "Build the AEM FileVault package (ZIP) from the current AEM tree. Requires an AEM tree (author it with set_aem_translated, or refine the pre-loaded one). Stores it for upload/export.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "get_package_info",
                "Size and file list of the built package.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "read_package_file",
                "Read a file from the built package by path.",
                serde_json::json!({"path": {"type":"string"}}),
                serde_json::json!(["path"]),
            ),
            t(
                "validate_aem_package",
                "Validate the built package: checks the required FileVault structure (META-INF + jcr_root boilerplate) and validates the form and DAM .content.xml against the AEM contract (well-formedness, escaping, JCR/CQ/FD/Sling structure). Run after build_aem_package, before upload_to_aem.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "review_output",
                "Fidelity review: compare the input (the engine's merged structured parse) against the converted AEM tree and report input text/elements missing from the output, with a coverage score. Compares the master language only (spot-check other languages with search_xfa). Also reports naming_violations, label_issues, dropped_fragments (standard fragments the engine derives from this same source that the tree does not reference \u{2014} a section rebuilt out of loose fields), and feedback_violations \u{2014} the swept UBS rules (DoR exclusion implies summary exclusion, the UBS panel everywhere, code-editor rules only, the Save Progress button, the internal-bank-use block and the Italy infobox reaching the PDF alone, checkbox richTextOptions, the jump-to-field button on the step-title panel, no retired germany/italy person or signature fragments \u{2014} the UBS partner and signature generics replace them), checked on the rendered JCR XML, which is the artefact that ships. Reports legacy_tables too: every panel still holding a table the pre-HTML-component way (named TBL_, children all static draws), each of which belongs in one HtmlDisplayer node carrying a real <table>. Reads the AEM tree, so edits made only to the content XML are not reflected. Run once the tree is authored and before you report the stage done; investigate every miss (fix the tree, or confirm it was intentionally dropped) and re-run.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "generate_xsd",
                "Generate the XSD schema for the form. Renders the working structured tree, or — on an AEM run, which has none — the working AEM tree lifted back to structured content.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "generate_html",
                "Generate an HTML preview of the form. Renders the working structured tree, or — on an AEM run, which has none — the working AEM tree lifted back to structured content.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            // §6 deploy + verify
            t(
                "upload_to_aem",
                "Upload and install the built package on the configured AEM instance.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "fetch_aem_form_html",
                "Fetch the rendered Adaptive Form HTML from AEM (after upload) for verification.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "fetch_aem_dor_pdf",
                "Fetch the Document-of-Record PDF from AEM and view its first page.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "aem_form_urls",
                "The deployed form's URLs (after upload_to_aem): its JCR path, one preview URL per language (the DAM rendition outside the editor, which is what a reviewer opens with browser_navigate) and the editor URL.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "inspect_pdf",
                "Look at a PDF the browser downloaded (the PDF a submission produces). Without `path`: list the files in the browser's output directory, newest first. With `path` (a file name from that list): render every page of that PDF as an image. Only files the browser produced can be inspected.",
                serde_json::json!({"path": {"type": "string", "description": "File name from the listing. Omit to list."}}),
                serde_json::json!([]),
            ),
            // §7 references
            t(
                "list_reference_forms",
                "List the profile's reference forms (hand-built, known-good worked examples). \
                 Consult references BEFORE building: they show the expected JCR structure, \
                 dictionary setup and DoR conventions for this profile's forms.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "search_references",
                "Semantic search for precedent forms by MEANING, not by name. The query must be a \
                 natural-language DESCRIPTION of the input you are building — the form's (or the \
                 current section's) purpose, the kinds of fields it contains and how they are \
                 grouped — NOT a form name or a single keyword. References are matched by embedding \
                 this description against each reference's stored description (a literal substring \
                 fallback over descriptions + package XML is folded in). Run this first (before \
                 building), section by section; each hit carries a ref_id to pass to \
                 get_reference_package / read_reference_file. Optional top_k caps hits per signal \
                 (default 3).",
                serde_json::json!({"query": {"type":"string"}, "top_k": {"type":"integer"}}),
                serde_json::json!(["query"]),
            ),
            t(
                "grep_references",
                "Literal/regex substring search over reference descriptions + AEM package XML — the \
                 grep counterpart to search_references. Use it to find a specific string (a field \
                 name, label, or AEM resource type) verbatim; use search_references when looking \
                 for a form that resembles your input by meaning.",
                serde_json::json!({"query": {"type":"string"}, "regex": {"type":"boolean"}}),
                serde_json::json!(["query"]),
            ),
            t(
                "read_reference_file",
                "Read a reference's description ('description') or a package file by path (get the \
                 path from get_reference_package). Use it to study how a known-good form was built \
                 and mirror its structure.",
                serde_json::json!({"ref_id": {"type":"string"}, "path": {"type":"string"}, "offset": {"type":"integer"}, "limit": {"type":"integer"}}),
                serde_json::json!(["ref_id", "path"]),
            ),
            t(
                "get_reference_package",
                "List the package files (known-good output) of a reference by its ref_id (from \
                 list_reference_forms / search_references), then read individual files with \
                 read_reference_file.",
                serde_json::json!({"ref_id": {"type":"string"}}),
                serde_json::json!(["ref_id"]),
            ),
            t(
                "list_reference_docs",
                "List the profile's reference documentation (.md/.txt).",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "read_reference_doc",
                "Read a reference documentation doc by id.",
                serde_json::json!({"doc_id": {"type":"string"}, "offset": {"type":"integer"}, "limit": {"type":"integer"}}),
                serde_json::json!(["doc_id"]),
            ),
            t(
                "grep_reference_docs",
                "Regex/substring search over reference documentation.",
                serde_json::json!({"query": {"type":"string"}, "regex": {"type":"boolean"}}),
                serde_json::json!(["query"]),
            ),
            // §8 control
            t(
                "get_schema",
                "Return the JSON schema for a working tree: 'aem_translated' (what set_aem_translated and the AEM editors take) or 'structured' (what set_structured and the structured editors take).",
                serde_json::json!({"kind": {"type":"string","enum":["aem_translated","structured"]}}),
                serde_json::json!(["kind"]),
            ),
            t(
                "get_profile_info",
                "Profile/AEM config: form_code, languages, JCR paths, binding flags.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "submit_review",
                "Terminal REVIEW step (Reviewer role) — call once, last, after building/validating/reviewing. approved=true means the form is fully correct and ends the run; approved=false returns your detailed issue list to the author for a fix round.",
                serde_json::json!({
                    "approved": {"type": "boolean"},
                    "report": {"type": "string", "description": "When not approved: a detailed, actionable list of every issue, with node paths where possible."}
                }),
                serde_json::json!(["approved"]),
            ),
        ]
    }
}

#[cfg(test)]
mod catalog_guards {
    use super::*;
    use crate::conversion::prompts::*;
    use std::collections::BTreeSet;

    /// The checked-in serialisation of [`ConversionAgent::tools`]. Regenerate
    /// with `UPDATE_SNAPSHOTS=1 cargo test -p agent` after an *intended* change,
    /// and review the diff — the catalog is prompt surface, so a wording change
    /// is a behaviour change.
    const SNAPSHOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/catalog.json");

    /// snake_case words that legitimately appear in tool descriptions and
    /// prompts without naming a tool in this catalog: AEM/XFA vocabulary, JSON
    /// field and property names, and the MCP-only tools that the `mcp` crate
    /// defines rather than the engine.
    const NON_TOOL_VOCABULARY: &[&str] = &[
        // AEM / XFA / profile vocabulary appearing verbatim in prose.
        "affrg",
        "affrg_germany",
        "always_in_pdf",
        "affrg_italy",
        "afforms_ubs_fragmentlib",
        "asset_containers",
        "bind_ref",
        "dor_exclude",
        "dor_exclude_title",
        "dor_header_slot",
        "footer_assets",
        "form_code",
        "formrange_afmasterlanguage",
        "formrange_language",
        "frag_ref",
        "header_assets",
        "is_conditional",
        "is_page",
        "jump_to_field",
        "jcr_root",
        "dropped_fragments",
        "feedback_violations",
        "label_issues",
        "legacy_tables",
        "max_occur",
        "min_occur",
        "naming_violations",
        "show_if_hidden",
        "styled_panels",
        "summary_exclude",
        "textbox",
        // Tool argument and enum values, not tools.
        "aem_translated",
        "parent_path",
        "ref_id",
        "top_k",
        // Tool-call protocol vocabulary, not a tool.
        "tool_result",
        // MCP-only tools and their arguments, defined in the `mcp` crate.
        "pdf_base64",
        "pdf_name",
        "pdf_path",
        "pdf_paths",
        "start_conversion",
        "write_package",
        "validate_aem_package_from_file",
        "upload_aem_package_from_file",
    ];

    fn specs() -> Vec<serde_json::Value> {
        all_tools()
    }

    /// Every `snake_case` word in `text`, which is close enough to "looks like a
    /// tool name" for a guard-rail: tool names are the only snake_case tokens
    /// the prose uses apart from [`NON_TOOL_VOCABULARY`].
    ///
    /// A run that follows an uppercase letter is a fragment of a CamelCase
    /// identifier (`AddressBlock_CountryDD` would otherwise yield `lock`), not a
    /// snake_case word, so it is skipped.
    fn snake_case_words(text: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut word = String::new();
        let mut after_uppercase = false;
        for ch in text.chars().chain(std::iter::once(' ')) {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
                word.push(ch);
                continue;
            }
            if !after_uppercase
                && word.contains('_')
                && word.starts_with(|c: char| c.is_ascii_lowercase())
            {
                out.insert(word.trim_matches('_').to_string());
            }
            word.clear();
            after_uppercase = ch.is_ascii_uppercase();
        }
        out
    }

    /// Tool descriptions and role prompts are shipped into the model's context
    /// on every turn. Naming a tool that does not exist costs tokens and then
    /// costs a failed call — so the prose may only name tools that are real.
    ///
    /// Regression guard: `build_aem_package` and `review_output` both told the
    /// model to "run convert_structured_to_aem first", a tool that has never
    /// existed in this catalog.
    #[test]
    fn prose_only_names_tools_that_exist() {
        let catalog = specs();
        let mut names: BTreeSet<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();
        // The browser tools are offered alongside the catalog (see BROWSER_SCOPES),
        // so the prompts may name them too.
        names.extend(crate::browser::BROWSER_TOOLS.iter().copied());

        let mut prose = String::new();
        for tool in &catalog {
            prose.push_str(tool["description"].as_str().unwrap_or_default());
            prose.push('\n');
        }
        for constant in [
            SYSTEM_PROMPT,
            SHARED_PREAMBLE,
            ANALYST_ADDENDUM,
            AUTHOR_ADDENDUM,
            REVIEWER_ADDENDUM,
            MCP_ADDENDUM,
            REDACTO_SYSTEM_PROMPT,
            REDACTO_SHARED_PREAMBLE,
            REDACTO_ANALYST_ADDENDUM,
            REDACTO_AUTHOR_ADDENDUM,
            REDACTO_REVIEWER_ADDENDUM,
        ] {
            prose.push_str(constant);
            prose.push('\n');
        }

        let unknown: Vec<String> = snake_case_words(&prose)
            .into_iter()
            .filter(|w| !names.contains(w.as_str()) && !NON_TOOL_VOCABULARY.contains(&w.as_str()))
            .collect();

        assert!(
            unknown.is_empty(),
            "prompts or tool descriptions name tools that are not in the catalog: {unknown:?}\n\
             Either the tool is missing, the name is a typo, or the word belongs in \
             NON_TOOL_VOCABULARY."
        );
    }

    /// [`SCOPING`] is the one place a tool's target and stages are decided, so
    /// it has to describe the catalog exactly — no orphan rows, no tool without
    /// a row. (`build_catalog` panics on the second case; this catches the
    /// first, and reports both at once.)
    #[test]
    fn scoping_covers_exactly_the_catalog() {
        let in_catalog: BTreeSet<&str> = catalog().iter().map(|t| t.name()).collect();
        let in_scoping: BTreeSet<&str> = SCOPING.iter().map(|(n, _, _)| *n).collect();

        let orphan_rows: Vec<_> = in_scoping.difference(&in_catalog).collect();
        let unscoped: Vec<_> = in_catalog.difference(&in_scoping).collect();
        assert!(
            orphan_rows.is_empty() && unscoped.is_empty(),
            "SCOPING rows with no tool: {orphan_rows:?}; tools with no SCOPING row: {unscoped:?}"
        );
        assert_eq!(SCOPING.len(), catalog().len(), "duplicate SCOPING rows");
    }

    /// A tool nobody is offered is dead weight; a tool offered to a stage whose
    /// target cannot execute it is a guaranteed refusal wasting a turn.
    #[test]
    fn every_tool_is_offered_somewhere_consistent_with_its_target() {
        for tool in catalog() {
            let name = tool.name();
            assert!(tool.scopes != 0, "{name} is offered to nobody");
            assert!(tool.targets != 0, "{name} can run under no target");
            if tool.targets == target::AEM {
                assert!(
                    tool.scopes & scope::REDACTO_STAGES == 0,
                    "{name} is AEM-only but offered to a Redacto stage, which would always refuse it"
                );
            }
            if tool.targets == target::REDACTO {
                assert!(
                    tool.scopes & scope::AEM_STAGES == 0,
                    "{name} is Redacto-only but offered to an AEM stage, which would always refuse it"
                );
            }
        }
    }

    /// The stage tool sets are what each role actually sees. Spot-check the
    /// invariants that used to live in the app's cross-crate list test.
    #[test]
    fn stage_tool_sets_keep_their_invariants() {
        let has = |target, scope, name: &str| {
            tools_for(target, scope)
                .iter()
                .any(|t| t["name"].as_str() == Some(name))
        };

        // Only the Author writes; only the Reviewer terminates.
        assert!(has(
            OutputTarget::Aem,
            scope::AEM_AUTHOR,
            "set_aem_translated"
        ));
        assert!(!has(
            OutputTarget::Aem,
            scope::AEM_ANALYST,
            "set_aem_translated"
        ));
        assert!(!has(
            OutputTarget::Aem,
            scope::AEM_REVIEWER,
            "set_aem_translated"
        ));
        assert!(has(OutputTarget::Aem, scope::AEM_REVIEWER, "submit_review"));
        assert!(!has(OutputTarget::Aem, scope::AEM_AUTHOR, "submit_review"));
        assert!(!has(OutputTarget::Aem, scope::AEM_ANALYST, "submit_review"));

        // Termination belongs to the controller. There is deliberately no
        // terminal tool at all: `finish` existed, was offered to nobody, and
        // spent five prompt sites telling the model not to call it.
        assert!(
            !catalog().iter().any(|t| t.name() == "finish"),
            "the run is ended by the controller, not by a tool"
        );

        // The Redacto Author edits the structured tree but never re-emits it
        // wholesale: set_structured discards the grouping the seed carried.
        assert!(has(
            OutputTarget::Redacto,
            scope::REDACTO_AUTHOR,
            "seed_structured_from_state"
        ));
        assert!(has(
            OutputTarget::Redacto,
            scope::REDACTO_AUTHOR,
            "build_redacto_dump"
        ));
        assert!(!has(
            OutputTarget::Redacto,
            scope::REDACTO_AUTHOR,
            "set_structured"
        ));

        // The Analyst reads and never edits.
        for stage in [scope::AEM_ANALYST, scope::REDACTO_ANALYST] {
            let target = if stage == scope::AEM_ANALYST {
                OutputTarget::Aem
            } else {
                OutputTarget::Redacto
            };
            for writer in [
                "set_structured_field",
                "set_aem_translated_field",
                "build_aem_package",
            ] {
                assert!(
                    !has(target, stage, writer),
                    "the Analyst must not have {writer}"
                );
            }
        }
    }

    /// The browser family is offered next to the catalog, so its names must not
    /// shadow an engine tool, and it must reach exactly the two stages that
    /// upload and verify, never a read-only pass, a Redacto stage or an MCP
    /// client that has its own browser.
    #[test]
    fn the_browser_family_is_scoped_once_and_collides_with_nothing() {
        for name in crate::browser::BROWSER_TOOLS {
            assert!(
                !catalog().iter().any(|t| t.name() == *name),
                "{name} is both a browser tool and an engine tool"
            );
            assert!(
                name.starts_with("browser_"),
                "{name}: the executor routes on the prefix"
            );
        }
        assert_eq!(BROWSER_SCOPES, scope::AEM_AUTHOR | scope::AEM_REVIEWER);
        assert_eq!(
            BROWSER_SCOPES
                & (scope::MCP | scope::DESCRIBE | scope::AEM_ANALYST | scope::REDACTO_STAGES),
            0
        );

        // The Rust-side companions of the browser go where the browser goes.
        let has = |scope, name: &str| {
            tools_for(OutputTarget::Aem, scope)
                .iter()
                .any(|t| t["name"].as_str() == Some(name))
        };
        for name in ["aem_form_urls", "inspect_pdf"] {
            assert!(
                has(scope::AEM_AUTHOR, name) && has(scope::AEM_REVIEWER, name),
                "{name}"
            );
            assert!(
                !has(scope::AEM_ANALYST, name) && !has(scope::DESCRIBE, name),
                "{name}"
            );
        }
        // inspect_pdf reads the browser's output directory, which an MCP client
        // never has; aem_form_urls is useful to anyone who uploaded.
        assert!(!has(scope::MCP, "inspect_pdf"));
        assert!(has(scope::MCP, "aem_form_urls"));
    }

    #[test]
    fn the_catalog_has_no_duplicate_tool_names() {
        let catalog = specs();
        let names: Vec<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();
        let unique: BTreeSet<&&str> = names.iter().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate tool names in the catalog: {names:?}"
        );
        assert_eq!(names.len(), catalog.len(), "a tool spec is missing a name");
    }

    #[test]
    fn every_tool_declares_an_object_input_schema() {
        for tool in specs() {
            let name = tool["name"].as_str().unwrap_or("<unnamed>");
            assert_eq!(
                tool["input_schema"]["type"].as_str(),
                Some("object"),
                "{name} must declare an object input_schema"
            );
            assert!(
                tool["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{name} must carry a description — it is the model's only guidance"
            );
        }
    }

    /// The catalog is prompt surface: an accidental wording or schema change
    /// silently alters how the model behaves. Pin it.
    #[test]
    fn the_catalog_matches_its_checked_in_snapshot() {
        let actual = format!(
            "{}\n",
            serde_json::to_string_pretty(&specs()).expect("catalog serialises")
        );

        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(SNAPSHOT).parent().unwrap()).ok();
            std::fs::write(SNAPSHOT, &actual).expect("write snapshot");
            return;
        }

        let expected = std::fs::read_to_string(SNAPSHOT).unwrap_or_default();
        assert_eq!(
            actual, expected,
            "the tool catalog no longer matches tests/catalog.json. If the change is \
             intended, regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p agent` and review \
             the diff."
        );
    }
}
