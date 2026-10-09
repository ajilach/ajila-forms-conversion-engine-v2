//! `rule_check` with judges: the scripted rules run their scripts, and each
//! judged rule (a rule no script decides, see `agent::rules`) is handed to a
//! judge agent, several at once. The agent crate has no model, so this is
//! where the one `rule_check` a stage calls becomes both.
//!
//! A judge is a stage of its own ([`crate::roles::JUDGE`]) on the run's model:
//! it reads the document and the source, edits nothing, and ends with
//! `submit_rule_verdict`. Its stage is one-shot (no stored conversation), it
//! never prompts the operator (a judge that fails is reported unchecked), and
//! its spend is folded into the stage that dispatched it.
//!
//! The trace gets the check as a whole ([`TraceEvent::RuleCheckStarted`] and
//! [`TraceEvent::RuleCheckFinished`]) and one [`TraceEvent::JudgeFinished`] per
//! judge, carrying what the judge decided and what it took: a judge's own
//! turns are not traced, so these are what the run analysis counts judges by.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use agent::rules::{JudgedRule, RuleVerdict, merge_rule_report};
use agent::{Caller, ToolReply};
use futures_util::StreamExt;

use crate::observer::{RunEvent, SharedObserver, Spend};
use crate::substage::{SubStageContext, run_sub_stage};
use crate::tools::SharedAgent;
use crate::trace::{self, JudgeVerdict, TraceEvent};

/// How many judges run at once.
const JUDGES_AT_ONCE: usize = 4;

/// Why a judged rule has no verdict when the document was edited while it was
/// judged.
const CHANGED_WHILE_JUDGED: &str = "the document changed while it was judged: check this rule again";

/// Numbers every `rule_check` of the process, so a judge's trace line names
/// the check it belongs to even when two checks overlap.
static CHECKS: AtomicU64 = AtomicU64::new(0);

/// Tells the observer where every rule stands now.
pub(crate) async fn report_rules(agent: &SharedAgent, obs: &SharedObserver) {
    let board = agent.lock().await.rule_board();
    obs.emit(RunEvent::Rules(board));
}

/// One `rule_check`: the scripts while holding the agent, then the judges
/// without it (each judge's tools take the agent call by call). A judged rule
/// that already has a verdict on the document's content, from any earlier
/// check of the run, keeps it and sends no judge (see
/// [`agent::ConversionAgent::cached_verdict`]). The trace counts only the
/// judges the check sent: a reused verdict costs nothing and traces nothing.
pub(crate) async fn rule_check(agent: &SharedAgent, ctx: &SubStageContext, input: &serde_json::Value) -> ToolReply {
    let started = Instant::now();
    let (plan, scripted, revision, content, cached) = {
        let mut guard = agent.lock().await;
        let plan = match guard.rule_check_plan(input) {
            Ok(plan) => plan,
            Err(e) => return ToolReply::Error(e),
        };
        let scripted = match guard.check_scripted(&plan).await {
            Ok(scripted) => scripted,
            Err(e) => return ToolReply::Error(e),
        };
        let (revision, content) = (guard.revision(), guard.content_hash());
        let cached: Vec<Option<RuleVerdict>> =
            plan.judged.iter().map(|rule| guard.cached_verdict(rule, content)).collect();
        let reused: Vec<(JudgedRule, Result<RuleVerdict, String>)> = plan
            .judged
            .iter()
            .zip(&cached)
            .filter_map(|(rule, verdict)| Some((rule.clone(), Ok(verdict.clone()?))))
            .collect();
        guard.record_judged(&reused, revision);
        (plan, scripted, revision, content, cached)
    };
    report_rules(agent, &ctx.obs).await;
    let to_judge: Vec<JudgedRule> = plan
        .judged
        .iter()
        .zip(&cached)
        .filter(|(_, verdict)| verdict.is_none())
        .map(|(rule, _)| rule.clone())
        .collect();
    let check = CHECKS.fetch_add(1, Ordering::Relaxed) + 1;
    let stage = judge_stage(ctx);
    ctx.obs.trace(TraceEvent::RuleCheckStarted {
        stage: stage.clone(),
        check,
        partial: is_partial(input),
        scripted: scripted.get("verdicts").and_then(serde_json::Value::as_array).map_or(0, Vec::len),
        judged: to_judge.len(),
        revision,
    });
    // `buffered`, not `buffer_unordered`: the report keeps the rules' order.
    let judged: Vec<Judged> = futures_util::stream::iter(to_judge)
        .map(|rule| judge(agent, ctx, rule, revision, content, check))
        .buffered(JUDGES_AT_ONCE)
        .collect()
        .await;
    let mut judges_spend = Spend::default();
    for one in &judged {
        judges_spend.merge(&one.spend);
    }
    let mut judged = judged.into_iter().map(|one| (one.rule, one.outcome));
    let mut verdicts: Vec<(JudgedRule, Result<RuleVerdict, String>)> = plan
        .judged
        .into_iter()
        .zip(&cached)
        .map(|(rule, verdict)| match verdict {
            Some(verdict) => (rule, Ok(verdict.clone())),
            None => judged.next().expect("a judge ran for every rule without a cached verdict"),
        })
        .collect();
    // Each judge put its outcome on the board for `revision`, which an edit
    // made since (another call of the same turn) shows as outdated. The model
    // is told to check again instead: a verdict on an older document is not
    // one on the document it now has.
    let outdated = agent.lock().await.revision() != revision;
    if outdated {
        for (_, outcome) in &mut verdicts {
            *outcome = Err(CHANGED_WHILE_JUDGED.into());
        }
    }
    ctx.obs.trace(TraceEvent::RuleCheckFinished {
        stage,
        check,
        duration_ms: trace::elapsed_ms(started),
        judges_spend,
        outdated,
    });
    let mut report = merge_rule_report(scripted, &verdicts);
    mark_reused(&mut report, &verdicts, &cached);
    ToolReply::Text(report.to_string())
}

