//! The run analyzer: records every conversion run into a folder of files a
//! person — or an AI assistant — can read afterwards to see where the run
//! spent its time, where it repeated itself, which errors recurred and what
//! the Reviewer found.
//!
//! It sits between the controller and the caller's own observer
//! ([`RecordingObserver`]), so the app and the CLI both get it from
//! [`crate::run`] without doing anything: every [`RunEvent`] and every
//! [`TraceEvent`] passes through on its way to the caller's observer, and is
//! written down as it passes. Files are appended as the run goes and flushed
//! line by line, so a run that crashes, is killed or is aborted still leaves
//! a complete record up to that moment.
//!
//! What is written, per run, into `<root>/<date>_<time>_<label>_<session>/`:
//! `README.md` (what the files are and how to use them), `report.md` (the
//! analysis, rewritten after every stage), `timeline.md`, one transcript per
//! stage under `transcript/`, `trace.jsonl` (everything, machine-readable) and
//! `summary.json` (the report's figures as data, for comparing runs). See
//! [`render::readme`] for the reader's guide.
//!
//! Recording is best-effort by construction: a file that cannot be written
//! turns recording off for the rest of the run with one warning, and never
//! stops or slows the conversion itself.

pub mod format;
pub mod render;
pub mod stats;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pipeline::{RetryAction, RunEvent, RunObserver, SharedObserver, TraceEvent};
use serde::Serialize;

use crate::settings::AppSettings;
use stats::Stats;

/// Folder that runs are recorded into when the settings name none (see
/// [`default_root`]).
const DEFAULT_DIR_NAME: &str = "run-analysis";
/// A stage's transcript is split into parts of about this size, so no single
/// file is too large to open or to hand to an AI assistant whole.
const TRANSCRIPT_PART_BYTES: u64 = 1_500_000;
/// The human evaluation of the run's result, filled in by hand afterwards and
/// collected across runs by `scripts/collect_evaluations.py`.
pub const EVALUATION_FILE: &str = "evaluation.md";
/// Version of the `trace.jsonl` / `summary.json` layout.
pub const SCHEMA_VERSION: u32 = 1;

/// Where runs are recorded, resolved from the settings: `None` when recording
/// is switched off.
pub fn root_dir(settings: &AppSettings) -> Option<PathBuf> {
    if !settings.run_analysis {
        return None;
    }
    let configured = settings.run_analysis_dir.trim();
    if !configured.is_empty() {
        return Some(PathBuf::from(configured));
    }
    Some(default_root())
}

/// Where runs go when the settings name no folder: `run-analysis/` at the
/// root of the checkout the engine was built from, so every run sits next to
/// the code that produced it. A binary moved away from its checkout falls back
/// to `<config_dir>/blueprint/run-analysis`.
pub fn default_root() -> PathBuf {
    if let Some(repo) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        if repo.join("Cargo.toml").is_file() {
            return repo.join(DEFAULT_DIR_NAME);
        }
    }
    let base = dirs::config_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
    });
    base.join("blueprint").join(DEFAULT_DIR_NAME)
}

/// What the run was, recorded at its start.
#[derive(Clone, Debug, Serialize)]
pub struct RunMeta {
    /// What is being converted — the source file names.
    pub label: String,
    pub session_id: String,
    /// "fresh conversion", "feedback round: …", "continuation".
    pub kind: String,
    /// Local start time, RFC 3339.
    pub started: String,
    pub profile: String,
    pub target: String,
    /// The endpoint and model, as the run's plan describes them.
    pub model: String,
    pub max_review_rounds: usize,
    /// The verifier the run checks its output with (u2s: AEM or Redacto).
    pub verification: String,
    pub engine_version: String,
}

/// How the run ended, recorded when it returns.
#[derive(Clone, Debug, Serialize)]
pub struct RunEnd {
    /// Whether the run produced a result at all (`false`: aborted, or given up).
    pub produced: bool,
    pub form_code: Option<String>,
    /// The artefacts the run produced (package, schema, SQL dump).
    pub outputs: Vec<String>,
    pub warnings: Vec<String>,
}

impl RunEnd {
    /// One sentence for the top of the report.
    pub fn describe(&self, approved: Option<bool>) -> String {
        if !self.produced {
            return "the run stopped before producing a result (aborted, or given up after a failed \
                    request)."
                .into();
        }
        let review = match approved {
            Some(true) => "the Reviewer approved it",
            Some(false) => "the Reviewer did not approve it",
            None => "there was no review verdict",
        };
        let code = self
            .form_code
            .as_deref()
            .map(|c| format!(" for form {c}"))
            .unwrap_or_default();
        let outputs = if self.outputs.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.outputs.join(", "))
        };
        format!("a result was produced{code}{outputs}; {review}.")
    }
}

