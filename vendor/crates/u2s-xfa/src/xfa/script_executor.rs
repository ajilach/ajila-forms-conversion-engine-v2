//! Script Executor - Handles XFA script execution as a separate concern from flattening.
//!
//! This module extracts script execution logic from the Flattened module to maintain
//! a clean separation of concerns:
//! - `ScriptExecutor` handles all side effects (script execution, presence changes)
//! - `Flattened` remains a pure transformation from XFA tree to absolute positions
//!
//! # Architecture
//!
//! ```text
//! XFA Nodes (immutable) ──► ScriptExecutor ──► ScriptExecutionResult
//!                                │                    │
//!                                │                    ├─ computed_values
//!                                │                    └─ presence_changes
//!                                ▼
//!                          XFA Nodes (cloned + modified)
//!                                │
//!                                ▼
//!                           Flattened (pure)
//! ```

use crate::flattened::{FieldAccess, PageOverrides};
use crate::xfa::instances::Prototypes;
use crate::xfa::scripting::som::SomResolver;
use crate::xfa::scripting::som::{child_som_path, sibling_indices, walk_som_path};
use crate::xfa::scripting::form::XfaNodeRef;
use crate::xfa::scripting::{
    EventActivity, EventRef, Presence, ScriptContentType, SomPath, XfaScript, XfaScriptEngine,
    parse_events_from_node,
};
use crate::xfa::{XfaNode, XfaNodeKind};
use crate::xfa::scripting::engine::MAX_RECORDED_SCRIPT_ERRORS;
use std::collections::{HashMap, HashSet};

/// A pair of child name and child ID.
type ChildNameIdPair = (String, String);

/// Represents an event with its associated metadata:
/// - name: The node name
/// - full_path: The full SOM path
/// - children: List of child (name, id) pairs
/// - script: The XFA script to execute
/// - presence: The node's presence value
/// - page_area: the name of the enclosing `pageArea`, for a node placed
///   directly in a page's master content; `None` for body content. A page
///   has exactly one chosen pageArea (XFA 3.3 §8, "each instance of a
///   pageArea represents a unique display surface"), so its master events
///   are the only ones that should run while that page is being evaluated.
/// - post_order: this event's rank in a *post-order* (children-before-parent)
///   depth-first traversal of the Form DOM, used only to order Phase 1
///   (initialize); see that phase for why.
type EventWithChildren = (
    String,
    String,
    Vec<ChildNameIdPair>,
    XfaScript,
    Presence,
    Option<String>,
    usize,
);

/// Result of script execution containing computed values and presence changes.
#[derive(Debug, Clone, Default)]
pub struct ScriptExecutionResult {
    /// Computed field values from script execution (field name/path -> value)
    pub computed_values: HashMap<SomPath, String>,
    /// Presence changes the scripts made, to apply to the nodes.
    pub presence_changes: Vec<PresenceChange>,
    /// `access` changes the scripts made (`this.access = "protected"` in an
    /// initialize script, say), under each node's canonical SOM path, to
    /// apply with [`ScriptExecutor::apply_access_changes`].
    pub access_changes: Vec<(SomPath, FieldAccess)>,
    /// Caption texts the scripts set (`caption.value.#text`), under each
    /// node's canonical SOM path, to apply with
    /// [`ScriptExecutor::apply_caption_changes`].
    pub caption_changes: Vec<(SomPath, String)>,
    /// Choice lists the scripts built (`addItem`, `clearItems`), under each
    /// field's canonical SOM path, to apply with
    /// [`ScriptExecutor::apply_item_changes`].
    pub item_changes: Vec<(SomPath, Vec<(String, String)>)>,
    /// The dialogs the load-time scripts raised and the scripts that failed.
    pub script_report: crate::xfa::scripting::ScriptReport,
}

/// One node's presence as a load-pass script left it.
#[derive(Debug, Clone, PartialEq)]
pub struct PresenceChange {
    /// The node's full SOM path, instance indices included -- what tells
    /// one instance of a repeated section from another.
    pub path: String,
    pub presence: Presence,
}

/// The master-page scripts, kept after the document-wide pass so that they
/// can be evaluated again once the page count is known.
///
/// Page-dependent scripts -- "Pagina 2 di 3", a barcode that encodes the page
/// number, a block that appears on the first page only -- cannot be evaluated
/// during the first pass, because how many pages there are is a *result* of
/// laying the body out. They are therefore run once more per page, in the
/// form's own engine (the one the pass ran in, so the form's script objects
/// and variables are still there, with every body script's effects in
/// place), with `xfa.layout` and the pageArea's `index` pointing at that
/// page.
pub struct LayoutScripts {
    events: Vec<EventWithChildren>,
    /// The full SOM path of every node inside a pageArea, with the value
    /// its template gives it (a field's or draw's `<value>`; `None` for
    /// anything else).
    master_keys: HashMap<String, Option<String>>,
    /// The master-page scripts that failed since the last
    /// [`take_failures`](Self::take_failures), each tagged with its page.
    page_failures: Vec<crate::xfa::scripting::ScriptFailure>,
}

/// The scripting environment the document-wide pass ran in, handed to the
/// form that keeps it, with the master-page scripts to run again per page.
pub struct LoadedScripts {
    pub engine: XfaScriptEngine,
    pub layout: LayoutScripts,
}

/// The master-page nodes' state as the form holds it outside the per-page
/// pass: each field's value (what the person typed into a header field,
/// what body scripts wrote there) and each node's presence (what a body
/// script hid). It is taken with [`LayoutScripts::master_state`] before
/// the pages are evaluated and put back with
/// [`LayoutScripts::restore_master`] afterwards, so the pass leaves no
/// page's own answers in the form.
pub struct MasterState {
    values: Vec<(String, String)>,
    presence: HashMap<String, String>,
}

impl LayoutScripts {
    pub fn master_state(&self, engine: &mut XfaScriptEngine) -> MasterState {
        let mut values = Vec::new();
        let mut presence = HashMap::new();
        for path in self.master_keys.keys() {
            if let Some(value) = engine.get_som_field_value(path) {
                values.push((path.clone(), value));
            }
            if let Some(p) = engine.get_js_presence(path) {
                presence.insert(path.clone(), p);
            }
        }
        MasterState { values, presence }
    }

    /// What a page's scripts start from (XFA 3.3 §8 "Master Pages": each
    /// page's master content is instantiated afresh): a field's template
    /// value, so a script guarded by `if (!this.rawValue)` computes this
    /// page's footer rather than keeping the document-wide pass's; the
    /// presence the form holds (`state`), so a block a body script hid stays
    /// hidden on every page; `access` and caption at their baselines.
    fn start_page(&self, engine: &mut XfaScriptEngine, state: &MasterState) {
        for (path, template_value) in &self.master_keys {
            engine.update_field_value(path, template_value.as_deref().unwrap_or(""));
            engine.reset_to_baseline(path);
        }
        for (path, presence) in &state.presence {
            engine.set_js_presence(&SomPath::new(path), presence);
        }
    }

    /// Put every master-page node back to `state` (its value and presence),
    /// and its `access` and caption to their baselines: what the engine,
    /// which the form goes on interacting in, is left with once the pages
    /// are laid out, so a typed header value survives the pass and a master
    /// script's per-page lock (AAGS_019_DE's account number) is not
    /// reported as the next interaction's doing.
    pub fn restore_master(&self, engine: &mut XfaScriptEngine, state: &MasterState) {
        self.start_page(engine, state);
        for (path, value) in &state.values {
            engine.update_field_value(path, value);
        }
    }

