//! Everything the run-analysis folder says in prose: the folder guide, the
//! timeline and transcript entries as they are appended, and the report
//! rendered from the accumulated [`Stats`].
//!
//! Written for two readers at once — a person skimming for where the time went,
//! and an AI assistant handed the files to analyse. Both are served by the same
//! choices: the conclusions first, stable headings, one fact per line, units on
//! every number, and a `seq` on every entry that points back to its full record
//! in `trace.jsonl`.

use std::fmt::Write as _;

use pipeline::{StageEnd, TraceEvent};

use super::RunMeta;
use super::format::{self, cell, duration, excerpt, number, percent, usd};
use super::stats::{Stats, stage_label};

/// Characters of a tool's arguments shown inline in the timeline.
const TIMELINE_ARGS: usize = 300;
/// Characters of a tool's result shown inline in the timeline.
const TIMELINE_RESULT: usize = 300;
/// Characters of the model's text shown inline in the timeline.
const TIMELINE_TEXT: usize = 1_500;
/// Characters of a single tool result or argument set kept in a transcript.
/// Beyond this the transcript points at `trace.jsonl`, which has it all.
pub const TRANSCRIPT_BLOCK: usize = 30_000;
/// Rows in each "top N" table of the report.
const TOP: usize = 15;
/// Consecutive calls of one tool that count as a streak worth reporting.
const STREAK_MIN: usize = 5;

/// `README.md`: what is in the folder and how to use it, written once at the
/// start so it is there even if the run never finishes.
pub fn readme(meta: &RunMeta) -> String {
    format!(
        "# Run analysis — {label}\n\
\n\
This folder records one run of the Conversion Engine's AI conversion: every\n\
stage, model turn and tool call, with timestamps, durations and the full\n\
content the agents saw. It was written while the run happened, so it is\n\
complete up to the moment the run stopped, even if it stopped abnormally.\n\
\n\
- Session: `{session}`\n\
- Started: {started}\n\
\n\
## Files\n\
\n\
| File | What it is | Read it when |\n\
|---|---|---|\n\
| `report.md` | The analysis: where the time went, per stage and per tool, the slowest turns and calls, repeated calls, recurring errors, review verdicts, control events. Rewritten after every stage and at the end. | Always first. It is short by design. |\n\
| `timeline.md` | Everything in order, one entry per model turn and tool call, with clock time, time since start, duration, token use and short excerpts. | To see *when* and *in what order* things happened. |\n\
| `transcript/NN-<Stage>.md` | One file per stage (split into parts when large): the system prompt, every model message in full, every tool call's arguments and result (long ones cut at {block} characters). | To see *why* an agent did something — what it was told and what it read. |\n\
| `trace.jsonl` | The complete machine-readable record: one JSON object per line, nothing truncated except images. | For scripts, `jq`, or to recover a result the transcript cut. |\n\
| `summary.json` | The report's figures as data, for comparing runs (`scripts/analyze_runs.py`). | To aggregate many runs. |\n\
| `evaluation.md` | The human evaluation of the result: verdict, scores, findings, requirements for v3. A template until someone fills it in; collected across runs by `scripts/collect_evaluations.py`. | After checking the converted form. |\n\
\n\
Every timeline entry and every table row that refers to a specific event\n\
carries its `seq`: the line number of that event in `trace.jsonl` (1-based).\n\
`#123` in a Markdown file means \"seq 123\".\n\
\n\
## Reading `trace.jsonl`\n\
\n\
Each line has `seq`, `time` (local, RFC 3339), `elapsed_ms` (since the run\n\
started), `stage_index` (1-based position of the stage in the run) and an\n\
`event` tag:\n\
\n\
- `run_started` / `run_finished` — the run's settings and its outcome.\n\
- `stage_started` — `stage`, `system_prompt`, `seed_message`, `max_turns`, `tools_offered`.\n\
- `attempt_started` — a fresh start of the stage; `attempt` > 1 means a restart after a failed request (`history_messages` then includes the request being re-sent).\n\
- `turn_started` — a request is sent; `history_messages` vs `sent_messages` shows context shaping, `shaping_ms` how long that took.\n\
- `request_failed` — a request that failed before the model answered: `latency_ms` until it failed, `error`.\n\
- `turn_finished` — `latency_ms`, `usage` (tokens), `cost_usd`, `finish_reason`, the model's `text` and `reasoning`, and the `tool_calls` it asked for (full arguments).\n\
- `tool_started` / `tool_finished` — `name`, `args`, `ok`, `duration_ms`, the full `result` text (images replaced by a placeholder), `result_hash`.\n\
- `review_verdict` — one review round: `round`, `approved` (`null` when the Reviewer ended without a verdict), `report`.\n\
- `control` — the controller steering the run: `kind` is one of `output_cap_nudge`, `stuck_stop`, `turn_budget_exhausted`, `transient_retry`, `operator_prompt`, `operator_retried`, `operator_cancelled`, `invalid_tool_call`, `context_budget_failed`, `aborted`, `stage_error`.\n\
- `stage_finished` — `ended`, `turns`, `attempts`, `duration_ms`, and the stage's own `spend`.\n\
- `progress`, `warning` — the recorder's own notes and the warnings the app and console showed.\n\
\n\
`stage_index` is the stage most recently started, so lines between two stages\n\
(a review verdict, the finalize step, `run_finished`) carry the previous one.\n\
\n\
```sh\n\
# The ten slowest tool calls\n\
jq -s -c 'map(select(.event==\"tool_finished\")) | sort_by(-.duration_ms)[:10][] | {{seq, stage, name, duration_ms}}' trace.jsonl\n\
# Every failed tool call with the start of its error\n\
jq -r 'select(.event==\"tool_finished\" and .ok==false) | \"\\(.seq) \\(.name): \\(.result[:200])\"' trace.jsonl\n\
```\n\
\n\
## Using these files with an AI assistant\n\
\n\
The files are meant to be handed to an AI as they are. Long runs produce\n\
large files, so give it `report.md` first, then only the parts it asks for:\n\
the timeline for order and timing, one stage's transcript at a time for\n\
reasoning, and `trace.jsonl` with a tool that can search it.\n\
\n\
Questions that work well:\n\
\n\
- Using `report.md`, where did this run spend its time, and which of it was avoidable?\n\
- The report lists repeated calls and recurring errors. Using the transcripts, explain why the agent repeated them and propose a prompt change, a deterministic script, or a new tool that would remove the repetition.\n\
- Which failures in \"Recurring errors\" could be fixed by a post-processing script instead of by the model?\n\
- Read the Reviewer's verdicts and the Author fix rounds that followed. Which review findings could the Author have avoided in its first pass?\n\
- Using the edit activity and the Author transcript, which parts of the form were authored independently of each other and could be built by agents in parallel?\n\
- Which tool results are the largest? Would a narrower tool (an MCP tool that returns only what is needed) have served the agent as well?\n",
        label = meta.label,
        session = meta.session_id,
        started = meta.started,
        block = number(TRANSCRIPT_BLOCK as u64),
    )
}

