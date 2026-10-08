//! The progress seam for a terminal: [`pipeline::RunObserver`] over stdout.
//!
//! The desktop app answers a failed turn with a Retry button; nobody is watching
//! a CLI run, so the decision is a budget fixed up front — retry until it is
//! spent, then stop. Everything else is the same event stream the app renders,
//! printed as it arrives and kept as a Markdown transcript so a finished run
//! leaves the same log the app offers as a download.

use std::collections::HashMap;
use std::time::Instant;

use pipeline::{RetryAction, RunEvent, RunObserver};

/// Prints a run as it happens and records it.
pub struct ConsoleObserver {
    /// The model's context window, so a per-turn fill can be read as a fraction.
    context_window: usize,
    /// Remaining operator-level retries. The controller has already exhausted
    /// its own automatic ones by the time it asks.
    retries_left: usize,
    /// The answer [`Self::retry_prompt`] decided on, read back by `poll_retry`.
    retry_action: Option<RetryAction>,
    /// The tools currently running, by call id: when each started, the line
    /// to print once it finishes, and its transcript entry. A stage runs a
    /// turn's reads side by side, so several can be running and they finish
    /// in any order.
    running: HashMap<String, Running>,
    /// Whether the abort notice has been printed (it is emitted repeatedly).
    aborted: bool,
    /// The run's cumulative spend, reported once when it finishes.
    spend: Option<pipeline::Spend>,
    transcript: Vec<String>,
}

struct Running {
    started: Instant,
    line: String,
    entry: usize,
}

impl ConsoleObserver {
    pub fn new(context_window: usize, retries: usize) -> Self {
        Self {
            context_window,
            retries_left: retries,
            retry_action: None,
            running: HashMap::new(),
            aborted: false,
            spend: None,
            transcript: vec!["# Agent Conversion Log\n".to_string()],
        }
    }

    /// The run's Markdown transcript, in the same shape the app's log download
    /// uses: thoughts as block quotes, tool calls as a checked list.
    pub fn transcript(&self) -> String {
        let mut text = self.transcript.join("\n");
        if let Some(spend) = self.spend {
            text.push_str(&format!("\n\n{}\n", spend.describe()));
        }
        text
    }

    /// Print what the run cost. Called once, after it finishes.
    pub fn report_spend(&mut self) {
        if let Some(spend) = self.spend {
            self.say(format!("\n{}", spend.describe()));
        }
    }

    /// This run's own cumulative spend, for folding into a session's running
    /// total once the session id is known — the observer itself is built
    /// before that id exists on a fresh run, so it cannot look the total up
    /// on its own.
    pub fn spend(&self) -> Option<pipeline::Spend> {
        self.spend
    }

    /// Print a session's total across every run it has been resumed for, once
    /// that differs from what this run alone cost — a first, only run has
    /// nothing else to add, and printing the same figure twice would just be
    /// noise.
    pub fn report_total_spend(&mut self, total: &pipeline::Spend) {
        if Some(*total) != self.spend {
            self.say(format!("\nSession total: {}", total.describe()));
        }
    }

    fn say(&mut self, line: impl AsRef<str>) {
        println!("{}", line.as_ref());
    }
}