    /// Evaluate the master-page scripts as they stand on one page, in
    /// `engine`, the form's own, starting from `state`.
    ///
    /// `page_area` is the name of the pageArea chosen for this page (XFA 3.3
    /// §8: each page has exactly one). Master events that belong to a
    /// *different* pageArea are skipped -- without this, two pageAreas that
    /// share a fragment (e.g. a `Footer_Line` present in both `MP` and
    /// `MP_Last`, but with different footer scripts) would run both sets of
    /// scripts on every page, and whichever ran last would silently overwrite
    /// the other's answer.
    ///
    /// Returns only what this page's scripts produced, which the flattener lays
    /// over the document-wide values for that page alone.
    pub fn evaluate(
        &mut self,
        engine: &mut XfaScriptEngine,
        state: &MasterState,
        page_area: &str,
        page_index: usize,
        page_count: usize,
    ) -> PageOverrides {
        engine.set_layout_context(page_index, page_count);
        // Whatever ran in this engine before this page is not this page's
        // to report.
        engine.take_report();

        // A field whose script does not refill it falls back to the
        // document-wide value, because empty results are dropped from the
        // overrides below.
        self.start_page(engine, state);

        let mut overrides = PageOverrides::default();
        for (field_name, full_path, child_fields, script, _presence, event_page_area, _post_order) in
            &self.events
        {
            if script.content_type != ScriptContentType::JavaScript {
                continue;
            }
            // Only the phases that establish a page's appearance. A change or
            // mouse event belongs to interaction, not to laying out a page.
            if !matches!(
                script.activity,
                EventActivity::Initialize | EventActivity::Ready | EventActivity::Calculate
            ) {
                continue;
            }
            if !self.master_keys.contains_key(full_path) {
                continue;
            }
            // This node's master content is not on this page at all (it
            // belongs to a pageArea this page did not choose), so its script
            // must not run -- running it anyway is exactly what produced
            // "Pagina 3/3" on every page of a form whose two pageAreas
            // compute the footer differently.
            if let Some(owner) = event_page_area
                && owner != page_area
            {
                continue;
            }

            engine.set_current_field_with_children(full_path, field_name, "", child_fields);
            engine.update_event_context(&script.activity, full_path, None);
            // A failure is recorded by the engine and collected below.
            let _ = engine.execute_script(script);

            if let Some(presence) = engine.get_current_field_presence() {
                overrides.presence.insert(full_path.clone(), presence);
            }
            for (child_name, _) in child_fields {
                if let Some((_, presence)) = engine.get_child_field_presence(child_name) {
                    overrides
                        .presence
                        .insert(format!("{full_path}.{child_name}"), presence);
                }
            }
        }

        // Only what this page actually produced. An empty result is the
        // absence of an answer, not an answer of "", and must let the
        // document-wide value through.
        for (path, value) in engine.get_all_som_field_values_by_path() {
            if !value.is_empty() {
                overrides.values.insert(SomPath::new(path), value);
            }
        }

        // Presence written to a master node *other* than the one whose script
        // ran (e.g. `mp.FIM_Off.presence = "hidden"` set from a sibling's
        // initialize event) is not captured by `get_current_field_presence`
        // above, which only reads the executing node (`_xfa_this_`). A
        // master node's presence is this page's answer whenever it is not
        // the template's (the flattener instantiates master content from
        // the template) or not what the form holds in `state` (AALU_019_DE's
        // barcode: hidden by the document-wide pass, which has no layout to
        // compute it from, and shown again by each page).
        for path in self.master_keys.keys() {
            let Some(current) = engine.get_js_presence(path) else {
                continue;
            };
            let same_as =
                |other: Option<&String>| other.is_some_and(|o| o.eq_ignore_ascii_case(&current));
            if same_as(state.presence.get(path))
                && same_as(engine.get_template_presence(path).as_ref())
            {
                continue;
            }
            let p: Presence = current.parse().unwrap_or_default();
            overrides.presence.insert(path.clone(), p);
        }

        // A dialog a master-page script raises would be raised again by every
        // page of every relayout; the document-wide pass already reported it
        // once, so only failures are kept.
        let report = engine.take_report();
        if self.page_failures.len() < MAX_RECORDED_SCRIPT_ERRORS {
            self.page_failures
                .extend(report.errors.into_iter().map(|mut failure| {
                    failure.page = Some(page_index);
                    failure
                }));
        }

        overrides
    }

    /// The master-page scripts that failed while the pages were laid out
    /// since the last call, each tagged with its page.
    pub fn take_failures(&mut self) -> Vec<crate::xfa::scripting::ScriptFailure> {
        std::mem::take(&mut self.page_failures)
    }
}

/// Executes XFA scripts and collects their results without mutating the input tree.
pub struct ScriptExecutor;

impl ScriptExecutor {
    /// Execute all form-ready scripts and return the results.
    ///
    /// This function does NOT mutate the input nodes. Instead, it returns
    /// a `ScriptExecutionResult` containing:
    /// - `computed_values`: Field values computed by scripts
    /// - `presence_changes`: Presence changes that should be applied to nodes
    ///
    /// The caller is responsible for applying presence changes to a cloned tree.
    ///
    /// # Arguments
    /// * `xfa_nodes` - The XFA node tree (read-only)
    ///
    /// # Returns
    /// `ScriptExecutionResult` on success, or prints a warning and returns default on failure.
    ///
    /// The template is first materialised into its Form DOM (each repeatable
    /// subform as its `initial` instances); the input is left untouched.
    pub fn execute(xfa_nodes: &[XfaNode]) -> ScriptExecutionResult {
        let mut nodes = xfa_nodes.to_vec();
        let mut prototypes = crate::xfa::instances::materialize_initial_instances(&mut nodes);
        Self::execute_with_layout(&mut nodes, &mut prototypes).0
    }

    /// As [`ScriptExecutor::execute`], but also hands back the live engine so
    /// that page-dependent master-page scripts can be re-evaluated once the
    /// page count is known. `None` when script execution failed, in which case
    /// there is nothing to re-evaluate either.
    ///
    /// `xfa_nodes` is the Form DOM, already materialised from the template
    /// with `prototypes`: a script that adds or removes instances of a
    /// repeatable subform during the pass changes it in place (XFA 3.3 §9),
    /// so the caller lays out what the scripts actually built.
    pub fn execute_with_layout(
        xfa_nodes: &mut Vec<XfaNode>,
        prototypes: &mut Prototypes,
    ) -> (ScriptExecutionResult, Option<LoadedScripts>) {
        // A pass that fails part-way must not leave a half-changed Form DOM
        // behind for the caller to lay out.
        let snapshot = (xfa_nodes.clone(), prototypes.clone());
        match Self::execute_internal(xfa_nodes, prototypes) {
            Ok((result, layout)) => (result, Some(layout)),
            Err(e) => {
                (*xfa_nodes, *prototypes) = snapshot;
                log::warn!(
                    "Script execution failed: {}. Continuing without script results.",
                    e
                );
                (ScriptExecutionResult::default(), None)
            }
        }
    }

