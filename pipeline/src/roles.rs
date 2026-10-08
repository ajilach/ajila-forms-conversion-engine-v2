//! Pipeline-stage policy: which stages a run has, how long each may run, what it
//! watches for a stall, and how its system prompt is composed.
//!
//! This is controller policy rather than engine capability — *which tools a
//! stage may call* is decided once in `agent`'s catalog (see `agent::scope`),
//! and a stage here names a scope rather than carrying its own list.

use agent::{
    AUTHOR_ADDENDUM, JUDGE_PREAMBLE, REDACTO_AUTHOR_ADDENDUM, REDACTO_REVIEWER_ADDENDUM, REDACTO_SHARED_PREAMBLE,
    REDACTO_SYSTEM_PROMPT, REVIEWER_ADDENDUM, SHARED_PREAMBLE, SYSTEM_PROMPT,
};

use agent::OutputTarget;

/// How many times a *transient* API failure (timeout, dropped connection,
/// overload, rate limit, 5xx) is retried automatically before the run pauses and
/// asks the user. A turn that fails mid-stream has not been appended to the
/// stage history, so re-sending it is safe — the request is simply rebuilt from
/// the unchanged history.
pub(crate) const MAX_AUTO_RETRIES: usize = 6;
/// Base delay before the first automatic retry, doubled on each further attempt
/// and capped at [`MAX_RETRY_BACKOFF_SECS`].
pub(crate) const RETRY_BACKOFF_SECS: u64 = 5;
/// Ceiling for the exponential retry backoff.
pub(crate) const MAX_RETRY_BACKOFF_SECS: u64 = 60;
/// How often the paused loop checks whether the user pressed Retry.
pub(crate) const RETRY_POLL_MS: u64 = 200;

/// How many consecutive calls of a stage's stuck tool with identical output are
/// allowed before the stage gives up (avoids an endless build loop).
pub(crate) const MAX_VALIDATE_REPEATS: usize = 3;
/// How many consecutive turns that overflow the output-token cap we nudge
/// toward incremental authoring before giving up (avoids an endless loop if the
/// model keeps trying to emit one oversized call regardless).
pub(crate) const MAX_MAX_TOKEN_NUDGES: usize = 3;

/// Injected when a turn is cut off at the output-token cap — almost always
/// mid-way through one oversized tool call (a monolithic whole-form patch for a
/// large form). Steers the agent to author incrementally so no single call has
/// to fit under the output-token cap. Per target, because it names the tools the
/// target actually has.
pub(crate) const AEM_MAX_TOKENS_NUDGE: &str = "\
Your previous turn was cut off at the output-token limit before it completed — that call \
was NOT executed. This almost always means you tried to emit too much in a single tool call \
(e.g. authoring a whole large form in one json_patch). Do NOT retry it as one call. \
Instead author the form incrementally so no single call is oversized:\n\
1. json_patch a SMALL skeleton only: one empty Panel per top-level section under \
`/form/children` (titles set, no inner fields yet).\n\
2. Then fill in each section one at a time with further json_patch calls, each adding the \
fields and sub-panels of one section to its Panel's `children`.\n\
Keep every individual call small. Proceed now.";

pub(crate) const REDACTO_MAX_TOKENS_NUDGE: &str = "\
Your previous turn was cut off at the output-token limit before it completed — that call \
was NOT executed. This almost always means you tried to emit too much in a single tool call \
(e.g. authoring a whole large document in one json_patch). Do NOT retry it as one call. \
Instead author the document incrementally so no single call is oversized: json_patch one \
section at a time, adding its assets to `/assets` and its components to `/body`. Keep every \
individual call small. Proceed now.";

// ── Roles ────────────────────────────────────────────────────────────────────

