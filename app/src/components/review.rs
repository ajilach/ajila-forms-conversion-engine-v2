//! The review of a finished conversion: the source rendered page by page on the
//! left, and on the right what the run's output looked like on its verifier at
//! the end of the run (the form's screenshots, then the output PDF's pages).
//!
//! The output side was captured by the run and lives in the history store
//! under the tab's session, so a tab reopened after a restart can still be
//! reviewed. The source side is rendered when the page opens.

use std::sync::Arc;

use base64::Engine;
use dioxus::prelude::*;

use super::page::FullPage;
use super::spinner::Spinner;
use crate::workspace::Tab;
use agent::review::{ReviewImage, ReviewImages};

/// One image, ready for an `img` tag.
struct Shown {
    label: String,
    src: String,
}

/// A list of images as a prop: a data URI is megabytes long, so cloning and
/// comparing one is a pointer copy and a pointer comparison.
#[derive(Clone)]
struct Images(Arc<[Shown]>);

impl PartialEq for Images {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Images {
    fn of(images: &[ReviewImage]) -> Self {
        Self(
            images
                .iter()
                .map(|image| Shown {
                    label: image.label.clone(),
                    src: format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&image.png)
                    ),
                })
                .collect(),
        )
    }
}

/// The output side, as the page shows it.
struct ShownOutput {
    form: Images,
    output: Images,
}

impl ShownOutput {
    fn of(images: &ReviewImages) -> Self {
        Self {
            form: Images::of(&images.form),
            output: Images::of(&images.output),
        }
    }
}

/// Runs `work` off the UI thread; a panic becomes an error.
async fn off_ui<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

#[component]
pub fn ReviewPage(tab: Tab, on_close: EventHandler<()>) -> Element {
    let files = tab.files;
    let session = tab.session_id;

    let input = use_resource(move || {
        let files = files.read().clone();
        async move { off_ui(move || agent::review::render_sources(&files).map(|images| Images::of(&images))).await }
    });
    let output = use_resource(move || {
        let session = session.read().clone();
        async move {
            off_ui(move || {
                let session = session.ok_or("This conversion has no recorded session.")?;
                let images = crate::db::load_review(&session)
                    .ok_or("No output images were captured for this conversion.")?;
                Ok(ShownOutput::of(&images))
            })
            .await
        }
    });

    rsx! {
        FullPage {
            title: "Review",
            subtitle: tab.state.read().form_code.clone(),
            on_close,
            div { class: "review-columns",
                section { class: "review-col",
                    h3 { class: "review-col-title", "Input" }
                    match &*input.read() {
                        None => rsx! {
                            ReviewLoading { what: "Rendering the source" }
                        },
                        Some(Err(e)) => rsx! {
                            div { class: "progress-error", "{e}" }
                        },
                        Some(Ok(images)) if images.0.is_empty() => rsx! {
                            div { class: "progress-note", "This conversion has no source PDF to show." }
                        },
                        Some(Ok(images)) => rsx! {
                            ReviewImageList { images: images.clone() }
                        },
                    }
                }
                section { class: "review-col",
                    h3 { class: "review-col-title", "Output" }
                    match &*output.read() {
                        None => rsx! {
                            ReviewLoading { what: "Loading the output" }
                        },
                        Some(Err(e)) => rsx! {
                            div { class: "progress-note", "{e}" }
                        },
                        Some(Ok(shown)) => rsx! {
                            if !shown.form.0.is_empty() {
                                h4 { class: "review-group-title", "Form" }
                                ReviewImageList { images: shown.form.clone() }
                            }
                            if !shown.output.0.is_empty() {
                                h4 { class: "review-group-title", "Output PDF" }
                                ReviewImageList { images: shown.output.clone() }
                            }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn ReviewLoading(what: &'static str) -> Element {
    rsx! {
        div { class: "review-loading",
            Spinner {}
            span { "{what}…" }
        }
    }
}

#[component]
fn ReviewImageList(images: Images) -> Element {
    rsx! {
        for (index , image) in images.0.iter().enumerate() {
            figure { key: "{index}", class: "review-image",
                img { src: "{image.src}", alt: "{image.label}" }
                figcaption { "{image.label}" }
            }
        }
    }
}
