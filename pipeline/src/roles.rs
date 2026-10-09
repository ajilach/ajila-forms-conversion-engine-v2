//! Pipeline-stage policy: which stages a run has, how long each may run, what it
//! watches for a stall, and how its system prompt is composed.
//!
//! This is controller policy rather than engine capability — *which tools a
//! stage may call* is decided once in `agent`'s catalog (see `agent::scope`),
//! and a stage here names a scope rather than carrying its own list.

use agent::{AUTHOR_ADDENDUM, JUDGE_PREAMBLE, REVIEWER_ADDENDUM, SHARED_PREAMBLE, SYSTEM_PROMPT};

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
/// to fit under the output-token cap.
pub(crate) const MAX_TOKENS_NUDGE: &str = "\
Your previous turn was cut off at the output-token limit before it completed — that call \
was NOT executed. This almost always means you tried to emit too much in a single tool call \
(e.g. authoring a whole large form in one json_patch). Do NOT retry it as one call. \
Instead author the form incrementally so no single call is oversized:\n\
1. json_patch a SMALL skeleton only: one empty Panel per top-level section under \
`/form/children` (titles set, no inner fields yet).\n\
2. Then fill in each section one at a time with further json_patch calls, each adding the \
fields and sub-panels of one section to its Panel's `children`.\n\
Keep every individual call small. Proceed now.";

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
    /// tools this role actually has.
    pub(crate) max_tokens_nudge: &'static str,
    /// Whether the stage's conversation is stored under the session. A judge's
    /// is not: it is one-shot, and judges run in parallel, so one stored
    /// conversation would mix them.
    pub(crate) remember: bool,
    /// Whether a stored conversation is loaded when the stage runs again, so
    /// it carries on where it left off. The Reviewer's is stored (for a
    /// diagnosis) but never loaded: every round reviews from scratch.
    pub(crate) resume: bool,
}

pub(crate) const AUTHOR: Role = Role {
    name: "Author",
    scope: agent::scope::AUTHOR,
    max_iterations: 200,
    stuck_tool: Some("build_aem_package"),
    stuck_activity: "the package build",
    max_tokens_nudge: MAX_TOKENS_NUDGE,
    remember: true,
    resume: true,
};

/// The Reviewer's budget covers a browser click-through of the deployed form
/// (one turn per page, per field group, per language) and driving every source
/// control the form's scripts read, not just the package checks.
///
/// It builds nothing, so its stall watch is on rule_check instead: the
/// document cannot change under it, so the same report three times running
/// means it is going in circles. It resumes nothing either: every round starts
/// fresh from the source, unbiased by how an earlier round (or the Author) saw
/// the form; the prior reports pinned in its system prompt are all it carries
/// over.
pub(crate) const REVIEWER: Role = Role {
    name: "Reviewer",
    scope: agent::scope::REVIEWER,
    max_iterations: 90,
    stuck_tool: Some("rule_check"),
    stuck_activity: "the rule check",
    max_tokens_nudge: MAX_TOKENS_NUDGE,
    remember: true,
    resume: false,
};

/// A judge: checks one rule `rule_check` handed it, reads, edits nothing, and
/// ends with `submit_rule_verdict`.
const JUDGE_NUDGE: &str = "Your previous turn was cut off at the output-token limit. Keep each \
call small: read one part of the document at a time, then call submit_rule_verdict.";

pub(crate) const JUDGE: Role = Role {
    name: "Judge",
    scope: agent::scope::JUDGE,
    max_iterations: 20,
    stuck_tool: None,
    stuck_activity: "judging",
    max_tokens_nudge: JUDGE_NUDGE,
    remember: false,
    resume: false,
};

/// What the Author stage header says it is doing.
pub(crate) const AUTHOR_DOING: &str = "building the AEM form";
/// Seed message that starts a fresh Author stage.
pub(crate) const AUTHOR_SEED: &str = "Inspect the source form, then author the full form in the \
                                      document, then rule_check and build_aem_package.";
/// Seed message for an Author stage that applies review feedback.
pub(crate) const AUTHOR_FIX_SEED: &str = "Apply the REVIEW FEEDBACK in your instructions to the \
                                          document, then rule_check and build_aem_package.";
/// Seed message for an Author stage carrying on from a previous run's document.
///
/// Distinct from [`AUTHOR_SEED`], which tells the Author to *begin* from the
/// source: authoring from scratch would throw away the document a
/// continuation was seeded with.
pub(crate) const AUTHOR_CONTINUE_SEED: &str = "The document already holds what an earlier run \
                                               built for this form. Inspect it against the source \
                                               with json_outline, finish whatever is missing or \
                                               incomplete, then rule_check and build_aem_package. \
                                               Do not start over.";

// ── Per-role system-prompt composition (reviews pinned in `system`) ──────────

/// The document's format, pinned into every stage right after its role text
/// and before the reviews, so the part of the prompt that stays the same across
/// rounds stays one prefix.
fn format_note() -> String {
    format!("\n\n{}", agent::conversion::document_format())
}

