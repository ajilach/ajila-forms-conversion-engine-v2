//! The output-target picker shown next to the profile picker.
//!
//! The target has to be chosen *before* a run starts: it decides what the
//! conversion agent authors (an AEM adaptive-form tree, or the structured
//! document a Redacto dump is generated from), not merely which file is offered
//! for download at the end.

use dioxus::prelude::*;

use agent::OutputTarget;

/// DOM id of the `<select>`, so the label can point at it.
const SELECT_ID: &str = "agent-target-select";

/// A `<select>` over the output targets the chosen profile supports and the
/// settings offer.
///
/// Renders nothing when fewer than two remain — there is no choice to make,
/// and an empty picker is just noise.
#[component]
pub fn OutputTargetSelector(
    /// The currently selected profile; its sections decide the options.
    profile: Option<String>,
    /// The targets the settings offer (the others are switched off).
    enabled: Vec<OutputTarget>,
    selected_target: Signal<OutputTarget>,
    disabled: bool,
) -> Element {
    let supported = profile
        .as_deref()
        .map(agent::profiles::profile_targets)
        .unwrap_or_default();
    let targets = offered_targets(&supported, &enabled);

    // Switching to a profile that does not support the current target would
    // otherwise leave a selection the run cannot honour.
    if !targets.is_empty() && !targets.contains(&selected_target.read()) {
        selected_target.set(targets[0]);
    }

    if targets.len() < 2 {
        return rsx! {};
    }

    rsx! {
        div { class: "profile-selector",
            label { r#for: SELECT_ID, "Output" }
            select {
                id: SELECT_ID,
                disabled,
                onchange: move |evt: Event<FormData>| {
                    if let Some(target) = OutputTarget::parse(&evt.value()) {
                        selected_target.set(target);
                    }
                },
                for target in targets.iter().copied() {
                    option {
                        value: "{target.as_str()}",
                        selected: *selected_target.read() == target,
                        "{target.label()}"
                    }
                }
            }
        }
    }
}

/// The profile's targets that the settings offer. When the settings switch
/// off every target the profile supports, the profile's own list stands: the
/// readiness check then says what is missing, rather than leaving a profile
/// with nothing to pick.
fn offered_targets(supported: &[OutputTarget], enabled: &[OutputTarget]) -> Vec<OutputTarget> {
    let offered: Vec<OutputTarget> =
        supported.iter().copied().filter(|target| enabled.contains(target)).collect();
    if offered.is_empty() { supported.to_vec() } else { offered }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OutputTarget::{Aem, Redacto};

    #[test]
    fn a_switched_off_target_is_not_offered() {
        assert_eq!(offered_targets(&[Aem, Redacto], &[Aem, Redacto]), vec![Aem, Redacto]);
        assert_eq!(offered_targets(&[Aem, Redacto], &[Aem]), vec![Aem]);
        assert_eq!(offered_targets(&[Aem, Redacto], &[Redacto]), vec![Redacto]);
    }

    #[test]
    fn a_profile_whose_targets_are_all_off_keeps_its_own() {
        assert_eq!(offered_targets(&[Aem], &[Redacto]), vec![Aem]);
        assert_eq!(offered_targets(&[], &[Aem]), Vec::<OutputTarget>::new());
    }
}