/// A pipeline stage: a name, the subset of the agent's tools it may call, and a
/// per-stage turn budget. The system prompt and seed message are supplied per
/// invocation by [`Run::execute`] (so the Reviewer reports can be pinned into
/// `system`).
pub(crate) struct Role {
    pub(crate) name: &'static str,
    /// Which catalog scope this stage is. The tools themselves are scoped in
    /// `agent`'s catalog, so a stage names a scope rather than carrying a list
    /// that has to be kept in step with the engine by hand.
    pub(crate) scope: agent::scope::Mask,
    pub(crate) max_iterations: usize,
    /// The tool whose repeated identical output means the stage is going in
    /// circles. `None` for stages that have no such tool.
    pub(crate) stuck_tool: Option<&'static str>,
    /// What [`Role::stuck_tool`] does, for the warning shown when it loops.
    pub(crate) stuck_activity: &'static str,
    /// Injected when a turn overflows the output-token cap. Names the authoring
    /// tools this role actually has, so it must be per target.
    pub(crate) max_tokens_nudge: &'static str,
    /// Whether the stage's conversation is stored under the session and loaded
    /// when it runs again. A judge's is not: it is one-shot, and judges run in
    /// parallel, so one stored conversation would mix them.
    pub(crate) remember: bool,
}

pub(crate) const AUTHOR: Role = Role {
    name: "Author",
    scope: agent::scope::AEM_AUTHOR,
    max_iterations: 110,
    stuck_tool: Some("build_aem_package"),
    stuck_activity: "the package build",
    max_tokens_nudge: AEM_MAX_TOKENS_NUDGE,
    remember: true,
};

/// The Reviewer's budget covers a browser click-through of the deployed form
/// (one turn per page, per field group, per language), not just the package
/// checks the Redacto reviewer needs.
pub(crate) const REVIEWER: Role = Role {
    name: "Reviewer",
    scope: agent::scope::AEM_REVIEWER,
    max_iterations: 60,
    stuck_tool: Some("build_aem_package"),
    stuck_activity: "the package build",
    max_tokens_nudge: AEM_MAX_TOKENS_NUDGE,
    remember: true,
};

// ── Redacto roles ────────────────────────────────────────────────────────────
//
// A Redacto document is text only, so these stages never touch an AEM form.

pub(crate) const REDACTO_AUTHOR: Role = Role {
    name: "Author",
    scope: agent::scope::REDACTO_AUTHOR,
    max_iterations: 110,
    stuck_tool: Some("build_redacto_dump"),
    stuck_activity: "the dump build",
    max_tokens_nudge: REDACTO_MAX_TOKENS_NUDGE,
    remember: true,
};

pub(crate) const REDACTO_REVIEWER: Role = Role {
    name: "Reviewer",
    scope: agent::scope::REDACTO_REVIEWER,
    max_iterations: 30,
    stuck_tool: Some("build_redacto_dump"),
    stuck_activity: "the dump build",
    max_tokens_nudge: REDACTO_MAX_TOKENS_NUDGE,
    remember: true,
};

/// A judge: checks one rule `rule_check` handed it, reads, edits nothing, and
/// ends with `submit_rule_verdict`. Same role for both targets but its scope.
const JUDGE_TURNS: usize = 20;
const JUDGE_NUDGE: &str = "Your previous turn was cut off at the output-token limit. Keep each \
call small: read one part of the document at a time, then call submit_rule_verdict.";

pub(crate) const JUDGE: Role = Role {
    name: "Judge",
    scope: agent::scope::AEM_JUDGE,
    max_iterations: JUDGE_TURNS,
    stuck_tool: None,
    stuck_activity: "judging",
    max_tokens_nudge: JUDGE_NUDGE,
    remember: false,
};

pub(crate) const REDACTO_JUDGE: Role = Role {
    name: "Judge",
    scope: agent::scope::REDACTO_JUDGE,
    max_iterations: JUDGE_TURNS,
    stuck_tool: None,
    stuck_activity: "judging",
    max_tokens_nudge: JUDGE_NUDGE,
    remember: false,
};