/// One `timeline.md` entry for a trace event, or `None` for the events the
/// timeline does not show on their own line.
pub fn timeline_entry(seq: u64, clock: &str, stats: &Stats, event: &TraceEvent) -> Option<String> {
    let at = format!("`{clock}` #{seq}");
    Some(match event {
        TraceEvent::StageStarted {
            stage,
            max_turns,
            tools_offered,
            system_prompt,
            ..
        } => {
            let current = stats.current_stage();
            let index = current.map_or(0, |s| s.index);
            let doing = current.map(|s| s.doing.as_str()).unwrap_or("");
            format!(
                "\n## {} — {doing}\n\n{at} Stage started. Turn budget {max_turns}, {} tools offered, system prompt {} characters.\n",
                stage_label(index, stage),
                tools_offered.len(),
                number(system_prompt.chars().count() as u64),
            )
        }
        TraceEvent::AttemptStarted {
            attempt,
            history_messages,
            ..
        } if *attempt > 1 => format!(
            "\n{at} **Restart:** attempt {attempt}, resuming from {history_messages} messages of history.\n"
        ),
        TraceEvent::AttemptStarted { .. } | TraceEvent::TurnStarted { .. } => return None,
        TraceEvent::TurnFinished {
            stage,
            turn,
            latency_ms,
            finish_reason,
            usage,
            cost_usd,
            text,
            tool_calls,
            ..
        } => {
            let mut out = format!(
                "\n{at} **{stage} turn {turn}** — model {}, prompt {} tokens ({} cached), output {} tokens, {}, ended: {}\n",
                duration(*latency_ms),
                number(usage.prompt_tokens()),
                number(usage.cached_input_tokens),
                number(usage.output_tokens),
                usd(*cost_usd),
                finish_reason.as_deref().unwrap_or("unknown"),
            );
            if !text.trim().is_empty() {
                out.push_str(&format::quote(&excerpt(text.trim(), TIMELINE_TEXT)));
                out.push('\n');
            }
            if !tool_calls.is_empty() {
                let names: Vec<&str> = tool_calls.iter().map(|c| c.name.as_str()).collect();
                // A blank line first, or Markdown folds this into the quote.
                let _ = writeln!(out, "\nAsked for: {}", names.join(", "));
            }
            out
        }
        TraceEvent::RequestFailed {
            stage,
            turn,
            latency_ms,
            error,
            ..
        } => format!(
            "\n{at} **{stage} turn {turn}: request FAILED** after {}: {}\n",
            duration(*latency_ms),
            cell(error, 600)
        ),
        TraceEvent::ReviewVerdict {
            round,
            approved,
            report,
            ..
        } => format!(
            "\n{at} **Review round {round}: {}.** {}\n",
            verdict_word(*approved),
            cell(report, 800)
        ),
        TraceEvent::ToolStarted { .. } => return None,
        TraceEvent::ToolFinished {
            name,
            ok,
            duration_ms,
            result,
            result_chars,
            image_count,
            ..
        } => {
            // The stats fold a call in when it finishes, before its timeline
            // entry is written, so the last call record is this one.
            let call = stats.calls.last().filter(|c| c.name == *name);
            let args = call.map(|c| c.args_excerpt.clone()).unwrap_or_default();
            // Numbered like every table row that names a call: by the line
            // that started it, which holds its arguments.
            let at = match call {
                Some(c) if c.seq != seq => format!("`{clock}` #{} (result #{seq})", c.seq),
                _ => at,
            };
            let glyph = if *ok { "ok" } else { "FAILED" };
            let images = if *image_count > 0 {
                format!(", {image_count} image(s)")
            } else {
                String::new()
            };
            format!(
                "- {at} `{name}` {} → {glyph} in {}, {} characters{images}: {}\n",
                cell(&args, TIMELINE_ARGS),
                duration(*duration_ms),
                number(*result_chars as u64),
                cell(result, TIMELINE_RESULT),
            )
        }
        TraceEvent::Control { kind, detail, .. } => format!(
            "\n{at} **Control — {}:** {}\n",
            kind.label(),
            excerpt(&format::one_line(detail), 600)
        ),
        TraceEvent::StageFinished {
            stage,
            ended,
            turns,
            attempts,
            duration_ms,
            spend,
        } => format!(
            "\n{at} **{stage} ended** ({}) after {}: {turns} turns, {attempts} attempt(s), {}.\n",
            ended.describe(),
            duration(*duration_ms),
            usd(spend.cost_usd),
        ),
    })
}

