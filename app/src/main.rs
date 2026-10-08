mod agent_runner;
mod components;
mod files;
mod mcp_install;
mod models;
mod run_status;
mod tabs;
mod upload;
mod workspace;

// The headless engine layer (edit-history store, reference store) lives in
// the `agent` crate; the LLM transport and the operator settings live in
// `runner`, shared with the CLI. Re-export both under the historical
// `crate::*` paths so the rest of the app is unchanged.
pub use agent::{db, references, session};
pub use runner::settings;

use dioxus::prelude::*;

use components::{AgentFlow, FormTabs, ReferencesPage, ReviewPage, SettingsPage};
use models::{ProcessingState, ProcessingStep};
use settings::AppSettings;
use tabs::{restored_view, RestoredView, SavedTab, SavedWorkspace, WORKSPACE_KEY};
use workspace::{Tab, Workspace};

/// Work out what is still there for a saved tab: the sources it was converted
/// from, and how much of its result survived.
///
/// A session that recorded nothing but its empty seed is not worth reopening —
/// the run died during analysis — so it is deleted rather than left to litter
/// the history with a shell nobody can load.
fn reopen_tab(saved: &SavedTab) -> (Vec<(String, Vec<u8>)>, RestoredView) {
    let files = saved
        .doc_hash
        .as_deref()
        .map(db::load_sources)
        .unwrap_or_default();

    // A run records its document after every edit under `#document`. A session
    // recorded before that holds its structured envelope, or its AEM tree under
    // `#aem`; it cannot be resumed, but it is kept. `> 0` because sequence zero
    // was the empty seed those runs wrote before they did anything.
    let has_snapshot = saved.session_id.as_deref().is_some_and(|session| {
        db::latest_seq(&agent::session::document_session(session)).is_some()
            || db::latest_seq(session).is_some_and(|seq| seq > 0)
            || db::latest_seq(&format!("{session}#aem")).is_some()
    });

    let view = restored_view(saved, has_snapshot);
    if view == RestoredView::Orphaned
        && let Some(session) = saved.session_id.as_deref()
    {
        db::delete_session(session);
    }
    (files, view)
}

/// How many recent sessions keep their source documents on disk.
///
/// A reopened session needs its sources to be continued at all, but keeping
/// every document ever converted would grow the store without bound.
const RETAINED_SESSIONS: usize = 50;

fn main() {
    // This executable is also the rule worker; see `agent::rules::runner`.
    agent::rules::serve_worker_if_invoked();
    let saved = AppSettings::load();
    let mut config = dioxus::desktop::Config::new().with_window(
        dioxus::desktop::WindowBuilder::new()
            .with_always_on_top(saved.always_on_top)
            // The agent box fills the window, so the size only has to fit the
            // content itself — not a centred column plus a backdrop.
            .with_inner_size(dioxus::desktop::LogicalSize::new(880.0, 720.0))
            .with_title("Ajila Forms Conversion Engine"),
    );

    // Window/taskbar icon (no-op on macOS, used on Windows/Linux).
    if let Some(icon) = load_window_icon() {
        config = config.with_icon(icon);
    }

    dioxus::LaunchBuilder::new().with_cfg(config).launch(App);
}

