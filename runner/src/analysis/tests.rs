//! The recorder end to end: synthetic trace events through a
//! [`RecordingObserver`] into a scratch folder, and the files read back.

use super::*;
use pipeline::{ControlKind, NullObserver, StageEnd, TracedToolCall, TurnUsage};

fn scratch_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blueprint-run-analysis-{}-{}",
        std::process::id(),
        chrono::Local::now().format("%H%M%S%f")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meta() -> RunMeta {
    RunMeta {
        label: "AAOS_033_IT.pdf".into(),
        session_id: "0123456789abcdef".into(),
        kind: "fresh conversion".into(),
        started: "2026-09-30T14:00:00+02:00".into(),
        profile: "ubs".into(),
        target: "aem".into(),
        model: "anthropic claude-opus-5".into(),
        max_review_rounds: 3,
        verification: "AEM verifier".into(),
        engine_version: "test".into(),
    }
}

/// A scripted Author stage: one turn that asks for a validation, the same
/// validation failing twice with the same result, then a Reviewer rejecting.
fn script(obs: &SharedObserver) {
    obs.emit(RunEvent::Stage {
        role: "Author",
        doing: "building the AEM form".into(),
    });
    obs.trace(TraceEvent::StageStarted {
        stage: "Author".into(),
        system_prompt: "You are the Author.".into(),
        seed_message: "Build it.".into(),
        max_turns: 110,
        tools_offered: vec!["validate_aem_package".into(), "set_aem_translated_field".into()],
    });
    obs.trace(TraceEvent::AttemptStarted {
        stage: "Author".into(),
        attempt: 1,
        history_messages: 0,
    });
    for turn in 1..=2 {
        obs.trace(TraceEvent::TurnStarted {
            stage: "Author".into(),
            attempt: 1,
            turn,
            history_messages: turn * 2,
            sent_messages: turn * 2,
            estimated_tokens: 1_000,
            shaping_ms: 0,
        });
        obs.trace(TraceEvent::TurnFinished {
            stage: "Author".into(),
            attempt: 1,
            turn,
            latency_ms: 4_000,
            finish_reason: Some("tool_calls".into()),
            usage: TurnUsage {
                input_tokens: 100,
                cached_input_tokens: 900,
                output_tokens: 50,
                ..TurnUsage::default()
            },
            cost_usd: Some(0.1),
            text: "Validating the package now.".into(),
            reasoning: String::new(),
            tool_calls: vec![TracedToolCall {
                call_id: format!("c{turn}"),
                name: "validate_aem_package".into(),
                args: serde_json::json!({}),
            }],
        });
        obs.trace(TraceEvent::ToolStarted {
            stage: "Author".into(),
            attempt: 1,
            turn,
            call_id: format!("c{turn}"),
            name: "validate_aem_package".into(),
            args: serde_json::json!({}),
        });
        obs.trace(TraceEvent::ToolFinished {
            stage: "Author".into(),
            attempt: 1,
            turn,
            call_id: format!("c{turn}"),
            name: "validate_aem_package".into(),
            ok: false,
            duration_ms: 1_500,
            result: "Error: field 'IBAN' has no bindRef\nmore detail".into(),
            result_chars: 40,
            image_count: 0,
            image_chars: 0,
            result_hash: "same".into(),
        });
    }
    obs.trace(TraceEvent::Control {
        stage: "Author".into(),
        kind: ControlKind::OutputCapNudge,
        detail: "nudge 1 of 3".into(),
    });
    obs.emit(RunEvent::Warning("Author: something odd".into()));
    obs.trace(TraceEvent::StageFinished {
        stage: "Author".into(),
        ended: StageEnd::Finished,
        turns: 2,
        attempts: 1,
        duration_ms: 12_000,
        spend: pipeline::Spend::default(),
    });
    obs.trace(TraceEvent::StageStarted {
        stage: "Reviewer".into(),
        system_prompt: "You are the Reviewer.".into(),
        seed_message: "Review it.".into(),
        max_turns: 60,
        tools_offered: vec![],
    });
    obs.trace(TraceEvent::ReviewVerdict {
        stage: "Reviewer".into(),
        round: 1,
        approved: Some(false),
        report: "The footer is missing.".into(),
    });
    obs.trace(TraceEvent::StageFinished {
        stage: "Reviewer".into(),
        ended: StageEnd::ReviewSubmitted,
        turns: 1,
        attempts: 1,
        duration_ms: 3_000,
        spend: pipeline::Spend::default(),
    });
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

#[test]
fn a_recorded_run_leaves_every_file_and_the_report_names_what_repeated() {
    let root = scratch_root();
    let caller = SharedObserver::new(NullObserver);
    let analysis = RunAnalysis::start(&root, meta(), &caller).expect("recording starts");
    let dir = analysis.dir();
    let obs = analysis.observer(caller.clone());

    script(&obs);
    analysis.finish(
        RunEnd {
            produced: true,
            form_code: Some("AAOS".into()),
            outputs: Vec::new(),
            warnings: vec![],
        },
        &caller,
    );

    for file in ["README.md", "report.md", "timeline.md", "trace.jsonl", "summary.json", "evaluation.md"] {
        assert!(dir.join(file).exists(), "{file} missing");
    }
    // The evaluation template names the run in the front matter that
    // scripts/collect_evaluations.py reads, and leaves the verdict to a person.
    let evaluation = read(&dir, "evaluation.md");
    assert!(evaluation.starts_with("---\n"), "{evaluation}");
    let folder = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(evaluation.contains(&format!("run_folder: {folder}\n")), "{evaluation}");
    assert!(evaluation.contains("\nverdict:\n"), "{evaluation}");
    for heading in ["## Scores", "## Findings", "## Requirements for v3"] {
        assert!(evaluation.contains(heading), "{heading} missing");
    }
    assert!(dir.join("transcript/01-Author.md").exists());
    assert!(dir.join("transcript/02-Reviewer.md").exists());

    // Every line of the trace is JSON, numbered from 1 with no gaps.
    let trace = read(&dir, "trace.jsonl");
    let lines: Vec<serde_json::Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect();
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(line["seq"], i as u64 + 1, "{line}");
        assert!(line["time"].is_string());
    }
    assert_eq!(lines.first().unwrap()["event"], "run_started");
    assert_eq!(lines.last().unwrap()["event"], "run_finished");
    let stage_opening = lines
        .iter()
        .find(|l| l["event"] == "stage_started" && l["stage"] == "Reviewer")
        .unwrap();
    assert_eq!(stage_opening["stage_index"], 2, "a stage's opening line carries its own index");

    let report = read(&dir, "report.md");
    assert!(report.contains("**Outcome:** a result was produced for form AAOS"), "{report}");
    assert!(report.contains("the Reviewer did not approve it"), "{report}");
    assert!(report.contains("## Stages"), "{report}");
    assert!(report.contains("1. Author"), "{report}");
    assert!(report.contains("## Repetition and possible loops"), "{report}");
    assert!(
        report.contains("1 call(s) repeated an earlier call and got an identical result back"),
        "{report}"
    );
    assert!(report.contains("## Recurring errors"), "{report}");
    assert!(report.contains("The footer is missing."), "{report}");
    assert!(report.contains("output-cap nudge"), "{report}");
    assert!(report.contains("Author: something odd"), "{report}");

    let timeline = read(&dir, "timeline.md");
    assert!(timeline.contains("**Author turn 1** — model 4.0s"), "{timeline}");
    assert!(timeline.contains("`validate_aem_package`"), "{timeline}");
    assert!(timeline.contains("FAILED in 1.5s"), "{timeline}");

    let transcript = read(&dir, "transcript/01-Author.md");
    assert!(transcript.contains("You are the Author."), "{transcript}");
    assert!(transcript.contains("Validating the package now."), "{transcript}");
    assert!(transcript.contains("more detail"), "the transcript keeps results in full");

    let summary: serde_json::Value = serde_json::from_str(&read(&dir, "summary.json")).unwrap();
    assert_eq!(summary["finished"], true);
    assert_eq!(summary["approved"], false);
    assert_eq!(summary["counts"]["tool_calls"], 2);
    assert_eq!(summary["counts"]["failed_tool_calls"], 2);
    assert_eq!(summary["stages"][0]["model_ms"], 8_000);
    assert_eq!(summary["repeated_calls"][0]["distinct_results"], 1);

    let _ = std::fs::remove_dir_all(root);
}