fn verdict_word(approved: Option<bool>) -> &'static str {
    match approved {
        Some(true) => "approved",
        Some(false) => "changes requested",
        None => "no verdict",
    }
}

/// A `timeline.md` line for a progress message the controller emitted.
pub fn timeline_note(seq: u64, clock: &str, label: &str, text: &str) -> String {
    format!("\n`{clock}` #{seq} *{label}:* {}\n", excerpt(&format::one_line(text), 800))
}

/// The heading of one stage's transcript file (or one part of it).
pub fn transcript_header(index: usize, stage: &str, part: usize) -> String {
    if part == 1 {
        format!("# Transcript — {}\n", stage_label(index, stage))
    } else {
        format!("# Transcript — {} (part {part})\n", stage_label(index, stage))
    }
}

/// One transcript block for a trace event: everything in full, up to
/// [`TRANSCRIPT_BLOCK`] characters per block.
pub fn transcript_entry(seq: u64, clock: &str, time: &str, event: &TraceEvent) -> Option<String> {
    let at = format!("`{clock}` {time} #{seq}");
    Some(match event {
        TraceEvent::StageStarted {
            system_prompt,
            seed_message,
            max_turns,
            tools_offered,
            ..
        } => format!(
            "\n{at}\n\n## System prompt\n\n{}\n## First message\n\n{}\n## Tools offered ({})\n\n{}\n\nTurn budget: {max_turns}.\n",
            format::fence(&cut_block_at(system_prompt, seq, TRANSCRIPT_BLOCK * 4), "text"),
            format::fence(seed_message, "text"),
            tools_offered.len(),
            tools_offered.join(", "),
        ),
        TraceEvent::AttemptStarted {
            attempt,
            history_messages,
            ..
        } => {
            if *attempt == 1 {
                return None;
            }
            format!("\n---\n\n{at} **Attempt {attempt}** — restarted from {history_messages} messages of history.\n")
        }
        TraceEvent::TurnStarted {
            turn,
            history_messages,
            sent_messages,
            estimated_tokens,
            ..
        } => {
            let shaped = if sent_messages < history_messages {
                format!(
                    " The context budget shortened the history from {history_messages} to {sent_messages} messages."
                )
            } else {
                String::new()
            };
            format!(
                "\n---\n\n## Turn {turn}\n\n{at} Request sent with {sent_messages} messages of history, about {} tokens estimated.{shaped}\n",
                number(*estimated_tokens as u64)
            )
        }
        TraceEvent::TurnFinished {
            latency_ms,
            finish_reason,
            usage,
            cost_usd,
            text,
            reasoning,
            tool_calls,
            ..
        } => {
            let mut out = format!(
                "\n{at} Model answered in {} — prompt {} tokens ({} cached, {} written to cache), output {} tokens, reasoning {} tokens, {}, ended: {}.\n",
                duration(*latency_ms),
                number(usage.prompt_tokens()),
                number(usage.cached_input_tokens),
                number(usage.cache_write_tokens),
                number(usage.output_tokens),
                number(usage.reasoning_tokens),
                usd(*cost_usd),
                finish_reason.as_deref().unwrap_or("unknown"),
            );
            if !reasoning.trim().is_empty() {
                out.push_str("\n### Model reasoning\n\n");
                out.push_str(&format::quote(&cut_block(reasoning, seq)));
                out.push('\n');
            }
            if !text.trim().is_empty() {
                out.push_str("\n### Model message\n\n");
                out.push_str(&format::quote(&cut_block(text, seq)));
                out.push('\n');
            }
            if !tool_calls.is_empty() {
                let names: Vec<String> = tool_calls.iter().map(|c| format!("`{}`", c.name)).collect();
                let _ = writeln!(out, "\nTool calls requested: {}", names.join(", "));
            }
            out
        }
        TraceEvent::ToolStarted { name, args, .. } => {
            let pretty = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
            format!(
                "\n### Tool call `{name}`\n\n{at}\n\n{}",
                format::fence(&cut_block(&pretty, seq), "json")
            )
        }
        TraceEvent::ToolFinished {
            name,
            ok,
            duration_ms,
            result,
            result_chars,
            image_count,
            image_chars,
            ..
        } => {
            let images = if *image_count > 0 {
                format!(
                    " It also returned {image_count} image(s) ({} base64 characters), not reproduced here.",
                    number(*image_chars as u64)
                )
            } else {
                String::new()
            };
            format!(
                "\n{at} `{name}` {} in {}, {} characters.{images}\n\n{}",
                if *ok { "succeeded" } else { "FAILED" },
                duration(*duration_ms),
                number(*result_chars as u64),
                format::fence(&cut_block(result, seq), "text"),
            )
        }
        TraceEvent::RequestFailed {
            latency_ms, error, ..
        } => format!(
            "\n{at} **Request failed** after {}:\n\n{}",
            duration(*latency_ms),
            format::fence(&cut_block(error, seq), "text")
        ),
        TraceEvent::ReviewVerdict {
            round,
            approved,
            report,
            ..
        } => format!(
            "\n{at} **Review round {round}: {}.**\n\n{}\n",
            verdict_word(*approved),
            if report.trim().is_empty() {
                "(no report)".to_string()
            } else {
                format::quote(&cut_block(report, seq))
            }
        ),
        TraceEvent::Control { kind, detail, .. } => format!(
            "\n{at} **Control — {}:**\n\n{}\n",
            kind.label(),
            format::quote(&cut_block(detail, seq))
        ),
        TraceEvent::StageFinished {
            ended,
            turns,
            attempts,
            duration_ms,
            spend,
            ..
        } => format!(
            "\n---\n\n{at} **Stage ended** ({}) after {}: {turns} turns, {attempts} attempt(s). {}\n",
            ended.describe(),
            duration(*duration_ms),
            spend.describe()
        ),
    })
}

