//! Whether this machine can run conversions, checked at startup and whenever
//! the settings change, so a missing Docker, image or registry login shows up
//! before a run is started rather than as its refusal.

use dioxus::prelude::*;

use agent::u2s::NotReady;

use crate::settings::AppSettings;

/// The readiness `agent::u2s::readiness` reports: the check rules and the AEM
/// verifier.
#[derive(Clone, Debug, PartialEq)]
pub struct Readiness(pub Result<String, NotReady>);

impl Readiness {
    pub async fn check(settings: AppSettings) -> Self {
        Self(agent::u2s::readiness(&settings.aem_verify).await)
    }

    pub fn is_ready(&self) -> bool {
        self.0.is_ok()
    }

    /// The report when ready, otherwise every problem, one per line.
    pub fn report(&self) -> Result<String, String> {
        self.0.clone().map_err(|e| e.to_string())
    }
}

/// Lists what keeps this machine from running a conversion, under the header.
/// Renders nothing while it is ready, or before the first check finishes.
#[component]
pub fn EnvironmentBanner(readiness: Resource<Readiness>) -> Element {
    let Some(Readiness(Err(NotReady(problems)))) = readiness.value().read().clone() else {
        return rsx! {};
    };
    let checking = readiness.pending();
    rsx! {
        div { class: "environment-banner",
            div { class: "environment-banner-head",
                strong { "This machine is not ready to run a conversion" }
                button {
                    class: "btn btn-secondary btn-sm",
                    disabled: checking,
                    onclick: move |_| readiness.restart(),
                    if checking { "Checking…" } else { "Re-check" }
                }
            }
            ul {
                for problem in problems {
                    li { "{problem}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_machine_reports_what_it_checked() {
        let readiness = Readiness(Ok("a".into()));
        assert!(readiness.is_ready());
        assert_eq!(readiness.report(), Ok("a".into()));
    }

    /// Each problem keeps its own line.
    #[test]
    fn every_problem_is_reported_on_its_own_line() {
        let readiness = Readiness(Err(NotReady(vec![
            "Docker is not reachable".into(),
            "the image x is missing".into(),
        ])));
        assert!(!readiness.is_ready());
        assert_eq!(readiness.report(), Err("Docker is not reachable\nthe image x is missing".into()));
    }
}