    /// Internal implementation that can return errors.
    fn execute_internal(
        xfa_nodes: &mut [XfaNode],
        prototypes: &mut Prototypes,
    ) -> Result<(ScriptExecutionResult, LoadedScripts), String> {
        let mut computed_values = HashMap::new();
        let mut presence_changes: Vec<PresenceChange> = Vec::new();
        // The template as it is before any script runs: what
        // `xfa.host.resetData` resets a field to.
        let pristine = xfa_nodes.to_vec();
        let mut engine = XfaScriptEngine::new();
        Self::prepare_engine(xfa_nodes, prototypes, &mut engine);

        let mut all_events = Self::scan_events(xfa_nodes, &mut engine);

        // A FormCalc script that did not translate (`events::executable`)
        // never runs: the JavaScript-only phases below skip it, so it is
        // reported here, once.
        for (_, full_path, _, script, _, _, _) in &all_events {
            if script.content_type == ScriptContentType::FormCalc {
                engine.record_unrunnable(script, full_path);
            }
        }

        // Phase 0: Execute calculate scripts with convergence loop
        // Per XFA 3.3 §10 p.407: "All value calculations are done, then all
        // property calculations, then all validations, and then all initialize
        // events are fired. Calculations are repeated if the values on which
        // they depend change."
        //
        // Per XFA 3.3 §10 p.380: "In cascading calculations the processing
        // application re-activates calculate objects as the values upon which
        // they depend change."
        //
        // Per XFA 3.3 §10 p.380 on circular references: "It is recommended
        // that the processing application provide some means of identifying
        // and terminating the execution of seemingly infinite loops."
        let max_calc_iterations: usize = std::env::var("XFA_MAX_CALC_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25);

        // Collect calculate events for the convergence loop
        let calc_events: Vec<&EventWithChildren> = all_events
            .iter()
            .filter(|(_, _, _, script, _, _, _)| {
                script.content_type == ScriptContentType::JavaScript
                    && script.activity == EventActivity::Calculate
            })
            .collect();

        for iteration in 0..max_calc_iterations {
            let mut any_changed = false;

            let inactive = Self::build_full_path_presence_map(&presence_changes);
            for (field_name, full_path, child_fields, script, static_presence, _page_area, _post_order) in
                &calc_events
            {
                if Self::is_effectively_inactive(full_path, *static_presence, &inactive) {
                    continue;
                }
                engine.set_current_field_with_children(full_path, field_name, "", child_fields);
                engine.update_event_context(&EventActivity::Calculate, full_path, None);

                let old_value = computed_values
                    .get(&SomPath::new(full_path.clone()))
                    .cloned();

                if let Ok(Some(value)) = engine.execute_script(script) {
                    // Per XFA 3.3 §10 p.380: the calculate result replaces the
                    // container's value, which the next calculation reads.
                    if old_value.as_ref() != Some(&value) {
                        any_changed = true;
                    }
                    engine.update_field_value(&SomPath::new(full_path.clone()), &value);
                    computed_values.insert(SomPath::new(full_path.clone()), value);
                }

                // Collect values set on fields during calculation (side effects)
                let calc_som_values = engine.get_all_som_field_values_by_path();
                for (calc_field_name, calc_value) in calc_som_values {
                    let key = SomPath::new(calc_field_name);
                    if computed_values.get(&key) != Some(&calc_value) {
                        any_changed = true;
                    }
                    computed_values.insert(key, calc_value);
                }
            }

            if !any_changed {
                break;
            }

            if iteration == max_calc_iterations - 1 {
                log::warn!(
                    "Calculation convergence not reached after {} iterations \
                     (possible circular dependency). Results may be incorrect.",
                    max_calc_iterations
                );
            }
        }

        Self::apply_load_ops(
            xfa_nodes,
            prototypes,
            &mut engine,
            &mut all_events,
            &mut computed_values,
            &mut presence_changes,
        )?;

        // Phase 0b: Execute validate scripts
        // Per XFA 3.3 §10 p.407: validations run after calculations, before
        // initialize events. Validate scripts are executed but their results
        // are informational (validation state), not value-replacing.
        // However, validate scripts may set rawValue as a side effect.
        let inactive = Self::build_full_path_presence_map(&presence_changes);
        for (field_name, full_path, child_fields, script, static_presence, _page_area, _post_order) in
            &all_events
        {
            if script.content_type == ScriptContentType::JavaScript
                && script.activity == EventActivity::Validate
                && !Self::is_effectively_inactive(full_path, *static_presence, &inactive)
            {
                engine.set_current_field_with_children(full_path, field_name, "", child_fields);
                engine.update_event_context(&EventActivity::Validate, full_path, None);
                let _ = engine.execute_script(script);

                // Collect any side-effect values set during validation
                let validate_som_values = engine.get_all_som_field_values_by_path();
                for (vf_name, vf_value) in validate_som_values {
                    computed_values.insert(SomPath::new(vf_name), vf_value);
                }
            }
        }

        Self::apply_load_ops(
            xfa_nodes,
            prototypes,
            &mut engine,
            &mut all_events,
            &mut computed_values,
            &mut presence_changes,
        )?;

        // Phase 1: Execute initialize events
        // Per XFA 3.3 §10 p.407 Rule 1: "The property is inspected at the
        // moment that the calculation, validation, or event would be triggered."
        // We maintain a running presence map so that if an earlier init script
        // sets a later field to inactive, that field's init script is skipped.
        //
        // Per XFA 3.3 §10 Rule 3 the initialize events fire "in a depth-first
        // traversal of the Form DOM", which does not by itself say whether a
        // container fires before or after its descendants. A real form
        // (AAOV_033_IT) pins the reading down: a leaf field's initialize sets
        // an unconditional default caption, and an ancestor subform's
        // initialize overwrites it with a caption chosen from a sibling
        // dropdown's selection. Acrobat's rendering (`pdf-screenshots/aaov`)
        // shows the dropdown-derived caption, which only happens if
        // descendants' initialize events run before their ancestor's: a
        // post-order traversal, children (in document order) before the
        // container. Phase 1 therefore sorts by the post-order rank
        // `find_all_events_with_child_ids` assigned each event; every other
        // phase keeps document order. A master page's content is
        // instantiated per page after the body, in Acrobat as here
        // (`LayoutScripts::evaluate`), so a footer reading what the root's
        // initialize prepared (AACB_033_IT's `form64689`) gets it there.
        let mut initialize_events: Vec<&EventWithChildren> = all_events.iter().collect();
        initialize_events.sort_by_key(|event| event.6);
        let mut init_dynamic_presence: HashMap<String, Presence> = HashMap::new();
        for (field_name, full_path, child_fields, script, static_presence, _page_area, _post_order) in
            &initialize_events
        {
            if script.content_type == ScriptContentType::JavaScript
                && script.activity == EventActivity::Initialize
                && !Self::is_effectively_inactive(
                    full_path,
                    *static_presence,
                    &init_dynamic_presence,
                )
            {
                engine.set_current_field_with_children(full_path, field_name, "", child_fields);
                engine.update_event_context(&EventActivity::Initialize, full_path, None);

                let result = engine.execute_script(script);
                if let Err(ref e) = result {
                    log::debug!("[INIT ERR] field={field_name} path={full_path}: {e}");
                }
                let _ = result;

                // Collect presence values set on the current field
                if let Some(presence) = engine.get_current_field_presence() {
                    presence_changes.push(PresenceChange {
                        path: full_path.clone(),
                        presence,
                    });
                }

                // Collect presence values set on child fields
                for (child_name, _) in child_fields {
                    if let Some((_, presence)) = engine.get_child_field_presence(child_name) {
                        presence_changes.push(PresenceChange {
                            path: format!("{full_path}.{child_name}"),
                            presence,
                        });
                    }
                }

                // Collect values from initialize scripts
                // Empty strings are valid per XFA spec (cleared fields, deselected exclGroups)
                let init_som_values = engine.get_all_som_field_values_by_path();
                for (init_field_name, init_value) in init_som_values {
                    computed_values.insert(SomPath::new(init_field_name), init_value);
                }

                // Update running presence map for intra-phase suppression.
                // Per XFA 3.3 §10 Rule 1: presence is checked at the moment
                // each event would fire, so changes from earlier init scripts
                // must suppress later ones within the same phase.
                // Use full SOM paths as keys to avoid collisions between
                // fields with the same leaf name in different subforms.
                if let Some(presence) = engine.get_current_field_presence() {
                    init_dynamic_presence.insert(full_path.clone(), presence);
                }
                for (child_name, _child_id) in child_fields {
                    if let Some((_id, presence)) = engine.get_child_field_presence(child_name) {
                        // child_name is already a leaf; prefix with parent path
                        let child_full = format!("{}.{}", full_path, child_name);
                        init_dynamic_presence.insert(child_full, presence);
                    }
                }
                let som_pres = engine.get_all_som_presence_changes();
                for (som_path, pres_str) in &som_pres {
                    let presence = pres_str.parse().unwrap_or_default();
                    // SOM paths from the engine are already full paths
                    init_dynamic_presence.insert(som_path.clone(), presence);
                }
            }
        }

        Self::apply_load_ops(
            xfa_nodes,
            prototypes,
            &mut engine,
            &mut all_events,
            &mut computed_values,
            &mut presence_changes,
        )?;

        // Build dynamic presence map from Phase 1 presence changes.
        // Per XFA 3.3 §10 p.407 Rule 1, presence is inspected at the moment
        // the event would be triggered.
        // Also collect SOM-level presence changes (e.g. `fieldB.presence = "inactive"`)
        // for cross-phase suppression only — these are NOT added to the output
        // presence_changes since they were not previously tracked there.
        let mut dynamic_presence_overrides: HashMap<String, Presence> = HashMap::new();
        let som_presence_changes = engine.get_all_som_presence_changes();
        for (som_path, presence_str) in &som_presence_changes {
            let presence = presence_str.parse().unwrap_or_default();
            // Use full SOM path as key to avoid leaf-name collisions
            dynamic_presence_overrides.insert(som_path.clone(), presence);
            engine.update_initial_presence(&SomPath::new(som_path), presence_str);
        }
        let mut presence_map_after_phase1 = Self::build_full_path_presence_map(&presence_changes);
        presence_map_after_phase1.extend(dynamic_presence_overrides.clone());

        // Phase 2: Execute form-ready JavaScript events
        Self::execute_phase_events(
            &all_events,
            EventActivity::Ready,
            Some(&EventRef::Form),
            &presence_map_after_phase1,
            &mut engine,
            &mut computed_values,
        );

        // Build dynamic presence map from Phase 1+2 presence changes.
        // Also collect SOM-level presence changes from Phase 2 for cross-phase suppression.
        let som_presence_changes_2 = engine.get_all_som_presence_changes();
        for (som_path, presence_str) in &som_presence_changes_2 {
            let presence = presence_str.parse().unwrap_or_default();
            // Use full SOM path as key to avoid leaf-name collisions
            dynamic_presence_overrides.insert(som_path.clone(), presence);
            engine.update_initial_presence(&SomPath::new(som_path), presence_str);
        }
        let mut presence_map_after_phase2 = Self::build_full_path_presence_map(&presence_changes);
        presence_map_after_phase2.extend(dynamic_presence_overrides.clone());

        Self::apply_load_ops(
            xfa_nodes,
            prototypes,
            &mut engine,
            &mut all_events,
            &mut computed_values,
            &mut presence_changes,
        )?;

        // Phase 3: Execute layout-ready JavaScript events
        Self::execute_phase_events(
            &all_events,
            EventActivity::Ready,
            Some(&EventRef::Layout),
            &presence_map_after_phase2,
            &mut engine,
            &mut computed_values,
        );

        Self::apply_load_ops(
            xfa_nodes,
            prototypes,
            &mut engine,
            &mut all_events,
            &mut computed_values,
            &mut presence_changes,
        )?;

        // Phase 4: Execute docReady JavaScript events
        // Per XFA 3.3 §10 p.408: docReady fires after all form-level events
        // (initialize, form:ready, layout:ready) have completed.
        Self::execute_phase_events(
            &all_events,
            EventActivity::DocReady,
            None,
            &presence_map_after_phase2,
            &mut engine,
            &mut computed_values,
        );

        Self::apply_load_ops(
            xfa_nodes,
            prototypes,
            &mut engine,
            &mut all_events,
            &mut computed_values,
            &mut presence_changes,
        )?;

        // Phase 5: Collect all values from SOM hierarchy
        // Empty strings are valid per XFA spec (cleared fields, deselected exclGroups)
        let som_values_by_path = engine.get_all_som_field_values_by_path();
        for (full_path, value) in som_values_by_path {
            computed_values
                .entry(SomPath::new(full_path))
                .or_insert(value);
        }

        // Every presence a script of any phase set on another node (a
        // ready script's `Section.presence = "hidden"`), so the Form DOM
        // opens as the scripts left it; and the engine's baseline moved to
        // that state, since the form goes on interacting in this engine
        // and a presence change it reports after that must be the
        // interaction's own.
        for (path, presence_str) in engine.get_all_som_presence_changes() {
            dynamic_presence_overrides.insert(path.clone(), presence_str.parse().unwrap_or_default());
            engine.update_initial_presence(&SomPath::new(&path), &presence_str);
        }
        for (path, presence) in dynamic_presence_overrides {
            match presence_changes.iter_mut().find(|c| c.path == path) {
                Some(change) => change.presence = presence,
                None => presence_changes.push(PresenceChange { path, presence }),
            }
        }
        Self::settle_engine(xfa_nodes, &pristine, &mut engine);

        // Master-page content re-evaluated per page later (`LayoutScripts`)
        // can set `access` again for each page; that per-page answer only
        // changes how a field is drawn, so what is collected here -- the
        // document-wide pass, page 1 of 1 -- is what the form keeps.
        let access_changes = engine.take_access_changes();
        let caption_changes = engine.take_caption_changes();
        let item_changes = engine.take_item_changes();

        // A master-page script runs again for every page once the pages are
        // known (`LayoutScripts::evaluate`), with every body script's effects
        // in place, and what it computes there is what is drawn. Its failure
        // in this document-wide pass is superseded, so it is left out here;
        // its own per-page failures are reported by `take_failures`.
        let master_keys = Self::master_page_event_keys(xfa_nodes);
        let mut script_report = engine.take_report();
        let master_paths: HashSet<&str> = master_keys.keys().map(String::as_str).collect();
        script_report
            .errors
            .retain(|failure| !master_paths.contains(failure.owner.as_str()));

        let layout = LayoutScripts {
            events: all_events,
            master_keys,
            page_failures: Vec::new(),
        };

        Ok((
            ScriptExecutionResult {
                computed_values,
                presence_changes,
                access_changes,
                caption_changes,
                item_changes,
                script_report,
            },
            LoadedScripts { engine, layout },
        ))
    }

    /// The scripting environment of a Form DOM before any of its scripts
    /// run: every node registered under its SOM path, the `<text>`
    /// variables, and the named script objects (XFA 3.3 §10), which are
    /// built on first reference in their owner subform's scope, so the
    /// owners must exist first. The one setup for every engine, whether
    /// the load pass then runs in it or (when the pass failed and was
    /// rolled back) nothing does.
    pub(crate) fn prepare_engine(
        xfa_nodes: &[XfaNode],
        prototypes: &Prototypes,
        engine: &mut XfaScriptEngine,
    ) {
        Self::extract_and_register_translations(xfa_nodes, engine);
        Self::build_and_register_xfa_som_hierarchy(xfa_nodes, prototypes, engine);
        engine.register_script_objects(xfa_nodes);
    }

    /// What the engine gets once the Form DOM has its final shape for
    /// opening: each field's template value for `xfa.host.resetData`
    /// (`pristine` is the template before any script ran), its picture
    /// clauses for `formattedValue`, and the values it opens with as the
    /// ones `getDeltas()` compares against.
    pub(crate) fn settle_engine(
        xfa_nodes: &[XfaNode],
        pristine: &[XfaNode],
        engine: &mut XfaScriptEngine,
    ) {
        let defaults = Self::template_defaults(pristine, engine);
        engine.set_template_defaults(&defaults);
        Self::install_value_formats(xfa_nodes, engine);
        engine.rebase_merged_values();
    }

    /// Every field's and exclusion group's value in `pristine`, the template
    /// before any script ran, under each registered path. A path the
    /// pristine tree does not reach (an instance the load scripts added) is
    /// looked up as its first instance, since every instance of a repeated
    /// section has the same template.
    fn template_defaults(
        pristine: &[XfaNode],
        engine: &mut XfaScriptEngine,
    ) -> HashMap<SomPath, String> {
        engine
            .get_field_values_by_canonical_path()
            .into_keys()
            .filter_map(|path| {
                let node = walk_som_path(pristine, path.as_str())
                    .or_else(|| walk_som_path(pristine, path.index_free().as_str()))?;
                (node.kind.is_field() || node.kind.is_exclgroup()).then(|| {
                    let value = XfaNodeRef::extract_value_from_xfa_node(node).unwrap_or_default();
                    (path, value)
                })
            })
            .collect()
    }

    /// Give every field's script object its `formattedValue` and
    /// `editValue`, through the pictures its template declares.
    fn install_value_formats(nodes: &[XfaNode], engine: &mut XfaScriptEngine) {
        for path in engine.get_field_values_by_canonical_path().into_keys() {
            let Some(meta) =
                walk_som_path(nodes, path.as_str()).and_then(|node| node.field_meta.as_deref())
            else {
                continue;
            };
            let source = |p: &Option<crate::xfa::field_meta::PictureSpec>| {
                p.as_ref().map(|p| p.source.clone())
            };
            engine.install_value_format(
                &path,
                source(&meta.format).as_deref(),
                source(&meta.edit).as_deref(),
                &meta.locale,
            );
        }
    }

    /// Register the instances the scripts added under `parent`, which the
    /// engine's [`XfaScriptEngine::reconcile_instances`] left standing as
    /// `placeholders` (each with the values scripts wrote into it, by path
    /// relative to the instance), now that the Form DOM holds them. Answers
    /// the new instances' paths.
    ///
    /// Registration skips every path already registered, so walking the
    /// parent's children registers exactly the new instances, linked in
    /// where their placeholders stood.
    pub(crate) fn register_added_instances(
        xfa_nodes: &[XfaNode],
        prototypes: &Prototypes,
        engine: &mut XfaScriptEngine,
        parent: &SomPath,
        placeholders: Vec<(SomPath, HashMap<String, String>)>,
    ) -> Result<Vec<SomPath>, String> {
        if placeholders.is_empty() {
            return Ok(Vec::new());
        }
        let parent_node = walk_som_path(xfa_nodes, parent.as_str())
            .ok_or_else(|| format!("{parent} is missing from the Form DOM"))?;
        Self::register_nodes_recursive(
            &parent_node.children,
            Some(parent.as_str()),
            prototypes,
            engine,
            false,
        );
        let mut new_instances = Vec::with_capacity(placeholders.len());
        for (path, written) in placeholders {
            for (rel, value) in written {
                engine.update_field_value(&format!("{path}.{rel}"), &value);
            }
            new_instances.push(path);
        }
        Ok(new_instances)
    }

    /// Every event script in the Form DOM, with its owner's name and full
    /// SOM path, registered for `execEvent()` (XFA 3.3 §10 pp.407-409 Rule
    /// 3). Run again whenever the Form DOM changes shape, since a new
    /// instance brings its own events and a shifted one new paths.
    fn scan_events(xfa_nodes: &[XfaNode], engine: &mut XfaScriptEngine) -> Vec<EventWithChildren> {
        // Build parent-child map for setting up `this.childField` access
        let parent_child_map = Self::build_parent_child_map_with_ids(xfa_nodes);

        let mut all_events = Vec::new();
        let mut post_order_counter: usize = 0;
        Self::find_all_events_with_child_ids(
            xfa_nodes,
            &mut all_events,
            &parent_child_map,
            None, // Start with no parent path
            None, // Start outside any pageArea
            &mut post_order_counter,
        );

        for (_, full_path, _, script, _, _, _) in &all_events {
            if script.content_type == ScriptContentType::JavaScript {
                engine.register_event_script(
                    full_path,
                    script.activity.activity_name(),
                    &script.source,
                );
            }
        }
        all_events
    }

    /// Run one event's script with its owner as `this`.
    fn run_event(
        engine: &mut XfaScriptEngine,
        event: &EventWithChildren,
    ) -> Result<Option<String>, String> {
        let (field_name, full_path, child_fields, script, _, _, _) = event;
        engine.set_current_field_with_children(full_path, field_name, "", child_fields);
        engine.update_event_context(&script.activity, full_path, None);
        engine.execute_script(script)
    }

    /// Apply the instance-manager changes the pass's scripts queued (XFA 3.3
    /// §9) to the Form DOM and the engine, between two phases.
    ///
    /// Applied at phase boundaries rather than after each script because a
    /// phase walks a fixed list of events: a later script in the same phase
    /// reaches a new instance through its placeholder, whose written values
    /// are carried over here. A new instance then gets the events loading
    /// would have given it -- `initialize` (post-order), its calculations --
    /// and it and every re-indexed instance get `indexChange`.
    fn apply_load_ops(
        xfa_nodes: &mut [XfaNode],
        prototypes: &mut Prototypes,
        engine: &mut XfaScriptEngine,
        all_events: &mut Vec<EventWithChildren>,
        computed_values: &mut HashMap<SomPath, String>,
        presence_changes: &mut Vec<PresenceChange>,
    ) -> Result<(), String> {
        const MAX_ROUNDS: usize = 32;
        for _ in 0..MAX_ROUNDS {
            let ops = engine.drain_instance_ops();
            if ops.is_empty() {
                return Ok(());
            }

            let mut arrays: Vec<(SomPath, String)> = Vec::new();
            for op in &ops {
                if let Some(message) = op.limit_message() {
                    log::debug!("load pass: {message}");
                    continue;
                }
                crate::xfa::instances::apply_to_tree(xfa_nodes, prototypes, op)?;
                let (parent, name) = op.array();
                let array = (parent.clone(), name.to_string());
                if !arrays.contains(&array) {
                    arrays.push(array);
                }
            }

            arrays.sort_by_key(|(parent, _)| std::cmp::Reverse(parent.segments().count()));
            let mut new_instances: Vec<SomPath> = Vec::new();
            let mut reindexed: Vec<SomPath> = Vec::new();
            for (parent, name) in &arrays {
                let reconciled = engine.reconcile_instances(parent, name);
                reindexed.extend(reconciled.moved.into_iter().map(|(_, new)| new));
                new_instances.extend(Self::register_added_instances(
                    xfa_nodes,
                    prototypes,
                    engine,
                    parent,
                    reconciled.placeholders,
                )?);
            }
            engine.replace_som_resolver(SomResolver::from_nodes(xfa_nodes));
            *all_events = Self::scan_events(xfa_nodes, engine);

            let under = |path: &str, root: &SomPath| SomPath::new(path).starts_with(root);
            for instance in &new_instances {
                let mut owned: Vec<&EventWithChildren> = all_events
                    .iter()
                    .filter(|e| under(&e.1, instance))
                    .collect();
                owned.sort_by_key(|e| e.6);
                for event in owned.iter().filter(|e| {
                    e.3.content_type == ScriptContentType::JavaScript
                        && e.3.activity == EventActivity::Initialize
                }) {
                    let _ = Self::run_event(engine, event);
                    if let Some(presence) = engine.get_current_field_presence() {
                        presence_changes.push(PresenceChange {
                            path: event.1.clone(),
                            presence,
                        });
                    }
                }
                for event in owned.iter().filter(|e| {
                    e.3.content_type == ScriptContentType::JavaScript
                        && e.3.activity == EventActivity::Calculate
                }) {
                    if let Ok(Some(value)) = Self::run_event(engine, event) {
                        engine.update_field_value(&event.1, &value);
                    }
                }
            }
            for instance in new_instances.iter().chain(&reindexed) {
                for event in all_events.iter().filter(|e| {
                    e.1 == instance.as_str()
                        && e.3.content_type == ScriptContentType::JavaScript
                        && e.3.activity == EventActivity::IndexChange
                }) {
                    let _ = Self::run_event(engine, event);
                }
            }

            for (path, value) in engine.get_all_som_field_values_by_path() {
                computed_values.insert(SomPath::new(path), value);
            }
        }
        Err(format!(
            "instance changes kept causing further instance changes after {MAX_ROUNDS} rounds \
             during the load pass; the form's scripts appear to loop"
        ))
    }

    /// The SOM path of every node inside a `pageArea`, with its template
    /// value (see [`LayoutScripts::master_keys`]).
    ///
    /// Master-page scripts are the ones that can depend on which page they are
    /// on, so they are the ones the layout pass re-runs; this is how they are
    /// told apart from the body's.
    fn master_page_event_keys(nodes: &[XfaNode]) -> HashMap<String, Option<String>> {
        fn walk(
            nodes: &[XfaNode],
            parent_path: Option<&str>,
            inside_page_area: bool,
            out: &mut HashMap<String, Option<String>>,
        ) {
            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                let name = node.name.clone().unwrap_or_default();
                // The same paths as find_all_events_with_child_ids.
                let full_path = if name.is_empty() {
                    parent_path.unwrap_or("").to_string()
                } else {
                    child_som_path(parent_path.unwrap_or(""), &name, index)
                };
                let inside = inside_page_area || matches!(node.kind, XfaNodeKind::PageArea);
                if inside && !name.is_empty() {
                    let template_value = (node.kind.is_field() || node.kind.is_draw())
                        .then(|| node.template_value())
                        .flatten();
                    out.insert(full_path.clone(), template_value);
                }
                let next_parent = if !name.is_empty() && node.kind.extends_som_path() {
                    Some(full_path.as_str())
                } else {
                    parent_path
                };
                walk(&node.children, next_parent, inside, out);
            }
        }

        let mut out = HashMap::new();
        walk(nodes, None, false, &mut out);
        out
    }

