//! The tool catalog as data: every tool's spec plus the targets that may run
//! it and the stages it is offered to.
//!
//! Scoping lives here and nowhere else — adding a tool means adding a
//! [`SCOPING`] row, and `scoping_covers_exactly_the_catalog` proves the table
//! and the catalog stay in step.

use crate::OutputTarget;

// ── Tool catalog ─────────────────────────────────────────────────────────────

/// Which output targets a tool may run under.
pub mod target {
    /// A set of [`crate::OutputTarget`]s, as a bitmask.
    pub type Mask = u8;
    pub const AEM: Mask = 1 << 0;
    pub const REDACTO: Mask = 1 << 1;
    pub const BOTH: Mask = AEM | REDACTO;
}

/// Which callers a tool is *offered* to.
///
/// Distinct from [`target`]: which targets may execute a tool, versus which
/// stages are handed it.
pub mod scope {
    /// A set of pipeline stages, as a bitmask.
    pub type Mask = u8;
    pub const AEM_AUTHOR: Mask = 1 << 0;
    pub const AEM_REVIEWER: Mask = 1 << 1;
    pub const REDACTO_AUTHOR: Mask = 1 << 2;
    pub const REDACTO_REVIEWER: Mask = 1 << 3;
    /// The read-only pass that writes a reference form's description. Sees the
    /// source and the package; edits nothing.
    pub const DESCRIBE: Mask = 1 << 5;
    /// A judge agent checking one rule `rule_check` handed it: reads the
    /// document and the source, edits nothing, ends with its verdict.
    pub const AEM_JUDGE: Mask = 1 << 6;
    pub const REDACTO_JUDGE: Mask = 1 << 7;

    pub const AEM_STAGES: Mask = AEM_AUTHOR | AEM_REVIEWER | AEM_JUDGE;
    pub const REDACTO_STAGES: Mask = REDACTO_AUTHOR | REDACTO_REVIEWER | REDACTO_JUDGE;
    pub const JUDGES: Mask = AEM_JUDGE | REDACTO_JUDGE;
    /// The Authors and Reviewers: the stages the pipeline runs in turn, which
    /// dispatch judges.
    pub const MAIN_STAGES: Mask = AEM_AUTHOR | AEM_REVIEWER | REDACTO_AUTHOR | REDACTO_REVIEWER;
    pub const ALL_STAGES: Mask = AEM_STAGES | REDACTO_STAGES;
    /// Every caller, the read-only describe pass included.
    pub const EVERYWHERE: Mask = ALL_STAGES | DESCRIBE;
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
    /// Whether a call may run beside other calls of the same turn.
    pub access: Access,
}

/// Whether a tool only reads state no other call of the turn can change.
///
/// A [`Access::Read`] call runs without holding the agent while its work is
/// done, so several of them (rendering every language's pages, say) run side
/// by side. Everything else is [`Access::Write`] and runs alone: the document
/// tools, the builds, a live form session, a verifier session. When in doubt,
/// a tool is a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

impl ToolSpec {
    pub fn name(&self) -> &str {
        self.spec["name"].as_str().unwrap_or_default()
    }
}

/// Whether `name` only reads (see [`Access`]); an unknown name is a write.
pub fn access_of(name: &str) -> Access {
    catalog()
        .iter()
        .find(|t| t.name() == name)
        .map_or(Access::Write, |t| t.access)
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
/// stages go through it, so a tool is scoped once, in [`SCOPING`], rather than
/// in a list per consumer.
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
        .chain(document_tool_specs())
        .chain(crate::u2s::tool_specs())
        .map(|spec| {
            let name = spec["name"].as_str().unwrap_or_default();
            let (_, targets, scopes, access) = SCOPING
                .iter()
                .find(|(n, _, _, _)| *n == name)
                .unwrap_or_else(|| panic!("tool {name:?} has no row in SCOPING"));
            ToolSpec {
                targets: *targets,
                scopes: *scopes,
                access: *access,
                spec,
            }
        })
        .collect()
}

