//! The run analyzer over a real pipeline run: rig's scripted model drives the
//! actual controller, the recording observer writes the folder, and the folder
//! is read back. Proves the two halves — `pipeline`'s trace and `runner`'s
//! recorder — agree about what a run looks like, with no network.
//!
//! Set `KEEP_RUN_ANALYSIS=1` to keep the folder and print where it is, to read
//! a sample run's files by eye.

use std::sync::Arc;

use agent::ConversionAgent;
use pipeline::{AbortFlag, ContextBudget, NullObserver, RunConfig, RunSeed, SharedObserver};
use rig_agent::agent::model::ModelHandle;
use rig_core::completion::Usage;
use rig_core::message::Message;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
use runner::analysis::{RunAnalysis, RunEnd, RunMeta};

/// An agent over a real source whose document builds — the controller
/// builds it before every review — with its evidence gate waived, as the
/// pipeline's own controller tests do.
fn buildable_agent() -> ConversionAgent {
    let name = "AAEV_019_EN.pdf";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../forms").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut agent = ConversionAgent::new(None, vec![(name.to_string(), bytes)], String::new())
        .expect("an agent over a source starts");
    let mut doc = agent.document().clone();
    doc["form"]["children"] = serde_json::json!([{
        "type": "Panel", "uuid": "6a9f2f5e-8c8e-4a8e-9b0e-1f2d3c4b5a61", "name": "PN_Details",
        "title": {"en": "Details"}, "children": [{
            "type": "TextField", "uuid": "6a9f2f5e-8c8e-4a8e-9b0e-1f2d3c4b5a62",
            "name": "TXT_LastName", "label": {"en": "Last name"}, "mandatory": false,
            "visible": true, "max_chars": null, "colspan": 12, "dor_colspan": null,
            "bind_ref": null, "kind": "Plain"
        }],
        "is_page": true, "visible": true, "is_conditional": false, "dor_num_cols": null,
        "colspan": 12, "dor_colspan": null, "bind_ref": null, "frag_ref": null
    }]);
    agent.seed_document(doc).expect("an AEM document");
    agent.ensure_built().expect("the test document builds");
    agent.waive_evidence();
    // No judged rules unless a test gives some: the check before every review
    // would send their judges the turns a test scripts for its stages.
    agent.set_judged_rules(Vec::new());
    agent
}

struct NoBudget;

impl ContextBudget for NoBudget {
    fn policy(&self) -> Arc<dyn rig_memory::MemoryPolicy> {
        Arc::new(rig_memory::NoopMemoryPolicy)
    }
    fn raw_estimate(&self, _history: &[Message]) -> usize {
        0
    }
    fn record_actual(&self, _raw_estimate: usize, _real_tokens: u64) {}
}

fn usage(input: u64, output: u64) -> Usage {
    let mut usage = Usage::new();
    usage.input_tokens = input;
    usage.output_tokens = output;
    usage
}

fn tool_turn(id: &str, name: &str, args: serde_json::Value, text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::tool_call(id, name, args),
        MockStreamEvent::final_response(usage(1_200, 80)),
    ]
}

fn text_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::final_response(usage(2_000, 400)),
    ]
}

fn review_turn(approved: bool, report: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::tool_call(
            "review",
            "submit_review",
            serde_json::json!({"approved": approved, "report": report}),
        ),
        MockStreamEvent::final_response(usage(3_000, 200)),
    ]
}