    /// Apply presence changes to a mutable XFA node tree.
    ///
    /// This should be called on a cloned tree to preserve the original.
    /// Write `access` changes a script made onto the nodes they belong to.
    ///
    pub fn apply_access_changes(nodes: &mut [XfaNode], changes: &[(SomPath, FieldAccess)]) {
        for (path, access) in changes {
            apply_property_change(nodes, path, &format!("access = {}", access.as_str()), &mut |node| {
                node.set_access(*access)
            });
        }
    }

    /// Relabel the nodes whose caption a script set, the way
    /// [`Self::apply_access_changes`] locks the ones a script locked.
    /// Write the choice lists scripts built onto the fields they belong
    /// to, so the renderer draws and the control listing offers what the
    /// scripts put there.
    pub fn apply_item_changes(nodes: &mut [XfaNode], changes: &[(SomPath, Vec<(String, String)>)]) {
        for (path, items) in changes {
            apply_property_change(
                nodes,
                path,
                &format!("{} items", items.len()),
                &mut |node| {
                    node.set_choice_list_items(items);
                },
            );
        }
    }

    pub fn apply_caption_changes(nodes: &mut [XfaNode], changes: &[(SomPath, String)]) {
        for (path, text) in changes {
            apply_property_change(nodes, path, &format!("caption {text:?}"), &mut |node| {
                node.set_caption_text(text);
            });
        }
    }

