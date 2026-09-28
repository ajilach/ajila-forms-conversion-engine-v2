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

use crate::flattened::PageOverrides;
use crate::xfa::scripting::{
    EventActivity, EventRef, Presence, ScriptContentType, SomPath, XfaScript, XfaScriptEngine,
    parse_events_from_node, wrap_script_object,
};
use crate::xfa::{XfaNode, XfaNodeKind};
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
///   (initialize events, see that phase for why). Every other phase keeps
///   using the vector's own (pre-order/document) position.
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
    /// Presence changes to apply to nodes (name, optional id, presence)
    pub presence_changes: Vec<(String, Option<String>, Presence)>,
}

/// The live script engine, kept after the document-wide pass so that
/// master-page scripts can be evaluated again once the page count is known.
///
/// Page-dependent scripts -- "Pagina 2 di 3", a barcode that encodes the page
/// number, a block that appears on the first page only -- cannot be evaluated
/// during the first pass, because how many pages there are is a *result* of
/// laying the body out. They are therefore run once more per page, against the
/// same engine (so the form's script objects and variables are still there),
/// with `xfa.layout` and the pageArea's `index` pointing at that page.
pub struct LayoutScripts {
    engine: XfaScriptEngine,
    events: Vec<EventWithChildren>,
    /// Which events belong to a master page: (node name, full SOM path).
    master_keys: HashSet<(String, String)>,
}

impl LayoutScripts {
    /// Feed the master-page engine the body's settled values.
    ///
    /// This engine was built once, at the document-wide pass, and normally
    /// only ever re-evaluates its own master-page scripts. An interaction
    /// changes the body's values in a *different* engine (`XfaForm`'s), so
    /// without this the master engine would keep computing footers like
    /// "page 1 of 3" against pre-interaction values -- correct at open time,
    /// stale after the first mutation.
    pub fn sync_values(&mut self, values: &HashMap<SomPath, String>) {
        for (path, value) in values {
            self.engine.update_field_value(path.as_str(), value);
        }
    }

