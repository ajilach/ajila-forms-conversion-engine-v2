//! One conversion per tab: identity, ordering, and the rules for opening and
//! closing them.
//!
//! Everything here is plain data and pure functions — no Dioxus — so the rules
//! that decide which tab the user lands on can be tested without a desktop
//! runtime. The signals that hold the live state are wired up in `main.rs`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::models::{ProcessingState, ProcessingStep};
use crate::run_status::screen_for;

/// Where the open tabs are recorded between sessions.
///
/// The `settings` table is a plain key/value store, and `AppSettings` already
/// lives in it under `"app"` — so the workspace needs no schema of its own.
pub const WORKSPACE_KEY: &str = "workspace";

/// How many conversions may be open at once.
///
/// Each open tab can hold a full model context, a browser session and a
/// multi-megabyte package, so this is a real resource ceiling rather than a
/// tidiness rule.
pub const MAX_TABS: usize = 8;

/// The longest tab label the strip shows before eliding; the full text stays
/// available as the button's tooltip.
const TITLE_CHARS: usize = 18;

/// A tab's identity.
///
/// Monotonic and never reused, so a run that finishes long after its tab was
/// closed cannot be mistaken for a new tab that happens to sit in the same slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

/// The counter behind [`TabId::next`].
static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

impl TabId {
    /// Mint the next identity.
    pub fn next() -> Self {
        Self(NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Restore a saved identity.
    pub fn from_saved(id: u64) -> Self {
        Self(id)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Make sure freshly minted ids sort above every restored one.
    ///
    /// Without this, a tab opened after a restore could be handed an id a
    /// restored tab already holds — and two tabs sharing an identity is exactly
    /// what makes a finished run write into the wrong one.
    pub fn reserve_above(highest: u64) {
        NEXT_TAB_ID.fetch_max(highest + 1, Ordering::Relaxed);
    }
}

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The phase a tab was in when the workspace was last written.
///
/// Recorded rather than re-derived, because it is the only way to tell a run
/// that was *interrupted* from one that finished: a tab still marked `Running`
/// when the app starts is by definition a run the window closed out from under.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabPhase {
    /// Nothing started yet.
    #[default]
    Upload,
    Running,
    Finished,
    Failed,
}

impl TabPhase {
    /// The phase a live tab is in right now.
    pub fn of(state: &ProcessingState, processing: bool) -> Self {
        if state.step == ProcessingStep::Complete {
            Self::Finished
        } else if processing {
            Self::Running
        } else if state.error.is_some() || state.aborted {
            Self::Failed
        } else {
            Self::Upload
        }
    }
}

/// One tab as it survives a restart.
///
/// Deliberately not the whole tab. The built package, the schema and the SQL
/// dump are rebuilt from the session rather than stored, and the activity
/// transcript is megabytes of model prose that would be rewritten on every
/// step — so what is kept here is the choices the user made plus enough of the
/// result to describe it before the outputs are rebuilt.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTab {
    pub id: u64,
    pub profile: Option<String>,
    pub target: blueprint::OutputTarget,
    /// The edit-history session, which is what makes the tab continuable.
    pub session_id: Option<String>,
    /// Content hash of the sources, for finding the stored bytes again.
    pub doc_hash: Option<String>,
    /// File names, so the chips can be shown before the bytes are loaded.
    pub source_names: Vec<String>,
    pub phase: TabPhase,
    pub form_code: Option<String>,
    pub aem_uploaded: bool,
    pub aem_form_path: Option<String>,
    pub elapsed_secs: Option<u64>,
    pub warnings: Vec<String>,
    /// Unsent feedback, so a half-typed note is not lost to a restart.
    pub feedback_draft: String,
}

/// Every open tab, as written to the store.
///
/// `version` is explicit and every field defaults, so a blob written by an older
/// or newer build still loads instead of throwing the workspace away.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedWorkspace {
    pub version: u32,
    pub active: Option<u64>,
    pub tabs: Vec<SavedTab>,
}

