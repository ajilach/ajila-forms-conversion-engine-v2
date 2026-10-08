//! Whether this machine can run conversions, checked at startup and whenever
//! the settings change, so a missing Docker, image or registry login shows up
//! before a run is started rather than as its refusal.

use dioxus::prelude::*;

use agent::OutputTarget;
use agent::u2s::NotReady;

use crate::settings::AppSettings;

/// Both targets' readiness: the check rules' sandbox and the verifier, each
/// as `agent::u2s::readiness` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Readiness {
    pub aem: Result<String, NotReady>,
    pub redacto: Result<String, NotReady>,
}

impl Readiness {
    /// Checks both targets at once against `settings`.
    pub async fn check(settings: AppSettings) -> Self {
        let (aem, redacto) = tokio::join!(
            agent::u2s::readiness(OutputTarget::Aem, &settings.aem_verify, &settings.redacto_verify),
            agent::u2s::readiness(OutputTarget::Redacto, &settings.aem_verify, &settings.redacto_verify),
        );
        Self { aem, redacto }
    }

    pub fn of(&self, target: OutputTarget) -> &Result<String, NotReady> {
        match target {
            OutputTarget::Aem => &self.aem,
            OutputTarget::Redacto => &self.redacto,
        }
    }

    /// Every target with its result, in display order.
    fn targets(&self) -> [(&'static str, &Result<String, NotReady>); 2] {
        [("AEM", &self.aem), ("Redacto", &self.redacto)]
    }

    /// One text for both targets: their reports when both are ready, otherwise
    /// every problem, prefixed with its target.
    pub fn report(&self) -> Result<String, String> {
        let problems: Vec<String> = self
            .targets()
            .into_iter()
            .filter_map(|(name, result)| result.as_ref().err().map(|e| format!("{name}: {e}")))
            .collect();
        if !problems.is_empty() {
            return Err(problems.join("\n"));
        }
        Ok(self
            .targets()
            .into_iter()
            .filter_map(|(name, result)| result.as_ref().ok().map(|r| format!("{name}: {r}")))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Lists what keeps each target from running, under the header. Renders
/// nothing while every target is ready, or before the first check finishes.
#[component]
pub fn EnvironmentBanner(readiness: Resource<Readiness>) -> Element {
    let Some(current) = readiness.value().read().clone() else {
        return rsx! {};
    };
    let failing: Vec<(&str, Vec<String>)> = current
        .targets()
        .into_iter()
        .filter_map(|(name, result)| result.as_ref().err().map(|NotReady(problems)| (name, problems.clone())))
        .collect();
    if failing.is_empty() {
        return rsx! {};
    }
    let checking = readiness.pending();
    rsx! {
        div { class: "environment-banner",
            div { class: "environment-banner-head",
                strong { "This machine is not ready to run every conversion" }
                button {
                    class: "btn btn-secondary btn-sm",
                    disabled: checking,
                    onclick: move |_| readiness.restart(),
                    if checking { "Checking…" } else { "Re-check" }
                }
            }
            for (name, problems) in failing {
                div { class: "environment-banner-target",
                    span { class: "environment-banner-name", "{name}" }
                    ul {
                        for problem in problems {
                            li { "{problem}" }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn not_ready(problems: &[&str]) -> Result<String, NotReady> {
        Err(NotReady(problems.iter().map(|p| p.to_string()).collect()))
    }

    #[test]
    fn a_ready_machine_reports_both_targets() {
        let readiness = Readiness { aem: Ok("a".into()), redacto: Ok("r".into()) };
        assert_eq!(readiness.report(), Ok("AEM: a\nRedacto: r".into()));
    }

    /// One target's problems are reported alone, the ready one's report is
    /// not mixed in, and each problem keeps its own line.
    #[test]
    fn only_the_failing_targets_problems_are_reported() {
        let readiness = Readiness {
            aem: Ok("a".into()),
            redacto: not_ready(&["Docker is not reachable", "the image x is missing"]),
        };
        assert_eq!(
            readiness.report(),
            Err("Redacto: Docker is not reachable\nthe image x is missing".into())
        );
        assert!(readiness.of(OutputTarget::Aem).is_ok());
        assert!(readiness.of(OutputTarget::Redacto).is_err());
    }
}
