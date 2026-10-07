//! Full-page reference-form manager.
//!
//! Add a reference form (with an LLM-generated description), browse/search and
//! delete references, and import/export reference datasets. A profile dropdown
//! scopes which profile new references are added to (and imported/exported).
//! Opened from the Settings panel.

use std::collections::HashSet;

use dioxus::prelude::*;

use super::page::{FullPage, PageTabs, RowInfo};
use crate::references::{ReferenceDocInfo, ReferenceInfo};
use crate::settings::AppSettings;
use crate::upload::read_files;

/// Prompt sent to the LLM to describe an input form when adding a reference.
/// The model is given tools to analyse the inputs first (see
/// [`crate::ai_tools::build_describe_tools`]).
/// The manager's tabs, in the order they are shown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RefTab {
    #[default]
    Forms,
    Docs,
    ImportExport,
}

impl RefTab {
    const ALL: &'static [Self] = &[Self::Forms, Self::Docs, Self::ImportExport];

    fn label(self, forms: usize, docs: usize) -> String {
        match self {
            Self::Forms => format!("Reference forms ({forms})"),
            Self::Docs => format!("Reference documentation ({docs})"),
            Self::ImportExport => "Import / Export".to_string(),
        }
    }
}

/// The outcome of the last action, shown as a banner above the cards.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Status {
    /// In progress or succeeded.
    Ok(String),
    Err(String),
}

impl Status {
    fn class(&self) -> &'static str {
        match self {
            Self::Ok(_) => "references-status references-status-ok",
            Self::Err(_) => "references-status references-status-err",
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Ok(m) | Self::Err(m) => m,
        }
    }
}

/// Resolve the profile an action applies to, reporting the problem if there is
/// none selected. Every action here is profile-scoped, so they all start here.
fn require_profile(profile: Option<String>, mut status: Signal<Option<Status>>) -> Option<String> {
    match profile.filter(|p| !p.is_empty()) {
        Some(p) => Some(p),
        None => {
            status.set(Some(Status::Err("Select a profile first.".into())));
            None
        }
    }
}

/// Full-page reference-form manager.
#[component]
pub fn ReferencesPage(
    /// Active conversion profile — used as the default profile selection.
    profile: Option<String>,
    /// Current settings (for the Anthropic API key and model).
    settings: ReadSignal<AppSettings>,
    /// Called when the user closes the page.
    on_close: EventHandler<()>,
) -> Element {
    // Bumped after every mutation; the two list memos below hang off it, so the
    // store is read once per change instead of once per render.
    let refresh = use_signal(|| 0u32);
    let status = use_signal(|| None::<Status>);
    let busy = use_signal(|| false);
    let mut tab = use_signal(RefTab::default);

    let profiles = use_hook(agent::profiles::list_profiles);

    // Profile that new references are added to / imported into / exported from.
    // Defaults to the active profile, falling back to the first configured one.
    let mut selected_profile = use_signal(|| profile.clone().or_else(|| profiles.first().cloned()));

    // The reference store is per-profile; union every profile's entries into one
    // list. Reading SQLite is synchronous, so this must not happen per render —
    // the memo confines it to an actual change.
    let all_refs = use_memo({
        let profiles = profiles.clone();
        move || {
            let _ = refresh();
            profiles
                .iter()
                .flat_map(|p| crate::references::store().list_references(p))
                .collect::<Vec<_>>()
        }
    });
    let all_docs = use_memo({
        let profiles = profiles.clone();
        move || {
            let _ = refresh();
            profiles
                .iter()
                .flat_map(|p| crate::references::store().list_docs(p))
                .collect::<Vec<_>>()
        }
    });

    let total_refs = use_memo(move || all_refs.read().len());
    let total_docs = use_memo(move || all_docs.read().len());

    // No profiles configured → nothing to scope references to.
    if profiles.is_empty() {
        return rsx! {
            FullPage { title: "Reference Forms", on_close,
                div { class: "page-content page-content-stack",
                    p { class: "references-empty", "No profiles are configured." }
                }
            }
        };
    }

    let current_profile = selected_profile.read().clone().unwrap_or_default();
    // Export is available if the selected profile has any forms or any docs.
    let can_export = all_refs.read().iter().any(|r| r.profile == current_profile)
        || all_docs.read().iter().any(|d| d.profile == current_profile);

    rsx! {
        FullPage {
            title: "References",
            subtitle: format!("{} form(s) · {} document(s)", total_refs(), total_docs()),
            on_close,

            PageTabs {
                labels: RefTab::ALL
                    .iter()
                    .map(|t| t.label(total_refs(), total_docs()))
                    .collect::<Vec<_>>(),
                active: RefTab::ALL.iter().position(|t| *t == tab()).unwrap_or(0),
                on_select: move |index: usize| {
                    if let Some(t) = RefTab::ALL.get(index) {
                        tab.set(*t);
                    }
                },
            }

            div { class: "page-content page-content-stack",

                if let Some(status) = status.read().as_ref() {
                    div { class: status.class(), "{status.message()}" }
                }

                // ── Profile scope (shared across both tabs) ─────────────────────
                section { class: "references-card",
                    div { class: "row",
                        RowInfo {
                            label: "Profile",
                            desc: "References and documentation are added to (and imported/exported for) this profile.",
                        }
                        select {
                            class: "settings-select",
                            value: "{current_profile}",
                            onchange: move |e: Event<FormData>| selected_profile.set(Some(e.value())),
                            for p in profiles.iter() {
                                option {
                                    value: "{p}",
                                    selected: current_profile == *p,
                                    "{p}"
                                }
                            }
                        }
                    }
                }

                match tab() {
                    RefTab::Forms => rsx! {
                        AddReferenceForm { settings, selected_profile, status, busy, refresh }
                        ReferenceList { items: all_refs, refresh }
                    },
                    RefTab::Docs => rsx! {
                        AddDocumentation { selected_profile, status, refresh }
                        DocList { items: all_docs, refresh }
                    },
                    RefTab::ImportExport => rsx! {
                        ImportExport { selected_profile, status, busy, refresh, can_export }
                    },
                }
            }
        }
    }
}

