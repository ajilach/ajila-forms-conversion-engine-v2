//! Pipeline-stage policy: which stages a run has, how long each may run, what it
//! watches for a stall, and how its system prompt is composed.
//!
//! This is controller policy rather than engine capability — *which tools a
//! stage may call* is decided once in `agent`'s catalog (see `agent::scope`),
//! and a stage here names a scope rather than carrying its own list.

use agent::{
    ANALYST_ADDENDUM, AUTHOR_ADDENDUM, REDACTO_ANALYST_ADDENDUM, REDACTO_AUTHOR_ADDENDUM,
    REDACTO_REVIEWER_ADDENDUM, REDACTO_SHARED_PREAMBLE, REDACTO_SYSTEM_PROMPT, REVIEWER_ADDENDUM,
    SHARED_PREAMBLE, SYSTEM_PROMPT,
};

use blueprint::OutputTarget;

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

/// How many consecutive `validate_aem_package` calls with identical output
/// are allowed before a stage gives up (avoids an endless validate loop).
pub(crate) const MAX_VALIDATE_REPEATS: usize = 3;
/// How many consecutive turns that overflow the output-token cap we nudge
/// toward incremental authoring before giving up (avoids an endless loop if the
/// model keeps trying to emit one oversized call regardless).
pub(crate) const MAX_MAX_TOKEN_NUDGES: usize = 3;

/// Injected when a turn is cut off at the output-token cap — almost always
/// mid-way through one oversized tool call (a monolithic whole-tree write for a
/// large form). Steers the agent to author incrementally so no single call has
/// to fit under the output-token cap. Per target, because it names the tools the
/// target actually has.
pub(crate) const AEM_MAX_TOKENS_NUDGE: &str = "\
Your previous turn was cut off at the output-token limit before it completed — that call \
was NOT executed. This almost always means you tried to emit too much in a single tool call \
(e.g. authoring a whole large form in one set_aem_translated). Do NOT retry it as one call. \
Instead author the tree incrementally so no single call is oversized:\n\
1. Call set_aem_translated with a SMALL skeleton only: the Root plus one empty Panel per \
top-level section (titles set, no inner fields yet).\n\
2. Then fill in each section one at a time with insert_aem_translated_node (add each field / \
sub-panel into its section's Panel), replace_aem_translated_node and set_aem_translated_field.\n\
Keep every individual call small. Proceed now.";

pub(crate) const REDACTO_MAX_TOKENS_NUDGE: &str = "\
Your previous turn was cut off at the output-token limit before it completed — that call \
was NOT executed. This almost always means you tried to emit too much in a single tool call \
(e.g. authoring a whole large document in one set_structured). Do NOT retry it as one call. \
Instead author the document incrementally so no single call is oversized:\n\
1. Call set_structured with a SMALL skeleton only: one empty section per top-level heading \
(titles set, no inner content yet).\n\
2. Then fill in each section one at a time with insert_structured_node (add each paragraph / \
field into its section), replace_structured_node and set_structured_field.\n\
Keep every individual call small. Proceed now.";

// ── Roles ────────────────────────────────────────────────────────────────────

/// A pipeline stage: a name, the subset of the agent's tools it may call, and a
/// per-stage turn budget. The system prompt and seed message are supplied per
/// invocation by [`Run::execute`] (so the Analyst's plan / Reviewer reports can
/// be pinned into `system`).
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
}

pub(crate) const ANALYST: Role = Role {
    name: "Analyst",
    scope: agent::scope::AEM_ANALYST,
    max_iterations: 25,
    stuck_tool: None,
    stuck_activity: "analysis",
    max_tokens_nudge: AEM_MAX_TOKENS_NUDGE,
};

pub(crate) const AUTHOR: Role = Role {
    name: "Author",
    scope: agent::scope::AEM_AUTHOR,
    max_iterations: 110,
    stuck_tool: Some("validate_aem_package"),
    stuck_activity: "validation",
    max_tokens_nudge: AEM_MAX_TOKENS_NUDGE,
};

/// The Reviewer's budget covers a browser click-through of the deployed form
/// (one turn per page, per field group, per language), not just the package
/// checks the Redacto reviewer needs.
pub(crate) const REVIEWER: Role = Role {
    name: "Reviewer",
    scope: agent::scope::AEM_REVIEWER,
    max_iterations: 60,
    stuck_tool: Some("validate_aem_package"),
    stuck_activity: "validation",
    max_tokens_nudge: AEM_MAX_TOKENS_NUDGE,
};