/// `text`, cut at [`TRANSCRIPT_BLOCK`] characters with a pointer to the full
/// record.
fn cut_block(text: &str, seq: u64) -> String {
    cut_block_at(text, seq, TRANSCRIPT_BLOCK)
}

fn cut_block_at(text: &str, seq: u64, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!(
        "{kept}\n… [cut: {} more characters — the full text is line {seq} of trace.jsonl]",
        number((total - max) as u64)
    )
}

/// What the run looked like at the time the report was written.
pub struct ReportContext<'a> {
    pub meta: &'a RunMeta,
    pub stats: &'a Stats,
    /// Milliseconds since the run started.
    pub elapsed_ms: u64,
    /// `None` while the run is still going.
    pub outcome: Option<&'a super::RunEnd>,
}

/// `report.md`: the analysis.
pub fn report(ctx: &ReportContext<'_>) -> String {
    let ReportContext {
        meta,
        stats,
        elapsed_ms,
        outcome,
    } = ctx;
    let mut out = String::new();
    let wall = *elapsed_ms;
    let model = stats.model_ms();
    let tools = stats.tool_ms();
    let shaping = stats.shaping_ms();
    let failed = stats.failed_request_ms();
    let other = stats.other_ms(wall);

    let _ = writeln!(out, "# Run analysis report — {}\n", meta.label);
    match outcome {
        None => {
            let _ = writeln!(
                out,
                "**The run is still in progress** (or stopped without finishing). This report covers \
                 the first {} and is rewritten after every stage.\n",
                duration(wall)
            );
        }
        Some(end) => {
            let _ = writeln!(out, "**Outcome:** {}\n", end.describe(stats.approved()));
        }
    }

    out.push_str("## At a glance\n\n");
    let _ = writeln!(out, "- Session: `{}` ({})", meta.session_id, meta.kind);
    let _ = writeln!(out, "- Started: {}", meta.started);
    let _ = writeln!(out, "- Wall time: {}", duration(wall));
    let _ = writeln!(
        out,
        "- Waiting for the model: {} ({})",
        duration(model),
        percent(model, wall)
    );
    let _ = writeln!(out, "- Running tools: {} ({})", duration(tools), percent(tools, wall));
    if failed > 0 || !stats.failed_requests.is_empty() {
        let _ = writeln!(
            out,
            "- Waiting for requests that then failed: {} ({}), {} request(s)",
            duration(failed),
            percent(failed, wall),
            stats.failed_requests.len()
        );
    }
    let _ = writeln!(
        out,
        "- Shaping requests to fit the context budget: {} ({})",
        duration(shaping),
        percent(shaping, wall)
    );
    let _ = writeln!(
        out,
        "- Everything else — retry waits, operator pauses, the controller itself: {} ({})",
        duration(other),
        percent(other, wall)
    );
    let _ = writeln!(
        out,
        "- Stages: {}; model turns: {}; tool calls: {} ({} failed)",
        stats.stages.len(),
        stats.turns.len(),
        stats.calls.len(),
        stats.calls.iter().filter(|c| !c.ok).count()
    );
    let _ = writeln!(out, "- {}", stats.spend.describe());
    let _ = writeln!(out, "- Model: {}", meta.model);
    let _ = writeln!(out, "- Profile: {}; target: {}", meta.profile, meta.target);
    if let Some(last) = stats.verdicts.last() {
        let _ = writeln!(
            out,
            "- Review rounds: {}; last: {}",
            stats.verdicts.len(),
            last.describe()
        );
    }
    let counts = stats.control_counts();
    if !counts.is_empty() {
        let list: Vec<String> = counts.iter().map(|(k, v)| format!("{k} ×{v}")).collect();
        let _ = writeln!(out, "- Control events: {}", list.join(", "));
    }
    out.push('\n');

    stage_table(&mut out, stats);
    tool_table(&mut out, stats);
    failed_requests(&mut out, stats);
    slow_turns(&mut out, stats);
    slow_calls(&mut out, stats);
    repetition(&mut out, stats);
    errors(&mut out, stats);
    biggest_results(&mut out, stats);
    edit_activity(&mut out, stats);
    reviews(&mut out, stats);
    controls(&mut out, stats);
    warnings(&mut out, stats);

    out.push_str(
        "## Where to look next\n\n\
         - `timeline.md` for the order of events, `transcript/` for the full content of one stage, \
         `trace.jsonl` for everything. `#N` above is line N of `trace.jsonl`.\n\
         - `README.md` explains every file and lists questions to ask an AI assistant about this run.\n",
    );
    out
}