    /// Write the presence a script set onto the node it set it on. The
    /// full path names exactly one node; one it does not reach is a defect
    /// in the paths the scripts ran under, logged rather than guessed at.
    pub fn apply_presence_changes(nodes: &mut [XfaNode], changes: &[PresenceChange]) {
        for change in changes {
            match crate::xfa::scripting::som::walk_som_path_mut(nodes, &change.path) {
                Some(node) => node.set_presence(change.presence),
                None => log::warn!(
                    "presence = {} on {} not applied: no node at that path",
                    change.presence.as_str(),
                    change.path
                ),
            }
        }
    }

    // ========================================================================
    // Helper functions moved from Flattened
    // ========================================================================

    /// Every container's named fields, as (name, id) pairs, keyed by the
    /// container's full SOM path -- the same path its events carry, so an
    /// event finds the fields `this.<field>` names.
    fn build_parent_child_map_with_ids(
        xfa_nodes: &[XfaNode],
    ) -> HashMap<String, Vec<(String, String)>> {
        fn collect(
            nodes: &[XfaNode],
            parent_path: Option<&str>,
            map: &mut HashMap<String, Vec<(String, String)>>,
        ) {
            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                let name = node.name.clone().unwrap_or_default();
                if let Some(parent) = parent_path
                    && node.kind.is_field()
                    && !name.is_empty()
                {
                    let id = node.attributes.get("id").cloned().unwrap_or_default();
                    map.entry(parent.to_string()).or_default().push((name.clone(), id));
                }
                if !name.is_empty() && node.kind.extends_som_path() {
                    let path = child_som_path(parent_path.unwrap_or(""), &name, index);
                    collect(&node.children, Some(&path), map);
                } else {
                    collect(&node.children, parent_path, map);
                }
            }
        }