// ── Redacto roles ────────────────────────────────────────────────────────────
//
// A Redacto document is text only, so these stages never touch the AEM tree.

pub(crate) const REDACTO_ANALYST: Role = Role {
    name: "Analyst",
    scope: agent::scope::REDACTO_ANALYST,
    max_iterations: 25,
    stuck_tool: None,
    stuck_activity: "analysis",
    max_tokens_nudge: REDACTO_MAX_TOKENS_NUDGE,
};

pub(crate) const REDACTO_AUTHOR: Role = Role {
    name: "Author",
    scope: agent::scope::REDACTO_AUTHOR,
    max_iterations: 110,
    stuck_tool: Some("build_redacto_dump"),
    stuck_activity: "the dump build",
    max_tokens_nudge: REDACTO_MAX_TOKENS_NUDGE,
};

pub(crate) const REDACTO_REVIEWER: Role = Role {
    name: "Reviewer",
    scope: agent::scope::REDACTO_REVIEWER,
    max_iterations: 30,
    stuck_tool: Some("build_redacto_dump"),
    stuck_activity: "the dump build",
    max_tokens_nudge: REDACTO_MAX_TOKENS_NUDGE,
};

/// The three stages for one output target.
pub(crate) struct TargetRoles {
    pub(crate) analyst: &'static Role,
    pub(crate) author: &'static Role,
    pub(crate) reviewer: &'static Role,
    /// What the Author stage header says it is doing.
    pub(crate) author_doing: &'static str,
    /// Seed message that starts a fresh Author stage.
    pub(crate) author_seed: &'static str,
    /// Seed message for an Author stage that applies review feedback.
    pub(crate) author_fix_seed: &'static str,
    /// Seed message for an Author stage carrying on from a previous run's tree.
    ///
    /// Distinct from [`Self::author_seed`], which tells the Author to *begin*
    /// from a plan it has just been given: a continuation has no plan, and
    /// authoring from scratch would throw away the tree it was seeded with.
    pub(crate) author_continue_seed: &'static str,
}

pub(crate) fn roles_for(target: OutputTarget) -> TargetRoles {
    match target {
        OutputTarget::Aem => TargetRoles {
            analyst: &ANALYST,
            author: &AUTHOR,
            reviewer: &REVIEWER,
            author_doing: "building the AEM form",
            author_seed: "Begin building the form per your CONVERSION PLAN. Author the full tree, \
                          then build_aem_package and validate_aem_package.",
            author_fix_seed: "Apply the REVIEW FEEDBACK in your instructions to the working tree, \
                              then build_aem_package and validate_aem_package.",
            author_continue_seed: "The working tree already holds what an earlier run built for \
                                   this form. Inspect it against the source with \
                                   get_aem_translated_outline, finish whatever is missing or \
                                   incomplete, then build_aem_package and validate_aem_package. \
                                   Do not start over.",
        },
        OutputTarget::Redacto => TargetRoles {
            analyst: &REDACTO_ANALYST,
            author: &REDACTO_AUTHOR,
            reviewer: &REDACTO_REVIEWER,
            author_doing: "building the Redacto document",
            author_seed: "Begin building the document per your CONVERSION PLAN. Author the full \
                          structured content, then build_redacto_dump and review_redacto_output.",
            author_fix_seed: "Apply the REVIEW FEEDBACK in your instructions to the structured \
                              content, then build_redacto_dump and review_redacto_output.",
            author_continue_seed: "The structured content already holds what an earlier run built \
                                   for this document. Inspect it against the source, finish \
                                   whatever is missing or incomplete, then build_redacto_dump and \
                                   review_redacto_output. Do not start over.",
        },
    }
}

// ── Per-role system-prompt composition (plan + reviews pinned in `system`) ─────

pub(crate) fn sys_analyst(target: OutputTarget, extra: &str) -> String {
    match target {
        OutputTarget::Aem => format!("{SHARED_PREAMBLE}{extra}\n\n{ANALYST_ADDENDUM}"),
        OutputTarget::Redacto => {
            format!("{REDACTO_SHARED_PREAMBLE}{extra}\n\n{REDACTO_ANALYST_ADDENDUM}")
        }
    }
}

/// The Author reuses the full [`SYSTEM_PROMPT`] authoring body, then the addendum,
/// then the pinned CONVERSION PLAN and every accumulated REVIEW FEEDBACK round.
pub(crate) fn sys_author(
    target: OutputTarget,
    extra: &str,
    template_note: &str,
    plan: &str,
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
    append_plan(&mut s, plan);
    append_reviews(
        &mut s,
        "## REVIEW FEEDBACK — address every point across all rounds",
        reviews,
    );
    s
}

