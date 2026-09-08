//! The app's only conversion UI: one status box that morphs through the whole
//! run — upload → live activity → done — without swapping screens. The activity
//! timeline is collapsed to its latest step by default and expands in place to
//! the full, scrollable history. The finished box carries the run's outputs and
//! the feedback field that re-runs the agent in the same session.
//!
//! [`AgentFlow`] owns the flow state and picks a [`Screen`]; everything below it
//! is a leaf that renders one band of the box.

use dioxus::html::HasFileData;
use dioxus::prelude::*;

use super::spinner::{Spinner, SpinnerSize};
use crate::files::download_file;
use crate::models::{
    AbortFlag, AgentStep, AgentStepKind, AgentStepStatus, ProcessingState, RetryAction,
    RunStateRead, UploadState,
};
use crate::run_status::{screen_for, RunStatus, Screen};
use crate::tabs::RestoredView;
use crate::workspace::Tab;
use crate::upload::read_upload_files;

/// Render the activity timeline as a Markdown transcript of the run.
fn agent_log_markdown(steps: &[AgentStep]) -> String {
    let mut out = String::from("# Agent Conversion Log\n\n");
    for step in steps {
        match step.kind {
            AgentStepKind::Thought => {
                out.push_str(&format!("> {}\n\n", step.label.replace('\n', "\n> ")));
            }
            AgentStepKind::Tool => {
                let icon = step.status.glyph();
                if step.detail.is_empty() {
                    out.push_str(&format!("- {icon} `{}`\n", step.label));
                } else {
                    out.push_str(&format!("- {icon} `{}` — {}\n", step.label, step.detail));
                }
            }
        }
    }
    out
}

/// Human-friendly duration, e.g. `"1m 18s"` or `"42s"`.
fn format_elapsed(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Map a file name to a short extension badge: `(css_class, label)`.
fn ext_badge(name: &str) -> (&'static str, &'static str) {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".pdf") {
        ("pdf", "PDF")
    } else if lower.ends_with(".zip") {
        ("zip", "ZIP")
    } else {
        ("file", "FILE")
    }
}

#[component]
pub fn AgentFlow(
    /// The conversion this box is showing. Everything it renders and everything
    /// it lets the user change lives on the tab, so switching away and back
    /// finds the box exactly as it was left.
    tab: Tab,
    profiles: Vec<String>,
    /// Whether agent processing is available (an API key is configured).
    ai_available: bool,
    /// AEM upload connection from settings, or `None` if not configured.
    aem_connection: Option<blueprint::AemConnection>,
    /// Start a fresh agent run in this tab from its uploaded files.
    on_ai_process: EventHandler<Vec<(String, Vec<u8>)>>,
    /// Re-run the agent in the same session with the user's feedback.
    on_feedback: EventHandler<String>,
    /// Carry the tab's existing session on with nothing to apply — the agent
    /// finishes the tree the previous run left behind.
    on_continue: EventHandler<()>,
    /// Install the finished package on the configured AEM instance.
    on_aem_upload: EventHandler<()>,
    /// Discard the finished result and return to a clean upload state.
    on_reset: EventHandler<()>,
) -> Element {
    let mut processing_state = tab.state;
    let mut uploaded_files = tab.files;
    let mut feedback = tab.feedback;
    let mut timeline_open = tab.timeline_open;

    let screen = screen_for(&processing_state.read(), (tab.processing)());

    rsx! {
        div { class: "agent-flow",
            div { class: "agent-single",
                div { class: "agent-page",
                    match screen {
                        Screen::Upload => rsx! {
                            UploadBox {
                                profiles,
                                selected_profile: tab.profile,
                                selected_target: tab.target,
                                ai_available,
                                uploaded_files,
                                on_start: move |files: Vec<(String, Vec<u8>)>| on_ai_process.call(files),
                            }
                        },
                        Screen::Run(status) => rsx! {
                            RunBox {
                                status,
                                state: processing_state.into(),
                                total_spend: *tab.total_spend.read(),
                                files: uploaded_files,
                                profile: tab.profile.read().clone(),
                                aem_connection,
                                abort: tab.abort.peek().clone(),
                                aem_upload: tab.aem_upload,
                                restored: *tab.restored.read(),
                                // The same rule the run itself uses, so the box
                                // never offers an action that would find nothing
                                // to replay.
                                can_continue: crate::tabs::is_resumable(&uploaded_files.read()),
                                last_download: tab.last_download,
                                timeline_open,
                                feedback,
                                on_aem_upload: move |()| on_aem_upload.call(()),
                                on_feedback: move |text: String| on_feedback.call(text),
                                on_continue: move |()| on_continue.call(()),
                                // Answer a paused run's retry prompt; the agent loop
                                // polls these on the shared processing state.
                                on_retry: move |_| {
                                    processing_state.write().retry_action = Some(RetryAction::Retry);
                                },
                                on_give_up: move |_| {
                                    processing_state.write().retry_action = Some(RetryAction::Cancel);
                                },
                                on_new: move |_| {
                                    uploaded_files.set(Vec::new());
                                    feedback.set(String::new());
                                    timeline_open.set(false);
                                    on_reset.call(());
                                },
                            }
                        },
                    }
                }
            }
        }
    }
}