/// The Author reuses the full [`SYSTEM_PROMPT`] authoring body, then the addendum,
/// then every accumulated REVIEW FEEDBACK round.
pub(crate) fn sys_author(extra: &str, template_note: &str, reviews: &[String]) -> String {
    let mut s = format!("{SYSTEM_PROMPT}{extra}{template_note}\n\n{AUTHOR_ADDENDUM}");
    s.push_str(&format_note());
    append_reviews(
        &mut s,
        "## REVIEW FEEDBACK — address every point across all rounds",
        reviews,
    );
    s
}

pub(crate) fn sys_reviewer(extra: &str, reviews: &[String]) -> String {
    let mut s = format!("{SHARED_PREAMBLE}{extra}\n\n{REVIEWER_ADDENDUM}");
    s.push_str(&format_note());
    append_reviews(
        &mut s,
        "## PRIOR REVIEW FEEDBACK (points to re-verify; review the whole form regardless)",
        reviews,
    );
    s
}

/// A judge's system prompt: its preamble, the document format, and the one
/// rule it judges.
pub(crate) fn sys_judge(rule: &agent::rules::JudgedRule, judgement: &str) -> String {
    format!(
        "{JUDGE_PREAMBLE}{}\n\n## THE RULE\njudgement: {judgement}\n{}\n\n{}",
        format_note(),
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

    /// Only the judges' calls leave the dispatching stage's evidence alone.
    #[test]
    fn only_judges_record_no_evidence() {
        use crate::run::stage_caller;
        use agent::Caller;
        assert_eq!(stage_caller(&AUTHOR), Caller::Stage);
        assert_eq!(stage_caller(&REVIEWER), Caller::Stage);
        assert_eq!(stage_caller(&JUDGE), Caller::Judge);
    }

    /// Which tools a stage may call is decided once, in the engine's catalog;
    /// `agent`'s own tests own those invariants. What this crate still has to
    /// guarantee is that each stage names a scope that resolves to a usable tool
    /// set: an empty set would leave the stage unable to act.
    #[test]
    fn every_stage_resolves_to_a_non_empty_tool_set() {
        for role in [&AUTHOR, &REVIEWER, &JUDGE] {
            assert!(!agent::tools_for(role.scope).is_empty(), "{} is offered no tools at all", role.name);
        }
    }

    /// The stuck detector watches one tool per stage. Watching a tool the stage
    /// is never offered would silently disable the detector.
    #[test]
    fn every_stuck_tool_is_one_its_stage_is_offered() {
        for role in [&AUTHOR, &REVIEWER, &JUDGE] {
            let Some(stuck) = role.stuck_tool else {
                continue;
            };
            let offered = agent::tools_for(role.scope);
            assert!(
                offered.iter().any(|t| t["name"].as_str() == Some(stuck)),
                "{} watches '{stuck}' but is never offered it",
                role.name
            );
        }
        // Keyed off the role, not a hard-coded name.
        assert_eq!(AUTHOR.stuck_tool, Some("build_aem_package"));
    }

    /// The stages must be distinct scopes — pointing two stages at the
    /// same scope would silently give one of them the other's tools.
    #[test]
    fn the_stages_have_distinct_scopes() {
        let scopes = [AUTHOR.scope, REVIEWER.scope, JUDGE.scope];
        for (i, a) in scopes.iter().enumerate() {
            for b in &scopes[i + 1..] {
                assert_ne!(a, b, "two stages share a scope: {scopes:?}");
            }
        }
    }

    /// Every tool the controller's own prose names (the Author seeds, the
    /// output-cap nudge) must be one the Author may call, or the stage spends
    /// a turn on a refusal.
    #[test]
    fn the_author_prose_only_names_tools_the_author_is_offered() {
        let offered = agent::tools_for(AUTHOR.scope);
        for tool in ["build_aem_package", "rule_check", "json_outline", "json_patch"] {
            assert!(
                offered.iter().any(|t| t["name"].as_str() == Some(tool)),
                "the Author prose names '{tool}', which it cannot call"
            );
        }
        assert!(AUTHOR_SEED.contains("build_aem_package"));
        assert!(AUTHOR_CONTINUE_SEED.contains("json_outline"));
        assert!(AUTHOR.max_tokens_nudge.contains("/form/children"));
    }

    /// Every stage carries the document format's schema.
    #[test]
    fn every_stage_prompt_carries_the_document_format() {
        let rule = agent::rules::JudgedRule {
            id: "r".into(),
            name: "n".into(),
            title: "t".into(),
            description: "d".into(),
        };
        for prompt in [sys_author("", "", &[]), sys_reviewer("", &[]), sys_judge(&rule, "judgement-1")] {
            assert!(prompt.contains("UBS AEM document"), "{prompt}");
        }
    }

    #[test]
    fn sys_author_pins_reviews() {
        let s = sys_author("", "", &["FIRST-REVIEW".into(), "SECOND-REVIEW".into()]);
        assert!(!s.contains("CONVERSION PLAN"));
        assert!(s.contains("## REVIEW FEEDBACK"));
        assert!(s.contains("FIRST-REVIEW"));
        assert!(s.contains("SECOND-REVIEW"));
        assert!(s.contains("Round 1") && s.contains("Round 2"));
        // The authoring body is still present.
        assert!(s.contains("AemNodeTranslated"));
    }
}