pub(crate) fn sys_reviewer(
    target: OutputTarget,
    extra: &str,
    plan: &str,
    reviews: &[String],
) -> String {
    let mut s = match target {
        OutputTarget::Aem => format!("{SHARED_PREAMBLE}{extra}\n\n{REVIEWER_ADDENDUM}"),
        OutputTarget::Redacto => {
            format!("{REDACTO_SHARED_PREAMBLE}{extra}\n\n{REDACTO_REVIEWER_ADDENDUM}")
        }
    };
    append_plan(&mut s, plan);
    append_reviews(
        &mut s,
        "## PRIOR REVIEW FEEDBACK (verify each point is now fixed)",
        reviews,
    );
    s
}

/// Pin the Analyst's plan into a stage's system prompt.
pub(crate) fn append_plan(s: &mut String, plan: &str) {
    if plan.trim().is_empty() {
        return;
    }
    s.push_str("\n\n## CONVERSION PLAN\n");
    s.push_str(plan);
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
                for role in [roles.analyst, roles.author, roles.reviewer] {
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
                for role in [roles.analyst, roles.author, roles.reviewer] {
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
            assert_eq!(AUTHOR.stuck_tool, Some("validate_aem_package"));
            assert_eq!(ANALYST.stuck_tool, None);
            assert_eq!(REDACTO_AUTHOR.stuck_tool, Some("build_redacto_dump"));
        }

    
        /// The six stages must be six distinct scopes — pointing two stages at the
        /// same scope would silently give one of them the other's tools.
        #[test]
        fn the_six_stages_have_distinct_scopes() {
            let scopes = [
                ANALYST.scope,
                AUTHOR.scope,
                REVIEWER.scope,
                REDACTO_ANALYST.scope,
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
                sys_analyst(target, ""),
                sys_author(target, "", "", "", &[]),
                sys_reviewer(target, "", "", &[]),
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
            assert!(prompts[1].contains("seed_structured_from_state"));
            assert!(prompts[1].contains("build_redacto_dump"));
            // The Author must be pointed at the batch editor and told the seeded
            // structure is not to be re-created — without both, translating the
            // document by rebuilding it is the cheaper path and the layout is lost.
            assert!(prompts[1].contains("set_structured_fields"));
            assert!(prompts[1].contains("columnFlow"));
            // …and told where a flattened layout would show up.
            assert!(prompts[1].contains("styled_panels"));
    
            // The AEM prompts must be untouched by the split.
            let aem = sys_author(OutputTarget::Aem, "", "", "", &[]);
            assert!(aem.contains("AemNodeTranslated"));
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
                roles.analyst.max_tokens_nudge,
                roles.reviewer.max_tokens_nudge,
                roles.author.stuck_activity,
                roles.analyst.stuck_activity,
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
            for tool in ["build_redacto_dump", "review_redacto_output"] {
                assert!(
                    offered.iter().any(|t| t["name"].as_str() == Some(tool)),
                    "the Redacto Author seed names '{tool}', which it cannot call"
                );
            }
    
            // The AEM side keeps its own vocabulary.
            let aem = roles_for(OutputTarget::Aem);
            assert!(aem.author_seed.contains("build_aem_package"));
            assert!(aem.author.max_tokens_nudge.contains("set_aem_translated"));
        }

    
        #[test]
        fn sys_author_pins_plan_and_reviews() {
            let s = sys_author(
                OutputTarget::Aem,
                "",
                "",
                "PLAN-BODY-MARKER",
                &["FIRST-REVIEW".into(), "SECOND-REVIEW".into()],
            );
            assert!(s.contains("## CONVERSION PLAN"));
            assert!(s.contains("PLAN-BODY-MARKER"));
            assert!(s.contains("## REVIEW FEEDBACK"));
            assert!(s.contains("FIRST-REVIEW"));
            assert!(s.contains("SECOND-REVIEW"));
            assert!(s.contains("Round 1") && s.contains("Round 2"));
            // The authoring body is still present.
            assert!(s.contains("AemNodeTranslated"));
        }

    
        #[test]
        fn sys_analyst_has_no_plan_section_by_default() {
            let s = sys_analyst(OutputTarget::Aem, "");
            // No pinned plan section (the addendum mentions the phrase, but the
            // controller never appends a "## CONVERSION PLAN" block for the Analyst).
            assert!(!s.contains("## CONVERSION PLAN"));
            assert!(s.contains("Analyst"));
        }

    }
