//! The open conversions and the signals each one owns.
//!
//! [`tabs`](crate::tabs) holds the rules; this holds the state they act on. A
//! tab is a bundle of signals rather than a struct behind one signal, because
//! the components already take exactly these handles — and because a run
//! emitting a step should not re-render the profile selector of the tab it is
//! running in, let alone of any other tab.

use std::collections::HashMap;
use std::path::PathBuf;

use dioxus::prelude::*;

use crate::models::{AbortFlag, ProcessingState, RetryAnswer, RunState, UploadState};
use crate::tabs::{
    active_after_close, RestoredView, SavedTab, SavedWorkspace, TabId, TabPhase, MAX_TABS,
    WORKSPACE_VERSION,
};

/// One open conversion.
///
/// `Copy`, so it can be captured by the event handlers and by the run's own
/// future without ceremony.
///
/// Every signal is created in [`ScopeId::APP`], which never unmounts. That is
/// the load-bearing detail: a run's progress task holds `state` for minutes and
/// writes to it throughout, so the handle has to outlive the tab strip button that
/// happened to be on screen when the run started, and survive other tabs being
/// opened, closed and reordered around it.
#[derive(Clone, Copy, PartialEq)]
pub struct Tab {
    pub id: TabId,
    /// The run's progress, read by the box. Written only on the UI thread, from
    /// what the run sends (see [`RunState`]).
    pub state: RunState,
    /// `true` while this tab's run future is alive.
    pub processing: Signal<bool>,
    /// Conversion profile, chosen per tab.
    pub profile: Signal<Option<String>>,
    /// What this tab's run produces, chosen per tab.
    pub target: Signal<blueprint::OutputTarget>,
    /// The uploaded sources — both the upload box's selection and the input a
    /// feedback re-run resends.
    pub files: Signal<Vec<(String, Vec<u8>)>>,
    /// Draft text in the feedback field.
    pub feedback: Signal<String>,
    /// Whether the activity timeline is expanded.
    pub timeline_open: Signal<bool>,
    /// Edit-history session, once the run has opened one.
    pub session_id: Signal<Option<String>>,
    /// Stops this tab's run, and only this tab's run.
    pub abort: Signal<AbortFlag>,
    /// The Retry / Give up answer for this tab's run when it pauses.
    pub retry: Signal<RetryAnswer>,
    /// Progress of the on-demand "Upload to AEM" action.
    pub aem_upload: Signal<UploadState>,
    /// Set when this tab came back from a previous session and has not been
    /// re-run since, so the box can say what did and did not survive.
    pub restored: Signal<Option<RestoredView>>,
    /// Where this tab last saved each artefact, keyed by file name.
    ///
    /// Artefact names carry only the form code, so two tabs converting one form
    /// collide. Remembering our own file is what lets a second download replace
    /// it instead of landing beside it as `(2)`, while still refusing to
    /// overwrite the other tab's.
    pub last_download: Signal<HashMap<String, PathBuf>>,
    /// What every run this tab has made has cost, together — unlike
    /// `state`'s own `spend`, a new run never resets this; only "Start over"
    /// does, since that begins a different form in this slot.
    pub total_spend: Signal<pipeline::Spend>,
}

