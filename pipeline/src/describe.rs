//! Cataloguing a reference form: a read-only pass that inspects a source form
//! and the AEM package built from it, then writes the description the reference
//! store matches against.
//!
//! One stage rather than a pipeline, but the same machinery — it runs on the
//! same [`run_stage`] and the same scoped tool catalog as a conversion, so it
//! inherits retry, abort and the stuck watchdog instead of reimplementing a
//! weaker loop of its own.

use std::sync::Arc;

use agent::ConversionAgent;
use blueprint::OutputTarget;
use rig_agent::agent::model::ModelHandle;

use crate::hooks::PriceFn;
use crate::memory::ContextBudget;
use crate::observer::{AbortFlag, SharedObserver};
use crate::roles::Role;
use crate::run::run_stage;

const DESCRIBE_PROMPT: &str = "\
You are cataloguing a reference form so it can later be matched against similar forms. \
First ANALYSE THE INPUTS using the tools. Call `get_source_info` for the source PDFs and their \
`doc_path`. Read the form's structure with `xfa_outline`, `xfa_node` and `xfa_search` (the XFA is \
the authoritative field/label/option source), and look at its pages with `xfa_render_pages`. \
Pass `dpi: 72`: you only have to read the pages, and a lower resolution costs far fewer tokens. \
Use `xfa_controls` to find the controls that show or hide sections. Inspect the resulting AEM \
package via `get_package_info` and `read_package_file`. Call as many as you need before \
answering.\n\n\
Then write a detailed description covering: the overall purpose; each section and its heading; \
the fields in order with their literal labels and types (text, date, number, select, radio, \
checkbox); logical groupings (address blocks, signature blocks, account-holder / client-details \
sections, type selectors like 'Tipo'/'Type'); and any dynamic behaviour (repeatable sections, \
conditional show/hide). Use precise, literal labels.\n\n\
Output ONLY the description text itself, as prose with no markdown. Do NOT include any preamble, \
sign-off, or meta-commentary about your analysis, the tools, or the sources. Never write sentences \
like \"I now have a complete picture...\", \"Based on the XFA and AEM package...\", or \"Here is the \
catalogue description.\". Begin immediately with the form's purpose (e.g. \"This form ...\").";

/// Describe the reference form made up of `pdfs` and the AEM package built from
/// it.
///
/// Returns the description text, or an error if the model never produced one.
#[allow(clippy::too_many_arguments)]
pub async fn describe_reference(
    profile: &str,
    pdfs: Vec<(String, Vec<u8>)>,
    package_zip: Vec<u8>,
    abort: &AbortFlag,
    model: ModelHandle,
    price: PriceFn,
    max_tokens: u32,
    context_budget: Arc<dyn ContextBudget>,
    obs: &SharedObserver,
) -> Result<String, String> {
    let _ = blueprint::load_profile_fonts(profile);

    // A throwaway agent over the same catalog: it reads the source and the
    // uploaded package and edits nothing, so it needs no history session.
    let mut agent = ConversionAgent::new(
        Some(profile.to_string()),
        pdfs,
        String::new(),
        OutputTarget::Aem,
    );
    agent.seed_package(package_zip);
    let shared_agent: crate::tools::SharedAgent = std::sync::Arc::new(tokio::sync::Mutex::new(agent));

    // A one-off pass, not part of a multi-stage run — nothing outlives this
    // call to fold the total into, so a fresh accumulator is the whole story.
    let mut spend = crate::observer::Spend::default();
    let description = run_stage(
        &shared_agent,
        &DESCRIBE,
        DESCRIBE_PROMPT,
        "Analyse the inputs with the tools, then write the catalogue description.",
        abort,
        model,
        price,
        max_tokens,
        context_budget,
        obs,
        &mut spend,
    )
    .await
    .ok_or("The description pass was cancelled.")?;

    if description.trim().is_empty() {
        return Err("The model produced no description.".into());
    }
    Ok(description)
}

/// The describe pass as a stage: read-only, no terminal tool, and a turn budget
/// well under an authoring stage's.
const DESCRIBE: Role = Role {
    name: "Describe",
    scope: agent::scope::DESCRIBE,
    max_iterations: 25,
    stuck_tool: None,
    stuck_activity: "analysis",
    // Never reached: this stage authors nothing, so it has no oversized call to
    // break up. Present because every Role carries one.
    max_tokens_nudge: "Summarise what you have and finish the description.",
};