/// Decode the bundled PNG into a `tao` window icon.
fn load_window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    let rgba = image::load_from_memory(include_bytes!("../icons/icon.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    dioxus::desktop::tao::window::Icon::from_rgba(rgba.into_raw(), width, height).ok()
}

#[component]
fn App() -> Element {
    // The profile list is baked into the binary, so read it once and start on
    // the first entry rather than re-deriving the default during every render.
    let profiles = use_hook(agent::profiles::list_profiles);
    let mut app_settings = use_signal(AppSettings::load);
    let mut settings_open = use_signal(|| false);
    // Whether the full-page reference-forms manager is open.
    let mut references_open = use_signal(|| false);
    // A hook, so it has to be called here rather than inside the settings
    // handler that uses it.
    let window = dioxus::desktop::use_window();

    // Every conversion the user has open. Each tab owns its own run state,
    // profile, target and stop control, so tabs convert independently.
    //
    // Reopened from the last session, sources and all, so a restart picks up
    // where the operator left off rather than discarding a batch of work.
    let saved = use_hook(|| SavedWorkspace::parse(db::get_setting(WORKSPACE_KEY).as_deref()));
    let mut workspace = Workspace::use_init(
        &saved,
        profiles.first().map(String::as_str),
        agent::OutputTarget::default(),
        reopen_tab,
    );

    // Written on the moments that matter — a tab opened, closed or switched, a
    // run started or finished, files attached — rather than on every keystroke.
    // The phases carry all the durable information; a half-typed feedback note
    // rides along with whichever of those happens next.
    let save_workspace = move || {
        let snapshot = workspace.snapshot(|files| {
            (!files.is_empty()).then(|| db::document_hash(files))
        });
        match serde_json::to_string(&snapshot) {
            Ok(json) => db::set_setting(WORKSPACE_KEY, &json),
            Err(e) => eprintln!("workspace: could not be recorded: {e}"),
        }
    };

    // A quit is the one moment nothing else covers: profile, target and file
    // choices made but not yet started have to survive it too.
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        if matches!(
            event,
            dioxus::desktop::tao::event::Event::WindowEvent {
                event: dioxus::desktop::tao::event::WindowEvent::CloseRequested,
                ..
            }
        ) {
            save_workspace();
        }
    });

    // Drop the stored bytes of documents no open tab and no recent session
    // refers to any more. Once, at startup, so a long-lived install does not
    // accumulate every PDF it has ever seen.
    use_hook(move || {
        let mut keep: Vec<String> = workspace
            .tabs()
            .iter()
            .filter_map(|tab| {
                let files = tab.files.read();
                (!files.is_empty()).then(|| db::document_hash(&files))
            })
            .collect();
        keep.extend(db::recent_doc_hashes(RETAINED_SESSIONS));
        if db::prune_sources(&keep) > 0 {
            db::vacuum();
        }
    });

    // Both entry points below start a run the same way: capture the tab's
    // choices, flip it into its running phase, and let the agent drive.
    let run_config = move |tab: Tab| agent_runner::RunConfig {
        profile: tab.profile.read().clone(),
        target: *tab.target.read(),
        settings: app_settings.read().clone(),
        abort: tab.abort.peek().clone(),
    };
    let begin_run = move |tab: Tab| {
        // A previous run in this tab may have left it set.
        tab.abort.peek().reset();
        // Whatever the box was explaining about the last session no longer
        // describes what is on screen.
        tab.restored.clone().set(None);
        tab.processing.clone().set(true);
        tab.state.clone().set(ProcessingState {
            step: ProcessingStep::Running,
            ..ProcessingState::default()
        });
    };

    // ── AI processing ───────────────────────────────────────────────────────
    // Hand the whole conversion to the autonomous agent: it drives the engine
    // via tools (extract → structure → convert → AEM → package → upload/verify),
    // versioning each step, and finalizes the result. The full file set is passed
    // so an attached content-package ZIP can be pre-loaded as the agent's
    // editable working tree.
    let on_ai_process = move |tab: Tab, file_data: Vec<(String, Vec<u8>)>| {
        let has_pdf = file_data
            .iter()
            .any(|(name, _)| agent::conversion::is_source_pdf(name));
        // An AEM content-package ZIP may be attached as an editable template for
        // the agent's working tree. Proceed with PDFs, a template, or both.
        let has_template = agent::conversion::template_of(&file_data).is_some();
        if !has_pdf && !has_template {
            return;
        }

        tab.session_id.clone().set(None);

        let config = run_config(tab);
        begin_run(tab);
        save_workspace();

        // Two layers on purpose. The run itself goes to a worker thread, so a
        // package build or a PDF extraction in one tab cannot freeze the other
        // tabs and the window along with them — it reaches the UI only through
        // the run state, which is the one handle built to cross threads. The
        // bookkeeping around it stays on the UI thread, where the rest of the
        // tab's signals live.
        spawn(async move {
            let session_label = file_data
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
                .join(", ");

            let run = tokio::spawn(async move {
                agent_runner::run_agent(file_data, config, session_label, tab.state).await
            });

            if let Some(session) = run.await.ok().flatten() {
                tab.session_id.clone().set(Some(session));
            }
            // Fold what this run cost into the tab's running total before the
            // next run's `begin_run` clears `state`'s own copy.
            if let Some(spend) = tab.state.read().spend {
                tab.total_spend.clone().write().merge(&spend);
            }
            workspace.finish_run(tab.id);
            save_workspace();
        });
    };

    // ── Carrying an existing session on ───────────────────────────────────────
    // Two ways in, one path: from the "done" screen the user can submit feedback
    // for the agent to apply, and a tab reopened from a previous session can be
    // continued with nothing to apply at all — the agent finishes the tree the
    // last run left. Everything around them is identical, so the seed carries
    // the difference rather than a second copy of the bookkeeping.
    //
    // Both return the tab to its in-progress phase, and neither runs unless the
    // user asked: a restored tab sits on its result until this is called.
    let resume_run = move |tab: Tab, seed: pipeline::RunSeed| {
        // Both guards hold whenever the box offered the action: it renders
        // neither Continue nor the feedback field unless `resumable` says the
        // tab has what a re-run needs, and a tab with a snapshot to resume
        // always carries the session it was recorded under.
        let (Some(session), Some(pdfs)) = (
            tab.session_id.read().clone(),
            tabs::resumable(&tab.files.read()),
        ) else {
            return;
        };

        let config = run_config(tab);
        begin_run(tab);
        save_workspace();

        spawn(async move {
            let run = tokio::spawn(async move {
                agent_runner::run_agent_resume(seed, pdfs, config, session, tab.state).await
            });

            if let Some(session) = run.await.ok().flatten() {
                tab.session_id.clone().set(Some(session));
            }
            // Fold what this round cost into the tab's running total before
            // the next run's `begin_run` clears `state`'s own copy.
            if let Some(spend) = tab.state.read().spend {
                tab.total_spend.clone().write().merge(&spend);
            }
            workspace.finish_run(tab.id);
            save_workspace();
        });
    };

    let active = workspace.active_tab();
    let running = workspace.running_count();

    // ── Render ────────────────────────────────────────────────────────────────
    rsx! {
        document::Stylesheet { href: asset!("/assets/styles.css") }

        // App Header
        header { class: "app-header",
            img {
                class: "app-header-logo",
                src: asset!("/assets/company-logo.webp"),
                alt: "Ajila Company Logo",
            }
            div { class: "app-header-right",
                h1 { class: "app-header-title", "Forms Conversion Engine" }
                span { class: "app-header-version", "v{env!(\"CARGO_PKG_VERSION\")}" }
            }
            // Background runs are invisible from the full-page views, so the
            // count doubles as the way back to them.
            if running > 0 {
                button {
                    class: "app-header-running",
                    title: "Back to the conversions",
                    onclick: move |_| {
                        settings_open.set(false);
                        references_open.set(false);
                    },
                    "▶ {running} running"
                }
            }
            button {
                class: "settings-btn",
                title: "Settings",
                onclick: move |_| settings_open.set(true),
                "⚙"
            }
        }

        // Settings, the references manager, or the workspace — full-page views
        // under the persistent header.
        if *settings_open.read() {
            SettingsPage {
                on_close: move |_| settings_open.set(false),
                settings: app_settings,
                on_settings_changed: move |new_settings: AppSettings| {
                    new_settings.save();
                    window.set_always_on_top(new_settings.always_on_top);
                    app_settings.set(new_settings);
                },
                on_open_references: move |_| {
                    settings_open.set(false);
                    references_open.set(true);
                },
            }
        } else if *references_open.read() {
            // Reference-forms manager (full page view)
            ReferencesPage {
                profile: active.profile.read().clone(),
                settings: app_settings,
                on_close: move |_| references_open.set(false),
            }
        } else if *active.review_open.read() {
            // The active conversion's review (full page view)
            ReviewPage {
                tab: active,
                on_close: move |_| active.review_open.clone().set(false),
            }
        } else {
            // The open conversions, and the active one's flow:
            // upload → live timeline → done.
            FormTabs { workspace, on_changed: move |()| save_workspace() }
            AgentFlow {
                tab: active,
                profiles,
                ai_available: !app_settings.read().active_api_key().is_empty(),
                on_ai_process: move |files: Vec<(String, Vec<u8>)>| {
                    on_ai_process(active, files);
                },
                on_feedback: move |text: String| {
                    resume_run(active, pipeline::RunSeed::resuming(&text));
                },
                on_continue: move |()| {
                    resume_run(active, pipeline::RunSeed::Continue);
                },
                on_reset: move |_| {
                    active.processing.clone().set(false);
                    active.session_id.clone().set(None);
                    active.files.clone().set(Vec::new());
                    active.feedback.clone().set(String::new());
                    active.timeline_open.clone().set(false);
                    active.state.clone().set(ProcessingState::default());
                    // Starting over begins a different form in this tab
                    // slot, so its cost should not carry the old form's total.
                    active.total_spend.clone().set(pipeline::Spend::default());
                    save_workspace();
                },
            }
        }
    }
}
