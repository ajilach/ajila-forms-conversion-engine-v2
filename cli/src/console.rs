//! The progress seam for a terminal: [`pipeline::RunObserver`] over stdout.
//!
//! The desktop app answers a failed turn with a Retry button; nobody is watching
//! a CLI run, so the decision is a budget fixed up front — retry until it is
//! spent, then stop. Everything else is the same event stream the app renders,
//! printed as it arrives and kept as a Markdown transcript so a finished run
//! leaves the same log the app offers as a download.

use std::io::Write;
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
    /// The tool currently running, so its result lands on the same line.
    running: Option<Instant>,
    /// Whether the abort notice has been printed (it is emitted repeatedly).
    aborted: bool,
    /// The run's cumulative spend, reported once when it finishes.
    spend: Option<pipeline::Spend>,
    transcript: Vec<String>,
}

impl ConsoleObserver {
    pub fn new(context_window: usize, retries: usize) -> Self {
        Self {
            context_window,
            retries_left: retries,
            retry_action: None,
            running: None,
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
                name,
                input_summary,
                ..
            } => {
                self.running = Some(Instant::now());
                let (line, entry) = if input_summary.is_empty() {
                    (format!("  · {name}"), format!("- `{name}`"))
                } else {
                    (
                        format!("  · {name} {input_summary}"),
                        format!("- `{name}` — {input_summary}"),
                    )
                };
                // No newline: the outcome and the elapsed time complete this
                // line as soon as the tool returns.
                print!("{line}");
                let _ = std::io::stdout().flush();
                self.transcript.push(entry);
            }
            RunEvent::ToolFinished { ok, .. } => {
                let secs = self
                    .running
                    .take()
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                let glyph = if ok { "✓" } else { "✗" };
                println!(" {glyph} ({secs:.1}s)");
                // Mark the entry this result belongs to, matching the app's log.
                if let Some(last) = self.transcript.last_mut() {
                    *last = last.replacen("- ", &format!("- {glyph} "), 1);
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
        });
        assert!(
            obs.transcript().contains("- ✗ `build_aem_package`"),
            "{}",
            obs.transcript()
        );
    }
}