/// One line of `trace.jsonl`: the envelope every event shares, then the
/// event's own fields.
#[derive(Serialize)]
struct Line<'a, T: Serialize> {
    seq: u64,
    time: &'a str,
    elapsed_ms: u64,
    stage_index: usize,
    #[serde(flatten)]
    body: &'a T,
}

/// The `trace.jsonl` records that are not trace events.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum RunRecord<'a> {
    RunStarted {
        schema_version: u32,
        #[serde(flatten)]
        meta: &'a RunMeta,
    },
    Progress {
        kind: &'static str,
        text: &'a str,
    },
    Warning {
        text: &'a str,
    },
    RunFinished {
        wall_ms: u64,
        model_ms: u64,
        tool_ms: u64,
        approved: Option<bool>,
        spend: pipeline::Spend,
        #[serde(flatten)]
        end: &'a RunEnd,
    },
}

/// The one transcript file currently being appended to.
struct TranscriptFile {
    stage_index: usize,
    part: usize,
    file: File,
    bytes: u64,
}

/// The files of one run and everything accumulated for its report.
pub struct RunRecorder {
    dir: PathBuf,
    meta: RunMeta,
    started: Instant,
    seq: u64,
    trace: Option<File>,
    timeline: Option<File>,
    transcript: Option<TranscriptFile>,
    stats: Stats,
    /// The first failure of an append-only file; once set, nothing more is
    /// written — a trace with a gap in it would mislead.
    failed: Option<String>,
    /// The latest failure to rewrite `report.md`/`summary.json`. Those are
    /// rewritten whole every stage, so a failure (a viewer holding the file
    /// open on Windows, say) does not stop the recording: the next rewrite
    /// may well succeed.
    report_failed: Option<String>,
    /// Problems not yet passed on to the operator, each passed on once.
    unreported: Vec<String>,
}