/// The revision [`SavedWorkspace`] is written at.
pub const WORKSPACE_VERSION: u32 = 1;

impl SavedWorkspace {
    /// Read a workspace back, falling back to a single empty tab.
    ///
    /// A blob that cannot be parsed is treated as no blob at all: losing the tab
    /// list is a nuisance, but refusing to start is worse.
    pub fn parse(json: Option<&str>) -> Self {
        let mut saved: Self = json
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        saved.tabs.truncate(MAX_TABS);
        // An id no tab carries would leave the workspace with nothing selected.
        if !saved.tabs.iter().any(|t| Some(t.id) == saved.active) {
            saved.active = saved.tabs.first().map(|t| t.id);
        }
        saved
    }

    /// The highest identity in the blob, so new tabs are minted above it.
    pub fn highest_id(&self) -> u64 {
        self.tabs.iter().map(|t| t.id).max().unwrap_or(0)
    }
}

/// What a restored tab shows before the user touches it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoredView {
    /// Nothing was started, or nothing survived worth showing.
    Upload,
    /// The run finished. Its outputs are rebuilt on demand.
    Finished,
    /// The window closed while the run was going. Its last snapshot is still
    /// there, so it can be rebuilt or carried on with feedback.
    Interrupted,
    /// A session was recorded but holds no document — nothing to go back to.
    Orphaned,
}

/// Decide what a restored tab shows.
///
/// `has_snapshot` is whether the session holds a document beyond the empty seed
/// a run writes when it starts. Passed in rather than looked up, so the decision
/// is testable on its own.
///
/// Whether the tab can be *continued* is a separate question, decided by the box
/// from the sources it actually holds: a tab whose outputs are describable is
/// still worth showing even when the bytes it would need to re-run are gone.
pub fn restored_view(tab: &SavedTab, has_snapshot: bool) -> RestoredView {
    // No session means no run ever got far enough to record one.
    if tab.session_id.is_none() {
        return RestoredView::Upload;
    }
    match tab.phase {
        TabPhase::Upload => RestoredView::Upload,
        // A session was recorded but holds nothing: the run died during
        // analysis, before the first snapshot. There is nothing to go back to.
        _ if !has_snapshot => RestoredView::Orphaned,
        // Still going when the window closed. The agent is gone, but every tool
        // call snapshotted the tree, so there is a partial result to rebuild —
        // and feedback resumes from exactly that point.
        TabPhase::Running => RestoredView::Interrupted,
        TabPhase::Finished | TabPhase::Failed => RestoredView::Finished,
    }
}


/// What to call a tab in the strip.
///
/// The form code first, because that is what the user calls the work; the file
/// it came from until the run has discovered one; and a placeholder before
/// anything has been dropped on it.
pub fn tab_title(form_code: Option<&str>, source_names: &[&str]) -> String {
    if let Some(code) = form_code.map(str::trim).filter(|c| !c.is_empty()) {
        return elide(code);
    }
    if let Some(name) = source_names.first() {
        // The extension is noise in an 18-character label, and every source
        // shares one of two.
        let stem = name.rsplit_once('.').map_or(*name, |(stem, _)| stem);
        if !stem.is_empty() {
            return elide(stem);
        }
    }
    "New form".to_string()
}

/// Shorten to [`TITLE_CHARS`], marking that something was cut.
///
/// Counts characters rather than bytes: a label cut mid-codepoint would panic.
fn elide(text: &str) -> String {
    if text.chars().count() <= TITLE_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", kept.trim_end())
}

/// The dot the strip shows for a tab, from the same rule the box uses.
pub fn tab_dot(state: &ProcessingState, processing: bool) -> &'static str {
    screen_for(state, processing).dot_class()
}