    /// Evaluate the master-page scripts as they stand on one page.
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
    pub fn evaluate(&mut self, page_area: &str, page_index: usize, page_count: usize) -> PageOverrides {
        self.engine.set_layout_context(page_index, page_count);

        // Clear the master fields before each page.
        //
        // These scripts guard themselves with `if (!this.rawValue)`, so a value
        // left over from the previous page -- or from the document-wide pass --
        // would stop them recomputing and page 3 would be stamped "Pagina 1/1".
        // Emptying them is what makes each page ask the question again; a field
        // whose script does not refill it falls back to the document-wide value,
        // because empty results are dropped from the overrides below.
        //
        // Presence is reset the same way, back to what the template declared:
        // a page's own scripts (or another node's script reaching into this
        // one, e.g. `mp.FIM_Off.presence = "hidden"`) must start from the
        // template baseline each time, not from whatever the previous page's
        // evaluation -- or the document-wide phase-1 pass -- left it at.
        for (name, full_path) in &self.master_keys {
            self.engine.update_field_value(full_path, "");
            self.engine.update_field_value(name, "");
            self.engine.reset_presence_to_template(full_path);
        }

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
            if !self
                .master_keys
                .contains(&(field_name.clone(), full_path.clone()))
            {
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

            self.engine
                .set_current_field_with_children(full_path, field_name, "", child_fields);
            self.engine
                .update_event_context(&script.activity, full_path, None);
            if let Err(e) = self.engine.execute_script(script) {
                log::debug!("[LAYOUT PASS] page {page_index} {full_path}: {e}");
            }

            if let Some(presence) = self.engine.get_current_field_presence() {
                overrides.presence.insert(field_name.clone(), presence);
                overrides.presence.insert(full_path.clone(), presence);
            }
            for (child_name, _) in child_fields {
                if let Some((_, presence)) = self.engine.get_child_field_presence(child_name) {
                    overrides.presence.insert(child_name.clone(), presence);
                }
            }
        }

        // Only what this page actually produced. An empty result is the
        // absence of an answer, not an answer of "", and must let the
        // document-wide value through.
        for (name, value) in self.engine.get_all_som_field_values() {
            if !value.is_empty() {
                overrides.values.insert(SomPath::new(name), value);
            }
        }
        for (path, value) in self.engine.get_all_som_field_values_by_path() {
            if !value.is_empty() {
                overrides.values.insert(SomPath::new(path), value);
            }
        }

        // Presence written to a master node *other* than the one whose script
        // ran (e.g. `mp.FIM_Off.presence = "hidden"` set from a sibling's
        // initialize event) is not captured by `get_current_field_presence`
        // above, which only reads the executing node (`_xfa_this_`). Compare
        // every master node against its own template baseline directly
        // instead of `get_all_som_presence_changes`, which diffs against the
        // engine-wide `initial_presence` map -- rebased by the document-wide
        // pass (`ScriptExecutor::execute_internal`'s `update_initial_presence`
        // call) to a value that has nothing to do with which page is being
        // laid out.
        let master_paths: Vec<String> =
            self.master_keys.iter().map(|(_, path)| path.clone()).collect();
        for (path, presence) in self.engine.presence_vs_template(&master_paths) {
            let p: Presence = presence.parse().unwrap_or_default();
            if let Some(leaf) = path.rsplit('.').next() {
                overrides.presence.insert(leaf.to_string(), p);
            }
            overrides.presence.insert(path, p);
        }

        overrides
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
    pub fn execute(xfa_nodes: &[XfaNode]) -> ScriptExecutionResult {
        Self::execute_with_layout(xfa_nodes).0
    }

    /// As [`ScriptExecutor::execute`], but also hands back the live engine so
    /// that page-dependent master-page scripts can be re-evaluated once the
    /// page count is known. `None` when script execution failed, in which case
    /// there is nothing to re-evaluate either.
    pub fn execute_with_layout(
        xfa_nodes: &[XfaNode],
    ) -> (ScriptExecutionResult, Option<LayoutScripts>) {
        match Self::execute_internal(xfa_nodes) {
            Ok((result, layout)) => (result, Some(layout)),
            Err(e) => {
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
        xfa_nodes: &[XfaNode],
    ) -> Result<(ScriptExecutionResult, LayoutScripts), String> {
        let mut computed_values = HashMap::new();
        let mut presence_changes: Vec<(String, Option<String>, Presence)> = Vec::new();
        let mut engine = XfaScriptEngine::new();

        // Extract and register translation objects from the XFA
        // (includes Footer_Line_txtlanguage, Footer_Line_txtformid, etc.)
        Self::extract_and_register_translations(xfa_nodes, &mut engine);

        // Build the XFA SOM hierarchy for unqualified references
        Self::build_and_register_xfa_som_hierarchy(xfa_nodes, &mut engine);

        // Per XFA 3.3 §10: a named script object is "registered with the
        // subform" that declares it, so `subform.scriptName` must resolve,
        // not only the bare global `extract_and_register_translations`
        // already exposed it as.
        if let Some(root) = Self::find_root_subform(xfa_nodes) {
            let root_name = root.name.clone().unwrap_or_default();
            if !root_name.is_empty() {
                for (script_name, _content, owner_path) in
                    crate::xfa::collect_variable_script_owners(&root_name, &root.children)
                {
                    engine.attach_script_object_to_subform(&owner_path, &script_name);
                }
            }
        }

        // Build parent-child map for setting up `this.childField` access
        let parent_child_map = Self::build_parent_child_map_with_ids(xfa_nodes);

        // Find all events recursively, starting from root
        let mut subform_counters: HashMap<String, usize> = HashMap::new();
        let mut all_events = Vec::new();
        let mut post_order_counter: usize = 0;
        Self::find_all_events_with_child_ids(
            xfa_nodes,
            &mut all_events,
            &parent_child_map,
            &mut subform_counters,
            None, // Start with no parent path
            None, // Start outside any pageArea
            &mut post_order_counter,
        );

        // Warn about FormCalc scripts that cannot be executed.
        // Only JavaScript is supported; FormCalc scripts are silently skipped.
        let formcalc_count = all_events
            .iter()
            .filter(|(_, _, _, script, _, _, _)| script.content_type == ScriptContentType::FormCalc)
            .count();
        if formcalc_count > 0 {
            log::warn!(
                "{} FormCalc script(s) found but not executed \
                 (only JavaScript is supported). Results may be incomplete.",
                formcalc_count
            );
        }

        // Register all event scripts in the _xfa_event_scripts_ registry
        // so that execEvent() can find them at runtime.
        // Per XFA 3.3 §10 pp.407-409 Rule 3.
        for (_, full_path, _, script, _, _, _) in &all_events {
            if script.content_type == ScriptContentType::JavaScript {
                engine.register_event_script(
                    full_path,
                    script.activity.activity_name(),
                    &script.source,
                );
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

            for (field_name, full_path, child_fields, script, _presence, _page_area, _post_order) in
                &calc_events
            {
                engine.set_current_field_with_children(full_path, field_name, "", child_fields);
                engine.update_event_context(&EventActivity::Calculate, full_path, None);

                let old_value = computed_values
                    .get(&SomPath::new(full_path.clone()))
                    .cloned();

                if let Ok(Some(value)) = engine.execute_script(script) {
                    // Per XFA 3.3 §10 p.380: the calculate result replaces the
                    // container's value.
                    if old_value.as_ref() != Some(&value) {
                        any_changed = true;
                    }
                    computed_values.insert(SomPath::new(field_name.clone()), value.clone());
                    computed_values.insert(SomPath::new(full_path.clone()), value);
                }

                // Collect values set on fields during calculation (side effects)
                let calc_som_values = engine.get_all_som_field_values();
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

        // Phase 0b: Execute validate scripts
        // Per XFA 3.3 §10 p.407: validations run after calculations, before
        // initialize events. Validate scripts are executed but their results
        // are informational (validation state), not value-replacing.
        // However, validate scripts may set rawValue as a side effect.
        for (field_name, full_path, child_fields, script, _presence, _page_area, _post_order) in
            &all_events
        {
            if script.content_type == ScriptContentType::JavaScript
                && script.activity == EventActivity::Validate
            {
                engine.set_current_field_with_children(full_path, field_name, "", child_fields);
                engine.update_event_context(&EventActivity::Validate, full_path, None);
                let _ = engine.execute_script(script);

                // Collect any side-effect values set during validation
                let validate_som_values = engine.get_all_som_field_values();
                for (vf_name, vf_value) in validate_som_values {
                    computed_values.insert(SomPath::new(vf_name), vf_value);
                }
            }
        }

        // Phase 1: Execute initialize events
        // Per XFA 3.3 §10 p.407 Rule 1: "The property is inspected at the
        // moment that the calculation, validation, or event would be triggered."
        // We maintain a running presence map so that if an earlier init script
        // sets a sibling's presence to inactive, subsequent init events for
        // that sibling are suppressed within the same phase.
        //
        // Per XFA 3.3 §10 p.407 Rule 3, initialize events fire "in order of
        // depth-first traversal of the Form DOM", which does not by itself
        // say whether a container fires before or after its descendants. A
        // real UBS form (AAOV) pins the reading down: a leaf field's
        // initialize sets an unconditional default caption, and an ancestor
        // subform's initialize overwrites it with a caption chosen from a
        // sibling dropdown's selection (also set by that dropdown's own
        // initialize). Adobe's rendering shows the dropdown-derived caption,
        // which only happens if descendants' initialize events run before
        // their ancestor's -- i.e. a *post-order* traversal, children (in
        // document order) before the container. We therefore iterate
        // `initialize_events`, sorted by the post-order rank
        // `find_all_events_with_child_ids` assigned each event, rather than
        // `all_events`'s own pre-order position (every other phase keeps
        // using document/pre-order, which Rule 3 does not govern).
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
                    presence_changes.push((field_name.clone(), None, presence));
                }

                // Collect presence values set on child fields
                for (child_name, child_id) in child_fields {
                    if let Some((id, presence)) = engine.get_child_field_presence(child_name) {
                        let storage_id = if !id.is_empty() {
                            Some(id)
                        } else if !child_id.is_empty() {
                            Some(child_id.clone())
                        } else {
                            None
                        };
                        presence_changes.push((child_name.clone(), storage_id, presence));
                    }
                }

                // Collect values from initialize scripts
                // Empty strings are valid per XFA spec (cleared fields, deselected exclGroups)
                let init_som_values = engine.get_all_som_field_values();
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
        let mut presence_map_after_phase1 =
            Self::build_full_path_presence_map(&all_events, &presence_changes);
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
        let mut presence_map_after_phase2 =
            Self::build_full_path_presence_map(&all_events, &presence_changes);
        presence_map_after_phase2.extend(dynamic_presence_overrides);

        // Phase 3: Execute layout-ready JavaScript events
        Self::execute_phase_events(
            &all_events,
            EventActivity::Ready,
            Some(&EventRef::Layout),
            &presence_map_after_phase2,
            &mut engine,
            &mut computed_values,
        );

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

        // Phase 5: Collect all values from SOM hierarchy
        // Empty strings are valid per XFA spec (cleared fields, deselected exclGroups)
        let som_values = engine.get_all_som_field_values();
        for (field_name, value) in som_values {
            computed_values
                .entry(SomPath::new(field_name.clone()))
                .or_insert(value);
        }

        // Also store values keyed by full SOM path to avoid collisions
        // between fields with the same short name in different subforms.
        // This is needed for xfa:embed URI resolution (id_to_field now maps
        // element IDs to full SOM paths).
        let som_values_by_path = engine.get_all_som_field_values_by_path();
        for (full_path, value) in som_values_by_path {
            computed_values
                .entry(SomPath::new(full_path))
                .or_insert(value);
        }

        let master_keys = Self::master_page_event_keys(xfa_nodes);
        let layout = LayoutScripts {
            engine,
            events: all_events,
            master_keys,
        };

        Ok((
            ScriptExecutionResult {
                computed_values,
                presence_changes,
            },
            layout,
        ))
    }

    /// The name and SOM path of every node inside a `pageArea`.
    ///
    /// Master-page scripts are the ones that can depend on which page they are
    /// on, so they are the ones the layout pass re-runs; this is how they are
    /// told apart from the body's. Two masters that contain the same named
    /// subtree (a shared footer fragment) collapse to the same keys, which is
    /// correct: both want per-page evaluation.
    fn master_page_event_keys(nodes: &[XfaNode]) -> HashSet<(String, String)> {
        fn walk(
            nodes: &[XfaNode],
            parent_path: Option<&str>,
            inside_page_area: bool,
            out: &mut HashSet<(String, String)>,
        ) {
            for node in nodes {
                let name = node.name.clone().unwrap_or_default();
                // Mirrors find_all_events_with_child_ids: only subforms and
                // exclGroups extend a SOM path.
                let full_path = if name.is_empty() {
                    parent_path.unwrap_or("").to_string()
                } else {
                    match parent_path {
                        Some(p) => format!("{p}.{name}"),
                        None => name.clone(),
                    }
                };
                let inside = inside_page_area || matches!(node.kind, XfaNodeKind::PageArea);
                if inside {
                    out.insert((name.clone(), full_path.clone()));
                }
                let next_parent =
                    if !name.is_empty() && (node.kind.is_subform() || node.kind.is_exclgroup()) {
                        Some(full_path.as_str())
                    } else {
                        parent_path
                    };
                walk(&node.children, next_parent, inside, out);
            }
        }

        let mut out = HashSet::new();
        walk(nodes, None, false, &mut out);
        out
    }

    /// Apply presence changes to a mutable XFA node tree.
    ///
    /// This should be called on a cloned tree to preserve the original.
    pub fn apply_presence_changes(
        nodes: &mut [XfaNode],
        changes: &[(String, Option<String>, Presence)],
    ) {
        for (name, id, presence) in changes {
            // Try to find by ID first (more specific)
            if let Some(id_val) = id
                && Self::apply_presence_by_id(nodes, id_val, *presence)
            {
                continue;
            }
            // Fall back to finding by name
            Self::apply_presence_by_name(nodes, name, *presence);
        }
    }

    /// Recursively find a node by ID and set its presence
    fn apply_presence_by_id(nodes: &mut [XfaNode], id: &str, presence: Presence) -> bool {
        for node in nodes {
            if node.attributes.get("id").map(|s| s.as_str()) == Some(id) {
                node.set_presence(presence);
                return true;
            }
            if Self::apply_presence_by_id(&mut node.children, id, presence) {
                return true;
            }
        }
        false
    }

    /// Recursively find ALL nodes by name and set their presence.
    ///
    /// When a matching node is found, its children are **not** searched for
    /// further matches. This prevents a common XFA pattern — a subform named
    /// "X" containing a field also named "X" — from having the child field's
    /// presence inadvertently overwritten when a script only intended to change
    /// the parent subform's visibility.
    fn apply_presence_by_name(nodes: &mut [XfaNode], name: &str, presence: Presence) -> bool {
        let mut found = false;
        for node in nodes {
            if node.name.as_deref() == Some(name) {
                node.set_presence(presence);
                found = true;
                // Do NOT recurse into children — a same-named child (e.g.
                // field "Company" inside subform "Company") is a different
                // entity whose presence should not be affected.
            } else if Self::apply_presence_by_name(&mut node.children, name, presence) {
                found = true;
            }
        }
        found
    }

    // ========================================================================
    // Helper functions moved from Flattened
    // ========================================================================

    /// Build a parent-child map that tracks both child names AND their unique IDs.
    fn build_parent_child_map_with_ids(
        xfa_nodes: &[XfaNode],
    ) -> HashMap<String, Vec<(String, String)>> {
        let mut parent_child_map: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut subform_counters: HashMap<String, usize> = HashMap::new();

        fn collect_children_with_ids(
            nodes: &[XfaNode],
            parent_key: Option<&str>,
            map: &mut HashMap<String, Vec<(String, String)>>,
            counters: &mut HashMap<String, usize>,
        ) {
            for node in nodes {
                let node_name = node.name.clone().unwrap_or_default();
                let node_id = node.attributes.get("id").cloned().unwrap_or_default();

                if let Some(parent) = parent_key {
                    let is_field = node.kind.is_field();

                    if is_field && !node_name.is_empty() {
                        map.entry(parent.to_string())
                            .or_default()
                            .push((node_name.clone(), node_id.clone()));
                    }
                }

                let is_subform = node.kind.is_subform();
                let is_exclgroup = node.kind.is_exclgroup();

                if (is_subform || is_exclgroup) && !node_name.is_empty() {
                    let key = if !node_id.is_empty() {
                        format!("{}#{}", node_name, node_id)
                    } else {
                        let count = counters.entry(node_name.clone()).or_insert(0);
                        let key = format!("{}[{}]", node_name, *count);
                        *count += 1;
                        key
                    };
                    collect_children_with_ids(&node.children, Some(&key), map, counters);
                } else if !is_subform && !is_exclgroup {
                    collect_children_with_ids(&node.children, parent_key, map, counters);
                }
            }
        }

        collect_children_with_ids(
            xfa_nodes,
            None,
            &mut parent_child_map,
            &mut subform_counters,
        );
        parent_child_map
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
                computed_values.insert(SomPath::new(field_name.clone()), value);
            }

            // Collect values set on child fields
            // Empty strings are valid per XFA spec (cleared fields, deselected exclGroups)
            for (child_name, child_id) in child_fields {
                if let Some((id, child_value)) = engine.get_child_field_value(child_name) {
                    let storage_key = if !id.is_empty() { id } else { child_id.clone() };

                    if !storage_key.is_empty() {
                        computed_values
                            .insert(SomPath::new(storage_key.clone()), child_value.clone());
                    }
                    computed_values.insert(SomPath::new(child_name.clone()), child_value);
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
        // Look up by full SOM path to avoid collisions between fields
        // with the same leaf name in different subforms.
        if let Some(&p) = dynamic_overrides.get(full_path) {
            return p == Presence::Inactive;
        }
        false
    }

    /// Build a lookup keyed by full SOM path from presence_changes.
    /// Uses `all_events` to resolve leaf names → full paths.
    fn build_full_path_presence_map(
        all_events: &[EventWithChildren],
        changes: &[(String, Option<String>, Presence)],
    ) -> HashMap<String, Presence> {
        // Build a leaf→full_path lookup from all_events.
        // Note: if multiple events share a leaf, the last full_path wins;
        // that's fine because name collisions are the problem we're solving
        // and full SOM paths are unique.
        let mut leaf_to_full: HashMap<&str, &str> = HashMap::new();
        for (name, full_path, children, _, _, _, _) in all_events {
            if !name.is_empty() {
                leaf_to_full.insert(name.as_str(), full_path.as_str());
            }
            for (child_name, _) in children {
                // Children paths are relative to the parent's full path
                leaf_to_full
                    .entry(child_name.as_str())
                    .or_insert(full_path.as_str());
            }
        }

        let mut map = HashMap::new();
        for (name, _id, presence) in changes {
            // If the name looks like a full path already (contains '.'), use it as-is.
            // Otherwise resolve via the leaf→full mapping.
            let key = if name.contains('.') {
                name.clone()
            } else if let Some(&full) = leaf_to_full.get(name.as_str()) {
                // Build child full path: parent_full_path.child_name
                if full.ends_with(name.as_str()) {
                    full.to_string()
                } else {
                    format!("{}.{}", full, name)
                }
            } else {
                name.clone()
            };
            map.insert(key, *presence);
        }
        map
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
        subform_counters: &mut HashMap<String, usize>,
        parent_path: Option<&str>,
        page_area: Option<&str>,
        post_order_counter: &mut usize,
    ) {
        for node in nodes {
            // XFA 3.3 §10 p.407 Rule 1: When a container has presence=inactive,
            // it does not generate any of its normal calculations, validations,
            // or events. Skip this node and all its children.
            if node.get_presence() == Presence::Inactive {
                continue;
            }
            let name = node.name.clone().unwrap_or_default();
            let node_id = node.attributes.get("id").cloned().unwrap_or_default();

            let is_subform = node.kind.is_subform();
            let is_exclgroup = node.kind.is_exclgroup();

            // Build the full SOM path for this node
            let full_path = if !name.is_empty() {
                match parent_path {
                    Some(p) => format!("{}.{}", p, name),
                    None => name.clone(),
                }
            } else {
                parent_path.unwrap_or("").to_string()
            };

            let key = if !node_id.is_empty() {
                format!("{}#{}", name, node_id)
            } else if (is_subform || is_exclgroup) && !name.is_empty() {
                let count = subform_counters.entry(name.clone()).or_insert(0);
                let key = format!("{}[{}]", name, *count);
                *count += 1;
                key
            } else {
                name.clone()
            };

            let children = parent_child_map.get(&key).cloned().unwrap_or_default();

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
            let next_parent = if !name.is_empty() && (is_subform || is_exclgroup) {
                Some(full_path.as_str())
            } else {
                parent_path
            };

            Self::find_all_events_with_child_ids(
                &node.children,
                events,
                parent_child_map,
                subform_counters,
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
    fn build_and_register_xfa_som_hierarchy(xfa_nodes: &[XfaNode], engine: &mut XfaScriptEngine) {
        #[allow(clippy::too_many_arguments)]
        fn register_nodes_recursive(
            nodes: &[XfaNode],
            parent_path: Option<&str>,
            engine: &mut XfaScriptEngine,
            parent_is_exclgroup: bool,
            // The nearest enclosing `pageSet`'s name, so a `pageArea` found
            // inside it can be attached as `pageSet.<name>` (and, as a
            // class-name fallback, `pageSet.pageArea`) -- the shape a
            // Designer-authored script reaches for (XFA 3.3 §10; see
            // `register_page_set`/`register_page_area`).
            current_page_set: Option<&str>,
            // `Some(pageAreaName)` only while registering that pageArea's own
            // *direct* children (a header draw, a footer subform, a
            // conditional alternate) -- these become properties of the
            // pageArea object (`mp.FIM_On`). Grandchildren are reached
            // through that child's own subform-property mechanism instead,
            // so this is cleared before recursing into a subform's children.
            direct_page_area: Option<&str>,
        ) {
            for node in nodes {
                let node_name = node.name.clone().unwrap_or_default();

                if node_name.is_empty() {
                    register_nodes_recursive(
                        &node.children,
                        parent_path,
                        engine,
                        parent_is_exclgroup,
                        current_page_set,
                        direct_page_area,
                    );
                    continue;
                }

                if matches!(node.kind, XfaNodeKind::PageSet) {
                    // Per XFA 3.3 §10 "Instantiation of Named Script Objects"
                    // and §3 "Reference by Class": a pageSet is addressed by
                    // name from its parent subform, or by the class name
                    // `pageSet` when a script does not care which (Designer
                    // commonly emits `form1.pageSet.<pageAreaName>`).
                    engine.register_page_set(&node_name, parent_path);
                    register_nodes_recursive(
                        &node.children,
                        parent_path,
                        engine,
                        false,
                        Some(&node_name),
                        None,
                    );
                    continue;
                }

                let is_subform = node.kind.is_subform();
                let is_field = node.kind.is_field();
                let is_exclgroup = node.kind.is_exclgroup();
                let is_draw = node.kind.is_draw();

                if !is_subform && !is_field && !is_exclgroup && !is_draw {
                    // A pageArea is not a field, but templates address it by
                    // name to ask which page they are on (`MP.index`), so it
                    // needs an object of its own. Its children keep the
                    // enclosing path: a pageArea does not extend SOM paths.
                    if matches!(node.kind, XfaNodeKind::PageArea) {
                        engine.register_page_area(&node_name, current_page_set);
                        register_nodes_recursive(
                            &node.children,
                            parent_path,
                            engine,
                            false,
                            current_page_set,
                            Some(&node_name),
                        );
                    } else {
                        register_nodes_recursive(
                            &node.children,
                            parent_path,
                            engine,
                            false,
                            current_page_set,
                            direct_page_area,
                        );
                    }
                    continue;
                }

                let full_path = match parent_path {
                    Some(p) => format!("{}.{}", p, node_name),
                    None => node_name.clone(),
                };

                let value = node.attributes.get("rawValue").cloned().unwrap_or_default();

                // Extract item values from <items> for exclGroup children
                // (XFA 3.3 §4 pp.195-197, §17 pp.758-759).
                let (item_key, off_value) = if parent_is_exclgroup {
                    node.extract_item_values()
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

                if let Some(pa) = direct_page_area {
                    engine.attach_to_page_area(pa, &node_name, &full_path);
                }

                if is_subform || is_exclgroup {
                    register_nodes_recursive(
                        &node.children,
                        Some(&full_path),
                        engine,
                        is_exclgroup,
                        current_page_set,
                        None,
                    );
                }
            }
        }

        // Find the root subform container (e.g., "UBSForms")
        if let Some(root) = Self::find_root_subform(xfa_nodes) {
            // Register the root subform first
            let root_name = root.name.clone().unwrap_or_default();
            if !root_name.is_empty() {
                engine.register_xfa_node(
                    &root_name, &root_name, None, false, "", false, None, None, "visible", &[],
                );
            }

            // Per XFA 3.3 §3: SOM paths include the root subform.
            // All children of the root are registered under the root's name
            // (e.g. "UBSForms_66816.Page", not just "Page").
            // Unqualified name resolution uses the _xfa_fields_ registry
            // and resolveNode scoping, not path stripping.
            register_nodes_recursive(&root.children, Some(&root_name), engine, false, None, None);
        }
    }

    /// Find the root content subform — delegates to the public helper.
    fn find_root_subform(xfa_nodes: &[XfaNode]) -> Option<&XfaNode> {
        super::find_root_subform(xfa_nodes)
    }

    /// Extract and register translations from <variables> elements.
    /// This handles both <text> variables (simple values) and <script> variables (code objects).
    fn extract_and_register_translations(xfa_nodes: &[XfaNode], engine: &mut XfaScriptEngine) {
        // Register <text> variables as fields using the shared helper
        let text_vars = super::collect_text_variables(xfa_nodes);
        for (name, value) in &text_vars {
            engine.register_field(name, name, value);
        }

        // Collect and register <script> variables using the shared helper
        let variable_scripts = super::collect_variable_scripts(xfa_nodes);

        // Register <script> variables as JavaScript objects
        // Per XFA 3.3 §10 pp. 376-378: named script objects expose all
        // top-level variables and functions as properties/methods.
        for (name, content) in &variable_scripts {
            let wrapped = wrap_script_object(name, content, false);
            let _ = engine.execute_variable_script(&wrapped);
        }
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

        let field_b_value = result.computed_values.get(&SomPath::new("fieldB")).cloned();
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
            .get(&SomPath::new("taxField"))
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
            .get(&SomPath::new("calcField"))
            .cloned();
        assert_eq!(
            calc_value,
            Some("100".to_string()),
            "Calculate script should have set calcField value"
        );

        // validField should have been set by the validate script that read calcField
        let valid_value = result
            .computed_values
            .get(&SomPath::new("validField"))
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
            .get(&SomPath::new("child".to_string()))
            .or_else(|| {
                result
                    .computed_values
                    .get(&SomPath::new("Root.S.child".to_string()))
            });
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
                .get(&SomPath::new("target".to_string()))
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