impl RunRecorder {
    /// Create the run's folder under `root` and write its opening files.
    pub fn create(root: &Path, meta: RunMeta) -> Result<Self, String> {
        let dir = new_run_dir(root, &meta)?;
        std::fs::create_dir(dir.join("transcript"))
            .map_err(|e| format!("could not create {}: {e}", dir.join("transcript").display()))?;
        let open = |file: &str| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(file))
                .map_err(|e| format!("could not create {}: {e}", dir.join(file).display()))
        };
        std::fs::write(dir.join("README.md"), render::readme(&meta))
            .map_err(|e| format!("could not write README.md: {e}"))?;
        // For the person checking the result: written once, never overwritten.
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        std::fs::write(dir.join(EVALUATION_FILE), render::evaluation_template(&meta, &folder))
            .map_err(|e| format!("could not write {EVALUATION_FILE}: {e}"))?;
        let trace = open("trace.jsonl")?;
        let mut timeline = open("timeline.md")?;
        write!(
            timeline,
            "# Timeline — {}\n\nSession `{}` ({}), started {}. Model: {}.\n\n\
             Each entry starts with the time since the run started and `#seq`, the entry's line in \
             `trace.jsonl`. Arguments and results are cut short here; `transcript/` has them in full.\n",
            meta.label, meta.session_id, meta.kind, meta.started, meta.model
        )
        .map_err(|e| format!("could not write timeline.md: {e}"))?;
        let mut recorder = Self {
            dir,
            meta,
            started: Instant::now(),
            seq: 0,
            trace: Some(trace),
            timeline: Some(timeline),
            transcript: None,
            stats: Stats::default(),
            failed: None,
            report_failed: None,
            unreported: Vec::new(),
        };
        let meta = recorder.meta.clone();
        recorder.write_line(&RunRecord::RunStarted {
            schema_version: SCHEMA_VERSION,
            meta: &meta,
        });
        recorder.write_report(None);
        Ok(recorder)
    }

    /// The run's folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn elapsed_ms(&self) -> u64 {
        pipeline::trace::elapsed_ms(self.started)
    }

    /// Stop recording for good: an append-only file could not be written.
    fn fail(&mut self, error: String) {
        if self.failed.is_none() {
            self.unreported
                .push(format!("recording stopped: {error}"));
            self.failed = Some(error);
        }
        self.trace = None;
        self.timeline = None;
        self.transcript = None;
    }

    /// The report could not be rewritten this time; recording goes on.
    fn report_failure(&mut self, error: String) {
        if self.report_failed.is_none() {
            self.unreported.push(format!(
                "{error} (the recording continues; the report is retried after the next stage)"
            ));
        }
        self.report_failed = Some(error);
    }

    /// Problems not yet passed on to the operator — each is passed on once.
    fn take_unreported(&mut self) -> Vec<String> {
        std::mem::take(&mut self.unreported)
    }

    /// Why the folder is incomplete, if it is: recording stopped, or the last
    /// attempt to write the report failed.
    fn incomplete(&self) -> Option<String> {
        self.failed.clone().or_else(|| self.report_failed.clone())
    }

    /// Append one record to `trace.jsonl`; returns its `seq`.
    fn write_line<T: Serialize>(&mut self, body: &T) -> u64 {
        self.seq += 1;
        let seq = self.seq;
        let Some(file) = self.trace.as_mut() else {
            return seq;
        };
        let time = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let line = Line {
            seq,
            time: &time,
            elapsed_ms: pipeline::trace::elapsed_ms(self.started),
            stage_index: self.stats.current_stage_index(),
            body,
        };
        let result = serde_json::to_string(&line)
            .map_err(|e| e.to_string())
            .and_then(|json| writeln!(file, "{json}").map_err(|e| e.to_string()))
            .and_then(|()| file.flush().map_err(|e| e.to_string()));
        if let Err(e) = result {
            self.fail(format!("could not write trace.jsonl: {e}"));
        }
        seq
    }

    fn append_timeline(&mut self, text: &str) {
        let Some(file) = self.timeline.as_mut() else {
            return;
        };
        if let Err(e) = file.write_all(text.as_bytes()).and_then(|()| file.flush()) {
            self.fail(format!("could not write timeline.md: {e}"));
        }
    }

    /// Append to the current stage's transcript, starting a new file for a
    /// new stage and a new part when the current one has grown too large.
    fn append_transcript(&mut self, text: &str) {
        if self.failed.is_some() {
            return;
        }
        let Some(stage) = self.stats.current_stage() else {
            return;
        };
        let (index, name) = (stage.index, stage.name.clone());
        let needs_new = match &self.transcript {
            None => Some(1),
            Some(t) if t.stage_index != index => Some(1),
            Some(t) if t.bytes >= TRANSCRIPT_PART_BYTES => Some(t.part + 1),
            Some(_) => None,
        };
        if let Some(part) = needs_new {
            let file_name = if part == 1 {
                format!("{index:02}-{}.md", format::slug(&name, 32))
            } else {
                format!("{index:02}-{}.part{part}.md", format::slug(&name, 32))
            };
            let path = self.dir.join("transcript").join(&file_name);
            match File::create(&path) {
                Ok(mut file) => {
                    let header = render::transcript_header(index, &name, part);
                    if let Err(e) = file.write_all(header.as_bytes()) {
                        self.fail(format!("could not write {}: {e}", path.display()));
                        return;
                    }
                    self.transcript = Some(TranscriptFile {
                        stage_index: index,
                        part,
                        file,
                        bytes: header.len() as u64,
                    });
                }
                Err(e) => {
                    self.fail(format!("could not create {}: {e}", path.display()));
                    return;
                }
            }
        }
        if let Some(t) = self.transcript.as_mut() {
            match t.file.write_all(text.as_bytes()).and_then(|()| t.file.flush()) {
                Ok(()) => t.bytes += text.len() as u64,
                Err(e) => self.fail(format!("could not write a transcript: {e}")),
            }
        }
    }

    /// Record one trace event in every file it belongs in.
    pub fn record_trace(&mut self, event: &TraceEvent) {
        if self.failed.is_some() {
            return;
        }
        let at_ms = self.elapsed_ms();
        // Stage events are folded in before the line is written, so the line
        // carries the stage it opens; the rest are folded in after.
        let opens_stage = matches!(event, TraceEvent::StageStarted { .. });
        if opens_stage {
            self.stats.record(self.seq + 1, at_ms, event);
        }
        let seq = self.write_line(event);
        if !opens_stage {
            self.stats.record(seq, at_ms, event);
        }
        let clock = format::clock(at_ms);
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        if let Some(entry) = render::timeline_entry(seq, &clock, &self.stats, event) {
            self.append_timeline(&entry);
        }
        if let Some(entry) = render::transcript_entry(seq, &clock, &time, event) {
            self.append_transcript(&entry);
        }
        // A verdict arrives after its Reviewer stage has closed, and is the
        // one finding a run killed during the next stage must not lose.
        if matches!(
            event,
            TraceEvent::StageFinished { .. } | TraceEvent::ReviewVerdict { .. }
        ) {
            self.write_report(None);
        }
    }

    /// Record one progress event.
    pub fn record_event(&mut self, event: &RunEvent) {
        if self.failed.is_some() {
            return;
        }
        match event {
            RunEvent::Stage { doing, .. } => self.stats.announce_stage(doing.clone()),
            RunEvent::Spend(spend) => self.stats.spend = *spend,
            RunEvent::Warning(text) => {
                let at_ms = self.elapsed_ms();
                self.stats.warnings.push((at_ms, text.clone()));
                let seq = self.write_line(&RunRecord::Warning { text });
                let note = render::timeline_note(seq, &format::clock(at_ms), "Warning", text);
                self.append_timeline(&note);
            }
            // Model text arrives as `Thought`s, and in full in `TurnFinished`
            // already; tool calls, retries and aborts have their own trace
            // events. Recording these too would only duplicate them.
            RunEvent::Thought(_)
            | RunEvent::ToolStarted { .. }
            | RunEvent::ToolFinished { .. }
            | RunEvent::ContextUsed(_)
            | RunEvent::Aborted => {}
        }
    }

    /// Record a controller note that is neither a trace event nor a warning —
    /// the analysis folder's own announcements.
    fn record_note(&mut self, text: &str) {
        if self.failed.is_some() {
            return;
        }
        let at_ms = self.elapsed_ms();
        let seq = self.write_line(&RunRecord::Progress { kind: "note", text });
        let note = render::timeline_note(seq, &format::clock(at_ms), "Note", text);
        self.append_timeline(&note);
    }

    fn write_report(&mut self, outcome: Option<&RunEnd>) {
        if self.failed.is_some() {
            return;
        }
        let elapsed_ms = self.elapsed_ms();
        let report = render::report(&render::ReportContext {
            meta: &self.meta,
            stats: &self.stats,
            elapsed_ms,
            outcome,
        });
        let summary = self.summary(elapsed_ms, outcome);
        let written = write_atomically(&self.dir.join("report.md"), report.as_bytes())
            .map_err(|e| format!("could not write report.md: {e}"))
            .and_then(|()| {
                serde_json::to_vec_pretty(&summary)
                    .map_err(|e| format!("could not serialize summary.json: {e}"))
            })
            .and_then(|bytes| {
                write_atomically(&self.dir.join("summary.json"), &bytes)
                    .map_err(|e| format!("could not write summary.json: {e}"))
            });
        match written {
            Ok(()) => self.report_failed = None,
            Err(e) => self.report_failure(e),
        }
    }

    fn summary(&self, elapsed_ms: u64, outcome: Option<&RunEnd>) -> serde_json::Value {
        let s = &self.stats;
        serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "run": self.meta,
            "finished": outcome.is_some(),
            "outcome": outcome,
            "approved": s.approved(),
            "time": {
                "wall_ms": elapsed_ms,
                "model_ms": s.model_ms(),
                "tool_ms": s.tool_ms(),
                "shaping_ms": s.shaping_ms(),
                "failed_request_ms": s.failed_request_ms(),
                "other_ms": s.other_ms(elapsed_ms),
            },
            "spend": s.spend,
            "counts": {
                "stages": s.stages.len(),
                "turns": s.turns.len(),
                "tool_calls": s.calls.len(),
                "failed_tool_calls": s.calls.iter().filter(|c| !c.ok).count(),
                "failed_requests": s.failed_requests.len(),
            },
            "stages": s.stages,
            "tools": s.tools_by_time(),
            "control_counts": s.control_counts(),
            "repeated_calls": s.repeated_calls(),
            "recurring_errors": s.recurring_errors(),
            "streaks": s.streaks(5),
            "edit_activity": s.edit_activity(),
            "review_verdicts": s.verdicts.iter().map(|v| serde_json::json!({
                "at_ms": v.at_ms,
                "round": v.round,
                "approved": v.approved,
                "verdict": v.describe(),
                "report": format::excerpt(v.report.trim(), 4_000),
            })).collect::<Vec<_>>(),
            "failed_requests": s.failed_requests,
            "warnings": s.warnings.iter().map(|(_, w)| w).collect::<Vec<_>>(),
        })
    }

    /// Close the run: the final record, report and summary.
    pub fn finish(&mut self, end: &RunEnd) {
        if self.failed.is_some() {
            return;
        }
        let wall_ms = self.elapsed_ms();
        let record = RunRecord::RunFinished {
            wall_ms,
            model_ms: self.stats.model_ms(),
            tool_ms: self.stats.tool_ms(),
            approved: self.stats.approved(),
            spend: self.stats.spend,
            end,
        };
        let seq = self.write_line(&record);
        let note = render::timeline_note(
            seq,
            &format::clock(wall_ms),
            "Run finished",
            &format!(
                "{} Wall time {}. {}",
                end.describe(self.stats.approved()),
                format::duration(wall_ms),
                self.stats.spend.describe()
            ),
        );
        self.append_timeline(&note);
        self.write_report(Some(end));
    }
}