        let mut map = HashMap::new();
        collect(xfa_nodes, None, &mut map);
        map
    }

    /// Execute a phase of JavaScript events (form:ready, layout:ready, or docReady).
    ///
    /// Iterates over all collected events, runs those matching the given
    /// `activity` (and optionally `event_ref`), and collects computed values.
    fn execute_phase_events(
        all_events: &[EventWithChildren],
        activity: EventActivity,
        event_ref: Option<&EventRef>,
        presence_map: &HashMap<String, Presence>,
        engine: &mut XfaScriptEngine,
        computed_values: &mut HashMap<SomPath, String>,
    ) {
        for (field_name, full_path, child_fields, script, static_presence, _page_area, _post_order) in
            all_events
        {
            if script.content_type != ScriptContentType::JavaScript
                || script.activity != activity
                || field_name.is_empty()
                || Self::is_effectively_inactive(full_path, *static_presence, presence_map)
            {
                continue;
            }
            if let Some(er) = event_ref {
                if script.event_ref != *er {
                    continue;
                }
            }

            engine.set_current_field_with_children(full_path, field_name, "", child_fields);
            engine.update_event_context(&activity, full_path, None);

            if let Ok(Some(value)) = engine.execute_script(script) {
                computed_values.insert(SomPath::new(full_path.clone()), value);
            }

            // Collect values set on child fields
            // Empty strings are valid per XFA spec (cleared fields, deselected exclGroups)
            for (child_name, _) in child_fields {
                if let Some((_, child_value)) = engine.get_child_field_value(child_name) {
                    computed_values.insert(SomPath::new(format!("{full_path}.{child_name}")), child_value);
                }
            }
        }
    }

    /// Check if a node is effectively inactive, considering both static
    /// presence and dynamic overrides set by earlier script phases.
    /// Per XFA 3.3 §10 p.407 Rule 1.
    fn is_effectively_inactive(
        full_path: &str,
        static_presence: Presence,
        dynamic_overrides: &HashMap<String, Presence>,
    ) -> bool {
        if static_presence == Presence::Inactive {
            return true;
        }
        // A node inside an inactive container is inactive with it (XFA 3.3
        // §10 Rule 1 and §2 "The presence property"), so every ancestor's
        // dynamic presence counts, keyed by its full SOM path.
        let mut path = full_path;
        loop {
            if dynamic_overrides.get(path) == Some(&Presence::Inactive) {
                return true;
            }
            match path.rsplit_once('.') {
                Some((parent, _)) => path = parent,
                None => return false,
            }
        }
    }

    /// Build a lookup keyed by full SOM path from presence_changes.
    fn build_full_path_presence_map(changes: &[PresenceChange]) -> HashMap<String, Presence> {
        changes
            .iter()
            .map(|change| (change.path.clone(), change.presence))
            .collect()
    }

    /// Find all events with child IDs and full SOM paths.
    ///
    /// Events are pushed in pre-order (document) position, which every phase
    /// but Phase 1 (initialize) uses directly. Each event's `post_order`
    /// field additionally records its rank in a post-order traversal
    /// (children's events, then the node's own), which Phase 1 sorts by; see
    /// that phase for why. `post_order_counter` is shared across the whole
    /// traversal and only incremented once a node's *entire* subtree
    /// (including the node's own events) has been assigned a rank.
    #[allow(clippy::too_many_arguments)]
    fn find_all_events_with_child_ids(
        nodes: &[XfaNode],
        events: &mut Vec<EventWithChildren>,
        parent_child_map: &HashMap<String, Vec<ChildNameIdPair>>,
        parent_path: Option<&str>,
        page_area: Option<&str>,
        post_order_counter: &mut usize,
    ) {
        for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
            // XFA 3.3 §10 p.407 Rule 1: When a container has presence=inactive,
            // it does not generate any of its normal calculations, validations,
            // or events. Skip this node and all its children.
            if node.get_presence() == Presence::Inactive {
                continue;
            }
            let name = node.name.clone().unwrap_or_default();

            // Build the full SOM path for this node
            let full_path = if !name.is_empty() {
                child_som_path(parent_path.unwrap_or(""), &name, index)
            } else {
                parent_path.unwrap_or("").to_string()
            };

            let children = parent_child_map.get(&full_path).cloned().unwrap_or_default();

            // A pageArea's own children (and their descendants, until the next
            // pageArea) belong to it: its master events must not run while a
            // different page is being evaluated. Mirrors `master_page_event_keys`.
            let this_page_area: Option<String> = if matches!(node.kind, XfaNodeKind::PageArea) {
                Some(name.clone())
            } else {
                page_area.map(str::to_string)
            };

            let node_events = parse_events_from_node(&node.children);
            // This node's own events are pushed now (pre-order), but their
            // post_order rank is only known once every descendant below has
            // been assigned one, so it is patched in after recursing.
            let own_start = events.len();
            for event in node_events {
                // Include both the name (for display) and full_path (for SOM lookup)
                events.push((
                    name.clone(),
                    full_path.clone(),
                    children.clone(),
                    event,
                    node.get_presence(),
                    this_page_area.clone(),
                    0, // post_order placeholder, patched below
                ));
            }
            let own_end = events.len();

            // Recurse with updated parent path
            let next_parent = if !name.is_empty() && node.kind.extends_som_path() {
                Some(full_path.as_str())
            } else {
                parent_path
            };

            Self::find_all_events_with_child_ids(
                &node.children,
                events,
                parent_child_map,
                next_parent,
                this_page_area.as_deref(),
                post_order_counter,
            );

            // All descendant events (recursed above) now have their post_order
            // rank; this node's own events rank immediately after, per XFA
            // 3.3 §10 Rule 3 read as post-order (see Phase 1 below).
            let rank = *post_order_counter;
            *post_order_counter += 1;
            for event in &mut events[own_start..own_end] {
                event.6 = rank;
            }
        }
    }

    /// Build and register the XFA SOM hierarchy in the scripting engine.
    fn build_and_register_xfa_som_hierarchy(
        xfa_nodes: &[XfaNode],
        prototypes: &Prototypes,
        engine: &mut XfaScriptEngine,
    ) {
        // Find the root subform container (e.g., "UBSForms")
        if let Some(root) = Self::find_root_subform(xfa_nodes) {
            // Register the root subform first
            let root_name = root.name.clone().unwrap_or_default();
            if !root_name.is_empty() {
                engine.register_xfa_node(
                    &root_name, &root_name, None, false, "", false, None, None, "visible", &[],
                );
                engine.init_access(&root_name, root.get_access());
            }

            // Per XFA 3.3 §3: SOM paths include the root subform.
            // All children of the root are registered under the root's name
            // (e.g. "UBSForms_66816.Page", not just "Page").
            // Unqualified name resolution is the scope walk in js_helpers
            // and resolveNode scoping, not path stripping.
            if !root_name.is_empty() {
                Self::install_repeatables(&SomPath::new(&root_name), prototypes, engine);
            }
            Self::register_nodes_recursive(&root.children, Some(&root_name), prototypes, engine, false);
        }
    }

    fn register_nodes_recursive(
        nodes: &[XfaNode],
        parent_path: Option<&str>,
        prototypes: &Prototypes,
        engine: &mut XfaScriptEngine,
        parent_is_exclgroup: bool,
    ) {
        for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
            let node_name = node.name.clone().unwrap_or_default();

            if node_name.is_empty() {
                Self::register_nodes_recursive(
                    &node.children,
                    parent_path,
                    prototypes,
                    engine,
                    parent_is_exclgroup,
                );
                continue;
            }

            // A pageSet and a pageArea are containers in the SOM path like a
            // subform (XFA 3.3 §3: `form1.pageSet.Page1.Footer`), so master
            // page content has the same path here as in the Form DOM, the
            // interactive form and the flattener.
            if matches!(node.kind, XfaNodeKind::PageSet | XfaNodeKind::PageArea) {
                let path = child_som_path(parent_path.unwrap_or(""), &node_name, index);
                engine.register_xfa_node(
                    &node_name,
                    &path,
                    parent_path,
                    false,
                    "",
                    false,
                    None,
                    None,
                    node.presence.as_str(),
                    &[],
                );
                if matches!(node.kind, XfaNodeKind::PageSet) {
                    engine.register_page_set(&path, parent_path);
                } else {
                    engine.register_page_area(&path, parent_path);
                }
                Self::register_nodes_recursive(&node.children, Some(&path), prototypes, engine, false);
                continue;
            }

            let is_subform = node.kind.is_subform();
            let is_field = node.kind.is_field();
            let is_exclgroup = node.kind.is_exclgroup();
            let is_draw = node.kind.is_draw();

            if !is_subform && !is_field && !is_exclgroup && !is_draw {
                Self::register_nodes_recursive(&node.children, parent_path, prototypes, engine, false);
                continue;
            }

            let full_path = child_som_path(parent_path.unwrap_or(""), &node_name, index);

            // A field starts with its saved `rawValue`, else its template
            // default (XFA 3.3 §4: an empty merge keeps the template value);
            // a draw's value is its template content (ACAV_001_DE's barcode
            // script reads its form id from the `txtformid` draw).
            let value = node
                .attributes
                .get("rawValue")
                .cloned()
                .or_else(|| (is_field || is_draw).then(|| node.template_value()).flatten())
                .unwrap_or_default();

            // Extract item values from <items> for exclGroup children
            // (XFA 3.3 §4 pp.195-197, §17 pp.758-759).
            let (item_key, off_value) = if parent_is_exclgroup {
                let values = node.check_button_values();
                (Some(values.on), Some(values.off))
            } else {
                (None, None)
            };

            // A choice-list field's own <items> (XFA 3.3 §17), seeding
            // its addItem/clearItems/... methods (§6).
            let choice_items = if is_field {
                node.extract_choice_list_items()
            } else {
                Vec::new()
            };

            engine.register_xfa_node(
                &node_name,
                &full_path,
                parent_path,
                is_field,
                &value,
                parent_is_exclgroup,
                item_key.as_deref(),
                off_value.as_deref(),
                node.presence.as_str(),
                &choice_items,
            );
            // `access` is a property of field, exclGroup and subform only
            // (XFA 3.3 §17); a draw has none.
            if is_draw {
                engine.mark_draw(&full_path, &value);
            } else {
                engine.init_access(&full_path, node.get_access());
            }
            if is_field || is_exclgroup {
                engine.init_caption(&full_path, node.caption_text().as_deref());
            }
            if node.is_globally_bound() {
                engine.join_global_binding(&full_path, &node_name);
            }

            if is_subform {
                Self::install_repeatables(&SomPath::new(&full_path), prototypes, engine);
            }

            if is_subform || is_exclgroup {
                Self::register_nodes_recursive(
                    &node.children,
                    Some(&full_path),
                    prototypes,
                    engine,
                    is_exclgroup,
                );
            }
        }
    }

    /// Give each repeatable subform declared under `parent` its occurrence
    /// limits, whether or not any instance of it exists (XFA 3.3 §9).
    fn install_repeatables(
        parent: &SomPath,
        prototypes: &Prototypes,
        engine: &mut XfaScriptEngine,
    ) {
        for (name, prototype) in prototypes.repeatables_under(parent) {
            engine.install_instance_manager(parent, name, &prototype.occur);
        }
    }

    /// Find the root content subform — delegates to the public helper.
    fn find_root_subform(xfa_nodes: &[XfaNode]) -> Option<&XfaNode> {
        super::find_root_subform(xfa_nodes)
    }

    /// Register the `<text>` variables of `<variables>` elements (named
    /// script objects are declared by `register_script_objects`).
    fn extract_and_register_translations(xfa_nodes: &[XfaNode], engine: &mut XfaScriptEngine) {
        let text_vars = super::collect_text_variables(xfa_nodes);
        for (name, value) in &text_vars {
            engine.register_field(name, name, value);
        }
    }
}