/// Marks each judged verdict of `report` that came from the cache rather than
/// a judge of this check with `"cached": true`, so whoever reads the report
/// (the model, the run's analysis) sees which verdicts cost a judge.
fn mark_reused(
    report: &mut serde_json::Value,
    verdicts: &[(JudgedRule, Result<RuleVerdict, String>)],
    cached: &[Option<RuleVerdict>],
) {
    let reused: Vec<&str> = verdicts
        .iter()
        .zip(cached)
        .filter(|((_, outcome), verdict)| outcome.is_ok() && verdict.is_some())
        .map(|((rule, _), _)| rule.id.as_str())
        .collect();
    let Some(entries) = report.get_mut("verdicts").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    for entry in entries.iter_mut().filter(|e| e["check"] == "agent") {
        if entry["rule_id"].as_str().is_some_and(|id| reused.contains(&id)) {
            entry["cached"] = serde_json::Value::Bool(true);
        }
    }
}

/// The judges' stage name, which their trace lines carry.
fn judge_stage(ctx: &SubStageContext) -> String {
    crate::roles::roles_for(ctx.target).judge.name.to_string()
}

/// Whether `rule_check` was asked for chosen rules rather than all of them.
fn is_partial(input: &serde_json::Value) -> bool {
    input.get("rule_ids").and_then(serde_json::Value::as_array).is_some_and(|ids| !ids.is_empty())
}

/// One judge's outcome and what it spent.
struct Judged {
    rule: JudgedRule,
    outcome: Result<RuleVerdict, String>,
    spend: Spend,
}