/// Which target and which stages each tool belongs to, and whether it only
/// reads (see [`Access`]).
///
/// One row per catalog entry — `scoping_covers_exactly_the_catalog` proves the
/// two stay in step, and [`build_catalog`] panics on a missing row, so a new
/// tool cannot be added without deciding who gets it.
#[rustfmt::skip]
const SCOPING: &[(&str, target::Mask, scope::Mask, Access)] = {
    use scope::*;
    use Access::{Read, Write};
    &[
        // §1 source. Every stage reads the source through the xfa_* tools,
        // which take the `doc_path` only get_source_info hands out.
        ("get_source_info",                   target::BOTH,    EVERYWHERE, Write),

        // §1b the source form through the vendored u2s servers (crate::u2s):
        // raw XFA reads, and rendering plus live interaction.
        ("xfa_packets",                       target::BOTH,    EVERYWHERE, Read),
        ("xfa_read",                          target::BOTH,    EVERYWHERE, Read),
        ("xfa_search",                        target::BOTH,    EVERYWHERE, Read),
        ("xfa_outline",                       target::BOTH,    EVERYWHERE, Read),
        ("xfa_node",                          target::BOTH,    EVERYWHERE, Read),
        ("xfa_info",                          target::BOTH,    EVERYWHERE, Read),
        ("xfa_open",                          target::BOTH,    EVERYWHERE, Write),
        ("xfa_set",                           target::BOTH,    EVERYWHERE, Write),
        ("xfa_reset",                         target::BOTH,    EVERYWHERE, Write),
        ("xfa_close",                         target::BOTH,    EVERYWHERE, Write),
        ("xfa_controls",                      target::BOTH,    EVERYWHERE, Write),
        ("xfa_render_page",                   target::BOTH,    EVERYWHERE, Read),
        ("xfa_render_pages",                  target::BOTH,    EVERYWHERE, Read),
        ("xfa_render_region",                 target::BOTH,    EVERYWHERE, Read),
        ("xfa_page_text",                     target::BOTH,    EVERYWHERE, Read),
        ("xfa_search_text",                   target::BOTH,    EVERYWHERE, Read),

        // §1c PDFs the verifiers produce, through the vendored u2s PDF renderer.
        ("pdf_info",                          target::BOTH,    MAIN_STAGES | JUDGES, Read),
        ("pdf_render_page",                   target::BOTH,    MAIN_STAGES | JUDGES, Read),
        ("pdf_render_pages",                  target::BOTH,    MAIN_STAGES | JUDGES, Read),
        ("pdf_render_region",                 target::BOTH,    MAIN_STAGES | JUDGES, Read),
        ("pdf_page_text",                     target::BOTH,    MAIN_STAGES | JUDGES, Read),
        ("pdf_search_text",                   target::BOTH,    MAIN_STAGES | JUDGES, Read),

        // §2 the run's output document: one revisioned JSON document per run,
        // read and patched with the json_* tools and held to the rules with the
        // rule_* ones. The UBS Redacto format has no rules yet.
        ("json_outline",                      target::BOTH,    EVERYWHERE, Write),
        ("json_get",                          target::BOTH,    EVERYWHERE, Write),
        ("json_search",                       target::BOTH,    EVERYWHERE, Write),
        ("json_patch",                        target::BOTH,    AEM_AUTHOR | REDACTO_AUTHOR, Write),
        ("json_validate",                     target::BOTH,    MAIN_STAGES | JUDGES, Write),
        ("rule_list",                         target::BOTH,    MAIN_STAGES, Write),
        ("rule_check",                        target::BOTH,    MAIN_STAGES, Write),
        ("rule_autofix",                      target::AEM,     AEM_AUTHOR, Write),

        // §3 building the output through the UBS encoders.
        // Only the Author builds: the Reviewer judges the build the pipeline
        // made of the Author's last document, and changes nothing.
        ("build_redacto_dump",                target::REDACTO, REDACTO_AUTHOR, Write),
        ("build_aem_package",                 target::AEM,     AEM_AUTHOR, Write),
        ("get_package_info",                  target::AEM,     AEM_AUTHOR | AEM_REVIEWER | AEM_JUDGE | DESCRIBE, Write),
        ("read_package_file",                 target::AEM,     AEM_AUTHOR | AEM_REVIEWER | AEM_JUDGE | DESCRIBE, Write),
        ("coverage_check",                    target::AEM,     AEM_AUTHOR | AEM_REVIEWER | AEM_JUDGE, Write),

        // §6 verification through the vendored u2s verifiers (crate::u2s): the
        // AEM package against a Docker AEM, the Redacto dump against a throwaway
        // Postgres.
        ("aem_verify_status",                 target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_package_check",          target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_run",                    target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_open",                   target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_controls",               target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_set",                    target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_next",                   target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_prev",                   target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_reset",                  target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_screenshot",             target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_submit",                 target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("aem_verify_close",                  target::AEM,     AEM_AUTHOR | AEM_REVIEWER, Write),
        ("redacto_verify_status",             target::REDACTO, REDACTO_AUTHOR | REDACTO_REVIEWER, Write),
        ("redacto_verify_dump_check",         target::REDACTO, REDACTO_AUTHOR | REDACTO_REVIEWER, Write),
        ("redacto_verify_run",                target::REDACTO, REDACTO_AUTHOR | REDACTO_REVIEWER, Write),

        // §7 references. The reference *forms* are AEM packages, so they are
        // pure token cost for a text-only Redacto document; only the reference
        // documentation is offered there.
        ("list_reference_forms",              target::BOTH,    AEM_AUTHOR, Read),
        ("search_references",                 target::BOTH,    AEM_AUTHOR, Read),
        ("grep_references",                   target::BOTH,    AEM_AUTHOR, Read),
        ("read_reference_file",               target::BOTH,    AEM_AUTHOR, Read),
        ("get_reference_package",             target::BOTH,    AEM_AUTHOR, Read),
        ("list_reference_docs",               target::BOTH,    AEM_AUTHOR | REDACTO_AUTHOR, Read),
        ("read_reference_doc",                target::BOTH,    AEM_AUTHOR | REDACTO_AUTHOR, Read),
        ("grep_reference_docs",               target::BOTH,    AEM_AUTHOR | REDACTO_AUTHOR, Read),

        // §8 meta.
        ("finish_authoring",                  target::BOTH,    AEM_AUTHOR | REDACTO_AUTHOR, Write),
        ("submit_review",                     target::BOTH,    AEM_REVIEWER | REDACTO_REVIEWER, Write),
        ("submit_rule_verdict",               target::BOTH,    JUDGES, Write),
    ]
};


/// The `json_*` and `rule_*` tools, specified by `u2s-doc-tools` itself, so
/// their descriptions are the ones v3's own agent is prompted with.
fn document_tool_specs() -> impl Iterator<Item = serde_json::Value> {
    crate::conversion::DOCUMENT_TOOLS.iter().map(|tool| {
        serde_json::json!({
            "name": tool.name(),
            "description": rule_tool_description(*tool).unwrap_or(tool.description()),
            "input_schema": tool.input_schema(),
        })
    })
}

/// The rule tools' descriptions here, where a rule is scripted or judged (see
/// `crate::rules`): the vendored ones know only scripts.
fn rule_tool_description(tool: u2s_doc_tools::native::NativeJsonTool) -> Option<&'static str> {
    use u2s_doc_tools::native::NativeJsonTool;
    match tool {
        NativeJsonTool::ListRules => Some(
            "List every rule the document is held to, by id, title and description. Read them all \
             before you author: the description says what is required and how to fix a break. \
             `check` says how a rule is checked: `script` (its script decides it, on every edit \
             too) or `agent` (a judge agent reads the document against the description).",
        ),
        NativeJsonTool::CheckRules => Some(
            "Check the document against its rules and return each rule's verdict and violations \
             (a JSON Pointer and what to fix). A scripted rule runs its script; a judged rule is \
             handed to a judge agent, several in parallel, which takes longer and costs a model \
             run per rule, so name the rules you need with rule_ids when you do not need them all. \
             Where no judge runs, a judged rule comes back `unchecked`. When the document has a \
             current build, `package_findings` lists what its package breaks. Read-only.",
        ),
        _ => None,
    }
}

