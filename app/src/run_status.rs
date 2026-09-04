//! What the conversion box — and the tab that stands for it — is showing.
//!
//! The tab strip's status dot and the run box both have to describe the same
//! run. Deriving them from one function here is what keeps them from drifting:
//! a tab that says "done" over a box that says "working" is worse than either.

use crate::models::{ProcessingState, ProcessingStep};

/// What the box shows: either the upload form, or a run in one of its states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    Upload,
    Run(RunStatus),
}

/// How a run is doing. `Paused` is a live run waiting on the user's answer to a
/// failed request — the header, the phase rail and the badge all switch on it,
/// so it is one value rather than a phase plus a flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Running,
    Paused,
    Done,
    /// The run ended on an error (including the user giving up on a paused,
    /// retryable request) — the box reports it and offers a fresh start.
    Failed,
}

impl RunStatus {
    /// Modifier class and glyph for the status badge. `None` glyph means the
    /// badge shows a spinner instead.
    pub fn badge(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Running => ("run", None),
            Self::Paused => ("warn", Some("⏸")),
            Self::Done => ("ok", Some("✓")),
            Self::Failed => ("err", Some("✗")),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Running => "Agent is working",
            Self::Paused => "Agent paused",
            Self::Done => "Finished",
            Self::Failed => "Agent stopped",
        }
    }

    /// Whether the run reached the end successfully.
    pub fn is_done(self) -> bool {
        self == Self::Done
    }
}

impl Screen {
    /// Modifier class for the dot that stands for this screen in the tab strip.
    ///
    /// Reuses [`RunStatus::badge`] so a tab and its box can never disagree; a
    /// tab that has not started yet gets its own neutral class.
    // Rendered by the tab strip, which does not exist yet.
    #[allow(dead_code)]
    pub fn dot_class(self) -> &'static str {
        match self {
            Self::Upload => "idle",
            Self::Run(status) => status.badge().0,
        }
    }
}

/// Derive what to show from the run state. The `Complete` step wins over
/// everything; a stopped run that recorded an error, or that the user aborted,
/// has ended; anything else with work in flight is a run in progress.
pub fn screen_for(state: &ProcessingState, processing: bool) -> Screen {
    if state.step == ProcessingStep::Complete {
        Screen::Run(RunStatus::Done)
    } else if !processing && (state.error.is_some() || state.aborted) {
        Screen::Run(RunStatus::Failed)
    } else if processing || state.step != ProcessingStep::Idle {
        Screen::Run(if state.retry_pending {
            RunStatus::Paused
        } else {
            RunStatus::Running
        })
    } else {
        Screen::Upload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four states the box can be in are derived from three separate fields,
    /// so pin the mapping down — a wrong screen strands the user.
    #[test]
    fn the_screen_follows_the_run_state() {
        let idle = ProcessingState::default();
        assert_eq!(screen_for(&idle, false), Screen::Upload);

        let running = ProcessingState {
            step: ProcessingStep::Running,
            ..Default::default()
        };
        assert_eq!(screen_for(&running, true), Screen::Run(RunStatus::Running));

        let paused = ProcessingState {
            retry_pending: true,
            error: Some("boom".into()),
            ..running.clone()
        };
        assert_eq!(screen_for(&paused, true), Screen::Run(RunStatus::Paused));

        // The run stopped and recorded an error: failed, not still running.
        let failed = ProcessingState {
            error: Some("boom".into()),
            ..running
        };
        assert_eq!(screen_for(&failed, false), Screen::Run(RunStatus::Failed));

        // A completed run reports Done even if it also collected an error.
        let complete = ProcessingState {
            step: ProcessingStep::Complete,
            error: Some("boom".into()),
            ..Default::default()
        };
        assert_eq!(screen_for(&complete, false), Screen::Run(RunStatus::Done));
    }

    /// An aborted run records no error, so without its own arm the box would sit
    /// on "Agent is working" forever after the run had already stopped.
    #[test]
    fn an_aborted_run_reaches_a_terminal_screen() {
        let aborting = ProcessingState {
            step: ProcessingStep::Running,
            aborted: true,
            ..Default::default()
        };

        // The flag is set the moment the button is pressed, but the run is still
        // unwinding — it must keep reporting as running until it has stopped.
        assert_eq!(
            screen_for(&aborting, true),
            Screen::Run(RunStatus::Running),
            "the box must not claim the run ended while it is still unwinding"
        );

        assert_eq!(
            screen_for(&aborting, false),
            Screen::Run(RunStatus::Failed),
            "once the run has stopped the box has to leave the running state"
        );
    }

    /// The tab strip and the box read the same run, so the dot and the badge
    /// have to be the same class — this is the assertion that stops them
    /// drifting apart as either side grows a new state.
    #[test]
    fn the_tab_dot_matches_the_box_badge() {
        for status in [
            RunStatus::Running,
            RunStatus::Paused,
            RunStatus::Done,
            RunStatus::Failed,
        ] {
            assert_eq!(
                Screen::Run(status).dot_class(),
                status.badge().0,
                "the dot for {status:?} has to be the badge's own class"
            );
        }

        // A tab that has not started carries neither, so it needs its own class.
        assert_eq!(Screen::Upload.dot_class(), "idle");
    }
}