/// Runs one judge on `rule`, dispatched on `revision` (whose content hashed to
/// `content`), and puts its outcome on the board as soon as it ends, before
/// the rule stops showing as judged: a check's judges end one by one, and a
/// check cut short keeps what its ended judges found. A verdict on a document
/// nobody edited meanwhile is kept for the next check of the same content.
async fn judge(
    agent: &SharedAgent,
    ctx: &SubStageContext,
    rule: JudgedRule,
    revision: u64,
    content: u64,
    check: u64,
) -> Judged {
    ctx.obs.emit(RunEvent::Judging { rule_id: rule.id.clone(), running: true });
    let _ended = JudgingEnds { obs: &ctx.obs, rule_id: rule.id.clone() };
    let started = Instant::now();
    let (outcome, turns, spend) = judge_rule(agent, ctx, &rule).await;
    let duration_ms = trace::elapsed_ms(started);
    let mut guard = agent.lock().await;
    // The judge read the live document. An edit made while it did means its
    // verdict may describe neither revision.
    let outcome = if guard.revision() == revision { outcome } else { Err(CHANGED_WHILE_JUDGED.into()) };
    if let Ok(verdict) = &outcome {
        guard.cache_verdict(&rule, content, verdict.clone());
    }
    let judged = (rule, outcome);
    guard.record_judged(std::slice::from_ref(&judged), revision);
    let board = guard.rule_board();
    drop(guard);
    let (rule, outcome) = judged;
    let judged = Judged { rule, outcome, spend };
    ctx.obs.trace(judge_finished(ctx, check, &judged, turns, duration_ms, revision));
    ctx.obs.emit(RunEvent::Rules(board));
    judged
}

/// The trace line of one ended judge.
fn judge_finished(
    ctx: &SubStageContext,
    check: u64,
    judged: &Judged,
    turns: usize,
    duration_ms: u64,
    revision: u64,
) -> TraceEvent {
    let rule = &judged.rule;
    let (verdict, violations, unchecked_reason) = match &judged.outcome {
        Ok(v) if v.pass => (JudgeVerdict::Positive, 0, None),
        Ok(v) => (JudgeVerdict::Negative, v.violations.len(), None),
        Err(reason) => (JudgeVerdict::Unchecked, 0, Some(reason.clone())),
    };
    TraceEvent::JudgeFinished {
        stage: judge_stage(ctx),
        check,
        rule_id: rule.id.clone(),
        rule_name: rule.name.clone(),
        rule_title: rule.title.clone(),
        verdict,
        violations,
        unchecked_reason,
        turns,
        duration_ms,
        revision,
        spend: judged.spend,
    }
}

/// Reports a judge's end however its future ends, a dropped one included, so
/// no rule is left shown as being judged.
struct JudgingEnds<'a> {
    obs: &'a SharedObserver,
    rule_id: String,
}

impl Drop for JudgingEnds<'_> {
    fn drop(&mut self) {
        self.obs.emit(RunEvent::Judging { rule_id: std::mem::take(&mut self.rule_id), running: false });
    }
}

