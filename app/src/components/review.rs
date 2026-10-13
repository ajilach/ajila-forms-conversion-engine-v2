//! The review of a finished conversion: the source rendered page by page on the
//! left, and on the right what the run's output looked like on its verifier at
//! the end of the run (the form's screenshots, then the output PDF's pages),
//! both in the language picked in the tabs above them.
//!
//! The output side was captured by the run and lives in the history store
//! under the tab's session, so a tab reopened after a restart can still be
//! reviewed. The source side is rendered when the page opens.

use std::sync::Arc;

use base64::Engine;
use dioxus::prelude::*;

use super::page::{FullPage, PageTabs};
use super::spinner::Spinner;
use crate::workspace::Tab;
use agent::review::{Review, ReviewImage, SourceReview};

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

/// One source PDF, as the page shows it.
struct ShownSource {
    /// `None` for a PDF that declares no language: it is shown in every one.
    language: Option<String>,
    pages: Images,
}

/// The input side: every source PDF, rendered once and filtered by language.
#[derive(Clone)]
struct ShownSources(Arc<[ShownSource]>);

impl ShownSources {
    fn of(sources: &[SourceReview]) -> Self {
        Self(
            sources
                .iter()
                .map(|source| ShownSource {
                    language: source.language.clone(),
                    pages: Images::of(&source.pages),
                })
                .collect(),
        )
    }

    /// The pages of the sources in `language`, and of those that declare none.
    /// An unknown language (images captured before languages were recorded)
    /// shows every source.
    fn pages_in(&self, language: &str) -> Vec<Images> {
        self.0
            .iter()
            .filter(|source| language.is_empty() || source.language.as_deref().is_none_or(|l| l == language))
            .map(|source| source.pages.clone())
            .collect()
    }
}

/// The output side in one language, as the page shows it.
struct ShownLanguage {
    language: String,
    form: Images,
    output: Images,
}

#[derive(Clone)]
struct ShownOutput(Arc<[ShownLanguage]>);

impl ShownOutput {
    fn of(review: &Review) -> Self {
        Self(
            review
                .languages
                .iter()
                .map(|language| ShownLanguage {
                    language: language.language.clone(),
                    form: Images::of(&language.images.form),
                    output: Images::of(&language.images.output),
                })
                .collect(),
        )
    }

    fn in_language(&self, language: &str) -> Option<&ShownLanguage> {
        self.0.iter().find(|shown| shown.language == language)
    }
}

/// The languages the page offers: the output's, in the form's order, then any
/// only a source has.
fn languages(output: Option<&ShownOutput>, sources: Option<&ShownSources>) -> Vec<String> {
    let mut languages: Vec<String> = output.into_iter().flat_map(|o| o.0.iter().map(|l| l.language.clone())).collect();
    for language in sources.into_iter().flat_map(|s| s.0.iter().filter_map(|s| s.language.clone())) {
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    languages
}

/// A language as its tab names it.
fn language_label(language: &str) -> String {
    if language.is_empty() {
        "Unknown language".to_string()
    } else {
        language.to_uppercase()
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
    // The language picked; until then, the first one offered.
    let mut picked = use_signal(|| None::<String>);

    let input = use_resource(move || {
        let files = files.read().clone();
        async move { off_ui(move || agent::review::render_sources(&files).map(|sources| ShownSources::of(&sources))).await }
    });
    let output = use_resource(move || {
        let session = session.read().clone();
        async move {
            off_ui(move || {
                let session = session.ok_or("This conversion has no recorded session.")?;
                let review = crate::db::load_review(&session)
                    .ok_or("No output images were captured for this conversion.")?;
                Ok(ShownOutput::of(&review))
            })
            .await
        }
    });

    // Each read once, and what is rendered copied out of it.
    let input = input.read().clone();
    let output = output.read().clone();
    let sources = input.as_ref().and_then(|r| r.as_ref().ok()).cloned();
    let shown_output = output.as_ref().and_then(|r| r.as_ref().ok()).cloned();
    let languages = languages(shown_output.as_ref(), sources.as_ref());
    let picked_language = picked.read().clone();
    let active = picked_language
        .and_then(|picked| languages.iter().position(|l| *l == picked))
        .unwrap_or(0);
    let language = languages.get(active).cloned();
    let labels: Vec<String> = languages.iter().map(|l| language_label(l)).collect();

    rsx! {
        FullPage {
            title: "Review",
            subtitle: tab.state.read().form_code.clone(),
            on_close,
            if languages.len() > 1 {
                PageTabs {
                    labels,
                    active,
                    on_select: move |index: usize| picked.set(languages.get(index).cloned()),
                }
            }
            div { class: "review-columns",
                section { class: "review-col",
                    h3 { class: "review-col-title", "Input" }
                    match (&input, &sources) {
                        (None, _) => rsx! {
                            ReviewLoading { what: "Rendering the source" }
                        },
                        (Some(Err(e)), _) => rsx! {
                            div { class: "progress-error", "{e}" }
                        },
                        (Some(Ok(_)), Some(sources)) => {
                            let pages = sources.pages_in(language.as_deref().unwrap_or_default());
                            rsx! {
                                if pages.is_empty() {
                                    div { class: "progress-note", "There is no source PDF in this language." }
                                }
                                for (index , images) in pages.into_iter().enumerate() {
                                    ReviewImageList { key: "{index}", images }
                                }
                            }
                        }
                        (Some(Ok(_)), None) => rsx! {},
                    }
                }
                section { class: "review-col",
                    h3 { class: "review-col-title", "Output" }
                    match (&output, &shown_output) {
                        (None, _) => rsx! {
                            ReviewLoading { what: "Loading the output" }
                        },
                        (Some(Err(e)), _) => rsx! {
                            div { class: "progress-note", "{e}" }
                        },
                        (Some(Ok(_)), Some(shown)) => match language.as_deref().and_then(|l| shown.in_language(l)) {
                            None => rsx! {
                                div { class: "progress-note", "No output was captured in this language." }
                            },
                            Some(shown) => rsx! {
                                if !shown.form.0.is_empty() {
                                    h4 { class: "review-group-title", "Form" }
                                    ReviewImageList { images: shown.form.clone() }
                                }
                                if !shown.output.0.is_empty() {
                                    h4 { class: "review-group-title", "Output PDF" }
                                    ReviewImageList { images: shown.output.clone() }
                                }
                            },
                        },
                        (Some(Ok(_)), None) => rsx! {},
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