#[tokio::test]
async fn a_pipeline_run_is_recorded_into_a_readable_folder() {
    let root = std::env::temp_dir().join(format!("blueprint-run-analysis-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let model = MockCompletionModel::from_stream_turns([
        tool_turn("a1", "get_source_info", serde_json::json!({}), "Looking at the source first."),
        tool_turn("a2", "get_source_info", serde_json::json!({}), "Checking it once more."),
        text_turn("Built the document."),
        review_turn(false, "The footer is missing."),
        text_turn("Added the footer."),
        review_turn(true, "Looks complete."),
    ]);
    let agent = buildable_agent();
    let config = RunConfig {
        profile: None,
        abort: AbortFlag::default(),
        max_review_rounds: 2,
        extra_instructions: String::new(),
        template_note: "",
        model: ModelHandle::new(model),
        price: Arc::new(|usage| Some(usage.input_tokens as f64 * 0.000_005)),
        max_tokens: 4096,
        context_budget: Arc::new(NoBudget),
        reviewer_model: None,
        judge_model: None,
        capture_review: false,
        final_rule_check: false,
        finish_nudge: false,
        warm_up_verifier: false,
    };
    let meta = RunMeta {
        label: "TEST_001_DE.pdf".into(),
        session_id: "it-session".into(),
        kind: "fresh conversion".into(),
        started: "2026-09-30T14:00:00+02:00".into(),
        profile: "(none)".into(),
        model: "scripted mock".into(),
        max_review_rounds: 2,
        verification: "AEM verifier".into(),
        engine_version: "test".into(),
    };

    let caller = SharedObserver::new(NullObserver);
    let analysis = RunAnalysis::start(&root, meta, &caller).expect("recording starts");
    let dir = analysis.dir();
    let shared = Arc::new(tokio::sync::Mutex::new(agent));
    let outcome = pipeline::run(shared, config, RunSeed::Fresh, analysis.observer(caller.clone())).await;
    analysis.finish(
        RunEnd {
            produced: outcome.is_some(),
            form_code: None,
            outputs: Vec::new(),
            warnings: vec![],
        },
        &caller,
    );

    let report = std::fs::read_to_string(dir.join("report.md")).unwrap();
    for stage in ["1. Author", "2. Reviewer", "3. Author", "4. Reviewer"] {
        assert!(report.contains(stage), "{stage} missing from the report:\n{report}");
    }
    assert!(report.contains("the Reviewer approved it"), "{report}");
    assert!(report.contains("The footer is missing."), "{report}");
    assert!(report.contains("`get_source_info`"), "{report}");
    assert!(
        report.contains("## Repetition and possible loops"),
        "two identical get_source_info calls are a repetition:\n{report}"
    );

    let timeline = std::fs::read_to_string(dir.join("timeline.md")).unwrap();
    assert!(timeline.contains("**Author turn 1**"), "{timeline}");
    assert!(timeline.contains("Looking at the source first."), "{timeline}");

    let transcripts: Vec<String> = std::fs::read_dir(dir.join("transcript"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(transcripts.len(), 4, "one transcript per stage: {transcripts:?}");

    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["approved"], true);
    assert_eq!(summary["counts"]["stages"], 4);
    assert_eq!(summary["counts"]["turns"], 6);
    assert!(summary["spend"]["cost_usd"].as_f64().unwrap() > 0.0);

    if std::env::var_os("KEEP_RUN_ANALYSIS").is_some() {
        println!("run analysis kept in {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The judges of the checks before each review: traced as one line each with
/// verdict, turns and spend, tabled in the report, counted in the summary,
/// and filed under the controller's rule checks rather than a stage. The
/// Author ends with `finish_authoring`, and the report says so rather than
/// calling it a review.
#[tokio::test]
async fn a_judge_is_recorded_and_split_out_of_its_stages_cost() {
    let root = std::env::temp_dir().join(format!("blueprint-run-analysis-judge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let judge_turn = |judgement: &str, pass: bool, input: u64| {
        let violations = if pass {
            serde_json::json!([])
        } else {
            serde_json::json!([{"pointer": "/form/children/0", "message": "split the table"}])
        };
        vec![
            MockStreamEvent::tool_call(
                "verdict",
                "submit_rule_verdict",
                serde_json::json!({"judgement": judgement, "pass": pass, "violations": violations}),
            ),
            MockStreamEvent::final_response(usage(input, 300)),
        ]
    };
    let model = MockCompletionModel::from_stream_turns([
        tool_turn(
            "a1",
            "json_patch",
            serde_json::json!({"expected_revision": 0, "ops": [
                {"op": "replace", "path": "/form/children/0/title/en", "value": "Details, one table"}
            ]}),
            "Building the table.",
        ),
        tool_turn("a2", "finish_authoring", serde_json::json!({"summary": "done"}), "Handing over."),
        // The check before the first review, with its judge.
        judge_turn("judgement-1", false, 10_000),
        review_turn(false, "Split the table."),
        // The Author fixes what the judge found, so the next check judges
        // content no judge saw yet rather than reusing the verdict above.
        tool_turn(
            "a3",
            "json_patch",
            serde_json::json!({"expected_revision": 1, "ops": [
                {"op": "replace", "path": "/form/children/0/title/en", "value": "Details, split"}
            ]}),
            "Splitting the table.",
        ),
        tool_turn("a4", "finish_authoring", serde_json::json!({"summary": "done"}), "Handing over."),
        // The check before the second review, with its own judge; the final
        // check after it keeps that verdict.
        judge_turn("judgement-2", true, 20_000),
        review_turn(true, "Looks complete."),
    ]);
    let mut agent = buildable_agent();
    agent.set_judged_rules(vec![agent::rules::JudgedRule {
        id: "id-tables".into(),
        name: "tables-are-split".into(),
        title: "Tables are split".into(),
        description: "Judge me.".into(),
        ..Default::default()
    }]);
    let config = RunConfig {
        profile: None,
        abort: AbortFlag::default(),
        max_review_rounds: 1,
        extra_instructions: String::new(),
        template_note: "",
        model: ModelHandle::new(model),
        price: Arc::new(|usage| Some(usage.input_tokens as f64 * 0.000_005)),
        max_tokens: 4096,
        context_budget: Arc::new(NoBudget),
        reviewer_model: None,
        judge_model: None,
        capture_review: false,
        final_rule_check: true,
        finish_nudge: false,
        warm_up_verifier: false,
    };
    let config = RunConfig { max_review_rounds: 2, ..config };
    let meta = RunMeta {
        label: "TEST_002_DE.pdf".into(),
        session_id: "it-judge".into(),
        kind: "fresh conversion".into(),
        started: "2026-10-09T14:00:00+02:00".into(),
        profile: "(none)".into(),
        model: "scripted mock".into(),
        max_review_rounds: 1,
        verification: "AEM verifier".into(),
        engine_version: "test".into(),
    };

    let caller = SharedObserver::new(NullObserver);
    let analysis = RunAnalysis::start(&root, meta, &caller).expect("recording starts");
    let dir = analysis.dir();
    let shared = Arc::new(tokio::sync::Mutex::new(agent));
    let outcome = pipeline::run(shared, config, RunSeed::Fresh, analysis.observer(caller.clone())).await;
    analysis.finish(
        RunEnd {
            produced: outcome.is_some(),
            form_code: None,
            outputs: Vec::new(),
            warnings: vec![],
        },
        &caller,
    );

    // trace.jsonl: one judge per check before a review; the final check
    // keeps the second one's verdict.
    let trace: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let judges: Vec<&serde_json::Value> = trace.iter().filter(|l| l["event"] == "judge_finished").collect();
    assert_eq!(judges.len(), 2, "one judge per review, none in the final check");
    let judge = judges[0];
    assert_eq!(judge["rule_name"], "tables-are-split");
    assert_eq!(judge["verdict"], "negative");
    assert_eq!(judge["violations"], 1);
    assert_eq!(judge["turns"], 1);
    assert_eq!(judge["spend"]["input_tokens"], 10_000);
    assert_eq!(judges[1]["verdict"], "positive");
    for event in ["rule_check_started", "rule_check_finished"] {
        assert!(trace.iter().any(|l| l["event"] == event), "{event} missing");
    }

    // summary.json: counted, priced, and filed under the controller's rule
    // checks, not under the stage that ended before them.
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["schema_version"], runner::analysis::SCHEMA_VERSION);
    assert_eq!(summary["judge_runs"], 2);
    assert_eq!(summary["counts"]["rule_checks"], 3, "before each of two reviews, and the final one");
    let judge_cost = summary["judge_cost_usd"].as_f64().unwrap();
    assert!((judge_cost - 0.15).abs() < 1e-9, "{judge_cost}");
    let cost = |v: &serde_json::Value| v["cost_usd"].as_f64().unwrap();
    for stage in summary["stages"].as_array().unwrap() {
        assert_eq!(stage["judge_runs"], 0, "{stage}");
        assert!((cost(&stage["agent_spend"]) - cost(&stage["spend"])).abs() < 1e-9, "{stage}");
    }
    let checks = &summary["judges"]["final_check"];
    assert_eq!(checks["name"], pipeline::run::RULE_CHECKS);
    assert_eq!(checks["judge_runs"], 2, "{checks}");
    assert!((cost(&checks["judges_spend"]) - 0.15).abs() < 1e-9, "{checks}");
    assert_eq!(summary["stages"][0]["ended_by"], "finish_authoring");
    let run_total = summary["spend"]["cost_usd"].as_f64().unwrap();
    assert!((summary["agent_cost_usd"].as_f64().unwrap() + judge_cost - run_total).abs() < 1e-9);
    assert_eq!(summary["judges"]["by_rule"][0]["negative"], 1);
    assert_eq!(summary["judges"]["by_rule"][0]["positive"], 1);

    // report.md: the judges' tables, and how each stage really ended.
    let report = std::fs::read_to_string(dir.join("report.md")).unwrap();
    for wanted in [
        "## Judges",
        "### By rule",
        "| `tables-are-split` | 2 | 1 | 1 | 0 |",
        "### By rule_check",
        "| 1 | Rule checks | all |",
        "- Judges: 2 run(s) over 3 rule_check(s), USD 0.15",
        "called finish_authoring",
        "submitted a review",
    ] {
        assert!(report.contains(wanted), "{wanted} missing from the report:\n{report}");
    }
    let timeline = std::fs::read_to_string(dir.join("timeline.md")).unwrap();
    assert!(timeline.contains("judge `tables-are-split` (rule_check 1) → negative (1 violation(s))"), "{timeline}");

    if std::env::var_os("KEEP_RUN_ANALYSIS").is_some() {
        println!("run analysis kept in {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&root);
    }
}
