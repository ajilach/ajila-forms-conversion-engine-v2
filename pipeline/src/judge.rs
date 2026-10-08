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

use std::sync::{Arc, Mutex};

use agent::rules::{JudgedRule, RuleVerdict, merge_rule_report};
use agent::{OutputTarget, ToolReply};
use futures_util::StreamExt;
use rig_agent::agent::model::ModelHandle;

use crate::hooks::PriceFn;
use crate::memory::ContextBudget;
use crate::observer::{AbortFlag, RetryAction, RunEvent, RunObserver, SharedObserver, Spend};
use crate::run::run_stage;
use crate::tools::SharedAgent;

/// How many judges run at once.
const JUDGES_AT_ONCE: usize = 4;

/// Why a judged rule has no verdict when the document was edited while it was
/// judged.
const CHANGED_WHILE_JUDGED: &str = "the document changed while it was judged: check this rule again";

/// What a judge stage needs from the stage that dispatches it.
#[derive(Clone)]
pub(crate) struct JudgeContext {
    pub(crate) target: OutputTarget,
    pub(crate) model: ModelHandle,
    pub(crate) price: PriceFn,
    pub(crate) max_tokens: u32,
    pub(crate) context_budget: Arc<dyn ContextBudget>,
    pub(crate) abort: AbortFlag,
    pub(crate) obs: SharedObserver,
    /// What the judges spent, which the dispatching stage folds into the
    /// run's total when it ends. Shared because judges run concurrently.
    pub(crate) spend: Arc<Mutex<Spend>>,
}

impl JudgeContext {
    /// A context whose judges have spent nothing yet.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        target: OutputTarget,
        model: ModelHandle,
        price: PriceFn,
        max_tokens: u32,
        context_budget: Arc<dyn ContextBudget>,
        abort: AbortFlag,
        obs: SharedObserver,
    ) -> Self {
        let spend = Arc::new(Mutex::new(Spend::default()));
        Self { target, model, price, max_tokens, context_budget, abort, obs, spend }
    }

    /// Folds what the judges spent into the run's `total`, and reports the new
    /// total when they spent anything.
    pub(crate) fn fold_spend(&self, total: &mut Spend) {
        let judged = *self.spend.lock().unwrap_or_else(|p| p.into_inner());
        if judged.input_tokens + judged.output_tokens > 0 {
            total.merge(&judged);
            self.obs.emit(RunEvent::Spend(*total));
        }
    }
}

/// Tells the observer where every rule stands now.
pub(crate) async fn report_rules(agent: &SharedAgent, obs: &SharedObserver) {
    let board = agent.lock().await.rule_board();
    obs.emit(RunEvent::Rules(board));
}

/// One `rule_check`: the scripts while holding the agent, then the judges
/// without it (each judge's tools take the agent call by call).
pub(crate) async fn rule_check(agent: &SharedAgent, ctx: &JudgeContext, input: &serde_json::Value) -> ToolReply {
    let (plan, scripted, revision) = {
        let mut guard = agent.lock().await;
        let plan = match guard.rule_check_plan(input) {
            Ok(plan) => plan,
            Err(e) => return ToolReply::Error(e),
        };
        match guard.check_scripted(&plan).await {
            Ok(scripted) => (plan, scripted, guard.revision()),
            Err(e) => return ToolReply::Error(e),
        }
    };
    report_rules(agent, &ctx.obs).await;
    // `buffered`, not `buffer_unordered`: the report keeps the rules' order.
    let mut verdicts: Vec<(JudgedRule, Result<RuleVerdict, String>)> = futures_util::stream::iter(plan.judged)
        .map(|rule| judge(agent, ctx, rule, revision))
        .buffered(JUDGES_AT_ONCE)
        .collect()
        .await;
    // Each judge put its outcome on the board for `revision`, which an edit
    // made since (another call of the same turn) shows as outdated. The model
    // is told to check again instead: a verdict on an older document is not
    // one on the document it now has.
    if agent.lock().await.revision() != revision {
        for (_, outcome) in &mut verdicts {
            *outcome = Err(CHANGED_WHILE_JUDGED.into());
        }
    }
    ToolReply::Text(merge_rule_report(scripted, &verdicts).to_string())
}