/// The box in its initial state: profile, dropzone, selected files, Start.
#[component]
fn UploadBox(
    profiles: Vec<String>,
    mut selected_profile: Signal<Option<String>>,
    selected_target: Signal<blueprint::OutputTarget>,
    ai_available: bool,
    mut uploaded_files: Signal<Vec<(String, Vec<u8>)>>,
    on_start: EventHandler<Vec<(String, Vec<u8>)>>,
) -> Element {
    // A drop target fires enter/leave for every child element it crosses, so the
    // highlight follows a depth counter rather than the last event seen.
    let mut drag_depth = use_signal(|| 0usize);
    let is_dragging = use_memo(move || drag_depth() > 0);

    let files = uploaded_files.read().clone();
    let has_pdf = files
        .iter()
        .any(|(name, _)| name.to_ascii_lowercase().ends_with(".pdf"));
    // An AEM content-package ZIP can be attached as an editable template; a run
    // needs at least a PDF or a template.
    let has_template = files
        .iter()
        .any(|(_, bytes)| blueprint::detect_aem_zip(bytes));
    let start_disabled = files.is_empty() || (!has_pdf && !has_template) || !ai_available;
    let start_title = if !ai_available {
        "Configure an API key in Settings to enable agent processing."
    } else if files.is_empty() {
        "Drop or choose a file to begin."
    } else if !has_pdf && !has_template {
        "Upload at least one PDF, or an AEM content-package ZIP template, to start the agent."
    } else {
        "Let the agent convert and upload the form."
    };

    rsx! {
        section { class: "ag-box",
            div { class: "ag-top",
                div { class: "ag-badge upload", "↑" }
                div { class: "ag-top-text",
                    h2 { class: "ag-title", "Convert a form" }
                    div { class: "ag-meta",
                        span { "Drop the files and the agent takes it from here." }
                    }
                }
            }

            div { class: "ag-phases",
                div { class: "ag-phase active",
                    span { class: "pn", "1" }
                    span { class: "pl", "Upload" }
                }
                div { class: "ag-pbar" }
                div { class: "ag-phase",
                    span { class: "pn", "2" }
                    span { class: "pl", "Convert" }
                }
                div { class: "ag-pbar" }
                div { class: "ag-phase",
                    span { class: "pn", "3" }
                    span { class: "pl", "Finish" }
                }
            }

            if !profiles.is_empty() {
                div { class: "profile-selector",
                    label { r#for: "agent-profile-select", "Profile" }
                    select {
                        id: "agent-profile-select",
                        onchange: move |evt: Event<FormData>| selected_profile.set(Some(evt.value())),
                        for name in profiles.iter() {
                            option {
                                value: "{name}",
                                selected: selected_profile.read().as_deref() == Some(name.as_str()),
                                "{name}"
                            }
                        }
                    }
                }
                super::OutputTargetSelector {
                    profile: selected_profile.read().clone(),
                    selected_target,
                    disabled: false,
                }
            }

            div {
                class: if is_dragging() { "upload-dropzone upload-dropzone-dragging agent-dropzone" } else { "upload-dropzone agent-dropzone" },
                ondragenter: move |evt: Event<DragData>| {
                    evt.prevent_default();
                    drag_depth += 1;
                },
                ondragover: move |evt: Event<DragData>| evt.prevent_default(),
                ondragleave: move |evt: Event<DragData>| {
                    evt.prevent_default();
                    let next = drag_depth().saturating_sub(1);
                    drag_depth.set(next);
                },
                ondrop: move |evt: Event<DragData>| {
                    evt.prevent_default();
                    drag_depth.set(0);
                    let dropped = evt.files();
                    async move {
                        let data = read_upload_files(dropped).await;
                        if !data.is_empty() {
                            uploaded_files.set(data);
                        }
                    }
                },

                div { class: "dz-icon", "↑" }
                h3 { "Drop files to start the agent" }
                p { class: "upload-hint",
                    "Upload the PDF forms here — optionally add an AEM content-package ZIP as an editable template."
                }
                div { class: "upload-actions",
                    label {
                        class: "btn btn-secondary btn-sm",
                        r#for: "agent-file-input",
                        "Choose Files"
                    }
                }
                input {
                    id: "agent-file-input",
                    class: "upload-input-hidden",
                    r#type: "file",
                    multiple: true,
                    accept: ".pdf,.zip",
                    onchange: move |evt: Event<FormData>| {
                        let chosen = evt.files();
                        async move {
                            let data = read_upload_files(chosen).await;
                            if !data.is_empty() {
                                uploaded_files.set(data);
                            }
                        }
                    },
                }
            }

            if !files.is_empty() {
                ul { class: "file-list-compact",
                    for (name , _bytes) in files.iter() {
                        li { "{name}" }
                    }
                }
                div { class: "ag-up-actions",
                    button {
                        class: "btn btn-primary",
                        disabled: start_disabled,
                        title: start_title,
                        onclick: move |_| on_start.call(files.clone()),
                        "Start"
                    }
                }
            }
        }
    }
}

/// The box while the agent runs and once it finishes: header, phase rail,
/// source files, the collapsible activity timeline, and (when done) feedback.
/// A failed request pauses the run instead of ending it, and is surfaced here as
/// a Retry / Give up prompt.
#[component]
fn RunBox(
    status: RunStatus,
    state: RunStateRead,
    /// What every run this tab has made has cost, together — not just this
    /// run's own figure, which `state.spend` already carries.
    total_spend: pipeline::Spend,
    files: Signal<Vec<(String, Vec<u8>)>>,
    profile: Option<String>,
    aem_connection: Option<blueprint::AemConnection>,
    abort: AbortFlag,
    /// Progress of the on-demand AEM install, held by the tab so switching away
    /// mid-upload neither cancels the request nor forgets it was made.
    aem_upload: Signal<UploadState>,
    /// Set when this tab came back from a previous session, so the box can say
    /// what did and did not survive the restart.
    restored: Option<RestoredView>,
    /// Whether a re-run — with feedback or without — has the sources it would
    /// need to replay. Gates both ways of carrying the session on.
    can_continue: bool,
    /// Where this tab last saved each artefact.
    last_download: Signal<std::collections::HashMap<String, std::path::PathBuf>>,
    timeline_open: Signal<bool>,
    feedback: Signal<String>,
    on_feedback: EventHandler<String>,
    /// Resume this tab's session as it stands, with nothing to apply.
    on_continue: EventHandler<()>,
    on_aem_upload: EventHandler<()>,
    /// Resume a paused run by re-sending the request that failed.
    on_retry: EventHandler<()>,
    /// Abandon a paused run instead of retrying it.
    on_give_up: EventHandler<()>,
    on_new: EventHandler<()>,
) -> Element {
    let done = status.is_done();
    let box_class = match status {
        RunStatus::Done => "ag-box done",
        RunStatus::Failed => "ag-box failed",
        _ => "ag-box",
    };

    rsx! {
        section { class: box_class,
            RunHeader {
                status,
                profile,
                elapsed_secs: state.read().elapsed_secs,
                abort,
                on_new,
            }
            PhaseRail { status }
            SourceFiles { files }
            ActivityTimeline { status, state, total_spend, timeline_open }

            // ---- Failed request: retry (or give up) without losing the run ----
            if status == RunStatus::Paused {
                RetryPrompt { error: state.read().error.clone(), on_retry, on_give_up }
            } else if let Some(error) = state.read().error.as_ref() {
                div { class: "progress-error",
                    strong { "Error: " }
                    "{error}"
                }
            } else if state.read().aborted {
                div { class: "progress-note", "Stopped at your request." }
            }

            // Non-fatal problems the run reported — a Redacto dump that could not
            // be built, a cross-language merge that failed. Without this the run
            // looks clean while an output is silently missing.
            if !state.read().warnings.is_empty() {
                div { class: "progress-warnings",
                    strong { "Warnings:" }
                    ul {
                        for warning in state.read().warnings.iter() {
                            li { "{warning}" }
                        }
                    }
                }
            }

            // ---- Result + feedback (done only) ----
            if done {
                if state.read().aem_uploaded && let Some(path) = state.read().aem_form_path.as_ref() {
                    div { class: "ag-aem",
                        span { class: "ag-aem-label", "Uploaded to AEM" }
                        span { class: "ag-aem-path", "{path}" }
                    }
                }
                if let Some(restored) = restored {
                    RestoredNotice { restored, can_continue }
                    if can_continue {
                        ContinueBar { restored, on_continue }
                    }
                }
                ResultActions { state, aem_connection, aem_upload, last_download, on_aem_upload }
                if can_continue {
                    FeedbackBox { feedback, on_feedback }
                }
            }
        }
    }
}

/// Status badge, title, profile/duration meta, and the "New form" escape hatch.
#[component]
fn RunHeader(
    status: RunStatus,
    profile: Option<String>,
    elapsed_secs: Option<u64>,
    abort: AbortFlag,
    on_new: EventHandler<()>,
) -> Element {
    let (badge_class, glyph) = status.badge();

    rsx! {
        div { class: "ag-top",
            div { class: "ag-badge {badge_class}",
                match glyph {
                    Some(g) => rsx! { "{g}" },
                    None => rsx! {
                        Spinner {}
                    },
                }
            }
            div { class: "ag-top-text",
                h2 { class: "ag-title", "{status.title()}" }
                div { class: "ag-meta",
                    if let Some(p) = profile.as_ref() {
                        span {
                            "Profile "
                            b { "{p}" }
                        }
                    }
                    if let Some(secs) = elapsed_secs.filter(|_| status.is_done()) {
                        span {
                            "in "
                            b { "{format_elapsed(secs)}" }
                        }
                    }
                }
            }
            div { class: "ag-actions",
                match status {
                    // A live run can be stopped. The flag is polled at the run's
                    // next checkpoint — including mid-response — so the button
                    // reports that it asked rather than that it finished.
                    RunStatus::Running | RunStatus::Paused => rsx! {
                        AbortButton { abort }
                    },
                    RunStatus::Done | RunStatus::Failed => rsx! {
                        button {
                            class: "btn btn-secondary btn-sm",
                            onclick: move |_| on_new.call(()),
                            "↻ New form"
                        }
                    },
                }
            }
        }
    }
}

/// Upload → Convert → Finish. Upload is always behind us by the time this
/// renders; only the Convert step reflects the run status.
#[component]
fn PhaseRail(status: RunStatus) -> Element {
    let (convert_class, convert_glyph) = match status {
        RunStatus::Running => ("ag-phase active", "●"),
        RunStatus::Paused => ("ag-phase paused", "⏸"),
        RunStatus::Done => ("ag-phase done", "✓"),
        RunStatus::Failed => ("ag-phase failed", "✗"),
    };
    let done = status.is_done();

    rsx! {
        div { class: "ag-phases",
            div { class: "ag-phase done",
                span { class: "pn", "✓" }
                span { class: "pl", "Upload" }
            }
            div { class: "ag-pbar done" }
            div { class: convert_class,
                span { class: "pn", "{convert_glyph}" }
                span { class: "pl", "Convert" }
            }
            div { class: if done { "ag-pbar done" } else { "ag-pbar" } }
            div { class: if done { "ag-phase done" } else { "ag-phase" },
                span { class: "pn", if done { "✓" } else { "3" } }
                span { class: "pl", "Finish" }
            }
        }
    }
}

/// The uploaded source files as extension-badged chips.
#[component]
fn SourceFiles(files: Signal<Vec<(String, Vec<u8>)>>) -> Element {
    if files.read().is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "ag-files",
            for (name , _bytes) in files.read().iter() {
                {
                    let (cls, label) = ext_badge(name);
                    rsx! {
                        span { class: "ag-file",
                            span { class: "ag-file-ext {cls}", "{label}" }
                            "{name}"
                        }
                    }
                }
            }
        }
    }
}

