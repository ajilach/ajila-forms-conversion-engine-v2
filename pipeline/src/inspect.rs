//! `inspect`: a stage hands read-only work to inspectors, one per brief,
//! several at once, and gets back each one's report.
//!
//! An inspector is a sub-stage ([`crate::substage`]) of
//! [`crate::roles::INSPECTOR`]: it reads the source and the document, edits
//! nothing, and ends with `submit_findings`. Unlike a judge it works on the
//! dispatching stage's behalf, so what it renders, sets and verifies is that
//! stage's evidence (`agent::Caller::Inspector`). At most one brief per call
//! walks the verifier ([`crate::roles::WALKER`]): the agent has one verifier
//! session, holding one form, so while the walker runs every other caller's
//! verifier calls are refused (`agent::ConversionAgent::begin_walk`).

use agent::{Caller, ToolReply};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::substage::{SubStageContext, run_sub_stage};
use crate::tools::SharedAgent;

/// How many inspectors run at once.
const INSPECTORS_AT_ONCE: usize = 4;
/// How many briefs one `inspect` takes, as its input schema says.
const MAX_TASKS: usize = 6;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectInput {
    tasks: Vec<Task>,
}

/// One brief, as `inspect` takes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    brief: String,
    #[serde(default)]
    walk: bool,
}

/// Parses `inspect`'s input into its briefs: one to [`MAX_TASKS`] of them,
/// none empty, at most one walking the verifier.
fn parse_tasks(input: &Value) -> Result<Vec<Task>, String> {
    let InspectInput { tasks } = serde_json::from_value(input.clone()).map_err(|e| format!("inspect: {e}"))?;
    if tasks.is_empty() || tasks.len() > MAX_TASKS {
        return Err(format!("inspect takes 1 to {MAX_TASKS} tasks, not {}", tasks.len()));
    }
    if tasks.iter().any(|t| t.brief.trim().is_empty()) {
        return Err("inspect: every task needs a brief".into());
    }
    if tasks.iter().filter(|t| t.walk).count() > 1 {
        return Err("inspect: at most one task may walk the verifier (walk=true)".into());
    }
    Ok(tasks)
}

/// One `inspect`: the inspectors run without the agent held (each one's tools
/// take it call by call), and the reply lists their reports in task order.
pub(crate) async fn inspect(agent: &SharedAgent, ctx: &SubStageContext, input: &Value) -> ToolReply {
    let tasks = match parse_tasks(input) {
        Ok(tasks) => tasks,
        Err(e) => return ToolReply::Error(e),
    };
    // `buffered`, not `buffer_unordered`: the reply keeps the tasks' order.
    let reports: Vec<Value> = futures_util::stream::iter(tasks)
        .map(|task| inspect_one(agent, ctx, task))
        .buffered(INSPECTORS_AT_ONCE)
        .collect()
        .await;
    ToolReply::Text(json!({ "reports": reports }).to_string())
}

/// Why a walk that failed may have left the verifier busy, told the stage so
/// it does not trip over the form its walker opened.
const FORM_MAY_BE_OPEN: &str = "the verifier may still hold the form the walker opened: \
                                aem_verify_open names it when it refuses, so close it with \
                                aem_verify_close before opening one";

/// Runs one inspector on `task` and reports what it found, or why it found
/// nothing.
async fn inspect_one(agent: &SharedAgent, ctx: &SubStageContext, task: Task) -> Value {
    // Opened and, for the walk, handed the verifier under one hold of the
    // agent, so no call of the stage's slips in between.
    let (inspection, revision, walk) = {
        let mut guard = agent.lock().await;
        let inspection = guard.open_inspection();
        let walk = match task.walk.then(|| Walk::begin(agent, &mut guard, &inspection)) {
            Some(Err(e)) => {
                guard.take_inspection(&inspection);
                return json!({ "brief": task.brief, "status": "failed", "document_changed": false, "reason": e });
            }
            Some(Ok(walk)) => Some(walk),
            None => None,
        };
        (inspection, guard.revision(), walk)
    };
    let roles = crate::roles::roles_for(ctx.target);
    let (role, who) = if task.walk { (roles.walker, "walker") } else { (roles.inspector, "inspector") };
    let end = run_sub_stage(
        agent,
        ctx,
        role,
        &crate::roles::sys_inspector(ctx.target, &task.brief, task.walk, &inspection),
        &format!("Do the brief, then call submit_findings with inspection {inspection}."),
        &Caller::Inspector(inspection.clone()),
        format!("{who} {inspection}: {}", excerpt(&task.brief)),
    )
    .await;
    let mut guard = agent.lock().await;
    // Given back before the reply is, so the stage's next verifier call finds
    // it free.
    if let Some(walk) = walk {
        walk.end(&mut guard);
    }
    let document_changed = guard.revision() != revision;
    match guard.take_inspection(&inspection) {
        Some(findings) => json!({
            "brief": task.brief,
            "status": "reported",
            "document_changed": document_changed,
            "report": findings,
        }),
        None => {
            let mut failed = json!({
                "brief": task.brief,
                "status": "failed",
                "document_changed": document_changed,
                "reason": end.why_no(who, "a report"),
            });
            if task.walk && ctx.target == agent::OutputTarget::Aem {
                failed["note"] = json!(FORM_MAY_BE_OPEN);
            }
            failed
        }
    }
}

