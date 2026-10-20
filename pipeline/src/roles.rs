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
/// invocation by the controller (`crate::run`).
#[derive(Clone, Copy)]
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
    /// it carries on where it left off: the Author's and the Reviewer's are,
    /// across rounds and across later runs of the session.
    pub(crate) resume: bool,
    /// The terminal tool a stage must end with, when it has one it can be
    /// held to: on its last turn the stage is offered only that tool, and a
    /// note two turns before says the budget is running out. A judge that
    /// ran out of turns used to end without a verdict, leaving its rule
    /// unchecked; now it hands in what it found.
    pub(crate) wrap_up_tool: Option<&'static str>,
    /// An output-token cap of the role's own, below the model's: a turn of a
    /// role that only reads and reports never needs the model's full cap,
    /// and a runaway turn there costs the whole cap.
    pub(crate) max_output_tokens: Option<u32>,
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
    wrap_up_tool: None,
    max_output_tokens: None,
};

/// The Reviewer decides on each round's rule report: it reads the rules, the
/// document, the source and the form on its verifier to confirm a verdict
/// before it sends the form back or waives a rule. That is a few targeted
/// reads per broken rule, not a click-through of the whole form, hence a
/// budget well under the Author's.
///
/// It builds nothing and checks no rules (the pipeline hands it the rule
/// report), so it has no stall watch: its turn budget and the run's spending
/// limit bound it. It resumes its conversation, so a round sees what earlier
/// rounds reported and decided, and a rule that flips back and forth between
/// rounds shows as the conflict it is.
pub(crate) const REVIEWER: Role = Role {
    name: "Reviewer",
    scope: agent::scope::REVIEWER,
    max_iterations: 50,
    stuck_tool: None,
    stuck_activity: "the review",
    max_tokens_nudge: MAX_TOKENS_NUDGE,
    remember: true,
    resume: true,
    wrap_up_tool: None,
    max_output_tokens: None,
};

/// A judge: checks one rule the rule check handed it, reads, edits nothing,
/// and ends with `submit_rule_verdict`.
///
/// Forty turns: a judge gets the part of the document its rule reads in its
/// prompt when the rule names one (see `agent::rules::JudgeScope`), and reads
/// several things per turn otherwise. A rule that walks every page of the
/// source asks for more in its `[judge] max_turns`.
const JUDGE_TURNS: usize = 40;
/// A judge's output cap. A verdict with a long list of violations fits in a
/// few thousand tokens; one judge was seen writing 130,000 output tokens in
/// eight turns.
const JUDGE_MAX_OUTPUT_TOKENS: u32 = 16_000;
const JUDGE_NUDGE: &str = "Your previous turn was cut off at the output-token limit. Keep each \
call small: read one part of the document at a time, then call submit_rule_verdict.";

pub(crate) static JUDGE: Role = Role {
    name: "Judge",
    scope: agent::scope::JUDGE,
    max_iterations: JUDGE_TURNS,
    stuck_tool: None,
    stuck_activity: "judging",
    max_tokens_nudge: JUDGE_NUDGE,
    remember: false,
    resume: false,
    wrap_up_tool: Some("submit_rule_verdict"),
    max_output_tokens: Some(JUDGE_MAX_OUTPUT_TOKENS),
};

/// The judge role with `max_turns` instead of the default turn budget when a
/// rule asks for one. A stage runs on a `&'static Role`, so each budget a rule
/// asks for is made once and kept for the process.
pub(crate) fn judge_role(max_turns: Option<usize>) -> &'static Role {
    let base = &JUDGE;
    let Some(turns) = max_turns.filter(|t| *t != base.max_iterations) else {
        return base;
    };
    static MADE: std::sync::OnceLock<std::sync::Mutex<Vec<&'static Role>>> = std::sync::OnceLock::new();
    let mut made = MADE.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if let Some(role) = made.iter().find(|r| r.max_iterations == turns) {
        return role;
    }
    let role: &'static Role = Box::leak(Box::new(Role { max_iterations: turns, ..*base }));
    made.push(role);
    role
}