impl RunObserver for ConsoleObserver {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::Stage { role, doing } => {
                self.say(format!("\n── {role} — {doing} ──"));
                self.transcript.push(format!("\n## {role} — {doing}\n"));
            }
            // Cumulative, so only the last one matters; it goes to the
            // transcript at the end rather than a line per turn.
            RunEvent::Spend(spend) => self.spend = Some(spend),
            RunEvent::Thought(text) => {
                self.say(&text);
                self.transcript
                    .push(format!("> {}\n", text.replace('\n', "\n> ")));
            }
            RunEvent::ToolStarted {
                id,
                name,
                input_summary,
            } => {
                let (line, entry) = if input_summary.is_empty() {
                    (format!("  · {name}"), format!("- `{name}`"))
                } else {
                    (
                        format!("  · {name} {input_summary}"),
                        format!("- `{name}` — {input_summary}"),
                    )
                };
                // The transcript keeps call order; the line is printed whole
                // once the call finishes, with its outcome and elapsed time.
                self.transcript.push(entry);
                let entry = self.transcript.len() - 1;
                self.running.insert(id, Running { started: Instant::now(), line, entry });
            }
            RunEvent::ToolFinished { id, ok, .. } => {
                let glyph = if ok { "✓" } else { "✗" };
                match self.running.remove(&id) {
                    Some(running) => {
                        let secs = running.started.elapsed().as_secs_f32();
                        println!("{} {glyph} ({secs:.1}s)", running.line);
                        // Mark the entry this result belongs to, matching the app's log.
                        let entry = &mut self.transcript[running.entry];
                        *entry = entry.replacen("- ", &format!("- {glyph} "), 1);
                    }
                    None => println!("  · (unknown call {id}) {glyph}"),
                }
            }
            RunEvent::Warning(w) => {
                eprintln!("warning: {w}");
                self.transcript.push(format!("\n**Warning:** {w}\n"));
            }
            RunEvent::ContextUsed(tokens) => {
                self.say(format!(
                    "  [context: {tokens} of {} tokens]",
                    self.context_window
                ));
            }
            RunEvent::Aborted => {
                if !self.aborted {
                    self.aborted = true;
                    self.say("\nRun aborted.");
                    self.transcript.push("\nRun aborted.\n".to_string());
                }
            }
            // The console keeps its log; the rule board is the app's view.
            RunEvent::Rules(_) | RunEvent::Judging { .. } => {}
        }
    }

    fn retry_prompt(&mut self, role: &str, error: &str) {
        eprintln!("\nerror: the {role} turn failed: {error}");
        // Decided here, announced in `retry_resolved`: the controller narrates
        // the pause in between, and the console should read in that order.
        self.retry_action = if self.retries_left > 0 {
            self.retries_left -= 1;
            Some(RetryAction::Retry)
        } else {
            Some(RetryAction::Cancel)
        };
        self.transcript
            .push(format!("\n**Failed ({role}):** {error}\n"));
    }

    fn poll_retry(&mut self) -> Option<RetryAction> {
        self.retry_action
    }

    fn retry_resolved(&mut self, action: RetryAction) {
        match action {
            RetryAction::Retry => eprintln!("Retrying. Retries left: {}", self.retries_left),
            RetryAction::Cancel => {
                eprintln!("Out of retries — stopping and keeping whatever was built.")
            }
        }
        self.retry_action = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stage runs a turn's reads side by side, so calls finish out of order;
    /// each outcome has to land on its own call's entry, not on the last one.
    #[test]
    fn calls_finishing_out_of_order_mark_their_own_entries() {
        let mut obs = ConsoleObserver::new(200_000, 0);
        for (id, name) in [("a", "xfa_render_pages"), ("b", "pdf_render_pages")] {
            obs.emit(RunEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input_summary: String::new(),
            });
        }
        obs.emit(RunEvent::ToolFinished { id: "b".into(), ok: false, reply_chars: 0 });
        obs.emit(RunEvent::ToolFinished { id: "a".into(), ok: true, reply_chars: 0 });

        let transcript = obs.transcript();
        assert!(transcript.contains("- ✓ `xfa_render_pages`"), "{transcript}");
        assert!(transcript.contains("- ✗ `pdf_render_pages`"), "{transcript}");
        assert!(obs.running.is_empty());
    }

    /// The retry budget has to run out: a headless run that answers "retry"
    /// forever would sit on a permanent failure — a revoked key, say — until it
    /// is killed, having reported nothing.
    #[test]
    fn the_retry_budget_is_spent_then_the_run_is_cancelled() {
        let mut obs = ConsoleObserver::new(200_000, 2);
        for _ in 0..2 {
            obs.retry_prompt("Author", "overloaded");
            assert_eq!(obs.poll_retry(), Some(RetryAction::Retry));
            obs.retry_resolved(RetryAction::Retry);
        }
        obs.retry_prompt("Author", "overloaded");
        assert_eq!(obs.poll_retry(), Some(RetryAction::Cancel));
    }

    /// A tool's outcome belongs on its own transcript entry — the log is the
    /// only record a headless run leaves of which call failed.
    #[test]
    fn the_transcript_marks_each_tool_with_its_outcome() {
        let mut obs = ConsoleObserver::new(200_000, 0);
        obs.emit(RunEvent::ToolStarted {
            id: "1".into(),
            name: "build_aem_package".into(),
            input_summary: String::new(),
        });
        obs.emit(RunEvent::ToolFinished {
            id: "1".into(),
            ok: false,
            reply_chars: 0,
        });
        assert!(
            obs.transcript().contains("- ✗ `build_aem_package`"),
            "{}",
            obs.transcript()
        );
    }
}