impl Tab {
    /// Open a tab, inheriting the choices the user last made.
    fn open(profile: Option<String>, target: blueprint::OutputTarget) -> Self {
        fn app<T: 'static>(value: T) -> Signal<T> {
            Signal::new_in_scope(value, ScopeId::APP)
        }
        Self {
            id: TabId::next(),
            state: app(ProcessingState::default()),
            processing: app(false),
            profile: app(profile),
            target: app(target),
            files: app(Vec::new()),
            feedback: app(String::new()),
            timeline_open: app(false),
            session_id: app(None),
            abort: app(AbortFlag::default()),
            retry: app(RetryAnswer::default()),
            aem_upload: app(UploadState::default()),
            restored: app(None),
            last_download: app(HashMap::new()),
            total_spend: app(pipeline::Spend::default()),
        }
    }

    /// Rebuild a tab from what a previous session recorded.
    ///
    /// The run's artefacts are not restored — they are rebuilt from the session
    /// on demand — but everything needed to describe the run and to carry it on
    /// is: the session id, the sources, and the result's headline facts.
    fn restore(saved: &SavedTab, files: Vec<(String, Vec<u8>)>, view: RestoredView) -> Self {
        fn app<T: 'static>(value: T) -> Signal<T> {
            Signal::new_in_scope(value, ScopeId::APP)
        }

        // Which screen a reopened tab lands on, and why it never lands on a
        // running one, is decided in `tabs` where it can be tested.
        let restored = crate::tabs::restored_run(saved, view);

        Self {
            id: TabId::from_saved(saved.id),
            state: app(restored.state),
            // Never `true`: reopening a workspace must not start anything. The
            // run that filled this tab ended with the process that drove it, and
            // carrying on is the operator's explicit call — the Continue button.
            processing: app(restored.processing),
            profile: app(saved.profile.clone()),
            target: app(saved.target),
            files: app(files),
            feedback: app(saved.feedback_draft.clone()),
            timeline_open: app(false),
            session_id: app(saved.session_id.clone()),
            abort: app(AbortFlag::default()),
            retry: app(RetryAnswer::default()),
            aem_upload: app(if saved.aem_uploaded {
                UploadState::Success
            } else {
                UploadState::default()
            }),
            restored: app(match view {
                RestoredView::Finished | RestoredView::Interrupted => Some(view),
                // Nothing came back, so there is nothing to explain.
                RestoredView::Upload | RestoredView::Orphaned => None,
            }),
            // Deliberately not restored: a path recorded in an earlier session
            // may since have been moved, renamed or deleted, and overwriting
            // whatever sits there now would be worse than adding a file.
            last_download: app(HashMap::new()),
            total_spend: app(saved.total_spend.unwrap_or_default()),
        }
    }

    /// This tab as it should survive a restart.
    pub fn snapshot(&self, doc_hash: Option<String>) -> SavedTab {
        let state = self.state.read();
        SavedTab {
            id: self.id.get(),
            profile: self.profile.read().clone(),
            target: *self.target.read(),
            session_id: self.session_id.read().clone(),
            doc_hash,
            source_names: self
                .files
                .read()
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            phase: TabPhase::of(&state, (self.processing)()),
            form_code: state.form_code.clone(),
            aem_uploaded: state.aem_uploaded,
            aem_form_path: state.aem_form_path.clone(),
            elapsed_secs: state.elapsed_secs,
            warnings: state.warnings.clone(),
            feedback_draft: self.feedback.read().clone(),
            total_spend: Some(*self.total_spend.read()),
        }
    }

    /// Release every signal this tab owns.
    ///
    /// Required, not housekeeping: an app-scope signal lives as long as the
    /// process, and `ProcessingState` holds the built package — several
    /// megabytes that would otherwise never be reclaimed.
    ///
    /// Only ever called once the tab's run has ended — its progress task has
    /// applied the last update — because writing to a released signal panics.
    fn release(self) {
        self.state.manually_drop();
        self.processing.manually_drop();
        self.profile.manually_drop();
        self.target.manually_drop();
        self.files.manually_drop();
        self.feedback.manually_drop();
        self.timeline_open.manually_drop();
        self.session_id.manually_drop();
        self.abort.manually_drop();
        self.retry.manually_drop();
        self.aem_upload.manually_drop();
        self.restored.manually_drop();
        self.last_download.manually_drop();
        self.total_spend.manually_drop();
    }

    /// Whether a run is in flight in this tab.
    pub fn is_running(&self) -> bool {
        crate::tabs::is_running(&self.state.read(), (self.processing)())
    }
}

/// Every open tab, plus the ones waiting for their aborted run to unwind.
#[derive(Clone, Copy, PartialEq)]
pub struct Workspace {
    tabs: Signal<Vec<Tab>>,
    active: Signal<TabId>,
    /// Tabs the user has closed whose run has not stopped yet. They are already
    /// gone from the strip; their signals are released once the run returns.
    closing: Signal<Vec<Tab>>,
}

impl Workspace {
    /// Build the workspace, reopening whatever the last session left. A hook —
    /// call it once, from the root component.
    ///
    /// `sources_for` loads the stored bytes for a tab's document hash; passed in
    /// so this stays free of the store.
    pub fn use_init(
        saved: &SavedWorkspace,
        profile: Option<&str>,
        target: blueprint::OutputTarget,
        // What survived for each saved tab: its stored sources, and how much of
        // its result is still reachable. Passed in so this stays free of the
        // store.
        reopen: impl Fn(&SavedTab) -> (Vec<(String, Vec<u8>)>, RestoredView),
    ) -> Self {
        let restored = use_hook(|| {
            // New tabs have to be minted above every restored id, or one could
            // be handed an identity a live tab already holds.
            TabId::reserve_above(saved.highest_id());

            let tabs: Vec<Tab> = saved
                .tabs
                .iter()
                .map(|tab| {
                    let (files, view) = reopen(tab);
                    Tab::restore(tab, files, view)
                })
                .collect();

            if tabs.is_empty() {
                let fresh = Tab::open(profile.map(str::to_string), target);
                return (vec![fresh], fresh.id);
            }
            let active = saved
                .active
                .and_then(|id| tabs.iter().find(|t| t.id.get() == id))
                .or_else(|| tabs.first())
                .map(|t| t.id)
                .expect("the tab list was checked non-empty");
            (tabs, active)
        });

        Self {
            tabs: use_signal(|| restored.0.clone()),
            active: use_signal(|| restored.1),
            closing: use_signal(Vec::new),
        }
    }