/// What the Author stage header says it is doing.
pub(crate) const AUTHOR_DOING: &str = "building the AEM form";
/// Seed message that starts a fresh Author stage.
pub(crate) const AUTHOR_SEED: &str = "Inspect the source form, then author the full form in the \
                                      document, then build_aem_package.";
/// Seed message for an Author stage that applies review feedback.
pub(crate) const AUTHOR_FIX_SEED: &str = "Apply the REVIEW FEEDBACK in your instructions to the \
                                          document, then build_aem_package.";
/// Seed message for an Author stage carrying on from a previous run's document.
///
/// Distinct from [`AUTHOR_SEED`], which tells the Author to *begin* from the
/// source: authoring from scratch would throw away the document a
/// continuation was seeded with.
pub(crate) const AUTHOR_CONTINUE_SEED: &str = "The document already holds what an earlier run \
                                               built for this form. Inspect it against the source \
                                               with json_outline, finish whatever is missing or \
                                               incomplete, then build_aem_package. \
                                               Do not start over.";

// ── Per-role prompt composition ─────────────────────────────────────────────

/// The document's format, pinned into every stage right after its role text
/// and before anything a run adds, so the part of the prompt that stays the
/// same across rounds stays one prefix.
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

/// The Reviewer's system prompt: the same in every round, so its
/// conversation (kept across rounds) reads on one cached prefix. Only what a
/// run brings with it is added: the user's feedback, which can override a
/// rule. Each round's rule report comes in that round's message
/// ([`reviewer_round`]).
pub(crate) fn sys_reviewer(extra: &str, user_feedback: Option<&str>) -> String {
    let mut s = format!("{SHARED_PREAMBLE}{extra}\n\n{REVIEWER_ADDENDUM}");
    s.push_str(&format_note());
    if let Some(feedback) = user_feedback {
        s.push_str(&format!(
            "\n\n## THE USER'S FEEDBACK — what the person asked of this run; where it overrides a \
             rule, the feedback wins\n{feedback}"
        ));
    }
    s
}

/// One Reviewer round's message: the rule report on the Author's latest
/// document, the Author's summary of its round (`None` when it ended without
/// handing over), and how many Author rounds are left to send the form back
/// to.
pub(crate) fn reviewer_round(round: usize, rule_report: &str, author_summary: Option<&str>, rounds_left: usize) -> String {
    let summary = author_summary.map_or_else(
        || "The Author ended without finish_authoring: it did not verify this form itself.".to_string(),
        str::to_string,
    );
    let left = match rounds_left {
        0 => "No Author round is left: accept the form with a waiver for every broken rule, or send it \
              back (approved=false) to end the run with those rules left broken for a person."
            .to_string(),
        1 => "1 Author round is left.".to_string(),
        n => format!("{n} Author rounds are left."),
    };
    format!(
        "# ROUND {round}\n\n## THE RULE REPORT — every rule, checked on the Author's latest document\n\
         {rule_report}\n\n## THE AUTHOR'S SUMMARY\n{summary}\n\n## ROUNDS\n{left}\n\nDecide on every \
         rule the report shows negative or without a verdict, then call submit_review."
    )
}

/// A judge's system prompt: its preamble, the document format, the one rule
/// it judges and, after it, what it was handed to read (`handed`, already
/// written as prompt sections, or empty).
/// A judge's system prompt: the same for every judge of a run, rule or not,
/// so the judges share one cached prefix (the tools and this prompt) instead
/// of each writing its own. What a judge is to judge comes in its first
/// message ([`judge_brief`]).
pub(crate) fn sys_judge() -> String {
    format!("{JUDGE_PREAMBLE}{}", format_note())
}

/// A judge's first message: its turn budget, its rule with the judgement id
/// its verdict goes under, and what it is handed of the document and the
/// source.
pub(crate) fn judge_brief(rule: &agent::rules::JudgedRule, judgement: &str, handed: &str, turns: usize) -> String {
    format!(
        "{}\n\n## THE RULE\njudgement: {judgement}\n{}\n\n{}{handed}\n\nJudge the rule \"{}\", then call \
         submit_rule_verdict with judgement {judgement}.",
        judge_budget_note(turns),
        rule.title,
        rule.description,
        rule.title,
    )
}

