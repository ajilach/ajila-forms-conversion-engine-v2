//! Whether this machine can run conversions, checked at startup and whenever
//! the settings change, so a missing Docker, image or registry login shows up
//! before a run is started rather than as its refusal.
//!
//! Only the output targets the operator uses are checked: a target switched
//! off in Settings (or from the banner) is not offered, so what it would need
//! is not reported either.

use dioxus::prelude::*;

use agent::OutputTarget;
use agent::u2s::NotReady;

use crate::settings::AppSettings;

/// Each target's readiness as `agent::u2s::readiness` reports it, or `None`
/// for a target switched off in the settings, which is not checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Readiness {
    pub aem: Option<Result<String, NotReady>>,
    pub redacto: Option<Result<String, NotReady>>,
}

impl Readiness {
    /// Checks every target `settings` offers, at once.
    pub async fn check(settings: AppSettings) -> Self {
        let check = |target: OutputTarget| {
            let settings = settings.clone();
            async move {
                if !settings.target_enabled(target) {
                    return None;
                }
                Some(agent::u2s::readiness(target, &settings.aem_verify, &settings.redacto_verify).await)
            }
        };
        let (aem, redacto) = tokio::join!(check(OutputTarget::Aem), check(OutputTarget::Redacto));
        Self { aem, redacto }
    }

    /// `target`'s result, or `None` when it is switched off.
    pub fn of(&self, target: OutputTarget) -> Option<&Result<String, NotReady>> {
        match target {
            OutputTarget::Aem => self.aem.as_ref(),
            OutputTarget::Redacto => self.redacto.as_ref(),
        }
    }

    /// Every checked target with its result, in display order.
    fn targets(&self) -> Vec<(OutputTarget, &Result<String, NotReady>)> {
        OutputTarget::ALL
            .into_iter()
            .filter_map(|target| self.of(target).map(|result| (target, result)))
            .collect()
    }

    /// One text for the checked targets: their reports when all are ready,
    /// otherwise every problem, prefixed with its target. A target switched
    /// off is named as such, so the report does not read as if it were ready.
    pub fn report(&self) -> Result<String, String> {
        let problems: Vec<String> = self
            .targets()
            .into_iter()
            .filter_map(|(target, result)| {
                result.as_ref().err().map(|e| format!("{}: {e}", name(target)))
            })
            .collect();
        if !problems.is_empty() {
            return Err(problems.join("\n"));
        }
        Ok(OutputTarget::ALL
            .into_iter()
            .map(|target| match self.of(target) {
                Some(Ok(report)) => format!("{}: {report}", name(target)),
                _ => format!("{}: switched off in Settings, not checked", name(target)),
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// The short name the banner and the report give a target.
fn name(target: OutputTarget) -> &'static str {
    match target {
        OutputTarget::Aem => "AEM",
        OutputTarget::Redacto => "Redacto",
    }
}

/// Lists what keeps each checked target from running, under the header.
/// Renders nothing while every checked target is ready, or before the first
/// check finishes. Each failing target can be switched off from here when it
/// is not used (`on_switch_off`), as long as another target stays on.
#[component]
pub fn EnvironmentBanner(
    readiness: Resource<Readiness>,
    /// Targets currently offered; one that is the last cannot be switched off.
    enabled_targets: Vec<OutputTarget>,
    /// Switches a target off in the settings (persisted by the caller).
    on_switch_off: EventHandler<OutputTarget>,
) -> Element {
    let Some(current) = readiness.value().read().clone() else {
        return rsx! {};
    };
    let failing: Vec<(OutputTarget, Vec<String>)> = current
        .targets()
        .into_iter()
        .filter_map(|(target, result)| {
            result.as_ref().err().map(|NotReady(problems)| (target, problems.clone()))
        })
        .collect();
    if failing.is_empty() {
        return rsx! {};
    }
    let checking = readiness.pending();
    let can_switch_off = enabled_targets.len() > 1;
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
            for (target, problems) in failing {
                div { class: "environment-banner-target",
                    span { class: "environment-banner-name", "{name(target)}" }
                    ul {
                        for problem in problems {
                            li { "{problem}" }
                        }
                    }
                    if can_switch_off {
                        button {
                            class: "btn btn-secondary btn-sm environment-banner-off",
                            title: "Stop offering this output format and stop checking for it. Switch it back on under Settings > Verification > Output formats.",
                            onclick: move |_| on_switch_off.call(target),
                            "I don't use {name(target)}"
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

    fn not_ready(problems: &[&str]) -> Option<Result<String, NotReady>> {
        Some(Err(NotReady(problems.iter().map(|p| p.to_string()).collect())))
    }

    #[test]
    fn a_ready_machine_reports_both_targets() {
        let readiness = Readiness { aem: Some(Ok("a".into())), redacto: Some(Ok("r".into())) };
        assert_eq!(readiness.report(), Ok("AEM: a\nRedacto: r".into()));
    }

    /// One target's problems are reported alone, the ready one's report is
    /// not mixed in, and each problem keeps its own line.
    #[test]
    fn only_the_failing_targets_problems_are_reported() {
        let readiness = Readiness {
            aem: Some(Ok("a".into())),
            redacto: not_ready(&["Docker is not reachable", "the image x is missing"]),
        };
        assert_eq!(
            readiness.report(),
            Err("Redacto: Docker is not reachable\nthe image x is missing".into())
        );
        assert!(readiness.of(OutputTarget::Aem).is_some_and(|r| r.is_ok()));
        assert!(readiness.of(OutputTarget::Redacto).is_some_and(|r| r.is_err()));
    }

    /// A switched-off target is not checked, so its missing images are no
    /// problem, and the report says it was skipped rather than ready.
    #[test]
    fn a_switched_off_target_is_neither_a_problem_nor_ready() {
        let readiness = Readiness { aem: Some(Ok("a".into())), redacto: None };
        assert_eq!(
            readiness.report(),
            Ok("AEM: a\nRedacto: switched off in Settings, not checked".into())
        );
        assert!(readiness.of(OutputTarget::Redacto).is_none());
        assert_eq!(readiness.targets().len(), 1);
    }
}
