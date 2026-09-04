//! Settings page component.
//!
//! Renders a full-page settings view (toggled from the header gear button),
//! organized into tabs. Every control funnels through one `update` callback:
//! it copies the current settings, applies the one field the row owns, and
//! hands the whole struct back to the app, which persists it.

use dioxus::prelude::*;

use runner::Provider;

use super::page::{FullPage, PageTabs, RowInfo};
use crate::settings::AppSettings;

/// Offered when the model list cannot be fetched from the API. Derived from
/// `runner::models::KNOWN_MODELS` so the picker cannot drift from the limits table.
fn anthropic_fallback_models() -> Vec<String> {
    runner::models::KNOWN_MODELS
        .iter()
        .map(|m| m.id.to_string())
        .collect()
}

/// The settings tabs, in the order they are shown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SettingsTab {
    #[default]
    General,
    Ai,
    Aem,
    References,
}

impl SettingsTab {
    const ALL: &'static [Self] = &[Self::General, Self::Ai, Self::Aem, Self::References];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Ai => "AI Model",
            Self::Aem => "AEM Connection",
            Self::References => "References",
        }
    }
}

/// A change to one settings field, applied to a copy of the current settings.
type Edit = Box<dyn FnOnce(&mut AppSettings)>;

#[component]
pub fn SettingsPage(
    /// Called when the user closes the settings page.
    on_close: EventHandler<()>,
    /// Current settings.
    settings: ReadSignal<AppSettings>,
    /// Called when the user changes any setting.
    on_settings_changed: EventHandler<AppSettings>,
    /// Called when the user opens the reference-forms manager page.
    on_open_references: EventHandler<()>,
) -> Element {
    let mut tab = use_signal(SettingsTab::default);

    // Whether the Blueprint MCP server is registered in Claude Desktop, and the
    // last install error (shown below the row). Checked once on mount; flipped
    // to `true` after a successful install.
    let mut mcp_installed = use_signal(crate::mcp_install::is_installed);
    let mut mcp_install_error: Signal<Option<String>> = use_signal(|| None);

    // The browser preparation's progress and outcome, shown under its row.
    // `Ok` lines are progress and the final report; `Err` is the preflight's
    // own message, which already says what to fix.
    let mut browser_status: Signal<Option<Result<String, String>>> = use_signal(|| None);
    let mut browser_preparing = use_signal(|| false);

    // The single write path: every row below calls this with the one field it
    // owns, so there is no per-row copy of the settings struct.
    let update = use_callback(move |edit: Edit| {
        let mut next = settings();
        edit(&mut next);
        on_settings_changed.call(next);
    });

    // Narrow the model fetch to the endpoint alone — a memo only fires when the
    // provider, key or base URL changes, so editing an unrelated setting does
    // not refetch.
    let endpoint = use_memo(move || settings.read().llm_endpoint());
    let models = use_resource(move || {
        let endpoint = endpoint();
        async move { endpoint.list_models().await }
    });
    // The offline fallback is Anthropic's table; an OpenAI-compatible endpoint
    // that cannot be listed gets a free-text field instead (see below), because
    // there is no catalogue to guess from.
    let model_list = use_memo(move || match &*models.read() {
        Some(Ok(list)) if !list.is_empty() => list.clone(),
        _ if settings.read().llm_provider == Provider::OpenAi => Vec::new(),
        _ => anthropic_fallback_models(),
    });

    let s = settings.read();

    rsx! {
        FullPage { title: "Settings", on_close,

            PageTabs {
                labels: SettingsTab::ALL.iter().map(|t| t.label().to_string()).collect::<Vec<_>>(),
                active: SettingsTab::ALL.iter().position(|t| *t == tab()).unwrap_or(0),
                on_select: move |index: usize| {
                    if let Some(t) = SettingsTab::ALL.get(index) {
                        tab.set(*t);
                    }
                },
            }

            div { class: "page-content",
                match tab() {
                    SettingsTab::General => rsx! {
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Window" }
                            ToggleRow {
                                label: "Always on top",
                                desc: "Keep the window above all other applications.",
                                checked: s.always_on_top,
                                on_toggle: move |v: bool| update.call(Box::new(move |s| s.always_on_top = v)),
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Claude Desktop" }
                            div { class: "row",
                                RowInfo {
                                    label: "Blueprint MCP server",
                                    desc: "Register Blueprint's conversion tools with Claude Desktop so you can drive conversions from Claude. Restart Claude Desktop after installing.",
                                }
                                if mcp_installed() {
                                    span { class: "mcp-installed", "Installed ✓" }
                                } else {
                                    button {
                                        class: "btn btn-primary btn-sm",
                                        onclick: move |_| {
                                            match crate::mcp_install::install() {
                                                Ok(()) => {
                                                    mcp_install_error.set(None);
                                                    mcp_installed.set(true);
                                                }
                                                Err(e) => mcp_install_error.set(Some(e)),
                                            }
                                        },
                                        "Install"
                                    }
                                }
                            }
                            if let Some(err) = mcp_install_error.read().as_ref() {
                                div { class: "mcp-error", "{err}" }
                            }
                        }
                    },

                    SettingsTab::Ai => rsx! {
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Provider" }
                            SelectRow {
                                label: "API",
                                desc: "Which API the conversion agent talks to. The OpenAI-compatible option reaches any chat-completions endpoint (OpenRouter, a local gateway); it sends no prompt cache breakpoints, so a long run costs more input tokens there.",
                                value: s.llm_provider.as_str().to_string(),
                                options: Provider::ALL.iter().map(|p| p.as_str().to_string()).collect(),
                                labels: Provider::ALL.iter().map(|p| p.label().to_string()).collect(),
                                on_change: move |v: String| {
                                    if let Some(p) = Provider::parse(&v) {
                                        update.call(Box::new(move |s| s.llm_provider = p));
                                    }
                                },
                            }
                        }
                        if s.llm_provider == Provider::Anthropic {
                            div { class: "settings-section",
                                h3 { class: "settings-section-title", "Anthropic" }
                                TextRow {
                                    label: "Anthropic API Key",
                                    desc: "Paste your Anthropic (Claude) API key here. Used for AI features. Stored locally on disk.",
                                    value: s.anthropic_api_key.clone(),
                                    placeholder: "sk-ant-...",
                                    secret: true,
                                    on_change: move |v: String| {
                                        update.call(Box::new(move |s| s.anthropic_api_key = v.trim().to_string()))
                                    },
                                }
                                SelectRow {
                                    label: "Model",
                                    desc: "Claude model used for AI features (the conversion agent and reference descriptions).",
                                    value: s.anthropic_model.clone(),
                                    options: model_list(),
                                    labels: Vec::new(),
                                    on_change: move |v: String| update.call(Box::new(move |s| s.anthropic_model = v)),
                                }
                            }
                        } else {
                            div { class: "settings-section",
                                h3 { class: "settings-section-title", "OpenAI-compatible endpoint" }
                                TextRow {
                                    label: "Base URL",
                                    desc: "API root of the endpoint, without /chat/completions.",
                                    value: s.openai_base_url.clone(),
                                    placeholder: "https://openrouter.ai/api/v1",
                                    secret: false,
                                    on_change: move |v: String| {
                                        update.call(Box::new(move |s| s.openai_base_url = v.trim().to_string()))
                                    },
                                }
                                TextRow {
                                    label: "API Key",
                                    desc: "Sent as an Authorization: Bearer header. Stored locally on disk.",
                                    value: s.openai_api_key.clone(),
                                    placeholder: "sk-or-...",
                                    secret: true,
                                    on_change: move |v: String| {
                                        update.call(Box::new(move |s| s.openai_api_key = v.trim().to_string()))
                                    },
                                }
                                if model_list().is_empty() {
                                    TextRow {
                                        label: "Model",
                                        desc: "Model id at this endpoint. The list could not be fetched, so type the id exactly as the endpoint spells it.",
                                        value: s.openai_model.clone(),
                                        placeholder: "anthropic/claude-opus-4.1",
                                        secret: false,
                                        on_change: move |v: String| {
                                            update.call(Box::new(move |s| s.openai_model = v.trim().to_string()))
                                        },
                                    }
                                } else {
                                    SelectRow {
                                        label: "Model",
                                        desc: "Model id at this endpoint. Only models that support tool calling and images can drive a conversion.",
                                        value: s.openai_model.clone(),
                                        options: model_list(),
                                        labels: Vec::new(),
                                        unset_label: "Select a model…",
                                        on_change: move |v: String| {
                                            update.call(Box::new(move |s| s.openai_model = v.trim().to_string()))
                                        },
                                    }
                                }
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Conversion" }
                            NumberRow {
                                label: "Max review rounds",
                                desc: "How many Reviewer → Author fix rounds before finalizing with whatever is built. Higher = more self-correction, more tokens.",
                                value: s.max_review_rounds,
                                min: 1,
                                step: 1,
                                on_change: move |v: usize| {
                                    update.call(Box::new(move |s| s.max_review_rounds = v.max(1)))
                                },
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Context management" }
                            NumberRow {
                                label: "Keep recent messages",
                                desc: "Most recent messages kept verbatim each turn (rounded up to even). Higher = better grounding, more tokens.",
                                value: s.evict_keep_recent_messages,
                                min: 2,
                                step: 2,
                                on_change: move |v: usize| {
                                    update.call(Box::new(move |s| s.evict_keep_recent_messages = v))
                                },
                            }
                            NumberRow {
                                label: "Parallel requests",
                                desc: "How many model requests conversions may have in flight at once. 0 removes the cap.",
                                value: s.max_concurrent_requests,
                                min: 0,
                                step: 1,
                                on_change: move |v: usize| {
                                    update.call(Box::new(move |s| s.max_concurrent_requests = v))
                                },
                            }
                            NumberRow {
                                label: "Elide text over (chars)",
                                desc: "Stale tool-result text longer than this is replaced with a re-fetchable stub.",
                                value: s.evict_text_over_chars,
                                min: 0,
                                step: 500,
                                on_change: move |v: usize| {
                                    update.call(Box::new(move |s| s.evict_text_over_chars = v))
                                },
                            }
                            NumberRow {
                                label: "Elide tool input over (chars)",
                                desc: "Stale tool-call inputs (e.g. whole-tree writes) longer than this are stubbed.",
                                value: s.evict_input_over_chars,
                                min: 0,
                                step: 500,
                                on_change: move |v: usize| {
                                    update.call(Box::new(move |s| s.evict_input_over_chars = v))
                                },
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Custom instructions" }
                            div { class: "row row-stack",
                                RowInfo {
                                    label: "Agent (AI processing)",
                                    desc: "Extra instructions appended to the autonomous conversion agent's system prompt. Applied to AI processing and feedback re-runs.",
                                }
                                textarea {
                                    class: "settings-textarea",
                                    rows: "4",
                                    placeholder: "e.g. Always keep signature blocks on the last page.",
                                    value: "{s.agent_instructions}",
                                    onchange: move |e: Event<FormData>| {
                                        let v = e.value();
                                        update.call(Box::new(move |s| s.agent_instructions = v));
                                    },
                                }
                            }
                        }
                    },

                    SettingsTab::Aem => rsx! {
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "AEM Connection" }
                            TextRow {
                                label: "AEM Host",
                                desc: "Base URL of the AEM author instance used for package upload.",
                                value: s.aem_host.clone(),
                                placeholder: "http://localhost:4502",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.aem_host = v.trim().to_string()))
                                },
                            }
                            TextRow {
                                label: "AEM Username",
                                desc: "Username for AEM HTTP basic auth.",
                                value: s.aem_username.clone(),
                                placeholder: "admin",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.aem_username = v.trim().to_string()))
                                },
                            }
                            TextRow {
                                label: "AEM Password",
                                desc: "Password for AEM HTTP basic auth. Stored locally on disk.",
                                value: s.aem_password.clone(),
                                placeholder: "••••••••",
                                secret: true,
                                on_change: move |v: String| update.call(Box::new(move |s| s.aem_password = v)),
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Browser verification" }
                            ToggleRow {
                                label: "Verify the deployed form in a browser",
                                desc: "After uploading, the Author and Reviewer open the form in a headless Chrome (Playwright MCP, pinned), fill it in, submit it and read the PDF. Needs Node.js and Google Chrome; a run refuses to start when the check fails.",
                                checked: s.browser_enabled,
                                on_toggle: move |v: bool| update.call(Box::new(move |s| s.browser_enabled = v)),
                            }
                            TextRow {
                                label: "npx path",
                                desc: "Leave empty to find npx on PATH and in the usual Node.js locations.",
                                value: s.browser_npx_path.clone(),
                                placeholder: "auto-detect",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.browser_npx_path = v.trim().to_string()))
                                },
                            }
                            div { class: "row",
                                RowInfo {
                                    label: "Prepare browser tooling",
                                    desc: "Download the pinned Playwright MCP into the npm cache once and confirm Node.js and Google Chrome are usable, so runs never wait on the network.",
                                }
                                button {
                                    class: "btn btn-primary btn-sm",
                                    disabled: browser_preparing(),
                                    onclick: move |_| {
                                        let npx = settings.read().browser_npx_path.trim().to_string();
                                        let cfg = agent::browser::BrowserConfig {
                                            npx: (!npx.is_empty()).then(|| std::path::PathBuf::from(npx)),
                                        };
                                        browser_preparing.set(true);
                                        browser_status.set(Some(Ok(String::new())));
                                        spawn(async move {
                                            let mut progress = |line: &str| {
                                                let mut status = browser_status.write();
                                                if let Some(Ok(text)) = status.as_mut() {
                                                    if !text.is_empty() {
                                                        text.push('\n');
                                                    }
                                                    text.push_str(line);
                                                }
                                            };
                                            let result = agent::browser::prepare(&cfg, &mut progress).await;
                                            browser_status.set(Some(result.map(|p| format!("Ready.\n{p}"))));
                                            browser_preparing.set(false);
                                        });
                                    },
                                    if browser_preparing() { "Preparing…" } else { "Prepare" }
                                }
                            }
                            match browser_status.read().as_ref() {
                                Some(Ok(text)) if !text.is_empty() => rsx! { div { class: "browser-status", "{text}" } },
                                Some(Err(err)) => rsx! { div { class: "mcp-error", "{err}" } },
                                _ => rsx! {},
                            }
                        }
                    },

                    SettingsTab::References => rsx! {
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Reference forms" }
                            div { class: "row",
                                RowInfo {
                                    label: "Manage reference forms",
                                    desc: "Add, import, export, and delete the reference forms used for matching.",
                                }
                                button {
                                    class: "btn btn-primary btn-sm",
                                    onclick: move |_| {
                                        on_close.call(());
                                        on_open_references.call(());
                                    },
                                    "Open…"
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// A settings row carrying an on/off switch.
#[component]
fn ToggleRow(
    label: &'static str,
    desc: &'static str,
    checked: bool,
    on_toggle: EventHandler<bool>,
) -> Element {
    rsx! {
        div { class: "row",
            RowInfo { label, desc }
            label { class: "toggle-switch",
                input {
                    r#type: "checkbox",
                    checked,
                    onchange: move |e: Event<FormData>| on_toggle.call(e.checked()),
                }
                span { class: "toggle-slider" }
            }
        }
    }
}

/// A settings row carrying a single-line text field. `secret` masks the input.
#[component]
fn TextRow(
    label: &'static str,
    desc: &'static str,
    value: String,
    placeholder: &'static str,
    secret: bool,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "row",
            RowInfo { label, desc }
            input {
                class: "settings-input-text",
                r#type: if secret { "password" } else { "text" },
                placeholder,
                value,
                onchange: move |e: Event<FormData>| on_change.call(e.value()),
            }
        }
    }
}

/// A settings row carrying a whole-number field. Unparseable input is ignored,
/// leaving the stored value untouched.
#[component]
fn NumberRow(
    label: &'static str,
    desc: &'static str,
    value: usize,
    min: usize,
    step: usize,
    on_change: EventHandler<usize>,
) -> Element {
    rsx! {
        div { class: "row",
            RowInfo { label, desc }
            input {
                class: "settings-input-number",
                r#type: "number",
                min: "{min}",
                step: "{step}",
                value: "{value}",
                onchange: move |e: Event<FormData>| {
                    if let Ok(v) = e.value().parse::<usize>() {
                        on_change.call(v);
                    }
                },
            }
        }
    }
}

/// A settings row carrying a dropdown over `options`.
///
/// `labels` is what the user reads, `options` what gets stored; an empty
/// `labels` shows the stored values themselves, which is what a list of model
/// ids wants. `unset_label` adds a leading empty entry while nothing is chosen,
/// so an unset value reads as unset rather than showing the first option as if
/// it had been picked.
#[component]
fn SelectRow(
    label: &'static str,
    desc: &'static str,
    value: String,
    options: Vec<String>,
    labels: Vec<String>,
    unset_label: Option<&'static str>,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "row",
            RowInfo { label, desc }
            select {
                class: "settings-select",
                value: "{value}",
                onchange: move |e: Event<FormData>| on_change.call(e.value()),
                if let Some(unset) = unset_label.filter(|_| value.is_empty()) {
                    option { value: "", selected: true, "{unset}" }
                }
                for (index, option_value) in options.iter().enumerate() {
                    option {
                        value: "{option_value}",
                        selected: value == *option_value,
                        "{labels.get(index).unwrap_or(option_value)}"
                    }
                }
            }
        }
    }
}
