//! The run analyzer over a real pipeline run: rig's scripted model drives the
//! actual controller, the recording observer writes the folder, and the folder
//! is read back. Proves the two halves — `pipeline`'s trace and `runner`'s
//! recorder — agree about what a run looks like, with no network.
//!
//! Set `KEEP_RUN_ANALYSIS=1` to keep the folder and print where it is, to read
//! a sample run's files by eye.

use std::sync::Arc;

use agent::{ConversionAgent, OutputTarget};
use pipeline::{AbortFlag, ContextBudget, NullObserver, RunConfig, RunSeed, SharedObserver};
use rig_agent::agent::model::ModelHandle;
use rig_core::completion::Usage;
use rig_core::message::Message;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
use runner::analysis::{RunAnalysis, RunEnd, RunMeta};

/// A Redacto agent over a real source whose document builds — the controller
/// builds it before every review — with its evidence gate waived, as the
/// pipeline's own controller tests do.
fn buildable_agent() -> ConversionAgent {
    let name = "AAEV_019_EN.pdf";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../forms").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut agent = ConversionAgent::new(None, vec![(name.to_string(), bytes)], String::new(), OutputTarget::Redacto)
        .expect("an agent over a source starts");
    let mut doc = agent.document().clone();
    doc["assets"] = serde_json::json!([{ "key": "intro", "kind": "text", "content": { "en": "<p>Intro.</p>" } }]);
    doc["body"] = serde_json::json!([{ "type": "assetContainer", "assets": ["intro"] }]);
    agent.seed_document(doc).expect("a Redacto document");
    agent.ensure_built().expect("the test document builds");
    agent.waive_evidence();
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
        target: OutputTarget::Redacto,
        abort: AbortFlag::default(),
        max_review_rounds: 2,
        extra_instructions: String::new(),
        template_note: "",
        model: ModelHandle::new(model),
        price: Arc::new(|usage| Some(usage.input_tokens as f64 * 0.000_005)),
        max_tokens: 4096,
        context_budget: Arc::new(NoBudget),
        capture_review: false,
        final_rule_check: false,
    };
    let meta = RunMeta {
        label: "TEST_001_DE.pdf".into(),
        session_id: "it-session".into(),
        kind: "fresh conversion".into(),
        started: "2026-09-30T14:00:00+02:00".into(),
        profile: "(none)".into(),
        target: "redacto".into(),
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