/// What an activity timeline with no steps in it means.
///
/// The two cases look identical in the run state — an empty `agent_steps` — and
/// mean opposite things on screen, which is the whole bug this exists to stop: a
/// tab reopened from a previous session has no transcript because transcripts
/// are never stored, and rendering that as a spinner over "Starting agent…"
/// made every restored tab look like an agent that had started itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmptyActivity {
    /// A live run that has not emitted its first event yet.
    Starting,
    /// A run that is over and left no transcript behind.
    NotRecorded,
}

impl EmptyActivity {
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting agent…",
            // Precisely this and not "nothing was kept": the session's edit
            // history holds a labelled snapshot per change the agent made to the
            // tree, which is what a reopened tab is rebuilt and continued from.
            // What is gone is the timeline itself — the model's prose and the
            // calls it made along the way.
            Self::NotRecorded => "The step-by-step activity is not kept between sessions.",
        }
    }

    /// Whether to spin. Only something still working may.
    fn is_working(self) -> bool {
        self == Self::Starting
    }
}

/// Which of the two an empty timeline is. Pure, so the distinction is pinned by
/// a test rather than by reading the render.
fn empty_activity(status: RunStatus) -> EmptyActivity {
    if status.is_live() {
        EmptyActivity::Starting
    } else {
        EmptyActivity::NotRecorded
    }
}