/// What a judge is told about its turn budget, in its first message: the
/// conversation keeps it from the first turn, and the last turn only narrows
/// the tools (see `crate::hooks::WrapUp`).
fn judge_budget_note(turns: usize) -> String {
    format!(
        "You have {turns} turns. On your last one only submit_rule_verdict is offered, so plan to \
         have looked at what matters by then. If you get there without having checked everything \
         the rule covers, still submit: one violation per break you found (pass=false), plus one \
         violation per part you did not get to, its message starting with \"{}\" and naming that \
         part. pass=true only when you checked everything and found no break.",
        crate::hooks::NOT_CHECKED_PREFIX
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

    /// A rule's own turn budget makes a judge role of its own, once; the
    /// default budget is the judge role itself.
    #[test]
    fn a_rule_can_ask_for_its_own_judge_turns() {
        let base = &JUDGE;
        assert_eq!(base.max_iterations, JUDGE_TURNS);
        assert!(std::ptr::eq(judge_role(None), base));
        assert!(std::ptr::eq(judge_role(Some(JUDGE_TURNS)), base));
        let wide = judge_role(Some(14));
        assert_eq!((wide.max_iterations, wide.scope), (14, base.scope));
        assert!(std::ptr::eq(judge_role(Some(14)), wide), "made once");
    }

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

    /// A stage held to its terminal call on its last turn must be offered
    /// that call, or its last turn offers nothing.
    #[test]
    fn every_wrap_up_tool_is_one_its_stage_is_offered() {
        for role in [&AUTHOR, &REVIEWER, &JUDGE] {
            let Some(tool) = role.wrap_up_tool else {
                continue;
            };
            let offered = agent::tools_for(role.scope);
            assert!(
                offered.iter().any(|t| t["name"].as_str() == Some(tool)),
                "{} wraps up with '{tool}' but is never offered it",
                role.name
            );
        }
        assert_eq!(JUDGE.wrap_up_tool, Some("submit_rule_verdict"));
    }

    /// The Reviewer's system prompt is the same every round, so a kept
    /// conversation reads on one cached prefix: the round's report goes into
    /// the round's message, and only the user's feedback is pinned.
    #[test]
    fn the_reviewer_gets_its_report_per_round_and_the_feedback_pinned() {
        const { assert!(REVIEWER.remember && REVIEWER.resume, "the Reviewer keeps its conversation") };
        let plain = sys_reviewer("", None);
        assert!(!plain.contains("THE RULE REPORT —") && !plain.contains("USER'S FEEDBACK"), "{plain}");
        let fed = sys_reviewer("", Some("Make IBAN mandatory."));
        assert!(fed.starts_with(&plain) && fed.contains("## THE USER'S FEEDBACK") && fed.contains("Make IBAN mandatory."));

        let round = reviewer_round(2, "{\"verdicts\":[]}", Some("built page 1"), 3);
        assert!(round.contains("# ROUND 2") && round.contains("## THE RULE REPORT") && round.contains("{\"verdicts\":[]}"));
        assert!(round.contains("built page 1") && round.contains("3 Author rounds are left") && round.contains("submit_review"));
        assert!(reviewer_round(1, "r", None, 1).contains("1 Author round is left"));
        let last = reviewer_round(4, "r", None, 0);
        assert!(last.contains("No Author round is left") && last.contains("without finish_authoring"), "{last}");
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
        for tool in ["build_aem_package", "rule_list", "rule_get", "finish_authoring", "json_outline", "json_patch"] {
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
            scope: Default::default(),
        };
        for prompt in [sys_author("", "", &[]), sys_reviewer("", None), sys_judge()] {
            assert!(prompt.contains("UBS AEM document"), "{prompt}");
        }
        // Nothing of the rule is in the judges' shared prompt; all of it is
        // in the brief.
        assert!(!sys_judge().contains("judgement-1"));
        let brief = judge_brief(&rule, "judgement-1", "\n\n## HANDED", 8);
        assert!(brief.contains("judgement: judgement-1") && brief.contains("## HANDED") && brief.contains("8 turns"));
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