fn stage_table(out: &mut String, stats: &Stats) {
    if stats.stages.is_empty() {
        return;
    }
    out.push_str("## Stages\n\n");
    out.push_str(
        "| Stage | Doing | Duration | Model | Tools | Turns (budget) | Attempts | Failed requests | Tool calls (failed) | Max prompt | Shaped turns | Cost | Ended |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for s in &stats.stages {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} ({}) | {} | {} | {} ({}) | {} tok | {} | {} | {} |",
            stage_label(s.index, &s.name),
            cell(&s.doing, 60),
            if s.ended.is_some() {
                duration(s.duration_ms)
            } else {
                "running".into()
            },
            duration(s.model_ms),
            duration(s.tool_ms),
            s.turns,
            s.max_turns,
            s.attempts,
            s.failed_requests,
            s.tool_calls,
            s.tool_failures,
            number(s.max_prompt_tokens),
            s.shaped_turns,
            usd(s.spend.cost_usd),
            s.ended.as_ref().map_or("-".into(), StageEnd::describe),
        );
    }
    out.push('\n');
}

fn tool_table(out: &mut String, stats: &Stats) {
    if stats.tools.is_empty() {
        return;
    }
    let tool_total = stats.tool_ms();
    out.push_str("## Tools, by total time\n\n");
    out.push_str(
        "| Tool | Calls | Failed | Total | Share of tool time | Average | Longest | Result characters (total / largest) |\n\
         |---|---|---|---|---|---|---|---|\n",
    );
    for t in stats.tools_by_time() {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} | {} | {} / {} |",
            t.name,
            t.calls,
            t.failures,
            duration(t.total_ms),
            percent(t.total_ms, tool_total),
            duration(t.total_ms / t.calls.max(1) as u64),
            duration(t.max_ms),
            number(t.result_chars),
            number(t.max_result_chars as u64),
        );
    }
    out.push('\n');
}