/// The start of `brief`, which labels its inspector's timeline.
fn excerpt(brief: &str) -> String {
    const LONGEST: usize = 48;
    let brief = brief.trim();
    match brief.char_indices().nth(LONGEST) {
        Some((cut, _)) => format!("{}...", &brief[..cut]),
        None => brief.to_string(),
    }
}

/// The verifier handed to one inspector until [`Walk::end`]. A walk whose
/// inspector's future is dropped instead (the stage was stopped) gives the
/// verifier back as it drops.
struct Walk {
    /// The agent to give the verifier back on, until it has been.
    agent: Option<SharedAgent>,
}

impl Walk {
    fn begin(agent: &SharedAgent, held: &mut agent::ConversionAgent, inspection: &str) -> Result<Self, String> {
        held.begin_walk(inspection)?;
        Ok(Self { agent: Some(agent.clone()) })
    }

    /// Gives the verifier back on `held`, the agent this walk began on.
    fn end(mut self, held: &mut agent::ConversionAgent) {
        held.end_walk();
        self.agent = None;
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        let Some(agent) = self.agent.take() else { return };
        // A drop cannot wait for the agent: take it when it is free, and
        // otherwise as soon as its holder lets go.
        if let Ok(mut guard) = agent.try_lock() {
            guard.end_walk();
            return;
        }
        tokio::spawn(async move { agent.lock().await.end_walk() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::Usage;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    use crate::observer::AbortFlag;
    use crate::substage::test_support;

    fn redacto_agent() -> SharedAgent {
        test_support::agent_with_judged(Vec::new())
    }

    fn context(model: MockCompletionModel, abort: AbortFlag) -> SubStageContext {
        test_support::context(model, abort)
    }

    fn findings_turn(inspection: &str, checked: &str, usage: Usage) -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::tool_call(
                "findings",
                "submit_findings",
                json!({"inspection": inspection, "checked": [checked],
                    "findings": [{"severity": "defect", "message": format!("{checked} is missing"), "source": checked}]}),
            ),
            MockStreamEvent::final_response(usage),
        ]
    }

    fn reports(reply: ToolReply) -> Vec<Value> {
        match reply {
            ToolReply::Text(text) => serde_json::from_str::<Value>(&text).unwrap()["reports"].as_array().unwrap().clone(),
            other => panic!("inspect failed: {other:?}"),
        }
    }

    #[test]
    fn tasks_parse_within_their_limits() {
        let tasks = parse_tasks(&json!({"tasks": [{"brief": "pages 1-2"}, {"brief": "walk it", "walk": true}]})).unwrap();
        assert_eq!(tasks[0], Task { brief: "pages 1-2".into(), walk: false });
        assert!(tasks[1].walk);

        let refused = |input: Value, why: &str| {
            let error = parse_tasks(&input).expect_err(why);
            assert!(error.contains(why), "{error}");
        };
        refused(json!({"tasks": []}), "1 to 6");
        refused(json!({"tasks": vec![json!({"brief": "x"}); 7]}), "1 to 6");
        refused(json!({"tasks": [{"brief": "  "}]}), "needs a brief");
        refused(json!({"tasks": [{"brief": "a", "walk": true}, {"brief": "b", "walk": true}]}), "at most one");
        refused(json!({"tasks": [{"brief": "a", "page": 1}]}), "unknown field");
        refused(json!({}), "missing field");
    }

    /// Each brief goes to an inspector of its own, told its brief, and the
    /// reply holds every report in the tasks' order, with what they spent
    /// folded into the dispatching stage's share.
    #[tokio::test]
    async fn every_brief_is_inspected_and_reported_in_order() {
        let mut usage = Usage::new();
        usage.input_tokens = 50;
        // `buffered` starts the inspectors in task order, and each one's first
        // request takes the next turn, so inspection-1 answers the first brief.
        let model = MockCompletionModel::from_stream_turns([
            findings_turn("inspection-1", "page 1", usage),
            findings_turn("inspection-2", "page 2", usage),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let input = json!({"tasks": [{"brief": "Read page 1."}, {"brief": "Read page 2."}]});
        let reports = reports(inspect(&redacto_agent(), &ctx, &input).await);

        assert_eq!(reports.len(), 2);
        for (report, page) in reports.iter().zip(["page 1", "page 2"]) {
            assert_eq!(report["status"], "reported", "{report}");
            assert_eq!(report["brief"], format!("Read {page}."));
            assert_eq!(report["report"]["checked"][0], page);
            assert_eq!(report["report"]["findings"][0]["severity"], "defect");
            assert_eq!(report["document_changed"], false);
        }
        let systems: Vec<String> = model
            .requests()
            .iter()
            .filter_map(|r| match r.chat_history.first() {
                Some(rig_core::message::Message::System { content }) => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert!(systems.iter().any(|s| s.contains("Read page 1.") && s.contains("inspection-1")), "{systems:?}");
        assert_eq!(ctx.spend.lock().unwrap().input_tokens, 100);
        assert_eq!(model.request_count(), 2, "submit_findings ends each inspector's stage");
    }

    /// An inspector that ends without submit_findings is reported failed, with
    /// why.
    #[tokio::test]
    async fn an_inspector_without_findings_is_reported_failed() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("Looks fine to me."),
            MockStreamEvent::final_response(Usage::new()),
        ]]);
        let ctx = context(model, AbortFlag::default());
        let reports = reports(inspect(&redacto_agent(), &ctx, &json!({"tasks": [{"brief": "Read page 1."}]})).await);
        assert_eq!(reports[0]["status"], "failed");
        assert_eq!(reports[0]["reason"], "the inspector ended without a report");
    }

    /// An aborted run sends no inspector to the model.
    #[tokio::test]
    async fn an_aborted_run_inspects_nothing() {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let abort = AbortFlag::default();
        abort.abort();
        let ctx = context(model.clone(), abort);
        let reports = reports(inspect(&redacto_agent(), &ctx, &json!({"tasks": [{"brief": "a"}, {"brief": "b"}]})).await);
        assert_eq!(model.request_count(), 0);
        for report in reports {
            assert_eq!(report["reason"], "the inspector was stopped before it gave a report");
        }
    }

    /// While the walker runs, the verifier is its alone, and what it renders
    /// is the stage's evidence; once it ends, the verifier is the stage's
    /// again.
    #[tokio::test]
    async fn the_walker_drives_the_verifier_and_its_work_counts() {
        let agent = redacto_agent();
        let render = json!({"doc_path": "/nowhere/source.pdf", "page": 1});
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("render", "xfa_render_pages", render.clone()),
                MockStreamEvent::tool_call("status", "redacto_verify_status", json!({})),
                MockStreamEvent::final_response(Usage::new()),
            ],
            findings_turn("inspection-1", "the PDFs", Usage::new()),
        ]);
        let ctx = context(model.clone(), AbortFlag::default());
        let names_render = |missing: Vec<String>| missing.join("\n").contains("xfa_render_pages");
        assert!(names_render(agent.lock().await.missing_evidence()));

        let reply = inspect(&agent, &ctx, &json!({"tasks": [{"brief": "Walk it.", "walk": true}]})).await;
        assert_eq!(reports(reply)[0]["status"], "reported");
        let answered = format!("{:?}", model.requests()[1].chat_history);
        assert!(!answered.contains("an inspector is walking"), "the walker was refused its own verifier: {answered}");
        assert!(!names_render(agent.lock().await.missing_evidence()), "the walker's render did not count");

        let after = agent.lock().await.execute("redacto_verify_status", &json!({})).await;
        assert!(!matches!(&after, ToolReply::Error(e) if e.contains("an inspector is walking")), "{after:?}");
    }

    /// While a walk is on, a second walk is refused rather than sharing the
    /// verifier.
    #[tokio::test]
    async fn a_second_walk_is_refused() {
        let agent = redacto_agent();
        let walk = {
            let mut guard = agent.lock().await;
            Walk::begin(&agent, &mut guard, "inspection-9").unwrap()
        };
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let ctx = context(model.clone(), AbortFlag::default());
        let reports = reports(inspect(&agent, &ctx, &json!({"tasks": [{"brief": "Walk it.", "walk": true}]})).await);
        assert_eq!(reports[0]["status"], "failed");
        assert!(reports[0]["reason"].as_str().unwrap().contains("walking the verifier already"));
        assert_eq!(reports[0]["document_changed"], false);
        let late = agent.lock().await.execute("submit_findings", &json!({"inspection": "inspection-1", "checked": ["x"]})).await;
        assert!(matches!(&late, ToolReply::Error(e) if e.contains("no open inspection")), "the refused walk's inspection stayed open: {late:?}");
        assert_eq!(model.request_count(), 0);
        drop(walk);
        assert!(agent.lock().await.begin_walk("inspection-10").is_ok(), "a dropped walk gives the verifier back");
    }
}
