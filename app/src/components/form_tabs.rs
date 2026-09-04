//! The strip of open conversions.
//!
//! One button per tab, each its own component so a chatty run only re-renders
//! its own button rather than the whole strip.

use dioxus::prelude::*;

use crate::tabs::{tab_dot, tab_title, MAX_TABS};
use crate::workspace::{Tab, Workspace};

#[component]
pub fn FormTabs(
    workspace: Workspace,
    /// Fired whenever the set of tabs, or which one is showing, changes — the
    /// moments the workspace has to be written back.
    on_changed: EventHandler<()>,
) -> Element {
    let tabs = workspace.tabs();
    let active = workspace.active_id();
    let full = workspace.is_full();
    // Only worth offering a close button once there is more than one tab; with
    // one open it would just clear the tab the user is working in.
    let closable = tabs.len() > 1;

    rsx! {
        div { class: "form-tabs",
            div { class: "form-tabs-scroll",
                for tab in tabs {
                    FormTab {
                        key: "{tab.id}",
                        tab,
                        active: tab.id == active,
                        closable,
                        workspace,
                        on_changed,
                    }
                }
            }
            button {
                class: "form-tab-add",
                disabled: full,
                title: if full {
                    format!("{MAX_TABS} conversions is the maximum")
                } else {
                    "Convert another form".to_string()
                },
                onclick: move |_| {
                    workspace.clone().open();
                    on_changed.call(());
                },
                "+"
            }
        }
    }
}

#[component]
fn FormTab(
    tab: Tab,
    active: bool,
    closable: bool,
    workspace: Workspace,
    on_changed: EventHandler<()>,
) -> Element {
    let state = tab.state.read();
    let dot = tab_dot(&state, (tab.processing)());

    let files = tab.files.read();
    let file_names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    let title = tab_title(state.form_code.as_deref(), &file_names);
    // The strip elides to keep eight tabs inside the window; the tooltip is
    // where the full name stays reachable.
    let full_title = state
        .form_code
        .clone()
        .or_else(|| file_names.first().map(|n| (*n).to_string()))
        .unwrap_or_else(|| "New form".to_string());

    rsx! {
        div { class: if active { "form-tab active" } else { "form-tab" },
            button {
                class: "form-tab-label",
                title: "{full_title}",
                onclick: move |_| {
                    workspace.clone().activate(tab.id);
                    on_changed.call(());
                },
                span { class: "form-tab-dot {dot}" }
                span { class: "form-tab-text", "{title}" }
            }
            if closable {
                button {
                    class: "form-tab-close",
                    title: "Close this conversion",
                    onclick: move |evt| {
                        // The label behind it would otherwise activate the tab
                        // on the way out.
                        evt.stop_propagation();
                        workspace.clone().close(tab.id);
                        on_changed.call(());
                    },
                    "×"
                }
            }
        }
    }
}