fn failed_requests(out: &mut String, stats: &Stats) {
    if stats.failed_requests.is_empty() {
        return;
    }
    out.push_str(
        "## Failed requests\n\n\
         Requests that failed before the model answered. What followed each (an automatic retry, a \
         pause for the operator) is under Control events.\n\n\
         | # | At | Stage | Turn | Took | Error |\n|---|---|---|---|---|---|\n",
    );
    for f in stats.failed_requests.iter().take(TOP * 2) {
        let _ = writeln!(
            out,
            "| #{} | {} | {} | {} | {} | {} |",
            f.seq,
            format::clock(f.at_ms),
            stage_label(f.stage_index, &f.stage),
            f.turn,
            duration(f.latency_ms),
            cell(&f.error, 200),
        );
    }
    out.push('\n');
}

fn slow_turns(out: &mut String, stats: &Stats) {
    if stats.turns.is_empty() {
        return;
    }
    let mut turns: Vec<_> = stats.turns.iter().collect();
    turns.sort_by_key(|t| std::cmp::Reverse(t.latency_ms));
    out.push_str("## Slowest model turns\n\n");
    out.push_str(
        "| Stage | Turn | Sent at | Model time | Prompt | Output | Then called | Said |\n\
         |---|---|---|---|---|---|---|---|\n",
    );
    for t in turns.into_iter().take(TOP) {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} tok | {} tok | {} | {} |",
            stage_label(t.stage_index, &t.stage),
            t.turn,
            format::clock(t.at_ms.saturating_sub(t.latency_ms)),
            duration(t.latency_ms),
            number(t.prompt_tokens),
            number(t.output_tokens),
            cell(&t.tool_names.join(", "), 80),
            cell(&t.text_excerpt, 120),
        );
    }
    out.push('\n');
}

fn slow_calls(out: &mut String, stats: &Stats) {
    if stats.calls.is_empty() {
        return;
    }
    let mut calls: Vec<_> = stats.calls.iter().collect();
    calls.sort_by_key(|c| std::cmp::Reverse(c.duration_ms));
    out.push_str("## Slowest tool calls\n\n");
    out.push_str("| # | Stage | Turn | Tool | Duration | OK | Arguments |\n|---|---|---|---|---|---|---|\n");
    for c in calls.into_iter().take(TOP) {
        let _ = writeln!(
            out,
            "| #{} | {} | {} | `{}` | {} | {} | {} |",
            c.seq,
            stage_label(c.stage_index, &c.stage),
            c.turn,
            c.name,
            duration(c.duration_ms),
            if c.ok { "yes" } else { "no" },
            cell(&c.args_excerpt, 120),
        );
    }
    out.push('\n');
}