/// Runs the judge, and returns its verdict with the turns it took and what it
/// spent.
async fn judge_rule(
    agent: &SharedAgent,
    ctx: &SubStageContext,
    rule: &JudgedRule,
) -> (Result<RuleVerdict, String>, usize, Spend) {
    let role = crate::roles::roles_for(ctx.target).judge;
    let judgement = agent.lock().await.open_judgement();
    let end = run_sub_stage(
        agent,
        ctx,
        role,
        &crate::roles::sys_judge(ctx.target, rule, &judgement),
        &format!("Judge the rule \"{}\", then call submit_rule_verdict with judgement {judgement}.", rule.title),
        &Caller::Judge,
        format!("judge: {}", rule.title),
    )
    .await;
    let verdict = agent.lock().await.take_judgement(&judgement).ok_or_else(|| end.why_no("judge", "a verdict"));
    (verdict, end.turns, end.spend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::observer::{AbortFlag, RetryAction, RunObserver};
    use crate::substage::test_support;
    use rig_core::completion::Usage;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    fn rule(name: &str) -> JudgedRule {
        JudgedRule {
            id: format!("id-{name}"),
            name: name.into(),
            title: format!("Rule {name}"),
            description: "Judge me.".into(),
        }
    }

    /// A Redacto agent holding `rules` as its judged ones.
    fn agent_with(rules: Vec<JudgedRule>) -> SharedAgent {
        test_support::agent_with_judged(rules)
    }

    fn context(model: MockCompletionModel, abort: AbortFlag) -> SubStageContext {
        test_support::context(model, abort)
    }

    /// The verdicts of the judged rules in `report`: the Redacto target has scripted rules too,
    /// whose verdicts stand beside them.
    fn judged(report: &serde_json::Value) -> Vec<&serde_json::Value> {
        report["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["check"] == "agent")
            .collect()
    }

    fn report(reply: ToolReply) -> serde_json::Value {
        match reply {
            ToolReply::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("rule_check failed: {other:?}"),
        }
    }

    /// A judged rule goes to a judge, whose verdict lands in the report like a
    /// scripted rule's, and whose spend the dispatching stage gets to fold in.
    #[tokio::test]
    async fn a_judged_rule_is_judged_and_its_verdict_reported() {
        let mut usage = Usage::new();
        usage.input_tokens = 100;
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::tool_call(
                "verdict",
                "submit_rule_verdict",
                serde_json::json!({"judgement": "judgement-1", "pass": false,
                    "violations": [{"pointer": "/body/0", "message": "split the table"}]}),
            ),
            MockStreamEvent::final_response(usage),
        ]]);
        let ctx = context(model.clone(), AbortFlag::default());
        let report = report(rule_check(&agent_with(vec![rule("a")]), &ctx, &serde_json::json!({})).await);

        let verdict = judged(&report)[0];
        assert_eq!(verdict["rule_id"], "id-a");
        assert_eq!(verdict["check"], "agent");
        assert_eq!(verdict["verdict"], "negative");
        assert_eq!(verdict["violations"][0]["message"], "split the table");
        // The judge was told its rule.
        let system = match model.requests()[0].chat_history.first() {
            Some(rig_core::message::Message::System { content }) => content.clone(),
            _ => String::new(),
        };
        assert!(system.contains("judgement-1") && system.contains("Rule a") && system.contains("submit_rule_verdict"), "{system}");
        assert_eq!(ctx.spend.lock().unwrap().input_tokens, 100);
    }

    /// Records every event a check reports, and its trace.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<RunEvent>>>, Arc<Mutex<Vec<TraceEvent>>>);
    impl RunObserver for Recorder {
        fn emit(&mut self, event: RunEvent) {
            self.0.lock().unwrap().push(event);
        }
        fn trace(&mut self, event: TraceEvent) {
            self.1.lock().unwrap().push(event);
        }
        fn retry_prompt(&mut self, _role: &str, _error: &str) {}
        fn poll_retry(&mut self) -> Option<RetryAction> {
            Some(RetryAction::Cancel)
        }
        fn retry_resolved(&mut self, _action: RetryAction) {}
    }

    /// The observer watches the check: the scripts' verdicts first, the rule
    /// judged between its judge's start and end, and the board with the
    /// judge's verdict last, current for the document it was judged on and
    /// reported before the judging ends, so the rule never falls back to its
    /// old state in between.
    #[tokio::test]
    async fn a_check_reports_the_judging_and_the_board() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::tool_call(
                "verdict",
                "submit_rule_verdict",
                serde_json::json!({"judgement": "judgement-1", "pass": true, "violations": []}),
            ),
            MockStreamEvent::final_response(Usage::new()),
        ]]);
        let recorder = Recorder::default();
        let mut ctx = context(model, AbortFlag::default());
        ctx.obs = SharedObserver::new(recorder.clone());
        let agent = agent_with(vec![rule("a")]);
        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);

        let events = recorder.0.lock().unwrap().clone();
        let judging: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::Judging { rule_id, running } => Some((rule_id.as_str(), *running)),
                _ => None,
            })
            .collect();
        assert_eq!(judging, [("id-a", true), ("id-a", false)]);
        let boards: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::Rules(board) => Some(board),
                _ => None,
            })
            .collect();
        assert_eq!(boards.len(), 2, "one board after the scripts, one after the judges");
        let judged_in = |board: &Vec<agent::RuleView>| board.iter().find(|r| r.rule_id == "id-a").unwrap().clone();
        assert_eq!(judged_in(boards[0]).state, agent::RuleState::NotChecked);
        let last = judged_in(boards[1]);
        assert_eq!((last.state, last.outdated), (agent::RuleState::Pass, false));
        // The scripted rules got their verdicts too.
        assert!(boards[1].iter().filter(|r| r.kind == agent::RuleKind::Script).all(|r| r.state != agent::RuleState::NotChecked));
        assert_eq!(agent.lock().await.rule_board(), *boards[1]);
        let position = |wanted: &dyn Fn(&RunEvent) -> bool| events.iter().position(wanted).unwrap();
        let judged_on_board = position(&|e| {
            matches!(e, RunEvent::Rules(board) if board.iter().any(|r| r.rule_id == "id-a" && r.state == agent::RuleState::Pass))
        });
        let judging_ended = position(&|e| matches!(e, RunEvent::Judging { running: false, .. }));
        assert!(judged_on_board < judging_ended, "the verdict reached the board only after the judging ended");
    }

    /// Every judge is traced as a whole inside its check: the rule, its
    /// verdict, the turns it took and what it spent, which
    /// is exactly what the dispatching stage gets to fold in.
    #[tokio::test]
    async fn a_judge_is_traced_with_its_verdict_turns_and_spend() {
        let turn = |judgement: &str| {
            let mut usage = Usage::new();
            usage.input_tokens = 100;
            usage.output_tokens = 10;
            vec![
                MockStreamEvent::tool_call(
                    "verdict",
                    "submit_rule_verdict",
                    serde_json::json!({"judgement": judgement, "pass": false,
                        "violations": [{"pointer": "/body/0", "message": "split the table"}]}),
                ),
                MockStreamEvent::final_response(usage),
            ]
        };
        // A refused verdict first: the judge takes two turns.
        let model = MockCompletionModel::from_stream_turns([turn("judgement-9"), turn("judgement-1")]);
        let recorder = Recorder::default();
        let mut ctx = context(model, AbortFlag::default());
        ctx.obs = SharedObserver::new(recorder.clone());
        let agent = agent_with(vec![rule("a")]);
        let revision = agent.lock().await.revision();
        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);

        let traced = recorder.1.lock().unwrap().clone();
        assert_eq!(traced.len(), 3, "{traced:?}");
        let TraceEvent::RuleCheckStarted { stage, check, partial, judged, scripted, revision: on, .. } = &traced[0] else {
            panic!("the check opens the trace: {traced:?}");
        };
        assert_eq!((stage.as_str(), *partial, *judged, *on), ("Judge", false, 1, revision));
        assert!(*scripted > 0, "the Redacto target has scripted rules too");
        let TraceEvent::JudgeFinished {
            check: judge_check,
            rule_id,
            rule_name,
            verdict,
            violations,
            unchecked_reason,
            turns,
            revision: judged_on,
            spend,
            ..
        } = &traced[1]
        else {
            panic!("one line per judge: {traced:?}");
        };
        assert_eq!(judge_check, check);
        assert_eq!((rule_id.as_str(), rule_name.as_str()), ("id-a", "a"));
        assert_eq!((*verdict, *violations, unchecked_reason.clone()), (JudgeVerdict::Negative, 1, None));
        assert_eq!((*turns, *judged_on), (2, revision));
        assert_eq!((spend.input_tokens, spend.output_tokens), (200, 20));
        assert!((spend.cost_usd.unwrap() - 2.0).abs() < 1e-9);
        let TraceEvent::RuleCheckFinished { check: finished, judges_spend, outdated, .. } = &traced[2] else {
            panic!("the check closes the trace: {traced:?}");
        };
        assert_eq!((finished, *outdated), (check, false));
        assert_eq!(judges_spend, spend);
        assert_eq!(*ctx.spend.lock().unwrap(), *spend, "the stage folds in what the judges were traced with");
    }

    /// A check of chosen rules says so, and a judge without a verdict is
    /// traced as unchecked with the reason.
    #[tokio::test]
    async fn a_partial_check_and_an_unchecked_judge_are_traced_as_such() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("I think it is fine."),
            MockStreamEvent::final_response(Usage::new()),
        ]]);
        let recorder = Recorder::default();
        let mut ctx = context(model, AbortFlag::default());
        ctx.obs = SharedObserver::new(recorder.clone());
        report(rule_check(&agent_with(vec![rule("a")]), &ctx, &serde_json::json!({"rule_ids": ["id-a"]})).await);

        let traced = recorder.1.lock().unwrap().clone();
        assert!(matches!(&traced[0], TraceEvent::RuleCheckStarted { partial: true, judged: 1, .. }), "{traced:?}");
        let TraceEvent::JudgeFinished { verdict, unchecked_reason, turns, .. } = &traced[1] else {
            panic!("{traced:?}");
        };
        assert_eq!(*verdict, JudgeVerdict::Unchecked);
        assert_eq!(unchecked_reason.as_deref(), Some("the judge ended without a verdict"));
        assert_eq!(*turns, 1);
    }

    /// A verdict under a judgement nobody opened, or with violations that do
    /// not match its pass, is refused, and the judge can correct it.
    #[tokio::test]
    async fn a_verdict_for_another_judgement_or_without_violations_is_refused() {
        let verdict = |judgement: &str, pass: bool| {
            vec![
                MockStreamEvent::tool_call(
                    "verdict",
                    "submit_rule_verdict",
                    serde_json::json!({"judgement": judgement, "pass": pass, "violations": []}),
                ),
                MockStreamEvent::final_response(Usage::new()),
            ]
        };
        let model = MockCompletionModel::from_stream_turns([
            verdict("judgement-7", true),
            verdict("judgement-1", false),
            verdict("judgement-1", true),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let report = report(rule_check(&agent_with(vec![rule("a")]), &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 3, "the two refused verdicts each cost the judge a turn");
        assert_eq!(judged(&report)[0]["verdict"], "positive");
    }

    /// A judge whose model fails gives up, and its rule says why.
    #[tokio::test]
    async fn a_failing_judge_reports_the_failure() {
        let model = MockCompletionModel::from_stream_turns([vec![MockStreamEvent::error(
            "Anthropic API error (400 Bad Request)",
        )]]);
        let ctx = context(model, AbortFlag::default());
        let report = report(rule_check(&agent_with(vec![rule("a")]), &ctx, &serde_json::json!({})).await);
        let reason = judged(&report)[0]["unchecked_reason"].as_str().unwrap();
        assert!(reason.starts_with("the judge failed") && reason.contains("400"), "{reason}");
    }

    /// A judge that ends without a verdict leaves its rule unchecked, saying so.
    #[tokio::test]
    async fn a_judge_without_a_verdict_leaves_its_rule_unchecked() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("I think it is fine."),
            MockStreamEvent::final_response(Usage::new()),
        ]]);
        let ctx = context(model, AbortFlag::default());
        let report = report(rule_check(&agent_with(vec![rule("a")]), &ctx, &serde_json::json!({})).await);
        let verdict = judged(&report)[0];
        assert_eq!(verdict["verdict"], "unchecked");
        assert_eq!(verdict["unchecked_reason"], "the judge ended without a verdict");
    }

    /// A judge's turn that gives `rule`'s judgement the verdict `pass`.
    fn verdict_turn(judgement: &str, pass: bool) -> Vec<MockStreamEvent> {
        let violations = if pass {
            serde_json::json!([])
        } else {
            serde_json::json!([{"pointer": "/body", "message": "split the table"}])
        };
        vec![
            MockStreamEvent::tool_call(
                "verdict",
                "submit_rule_verdict",
                serde_json::json!({"judgement": judgement, "pass": pass, "violations": violations}),
            ),
            MockStreamEvent::final_response(Usage::new()),
        ]
    }

    /// Edits the agent's document with `ops`, as a stage's `json_patch` does.
    async fn edit(agent: &SharedAgent, ops: serde_json::Value) {
        let mut guard = agent.lock().await;
        let revision = guard.revision();
        let reply = guard.execute("json_patch", &serde_json::json!({"ops": ops, "expected_revision": revision})).await;
        assert!(!matches!(reply, ToolReply::Error(_)), "{reply:?}");
    }

    const ADD_ASSET: &str =
        r#"[{"op": "add", "path": "/assets/-", "value": {"key": "intro", "kind": "text", "content": {"en": "<p>Hi</p>"}}}]"#;
    const REMOVE_ASSET: &str = r#"[{"op": "remove", "path": "/assets/0"}]"#;

    /// A second check of the same document sends no judge: each judged rule
    /// keeps the verdict its judge gave on that content, negative ones too,
    /// and the report says the verdict was reused. (Each rule is first
    /// judged on its own: judges of one check run at once, and the scripted
    /// model answers in order, not by judgement.)
    #[tokio::test]
    async fn a_second_check_of_the_same_content_sends_no_judge() {
        let model = MockCompletionModel::from_stream_turns([
            verdict_turn("judgement-1", true),
            verdict_turn("judgement-2", false),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let agent = agent_with(vec![rule("a"), rule("b")]);
        for id in ["id-a", "id-b"] {
            let first = report(rule_check(&agent, &ctx, &serde_json::json!({"rule_ids": [id]})).await);
            assert!(judged(&first).iter().all(|v| v.get("cached").is_none()), "{first}");
        }
        assert_eq!(model.request_count(), 2);

        let second = report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 2, "the second check sent a judge");
        let verdicts: Vec<_> = judged(&second)
            .iter()
            .map(|v| (v["rule_id"].clone(), v["verdict"].clone(), v["cached"].clone()))
            .collect();
        assert_eq!(
            verdicts,
            [
                (serde_json::json!("id-a"), serde_json::json!("positive"), serde_json::json!(true)),
                (serde_json::json!("id-b"), serde_json::json!("negative"), serde_json::json!(true)),
            ]
        );
        assert_eq!(judged(&second)[1]["violations"][0]["message"], "split the table");
        // The board shows the reused verdicts as current.
        let board = agent.lock().await.rule_board();
        let a = board.iter().find(|r| r.rule_id == "id-a").unwrap();
        assert_eq!((a.state.clone(), a.outdated), (agent::RuleState::Pass, false));
    }

    /// The Reviewer, after a clean check of the Author's on the same content,
    /// sends no judge either: the verdicts belong to the run, not the stage
    /// that paid for them.
    #[tokio::test]
    async fn the_reviewer_reuses_the_authors_verdicts() {
        let agent = agent_with(vec![rule("a")]);
        let author_model = MockCompletionModel::from_stream_turns([verdict_turn("judgement-1", true)]);
        let author = context(author_model.clone(), AbortFlag::default());
        report(rule_check(&agent, &author, &serde_json::json!({})).await);
        assert_eq!(author_model.request_count(), 1);

        // A Reviewer of its own model, which would fail any judge sent to it.
        let reviewer_model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let reviewer = context(reviewer_model.clone(), AbortFlag::default());
        let review = report(rule_check(&agent, &reviewer, &serde_json::json!({})).await);
        assert_eq!(reviewer_model.request_count(), 0, "the Reviewer sent a judge");
        assert!(judged(&review).iter().all(|v| v["verdict"] == "positive" && v["cached"] == true), "{review}");
        assert_eq!(reviewer.spend.lock().unwrap().input_tokens, 0);
    }

    /// A check whose verdicts all come from the cache traces no judge: it
    /// opens with no judged rule to send and closes having spent nothing.
    #[tokio::test]
    async fn a_check_answered_from_the_cache_traces_no_judge() {
        let model = MockCompletionModel::from_stream_turns([verdict_turn("judgement-1", true)]);
        let recorder = Recorder::default();
        let mut ctx = context(model, AbortFlag::default());
        ctx.obs = SharedObserver::new(recorder.clone());
        let agent = agent_with(vec![rule("a")]);
        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        recorder.1.lock().unwrap().clear();

        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        let traced = recorder.1.lock().unwrap().clone();
        assert_eq!(traced.len(), 2, "{traced:?}");
        assert!(matches!(&traced[0], TraceEvent::RuleCheckStarted { judged: 0, .. }), "{traced:?}");
        let TraceEvent::RuleCheckFinished { judges_spend, outdated: false, .. } = &traced[1] else {
            panic!("{traced:?}");
        };
        assert_eq!(*judges_spend, Spend::default());
    }

    /// The cache follows the content, not the revision: an edit sends the
    /// judge again, and undoing it gives back the verdict the first content
    /// had, though the revision moved on twice.
    #[tokio::test]
    async fn an_edit_is_judged_again_and_an_undone_one_is_not() {
        let model = MockCompletionModel::from_stream_turns([
            verdict_turn("judgement-1", true),
            verdict_turn("judgement-2", false),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let agent = agent_with(vec![rule("a")]);
        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);

        edit(&agent, serde_json::from_str(ADD_ASSET).unwrap()).await;
        let edited = report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 2, "the edited document was not judged again");
        assert_eq!(judged(&edited)[0]["verdict"], "negative");
        assert!(judged(&edited)[0].get("cached").is_none());

        edit(&agent, serde_json::from_str(REMOVE_ASSET).unwrap()).await;
        let undone = report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 2, "the undone edit was judged again");
        assert_eq!(judged(&undone)[0]["verdict"], "positive");
        assert_eq!(judged(&undone)[0]["cached"], true);
    }

    /// A rule whose text changed is a rule its old verdicts were not given
    /// for: its judge runs again on the same content.
    #[tokio::test]
    async fn a_changed_rule_is_judged_again() {
        let model = MockCompletionModel::from_stream_turns([
            verdict_turn("judgement-1", true),
            verdict_turn("judgement-2", true),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let agent = agent_with(vec![rule("a")]);
        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        let mut changed = rule("a");
        changed.description = "Judge me harder.".into();
        agent.lock().await.set_judged_rules(vec![changed]);
        report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 2);
    }

    /// Only a verdict is kept: a judge that failed is sent again next check.
    #[tokio::test]
    async fn a_failed_judge_is_not_cached() {
        let model = MockCompletionModel::from_stream_turns([
            vec![MockStreamEvent::error("Anthropic API error (400 Bad Request)")],
            verdict_turn("judgement-2", true),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let agent = agent_with(vec![rule("a")]);
        let failed = report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(judged(&failed)[0]["verdict"], "unchecked");
        let retried = report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 2);
        assert_eq!(judged(&retried)[0]["verdict"], "positive");
    }

    /// A check of only some rules reuses what it can and judges the rest; a
    /// later full check then judges only what no check judged yet.
    #[tokio::test]
    async fn a_partial_check_fills_the_cache_for_a_full_one() {
        let model = MockCompletionModel::from_stream_turns([
            verdict_turn("judgement-1", true),
            verdict_turn("judgement-2", true),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let agent = agent_with(vec![rule("a"), rule("b")]);
        report(rule_check(&agent, &ctx, &serde_json::json!({"rule_ids": ["id-a"]})).await);
        assert_eq!(model.request_count(), 1);
        let full = report(rule_check(&agent, &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 2, "the full check judged rule a again");
        let cached: Vec<_> = judged(&full).iter().map(|v| (v["rule_id"].clone(), v.get("cached").cloned())).collect();
        assert_eq!(
            cached,
            [(serde_json::json!("id-a"), Some(serde_json::json!(true))), (serde_json::json!("id-b"), None)]
        );
    }

    /// An aborted run sends no judge to the model.
    #[tokio::test]
    async fn an_aborted_run_judges_nothing() {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let abort = AbortFlag::default();
        abort.abort();
        let ctx = context(model.clone(), abort);
        let report = report(rule_check(&agent_with(vec![rule("a"), rule("b")]), &ctx, &serde_json::json!({})).await);
        assert_eq!(model.request_count(), 0);
        let stopped = judged(&report);
        assert_eq!(stopped.len(), 2);
        for verdict in stopped {
            assert_eq!(verdict["unchecked_reason"], "the judge was stopped before it gave a verdict");
        }
    }
}