/// Apply a property a script wrote to the node it wrote it on. The full
/// path names exactly one node; a path that reaches none is a defect in the
/// paths the scripts ran under, logged with `what` rather than guessed at.
fn apply_property_change(
    nodes: &mut [XfaNode],
    path: &SomPath,
    what: &str,
    set: &mut dyn FnMut(&mut XfaNode),
) {
    match crate::xfa::scripting::som::walk_som_path_mut(nodes, path.as_str()) {
        Some(node) => set(node),
        None => log::warn!("{what} on {path} not applied: no node at that path"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xfa::{XfaNode, XfaNodeKind};
    use std::collections::HashMap;

    /// Build a minimal field node with an event script.
    ///
    /// `activity` is e.g. `"initialize"`, `"ready"`, `"calculate"`, `"validate"`.
    /// `event_ref` is an optional ref attribute (e.g. `"$form"` for form-ready).
    fn make_field_with_script(
        name: &str,
        script_source: &str,
        presence: &str,
        activity: &str,
        event_ref: Option<&str>,
    ) -> XfaNode {
        let mut attrs = HashMap::new();
        attrs.insert("name".to_string(), name.to_string());
        if !presence.is_empty() {
            attrs.insert("presence".to_string(), presence.to_string());
        }

        let mut field = XfaNode::new(XfaNodeKind::Field, attrs);

        let script_node = XfaNode::new(
            XfaNodeKind::Element {
                tag_name: "script".to_string(),
                text_content: Some(script_source.to_string()),
            },
            {
                let mut a = HashMap::new();
                a.insert(
                    "contentType".to_string(),
                    "application/x-javascript".to_string(),
                );
                a
            },
        );

        let mut event_attrs = HashMap::new();
        event_attrs.insert("activity".to_string(), activity.to_string());
        if let Some(r) = event_ref {
            event_attrs.insert("ref".to_string(), r.to_string());
        }

        let mut event_node = XfaNode::new(
            XfaNodeKind::Element {
                tag_name: "event".to_string(),
                text_content: None,
            },
            event_attrs,
        );
        event_node.children.push(script_node);
        field.children.push(event_node);
        field
    }

    fn make_field_with_init_script(name: &str, script_source: &str, presence: &str) -> XfaNode {
        make_field_with_script(name, script_source, presence, "initialize", None)
    }

    fn make_field_with_ready_script(name: &str, script_source: &str, presence: &str) -> XfaNode {
        make_field_with_script(name, script_source, presence, "ready", Some("$form"))
    }

    fn make_field_with_calculate_script(
        name: &str,
        script_source: &str,
        presence: &str,
    ) -> XfaNode {
        make_field_with_script(name, script_source, presence, "calculate", None)
    }

    fn make_field_with_validate_script(name: &str, script_source: &str) -> XfaNode {
        make_field_with_script(name, script_source, "", "validate", None)
    }

    /// Wrap fields in a minimal template > subform structure.
    fn wrap_in_template(fields: Vec<XfaNode>) -> Vec<XfaNode> {
        let mut subform = XfaNode::new(XfaNodeKind::Subform, {
            let mut a = HashMap::new();
            a.insert("name".to_string(), "Root".to_string());
            a
        });
        subform.children = fields;

        let mut template = XfaNode::new(XfaNodeKind::Template, HashMap::new());
        template.children.push(subform);
        vec![template]
    }

    /// Build a subform with the given name, children, and an `initialize`
    /// event of its own.
    fn make_subform_with_init_script(
        name: &str,
        children: Vec<XfaNode>,
        script_source: &str,
    ) -> XfaNode {
        let mut subform = XfaNode::new(XfaNodeKind::Subform, {
            let mut a = HashMap::new();
            a.insert("name".to_string(), name.to_string());
            a
        });

        let script_node = XfaNode::new(
            XfaNodeKind::Element {
                tag_name: "script".to_string(),
                text_content: Some(script_source.to_string()),
            },
            {
                let mut a = HashMap::new();
                a.insert(
                    "contentType".to_string(),
                    "application/x-javascript".to_string(),
                );
                a
            },
        );
        let mut event_attrs = HashMap::new();
        event_attrs.insert("activity".to_string(), "initialize".to_string());
        let mut event_node = XfaNode::new(
            XfaNodeKind::Element {
                tag_name: "event".to_string(),
                text_content: None,
            },
            event_attrs,
        );
        event_node.children.push(script_node);
        subform.children.push(event_node);

        subform.children.extend(children);
        subform
    }

    // =========================================================================
    // Test: inactive presence suppresses script execution (static)
    // =========================================================================

    #[test]
    fn test_inactive_presence_suppresses_initialize_event() {
        // An active field whose initialize script sets a value
        let active_field =
            make_field_with_init_script("activeField", r#"this.rawValue = "hello";"#, "");
        // An inactive field whose initialize script would set a value
        let inactive_field = make_field_with_init_script(
            "inactiveField",
            r#"this.rawValue = "should_not_appear";"#,
            "inactive",
        );

        let nodes = wrap_in_template(vec![active_field, inactive_field]);
        let result = ScriptExecutor::execute(&nodes);

        // Active field's script should have executed
        let active_found = result.computed_values.values().any(|v| v == "hello");
        assert!(
            active_found,
            "Active field's initialize script should have executed"
        );

        // Inactive field's script should NOT have executed
        let inactive_found = result
            .computed_values
            .values()
            .any(|v| v == "should_not_appear");
        assert!(
            !inactive_found,
            "Inactive field's initialize script must NOT execute per XFA 3.3 §10 Rule 1"
        );
    }

    #[test]
    fn test_inactive_presence_suppresses_form_ready_event() {
        // An active field with a form-ready script
        let active_field =
            make_field_with_ready_script("activeReady", r#"this.rawValue = "ready_value";"#, "");
        // An inactive field with a form-ready script
        let inactive_field = make_field_with_ready_script(
            "inactiveReady",
            r#"this.rawValue = "inactive_ready";"#,
            "inactive",
        );

        let nodes = wrap_in_template(vec![active_field, inactive_field]);
        let result = ScriptExecutor::execute(&nodes);

        let active_found = result.computed_values.values().any(|v| v == "ready_value");
        assert!(
            active_found,
            "Active field's form-ready script should have executed"
        );

        let inactive_found = result
            .computed_values
            .values()
            .any(|v| v == "inactive_ready");
        assert!(
            !inactive_found,
            "Inactive field's form-ready script must NOT execute per XFA 3.3 §10 Rule 1"
        );
    }

    // =========================================================================
    // Test: dynamic presence change across phases
    // =========================================================================

    #[test]
    fn test_dynamic_inactive_suppresses_later_phases() {
        // fieldA's initialize script sets fieldB's presence to inactive
        // using `fieldB.presence = "inactive"` via SOM global access.
        // fieldB has a form-ready script that should be suppressed because
        // fieldA's initialize script set it inactive before Phase 2 runs.
        let field_a =
            make_field_with_init_script("fieldA", r#"Root.fieldB.presence = "inactive";"#, "");
        let field_b = make_field_with_ready_script(
            "fieldB",
            r#"this.rawValue = "should_be_suppressed";"#,
            "",
        );

        // Wrap in a subform so fieldB is a sibling/child of the same parent
        let mut subform = XfaNode::new(XfaNodeKind::Subform, {
            let mut a = HashMap::new();
            a.insert("name".to_string(), "Root".to_string());
            a
        });
        subform.children = vec![field_a, field_b];

        let mut template = XfaNode::new(XfaNodeKind::Template, HashMap::new());
        template.children.push(subform);
        let nodes = vec![template];

        let result = ScriptExecutor::execute(&nodes);

        // fieldB's form-ready script should NOT have executed
        // because fieldA's initialize script set fieldB's presence to inactive
        // via the SOM hierarchy, and our cross-phase presence tracking suppresses it.
        let suppressed = result
            .computed_values
            .values()
            .any(|v| v == "should_be_suppressed");
        assert!(
            !suppressed,
            "fieldB's form-ready script must be suppressed after dynamic presence change to inactive"
        );
    }

    // =========================================================================
    // Test: children of inactive container are also suppressed
    // =========================================================================

    #[test]
    fn test_inactive_container_suppresses_children() {
        // An inactive subform containing a field with a script
        let child_field =
            make_field_with_init_script("childField", r#"this.rawValue = "child_value";"#, "");

        let mut inactive_subform = XfaNode::new(XfaNodeKind::Subform, {
            let mut a = HashMap::new();
            a.insert("name".to_string(), "InactiveGroup".to_string());
            a.insert("presence".to_string(), "inactive".to_string());
            a
        });
        inactive_subform.children.push(child_field);

        let nodes = wrap_in_template(vec![inactive_subform]);
        let result = ScriptExecutor::execute(&nodes);

        let child_found = result.computed_values.values().any(|v| v == "child_value");
        assert!(
            !child_found,
            "Children of inactive containers must NOT have events executed per XFA 3.3 §10 Rule 1"
        );
    }

    // =========================================================================
    // Test: calculate events run before initialize events
    // =========================================================================

    #[test]
    fn test_calculate_runs_before_initialize() {
        // Per XFA 3.3 §10 p.407: "All value calculations are done, then all
        // property calculations, then all validations, and then all initialize
        // events are fired."
        //
        // fieldA has a calculate script that sets its value.
        // fieldB has an initialize script that reads fieldA's value.
        // If calculate runs before initialize, fieldB should see fieldA's
        // calculated value.
        let field_a =
            make_field_with_calculate_script("fieldA", r#"this.rawValue = "calculated";"#, "");
        let field_b =
            make_field_with_init_script("fieldB", r#"this.rawValue = Root.fieldA.rawValue;"#, "");

        let nodes = wrap_in_template(vec![field_a, field_b]);
        let result = ScriptExecutor::execute(&nodes);

        let field_b_value = result.computed_values.get(&SomPath::new("Root.fieldB")).cloned();
        assert_eq!(
            field_b_value,
            Some("calculated".to_string()),
            "Initialize script should see calculate results (calculate runs first per XFA spec)"
        );
    }

    #[test]
    fn test_calculate_result_replaces_field_value() {
        // Per XFA 3.3 §10 p.380: "field: Replaces the value of the container
        // object" — the calculate script return value becomes the field value.
        let field = make_field_with_calculate_script("taxField", r#"this.rawValue = "42.50";"#, "");

        let nodes = wrap_in_template(vec![field]);
        let result = ScriptExecutor::execute(&nodes);

        let tax_value = result
            .computed_values
            .get(&SomPath::new("Root.taxField"))
            .cloned();
        assert_eq!(
            tax_value,
            Some("42.50".to_string()),
            "Calculate script result should replace field value"
        );
    }

    // =========================================================================
    // Test: validate scripts run after calculations, before initialize
    // =========================================================================

    #[test]
    fn test_validate_runs_after_calculate_before_initialize() {
        // Per XFA 3.3 §10 p.407: calculate → validate → initialize.
        // Validate scripts can read calculated values. They don't replace
        // field values, but they can set side-effect state.
        // Here we verify that a validate script can read a value that
        // was set by a calculate script, confirming correct ordering.
        let field_a =
            make_field_with_calculate_script("calcField", r#"this.rawValue = "100";"#, "");
        // Validate script reads calcField's value and marks a validation
        // result on a different field.
        let field_b = make_field_with_validate_script(
            "validField",
            r#"
                var v = Root.calcField.rawValue;
                if (v === "100") {
                    this.rawValue = "valid_" + v;
                }
            "#,
        );

        let nodes = wrap_in_template(vec![field_a, field_b]);
        let result = ScriptExecutor::execute(&nodes);

        // calcField should have its calculated value
        let calc_value = result
            .computed_values
            .get(&SomPath::new("Root.calcField"))
            .cloned();
        assert_eq!(
            calc_value,
            Some("100".to_string()),
            "Calculate script should have set calcField value"
        );

        // validField should have been set by the validate script that read calcField
        let valid_value = result
            .computed_values
            .get(&SomPath::new("Root.validField"))
            .cloned();
        assert_eq!(
            valid_value,
            Some("valid_100".to_string()),
            "Validate script should see calculated values (validate runs after calculate)"
        );
    }

    // =========================================================================
    // Test: intra-phase presence suppression within initialize
    // =========================================================================

    #[test]
    fn test_intra_phase_init_presence_suppression() {
        // Per XFA 3.3 §10 Rule 1: "The property is inspected at the moment
        // that the calculation, validation, or event would be triggered."
        //
        // fieldA's initialize script sets fieldB to inactive.
        // fieldB's initialize script should NOT execute because fieldA's
        // script already set it inactive, and presence is checked at the
        // moment fieldB's event would fire (within the same init phase).
        let field_a =
            make_field_with_init_script("fieldA", r#"Root.fieldB.presence = "inactive";"#, "");
        let field_b =
            make_field_with_init_script("fieldB", r#"this.rawValue = "should_not_run";"#, "");

        let nodes = wrap_in_template(vec![field_a, field_b]);
        let result = ScriptExecutor::execute(&nodes);

        let suppressed = result
            .computed_values
            .values()
            .any(|v| v == "should_not_run");
        assert!(
            !suppressed,
            "fieldB's init script must be suppressed by intra-phase presence change from fieldA"
        );
    }

    // =========================================================================
    // Test: initialize events run in post-order (children before parent)
    // =========================================================================

    #[test]
    fn test_child_initialize_runs_before_parent_initialize() {
        // Per XFA 3.3 §10 Rule 3, read as post-order: a child field's own
        // initialize (an unconditional default) must run *before* its parent
        // subform's initialize (an override), matching AAOV's real
        // ffDesSignature/DYN_Signature pattern -- otherwise the parent's
        // override would be clobbered by the child's default running after it.
        let child = make_field_with_init_script("child", r#"this.rawValue = "child_default";"#, "");
        let subform = make_subform_with_init_script(
            "S",
            vec![child],
            r#"Root.S.child.rawValue = "parent_override";"#,
        );

        let nodes = wrap_in_template(vec![subform]);
        let result = ScriptExecutor::execute(&nodes);

        let value = result
            .computed_values
            .get(&SomPath::new("Root.S.child"));
        assert_eq!(
            value.map(String::as_str),
            Some("parent_override"),
            "parent subform's initialize must run after (and override) its child's, got {result:?}"
        );
    }

    #[test]
    fn test_sibling_initialize_order_is_unchanged_document_order() {
        // Post-order only reorders a node's own events relative to its
        // descendants'; two siblings with no ancestor/descendant relationship
        // still run in document order, with the later one winning when both
        // write the same field (unchanged from before this change).
        let field_a =
            make_field_with_init_script("fieldA", r#"Root.target.rawValue = "from_a";"#, "");
        let field_b =
            make_field_with_init_script("fieldB", r#"Root.target.rawValue = "from_b";"#, "");
        let target = make_field_with_init_script("target", "", "");

        let nodes = wrap_in_template(vec![field_a, field_b, target]);
        let result = ScriptExecutor::execute(&nodes);

        assert_eq!(
            result
                .computed_values
                .get(&SomPath::new("Root.target"))
                .map(String::as_str),
            Some("from_b"),
            "later sibling in document order should still win, got {result:?}"
        );
    }

    #[test]
    fn test_child_initialize_runs_even_when_parent_hides_itself() {
        // Per XFA 3.3 §10 Rule 1, presence is inspected "at the moment" an
        // event would fire. With post-order, the child's initialize fires
        // before the parent's own initialize (which sets `this.presence =
        // "inactive"`), so the child's script must still have run.
        let child = make_field_with_init_script("child", r#"this.rawValue = "child_ran";"#, "");
        let subform =
            make_subform_with_init_script("S", vec![child], r#"this.presence = "inactive";"#);

        let nodes = wrap_in_template(vec![subform]);
        let result = ScriptExecutor::execute(&nodes);

        let ran = result
            .computed_values
            .values()
            .any(|v| v == "child_ran");
        assert!(
            ran,
            "child's initialize must run before its parent's own initialize hides it, got {result:?}"
        );
    }
}