/// A long text that collapses to one line and expands on click.
#[component]
fn ExpandableText(id: String, text: String, mut expanded: Signal<HashSet<String>>) -> Element {
    if text.trim().is_empty() {
        return rsx! {};
    }

    let is_expanded = expanded.read().contains(&id);
    rsx! {
        span {
            class: if is_expanded { "references-item-desc references-item-desc-expanded" } else { "references-item-desc" },
            title: if is_expanded { "Click to collapse" } else { "Click to expand" },
            onclick: move |_| {
                let mut set = expanded.write();
                if !set.remove(&id) {
                    set.insert(id.clone());
                }
            },
            "{text}"
        }
    }
}

/// Upload the original PDFs plus the finished AEM package, have the model
/// describe the form, and store it for matching.
#[component]
fn AddReferenceForm(
    settings: ReadSignal<AppSettings>,
    selected_profile: Signal<Option<String>>,
    mut status: Signal<Option<Status>>,
    mut busy: Signal<bool>,
    mut refresh: Signal<u32>,
) -> Element {
    let mut pdfs = use_signal(Vec::<(String, Vec<u8>)>::new);
    let mut pkg = use_signal(|| None::<(String, Vec<u8>)>);

    let pdf_names = pdfs
        .read()
        .iter()
        .map(|(n, _)| n.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let pdf_desc = if pdf_names.is_empty() {
        "The input form (one or more XFA PDFs).".to_string()
    } else {
        pdf_names
    };
    let pkg_desc = pkg
        .read()
        .as_ref()
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "The resulting FileVault ZIP.".to_string());

    rsx! {
        section { class: "references-card",
            h3 { "Add a reference form" }
            p { class: "references-card-desc",
                "Pick the original input form and the resulting AEM package. The form is analysed and described automatically, then stored for matching."
            }
            div { class: "row",
                RowInfo { label: "Original PDF(s)", desc: pdf_desc }
                input {
                    r#type: "file",
                    accept: ".pdf",
                    multiple: true,
                    onchange: move |evt: Event<FormData>| async move {
                        let collected = read_files(evt.files()).await;
                        if !collected.is_empty() {
                            pdfs.set(collected);
                        }
                    },
                }
            }
            div { class: "row",
                RowInfo { label: "Final AEM package", desc: pkg_desc }
                input {
                    r#type: "file",
                    accept: ".zip",
                    onchange: move |evt: Event<FormData>| async move {
                        if let Some(file) = read_files(evt.files()).await.into_iter().next() {
                            pkg.set(Some(file));
                        }
                    },
                }
            }
            div { class: "row row-actions",
                button {
                    class: "btn btn-primary",
                    disabled: busy(),
                    onclick: move |_| {
                        let endpoint = settings.read().llm_endpoint();
                        let pdf_data = pdfs.read().clone();
                        let pkg_data = pkg.read().clone();
                        let profile = selected_profile.read().clone();
                        spawn(async move {
                            let Some(profile) = require_profile(profile, status) else {
                                return;
                            };
                            let Some((_, pkg_bytes)) = pkg_data else {
                                status.set(Some(Status::Err("Pick an AEM package first.".into())));
                                return;
                            };
                            if pdf_data.is_empty() {
                                status
                                    .set(
                                        Some(Status::Err("Pick at least one original PDF first.".into())),
                                    );
                                return;
                            }
                            busy.set(true);
                            status.set(Some(Status::Ok("Analysing the inputs…".into())));

                            let description = match crate::agent_runner::describe_reference(
                                    &profile,
                                    pdf_data.clone(),
                                    pkg_bytes.clone(),
                                    endpoint,
                                )
                                .await
                            {
                                Ok(d) => d,
                                Err(e) => {
                                    busy.set(false);
                                    status.set(Some(Status::Err(format!("Describe failed: {e}"))));
                                    return;
                                }
                            };

                            status.set(Some(Status::Ok("Saving the reference…".into())));
                            let result = tokio::task::spawn_blocking(move || {
                                    crate::references::store().ingest_reference(
                                        &profile,
                                        pdf_data,
                                        &pkg_bytes,
                                        &description,
                                    )
                                })
                                .await
                                .unwrap_or_else(|e| Err(e.to_string()));

                            busy.set(false);
                            match result {
                                Ok(()) => {
                                    status.set(Some(Status::Ok("Reference form added.".into())));
                                    pdfs.set(Vec::new());
                                    pkg.set(None);
                                    refresh += 1;
                                }
                                Err(e) => status.set(Some(Status::Err(format!("Add failed: {e}")))),
                            }
                        });
                    },
                    if busy() { "Working…" } else { "Add reference form" }
                }
            }
        }
    }
}