/// The run's activity: collapsed to the latest step, or expanded to the full
/// scrollable history with the context-window indicator.
#[component]
fn ActivityTimeline(
    /// Whether this run is still going, which is what tells an empty timeline
    /// apart from one whose transcript was never stored.
    status: RunStatus,
    state: RunStateRead,
    /// What every run this tab has made has cost, together.
    total_spend: pipeline::Spend,
    mut timeline_open: Signal<bool>,
) -> Element {
    // The scroll anchor has to name *this* timeline. Once several runs are open
    // at once a shared id would let one timeline scroll another's box, so the id
    // is minted per component instance rather than written as a constant.
    let anchor = use_hook(|| {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "agent-flow-end-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    });

    // Keep the timeline pinned to the newest step as the agent works. The memo
    // makes the dependency explicit — the effect must re-run on a new step, not
    // on every unrelated change to the run state.
    let step_count = use_memo(move || state.read().agent_steps.len());
    use_effect({
        let anchor = anchor.clone();
        move || {
            let _ = step_count();
            if timeline_open() {
                document::eval(&format!(
                    r#"setTimeout(() => {{
                    const el = document.getElementById('{anchor}');
                    if (el) el.scrollIntoView({{ block: 'end' }});
                }}, 0);"#
                ));
            }
        }
    });

    let open = timeline_open();
    let state = state.read();
    let steps = &state.agent_steps;

    rsx! {
        div { class: "ag-tl",
            button {
                class: "ag-tl-bar",
                onclick: move |_| timeline_open.toggle(),
                if open {
                    span { class: "ag-tl-title", "Activity · {steps.len()} steps" }
                    ContextGauge {
                        used: state.context_used_tokens,
                        window: state.context_window,
                    }
                    SpendTag { spend: state.spend, total: total_spend }
                } else {
                    // Collapsed: show only the latest step.
                    match steps.last() {
                        Some(s) if s.kind == AgentStepKind::Tool => rsx! {
                            span { class: "ag-tl-dot {s.status.dot_class()}", {status_glyph(s.status)} }
                            span { class: "ag-tl-latest",
                                span { class: "nm", "{s.label}" }
                                if !s.detail.is_empty() {
                                    span { class: "dt", "{s.detail}" }
                                }
                            }
                        },
                        Some(s) => rsx! {
                            span { class: "ag-tl-dot" }
                            span { class: "ag-tl-latest",
                                span { class: "nm-thought", "{s.label}" }
                            }
                        },
                        None => {
                            let empty = empty_activity(status);
                            rsx! {
                                span { class: "ag-tl-dot",
                                    if empty.is_working() {
                                        Spinner { size: SpinnerSize::Sm }
                                    }
                                }
                                span { class: "ag-tl-latest",
                                    span { class: "nm", "{empty.label()}" }
                                }
                            }
                        }
                    }
                }
                span { class: "ag-tl-chevron",
                    if open {
                        "Collapse"
                    } else {
                        "Show full history"
                    }
                    span { class: if open { "chev chev-open" } else { "chev" }, "▾" }
                }
            }
            if open {
                div { class: "ag-tl-full",
                    div { class: "af-timeline",
                        if steps.is_empty() {
                            div { class: "af-thought", "{empty_activity(status).label()}" }
                        }
                        for (i , s) in steps.iter().enumerate() {
                            {
                                match s.kind {
                                    AgentStepKind::Thought => rsx! {
                                        div { key: "{i}", class: "af-thought", "{s.label}" }
                                    },
                                    AgentStepKind::Tool => rsx! {
                                        div { key: "{i}", class: "af-tool",
                                            span { class: "af-node", {status_glyph(s.status)} }
                                            div { class: "af-tool-body",
                                                span { class: "af-tool-name", "{s.label}" }
                                                if !s.detail.is_empty() {
                                                    span { class: "af-tool-detail", "{s.detail}" }
                                                }
                                            }
                                        }
                                    },
                                }
                            }
                        }
                        div { id: "{anchor}" }
                    }
                }
            }
        }
    }
}