fn tool_specs() -> Vec<serde_json::Value> {
    {
        let source = serde_json::json!({
            "source": {
                "type": "object",
                "description": "Optional: which input to read. Omit for the uploaded form, or {\"reference\": \"<ref_id>\"} for a reference's input.",
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

        let mut specs = vec![
            // §1 extraction (source-parameterized)
            t(
                "get_source_info",
                "The source PDFs: each one's file name, language, XFA template `variables` and the `doc_path` every xfa_* tool takes. Call this first.",
                with_source(serde_json::json!({})),
                serde_json::json!([]),
            ),
            // §3 building the output
            t(
                "build_redacto_dump",
                "Encode the document into the Redacto PostgreSQL dump, adding the UBS metadata, page header and footer from each language's source, and report what it holds: the document id, languages, asset count and dump size. A document the Redacto model refuses (an empty body, an asset missing a language, a reference to an asset that does not exist) is reported with every violation and builds nothing. Build after every substantive change; the redacto_verify_* tools check the latest build.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "build_aem_package",
                "Encode the document into the UBS AEM FileVault package (ZIP) through the UBS writer, along with the same form bound to its schema and the schema (XSD) itself, and check the package's form and DAM XML. A document the encoder refuses (a text in a language `languages` does not list, a master text translated two ways, a variable the profile needs missing), or a package that fails the XML checks, is reported and builds nothing. Build after every substantive change; the aem_verify_* tools check the latest build.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "get_package_info",
                "Size and file list of the latest built package.",
                serde_json::json!({}),
                serde_json::json!([]),
            ),
            t(
                "read_package_file",
                "Read a file from the built package by path. Replies with a 500-line window by \
                 default — pass offset (lines to skip) and limit (lines to return) to page \
                 through a larger file. The total reply is capped regardless of limit; a \
                 truncation note says so if it is hit.",
                serde_json::json!({"path": {"type":"string"}, "offset": {"type":"integer"}, "limit": {"type":"integer"}}),
                serde_json::json!(["path"]),
            ),
            t(
                "coverage_check",
                "Which texts of the source form did not reach the document. Compares every \
                 user-visible text of each source PDF's XFA template (draws, captions, choice-list \
                 items, master pages; every configurator variant, since the template holds them all) \
                 with the document's texts in that PDF's language, and replies per language with the \
                 coverage and the missing texts. A missing text is a lead to look up on the rendered \
                 source page, not a defect by itself: texts only scripts use, page furniture and \
                 texts a referenced fragment renders itself are expected misses. Pass language to \
                 check one language only.",
                serde_json::json!({"language": {"type":"string", "description": "A language code get_source_info lists; omit for every language."}}),
                serde_json::json!([]),
            ),
        ];
        // §7 references, specified by the references server.
        specs.extend(references_mcp::specs::tool_specs());
        specs.extend([
            // §8 control
            t(
                "finish_authoring",
                "Terminal AUTHOR step: call once, last, when the form is complete and you have \
                 verified it yourself. Refused, with the list of what is missing, until this stage \
                 has used the current build on its verifier, read the PDF that produced, rendered \
                 the source pages and (AEM) set every source control the form's scripts read; do \
                 those and call it again. Ends your stage and hands the form to the Reviewer.",
                serde_json::json!({
                    "summary": {"type": "string", "description": "What you compared against the source, what you changed, and what the verification showed."}
                }),
                serde_json::json!(["summary"]),
            ),
            t(
                "submit_rule_verdict",
                "Terminal step of a judge: call once, last, with your verdict on the one rule you \
                 were given. pass=true when the document keeps the rule everywhere; otherwise \
                 pass=false and one violation per place that breaks it, each with the JSON Pointer \
                 of the node and a message saying what is wrong and how to fix it.",
                serde_json::json!({
                    "judgement": {"type": "string", "description": "The judgement id you were given with the rule."},
                    "pass": {"type": "boolean"},
                    "violations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"pointer": {"type": "string"}, "message": {"type": "string"}},
                            "required": ["pointer", "message"]
                        }
                    }
                }),
                serde_json::json!(["judgement", "pass", "violations"]),
            ),
            t(
                "submit_review",
                "Terminal REVIEW step (Reviewer role): call once, last, after validating and reviewing. approved=true means the form is fully correct and ends the run; it is refused, with the list of what is missing, until this stage has used the current build on its verifier, read the PDF that produced, rendered the source pages and (AEM) set every source control the form's scripts read. approved=false returns your detailed issue list to the author for a fix round and is never refused, save for a malformed rule_conflicts entry. rule_conflicts lists the places where rules ask for opposite things, which no fix can settle: the author is told to leave them alone, and when they are all that is left (report empty or only engine defects) the run stops for a person instead of starting another round.",
                serde_json::json!({
                    "approved": {"type": "boolean"},
                    "report": {"type": "string", "description": "When not approved: a detailed, actionable list of every issue, with node paths where possible."},
                    "rule_conflicts": {
                        "type": "array",
                        "description": "Places where the rules contradict each other, so that whatever the author does there one of them breaks. Not issues for the author; only with approved=false.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "rules": {"type": "array", "items": {"type": "string"}, "minItems": 2, "description": "The ids of the rules involved."},
                                "path": {"type": "string", "description": "The JSON Pointer of the node they disagree about."},
                                "why": {"type": "string", "description": "What each rule asks for there."}
                            },
                            "required": ["rules", "path", "why"]
                        }
                    }
                }),
                serde_json::json!(["approved"]),
            ),
        ]);
        specs
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
    /// field and property names.
    const NON_TOOL_VOCABULARY: &[&str] = &[
        // Argument and result names in the u2s document tools' own descriptions.
        "autofix_available",
        "rule_ids",
        "rule_conflicts",
        // AEM / XFA / profile vocabulary appearing verbatim in prose.
        "affrg",
        "affrg_germany",
        "always_in_pdf",
        "affrg_italy",
        "afforms_global_fragmentlib",
        "afforms_ubs_fragmentlib",
        "affrg_global",
        "bind_ref",
        "dor_exclude",
        "dor_exclude_title",
        "dor_header_slot",
        "form_code",
        "formrange_afmasterlanguage",
        "formrange_language",
        "frag_ref",
        "is_conditional",
        "is_page",
        "jump_to_field",
        "jcr_root",
        "max_occur",
        "min_occur",
        "show_if_hidden",
        "summary_exclude",
        "textbox",
        "init_hide",
        "init_show",
        // u2s tool arguments and reply fields.
        "affects_layout",
        "side_effects",
        "budget_hit",
        "doc_path",
        "expected_revision",
        "form_type",
        "max_depth",
        "max_edge_px",
        "next_from",
        "not_xfa",
        "rect_pt",
        "total_chars",
        "total_matches",
        "xfa_foreground",
        "xfa_full",
        // Verifier arguments and reply fields.
        "artifact_blob",
        "artifact_path",
        "dry_run",
        "has_next",
        "is_terminal",
        "package_path",
        "session_id",
        // Tool family prefixes (`xfa_*`), not tools.
        "aem_verify",
        "redacto_verify",
        "xfa",
        "parent_path",
        "ref_id",
        "top_k",
        // rule_check's report of the current build's package checks.
        "package_findings",
        // Tool-call protocol vocabulary, not a tool.
        "tool_result",
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
        let names: BTreeSet<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();

        let mut prose = String::new();
        for tool in &catalog {
            prose.push_str(tool["description"].as_str().unwrap_or_default());
            prose.push('\n');
        }
        for constant in [
            SYSTEM_PROMPT,
            SHARED_PREAMBLE,
            AUTHOR_ADDENDUM,
            REVIEWER_ADDENDUM,
            JUDGE_PREAMBLE,
            REDACTO_SYSTEM_PROMPT,
            REDACTO_SHARED_PREAMBLE,
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

    /// A tool existing somewhere is not enough: a stage told to call a tool it
    /// is not offered spends a turn on a refusal, or silently skips the step.
    /// So every prompt may only name tools its own stage is offered under its
    /// own target.
    ///
    /// Regression guard: the Author was told to start with list_reference_docs
    /// and list_reference_forms, which only the retired Analyst was offered.
    #[test]
    fn each_stage_prompt_only_names_tools_that_stage_is_offered() {
        let names: BTreeSet<&str> = catalog().iter().map(|t| t.name()).collect();
        let stages: [(OutputTarget, scope::Mask, &str, Vec<&str>); 6] = [
            (OutputTarget::Aem, scope::AEM_JUDGE, "AEM Judge", vec![JUDGE_PREAMBLE]),
            (OutputTarget::Redacto, scope::REDACTO_JUDGE, "Redacto Judge", vec![JUDGE_PREAMBLE]),
            (OutputTarget::Aem, scope::AEM_AUTHOR, "AEM Author", vec![SYSTEM_PROMPT, AUTHOR_ADDENDUM]),
            (OutputTarget::Aem, scope::AEM_REVIEWER, "AEM Reviewer", vec![SHARED_PREAMBLE, REVIEWER_ADDENDUM]),
            (
                OutputTarget::Redacto,
                scope::REDACTO_AUTHOR,
                "Redacto Author",
                vec![REDACTO_SYSTEM_PROMPT, REDACTO_AUTHOR_ADDENDUM],
            ),
            (
                OutputTarget::Redacto,
                scope::REDACTO_REVIEWER,
                "Redacto Reviewer",
                vec![REDACTO_SHARED_PREAMBLE, REDACTO_REVIEWER_ADDENDUM],
            ),
        ];

        let mut problems = Vec::new();
        for (target, stage, label, prompts) in stages {
            let offered: BTreeSet<String> = tools_for(target, stage)
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect();
            let named = snake_case_words(&prompts.concat());
            for tool in named.iter().filter(|w| names.contains(w.as_str())) {
                if !offered.contains(tool) {
                    problems.push(format!("{label} names {tool}"));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "prompts name tools their stage is not offered: {problems:?}"
        );
    }

    /// [`SCOPING`] is the one place a tool's target and stages are decided, so
    /// it has to describe the catalog exactly — no orphan rows, no tool without
    /// a row. (`build_catalog` panics on the second case; this catches the
    /// first, and reports both at once.)
    #[test]
    fn scoping_covers_exactly_the_catalog() {
        let in_catalog: BTreeSet<&str> = catalog().iter().map(|t| t.name()).collect();
        let in_scoping: BTreeSet<&str> = SCOPING.iter().map(|(n, _, _, _)| *n).collect();

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

        // Only the Author edits the document; only the Reviewer terminates.
        for target in OutputTarget::ALL {
            let (author, reviewer) = match target {
                OutputTarget::Aem => (scope::AEM_AUTHOR, scope::AEM_REVIEWER),
                OutputTarget::Redacto => (scope::REDACTO_AUTHOR, scope::REDACTO_REVIEWER),
            };
            assert!(has(target, author, "json_patch"));
            assert!(!has(target, reviewer, "json_patch"));
            assert!(has(target, reviewer, "json_outline") && has(target, reviewer, "json_get"));
            assert!(has(target, reviewer, "submit_review"));
            assert!(!has(target, author, "submit_review"));
            assert!(has(target, author, "finish_authoring"));
            assert!(!has(target, reviewer, "finish_authoring"));
            // The Reviewer changes nothing: no edit, no build.
            for barred in ["json_patch", "rule_autofix", "build_aem_package", "build_redacto_dump"] {
                assert!(!has(target, reviewer, barred), "the Reviewer must not have {barred}");
            }
        }
        assert!(has(OutputTarget::Aem, scope::AEM_AUTHOR, "rule_autofix"));
        assert!(!has(OutputTarget::Aem, scope::AEM_REVIEWER, "rule_autofix"));
        assert!(has(OutputTarget::Aem, scope::AEM_REVIEWER, "rule_check"));
        assert!(has(OutputTarget::Redacto, scope::REDACTO_AUTHOR, "build_redacto_dump"));

        // The run is ended by the controller. A stage ends with its own
        // gated terminal call (`finish_authoring`, `submit_review`); the old
        // ungated `finish`, offered to nobody, stays gone.
        assert!(
            !catalog().iter().any(|t| t.name() == "finish"),
            "the run is ended by the controller, not by a tool"
        );

        // Nobody is handed the engine's precomputed states any more: every
        // stage reads the source form through the u2s tools.
        for scope in [scope::AEM_AUTHOR, scope::AEM_REVIEWER, scope::DESCRIBE] {
            assert!(has(OutputTarget::Aem, scope, "xfa_render_page"));
            assert!(has(OutputTarget::Aem, scope, "xfa_outline"));
        }

        // A judge reads, never edits, never dispatches judges of its own, and
        // ends with its verdict.
        for (target, judge) in [(OutputTarget::Aem, scope::AEM_JUDGE), (OutputTarget::Redacto, scope::REDACTO_JUDGE)] {
            for barred in ["json_patch", "rule_check", "rule_autofix", "submit_review", "build_aem_package", "build_redacto_dump"] {
                assert!(!has(target, judge, barred), "a judge must not have {barred}");
            }
            for needed in ["submit_rule_verdict", "json_get", "json_outline", "xfa_page_text", "get_source_info"] {
                assert!(has(target, judge, needed), "a judge needs {needed}");
            }
        }
        assert!(!has(OutputTarget::Aem, scope::AEM_REVIEWER, "submit_rule_verdict"));

        // Only the Authors and Reviewers drive a verifier: a judge or the
        // describe pass runs beside a stage that may be driving it already.
        for tool in catalog() {
            let name = tool.name();
            if name.starts_with("aem_verify_") || name.starts_with("redacto_verify_") {
                assert_eq!(tool.scopes & !scope::MAIN_STAGES, 0, "{name} is offered beyond the Authors and Reviewers");
            }
        }

        // The describe pass reads and never edits.
        for writer in ["json_patch", "rule_autofix", "build_aem_package", "build_redacto_dump"] {
            assert!(
                !has(OutputTarget::Aem, scope::DESCRIBE, writer),
                "the describe pass must not have {writer}"
            );
        }
    }

    /// Each verifier checks one target's artifact, so it reaches that target's
    /// Author and Reviewer, never the other target or a
    /// read-only pass.
    #[test]
    fn each_verifier_reaches_only_its_own_targets_writers() {
        for (prefix, target) in [
            ("aem_verify_", OutputTarget::Aem),
            ("redacto_verify_", OutputTarget::Redacto),
        ] {
            let family: Vec<&ToolSpec> = catalog()
                .iter()
                .filter(|t| t.name().starts_with(prefix))
                .collect();
            assert!(!family.is_empty(), "no {prefix}* tools in the catalog");
            for tool in family {
                assert_eq!(tool.targets, target_mask(target), "{}", tool.name());
                assert_eq!(tool.scopes & scope::DESCRIBE, 0, "{}", tool.name());
            }
        }
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
