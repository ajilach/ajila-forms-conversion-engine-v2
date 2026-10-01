mod agent_runner;
mod components;
mod files;
mod mcp_install;
mod models;
mod run_status;
mod tabs;
mod upload;
mod workspace;

// The headless engine layer (edit-history store, reference store, AEM client)
// lives in the `agent` crate; the LLM transport and the operator settings live
// in `runner`, shared with the CLI. Re-export both under the historical
// `crate::*` paths so the rest of the app is unchanged.
pub use agent::{aem_client, db, references, session};
pub use runner::settings;

use dioxus::prelude::*;

use components::{AgentFlow, FormTabs, ReferencesPage, SettingsPage};
use models::{ProcessingState, ProcessingStep, UploadState};
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

    // Either stream holds the document: the structured envelope, or the AEM
    // tree snapshotted after every mutating tool call. `> 0` because sequence
    // zero is the empty seed a run writes before it does anything.
    let has_snapshot = saved.session_id.as_deref().is_some_and(|session| {
        db::latest_seq(session).is_some_and(|seq| seq > 0)
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
    let profiles = use_hook(blueprint::list_profiles);
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
        blueprint::OutputTarget::default(),
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
        retry: tab.retry.peek().clone(),
    };
    let begin_run = move |tab: Tab| {
        // A previous run in this tab may have left them set.
        tab.abort.peek().reset();
        tab.retry.peek().clear();
        // Whatever the box was explaining about the last session no longer
        // describes what is on screen.
        tab.restored.clone().set(None);
        tab.processing.clone().set(true);
        tab.state.clone().set(ProcessingState {
            step: ProcessingStep::Running,
            ..ProcessingState::default()
        });
    };

    // What every run does once it is under way, however it was started: show
    // its progress until it ends, then do the tab's bookkeeping, all on the UI
    // thread.
    let settle_run = move |tab: Tab,
                           run: tokio::task::JoinHandle<()>,
                           updates: agent_runner::ProgressReceiver| async move {
        let session = match agent_runner::show_progress(updates, tab.state).await {
            agent_runner::Settled::Finished(session) => session,
            // Only a panic ends the run's task without a result, and its
            // channel has closed, so the task is already unwinding. The tab
            // still counts as running here, so closing it meanwhile parks it
            // rather than releasing the signals written below.
            agent_runner::Settled::Vanished => {
                let error = match run.await {
                    Err(e) => format!("The run stopped unexpectedly: {e}"),
                    Ok(()) => "The run stopped without reporting a result.".to_string(),
                };
                tab.state.clone().write().error.get_or_insert(error);
                None
            }
        };
        // Nothing has been awaited since the result was applied, so no click on
        // the finished box can have run in between.
        if let Some(session) = session {
            tab.session_id.clone().set(Some(session));
        }
        // Fold what this run cost into the tab's running total before the next
        // run's `begin_run` clears `state`'s own copy.
        if let Some(spend) = tab.state.read().spend {
            tab.total_spend.clone().write().merge(&spend);
        }
        workspace.finish_run(tab.id);
        save_workspace();
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
            .any(|(name, _)| name.to_ascii_lowercase().ends_with(".pdf"));
        // An AEM content-package ZIP may be attached as an editable template for
        // the agent's working tree. Proceed with PDFs, a template, or both.
        let has_template = file_data
            .iter()
            .any(|(_, bytes)| blueprint::detect_aem_zip(bytes));
        if !has_pdf && !has_template {
            return;
        }

        tab.session_id.clone().set(None);
        tab.aem_upload.clone().set(UploadState::Idle);

        let config = run_config(tab);
        begin_run(tab);
        save_workspace();

        // Two layers on purpose. The run itself goes to a worker thread, so a
        // package build or a PDF extraction in one tab cannot freeze the other
        // tabs and the window along with them. It never touches the tab's
        // signals: it sends its progress down a channel, and this task — on the
        // UI thread, where the tab's signals live — applies it and does the
        // bookkeeping once the run is over.
        spawn(async move {
            let session_label = file_data
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
                .join(", ");

            let (progress, updates) = agent_runner::progress_channel();
            let run = tokio::spawn(agent_runner::run_agent(
                file_data,
                config,
                session_label,
                progress,
            ));
            settle_run(tab, run, updates).await;
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
            let (progress, updates) = agent_runner::progress_channel();
            let run = tokio::spawn(agent_runner::run_agent_resume(
                seed, pdfs, config, session, progress,
            ));
            settle_run(tab, run, updates).await;
        });
    };

    // ── On-demand AEM install ────────────────────────────────────────────────
    // Started here rather than inside the result panel: switching tabs unmounts
    // that panel, and Dioxus cancels a scope's tasks along with it, which would
    // abandon an install already in flight.
    let on_aem_upload = move |tab: Tab| {
        let Some(connection) = app_settings.read().aem_connection() else {
            return;
        };
        let (package, package_name) = {
            let run = tab.state.read();
            let Some(package) = run.aem_package.clone() else {
                return;
            };
            (
                package,
                run.form_code
                    .clone()
                    .unwrap_or_else(|| "forms-package".to_string()),
            )
        };

        let mut upload = tab.aem_upload;
        upload.set(UploadState::Uploading);
        spawn(async move {
            match crate::aem_client::upload_and_install_package(
                &connection,
                package,
                &package_name,
            )
            .await
            {
                Ok(()) => upload.set(UploadState::Success),
                Err(e) => upload.set(UploadState::Error(e)),
            }
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
        } else {
            // The open conversions, and the active one's flow:
            // upload → live timeline → done.
            FormTabs { workspace, on_changed: move |()| save_workspace() }
            AgentFlow {
                tab: active,
                profiles,
                ai_available: !app_settings.read().active_api_key().is_empty(),
                aem_connection: app_settings.read().aem_connection(),
                on_ai_process: move |files: Vec<(String, Vec<u8>)>| {
                    on_ai_process(active, files);
                },
                on_feedback: move |text: String| {
                    resume_run(active, pipeline::RunSeed::resuming(&text));
                },
                on_continue: move |()| {
                    resume_run(active, pipeline::RunSeed::Continue);
                },
                on_aem_upload: move |()| {
                    on_aem_upload(active);
                },
                on_reset: move |_| {
                    active.processing.clone().set(false);
                    active.session_id.clone().set(None);
                    active.files.clone().set(Vec::new());
                    active.feedback.clone().set(String::new());
                    active.timeline_open.clone().set(false);
                    active.aem_upload.clone().set(UploadState::Idle);
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
