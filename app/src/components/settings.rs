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
            Self::Aem => "Verification",
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

    // The image-pull and readiness-check progress and outcome, shown under the
    // verification buttons. `Ok` lines are progress and the final report; `Err`
    // is the check's own message, which already says what to fix.
    let mut verify_status: Signal<Option<Result<String, String>>> = use_signal(|| None);
    let mut verify_busy = use_signal(|| false);

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
                            h3 { class: "settings-section-title", "Requests" }
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
                            h3 { class: "settings-section-title", "AEM verification" }
                            TextRow {
                                label: "AEM image",
                                desc: "The AEM Forms image the verifier boots, from ajila's private registry. Pull it after `az acr login` (see docker/aem/README.md).",
                                value: s.aem_verify.image.clone(),
                                placeholder: "",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.aem_verify.image = v.trim().to_string()))
                                },
                            }
                            TextRow {
                                label: "Data volume",
                                desc: "The Docker volume holding the deployed UBS platform.",
                                value: s.aem_verify.data_volume.clone(),
                                placeholder: "",
                                secret: false,
                                on_change: move |v: String| {
                                    update
                                        .call(
                                            Box::new(move |s| s.aem_verify.data_volume = v.trim().to_string()),
                                        )
                                },
                            }
                            div { class: "row",
                                RowInfo {
                                    label: "Container port",
                                    desc: "The port AEM listens on inside the image.".to_string(),
                                }
                                input {
                                    class: "settings-input-number",
                                    r#type: "number",
                                    placeholder: "8080",
                                    value: "{s.aem_verify.container_port}",
                                    onchange: move |e: Event<FormData>| {
                                        if let Ok(v) = e.value().parse::<u16>() {
                                            update.call(Box::new(move |s| s.aem_verify.container_port = v));
                                        }
                                    },
                                }
                            }
                            TextRow {
                                label: "AEM Username",
                                desc: "The AEM admin login inside the verifier's container.",
                                value: s.aem_verify.user.clone(),
                                placeholder: "admin",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.aem_verify.user = v.trim().to_string()))
                                },
                            }
                            TextRow {
                                label: "AEM Password",
                                desc: "The AEM admin password inside the verifier's container. Stored locally on disk.",
                                value: s.aem_verify.password.clone(),
                                placeholder: "••••••••",
                                secret: true,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.aem_verify.password = v))
                                },
                            }
                            TextRow {
                                label: "Platform",
                                desc: "Empty uses the default.",
                                value: s.aem_verify.platform.clone(),
                                placeholder: "linux/amd64",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.aem_verify.platform = v.trim().to_string()))
                                },
                            }
                            TextRow {
                                label: "Redacto URL",
                                desc: "Only for a Redacto renderer running outside AEM.",
                                value: s.aem_verify.redacto_url.clone(),
                                placeholder: "",
                                secret: false,
                                on_change: move |v: String| {
                                    update
                                        .call(
                                            Box::new(move |s| s.aem_verify.redacto_url = v.trim().to_string()),
                                        )
                                },
                            }
                            TextRow {
                                label: "Mandator",
                                desc: "Optional.",
                                value: s.aem_verify.mandator.clone(),
                                placeholder: "",
                                secret: false,
                                on_change: move |v: String| {
                                    update
                                        .call(Box::new(move |s| s.aem_verify.mandator = v.trim().to_string()))
                                },
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Redacto verification" }
                            TextRow {
                                label: "Postgres image",
                                desc: "The public Postgres image dumps are imported into.",
                                value: s.redacto_verify.postgres_image.clone(),
                                placeholder: "postgres:16-alpine",
                                secret: false,
                                on_change: move |v: String| {
                                    update
                                        .call(
                                            Box::new(move |s| {
                                                s.redacto_verify.postgres_image = v.trim().to_string()
                                            }),
                                        )
                                },
                            }
                            TextRow {
                                label: "Rendering URL",
                                desc: "Empty skips rendering; the import check still runs.",
                                value: s.redacto_verify.rendering_url.clone(),
                                placeholder: "",
                                secret: false,
                                on_change: move |v: String| {
                                    update
                                        .call(
                                            Box::new(move |s| {
                                                s.redacto_verify.rendering_url = v.trim().to_string()
                                            }),
                                        )
                                },
                            }
                            TextRow {
                                label: "Username",
                                desc: "Basic auth username for the Redacto platform.",
                                value: s.redacto_verify.user.clone(),
                                placeholder: "admin",
                                secret: false,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.redacto_verify.user = v.trim().to_string()))
                                },
                            }
                            TextRow {
                                label: "Password",
                                desc: "Basic auth password. Stored locally on disk.",
                                value: s.redacto_verify.password.clone(),
                                placeholder: "••••••••",
                                secret: true,
                                on_change: move |v: String| {
                                    update.call(Box::new(move |s| s.redacto_verify.password = v))
                                },
                            }
                        }
                        div { class: "settings-section",
                            h3 { class: "settings-section-title", "Verifier tooling" }
                            div { class: "row",
                                RowInfo {
                                    label: "Pull images",
                                    desc: "Download the public verifier images (headless Chrome, Postgres) so a run never waits on the network. The AEM image is private and has to be pulled by hand after `az acr login`.".to_string(),
                                }
                                button {
                                    class: "btn btn-secondary btn-sm",
                                    disabled: verify_busy(),
                                    onclick: move |_| {
                                        let s = settings.read();
                                        let images = [
                                            "chromedp/headless-shell:stable".to_string(),
                                            s.redacto_verify.postgres_image.clone(),
                                        ];
                                        let platform = {
                                            let p = s.aem_verify.platform.trim();
                                            if p.is_empty() { "linux/amd64".to_string() } else { p.to_string() }
                                        };
                                        verify_busy.set(true);
                                        verify_status.set(None);
                                        spawn(async move {
                                            let refs: Vec<&str> = images.iter().map(String::as_str).collect();
                                            let result = agent::u2s::pull_public_images(&refs, &platform)
                                                .await
                                                .map(|()| "Images pulled.".to_string());
                                            verify_status.set(Some(result));
                                            verify_busy.set(false);
                                        });
                                    },
                                    if verify_busy() { "Pulling…" } else { "Pull images" }
                                }
                                button {
                                    class: "btn btn-primary btn-sm",
                                    disabled: verify_busy(),
                                    onclick: move |_| {
                                        let s = settings.read();
                                        let aem_verify = s.aem_verify.clone();
                                        let redacto_verify = s.redacto_verify.clone();
                                        verify_busy.set(true);
                                        verify_status.set(None);
                                        spawn(async move {
                                            let aem = agent::u2s::aem_verify_readiness(&aem_verify).await;
                                            let redacto = agent::u2s::redacto_verify_readiness(
                                                    &redacto_verify,
                                                )
                                                .await;
                                            let report = match (&aem, &redacto) {
                                                (Ok(a), Ok(r)) => Ok(format!("AEM: {a}\nRedacto: {r}")),
                                                _ => {
                                                    let mut problems = Vec::new();
                                                    if let Err(e) = &aem {
                                                        problems.push(format!("AEM: {e}"));
                                                    }
                                                    if let Err(e) = &redacto {
                                                        problems.push(format!("Redacto: {e}"));
                                                    }
                                                    Err(problems.join("\n"))
                                                }
                                            };
                                            verify_status.set(Some(report));
                                            verify_busy.set(false);
                                        });
                                    },
                                    if verify_busy() { "Checking…" } else { "Check" }
                                }
                            }
                            match verify_status.read().as_ref() {
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