/// The glyph (or spinner) that stands for a step's status.
fn status_glyph(status: AgentStepStatus) -> Element {
    match status {
        AgentStepStatus::Running => rsx! {
            Spinner { size: SpinnerSize::Sm }
        },
        AgentStepStatus::Done => rsx! {
            span { class: "af-ok", "{status.glyph()}" }
        },
        AgentStepStatus::Error => rsx! {
            span { class: "af-err", "{status.glyph()}" }
        },
    }
}

/// What the run has cost so far, beside the context gauge.
///
/// Absent until the first turn reports usage. A model with no published rate
/// shows its token counts and says the cost is unknown, rather than showing a
/// figure that is really a zero.
///
/// `total` is what every run this *form* has made — this one plus any earlier
/// feedback round — has cost together. It is only shown once it says
/// something `spend` alone does not: a form's first (and so far only) run has
/// nothing else to add, and right after any run finishes the two briefly
/// agree again (the total has just absorbed exactly what that run billed).
#[component]
fn SpendTag(spend: Option<pipeline::Spend>, total: pipeline::Spend) -> Element {
    let Some(spend) = spend else {
        return rsx! {};
    };
    let label = match spend.cost_usd {
        Some(cost) => format!("USD {cost:.2}"),
        None => "cost n/a".to_string(),
    };
    let show_total = total != pipeline::Spend::default() && total != spend;
    let total_label = match total.cost_usd {
        Some(cost) => format!("USD {cost:.2} total"),
        None => "cost n/a total".to_string(),
    };
    rsx! {
        span { class: "ag-spend", title: "{spend.describe()}", "{label}" }
        if show_total {
            span { class: "ag-spend-total", title: "{total.describe()}", "{total_label}" }
        }
    }
}

/// How much of the model's context window the run has filled. Renders nothing
/// until the agent reports a window.
#[component]
fn ContextGauge(used: usize, window: usize) -> Element {
    if window == 0 {
        return rsx! {};
    }

    let used = used.min(window);
    let pct = (used as f32 / window as f32 * 100.0).round() as u32;
    let fill = if pct >= 90 {
        "var(--danger)"
    } else if pct >= 75 {
        "var(--warn)"
    } else {
        "var(--accent)"
    };
    let ring = format!(
        "background: conic-gradient({fill} {}deg, var(--border) 0);",
        pct * 36 / 10
    );

    rsx! {
        span {
            class: "ag-ctx",
            title: "Context window · {used} / {window} tokens ({pct}%)",
            span { class: "ag-ctx-ring", style: "{ring}" }
            span { class: "ag-ctx-pct", "{pct}%" }
        }
    }
}

/// Says what a reopened tab actually carries.
///
/// A restored tab looks like a finished one, but it is not: the built package,
/// the schema and the activity log were never stored — they are regenerated
/// from the session, or from a re-run. Saying so is the difference between an
/// empty download row that reads as a bug and one that reads as expected.
#[component]
fn RestoredNotice(restored: RestoredView, can_continue: bool) -> Element {
    let headline = match restored {
        RestoredView::Interrupted => "This run was interrupted when the app closed.",
        _ => "Reopened from the saved session.",
    };
    let detail = match (restored, can_continue) {
        (RestoredView::Interrupted, true) => {
            "The agent kept a snapshot of everything it had built. It is not running: nothing \
             carries on until you ask it to."
        }
        (_, true) => {
            "The downloads are not kept between sessions, so they have to be produced again."
        }
        // Resuming replays the original PDFs through the agent, and those are
        // the one thing that cannot be reconstructed. Worded for both ways of
        // ending up here: the stored sources were dropped, or the conversion ran
        // from a template and never had a PDF beside it.
        (_, false) => {
            "There are no source documents to replay, so this conversion cannot be continued. \
             Start over with the sources to re-run it."
        }
    };

    rsx! {
        div { class: "ag-restored",
            span { class: "ag-restored-title", "{headline}" }
            span { class: "ag-restored-detail", "{detail}" }
        }
    }
}