fn repetition(out: &mut String, stats: &Stats) {
    let repeated = stats.repeated_calls();
    let streaks = stats.streaks(STREAK_MIN);
    if repeated.is_empty() && streaks.is_empty() {
        return;
    }
    out.push_str("## Repetition and possible loops\n\n");
    if !repeated.is_empty() {
        out.push_str(
            "The same tool called with exactly the same arguments more than once. \
             \"Distinct results\" = 1 means every repeat got the same answer back and learned nothing new; \
             more than 1 usually means something changed in between (an edit, then a re-check).\n\n\
             | Tool | Times | Distinct results | Time spent | Stages | Arguments | Occurrences |\n\
             |---|---|---|---|---|---|---|\n",
        );
        for r in repeated.iter().take(TOP * 2) {
            let seqs: Vec<String> = r.seqs.iter().take(12).map(|s| format!("#{s}")).collect();
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {}{} |",
                r.name,
                r.count,
                r.distinct_results,
                duration(r.total_ms),
                r.stages.join(", "),
                cell(&r.args_excerpt, 100),
                seqs.join(" "),
                if r.seqs.len() > 12 { " …" } else { "" },
            );
        }
        let wasted: usize = repeated
            .iter()
            .filter(|r| r.distinct_results == 1)
            .map(|r| r.count - 1)
            .sum();
        let _ = writeln!(
            out,
            "\n{wasted} call(s) repeated an earlier call and got an identical result back.\n"
        );
    }
    if !streaks.is_empty() {
        let _ = writeln!(
            out,
            "Runs of {STREAK_MIN} or more consecutive calls to the same tool (often one-at-a-time work a batch tool could do in one call):\n"
        );
        out.push_str("| Stage | Tool | Calls in a row | Time | From → to |\n|---|---|---|---|---|\n");
        for s in streaks.iter().take(TOP) {
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {} | #{} → #{} |",
                stage_label(s.stage_index, &s.stage),
                s.tool,
                s.length,
                duration(s.total_ms),
                s.from_seq,
                s.to_seq,
            );
        }
        out.push('\n');
    }
}

fn errors(out: &mut String, stats: &Stats) {
    let errors = stats.recurring_errors();
    if errors.is_empty() {
        return;
    }
    out.push_str(
        "## Recurring errors\n\n\
         Failed tool calls grouped by tool and error line (numbers and quoted names folded, so the \
         same problem on different fields counts as one). Frequent ones are candidates for a \
         deterministic fix script, a better tool description, or a new tool.\n\n\
         | Tool | Times | Stages | Error pattern | Example | Occurrences |\n|---|---|---|---|---|---|\n",
    );
    for e in errors.iter().take(TOP * 2) {
        let seqs: Vec<String> = e.seqs.iter().take(10).map(|s| format!("#{s}")).collect();
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {}{} |",
            e.tool,
            e.count,
            e.stages.join(", "),
            cell(&e.pattern, 140),
            cell(&e.example, 200),
            seqs.join(" "),
            if e.seqs.len() > 10 { " …" } else { "" },
        );
    }
    out.push('\n');
}

fn biggest_results(out: &mut String, stats: &Stats) {
    if stats.calls.is_empty() {
        return;
    }
    let mut calls: Vec<_> = stats.calls.iter().collect();
    calls.sort_by_key(|c| std::cmp::Reverse(c.result_chars));
    out.push_str(
        "## Largest tool results\n\n\
         What filled the context window. A tool that routinely returns far more than the agent uses \
         is a candidate for a narrower one.\n\n\
         | # | Stage | Tool | Characters | Arguments |\n|---|---|---|---|---|\n",
    );
    for c in calls.into_iter().take(10) {
        let _ = writeln!(
            out,
            "| #{} | {} | `{}` | {} | {} |",
            c.seq,
            stage_label(c.stage_index, &c.stage),
            c.name,
            number(c.result_chars as u64),
            cell(&c.args_excerpt, 120),
        );
    }
    out.push('\n');
}

fn edit_activity(out: &mut String, stats: &Stats) {
    let activity = stats.edit_activity();
    if activity.is_empty() {
        return;
    }
    out.push_str(
        "## Edit activity (for the parallelism question)\n\n\
         Calls to tools that change the form (`set_*`, `insert_*`, `replace_*`, `remove_*`, `write_*`, `seed_*`), \
         and how many distinct targets their arguments named. Many distinct targets edited one turn at a \
         time suggest work that independent agents could split. The target is read from the usual \
         argument keys (`path`, `id`, `name`, …), so treat it as a heuristic.\n\n\
         | Stage | Write calls | Distinct targets | Turns with writes | Turns that called several tools | By tool |\n\
         |---|---|---|---|---|---|\n",
    );
    for a in activity {
        let by_tool: Vec<String> = a.by_tool.iter().map(|(t, n)| format!("`{t}` ×{n}")).collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            stage_label(a.stage_index, &a.stage),
            a.write_calls,
            a.distinct_targets,
            a.turns_with_writes,
            a.turns_with_several_calls,
            by_tool.join(", "),
        );
    }
    out.push('\n');
}

fn reviews(out: &mut String, stats: &Stats) {
    if stats.verdicts.is_empty() {
        return;
    }
    out.push_str("## Review verdicts\n\n");
    for v in &stats.verdicts {
        let _ = writeln!(
            out,
            "### Round {}: {} ({})\n",
            v.round,
            v.describe(),
            format::clock(v.at_ms)
        );
        if v.report.trim().is_empty() {
            out.push_str("(no report)\n\n");
        } else {
            out.push_str(&format::quote(&excerpt(v.report.trim(), 4_000)));
            out.push_str("\n\n");
        }
    }
}

