//! Reaching the default state, at three levels of faithfulness.
//!
//! "Render the form as it opens" is not one operation — the XFA template is
//! only the starting point, and how close you get to what a user would see
//! depends on how much of the form's own machinery you are willing to run.
//!
//! Level 3 is what a caller normally wants. Levels 1 and 2 exist because a
//! form whose init script throws should still render *something*, with the
//! degradation reported rather than swallowed.

use std::collections::HashMap;

use crate::flattened::{Flattened, PageOverrides};
use crate::xfa::XfaNode;
use crate::xfa::script_executor::ScriptExecutor;
use crate::{XfaError, xfa::scripting::SomPath};

/// How much of the form's machinery was actually run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// The template as authored. Script-hidden sections are still visible and
    /// script-computed values are empty.
    Template,
    /// Plus the Form DOM merges: dropdown items, visibility and access that
    /// Designer baked into the saved packet. No JavaScript executed.
    FormDom,
    /// Plus the form's own init and calculate scripts, run to a fixpoint. The
    /// authoritative default state.
    Scripts,
}

/// A flattened form together with how faithfully it was produced.
// `Flattened` is not Debug upstream and making it so would be a large diff for
// no benefit, so this isn't either.
pub struct Prepared {
    pub flattened: Flattened,
    pub fidelity: Fidelity,
    /// Set when the intended fidelity could not be reached — the caller should
    /// surface this, because the render is real but incomplete.
    pub warning: Option<String>,
}

/// Level 2: the Form DOM merges. These are associated functions on `Flattened`
/// upstream and involve no JavaScript, so they are always safe to apply.
fn apply_form_dom_merges(
    nodes: &mut [XfaNode],
    presence: &[crate::xfa::script_executor::PresenceChange],
) {
    Flattened::merge_form_items_into_template(nodes);
    Flattened::merge_form_presence_into_template(nodes, presence);
    Flattened::merge_form_access_into_template(nodes);
}

/// Produce the default state at the highest fidelity that succeeds.
///
/// This replicates upstream's `XfaForm::new` prelude — script execution, the
/// presence application, the three Form DOM merges, then flatten with the
/// computed values — and deliberately stops there. `XfaForm` continues by
/// building a *persistent* script engine and mirroring the whole SOM hierarchy
/// as JS objects, which only interactive events need; for rendering it is pure
/// overhead.
///
/// A panicking or failing script degrades to level 2 rather than failing the
/// render: a broken script in one field should not make the entire document
/// invisible. It must never be silent, though, so the fallback is reported.
pub fn prepare_default(nodes: &[XfaNode]) -> Result<Prepared, XfaError> {
    let mut working = nodes.to_vec();
    // The Form DOM of an empty merge: each repeatable subform as its
    // `initial` instances (XFA 3.3 §9), before any script sees the form.
    let prototypes = crate::xfa::instances::materialize_initial_instances(&mut working);

    // Scripts run arbitrary JavaScript from an untrusted document. A panic here
    // must not take the process down, so it is contained and treated as "this
    // form's scripts did not run" rather than as a fatal error. The pass runs
    // on a copy, since its scripts may add or remove instances: a panic
    // part-way leaves `working` as it was.
    let executed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut form_dom = working.clone();
        let mut prototypes = prototypes.clone();
        let outcome = ScriptExecutor::execute_with_layout(&mut form_dom, &mut prototypes);
        (outcome, form_dom)
    }));

    match executed {
        Ok(((result, layout), form_dom)) => {
            working = form_dom;
            ScriptExecutor::apply_presence_changes(&mut working, &result.presence_changes);
            // As `XfaForm::new_with_layout` does, so a stateless render draws
            // a script-locked field the way a session does.
            ScriptExecutor::apply_access_changes(&mut working, &result.access_changes);
            apply_form_dom_merges(&mut working, &result.presence_changes);

            // Master-page scripts that depend on the page ("Pagina 2 di 3", a
            // first-page-only block) can only be evaluated once the page count
            // is known, which is a result of laying the body out. The flattener
            // calls back into the still-live engine as it reaches each page.
            let mut warning = None;
            let flattened = match layout {
                Some(mut layout) => {
                    let mut failed = false;
                    let flattened = Flattened::from_xfa_paged(
                        &working,
                        &result.computed_values,
                        &mut |page_area, page_index, page_count| {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                layout.evaluate(page_area, page_index, page_count)
                            }))
                            .unwrap_or_else(|_| {
                                failed = true;
                                PageOverrides::default()
                            })
                        },
                    )
                    .map_err(XfaError::Layout)?;
                    if failed {
                        warning = Some(
                            "this form's page-dependent master-page scripts could not \
                             be evaluated; headers and footers may show the first \
                             page's values on every page"
                                .to_string(),
                        );
                    }
                    flattened
                }
                None => Flattened::from_xfa(&working, &result.computed_values)
                    .map_err(XfaError::Layout)?,
            };

            Ok(Prepared {
                flattened,
                fidelity: Fidelity::Scripts,
                warning,
            })
        }
        Err(_) => {
            // Fall back to level 2: the merges still apply, with no script
            // presence changes to skip.
            apply_form_dom_merges(&mut working, &[]);
            let empty: HashMap<SomPath, String> = HashMap::new();
            let flattened = Flattened::from_xfa(&working, &empty).map_err(XfaError::Layout)?;
            Ok(Prepared {
                flattened,
                fidelity: Fidelity::FormDom,
                warning: Some(
                    "this form's scripts could not be executed; rendered from the template \
                     and its saved Form DOM state instead, so script-computed values are \
                     missing and script-hidden sections may still be visible"
                        .to_string(),
                ),
            })
        }
    }
}

/// Level 1: the template alone, no scripts and no merges. Exposed for tests
/// and diagnosis — it is the cheapest path and the least faithful.
pub fn prepare_template_only(nodes: &[XfaNode]) -> Result<Prepared, XfaError> {
    Ok(Prepared {
        flattened: Flattened::from_xfa_simple(nodes).map_err(XfaError::Layout)?,
        fidelity: Fidelity::Template,
        warning: None,
    })
}