/// The stored reference forms across all profiles, searchable.
#[component]
fn ReferenceList(items: ReadSignal<Vec<ReferenceInfo>>, mut refresh: Signal<u32>) -> Element {
    let mut search = use_signal(String::new);
    let expanded = use_signal(HashSet::<String>::new);

    let total = items.read().len();
    let filtered = use_memo(move || {
        let query = search.read().trim().to_lowercase();
        items
            .read()
            .iter()
            .filter(|r| {
                query.is_empty()
                    || r.label.to_lowercase().contains(&query)
                    || r.profile.to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>()
    });

    rsx! {
        section { class: "references-card",
            h3 { "Existing references ({total})" }
            input {
                class: "references-search",
                r#type: "text",
                placeholder: "Search references by name or profile…",
                value: "{search}",
                oninput: move |e: Event<FormData>| search.set(e.value()),
            }
            if total == 0 {
                p { class: "references-empty", "No reference forms yet." }
            } else if filtered.read().is_empty() {
                p { class: "references-empty", "No references match your search." }
            } else {
                ul { class: "session-list",
                    for r in filtered.read().iter() {
                        li { key: "{r.ref_id}", class: "session-item",
                            div { class: "session-meta",
                                span { class: "session-label", "{r.label}" }
                                span { class: "session-submeta",
                                    "{r.profile} · {r.pdf_count} pdf(s) · {r.files.len()} file(s)"
                                }
                                ExpandableText {
                                    id: r.ref_id.clone(),
                                    text: r.description.clone(),
                                    expanded,
                                }
                            }
                            button {
                                class: "btn btn-secondary btn-sm",
                                onclick: {
                                    let ref_id = r.ref_id.clone();
                                    move |_| {
                                        crate::references::store().delete_reference(&ref_id);
                                        refresh += 1;
                                    }
                                },
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Upload a plain-text or Markdown file to keep alongside a profile's references.
#[component]
fn AddDocumentation(
    selected_profile: Signal<Option<String>>,
    mut status: Signal<Option<Status>>,
    mut refresh: Signal<u32>,
) -> Element {
    let mut doc_file = use_signal(|| None::<(String, String)>);

    let doc_desc = doc_file
        .read()
        .as_ref()
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "A plain .txt or .md file.".to_string());

    rsx! {
        section { class: "references-card",
            h3 { "Add reference documentation" }
            p { class: "references-card-desc",
                "Upload a plain text or Markdown file to keep alongside this profile's references."
            }
            div { class: "row",
                RowInfo { label: "Documentation file (.txt / .md)", desc: doc_desc }
                input {
                    r#type: "file",
                    accept: ".txt,.md,text/plain,text/markdown",
                    onchange: move |evt: Event<FormData>| async move {
                        if let Some((name, bytes)) = read_files(evt.files()).await.into_iter().next()
                        {
                            doc_file.set(Some((name, String::from_utf8_lossy(&bytes).to_string())));
                        }
                    },
                }
            }
            div { class: "row row-actions",
                button {
                    class: "btn btn-primary",
                    disabled: doc_file.read().is_none(),
                    onclick: move |_| {
                        let Some(profile) = require_profile(selected_profile.read().clone(), status)
                        else {
                            return;
                        };
                        let Some((name, content)) = doc_file.read().clone() else {
                            status
                                .set(Some(Status::Err("Pick a documentation file first.".into())));
                            return;
                        };
                        let label = name.trim_end_matches(".md").trim_end_matches(".txt").to_string();
                        let doc_id = crate::references::compute_doc_id(&content);
                        match crate::references::store().add_doc(&profile, &doc_id, &label, &content) {
                            Ok(()) => {
                                status.set(Some(Status::Ok("Documentation added.".into())));
                                doc_file.set(None);
                                refresh += 1;
                            }
                            Err(e) => status.set(Some(Status::Err(format!("Add failed: {e}")))),
                        }
                    },
                    "Add documentation"
                }
            }
        }
    }
}

/// The stored documentation across all profiles, searchable by content too.
#[component]
fn DocList(items: ReadSignal<Vec<ReferenceDocInfo>>, mut refresh: Signal<u32>) -> Element {
    let mut search = use_signal(String::new);
    let expanded = use_signal(HashSet::<String>::new);

    let total = items.read().len();
    let filtered = use_memo(move || {
        let query = search.read().trim().to_lowercase();
        items
            .read()
            .iter()
            .filter(|d| {
                query.is_empty()
                    || d.label.to_lowercase().contains(&query)
                    || d.profile.to_lowercase().contains(&query)
                    || d.content.to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>()
    });

    rsx! {
        section { class: "references-card",
            h3 { "Reference documentation ({total})" }
            input {
                class: "references-search",
                r#type: "text",
                placeholder: "Search documentation by name, profile, or content…",
                value: "{search}",
                oninput: move |e: Event<FormData>| search.set(e.value()),
            }
            if total == 0 {
                p { class: "references-empty", "No reference documentation yet." }
            } else if filtered.read().is_empty() {
                p { class: "references-empty", "No documentation matches your search." }
            } else {
                ul { class: "session-list",
                    for d in filtered.read().iter() {
                        li { key: "{d.doc_id}", class: "session-item",
                            div { class: "session-meta",
                                span { class: "session-label", "{d.label}" }
                                span { class: "session-submeta",
                                    "{d.profile} · {d.content.chars().count()} chars"
                                }
                                ExpandableText {
                                    id: d.doc_id.clone(),
                                    text: d.content.clone(),
                                    expanded,
                                }
                            }
                            button {
                                class: "btn btn-secondary btn-sm",
                                onclick: {
                                    let doc_id = d.doc_id.clone();
                                    move |_| {
                                        crate::references::store().delete_doc(&doc_id);
                                        refresh += 1;
                                    }
                                },
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Move a profile's references and documentation in and out as a dataset file.
#[component]
fn ImportExport(
    selected_profile: Signal<Option<String>>,
    mut status: Signal<Option<Status>>,
    mut busy: Signal<bool>,
    mut refresh: Signal<u32>,
    can_export: bool,
) -> Element {
    let mut import_file = use_signal(|| None::<(String, Vec<u8>)>);

    let import_desc = import_file
        .read()
        .as_ref()
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "Load a reference dataset (.db) into the selected profile.".to_string());

    rsx! {
        section { class: "references-card",
            h3 { "Import / Export" }
            div { class: "row",
                RowInfo { label: "Import dataset", desc: import_desc }
                input {
                    r#type: "file",
                    accept: ".db,.sqlite",
                    onchange: move |evt: Event<FormData>| async move {
                        if let Some(file) = read_files(evt.files()).await.into_iter().next() {
                            import_file.set(Some(file));
                        }
                    },
                }
            }
            div { class: "row row-actions",
                button {
                    class: "btn btn-primary",
                    disabled: busy() || import_file.read().is_none(),
                    onclick: move |_| {
                        let profile = selected_profile.read().clone();
                        let file = import_file.read().clone();
                        spawn(async move {
                            let Some(profile) = require_profile(profile, status) else {
                                return;
                            };
                            let Some((_, bytes)) = file else {
                                status.set(Some(Status::Err("Pick a dataset file first.".into())));
                                return;
                            };
                            busy.set(true);
                            status.set(Some(Status::Ok("Importing dataset…".into())));
                            let res = tokio::task::spawn_blocking(move || {
                                    // Per-process name: two apps importing at once
                                    // must not scribble over each other's copy.
                                    let tmp = std::env::temp_dir()
                                        .join(
                                            format!("blueprint-ref-import-{}.db", std::process::id()),
                                        );
                                    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
                                    let r = crate::references::store().import_reference_db(
                                        &tmp.to_string_lossy(),
                                        &profile,
                                    );
                                    let _ = std::fs::remove_file(&tmp);
                                    r
                                })
                                .await
                                .unwrap_or_else(|e| Err(e.to_string()));
                            busy.set(false);
                            match res {
                                Ok((refs, docs)) => {
                                    status
                                        .set(
                                            Some(
                                                Status::Ok(
                                                    format!("Imported {refs} reference(s), {docs} doc(s)."),
                                                ),
                                            ),
                                        );
                                    import_file.set(None);
                                    refresh += 1;
                                }
                                Err(e) => status.set(Some(Status::Err(format!("Import failed: {e}")))),
                            }
                        });
                    },
                    if busy() { "Working…" } else { "Import dataset" }
                }
            }
            div { class: "row",
                RowInfo {
                    label: "Export references",
                    desc: "Save the selected profile's references and documentation to a dataset in your Downloads folder.",
                }
                button {
                    class: "btn btn-secondary",
                    disabled: !can_export,
                    onclick: move |_| {
                        let profile = selected_profile.read().clone();
                        spawn(async move {
                            let Some(profile) = require_profile(profile, status) else {
                                return;
                            };
                            let out = match crate::files::downloads_dir() {
                                Ok(dir) => dir.join(format!("references-{profile}.db")),
                                Err(e) => {
                                    status.set(Some(Status::Err(e)));
                                    return;
                                }
                            };
                            let out_str = out.to_string_lossy().to_string();
                            let res = tokio::task::spawn_blocking(move || {
                                    crate::references::store().export_references(&out_str, Some(&profile))
                                })
                                .await
                                .unwrap_or_else(|e| Err(e.to_string()));
                            match res {
                                Ok((refs, docs)) => {
                                    status
                                        .set(
                                            Some(
                                                Status::Ok(
                                                    format!(
                                                        "Exported {refs} reference(s), {docs} doc(s) to {}",
                                                        out.display(),
                                                    ),
                                                ),
                                            ),
                                        );
                                    crate::files::reveal_in_file_explorer(&out);
                                }
                                Err(e) => status.set(Some(Status::Err(format!("Export failed: {e}")))),
                            }
                        });
                    },
                    "Export references"
                }
            }
        }
    }
}