/// A run that never finishes — killed, crashed — must still leave a report
/// covering what happened, since that is the run most in need of explaining.
#[test]
fn an_unfinished_run_still_has_a_report_after_its_first_stage() {
    let root = scratch_root();
    let caller = SharedObserver::new(NullObserver);
    let analysis = RunAnalysis::start(&root, meta(), &caller).unwrap();
    let dir = analysis.dir();
    script(&analysis.observer(caller.clone()));
    drop(analysis);

    let report = read(&dir, "report.md");
    assert!(report.contains("still in progress"), "{report}");
    assert!(report.contains("1. Author"), "{report}");
    let summary: serde_json::Value = serde_json::from_str(&read(&dir, "summary.json")).unwrap();
    assert_eq!(summary["finished"], false);

    let _ = std::fs::remove_dir_all(root);
}

/// A large stage is split into parts, and a huge result is cut in the
/// transcript with a pointer to its full line in the trace.
#[test]
fn large_transcripts_are_split_and_huge_results_point_to_the_trace() {
    let root = scratch_root();
    let caller = SharedObserver::new(NullObserver);
    let analysis = RunAnalysis::start(&root, meta(), &caller).unwrap();
    let dir = analysis.dir();
    let obs = analysis.observer(caller.clone());
    obs.trace(TraceEvent::StageStarted {
        stage: "Author".into(),
        system_prompt: "sys".into(),
        seed_message: "go".into(),
        max_turns: 110,
        tools_offered: vec![],
    });
    let huge = "x".repeat(render::TRANSCRIPT_BLOCK + 10);
    for i in 0..60 {
        obs.trace(TraceEvent::ToolStarted {
            stage: "Author".into(),
            attempt: 1,
            turn: 1,
            call_id: format!("c{i}"),
            name: "get_xfa".into(),
            args: serde_json::json!({}),
        });
        obs.trace(TraceEvent::ToolFinished {
            stage: "Author".into(),
            attempt: 1,
            turn: 1,
            call_id: format!("c{i}"),
            name: "get_xfa".into(),
            ok: true,
            duration_ms: 1,
            result: huge.clone(),
            result_chars: huge.len(),
            image_count: 0,
            image_chars: 0,
            result_hash: "h".into(),
        });
    }
    drop(obs);
    drop(analysis);

    let first = read(&dir, "transcript/01-Author.md");
    assert!(first.contains("the full text is line"), "a cut result says where the rest is");
    assert!(dir.join("transcript/01-Author.part2.md").exists(), "the stage was split");
    assert!(
        (first.len() as u64) < TRANSCRIPT_PART_BYTES + render::TRANSCRIPT_BLOCK as u64 * 4,
        "a part stays near its size cap: {}",
        first.len()
    );
    let trace = read(&dir, "trace.jsonl");
    assert!(trace.contains(&huge), "the trace keeps the result whole");

    let _ = std::fs::remove_dir_all(root);
}