/// The stages for one output target.
pub(crate) struct TargetRoles {
    pub(crate) author: &'static Role,
    pub(crate) reviewer: &'static Role,
    /// The judge `rule_check` dispatches for each judged rule.
    pub(crate) judge: &'static Role,
    /// What the Author stage header says it is doing.
    pub(crate) author_doing: &'static str,
    /// Seed message that starts a fresh Author stage.
    pub(crate) author_seed: &'static str,
    /// Seed message for an Author stage that applies review feedback.
    pub(crate) author_fix_seed: &'static str,
    /// Seed message for an Author stage carrying on from a previous run's tree.
    ///
    /// Distinct from [`Self::author_seed`], which tells the Author to *begin*
    /// from the source: authoring from scratch would throw away the tree a
    /// continuation was seeded with.
    pub(crate) author_continue_seed: &'static str,
}

pub(crate) fn roles_for(target: OutputTarget) -> TargetRoles {
    match target {
        OutputTarget::Aem => TargetRoles {
            author: &AUTHOR,
            reviewer: &REVIEWER,
            judge: &JUDGE,
            author_doing: "building the AEM form",
            author_seed: "Inspect the source form, then author the full form in the document, \
                          then rule_check and build_aem_package.",
            author_fix_seed: "Apply the REVIEW FEEDBACK in your instructions to the document, then \
                              rule_check and build_aem_package.",
            author_continue_seed: "The document already holds what an earlier run built for this \
                                   form. Inspect it against the source with json_outline, finish \
                                   whatever is missing or incomplete, then rule_check and \
                                   build_aem_package. Do not start over.",
        },
        OutputTarget::Redacto => TargetRoles {
            author: &REDACTO_AUTHOR,
            reviewer: &REDACTO_REVIEWER,
            judge: &REDACTO_JUDGE,
            author_doing: "building the Redacto document",
            author_seed: "Inspect the source document, then author the full document, then \
                          build_redacto_dump.",
            author_fix_seed: "Apply the REVIEW FEEDBACK in your instructions to the document, then \
                              build_redacto_dump.",
            author_continue_seed: "The document already holds what an earlier run built for this \
                                   document. Inspect it against the source with json_outline, \
                                   finish whatever is missing or incomplete, then \
                                   build_redacto_dump. Do not start over.",
        },
    }
}

// ── Per-role system-prompt composition (reviews pinned in `system`) ──────────

/// The document's format, pinned into every stage right after its role text
/// and before the reviews, so the part of the prompt that stays the same across
/// rounds stays one prefix.
fn format_note(target: OutputTarget) -> String {
    format!("\n\n{}", agent::conversion::document_format(target))
}

/// The Author reuses the full [`SYSTEM_PROMPT`] authoring body, then the addendum,
/// then every accumulated REVIEW FEEDBACK round.
pub(crate) fn sys_author(
    target: OutputTarget,
    extra: &str,
    template_note: &str,
    reviews: &[String],
) -> String {
    let mut s = match target {
        OutputTarget::Aem => {
            format!("{SYSTEM_PROMPT}{extra}{template_note}\n\n{AUTHOR_ADDENDUM}")
        }
        // No template note: an uploaded content package is an AEM artefact and
        // is not pre-loaded for this target.
        OutputTarget::Redacto => {
            format!("{REDACTO_SYSTEM_PROMPT}{extra}\n\n{REDACTO_AUTHOR_ADDENDUM}")
        }
    };
    s.push_str(&format_note(target));
    append_reviews(
        &mut s,
        "## REVIEW FEEDBACK — address every point across all rounds",
        reviews,
    );
    s
}