/// The one action a reopened session offers: start the agent on it again.
///
/// Its own band rather than a line in the feedback box, because it answers a
/// different question. Feedback is "change this"; Continue is "pick this up" —
/// and until it is pressed nothing is running, which is the whole point of
/// reopening a session rather than resuming it automatically.
///
/// Mounted only while the tab is still the restored one: starting a run clears
/// the notice, after which the result on screen is this session's own and
/// feedback is the way to refine it.
#[component]
fn ContinueBar(restored: RestoredView, on_continue: EventHandler<()>) -> Element {
    let label = match restored {
        RestoredView::Interrupted => "▶ Continue where it stopped",
        _ => "▶ Continue this conversion",
    };
    let hint = match restored {
        RestoredView::Interrupted => {
            "The agent picks the snapshot up, finishes what is missing and reviews it."
        }
        // Worth saying plainly: this is a full run, not a re-export. It costs a
        // run's worth of tokens and the agent may change the form on the way.
        _ => "A full run: the agent reviews the form, may change it, and rebuilds the outputs.",
    };

    rsx! {
        div { class: "ag-continue",
            button {
                class: "btn btn-primary",
                onclick: move |_| on_continue.call(()),
                "{label}"
            }
            span { class: "ag-continue-hint", "{hint}" }
        }
    }
}

/// A failed request paused the run: offer to re-send it, or to give up.
#[component]
fn RetryPrompt(
    error: Option<String>,
    on_retry: EventHandler<()>,
    on_give_up: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "ag-retry",
            div { class: "ag-retry-title", "The request to Claude failed — the run is paused." }
            if let Some(error) = error.as_ref() {
                div { class: "ag-retry-msg", "{error}" }
            }
            div { class: "ag-retry-hint",
                "Everything the agent has built so far is still in memory. Retry re-sends only \
                 the step that failed."
            }
            div { class: "ag-retry-actions",
                button {
                    class: "btn btn-primary",
                    onclick: move |_| on_retry.call(()),
                    "↻ Retry"
                }
                button {
                    class: "btn btn-secondary",
                    onclick: move |_| on_give_up.call(()),
                    "Give up"
                }
            }
        }
    }
}

/// Stop a live run.
///
/// Its own component so the "asked already" state lives exactly as long as the
/// run does: the button is mounted only while the run is live, so a finished run
/// drops it and the next run starts with a fresh button. Keeping the state in
/// the header instead would carry a stale "Stopping…" into a feedback re-run.
#[component]
fn AbortButton(abort: AbortFlag) -> Element {
    // Read off the flag itself rather than a local signal: this component
    // belongs to whichever tab is on screen, so a local "already asked" would
    // follow the user to a tab whose run nobody stopped.
    let asked = abort.is_aborted();

    rsx! {
        button {
            class: "btn btn-secondary btn-sm",
            disabled: asked,
            title: "Stop this conversion. Whatever the agent has built so far is kept.",
            onclick: move |_| abort.abort(),
            if asked { "Stopping…" } else { "✕ Abort" }
        }
    }
}