fn controls(out: &mut String, stats: &Stats) {
    let steering = &stats.controls;
    if steering.is_empty() {
        return;
    }
    out.push_str(
        "## Control events\n\n\
         Every time the controller had to steer: nudges, retries, stuck-watch stops, exhausted budgets, \
         invalid tool calls.\n\n| At | Stage | Kind | Detail |\n|---|---|---|---|\n",
    );
    for c in steering.iter().take(TOP * 4) {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            format::clock(c.at_ms),
            stage_label(c.stage_index, &c.stage),
            c.kind.label(),
            cell(&c.detail, 200),
        );
    }
    if steering.len() > TOP * 4 {
        let _ = writeln!(out, "\n… and {} more in `timeline.md`.", steering.len() - TOP * 4);
    }
    out.push('\n');
}

fn warnings(out: &mut String, stats: &Stats) {
    if stats.warnings.is_empty() {
        return;
    }
    out.push_str("## Warnings\n\n");
    for (at, w) in &stats.warnings {
        let _ = writeln!(out, "- `{}` {}", format::clock(*at), format::one_line(w));
    }
    out.push('\n');
}

/// `evaluation.md`: the human evaluation of the run's result, a template the
/// person who checks the converted form fills in. The front matter and the
/// two tables are what `scripts/collect_evaluations.py` reads across runs, so
/// their shape is fixed; everything else is free text.
pub fn evaluation_template(meta: &RunMeta, folder: &str) -> String {
    format!(
        "---\n\
form: {label}\n\
session: {session}\n\
run_folder: {folder}\n\
started: {started}\n\
model: {model}\n\
evaluator:\n\
evaluated_on:\n\
# usable | small-fixes | major-rework | unusable\n\
verdict:\n\
# overall 1-5 (5 = deliverable as is)\n\
score:\n\
# minutes a person needed (or would need) to make the result deliverable\n\
manual_fix_minutes:\n\
---\n\
\n\
# Human evaluation — {label}\n\
\n\
Fill this in after checking the converted form (AEM preview, DoR, Redacto) against\n\
the source PDF. Keep the front matter keys and the table columns as they are:\n\
`scripts/collect_evaluations.py` reads them across all runs. `report.md` in this\n\
folder shows where the run spent its time; cite its `#seq` numbers where useful.\n\
\n\
## Summary\n\
\n\
<!-- Two or three sentences: what is good, what is wrong, would you ship it. -->\n\
\n\
## Scores\n\
\n\
1 = wrong or missing, 3 = right after small fixes, 5 = right as produced, `-` = not\n\
applicable.\n\
\n\
| Area | Score | Notes |\n\
|---|---|---|\n\
| Structure and sections (panels, wizard pages, order) | | |\n\
| Texts and labels (every language, formatting) | | |\n\
| Fields (types, options, required, lengths) | | |\n\
| Behaviour (show/hide, repeatables, set values, scripts) | | |\n\
| UBS conventions (header, PN_BR, signatures, internal bank use) | | |\n\
| DoR / PDF output | | |\n\
| Redacto output | | |\n\
\n\
## Findings\n\
\n\
One row per error found. **Category**: `source-reading`, `structure`, `text`,\n\
`field`, `behaviour`, `ubs-convention`, `dor`, `redacto`, `verification`,\n\
`pipeline`, `other`. **Severity**: `blocker` (form unusable), `major` (wrong for\n\
the customer), `minor` (cosmetic). **Fix**: what you did by hand, and how long it\n\
took.\n\
\n\
| # | Category | Severity | Where (panel / field) | What is wrong | Fix |\n\
|---|---|---|---|---|---|\n\
| 1 | | | | | |\n\
\n\
## How the run went\n\
\n\
<!-- What you noticed while it ran or in report.md: stuck stages, tool errors,\n\
     time sinks, missing review verdict, the app freezing, restarts. -->\n\
\n\
## Requirements for v3\n\
\n\
One line each, starting with `- R:`. Say what v3 must do and which finding it\n\
comes from, e.g. `- R: set min=max occurrences of signature panels from the\n\
source (finding 3)`.\n\
\n\
- R:\n",
        label = meta.label.replace('\n', " "),
        session = meta.session_id,
        started = meta.started,
        model = meta.model.replace('\n', " "),
    )
}