/// Runs one judge on `rule`, dispatched on `revision`, and puts its outcome on
/// the board as soon as it ends, before the rule stops showing as judged: a
/// check's judges end one by one, and a check cut short keeps what its ended
/// judges found.
async fn judge(
    agent: &SharedAgent,
    ctx: &JudgeContext,
    rule: JudgedRule,
    revision: u64,
) -> (JudgedRule, Result<RuleVerdict, String>) {
    ctx.obs.emit(RunEvent::Judging { rule_id: rule.id.clone(), running: true });
    let _ended = JudgingEnds { obs: &ctx.obs, rule_id: rule.id.clone() };
    let outcome = judge_rule(agent, ctx, &rule).await;
    let mut guard = agent.lock().await;
    // The judge read the live document. An edit made while it did means its
    // verdict may describe neither revision.
    let judged = (rule, if guard.revision() == revision { outcome } else { Err(CHANGED_WHILE_JUDGED.into()) });
    guard.record_judged(std::slice::from_ref(&judged), revision);
    let board = guard.rule_board();
    drop(guard);
    ctx.obs.emit(RunEvent::Rules(board));
    judged
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

async fn judge_rule(agent: &SharedAgent, ctx: &JudgeContext, rule: &JudgedRule) -> Result<RuleVerdict, String> {
    let role = crate::roles::roles_for(ctx.target).judge;
    let failure = Arc::new(Mutex::new(None));
    let obs = SharedObserver::new(JudgeObserver {
        inner: ctx.obs.clone(),
        rule: rule.title.clone(),
        failure: failure.clone(),
    });
    let judgement = agent.lock().await.open_judgement();
    let mut spend = Spend::default();
    let ended = run_stage(
        agent,
        role,
        &crate::roles::sys_judge(ctx.target, rule, &judgement),
        &format!("Judge the rule \"{}\", then call submit_rule_verdict with judgement {judgement}.", rule.title),
        &ctx.abort,
        ctx.model.clone(),
        ctx.price.clone(),
        ctx.max_tokens,
        ctx.context_budget.clone(),
        &obs,
        &mut spend,
    )
    .await;
    ctx.spend.lock().unwrap_or_else(|p| p.into_inner()).merge(&spend);
    let verdict = agent.lock().await.take_judgement(&judgement);
    let failed = failure.lock().unwrap_or_else(|p| p.into_inner()).take();
    match (verdict, ended, failed) {
        (Some(verdict), _, _) => Ok(verdict),
        (None, _, Some(error)) => Err(format!("the judge failed: {error}")),
        (None, None, None) => Err("the judge was stopped before it gave a verdict".to_string()),
        (None, Some(_), None) => Err("the judge ended without a verdict".to_string()),
    }
}

/// What a judge reports to the run's observer: its tool timeline and its
/// warnings and thoughts, labelled with the rule. Not its stage header, its
/// spend (cumulative per stage, so it would read as the run's total; the
/// dispatching stage folds it in instead) or its context fill, and never a
/// retry prompt: a judge that fails permanently gives up, and its rule is
/// reported unchecked.
struct JudgeObserver {
    inner: SharedObserver,
    rule: String,
    /// The error a judge gave up on, which its rule is reported unchecked with.
    failure: Arc<Mutex<Option<String>>>,
}

impl RunObserver for JudgeObserver {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::Stage { .. } | RunEvent::Spend(_) | RunEvent::ContextUsed(_) => {}
            RunEvent::Thought(text) => self.inner.emit(RunEvent::Thought(format!("[judge: {}] {text}", self.rule))),
            RunEvent::Warning(text) => self.inner.emit(RunEvent::Warning(format!("[judge: {}] {text}", self.rule))),
            // Several judges run at once: each call says whose it is.
            RunEvent::ToolStarted { id, name, input_summary } => self.inner.emit(RunEvent::ToolStarted {
                id,
                name,
                input_summary: format!("[judge: {}] {input_summary}", self.rule),
            }),
            other => self.inner.emit(other),
        }
    }

    fn retry_prompt(&mut self, _role: &str, error: &str) {
        *self.failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.to_string());
        self.inner.emit(RunEvent::Warning(format!(
            "[judge: {}] gave up after a failed turn: {error}",
            self.rule
        )));
    }

    fn poll_retry(&mut self) -> Option<RetryAction> {
        Some(RetryAction::Cancel)
    }

    fn retry_resolved(&mut self, _action: RetryAction) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::Usage;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    struct NoBudget;
    impl ContextBudget for NoBudget {
        fn policy(&self) -> Arc<dyn rig_memory::MemoryPolicy> {
            Arc::new(rig_memory::NoopMemoryPolicy)
        }
        fn raw_estimate(&self, _history: &[rig_core::message::Message]) -> usize {
            0
        }
        fn record_actual(&self, _raw_estimate: usize, _real_tokens: u64) {}
    }

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
        let mut agent = agent::ConversionAgent::new(None, Vec::new(), String::new(), OutputTarget::Redacto)
            .expect("an agent without sources starts");
        agent.set_judged_rules(rules);
        Arc::new(tokio::sync::Mutex::new(agent))
    }

    fn context(model: MockCompletionModel, abort: AbortFlag) -> JudgeContext {
        JudgeContext {
            target: OutputTarget::Redacto,
            model: ModelHandle::new(model),
            price: Arc::new(|usage| Some(usage.input_tokens as f64 * 0.01)),
            max_tokens: 1000,
            context_budget: Arc::new(NoBudget),
            abort,
            obs: SharedObserver::new(crate::observer::NullObserver),
            spend: Arc::default(),
        }
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

    /// Records every event a check reports.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<RunEvent>>>);
    impl RunObserver for Recorder {
        fn emit(&mut self, event: RunEvent) {
            self.0.lock().unwrap().push(event);
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