/// Create a fresh folder for the run under `root`, never reusing one: two
/// runs of the same session started within the same second would otherwise
/// append to one trace.
fn new_run_dir(root: &Path, meta: &RunMeta) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("could not create {}: {e}", root.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string();
    let short_session: String = meta.session_id.chars().take(8).collect();
    let base = format!(
        "{stamp}_{}_{}",
        format::slug(&meta.label, 48),
        format::slug(&short_session, 8)
    );
    for n in 1..100 {
        let name = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        let dir = root.join(name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create {}: {e}", dir.display())),
        }
    }
    Err(format!("could not find a free folder name for {base} in {}", root.display()))
}

/// Write `bytes` to `path` through a temporary file and a rename, so a reader
/// never sees a half-written report.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// A handle on one run's recording, held by [`crate::run`] for the run's
/// duration.
pub struct RunAnalysis {
    recorder: Arc<Mutex<RunRecorder>>,
}

impl RunAnalysis {
    /// Start recording under `root`. On failure the run goes ahead unrecorded,
    /// and the observer is told why.
    pub fn start(root: &Path, meta: RunMeta, obs: &SharedObserver) -> Option<Self> {
        match RunRecorder::create(root, meta) {
            Ok(mut recorder) => {
                let note = format!(
                    "Recording this run for analysis in {}",
                    recorder.dir().display()
                );
                recorder.record_note(&note);
                obs.emit(RunEvent::Thought(note));
                Some(Self {
                    recorder: Arc::new(Mutex::new(recorder)),
                })
            }
            Err(e) => {
                obs.emit(RunEvent::Warning(format!(
                    "The run will not be recorded for analysis: {e}"
                )));
                None
            }
        }
    }

