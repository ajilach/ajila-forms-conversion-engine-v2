//! Shared chrome for the full-page views (settings, reference forms): the page
//! shell, its header with a Close button, the scrolling content area, and the
//! label/description column every settings-style row starts with.

use dioxus::prelude::*;

/// The label + description column of a `.row`, on either full-page view.
#[component]
pub fn RowInfo(label: &'static str, desc: String) -> Element {
    rsx! {
        div { class: "row-info",
            span { class: "row-label", "{label}" }
            span { class: "row-desc", "{desc}" }
        }
    }
}

/// The tab bar of a full-page view.
///
/// Both full-page views used to carry their own copy of this markup, which is
/// how the two drifted apart in the first place. The labels are passed as
/// strings rather than an enum because the references page counts its entries
/// into them.
///
/// Not shared with the form-tab strip: that one carries a status dot, a close
/// button, an add button and horizontal scrolling, and bending this into all of
/// that would leave a component made of holes.
#[component]
pub fn PageTabs(labels: Vec<String>, active: usize, on_select: EventHandler<usize>) -> Element {
    rsx! {
        div { class: "tabs",
            for (index , label) in labels.into_iter().enumerate() {
                button {
                    key: "{index}",
                    class: if index == active { "tab active" } else { "tab" },
                    onclick: move |_| on_select.call(index),
                    "{label}"
                }
            }
        }
    }
}

/// A full-page view under the persistent app header.
#[component]
pub fn FullPage(
    title: &'static str,
    /// Optional line under the title, e.g. a summary count.
    subtitle: Option<String>,
    on_close: EventHandler<()>,
    /// Tab bar and content — everything below the header.
    children: Element,
) -> Element {
    rsx! {
        div { class: "page",
            div { class: "page-header",
                div {
                    h2 { "{title}" }
                    if let Some(subtitle) = subtitle.as_ref() {
                        span { class: "page-subtitle", "{subtitle}" }
                    }
                }
                button {
                    class: "btn btn-secondary",
                    onclick: move |_| on_close.call(()),
                    "✕ Close"
                }
            }
            {children}
        }
    }
}