/// Whether a tab has a run in flight that closing it would abandon.
///
/// A run is in flight exactly while its future is alive and it has not published
/// a result. A *paused* run counts: it is waiting on the user's answer to a
/// failed request, not finished, and its whole working tree is still in memory.
pub fn is_running(state: &ProcessingState, processing: bool) -> bool {
    processing && state.step != ProcessingStep::Complete
}

/// Which tab to show once `closing` is gone.
///
/// Returns `None` when the closed tab was the last one — the caller opens a
/// fresh tab rather than showing an empty workspace.
///
/// Moves right, because the tabs to the right shift into the closed tab's
/// position and landing on the one that took its place is what the user expects.
/// Closing the rightmost tab falls back to its left neighbour.
pub fn active_after_close(ids: &[TabId], closing: TabId, active: TabId) -> Option<TabId> {
    let Some(index) = ids.iter().position(|id| *id == closing) else {
        // Closing some other tab never moves the selection.
        return Some(active);
    };
    if active != closing {
        return Some(active);
    }
    ids.get(index + 1).or_else(|| index.checked_sub(1).and_then(|i| ids.get(i))).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<TabId> {
        (0..n).map(|_| TabId::next()).collect()
    }

    #[test]
    fn identities_are_never_reused() {
        let a = TabId::next();
        let b = TabId::next();
        assert_ne!(a, b, "a reused id would let a finished run write to a new tab");
    }

    /// The tab that slides into the closed one's place is the one the user is
    /// looking at, so the selection moves right and only falls back at the end.
    #[test]
    fn closing_the_active_tab_lands_on_its_neighbour() {
        let t = ids(3);

        // Middle: the tab to the right takes its place.
        assert_eq!(active_after_close(&t, t[1], t[1]), Some(t[2]));
        // First: same rule.
        assert_eq!(active_after_close(&t, t[0], t[0]), Some(t[1]));
        // Last: nothing to the right, so fall back left.
        assert_eq!(active_after_close(&t, t[2], t[2]), Some(t[1]));
    }

    #[test]
    fn closing_a_background_tab_leaves_the_selection_alone() {
        let t = ids(3);
        assert_eq!(active_after_close(&t, t[0], t[2]), Some(t[2]));
        assert_eq!(active_after_close(&t, t[2], t[0]), Some(t[0]));
    }

    /// The workspace is never empty: the caller reads `None` as "open a fresh
    /// tab", which is less jarring than an empty-state screen.
    #[test]
    fn closing_the_only_tab_leaves_nothing_to_select() {
        let t = ids(1);
        assert_eq!(active_after_close(&t, t[0], t[0]), None);
    }

    #[test]
    fn the_title_prefers_the_form_code() {
        assert_eq!(tab_title(Some("AAOV_033"), &["AAOV_033_DE.pdf"]), "AAOV_033");
        assert_eq!(tab_title(None, &["AAOV_033_DE.pdf"]), "AAOV_033_DE");
        assert_eq!(tab_title(None, &[]), "New form");
        // A run that reported an empty code is the same as no code.
        assert_eq!(tab_title(Some("  "), &["AABF_019_EN.pdf"]), "AABF_019_EN");
        // A name that is nothing but an extension has no stem to show.
        assert_eq!(tab_title(None, &[".pdf"]), "New form");
    }

    #[test]
    fn a_long_title_is_elided_rather_than_cut_mid_character() {
        let title = tab_title(None, &["Lebensversicherungsantrag_Ü.pdf"]);
        assert!(title.chars().count() <= TITLE_CHARS, "{title}");
        assert!(title.ends_with('…'), "{title}");
        // A label exactly at the limit keeps every character.
        let exact = "a".repeat(TITLE_CHARS);
        assert_eq!(tab_title(Some(&exact), &[]), exact);
    }

    fn saved(phase: TabPhase, session: Option<&str>) -> SavedTab {
        SavedTab {
            id: 1,
            phase,
            session_id: session.map(str::to_string),
            ..SavedTab::default()
        }
    }

    /// The three cases a restart has to tell apart, plus the one where there is
    /// genuinely nothing left.
    #[test]
    fn a_restored_tab_shows_what_actually_survived() {
        // Never started: back to the upload screen with its choices intact.
        assert_eq!(
            restored_view(&saved(TabPhase::Upload, None), false),
            RestoredView::Upload
        );
        // A phase without a session cannot be anything but a fresh tab.
        assert_eq!(
            restored_view(&saved(TabPhase::Finished, None), true),
            RestoredView::Upload
        );

        // Finished, with a document to rebuild from.
        assert_eq!(
            restored_view(&saved(TabPhase::Finished, Some("s")), true),
            RestoredView::Finished
        );
        // A failed run that still recorded work is worth reopening the same way.
        assert_eq!(
            restored_view(&saved(TabPhase::Failed, Some("s")), true),
            RestoredView::Finished
        );

        // Still running when the window closed — interrupted, not failed.
        assert_eq!(
            restored_view(&saved(TabPhase::Running, Some("s")), true),
            RestoredView::Interrupted
        );

        // A session that holds nothing but its empty seed.
        assert_eq!(
            restored_view(&saved(TabPhase::Running, Some("s")), false),
            RestoredView::Orphaned
        );
        assert_eq!(
            restored_view(&saved(TabPhase::Finished, Some("s")), false),
            RestoredView::Orphaned
        );
    }

    #[test]
    fn the_phase_is_read_off_the_live_run() {
        let idle = ProcessingState::default();
        assert_eq!(TabPhase::of(&idle, false), TabPhase::Upload);

        let running = ProcessingState {
            step: ProcessingStep::Running,
            ..Default::default()
        };
        assert_eq!(TabPhase::of(&running, true), TabPhase::Running);

        let done = ProcessingState {
            step: ProcessingStep::Complete,
            ..Default::default()
        };
        assert_eq!(TabPhase::of(&done, false), TabPhase::Finished);

        let failed = ProcessingState {
            error: Some("boom".into()),
            ..running.clone()
        };
        assert_eq!(TabPhase::of(&failed, false), TabPhase::Failed);

        // An aborted run is terminal too, and not a failure of the box's making.
        let aborted = ProcessingState {
            aborted: true,
            ..running
        };
        assert_eq!(TabPhase::of(&aborted, false), TabPhase::Failed);
    }

    #[test]
    fn a_workspace_round_trips_through_the_store() {
        let original = SavedWorkspace {
            version: WORKSPACE_VERSION,
            active: Some(2),
            tabs: vec![
                saved(TabPhase::Finished, Some("s1")),
                SavedTab {
                    id: 2,
                    profile: Some("ubs".into()),
                    target: blueprint::OutputTarget::Redacto,
                    source_names: vec!["AAOV_033_DE.pdf".into()],
                    feedback_draft: "half a thought".into(),
                    ..SavedTab::default()
                },
            ],
        };

        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(SavedWorkspace::parse(Some(&json)), original);
    }

    /// A blob from another build, or a corrupt one, must not stop the app.
    #[test]
    fn an_unreadable_workspace_falls_back_instead_of_failing() {
        for json in [None, Some("not json at all"), Some("{}"), Some("[]")] {
            let saved = SavedWorkspace::parse(json);
            assert!(saved.tabs.is_empty(), "{json:?}");
            assert_eq!(saved.active, None, "{json:?}");
        }

        // Unknown fields are ignored and missing ones default, so a blob written
        // by a newer build still opens.
        let forward = SavedWorkspace::parse(Some(
            r#"{"version":99,"active":7,"tabs":[{"id":7,"whats_this":true}],"and_this":1}"#,
        ));
        assert_eq!(forward.tabs.len(), 1);
        assert_eq!(forward.active, Some(7));
        assert_eq!(forward.tabs[0].phase, TabPhase::Upload);
    }

    /// The selection has to name a tab that is actually there.
    #[test]
    fn a_dangling_active_id_falls_back_to_the_first_tab() {
        let saved = SavedWorkspace::parse(Some(
            r#"{"version":1,"active":42,"tabs":[{"id":3},{"id":4}]}"#,
        ));
        assert_eq!(saved.active, Some(3));
    }

    #[test]
    fn a_blob_over_the_tab_cap_is_trimmed() {
        let tabs: Vec<String> = (0..MAX_TABS + 4).map(|i| format!(r#"{{"id":{i}}}"#)).collect();
        let json = format!(r#"{{"version":1,"tabs":[{}]}}"#, tabs.join(","));
        let saved = SavedWorkspace::parse(Some(&json));
        assert_eq!(saved.tabs.len(), MAX_TABS);
    }

    /// A restored id must never be handed out again: two tabs sharing an
    /// identity is what makes a finished run publish into the wrong one.
    #[test]
    fn new_identities_sort_above_restored_ones() {
        TabId::reserve_above(10_000);
        assert!(TabId::next().get() > 10_000);
    }

    /// The whole promise of persistence in one pass: three tabs in different
    /// states go through the store as JSON and each comes back describing
    /// itself correctly.
    #[test]
    fn a_mixed_workspace_survives_the_round_trip_intact() {
        let workspace = SavedWorkspace {
            version: WORKSPACE_VERSION,
            active: Some(2),
            tabs: vec![
                SavedTab {
                    id: 1,
                    phase: TabPhase::Finished,
                    session_id: Some("done".into()),
                    doc_hash: Some("h1".into()),
                    form_code: Some("AAOV_033".into()),
                    source_names: vec!["AAOV_033_DE.pdf".into()],
                    elapsed_secs: Some(412),
                    ..SavedTab::default()
                },
                SavedTab {
                    id: 2,
                    phase: TabPhase::Running,
                    session_id: Some("cut-off".into()),
                    doc_hash: Some("h2".into()),
                    source_names: vec!["AABF_019_EN.pdf".into()],
                    feedback_draft: "make the phone field optional".into(),
                    ..SavedTab::default()
                },
                SavedTab {
                    id: 3,
                    profile: Some("ubs".into()),
                    target: blueprint::OutputTarget::Redacto,
                    ..SavedTab::default()
                },
            ],
        };

        let json = serde_json::to_string(&workspace).unwrap();
        let back = SavedWorkspace::parse(Some(&json));
        assert_eq!(back, workspace, "the workspace did not survive the store");
        assert_eq!(back.active, Some(2));
        assert_eq!(back.highest_id(), 3);

        // The finished tab has a snapshot to describe and rebuild from.
        assert_eq!(restored_view(&back.tabs[0], true), RestoredView::Finished);
        // The one the window closed on is interrupted, not failed — its partial
        // tree is there, and the half-typed note with it.
        assert_eq!(restored_view(&back.tabs[1], true), RestoredView::Interrupted);
        assert_eq!(back.tabs[1].feedback_draft, "make the phone field optional");
        // The one that never started keeps its choices and nothing else.
        assert_eq!(restored_view(&back.tabs[2], false), RestoredView::Upload);
        assert_eq!(back.tabs[2].target, blueprint::OutputTarget::Redacto);
        assert_eq!(back.tabs[2].profile.as_deref(), Some("ubs"));
    }

    #[test]
    fn only_a_tab_with_work_in_flight_counts_as_running() {
        let idle = ProcessingState::default();
        assert!(!is_running(&idle, false));

        let running = ProcessingState {
            step: ProcessingStep::Running,
            ..Default::default()
        };
        assert!(is_running(&running, true));

        // Finished, failed and aborted tabs all close without losing anything.
        let done = ProcessingState {
            step: ProcessingStep::Complete,
            ..Default::default()
        };
        assert!(!is_running(&done, false));
        let failed = ProcessingState {
            error: Some("boom".into()),
            ..running
        };
        assert!(!is_running(&failed, false));
    }
}