pub(crate) fn sys_reviewer(target: OutputTarget, extra: &str, reviews: &[String]) -> String {
    let mut s = match target {
        OutputTarget::Aem => format!("{SHARED_PREAMBLE}{extra}\n\n{REVIEWER_ADDENDUM}"),
        OutputTarget::Redacto => {
            format!("{REDACTO_SHARED_PREAMBLE}{extra}\n\n{REDACTO_REVIEWER_ADDENDUM}")
        }
    };
    s.push_str(&format_note(target));
    append_reviews(
        &mut s,
        "## PRIOR REVIEW FEEDBACK (verify each point is now fixed)",
        reviews,
    );
    s
}

/// A judge's system prompt: its preamble, the document format, and the one
/// rule it judges.
pub(crate) fn sys_judge(target: OutputTarget, rule: &agent::rules::JudgedRule, judgement: &str) -> String {
    format!(
        "{JUDGE_PREAMBLE}{}\n\n## THE RULE\njudgement: {judgement}\n{}\n\n{}",
        format_note(target),
        rule.title,
        rule.description
    )
}

pub(crate) fn append_reviews(s: &mut String, heading: &str, reviews: &[String]) {
    use std::fmt::Write;

    if reviews.is_empty() {
        return;
    }
    s.push_str("\n\n");
    s.push_str(heading);
    s.push('\n');
    for (i, r) in reviews.iter().enumerate() {
        let _ = write!(s, "\n### Round {}\n{r}\n", i + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    
        /// Which tools a stage may call is decided once, in the engine's catalog;
        /// `agent`'s own tests own those invariants. What this crate still has to
        /// guarantee is that each stage names a scope that resolves to a usable tool
        /// set under its target — an empty set would leave the stage unable to act.
        #[test]
        fn every_stage_resolves_to_a_non_empty_tool_set() {
            for target in [
                OutputTarget::Aem,
                OutputTarget::Redacto,
            ] {
                let roles = roles_for(target);
                for role in [roles.author, roles.reviewer, roles.judge] {
                    let tools = agent::tools_for(target, role.scope);
                    assert!(
                        !tools.is_empty(),
                        "{target:?} {} is offered no tools at all",
                        role.name
                    );
                }
            }
        }

    
        /// The stuck detector watches one tool per stage. Watching a tool the stage
        /// is never offered would silently disable the detector.
        #[test]
        fn every_stuck_tool_is_one_its_stage_is_offered() {
            for target in [
                OutputTarget::Aem,
                OutputTarget::Redacto,
            ] {
                let roles = roles_for(target);
                for role in [roles.author, roles.reviewer, roles.judge] {
                    let Some(stuck) = role.stuck_tool else {
                        continue;
                    };
                    let offered = agent::tools_for(target, role.scope);
                    assert!(
                        offered.iter().any(|t| t["name"].as_str() == Some(stuck)),
                        "{target:?} {} watches '{stuck}' but is never offered it",
                        role.name
                    );
                }
            }
            // Keyed off the role, not a hard-coded name.
            assert_eq!(AUTHOR.stuck_tool, Some("build_aem_package"));
            assert_eq!(REDACTO_AUTHOR.stuck_tool, Some("build_redacto_dump"));
        }

    
        /// The six stages must be six distinct scopes — pointing two stages at the
        /// same scope would silently give one of them the other's tools.
        #[test]
        fn the_six_stages_have_distinct_scopes() {
            let scopes = [
                AUTHOR.scope,
                REVIEWER.scope,
                JUDGE.scope,
                REDACTO_JUDGE.scope,
                REDACTO_AUTHOR.scope,
                REDACTO_REVIEWER.scope,
            ];
            for (i, a) in scopes.iter().enumerate() {
                for b in &scopes[i + 1..] {
                    assert_ne!(a, b, "two stages share a scope: {scopes:?}");
                }
            }
        }

    
        /// The two prompt families are deliberate copies, so nothing stops a
        /// copy-paste of the wrong constant. This is what catches it.
        #[test]
        fn redacto_prompts_do_not_leak_aem_vocabulary() {
            let target = OutputTarget::Redacto;
            let prompts = [
                sys_author(target, "", "", &[]),
                sys_reviewer(target, "", &[]),
            ];
    
            for prompt in &prompts {
                for leaked in [
                    "AemNodeTranslated",
                    "build_aem_package",
                    "validate_aem_package",
                    "affrg_",
                    "fragRef",
                    "wizard page",
                ] {
                    assert!(
                        !prompt.contains(leaked),
                        "the Redacto prompt must not mention '{leaked}'"
                    );
                }
                // …and must name its own vocabulary.
                assert!(prompt.contains("Redacto"), "{prompt}");
            }
            assert!(prompts[0].contains("json_patch"));
            assert!(prompts[0].contains("xfa_page_text"));
            assert!(prompts[0].contains("build_redacto_dump"));
            // The Author must be told how a layout is expressed, or the columns
            // and footnotes are flattened into plain containers.
            assert!(prompts[0].contains("layout-split"));
            assert!(prompts[0].contains("styledPanel"));
            // Every stage carries its format's schema.
            for prompt in &prompts {
                assert!(prompt.contains("UBS Redacto document"), "{prompt}");
            }

            // The AEM prompts must be untouched by the split.
            let aem = sys_author(OutputTarget::Aem, "", "", &[]);
            assert!(aem.contains("AemNodeTranslated"));
            assert!(aem.contains("UBS AEM document"));
            assert!(!aem.contains("build_redacto_dump"));
        }

    
        /// The system prompts were split per target, but the controller's own prose
        /// — stage headers, Author seeds, the output-cap nudge — was not. Telling a
        /// Redacto run to call `build_aem_package` names a tool the agent refuses,
        /// so cover every string the controller sends, not just the prompts.
        #[test]
        fn redacto_stage_prose_does_not_mention_aem_tools() {
            let roles = roles_for(OutputTarget::Redacto);
            let prose = [
                roles.author_doing,
                roles.author_seed,
                roles.author_fix_seed,
                roles.author_continue_seed,
                roles.author.max_tokens_nudge,
                roles.reviewer.max_tokens_nudge,
                roles.author.stuck_activity,
                roles.reviewer.stuck_activity,
            ];
    
            for text in prose {
                for leaked in [
                    "AEM",
                    "build_aem_package",
                    "validate_aem_package",
                    "set_aem_translated",
                    "insert_aem_translated_node",
                    "replace_aem_translated_node",
                ] {
                    assert!(
                        !text.contains(leaked),
                        "Redacto stage prose must not mention '{leaked}': {text}"
                    );
                }
            }
    
            // Every tool the seeds name must be one the Redacto Author may call.
            let offered =
                agent::tools_for(OutputTarget::Redacto, REDACTO_AUTHOR.scope);
            for tool in ["build_redacto_dump", "json_outline"] {
                assert!(
                    offered.iter().any(|t| t["name"].as_str() == Some(tool)),
                    "the Redacto Author seed names '{tool}', which it cannot call"
                );
            }
    
            // The AEM side keeps its own vocabulary.
            let aem = roles_for(OutputTarget::Aem);
            assert!(aem.author_seed.contains("build_aem_package"));
            assert!(aem.author.max_tokens_nudge.contains("/form/children"));
        }

    
        #[test]
        fn sys_author_pins_reviews() {
            let s = sys_author(
                OutputTarget::Aem,
                "",
                "",
                &["FIRST-REVIEW".into(), "SECOND-REVIEW".into()],
            );
            assert!(!s.contains("CONVERSION PLAN"));
            assert!(s.contains("## REVIEW FEEDBACK"));
            assert!(s.contains("FIRST-REVIEW"));
            assert!(s.contains("SECOND-REVIEW"));
            assert!(s.contains("Round 1") && s.contains("Round 2"));
            // The authoring body is still present.
            assert!(s.contains("AemNodeTranslated"));
        }

    }
