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

/// A `<select>` over the output targets the chosen profile supports.
///
/// Renders nothing when the profile supports fewer than two — there is no
/// choice to make, and an empty picker is just noise.
#[component]
pub fn OutputTargetSelector(
    /// The currently selected profile; its sections decide the options.
    profile: Option<String>,
    selected_target: Signal<OutputTarget>,
    disabled: bool,
) -> Element {
    let targets = profile
        .as_deref()
        .map(agent::profiles::profile_targets)
        .unwrap_or_default();

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