    /// The whole workspace as it should survive a restart.
    ///
    /// `doc_hash_for` turns a tab's files into the key its stored bytes live
    /// under; passed in so this stays free of the store.
    pub fn snapshot(
        &self,
        doc_hash_for: impl Fn(&[(String, Vec<u8>)]) -> Option<String>,
    ) -> SavedWorkspace {
        SavedWorkspace {
            version: WORKSPACE_VERSION,
            active: Some((self.active)().get()),
            tabs: self
                .tabs
                .read()
                .iter()
                .map(|tab| {
                    let hash = doc_hash_for(&tab.files.read());
                    tab.snapshot(hash)
                })
                .collect(),
        }
    }

    pub fn tabs(&self) -> Vec<Tab> {
        self.tabs.read().clone()
    }

    pub fn active_id(&self) -> TabId {
        (self.active)()
    }

    /// The tab currently on screen.
    ///
    /// Falls back to the first tab: the active id and the list are written
    /// together, so they cannot actually diverge, and a panic here would take
    /// the window down over a bookkeeping slip.
    pub fn active_tab(&self) -> Tab {
        let tabs = self.tabs.read();
        let active = (self.active)();
        tabs.iter()
            .find(|t| t.id == active)
            .or_else(|| tabs.first())
            .copied()
            .expect("the workspace always holds at least one tab")
    }

    /// How many tabs have a run in flight, for the header's badge.
    ///
    /// Reads only `processing`, which flips twice per run. Deriving it from the
    /// run states instead would subscribe the root component to every tab's
    /// progress, so one chatty run would re-render every other tab's box on
    /// every tool call.
    pub fn running_count(&self) -> usize {
        self.tabs.read().iter().filter(|t| (t.processing)()).count()
    }

    pub fn is_full(&self) -> bool {
        self.tabs.read().len() >= MAX_TABS
    }

    /// Open a tab beside the current one and switch to it.
    pub fn open(&mut self) {
        if self.is_full() {
            return;
        }
        let current = self.active_tab();
        let tab = Tab::open(current.profile.peek().clone(), *current.target.peek());
        self.tabs.write().push(tab);
        self.active.set(tab.id);
    }

    pub fn activate(&mut self, id: TabId) {
        self.active.set(id);
    }

    /// Close a tab, stopping its run if one is still going.
    ///
    /// The tab leaves the strip immediately either way. When a run is still in
    /// flight the signals cannot be released yet — the run is still writing to
    /// them — so the tab is parked in `closing` and [`Self::finish_run`] cleans
    /// it up when the run returns.
    pub fn close(&mut self, id: TabId) {
        let ids: Vec<TabId> = self.tabs.read().iter().map(|t| t.id).collect();
        let Some(index) = ids.iter().position(|i| *i == id) else {
            return;
        };
        let next = active_after_close(&ids, id, (self.active)());
        let tab = self.tabs.write().remove(index);

        match next {
            Some(next) => self.active.set(next),
            // The workspace is never empty: closing the last tab leaves a fresh
            // one rather than a screen with nothing on it.
            None => {
                let fresh = Tab::open(tab.profile.peek().clone(), *tab.target.peek());
                self.tabs.write().push(fresh);
                self.active.set(fresh.id);
            }
        }

        if tab.is_running() {
            tab.abort.peek().abort();
            self.closing.write().push(tab);
        } else {
            tab.release();
        }
    }

    /// Record that `id`'s run has ended, releasing the tab if it was closed
    /// while the run was still unwinding.
    pub fn finish_run(&mut self, id: TabId) {
        if let Some(tab) = self.tabs.read().iter().find(|t| t.id == id) {
            tab.processing.clone().set(false);
            return;
        }
        let parked = self.closing.read().iter().position(|t| t.id == id);
        if let Some(index) = parked {
            self.closing.write().remove(index).release();
        }
    }
}