/// Everything the finished run offers: one button per artefact it produced,
/// then the AEM install as the row's single emphasised action.
#[component]
fn ResultActions(
    state: RunStateRead,
    aem_connection: Option<blueprint::AemConnection>,
    aem_upload: Signal<UploadState>,
    last_download: Signal<std::collections::HashMap<String, std::path::PathBuf>>,
    on_aem_upload: EventHandler<()>,
) -> Element {
    let upload_state = aem_upload;
    let run = state.read();

    rsx! {
        div { class: "ag-result-actions",
            for artifact in Artifact::ALL.iter().copied() {
                if artifact.is_offered(&run) {
                    DownloadButton {
                        key: "{artifact:?}",
                        class: "btn btn-secondary",
                        artifact,
                        state,
                        last_download,
                    }
                }
            }
            if run.aem_package.as_ref().filter(|_| Artifact::Package.is_offered(&run)).is_some() {
                {
                    let st = upload_state.read().clone();
                    let uploading = st == UploadState::Uploading;
                    let no_connection = aem_connection.is_none();
                    let upload_title = match &st {
                        UploadState::Error(msg) => msg.clone(),
                        _ if no_connection => {
                            "Configure the AEM connection in Settings to enable this".to_string()
                        }
                        _ => "Upload and install the package on the configured AEM instance".to_string(),
                    };

                    rsx! {
                        button {
                            class: "btn btn-primary",
                            disabled: uploading || no_connection,
                            title: upload_title,
                            // The request is started by the app, not here: this
                            // scope unmounts the moment the user switches tab,
                            // and Dioxus cancels a scope's tasks with it — which
                            // would abandon an install already in flight.
                            onclick: move |_| on_aem_upload.call(()),
                            match st {
                                UploadState::Uploading => rsx! {
                                    Spinner { size: SpinnerSize::Sm }
                                    span { "Uploading…" }
                                },
                                UploadState::Success => rsx! { "✓ Uploaded to AEM" },
                                UploadState::Error(_) => rsx! { "⚠ Upload failed — retry" },
                                UploadState::Idle => rsx! { "⬆ Upload to AEM" },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One artefact a finished run offers for download.
///
/// A variant, not the payload: `ProcessingState` holds the multi-MB AEM package,
/// so a component that took the bytes as a prop would deep-copy them on every
/// render and then byte-compare them again for prop memoisation. The bytes are
/// materialised only when the button is actually pressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Artifact {
    Package,
    PackageBound,
    RedactoSql,
    Xsd,
    AgentLog,
}

impl Artifact {
    /// Every artefact, in the order the result row offers them. Which of these
    /// actually appear is [`Artifact::is_offered`]'s call.
    const ALL: &'static [Self] = &[
        Self::Package,
        Self::PackageBound,
        Self::RedactoSql,
        Self::Xsd,
        Self::AgentLog,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Package => "⬇ Download CRX package",
            Self::PackageBound => "⬇ Download CRX package with bindRefs",
            Self::RedactoSql => "Redacto SQL",
            Self::Xsd => "XSD schema",
            Self::AgentLog => "Agent log",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Package => {
                "Download the AEM content package (CRX) as a ZIP, without schema bindings"
            }
            Self::PackageBound => {
                "The same package with a bindRef on every field and the matching XSD bundled"
            }
            Self::RedactoSql => {
                "The Redacto PostgreSQL dump (document, components and text assets)"
            }
            Self::Xsd => "The XML Schema Definition for the converted form",
            Self::AgentLog => "The agent's full activity timeline as a Markdown transcript",
        }
    }

    /// `(filename prefix, extension)`, paired here so a payload cannot end up
    /// under another artefact's name.
    fn naming(self) -> (&'static str, &'static str) {
        match self {
            Self::Package => runner::Artifact::Package.naming(),
            Self::PackageBound => runner::Artifact::PackageBound.naming(),
            Self::RedactoSql => runner::Artifact::RedactoSql.naming(),
            Self::Xsd => runner::Artifact::Xsd.naming(),
            // The app's own by-product, not one of the run's artefacts.
            Self::AgentLog => ("agent-log", "md"),
        }
    }

    /// Whether this artefact belongs to `target`'s result panel.
    ///
    /// Presence alone is not the rule: an artefact that only makes sense for the
    /// other target must stay hidden even if the run happens to have produced it.
    /// The log belongs to every run.
    fn belongs_to(self, target: blueprint::OutputTarget) -> bool {
        match self {
            Self::Package | Self::PackageBound | Self::Xsd => {
                target == blueprint::OutputTarget::Aem
            }
            Self::RedactoSql => target == blueprint::OutputTarget::Redacto,
            Self::AgentLog => true,
        }
    }

    /// This artefact's bytes, or `None` if the run did not produce it.
    fn bytes(self, state: &ProcessingState) -> Option<Vec<u8>> {
        match self {
            Self::Package => state.aem_package.clone(),
            Self::PackageBound => state.aem_package_bound.clone(),
            Self::RedactoSql => state.redacto_sql.as_ref().map(|s| s.clone().into_bytes()),
            Self::Xsd => state.xsd_schema.as_ref().map(|s| s.clone().into_bytes()),
            Self::AgentLog => (!state.agent_steps.is_empty())
                .then(|| agent_log_markdown(&state.agent_steps).into_bytes()),
        }
    }

    /// Whether the run produced it *and* it belongs to that run's target.
    fn is_offered(self, state: &ProcessingState) -> bool {
        if !self.belongs_to(state.target) {
            return false;
        }
        match self {
            Self::Package => state.aem_package.is_some(),
            Self::PackageBound => state.aem_package_bound.is_some(),
            Self::RedactoSql => state.redacto_sql.is_some(),
            Self::Xsd => state.xsd_schema.is_some(),
            Self::AgentLog => !state.agent_steps.is_empty(),
        }
    }
}

/// A button that saves one artefact to the user's Downloads folder.
#[component]
fn DownloadButton(
    class: &'static str,
    artifact: Artifact,
    state: RunStateRead,
    /// Where this tab last saved each artefact, so a second download replaces
    /// its own file rather than landing beside it.
    mut last_download: Signal<std::collections::HashMap<String, std::path::PathBuf>>,
) -> Element {
    // A failed save has to be visible: the user pressed a button and would
    // otherwise be left looking for a file that was never written.
    let mut error = use_signal(|| None::<String>);

    rsx! {
        button {
            class,
            title: artifact.title(),
            onclick: move |_| {
                let state = state.read();
                let (prefix, ext) = artifact.naming();
                let name = runner::artifact_filename(prefix, state.form_code.as_deref(), ext);
                let previous = last_download.peek().get(&name).cloned();
                error
                    .set(
                        match artifact.bytes(&state) {
                            Some(bytes) => {
                                match download_file(&bytes, &name, previous.as_deref()) {
                                    Ok(path) => {
                                        last_download.write().insert(name, path);
                                        None
                                    }
                                    Err(e) => Some(e),
                                }
                            }
                            None => Some("That output is no longer available.".to_string()),
                        },
                    );
            },
            "{artifact.label()}"
        }
        if let Some(error) = error.read().as_ref() {
            span { class: "ag-save-error", "{error}" }
        }
    }
}

/// Tell the agent what to change; it re-runs in the same session.
#[component]
fn FeedbackBox(mut feedback: Signal<String>, on_feedback: EventHandler<String>) -> Element {
    rsx! {
        div { class: "ag-fb",
            div { class: "ag-fb-label",
                "Not quite right? Tell the agent what to change — it re-runs in the same session."
            }
            textarea {
                class: "af-feedback-input",
                rows: "3",
                placeholder: "e.g. The phone number field should be optional.",
                value: "{feedback}",
                oninput: move |evt| feedback.set(evt.value()),
            }
            div { class: "ag-fb-row",
                button {
                    class: "btn btn-primary",
                    disabled: feedback.read().trim().is_empty(),
                    onclick: move |_| {
                        let text = feedback.read().trim().to_string();
                        if !text.is_empty() {
                            feedback.set(String::new());
                            on_feedback.call(text);
                        }
                    },
                    "Send feedback"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ProcessingStep;

    fn step(kind: AgentStepKind, label: &str, detail: &str, status: AgentStepStatus) -> AgentStep {
        AgentStep {
            id: String::new(),
            kind,
            label: label.to_string(),
            detail: detail.to_string(),
            status,
        }
    }

    /// The bug this is here to stop: a tab reopened from a previous session has
    /// no transcript, because transcripts are never stored — and the timeline
    /// read that empty list as "the run has not reported anything yet" and put a
    /// spinner over "Starting agent…". Every restored tab therefore claimed the
    /// agent had started itself on it.
    ///
    /// A restored tab is `Done` (see `tabs::restored_run`), so the assertion
    /// that matters is that a run which is over never says it is starting and
    /// never spins.
    #[test]
    fn a_run_that_is_over_does_not_claim_to_be_starting() {
        for status in [RunStatus::Done, RunStatus::Failed] {
            let empty = empty_activity(status);
            assert_eq!(
                empty,
                EmptyActivity::NotRecorded,
                "{status:?} reported nothing and never will"
            );
            assert!(
                !empty.is_working(),
                "{status:?} must not spin — that is what made a reopened tab look live"
            );
            assert!(
                !empty.label().contains("Starting"),
                "{status:?} said {:?}",
                empty.label()
            );
        }

        // A live run genuinely has not got there yet, so the spinner is right.
        for status in [RunStatus::Running, RunStatus::Paused] {
            let empty = empty_activity(status);
            assert_eq!(empty, EmptyActivity::Starting);
            assert!(empty.is_working());
        }
    }

    #[test]
    fn filename_falls_back_when_the_form_code_is_unknown() {
        assert_eq!(
            runner::artifact_filename("forms-package", Some("AAEV"), "zip"),
            "forms-package-AAEV.zip"
        );
        assert_eq!(
            runner::artifact_filename("redacto", None, "sql"),
            "redacto.sql"
        );
    }



    /// The log is the only durable record of a run once the window is closed, so
    /// every step kind has to survive the transcript.
    #[test]
    fn agent_log_renders_thoughts_and_tool_calls() {
        let md = agent_log_markdown(&[
            step(
                AgentStepKind::Thought,
                "Analysing",
                "",
                AgentStepStatus::Done,
            ),
            step(
                AgentStepKind::Tool,
                "build_aem_package",
                "12 components",
                AgentStepStatus::Done,
            ),
            step(
                AgentStepKind::Tool,
                "upload_to_aem",
                "",
                AgentStepStatus::Error,
            ),
        ]);

        assert!(md.starts_with("# Agent Conversion Log\n\n"), "{md}");
        assert!(md.contains("> Analysing\n"), "{md}");
        assert!(
            md.contains("- ✓ `build_aem_package` — 12 components\n"),
            "{md}"
        );
        // A detail-less tool call must not leave a dangling em dash.
        assert!(md.contains("- ✗ `upload_to_aem`\n"), "{md}");
    }

    /// A multi-line thought has to stay inside the blockquote, otherwise the
    /// continuation lines render as body text.
    #[test]
    fn agent_log_keeps_multiline_thoughts_quoted() {
        let md = agent_log_markdown(&[step(
            AgentStepKind::Thought,
            "First line\nSecond line",
            "",
            AgentStepStatus::Done,
        )]);

        assert!(md.contains("> First line\n> Second line\n"), "{md}");
    }

    /// A state holding every artefact, so the target rule is what decides which
    /// ones the panel offers.
    fn state_with_everything(target: blueprint::OutputTarget) -> ProcessingState {
        ProcessingState {
            step: ProcessingStep::Complete,
            target,
            aem_package: Some(vec![1, 2, 3]),
            xsd_schema: Some("<xsd/>".into()),
            redacto_sql: Some("INSERT ...".into()),
            agent_steps: vec![step(
                AgentStepKind::Tool,
                "build_aem_package",
                "",
                AgentStepStatus::Done,
            )],
            ..Default::default()
        }
    }

    /// Presence is not the rule. An AEM run must not offer the Redacto dump even
    /// when one exists, and vice versa — otherwise the panel advertises an
    /// artefact from the target the user did not pick.
    #[test]
    fn each_target_offers_only_its_own_artifacts() {
        let aem = state_with_everything(blueprint::OutputTarget::Aem);
        assert!(Artifact::Package.is_offered(&aem));
        assert!(Artifact::Xsd.is_offered(&aem));
        assert!(!Artifact::RedactoSql.is_offered(&aem));

        let redacto = state_with_everything(blueprint::OutputTarget::Redacto);
        assert!(Artifact::RedactoSql.is_offered(&redacto));
        assert!(!Artifact::Package.is_offered(&redacto));
        assert!(!Artifact::Xsd.is_offered(&redacto));
    }

    /// The log is the record of the run itself, so it survives either target.
    #[test]
    fn the_agent_log_is_offered_for_every_target() {
        for target in blueprint::OutputTarget::ALL {
            assert!(
                Artifact::AgentLog.is_offered(&state_with_everything(target)),
                "{target:?}"
            );
        }
    }

    /// A target that produced nothing offers nothing: the rule gates on top of
    /// presence, it does not replace it.
    #[test]
    fn an_artifact_the_run_never_produced_is_not_offered() {
        let empty = ProcessingState {
            step: ProcessingStep::Complete,
            target: blueprint::OutputTarget::Aem,
            ..Default::default()
        };

        assert!(!Artifact::Package.is_offered(&empty));
        assert!(!Artifact::Xsd.is_offered(&empty));
        assert!(!Artifact::AgentLog.is_offered(&empty));
    }
}