/// Collects the warnings and notes a caller's observer is shown.
#[derive(Default)]
struct Messages {
    warnings: Vec<String>,
    thoughts: Vec<String>,
}

impl RunObserver for Messages {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::Warning(w) => self.warnings.push(w),
            RunEvent::Thought(t) => self.thoughts.push(t),
            _ => {}
        }
    }
    fn retry_prompt(&mut self, _role: &str, _error: &str) {}
    fn poll_retry(&mut self) -> Option<RetryAction> {
        None
    }
    fn retry_resolved(&mut self, _action: RetryAction) {}
}

/// A root that cannot be created leaves the run unrecorded, with one warning
/// saying so — and the run itself is not refused.
#[test]
fn an_unwritable_root_warns_once_and_records_nothing() {
    let root = scratch_root();
    let blocker = root.join("a-file");
    std::fs::write(&blocker, "not a folder").unwrap();
    let messages = Arc::new(Mutex::new(Messages::default()));
    let caller = SharedObserver::from_arc(messages.clone());

    assert!(RunAnalysis::start(&blocker.join("runs"), meta(), &caller).is_none());
    let messages = messages.lock().unwrap();
    assert_eq!(messages.warnings.len(), 1, "{:?}", messages.warnings);
    assert!(messages.warnings[0].contains("will not be recorded"), "{:?}", messages.warnings);
    let _ = std::fs::remove_dir_all(root);
}

