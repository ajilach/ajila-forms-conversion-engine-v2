//! The run's figures, accumulated event by event while it happens: what the
//! report and `summary.json` are rendered from.
//!
//! Only what the analysis needs is kept — counts, durations, hashes and short
//! excerpts — never the full tool results, which live in `trace.jsonl` and the
//! transcripts. A run of thousands of tool calls stays a few megabytes here.

use std::collections::BTreeMap;

use pipeline::{ControlKind, Spend, StageEnd, TraceEvent};

use serde::Serialize;

use super::format;

/// How many characters of a tool call's arguments the figures keep.
const ARGS_EXCERPT: usize = 240;
/// How many characters of an error's text the figures keep.
const ERROR_EXCERPT: usize = 400;
/// Tool-name prefixes that change the form, for the edit-activity figures.
const WRITE_PREFIXES: &[&str] = &["set_", "insert_", "replace_", "remove_", "write_", "seed_"];
/// Argument keys that usually name what a write tool changes.
const TARGET_KEYS: &[&str] = &[
    "path", "node_path", "node_id", "id", "target", "name", "field", "panel", "parent",
];

#[derive(Clone, Debug, Default, Serialize)]
pub struct StageStats {
    /// 1-based position of the stage in the run.
    pub index: usize,
    pub name: String,
    /// What the controller said the stage was doing ("reviewing (round 2)").
    pub doing: String,
    /// When it started, in milliseconds since the run started.
    pub started_ms: u64,
    pub duration_ms: u64,
    /// `None` while the stage is still running.
    pub ended: Option<StageEnd>,
    pub turns: usize,
    pub attempts: usize,
    pub max_turns: usize,
    pub tool_calls: usize,
    pub tool_failures: usize,
    /// Time spent waiting for the model, summed over the stage's turns.
    pub model_ms: u64,
    /// Time spent executing tools, summed over the stage's calls.
    pub tool_ms: u64,
    /// Time spent shaping requests to fit the context budget.
    pub shaping_ms: u64,
    /// Requests that failed before the model answered, and the time they
    /// took to fail.
    pub failed_requests: usize,
    pub failed_request_ms: u64,
    pub spend: Spend,
    /// The largest prompt any turn of the stage sent, in tokens.
    pub max_prompt_tokens: u64,
    /// Turns whose history the context budget had to shorten.
    pub shaped_turns: usize,
    pub system_prompt_chars: usize,
    pub tools_offered: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolStats {
    pub name: String,
    pub calls: usize,
    pub failures: usize,
    pub total_ms: u64,
    pub max_ms: u64,
    pub result_chars: u64,
    pub max_result_chars: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct TurnRecord {
    pub stage_index: usize,
    pub stage: String,
    pub turn: usize,
    pub at_ms: u64,
    pub latency_ms: u64,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub finish_reason: Option<String>,
    pub tool_names: Vec<String>,
    pub text_excerpt: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CallRecord {
    /// The call's line in `trace.jsonl` (its `seq`), to find the full content.
    pub seq: u64,
    pub stage_index: usize,
    pub stage: String,
    pub turn: usize,
    pub at_ms: u64,
    pub name: String,
    pub args_hash: String,
    pub args_excerpt: String,
    /// What the call changed, when its arguments name it (write tools only).
    pub target: Option<String>,
    pub ok: bool,
    pub duration_ms: u64,
    pub result_hash: String,
    pub result_chars: usize,
    /// The start of the result when the call failed.
    pub error: Option<String>,
}

/// One review round's verdict.
#[derive(Clone, Debug, Serialize)]
pub struct VerdictRecord {
    pub at_ms: u64,
    pub stage_index: usize,
    pub round: usize,
    /// `None`: the Reviewer ended without giving one.
    pub approved: Option<bool>,
    pub report: String,
}

impl VerdictRecord {
    /// "approved", "changes requested", "no verdict".
    pub fn describe(&self) -> &'static str {
        match self.approved {
            Some(true) => "approved",
            Some(false) => "changes requested",
            None => "no verdict",
        }
    }
}

/// A request that failed before the model answered.
#[derive(Clone, Debug, Serialize)]
pub struct FailedRequest {
    pub seq: u64,
    pub at_ms: u64,
    pub stage_index: usize,
    pub stage: String,
    pub turn: usize,
    pub latency_ms: u64,
    pub error: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlRecord {
    pub at_ms: u64,
    pub stage_index: usize,
    pub stage: String,
    pub kind: ControlKind,
    pub detail: String,
}

/// The same call — same tool, same arguments — made more than once.
#[derive(Clone, Debug, Serialize)]
pub struct RepeatedCall {
    pub name: String,
    pub args_excerpt: String,
    pub count: usize,
    /// How many distinct results those calls got back: 1 means every repeat
    /// learned nothing new.
    pub distinct_results: usize,
    pub stages: Vec<String>,
    pub total_ms: u64,
    pub seqs: Vec<u64>,
}

/// Failures of one tool that share an error line, once digits and quoted
/// names are folded away.
#[derive(Clone, Debug, Serialize)]
pub struct RecurringError {
    pub tool: String,
    pub pattern: String,
    pub count: usize,
    pub example: String,
    pub stages: Vec<String>,
    pub seqs: Vec<u64>,
}

/// A run of consecutive calls to one tool within a stage.
#[derive(Clone, Debug, Serialize)]
pub struct Streak {
    pub stage_index: usize,
    pub stage: String,
    pub tool: String,
    pub length: usize,
    pub from_seq: u64,
    pub to_seq: u64,
    pub total_ms: u64,
}

/// What one stage changed with write tools, for the parallelism question.
#[derive(Clone, Debug, Serialize)]
pub struct EditActivity {
    pub stage_index: usize,
    pub stage: String,
    pub write_calls: usize,
    pub distinct_targets: usize,
    pub turns_with_writes: usize,
    pub turns_with_several_calls: usize,
    pub by_tool: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub stages: Vec<StageStats>,
    pub tools: BTreeMap<String, ToolStats>,
    pub turns: Vec<TurnRecord>,
    pub calls: Vec<CallRecord>,
    pub controls: Vec<ControlRecord>,
    pub verdicts: Vec<VerdictRecord>,
    pub failed_requests: Vec<FailedRequest>,
    pub warnings: Vec<(u64, String)>,
    /// The run's cumulative spend, as last reported.
    pub spend: Spend,
    /// Starts of calls still running, by call id: (seq, ms, name, args).
    #[serde(skip)]
    open_calls: BTreeMap<String, (u64, u64, String, serde_json::Value)>,
    /// What the next stage to start was announced as doing.
    #[serde(skip)]
    pending_doing: Option<String>,
}

impl Stats {
    /// The stage events are attributed to: the last one started.
    pub fn current_stage(&self) -> Option<&StageStats> {
        self.stages.last()
    }

    pub fn current_stage_index(&self) -> usize {
        self.stages.last().map_or(0, |s| s.index)
    }

    /// Remember what the controller announced the next stage as doing.
    pub fn announce_stage(&mut self, doing: String) {
        self.pending_doing = Some(doing);
    }

    fn stage_mut(&mut self) -> Option<&mut StageStats> {
        self.stages.last_mut()
    }

    /// Fold one trace event in. `seq` is its line in `trace.jsonl`, `at_ms`
    /// when it arrived relative to the run's start.
    pub fn record(&mut self, seq: u64, at_ms: u64, event: &TraceEvent) {
        match event {
            TraceEvent::StageStarted {
                stage,
                system_prompt,
                max_turns,
                tools_offered,
                ..
            } => {
                let index = self.stages.len() + 1;
                self.stages.push(StageStats {
                    index,
                    name: stage.clone(),
                    doing: self.pending_doing.take().unwrap_or_default(),
                    started_ms: at_ms,
                    max_turns: *max_turns,
                    system_prompt_chars: system_prompt.chars().count(),
                    tools_offered: tools_offered.len(),
                    ..StageStats::default()
                });
            }
            TraceEvent::AttemptStarted { attempt, .. } => {
                if let Some(stage) = self.stage_mut() {
                    stage.attempts = stage.attempts.max(*attempt);
                }
            }
            TraceEvent::TurnStarted {
                history_messages,
                sent_messages,
                shaping_ms,
                ..
            } => {
                if let Some(stage) = self.stage_mut() {
                    stage.shaping_ms += shaping_ms;
                    if sent_messages < history_messages {
                        stage.shaped_turns += 1;
                    }
                }
            }
            TraceEvent::RequestFailed {
                stage,
                turn,
                latency_ms,
                error,
                ..
            } => {
                let stage_index = self.current_stage_index();
                if let Some(s) = self.stage_mut() {
                    s.failed_requests += 1;
                    s.failed_request_ms += latency_ms;
                }
                self.failed_requests.push(FailedRequest {
                    seq,
                    at_ms,
                    stage_index,
                    stage: stage.clone(),
                    turn: *turn,
                    latency_ms: *latency_ms,
                    error: format::excerpt(error, ERROR_EXCERPT),
                });
            }
            TraceEvent::ReviewVerdict {
                round,
                approved,
                report,
                ..
            } => {
                let stage_index = self.current_stage_index();
                self.verdicts.push(VerdictRecord {
                    at_ms,
                    stage_index,
                    round: *round,
                    approved: *approved,
                    report: report.clone(),
                });
            }
            TraceEvent::TurnFinished {
                stage,
                turn,
                latency_ms,
                finish_reason,
                usage,
                text,
                tool_calls,
                ..
            } => {
                let stage_index = self.current_stage_index();
                if let Some(s) = self.stage_mut() {
                    s.turns = s.turns.max(*turn);
                    s.model_ms += latency_ms;
                    s.max_prompt_tokens = s.max_prompt_tokens.max(usage.prompt_tokens());
                }
                self.turns.push(TurnRecord {
                    stage_index,
                    stage: stage.clone(),
                    turn: *turn,
                    at_ms,
                    latency_ms: *latency_ms,
                    prompt_tokens: usage.prompt_tokens(),
                    output_tokens: usage.output_tokens,
                    finish_reason: finish_reason.clone(),
                    tool_names: tool_calls.iter().map(|c| c.name.clone()).collect(),
                    text_excerpt: format::excerpt(&format::one_line(text), 300),
                });
            }
            TraceEvent::ToolStarted {
                call_id, name, args, ..
            } => {
                self.open_calls
                    .insert(call_id.clone(), (seq, at_ms, name.clone(), args.clone()));
            }
            TraceEvent::ToolFinished {
                stage,
                turn,
                call_id,
                name,
                ok,
                duration_ms,
                result,
                result_chars,
                result_hash,
                ..
            } => {
                let (start_seq, start_ms, args) = match self.open_calls.remove(call_id) {
                    Some((s, ms, _, args)) => (s, ms, args),
                    None => (seq, at_ms, serde_json::Value::Null),
                };
                let args_text = if args.is_null() {
                    String::new()
                } else {
                    args.to_string()
                };
                let stage_index = self.current_stage_index();
                if let Some(s) = self.stage_mut() {
                    s.tool_calls += 1;
                    s.tool_ms += duration_ms;
                    if !ok {
                        s.tool_failures += 1;
                    }
                }
                let tool = self.tools.entry(name.clone()).or_insert_with(|| ToolStats {
                    name: name.clone(),
                    ..ToolStats::default()
                });
                tool.calls += 1;
                tool.total_ms += duration_ms;
                tool.max_ms = tool.max_ms.max(*duration_ms);
                tool.result_chars += *result_chars as u64;
                tool.max_result_chars = tool.max_result_chars.max(*result_chars);
                if !ok {
                    tool.failures += 1;
                }
                self.calls.push(CallRecord {
                    seq: start_seq,
                    stage_index,
                    stage: stage.clone(),
                    turn: *turn,
                    at_ms: start_ms,
                    name: name.clone(),
                    args_hash: pipeline::trace::stable_hash(&args_text),
                    args_excerpt: format::excerpt(&args_text, ARGS_EXCERPT),
                    target: write_target(name, &args),
                    ok: *ok,
                    duration_ms: *duration_ms,
                    result_hash: result_hash.clone(),
                    result_chars: *result_chars,
                    error: (!ok).then(|| format::excerpt(result.trim(), ERROR_EXCERPT)),
                });
            }
            TraceEvent::Control { stage, kind, detail } => {
                let stage_index = self.current_stage_index();
                self.controls.push(ControlRecord {
                    at_ms,
                    stage_index,
                    stage: stage.clone(),
                    kind: *kind,
                    detail: detail.clone(),
                });
            }
            TraceEvent::StageFinished {
                ended,
                turns,
                attempts,
                duration_ms,
                spend,
                ..
            } => {
                if let Some(s) = self.stage_mut() {
                    s.ended = Some(ended.clone());
                    s.turns = s.turns.max(*turns);
                    s.attempts = s.attempts.max(*attempts);
                    s.duration_ms = *duration_ms;
                    s.spend = *spend;
                }
            }
        }
    }

    /// Wall time spent waiting for the model, over the whole run.
    pub fn model_ms(&self) -> u64 {
        self.stages.iter().map(|s| s.model_ms).sum()
    }

    /// Wall time spent executing tools, over the whole run.
    pub fn tool_ms(&self) -> u64 {
        self.stages.iter().map(|s| s.tool_ms).sum()
    }

    /// Wall time spent shaping requests, over the whole run.
    pub fn shaping_ms(&self) -> u64 {
        self.stages.iter().map(|s| s.shaping_ms).sum()
    }

    /// Wall time spent on requests that failed, over the whole run.
    pub fn failed_request_ms(&self) -> u64 {
        self.stages.iter().map(|s| s.failed_request_ms).sum()
    }

    /// What the four measured kinds of time leave unexplained: retry waits,
    /// operator pauses, the controller and the tools' own bookkeeping.
    pub fn other_ms(&self, wall_ms: u64) -> u64 {
        wall_ms.saturating_sub(
            self.model_ms() + self.tool_ms() + self.shaping_ms() + self.failed_request_ms(),
        )
    }

    /// Tools, most time-consuming first.
    pub fn tools_by_time(&self) -> Vec<&ToolStats> {
        let mut tools: Vec<&ToolStats> = self.tools.values().collect();
        tools.sort_by(|a, b| b.total_ms.cmp(&a.total_ms).then(b.calls.cmp(&a.calls)));
        tools
    }

    /// The same call made twice or more, most repeated first.
    pub fn repeated_calls(&self) -> Vec<RepeatedCall> {
        let mut groups: BTreeMap<(String, String), Vec<&CallRecord>> = BTreeMap::new();
        for call in &self.calls {
            groups
                .entry((call.name.clone(), call.args_hash.clone()))
                .or_default()
                .push(call);
        }
        let mut repeated: Vec<RepeatedCall> = groups
            .into_values()
            .filter(|calls| calls.len() > 1)
            .map(|calls| {
                let mut results: Vec<&str> = calls.iter().map(|c| c.result_hash.as_str()).collect();
                results.sort_unstable();
                results.dedup();
                let mut stages: Vec<String> = calls.iter().map(|c| stage_label(c.stage_index, &c.stage)).collect();
                stages.dedup();
                RepeatedCall {
                    name: calls[0].name.clone(),
                    args_excerpt: calls[0].args_excerpt.clone(),
                    count: calls.len(),
                    distinct_results: results.len(),
                    stages,
                    total_ms: calls.iter().map(|c| c.duration_ms).sum(),
                    seqs: calls.iter().map(|c| c.seq).collect(),
                }
            })
            .collect();
        repeated.sort_by(|a, b| b.count.cmp(&a.count).then(b.total_ms.cmp(&a.total_ms)));
        repeated
    }

    /// Failures grouped by tool and normalized error line, most frequent first.
    pub fn recurring_errors(&self) -> Vec<RecurringError> {
        let mut groups: BTreeMap<(String, String), Vec<&CallRecord>> = BTreeMap::new();
        for call in self.calls.iter().filter(|c| !c.ok) {
            let line = format::first_line(call.error.as_deref().unwrap_or(""));
            groups
                .entry((call.name.clone(), format::normalize_error(line)))
                .or_default()
                .push(call);
        }
        let mut errors: Vec<RecurringError> = groups
            .into_iter()
            .map(|((tool, pattern), calls)| {
                let mut stages: Vec<String> = calls.iter().map(|c| stage_label(c.stage_index, &c.stage)).collect();
                stages.dedup();
                RecurringError {
                    tool,
                    pattern,
                    count: calls.len(),
                    example: calls[0].error.clone().unwrap_or_default(),
                    stages,
                    seqs: calls.iter().map(|c| c.seq).collect(),
                }
            })
            .collect();
        errors.sort_by_key(|e| std::cmp::Reverse(e.count));
        errors
    }

    /// Runs of at least `min` consecutive calls to one tool within a stage.
    pub fn streaks(&self, min: usize) -> Vec<Streak> {
        let mut streaks = Vec::new();
        let mut i = 0;
        while i < self.calls.len() {
            let first = &self.calls[i];
            let mut j = i + 1;
            while j < self.calls.len()
                && self.calls[j].name == first.name
                && self.calls[j].stage_index == first.stage_index
            {
                j += 1;
            }
            if j - i >= min {
                streaks.push(Streak {
                    stage_index: first.stage_index,
                    stage: first.stage.clone(),
                    tool: first.name.clone(),
                    length: j - i,
                    from_seq: first.seq,
                    to_seq: self.calls[j - 1].seq,
                    total_ms: self.calls[i..j].iter().map(|c| c.duration_ms).sum(),
                });
            }
            i = j;
        }
        streaks.sort_by_key(|s| std::cmp::Reverse(s.length));
        streaks
    }

    /// Write-tool activity per stage that used any.
    pub fn edit_activity(&self) -> Vec<EditActivity> {
        let mut out = Vec::new();
        for stage in &self.stages {
            let writes: Vec<&CallRecord> = self
                .calls
                .iter()
                .filter(|c| c.stage_index == stage.index && is_write_tool(&c.name))
                .collect();
            if writes.is_empty() {
                continue;
            }
            let mut targets: Vec<&str> = writes.iter().filter_map(|c| c.target.as_deref()).collect();
            targets.sort_unstable();
            targets.dedup();
            let mut write_turns: Vec<usize> = writes.iter().map(|c| c.turn).collect();
            write_turns.dedup();
            let mut by_tool = BTreeMap::new();
            for call in &writes {
                *by_tool.entry(call.name.clone()).or_insert(0) += 1;
            }
            out.push(EditActivity {
                stage_index: stage.index,
                stage: stage.name.clone(),
                write_calls: writes.len(),
                distinct_targets: targets.len(),
                turns_with_writes: write_turns.len(),
                turns_with_several_calls: self
                    .turns
                    .iter()
                    .filter(|t| t.stage_index == stage.index && t.tool_names.len() > 1)
                    .count(),
                by_tool,
            });
        }
        out
    }

    /// How many control events of each kind the run had.
    pub fn control_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for control in &self.controls {
            *counts.entry(control.kind.label()).or_insert(0) += 1;
        }
        counts
    }

    /// Whether the last review round approved the form; `None` when there
    /// was no review, or the last round ended without a verdict.
    pub fn approved(&self) -> Option<bool> {
        self.verdicts.last().and_then(|v| v.approved)
    }
}

/// "3. Author", the label a stage goes by in every table.
pub fn stage_label(index: usize, name: &str) -> String {
    format!("{index}. {name}")
}

fn is_write_tool(name: &str) -> bool {
    WRITE_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// What a write tool's arguments say it changes — the first of the usual
/// naming keys present, as text.
fn write_target(name: &str, args: &serde_json::Value) -> Option<String> {
    if !is_write_tool(name) {
        return None;
    }
    let object = args.as_object()?;
    TARGET_KEYS.iter().find_map(|key| {
        object.get(*key).map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline::TurnUsage;
    use serde_json::json;

    fn stage(name: &str) -> TraceEvent {
        TraceEvent::StageStarted {
            stage: name.into(),
            system_prompt: "sys".into(),
            seed_message: "go".into(),
            max_turns: 10,
            tools_offered: vec!["a".into()],
        }
    }

    fn call(stats: &mut Stats, seq: u64, name: &str, args: serde_json::Value, ok: bool, result: &str) {
        let id = format!("c{seq}");
        stats.record(
            seq,
            seq * 10,
            &TraceEvent::ToolStarted {
                stage: "Author".into(),
                attempt: 1,
                turn: 1,
                call_id: id.clone(),
                name: name.into(),
                args,
            },
        );
        stats.record(
            seq + 1,
            seq * 10 + 5,
            &TraceEvent::ToolFinished {
                stage: "Author".into(),
                attempt: 1,
                turn: 1,
                call_id: id,
                name: name.into(),
                ok,
                duration_ms: 5,
                result: result.into(),
                result_chars: result.len(),
                image_count: 0,
                image_chars: 0,
                result_hash: pipeline::trace::stable_hash(result),
            },
        );
    }

    #[test]
    fn a_repeated_call_with_the_same_result_is_flagged_as_learning_nothing() {
        let mut stats = Stats::default();
        stats.record(0, 0, &stage("Author"));
        call(&mut stats, 1, "validate_aem_package", json!({}), false, "3 violations");
        call(&mut stats, 3, "validate_aem_package", json!({}), false, "3 violations");
        call(&mut stats, 5, "get_structured_node", json!({"id": "a"}), true, "x");

        let repeated = stats.repeated_calls();
        assert_eq!(repeated.len(), 1);
        assert_eq!(repeated[0].name, "validate_aem_package");
        assert_eq!(repeated[0].count, 2);
        assert_eq!(repeated[0].distinct_results, 1);
        assert_eq!(repeated[0].seqs, vec![1, 3]);
    }

    #[test]
    fn failures_differing_only_in_names_recur_as_one_error() {
        let mut stats = Stats::default();
        stats.record(0, 0, &stage("Author"));
        call(&mut stats, 1, "set_aem_translated_field", json!({"path": "a"}), false, "Error: node 'a' not found");
        call(&mut stats, 3, "set_aem_translated_field", json!({"path": "b"}), false, "Error: node 'b' not found");

        let errors = stats.recurring_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].count, 2);
        assert_eq!(stats.tools["set_aem_translated_field"].failures, 2);
    }

    #[test]
    fn consecutive_calls_to_one_tool_form_a_streak() {
        let mut stats = Stats::default();
        stats.record(0, 0, &stage("Author"));
        for i in 0..5 {
            call(&mut stats, 1 + i * 2, "get_structured_node", json!({"id": i}), true, "x");
        }
        call(&mut stats, 20, "write_package", json!({}), true, "ok");
        let streaks = stats.streaks(4);
        assert_eq!(streaks.len(), 1);
        assert_eq!(streaks[0].length, 5);
    }

    #[test]
    fn write_tools_report_what_they_touched() {
        let mut stats = Stats::default();
        stats.record(0, 0, &stage("Author"));
        call(&mut stats, 1, "set_aem_translated_field", json!({"path": "p1"}), true, "ok");
        call(&mut stats, 3, "set_aem_translated_field", json!({"path": "p2"}), true, "ok");
        call(&mut stats, 5, "set_aem_translated_field", json!({"path": "p1"}), true, "ok");
        let activity = stats.edit_activity();
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].write_calls, 3);
        assert_eq!(activity[0].distinct_targets, 2);
    }

    #[test]
    fn stage_figures_add_up_from_turns_and_calls() {
        let mut stats = Stats::default();
        stats.announce_stage("building".into());
        stats.record(0, 0, &stage("Author"));
        let usage = TurnUsage {
            input_tokens: 100,
            cached_input_tokens: 900,
            ..TurnUsage::default()
        };
        stats.record(
            1,
            10,
            &TraceEvent::TurnFinished {
                stage: "Author".into(),
                attempt: 1,
                turn: 1,
                latency_ms: 2_000,
                finish_reason: Some("tool_calls".into()),
                usage,
                cost_usd: None,
                text: "thinking".into(),
                reasoning: String::new(),
                tool_calls: vec![],
            },
        );
        call(&mut stats, 2, "get_source_info", json!({}), true, "info");
        let s = &stats.stages[0];
        assert_eq!(s.doing, "building");
        assert_eq!(s.model_ms, 2_000);
        assert_eq!(s.tool_ms, 5);
        assert_eq!(s.max_prompt_tokens, 1_000);
        assert_eq!(s.tool_calls, 1);
    }

    fn verdict(stats: &mut Stats, round: usize, approved: Option<bool>) {
        stats.record(
            1,
            1,
            &TraceEvent::ReviewVerdict {
                stage: "Reviewer".into(),
                round,
                approved,
                report: "fix it".into(),
            },
        );
    }

    #[test]
    fn the_last_verdict_decides_approval() {
        let mut stats = Stats::default();
        assert_eq!(stats.approved(), None);
        stats.record(0, 0, &stage("Reviewer"));
        verdict(&mut stats, 1, Some(false));
        verdict(&mut stats, 2, Some(true));
        assert_eq!(stats.approved(), Some(true));
    }

    /// A round the Reviewer ended without a verdict is not a rejection.
    #[test]
    fn a_round_without_a_verdict_is_neither_approval_nor_rejection() {
        let mut stats = Stats::default();
        stats.record(0, 0, &stage("Reviewer"));
        verdict(&mut stats, 1, None);
        assert_eq!(stats.approved(), None);
        assert_eq!(stats.verdicts[0].describe(), "no verdict");
    }

    #[test]
    fn a_failed_request_counts_against_its_stage() {
        let mut stats = Stats::default();
        stats.record(0, 0, &stage("Author"));
        stats.record(
            1,
            100,
            &TraceEvent::RequestFailed {
                stage: "Author".into(),
                attempt: 1,
                turn: 3,
                latency_ms: 30_000,
                error: "timed out".into(),
            },
        );
        assert_eq!(stats.stages[0].failed_requests, 1);
        assert_eq!(stats.failed_request_ms(), 30_000);
        assert_eq!(stats.other_ms(40_000), 10_000);
    }
}