    /// An observer that records everything on its way to `inner`.
    pub fn observer(&self, inner: SharedObserver) -> SharedObserver {
        SharedObserver::new(RecordingObserver {
            inner,
            recorder: self.recorder.clone(),
        })
    }

    /// The run's folder.
    pub fn dir(&self) -> PathBuf {
        self.lock().dir().to_path_buf()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RunRecorder> {
        self.recorder.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Close the recording and tell the observer where it is.
    pub fn finish(self, end: RunEnd, obs: &SharedObserver) {
        let (dir, unreported, incomplete) = {
            let mut recorder = self.lock();
            recorder.finish(&end);
            (
                recorder.dir().to_path_buf(),
                recorder.take_unreported(),
                recorder.incomplete(),
            )
        };
        for problem in unreported {
            obs.emit(RunEvent::Warning(format!("Run analysis: {problem}")));
        }
        match incomplete {
            Some(e) => obs.emit(RunEvent::Warning(format!(
                "Run analysis: the folder {} is incomplete: {e}",
                dir.display()
            ))),
            None => obs.emit(RunEvent::Thought(format!(
                "Run analysis written to {} — start with report.md.",
                dir.display()
            ))),
        }
    }
}

/// Records every event on its way to the caller's observer. Retry prompts are
/// answered by the caller's observer, untouched.
pub struct RecordingObserver {
    inner: SharedObserver,
    recorder: Arc<Mutex<RunRecorder>>,
}

impl RecordingObserver {
    /// Run `f` on the recorder, and pass a new recording failure on to the
    /// caller's observer once, as a warning.
    fn with_recorder(&self, f: impl FnOnce(&mut RunRecorder)) {
        let problems = {
            let mut recorder = self.recorder.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut recorder);
            recorder.take_unreported()
        };
        for problem in problems {
            self.inner
                .emit(RunEvent::Warning(format!("Run analysis: {problem}")));
        }
    }
}

impl RunObserver for RecordingObserver {
    fn emit(&mut self, event: RunEvent) {
        self.with_recorder(|r| r.record_event(&event));
        self.inner.emit(event);
    }

    fn retry_prompt(&mut self, role: &str, error: &str) {
        self.inner.retry_prompt(role, error);
    }

    fn poll_retry(&mut self) -> Option<RetryAction> {
        self.inner.poll_retry()
    }

    fn retry_resolved(&mut self, action: RetryAction) {
        self.inner.retry_resolved(action);
    }

    fn trace(&mut self, event: TraceEvent) {
        self.with_recorder(|r| r.record_trace(&event));
        self.inner.trace(event);
    }
}

#[cfg(test)]
mod tests;