/// A failure mid-run is reported once, the run carries on, and the closing
/// message says the folder is incomplete instead of pointing at it as done.
#[test]
fn a_failure_mid_run_is_reported_once_and_the_ending_is_honest() {
    let root = scratch_root();
    let messages = Arc::new(Mutex::new(Messages::default()));
    let caller = SharedObserver::from_arc(messages.clone());
    let analysis = RunAnalysis::start(&root, meta(), &caller).unwrap();
    let dir = analysis.dir();
    let obs = analysis.observer(caller.clone());

    // The folder vanishing under a running recording: the open trace keeps
    // working (on Unix), but the report can no longer be rewritten.
    std::fs::remove_dir_all(&dir).unwrap();
    script(&obs);
    analysis.finish(
        RunEnd {
            produced: true,
            form_code: None,
            outputs: Vec::new(),
            warnings: vec![],
        },
        &caller,
    );

    let messages = messages.lock().unwrap();
    let analysis_warnings: Vec<&String> = messages
        .warnings
        .iter()
        .filter(|w| w.starts_with("Run analysis"))
        .collect();
    assert_eq!(
        analysis_warnings.len(),
        2,
        "one warning for the failure, one for the incomplete folder: {:?}",
        messages.warnings
    );
    assert!(
        analysis_warnings.last().unwrap().contains("is incomplete"),
        "{analysis_warnings:?}"
    );
    assert!(
        !messages.thoughts.iter().any(|t| t.starts_with("Run analysis written")),
        "an incomplete folder must not be announced as written: {:?}",
        messages.thoughts
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Two runs of one session started within the same second get two folders.
#[test]
fn two_runs_never_share_a_folder() {
    let root = scratch_root();
    let caller = SharedObserver::new(NullObserver);
    let first = RunAnalysis::start(&root, meta(), &caller).unwrap();
    let second = RunAnalysis::start(&root, meta(), &caller).unwrap();
    assert_ne!(first.dir(), second.dir());
    let _ = std::fs::remove_dir_all(root);
}

/// Recording off in the settings means no folder at all.
#[test]
fn switching_recording_off_records_nothing() {
    let settings = AppSettings {
        run_analysis: false,
        ..AppSettings::default()
    };
    assert_eq!(root_dir(&settings), None);

    let settings = AppSettings {
        run_analysis_dir: "/tmp/somewhere".into(),
        ..AppSettings::default()
    };
    assert_eq!(root_dir(&settings), Some(PathBuf::from("/tmp/somewhere")));
    assert!(AppSettings::default().run_analysis, "on by default");
}

/// The caller's observer still sees everything, and still answers retries.
#[test]
fn the_callers_observer_is_passed_everything() {
    #[derive(Default)]
    struct Counting {
        events: usize,
        traces: usize,
    }
    impl RunObserver for Counting {
        fn emit(&mut self, _event: RunEvent) {
            self.events += 1;
        }
        fn retry_prompt(&mut self, _role: &str, _error: &str) {}
        fn poll_retry(&mut self) -> Option<RetryAction> {
            Some(RetryAction::Retry)
        }
        fn retry_resolved(&mut self, _action: RetryAction) {}
        fn trace(&mut self, _event: TraceEvent) {
            self.traces += 1;
        }
    }

    let root = scratch_root();
    let counting = Arc::new(Mutex::new(Counting::default()));
    let caller = SharedObserver::from_arc(counting.clone());
    let analysis = RunAnalysis::start(&root, meta(), &caller).unwrap();
    let obs = analysis.observer(caller.clone());
    script(&obs);
    assert_eq!(obs.poll_retry(), Some(RetryAction::Retry));
    drop(obs);
    drop(analysis);

    let counting = counting.lock().unwrap();
    assert_eq!(counting.traces, 15);
    // The "recording in …" note, the stage announcement and the warning.
    assert_eq!(counting.events, 3);
    let _ = std::fs::remove_dir_all(root);
}
