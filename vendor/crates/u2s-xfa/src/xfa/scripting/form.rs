//! XFA Form Interface - High-level API for interacting with XFA forms
//!
//! This module provides the main `XfaForm` struct and node reference types
//! for working with XFA forms at a high level.

use super::dependency::DependencyTracker;
use super::engine::XfaScriptEngine;
use super::events::{
    EventActivity, EventRef, ListenScope, RunAt, ScriptContentType, XfaScript,
    parse_events_from_node,
};
use super::registry::{RegisteredScript, ScriptRegistry, ScriptType};
use super::som::{SomPath, SomResolver, child_som_path, concrete_segments, sibling_indices};
use super::state::Presence;
use crate::xfa::instances::{Prototypes, materialize_initial_instances};

use crate::flattened::{FieldAccess, Flattened, FlattenedNode, FlattenedNodeKind};
use crate::xfa::{Num, XfaNode, XfaNodeKind};

use std::collections::HashMap;
use std::sync::Arc;

/// Position and size of a node in the flattened layout
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeBounds {
    pub x: Num,
    pub y: Num,
    pub width: Num,
    pub height: Num,
}

/// Result of executing an event
#[derive(Debug, Default)]
pub struct EventResult {
    /// Whether any values changed
    pub values_changed: bool,
    /// Whether presence changed on any node
    pub presence_changed: bool,
    /// Whether a script changed any node's `access` (XFA 3.3 §17)
    pub access_changed: bool,
    /// SOM paths of fields whose values changed
    pub changed_fields: Vec<SomPath>,
    /// Whether an instance manager added, removed or moved an instance of a
    /// repeatable subform (XFA 3.3 §9): the Form DOM changed shape, so the
    /// layout must be rebuilt.
    pub instances_changed: bool,
    /// Set when an instance-manager call did nothing because it would have
    /// left the subform's `<occur>` limits (or named no instance): what was
    /// refused and why. A form does this silently; the host should say so.
    pub instance_limit_hit: Option<String>,
}

impl EventResult {
    /// Fold another step of the same interaction into this one.
    fn absorb(&mut self, other: EventResult) {
        self.values_changed |= other.values_changed;
        self.presence_changed |= other.presence_changed;
        self.access_changed |= other.access_changed;
        self.changed_fields.extend(other.changed_fields);
        self.instances_changed |= other.instances_changed;
        if self.instance_limit_hit.is_none() {
            self.instance_limit_hit = other.instance_limit_hit;
        }
    }
}

/// A node's access as the person filling the form meets it: its own `access`
/// tightened by its containers' (XFA 3.3 §2, §17). See
/// [`XfaForm::effective_access`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAccess {
    pub access: FieldAccess,
    /// The enclosing subform or exclusion group that imposes `access`, when
    /// the node's own `access` is less restrictive. `None` when the node's
    /// own value is what counts.
    pub inherited_from: Option<SomPath>,
}

/// What applying queued instance-manager changes did, accumulated over one
/// interaction.
#[derive(Debug, Default, Clone)]
struct InstanceReport {
    changed: bool,
    limit_hit: Option<String>,
}

/// How many rounds of instance changes triggering further instance changes
/// (an `initialize` script on a new instance adding another) are followed
/// before the form is declared to be looping.
const MAX_INSTANCE_ROUNDS: usize = 32;

/// A reference to a resolved node in the XFA form (immutable)
pub struct XfaNodeRef<'a> {
    /// The XFA node
    xfa_node: &'a XfaNode,
    /// The flattened node (if visible in layout)
    flattened_node: Option<&'a FlattenedNode>,
    /// The SOM path used to resolve this node
    som_path: SomPath,
    /// Whether the node AND all its ancestors have visible presence
    ancestors_visible: bool,
}

impl<'a> XfaNodeRef<'a> {
    /// Get the presence of this node
    pub fn presence(&self) -> Presence {
        self.xfa_node.get_presence()
    }

    /// Get the bounds (position and size) from the flattened layout
    pub fn bounds(&self) -> Option<NodeBounds> {
        self.flattened_node.map(|n| NodeBounds {
            x: n.x,
            y: n.y,
            width: n.width,
            height: n.height,
        })
    }

    /// Get the position (x, y) from the flattened layout
    pub fn position(&self) -> Option<(Num, Num)> {
        self.flattened_node.map(|n| (n.x, n.y))
    }

    /// Get the size (width, height) from the flattened layout
    pub fn size(&self) -> Option<(Num, Num)> {
        self.flattened_node.map(|n| (n.width, n.height))
    }

    /// Get the raw value of this node
    pub fn raw_value(&self) -> Option<String> {
        // First try flattened node
        if let Some(flat) = self.flattened_node {
            match &flat.kind {
                FlattenedNodeKind::Field { value, .. } if !value.is_empty() => {
                    return Some(value.clone());
                }
                FlattenedNodeKind::Text { content, .. } if !content.is_empty() => {
                    return Some(content.clone());
                }
                _ => {}
            }
        }

        // Fall back to XFA node attributes or value child
        if let Some(raw) = self.xfa_node.attributes.get("rawValue") {
            return Some(raw.clone());
        }

        // Try text_content for <text> variable elements
        if let XfaNodeKind::Element {
            text_content: Some(content),
            ..
        } = &self.xfa_node.kind
            && !content.is_empty()
        {
            return Some(content.clone());
        }

        Self::extract_value_from_xfa_node(self.xfa_node)
    }

    /// Get the name of this node
    pub fn name(&self) -> Option<&str> {
        self.xfa_node.name.as_deref()
    }

    /// Get the SOM path used to resolve this node
    pub fn som_path(&self) -> &SomPath {
        &self.som_path
    }

    /// Check if this node is visible based on its presence and ancestor presence
    pub fn is_visible(&self) -> bool {
        let own_presence = self.xfa_node.get_presence();
        if own_presence.should_skip_layout() {
            return false;
        }
        self.ancestors_visible
    }

    /// Get the XFA node kind
    pub fn kind(&self) -> &XfaNodeKind {
        &self.xfa_node.kind
    }

    /// Get a reference to the underlying XFA node (for debugging/advanced usage)
    pub fn xfa_node(&self) -> &XfaNode {
        self.xfa_node
    }

    /// Find a child element inside this node's `<ui>` element by tag name.
    ///
    /// XFA fields store their widget type as a child of the `<ui>` element
    /// (e.g. `<ui><checkButton .../></ui>`).  This helper traverses
    /// `children → ui → children` and returns the first match.
    fn find_ui_child(&self, target_tag: &str) -> Option<&XfaNode> {
        for child in &self.xfa_node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "ui"
            {
                for ui_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: ui_tag, ..
                    } = &ui_child.kind
                        && ui_tag == target_tag
                    {
                        return Some(ui_child);
                    }
                }
            }
        }
        None
    }

    /// Check if this is a dropdown/choicelist field
    pub fn is_dropdown(&self) -> bool {
        self.has_choice_list()
    }

    /// Check if this dropdown's items are populated via JavaScript
    pub fn is_script_populated_dropdown(&self) -> bool {
        if !self.is_dropdown() {
            return false;
        }

        for child in &self.xfa_node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "event"
            {
                for script_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: script_tag,
                        text_content,
                        ..
                    } = &script_child.kind
                        && script_tag == "script"
                        && let Some(content) = text_content
                        && content.contains("addItem")
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if this field has a choiceList UI element
    fn has_choice_list(&self) -> bool {
        self.find_ui_child("choiceList").is_some()
    }

    /// Check if this is a radio button field
    pub fn is_radio_button(&self) -> bool {
        self.get_check_button_shape() == Some("round".to_string())
    }

    /// Check if this is a checkbox field
    pub fn is_checkbox(&self) -> bool {
        if let Some(shape) = self.get_check_button_shape() {
            shape == "square"
        } else {
            self.has_check_button()
        }
    }

    /// Check if this is a button field
    pub fn is_button(&self) -> bool {
        self.has_button_ui()
    }

    fn has_button_ui(&self) -> bool {
        self.find_ui_child("button").is_some()
    }

    /// Check if this field has a checkButton UI element
    pub fn has_check_button(&self) -> bool {
        self.find_check_button().is_some()
    }

    fn get_check_button_shape(&self) -> Option<String> {
        self.find_check_button()
            .and_then(|cb| cb.attributes.get("shape").cloned())
    }

    fn find_check_button(&self) -> Option<&XfaNode> {
        self.find_ui_child("checkButton")
    }

    /// Get the dropdown options (display values and save values)
    pub fn dropdown_options(&self) -> Vec<(String, String)> {
        let mut display_items: Vec<String> = Vec::new();
        let mut save_items: Vec<String> = Vec::new();

        for child in &self.xfa_node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "items"
            {
                let is_save = child
                    .attributes
                    .get("save")
                    .map(|s| s == "1")
                    .unwrap_or(false);
                let items = Self::extract_items_values(child);

                if is_save {
                    save_items = items;
                } else if display_items.is_empty() {
                    display_items = items;
                } else if save_items.is_empty() {
                    save_items = items;
                }
            }
        }

        // Per XFA spec: when only one <items> element exists, display = save
        // regardless of which attribute was set.
        if display_items.is_empty() {
            display_items = save_items.clone();
        }
        if save_items.is_empty() {
            save_items = display_items.clone();
        }

        display_items.into_iter().zip(save_items).collect()
    }

    /// Get just the display values for dropdown options
    pub fn dropdown_display_values(&self) -> Vec<String> {
        self.dropdown_options()
            .into_iter()
            .map(|(d, _)| d)
            .collect()
    }

    /// Get just the save values for dropdown options
    pub fn dropdown_save_values(&self) -> Vec<String> {
        self.dropdown_options()
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    /// Get the number of dropdown options
    pub fn dropdown_option_count(&self) -> usize {
        self.dropdown_options().len()
    }

    /// Get the currently selected dropdown index (0-based)
    pub fn selected_dropdown_index(&self) -> Option<usize> {
        let current_value = self.raw_value()?;
        let save_values = self.dropdown_save_values();
        save_values.iter().position(|v| v == &current_value)
    }

    /// Get the currently selected dropdown display text
    pub fn selected_dropdown_text(&self) -> Option<String> {
        let idx = self.selected_dropdown_index()?;
        self.dropdown_display_values().get(idx).cloned()
    }

    fn extract_items_values(items_node: &XfaNode) -> Vec<String> {
        let mut values = Vec::new();
        for child in &items_node.children {
            match &child.kind {
                XfaNodeKind::Element {
                    tag_name,
                    text_content,
                } => match tag_name.as_str() {
                    "text" | "integer" | "decimal" | "float" | "boolean" | "date" | "dateTime"
                    | "time" => {
                        if let Some(content) = text_content {
                            values.push(content.clone());
                        } else {
                            values.push(String::new());
                        }
                    }
                    _ => {}
                },
                XfaNodeKind::Text { content } => {
                    values.push(content.clone());
                }
                _ => {}
            }
        }
        values
    }

    fn extract_value_from_xfa_node(node: &XfaNode) -> Option<String> {
        for child in &node.children {
            if matches!(child.kind, XfaNodeKind::Value) {
                for text_child in &child.children {
                    if let XfaNodeKind::Text { content } = &text_child.kind
                        && !content.is_empty()
                    {
                        return Some(content.clone());
                    }
                    if let XfaNodeKind::Element {
                        text_content: Some(content),
                        ..
                    } = &text_child.kind
                        && !content.is_empty()
                    {
                        return Some(content.clone());
                    }
                }
            }
        }
        None
    }
}

/// A mutable reference to a resolved node in the XFA form
pub struct XfaNodeRefMut<'a> {
    /// The XFA node (mutable)
    xfa_node: &'a mut XfaNode,
    /// The SOM path used to resolve this node
    som_path: SomPath,
    /// Reference to the persistent script engine (source of truth for field values)
    script_engine: &'a mut XfaScriptEngine,
}

impl<'a> XfaNodeRefMut<'a> {
    /// Get the presence of this node
    pub fn presence(&self) -> Presence {
        self.xfa_node.get_presence()
    }

    /// Set the presence of this node
    pub fn set_presence(&mut self, presence: Presence) {
        self.xfa_node.set_presence(presence);
    }

    /// Get the raw value of this node
    pub fn raw_value(&mut self) -> Option<String> {
        // Read from the engine (single source of truth)
        if let Some(value) = self.script_engine.get_field_value(&self.som_path) {
            return Some(value);
        }
        // Fallback to node attributes
        if let Some(raw) = self.xfa_node.attributes.get("rawValue") {
            return Some(raw.clone());
        }
        XfaNodeRef::extract_value_from_xfa_node(self.xfa_node)
    }

    /// Set the raw value of this node
    pub fn set_raw_value(&mut self, value: &str) {
        // Write to the engine (single source of truth)
        self.script_engine.update_field_value(&self.som_path, value);
        // Also update the XFA node
        Self::set_node_value(self.xfa_node, value);
    }

    /// Get the name of this node
    pub fn name(&self) -> Option<&str> {
        self.xfa_node.name.as_deref()
    }

    /// Get the SOM path used to resolve this node
    pub fn som_path(&self) -> &SomPath {
        &self.som_path
    }

    pub(crate) fn set_node_value(node: &mut XfaNode, value: &str) {
        for child in &mut node.children {
            if matches!(child.kind, XfaNodeKind::Value) {
                for text_child in &mut child.children {
                    if let XfaNodeKind::Text { content } = &mut text_child.kind {
                        *content = value.to_string();
                        return;
                    }
                    if let XfaNodeKind::Element { text_content, .. } = &mut text_child.kind {
                        *text_content = Some(value.to_string());
                        return;
                    }
                }
            }
        }
        node.attributes
            .insert("rawValue".to_string(), value.to_string());
    }
}

/// High-level interface for interacting with an XFA form
pub struct XfaForm {
    /// The XFA node tree
    nodes: Vec<XfaNode>,
    /// The current flattened layout
    flattened: Flattened,
    /// SOM resolver for node lookups
    som_resolver: SomResolver,
    /// Cached mapping of field names to their flattened node indices
    field_index_cache: HashMap<String, usize>,
    /// Registry of all scripts in the form, categorized by type
    script_registry: Arc<ScriptRegistry>,
    /// Dependency tracker for cascading calculations
    dependency_tracker: DependencyTracker,
    /// Dirty flag - set when changes require refresh
    dirty: bool,
    /// Persistent script engine — single source of truth for field values
    script_engine: XfaScriptEngine,
    /// The pristine template declaration of every repeatable subform, which
    /// a new instance is cloned from (XFA 3.3 §9).
    prototypes: Prototypes,
    /// Instance changes applied since the current interaction began.
    /// Interior state by design: it is filled from deep inside event
    /// dispatch (every script run may queue instance changes) and read once
    /// by the public entry point that started the interaction.
    instance_report: InstanceReport,
    /// True while queued instance changes are being applied, so the events
    /// that application fires (initialize, indexChange) do not re-enter it;
    /// what they queue is picked up by the next round of the same loop.
    applying_instances: bool,
}

impl XfaForm {
    /// Create a new XFA form from parsed nodes.
    pub fn new(nodes: Vec<XfaNode>) -> Result<Self, String> {
        Self::new_with_layout(nodes).map(|(form, _layout)| form)
    }

    /// As [`new`](Self::new), but also handing back the master-page engine
    /// from the very same document-wide script pass that seeded this form,
    /// so a caller that goes on to call
    /// [`refresh_paged`](Self::refresh_paged) gets one that already agrees
    /// with this form's own values -- rather than running the pass a second
    /// time in a fresh engine, which is what `prepare_default` does and is
    /// exactly the discrepancy an interactive session must not have. `None`
    /// only when this document's scripts could not be executed at all, in
    /// which case there is nothing to page through either.
    pub fn new_with_layout(
        mut nodes: Vec<XfaNode>,
    ) -> Result<(Self, Option<crate::xfa::script_executor::LayoutScripts>), String> {
        // The Form DOM of an empty merge: each repeatable subform as its
        // `initial` instances (XFA 3.3 §9), before any script sees the form.
        let mut prototypes = materialize_initial_instances(&mut nodes);

        let script_registry = Arc::new(Self::build_script_registry(&nodes));
        let dependency_tracker = DependencyTracker::new();

        // Execute scripts using ScriptExecutor, keeping the layout engine
        // alive so page-dependent master-page scripts can be re-evaluated
        // later without a second, independent script pass.
        let (script_result, layout) =
            crate::xfa::script_executor::ScriptExecutor::execute_with_layout(
                &mut nodes,
                &mut prototypes,
            );

        // Apply presence changes to the nodes
        crate::xfa::script_executor::ScriptExecutor::apply_presence_changes(
            &mut nodes,
            &script_result.presence_changes,
        );
        // And the `access` the load-time scripts set (an initialize script's
        // `this.access = "protected"`), so the form opens locked where the
        // form itself locks it.
        crate::xfa::script_executor::ScriptExecutor::apply_access_changes(
            &mut nodes,
            &script_result.access_changes,
        );

        // Merge items from Form DOM packet into Template DOM fields.
        // The Form DOM preserves runtime state (e.g. script-populated dropdown items).
        Flattened::merge_form_items_into_template(&mut nodes);

        // Merge presence values from Form DOM into Template DOM.
        // The Form DOM preserves visibility state set by scripts (e.g. hiding
        // a section based on dropdown selection).
        // Pass the script presence changes so we skip paths already handled
        // by script execution (which produces authoritative runtime state).
        Flattened::merge_form_presence_into_template(&mut nodes, &script_result.presence_changes);

        // Merge access values from Form DOM into Template DOM. Fields whose
        // template leaves `access` unset but which become `nonInteractive` at
        // runtime (via init script or the form packet) are captured here so they
        // render as static captions/labels rather than editable inputs.
        Flattened::merge_form_access_into_template(&mut nodes);

        let init_values = script_result.computed_values;

        // Flatten with the init-time computed values
        let flattened = Flattened::from_xfa(&nodes, &init_values)?;

        let som_resolver = SomResolver::from_nodes(&nodes);
        let field_index_cache = Self::build_field_index_cache(&flattened);

        // Create persistent script engine — disable auto exclGroup sync since
        // interactive events use select_radio_button for explicit propagation
        let mut script_engine = XfaScriptEngine::new();

        // Register fields/subforms FIRST so that script objects can reference
        // them at initialization time (e.g., `var b = UBSForms.Page;`).
        Self::build_som_hierarchy_with_values(
            &nodes,
            &init_values,
            &prototypes,
            &mut script_engine,
        );
        // Initialize engine with form context - variables like Footer_Line_txtlanguage
        // and Footer_Line_txtformid are extracted from XFA <variables><text> elements
        Self::extract_and_register_translations(&nodes, &mut script_engine);

        Ok((
            XfaForm {
                nodes,
                flattened,
                som_resolver,
                field_index_cache,
                script_registry,
                dependency_tracker,
                dirty: false,
                script_engine,
                prototypes,
                instance_report: InstanceReport::default(),
                applying_instances: false,
            },
            layout,
        ))
    }

    /// Resolve a node by SOM expression (immutable)
    pub fn resolve(&self, som_expression: &str) -> Option<XfaNodeRef<'_>> {
        let resolved_path = self.som_resolver.resolve_node(som_expression, None)?;

        let xfa_node = Self::find_xfa_node_by_path(&self.nodes, &resolved_path)?;

        let node_name = xfa_node.name.as_ref()?;
        let flattened_node = self
            .field_index_cache
            .get(node_name)
            .and_then(|&idx| self.flattened.iter_nodes().nth(idx));

        // `check_ancestors_visible` matches by leaf name alone, so on a form
        // with the same field/subform name repeated under different parents
        // (a common template pattern — "RB_No"/"RB_Yes" under many unrelated
        // exclGroups, say) it silently reports whichever occurrence comes
        // first in document order for *every* occurrence. `is_path_visible`
        // walks the full resolved SOM path instead, so it answers for the
        // actual node this call resolved to.
        let ancestors_visible = self.is_path_visible(resolved_path.as_str());

        Some(XfaNodeRef {
            xfa_node,
            flattened_node,
            som_path: resolved_path,
            ancestors_visible,
        })
    }

    /// Resolve a node by SOM expression (mutable)
    pub fn resolve_mut(&mut self, som_expression: &str) -> Option<XfaNodeRefMut<'_>> {
        let resolved_path = self.som_resolver.resolve_node(som_expression, None)?;

        let xfa_node = Self::find_xfa_node_by_path_mut(&mut self.nodes, &resolved_path)?;

        Some(XfaNodeRefMut {
            xfa_node,
            som_path: resolved_path,
            script_engine: &mut self.script_engine,
        })
    }

    /// Evaluate a JavaScript expression directly in the engine (for debugging/testing).
    #[cfg(test)]
    pub fn eval_js(&mut self, source: &str) -> Result<Option<String>, String> {
        use super::events::{EventRef, ListenScope, RunAt, ScriptContentType, XfaScript};
        self.script_engine.execute_script(&XfaScript {
            source: source.to_string(),
            content_type: ScriptContentType::JavaScript,
            activity: EventActivity::Calculate,
            event_ref: EventRef::Current,
            name: None,
            run_at: RunAt::Client,
            listen: ListenScope::default(),
        })
    }

    /// Execute an event activity on a node.
    ///
    /// For change events, `prev_value` should carry the field's value before
    /// the change so that `xfa.event.prevText` / `newText` are correct.
    pub fn execute_event(
        &mut self,
        som_expression: &str,
        activity: EventActivity,
        prev_value: Option<&str>,
    ) -> Result<EventResult, String> {
        let resolved_path = self
            .som_resolver
            .resolve_node(som_expression, None)
            .ok_or_else(|| format!("Could not resolve SOM expression: {}", som_expression))?;

        let node_name = Self::find_xfa_node_by_path(&self.nodes, &resolved_path)
            .and_then(|n| n.name.clone())
            .ok_or_else(|| format!("Node has no name: {}", resolved_path))?;

        let scripts = self.find_node_scripts(&resolved_path, &activity);

        if scripts.is_empty() {
            return Ok(EventResult::default());
        }

        // Snapshot field values before script execution for change detection,
        // by full path: a bare name would conflate the instances of a
        // repeated section.
        let pre_values = self.script_engine.get_all_som_field_values_by_path();

        let current_value = self
            .script_engine
            .get_field_value(&resolved_path)
            .unwrap_or_default();

        self.script_engine
            .set_current_field(&resolved_path, &node_name, &current_value);

        // Set up $event context before script execution (XFA 3.3 §10 pp.398-404)
        self.script_engine
            .update_event_context(&activity, &resolved_path, prev_value);

        let mut changed_fields = Vec::new();
        for script in &scripts {
            let result = self.script_engine.execute_script(script);
            if let Ok(Some(value)) = result {
                // Empty strings are valid per XFA spec (e.g. rawValue = "" clears a field)
                changed_fields.push(resolved_path.clone());
                // Update engine with the script return value
                self.script_engine
                    .update_field_value(&resolved_path, &value);
            }
        }

        let mut presence_changed =
            if let Some(presence) = self.script_engine.get_current_field_presence() {
                Self::apply_presence_by_path(&mut self.nodes, &resolved_path, presence);
                true
            } else {
                false
            };

        let som_presence_changes = self.script_engine.get_all_som_presence_changes();
        for (som_path, presence_str) in &som_presence_changes {
            let presence = presence_str.parse().unwrap_or_default();
            Self::apply_presence_by_path(&mut self.nodes, som_path, presence);
            presence_changed = true;
        }
        // Update initial_presence baseline so subsequent events detect reverts correctly
        for (som_path, presence_str) in &som_presence_changes {
            self.script_engine
                .update_initial_presence(&SomPath::new(som_path), presence_str);
        }

        let access_changed = self.sync_access_from_engine();

        // Detect side-effect value changes by comparing with pre-execution snapshot.
        // Per XFA 3.3 §10: when a script sets rawValue on another field, that
        // assignment updates the Form DOM.  We must propagate these side-effect
        // changes back into the XFA node tree so that subsequent reflattening
        // picks them up (mirroring the write-back already done for presence).
        let post_values = self.script_engine.get_all_som_field_values_by_path();
        for (field_name, new_value) in &post_values {
            let field_som_path = SomPath::new(field_name);
            if pre_values.get(field_name) != Some(new_value) {
                changed_fields.push(field_som_path.clone());
                // Write the updated value back to the XFA node tree
                if let Some(node) =
                    Self::find_xfa_node_by_path_mut(&mut self.nodes, &field_som_path)
                {
                    XfaNodeRefMut::set_node_value(node, new_value);
                }
            }
        }

        // Event propagation: walk up to ancestor containers, firing any
        // handlers with listen="refAndDescendents" that match this activity.
        // Per XFA 3.3 §10 p.387: "events can now propagate upward to
        // enclosing containers."
        let mut ancestor = resolved_path.parent();
        while let Some(ancestor_path) = ancestor {
            let propagating_scripts = self.find_propagating_scripts(&ancestor_path, &activity);
            if !propagating_scripts.is_empty() {
                let ancestor_name = ancestor_path.name().to_string();
                let ancestor_value = self
                    .script_engine
                    .get_field_value(&ancestor_path)
                    .unwrap_or_default();
                self.script_engine.set_current_field(
                    &ancestor_path,
                    &ancestor_name,
                    &ancestor_value,
                );
                // Keep $event.target pointing at the ORIGINAL target
                self.script_engine
                    .update_event_context(&activity, &resolved_path, prev_value);

                for script in &propagating_scripts {
                    let result = self.script_engine.execute_script(script);
                    if let Ok(Some(value)) = result {
                        changed_fields.push(ancestor_path.clone());
                        self.script_engine
                            .update_field_value(&ancestor_path, &value);
                    }
                }
            }
            ancestor = ancestor_path.parent();
        }

        // Instance-manager calls the scripts made reshape the Form DOM now,
        // so whatever runs next (and the next refresh) sees the new shape.
        self.apply_pending_instance_ops()?;

        let values_changed = !changed_fields.is_empty();

        if values_changed || presence_changed || access_changed || self.instance_report.changed {
            self.dirty = true;
        }

        Ok(EventResult {
            values_changed,
            presence_changed,
            access_changed,
            changed_fields,
            instances_changed: self.instance_report.changed,
            instance_limit_hit: self.instance_report.limit_hit.clone(),
        })
    }

    /// A person clicking the node (XFA 3.3 §10 `click`): its click scripts
    /// run, and any instance-manager change they make is applied to the
    /// Form DOM before this returns, so the next refresh lays out the new
    /// instances.
    pub fn click(&mut self, som_expression: &str) -> Result<EventResult, String> {
        // A button that is not open cannot be pressed (XFA 3.3 §17): a
        // protected or nonInteractive one generates no events at all, and a
        // readOnly one none tied to a direct action such as a press.
        if let Some(refusal) = self.access_refusal(som_expression) {
            return Err(refusal);
        }
        self.instance_report = InstanceReport::default();
        self.execute_event(som_expression, EventActivity::Click, None)
    }

    /// Convenience method to execute a change event
    pub fn change(&mut self, som_expression: &str) -> Result<EventResult, String> {
        self.execute_event(som_expression, EventActivity::Change, None)
    }

    /// Convenience method to execute an initialize event
    pub fn initialize(&mut self, som_expression: &str) -> Result<EventResult, String> {
        self.execute_event(som_expression, EventActivity::Initialize, None)
    }

    /// Convenience method to execute an enter event
    pub fn enter(&mut self, som_expression: &str) -> Result<EventResult, String> {
        self.execute_event(som_expression, EventActivity::Enter, None)
    }

    /// Convenience method to execute an exit event
    pub fn exit(&mut self, som_expression: &str) -> Result<EventResult, String> {
        self.execute_event(som_expression, EventActivity::Exit, None)
    }

    /// The part of a refresh that is the same regardless of how the result
    /// gets flattened: run cross-branch calculations, materialize dynamic
    /// subform instances, and sync script-driven presence back onto the node
    /// tree. Returns the values the flattener should use.
    fn refresh_prelude(&mut self) -> Result<HashMap<SomPath, String>, String> {
        // Run all Calculate scripts in the form to ensure cross-branch
        // dependencies are resolved. Per XFA 3.3, Calculate scripts should
        // re-run whenever dependent values change. This catches cases where
        // a Calculate script on one branch reads a field value from another
        // branch (e.g., Agreement.SectionTitle reading RB_Group_NeW.rawValue).
        self.run_all_calculate_scripts();
        self.apply_pending_instance_ops()?;

        // Sync presence changes from JS engine back to XFA nodes so that
        // reflattening picks up visibility toggled by scripts.
        let presence_changes = self.script_engine.get_all_som_presence_changes();
        for (som_path, presence_str) in &presence_changes {
            let presence: crate::xfa::Presence = presence_str.parse().unwrap();
            Self::apply_presence_by_path(&mut self.nodes, som_path, presence);
        }
        // Calculate scripts can set `access` too.
        self.sync_access_from_engine();

        Ok(self.script_engine.get_all_field_values_for_flattening())
    }

    /// Re-flatten the form to reflect any changes.
    pub fn refresh(&mut self) -> Result<(), String> {
        let values = self.refresh_prelude()?;
        self.flattened = Flattened::reflatten(&self.nodes, &values)?;
        self.som_resolver = SomResolver::from_nodes(&self.nodes);
        self.field_index_cache = Self::build_field_index_cache(&self.flattened);
        self.dirty = false;
        Ok(())
    }

    /// As [`refresh`](Self::refresh), but re-evaluating page-dependent
    /// master-page scripts against `layout` instead of reusing whatever they
    /// produced at the document-wide pass.
    ///
    /// `refresh` alone has no master-page engine to call into, which is why a
    /// form rendered through an interacted state has always shown page one's
    /// header and footer values on every page: `reflatten` never asks a
    /// master page's own scripts what page they are on. `layout` is the
    /// engine `ScriptExecutor::execute_with_layout` returned when this form
    /// was built, kept alive alongside it for exactly this call.
    pub fn refresh_paged(
        &mut self,
        layout: &mut crate::xfa::script_executor::LayoutScripts,
    ) -> Result<(), String> {
        let values = self.refresh_prelude()?;
        layout.sync_values(&values);
        self.flattened = Flattened::from_xfa_paged(
            &self.nodes,
            &values,
            &mut |page_area, page_index, page_count| {
                layout.evaluate(page_area, page_index, page_count)
            },
        )?;
        self.som_resolver = SomResolver::from_nodes(&self.nodes);
        self.field_index_cache = Self::build_field_index_cache(&self.flattened);
        self.dirty = false;
        Ok(())
    }

    /// Apply every instance-manager change the scripts have queued to the
    /// Form DOM and the script engine (XFA 3.3 §9), then fire the events a
    /// change implies: `initialize` on each new instance (post-order, as
    /// at load) and then `indexChange` on every instance whose index is new
    /// or changed (§10 "Instance Manager Events").
    ///
    /// Those events may queue further changes; they are applied in further
    /// rounds of the same loop, up to [`MAX_INSTANCE_ROUNDS`], after which
    /// the form is reported as looping rather than left half-applied.
    fn apply_pending_instance_ops(&mut self) -> Result<(), String> {
        if self.applying_instances {
            return Ok(());
        }
        self.applying_instances = true;
        let result = self.apply_instance_rounds();
        self.applying_instances = false;
        result
    }

    fn apply_instance_rounds(&mut self) -> Result<(), String> {
        for _ in 0..MAX_INSTANCE_ROUNDS {
            let ops = self.script_engine.drain_instance_ops();
            if ops.is_empty() {
                return Ok(());
            }

            // 1. The Form DOM tree, op by op, in the order the scripts made them.
            let mut arrays: Vec<(SomPath, String)> = Vec::new();
            for op in &ops {
                if let Some(message) = op.limit_message() {
                    self.instance_report.limit_hit.get_or_insert(message);
                    continue;
                }
                crate::xfa::instances::apply_to_tree(&mut self.nodes, &mut self.prototypes, op)?;
                self.instance_report.changed = true;
                let (parent, name) = op.array();
                let array = (parent.clone(), name.to_string());
                if !arrays.contains(&array) {
                    arrays.push(array);
                }
            }

            // 2. The engine. Deepest arrays first: their parents' paths are
            // still the ones registered, which an outer re-key would move.
            arrays.sort_by_key(|(parent, _)| std::cmp::Reverse(parent.segments().count()));
            let mut new_instances: Vec<SomPath> = Vec::new();
            let mut reindexed: Vec<SomPath> = Vec::new();
            for (parent, name) in &arrays {
                let reconciled = self.script_engine.reconcile_instances(parent, name);
                reindexed.extend(reconciled.moved.into_iter().map(|(_, new)| new));
                for (path, written) in reconciled.placeholders {
                    let node =
                        super::som::walk_som_path(&self.nodes, path.as_str()).ok_or_else(|| {
                            format!("new instance {path} is missing from the Form DOM")
                        })?;
                    let values: HashMap<SomPath, String> = written
                        .into_iter()
                        .map(|(rel, v)| (SomPath::new(format!("{path}.{rel}")), v))
                        .collect();
                    register_node_at(
                        node,
                        path.as_str(),
                        parent.as_str(),
                        &values,
                        &self.prototypes,
                        &mut self.script_engine,
                        false,
                    );
                    new_instances.push(path);
                }
            }
            self.som_resolver = SomResolver::from_nodes(&self.nodes);
            self.script_engine
                .replace_som_resolver(SomResolver::from_nodes(&self.nodes));

            // 3. Events: initialize on the new instances, then indexChange.
            for instance in &new_instances {
                for path in Self::initialize_order(&self.nodes, instance) {
                    self.execute_event(path.as_str(), EventActivity::Initialize, None)?;
                }
            }
            for instance in new_instances.iter().chain(&reindexed) {
                self.execute_event(instance.as_str(), EventActivity::IndexChange, None)?;
            }
        }
        Err(format!(
            "instance changes kept causing further instance changes after \
             {MAX_INSTANCE_ROUNDS} rounds; the form's scripts appear to loop"
        ))
    }

    /// Every node in the subtree at `root` that has an `initialize` script,
    /// children before their container (post-order), as at load time (see
    /// `ScriptExecutor`'s Phase 1).
    fn initialize_order(nodes: &[XfaNode], root: &SomPath) -> Vec<SomPath> {
        fn walk(node: &XfaNode, path: &str, out: &mut Vec<SomPath>) {
            for (child, index) in node.children.iter().zip(sibling_indices(&node.children)) {
                let child_path = match &child.name {
                    Some(name) => child_som_path(path, name, index),
                    None => path.to_string(),
                };
                walk(child, &child_path, out);
            }
            let has_initialize = parse_events_from_node(&node.children)
                .iter()
                .any(|s| s.activity == EventActivity::Initialize);
            if node.name.is_some() && has_initialize {
                out.push(SomPath::new(path));
            }
        }
        let mut out = Vec::new();
        if let Some(node) = super::som::walk_som_path(nodes, root.as_str()) {
            walk(node, root.as_str(), &mut out);
        }
        out
    }

    /// Return all SOM presence changes detected by the script engine.
    pub fn get_presence_changes(&mut self) -> HashMap<String, String> {
        self.script_engine.get_all_som_presence_changes()
    }

    /// Execute change event scripts on the parent exclGroup when a radio button is selected.
    pub fn trigger_change_on_excl_group(
        &mut self,
        field_path: &str,
        prev_value: Option<&str>,
    ) -> Result<EventResult, String> {
        let excl_group_path = self.find_parent_excl_group_by_path(field_path);

        if let Some(ref excl_path) = excl_group_path {
            let result = self.execute_event(excl_path, EventActivity::Change, prev_value)?;
            self.cascade_calculations(excl_path)?;
            self.run_ancestor_calculate_scripts(excl_path)?;
            Ok(result)
        } else {
            let resolved_path = self
                .som_resolver
                .resolve_node(field_path, None)
                .unwrap_or_else(|| SomPath::from(field_path));
            let result = self.execute_event(field_path, EventActivity::Change, prev_value)?;
            self.cascade_calculations(&resolved_path)?;
            self.run_ancestor_calculate_scripts(&resolved_path)?;
            Ok(result)
        }
    }

    /// Set a field value as if the user interacted with it.
    ///
    /// Unlike the low-level `set_raw_value` (which is a pure setter), this method
    /// also fires the `change` event and cascades dependent calculations — matching
    /// the XFA 3.3 spec requirement that change events fire when a user "makes a
    /// selection from a choice list or drop-down menu, checks or unchecks a checkbox".
    ///
    /// Use this for any user-simulated interaction (exhaustive exploration, replay).
    /// For programmatic / calculated value changes, use `set_raw_value` directly.
    pub fn set_value_as_user(
        &mut self,
        field_path: &str,
        value: &str,
    ) -> Result<EventResult, String> {
        let resolved_path = self
            .som_resolver
            .resolve_node(field_path, None)
            .ok_or_else(|| format!("Could not resolve field: {}", field_path))?;

        // Capture the previous value BEFORE updating so that
        // xfa.event.prevText is correct in change event scripts.
        let prev_value = self
            .script_engine
            .get_field_value(&resolved_path)
            .unwrap_or_default();

        // Update engine (source of truth) and XFA node
        self.script_engine.update_field_value(&resolved_path, value);
        if let Some(node) = Self::find_xfa_node_by_path_mut(&mut self.nodes, &resolved_path) {
            XfaNodeRefMut::set_node_value(node, value);
        }

        self.dirty = true;

        // Fire change event and cascade calculations
        self.trigger_change_on_excl_group(&resolved_path, Some(&prev_value))
    }

    /// Select a radio button in an exclusion group.
    /// The value this radio button contributes to its exclGroup when selected.
    ///
    /// Per XFA 3.3 §4 the on-value comes from the button's `<items>` element;
    /// falling back to deriving one from the field name only covers a button
    /// with no `<items>` at all. Shared by `select_radio_button`, which writes
    /// this value, and `interact`, which validates a caller's guess against it
    /// before writing anything.
    fn radio_on_value(&self, resolved_path: &SomPath) -> String {
        Self::find_xfa_node_by_path(&self.nodes, resolved_path)
            .and_then(|node| node.extract_item_values().0)
            .unwrap_or_else(|| {
                let button_name = resolved_path.name();
                button_name
                    .strip_prefix("RB_")
                    .or_else(|| button_name.rsplit('_').next())
                    .unwrap_or(button_name)
                    .to_string()
            })
    }

    pub fn select_radio_button(&mut self, radio_button_path: &str) -> Result<EventResult, String> {
        let resolved_path = self
            .som_resolver
            .resolve_node(radio_button_path, None)
            .ok_or_else(|| format!("Could not resolve radio button: {}", radio_button_path))?;

        let button_value = self.radio_on_value(&resolved_path);

        // Capture the previous value before updating for change event context.
        let _prev_value = self
            .script_engine
            .get_field_value(&resolved_path)
            .unwrap_or_default();

        // Update engine (source of truth) and XFA node
        self.script_engine.update_field_value(&resolved_path, "1");

        if let Some(node) = Self::find_xfa_node_by_path_mut(&mut self.nodes, &resolved_path) {
            node.attributes
                .insert("rawValue".to_string(), "1".to_string());
        }

        let excl_group_path = self.find_parent_excl_group_by_path(&resolved_path);

        if let Some(ref excl_path) = excl_group_path {
            let excl_prev = self
                .script_engine
                .get_field_value(excl_path)
                .unwrap_or_default();

            self.script_engine
                .update_field_value(excl_path, &button_value);

            if let Some(excl_node) = Self::find_xfa_node_by_path_mut(&mut self.nodes, excl_path) {
                excl_node
                    .attributes
                    .insert("rawValue".to_string(), button_value.to_string());
            }

            self.dirty = true;

            self.trigger_change_on_excl_group(&resolved_path, Some(&excl_prev))
        } else {
            self.dirty = true;
            Ok(EventResult::default())
        }
    }

    /// One field interaction, in the order a person's actually is: focus in,
    /// change, focus out. Dispatches on kind, because an exclGroup member is
    /// *selected* -- which deselects its siblings and sets the group's value
    /// -- rather than assigned like an ordinary field.
    ///
    /// `set_value_as_user` alone fires only the change event, which is right
    /// for replaying a recorded value and wrong for simulating a person:
    /// forms commonly hang recalculation off `exit`, so a real interaction
    /// must fire it too.
    ///
    /// For a radio, `value` must equal the button's own on-value -- there is
    /// no off-value, since a person cannot deselect a radio -- and a mismatch
    /// is refused rather than silently ignored, since `select_radio_button`
    /// itself ignores its caller's value entirely.
    pub fn interact(&mut self, field_path: &str, value: &str) -> Result<EventResult, String> {
        let resolved_path = self
            .som_resolver
            .resolve_node(field_path, None)
            .ok_or_else(|| format!("Could not resolve field: {}", field_path))?;

        // A person cannot change a field that is not open, so neither does
        // an interaction simulating one; no event fires for the attempt.
        if let Some(refusal) = self.access_refusal(field_path) {
            return Err(refusal);
        }

        let is_radio = self
            .resolve(field_path)
            .map(|n| n.is_radio_button())
            .unwrap_or(false);

        if is_radio {
            let on_value = self.radio_on_value(&resolved_path);
            if value != on_value {
                return Err(format!(
                    "'{value}' is not a value {field_path} can be set to; a radio button is \
                     selected with its own on-value, which is '{on_value}'"
                ));
            }
        }

        self.instance_report = InstanceReport::default();
        let mut result = self.enter(field_path)?;

        let write_result = if is_radio {
            self.select_radio_button(field_path)?
        } else {
            self.set_value_as_user(field_path, value)?
        };
        result.absorb(write_result);

        let exit_result = self.exit(field_path)?;
        result.absorb(exit_result);

        // A cascade of calculate scripts can queue instance changes outside
        // any one event; apply those too, then report the whole interaction.
        self.apply_pending_instance_ops()?;
        result.instances_changed = self.instance_report.changed;
        result.instance_limit_hit = self.instance_report.limit_hit.clone();
        Ok(result)
    }

    /// Run calculate scripts for all fields that depend on the changed field.
    pub fn cascade_calculations(&mut self, changed_field: &SomPath) -> Result<(), String> {
        let dependents = self
            .dependency_tracker
            .get_dependents_cascade(changed_field);

        if dependents.is_empty() {
            return Ok(());
        }

        for dependent_path in dependents {
            let scripts = self
                .script_registry
                .get_event_scripts(&dependent_path, &EventActivity::Calculate);

            for registered_script in scripts {
                // The concrete instance, not the registry's template path:
                // `this` must be the dependent that is being recalculated.
                self.script_engine.set_current_field(
                    &dependent_path,
                    &registered_script.owner_name,
                    "",
                );

                if let Ok(Some(value)) =
                    self.script_engine.execute_script(&registered_script.script)
                    && !value.is_empty()
                {
                    // Update engine directly (source of truth)
                    self.script_engine
                        .update_field_value(&dependent_path, &value);
                }
            }
        }

        self.dirty = true;
        Ok(())
    }

    /// Run calculate scripts on ancestor subforms of a changed field.
    ///
    /// Per XFA 3.3: calculate scripts on container nodes (subforms) fire when
    /// any descendant field value changes. Walk upward from the changed field
    /// and execute calculate scripts registered on each ancestor.
    fn run_ancestor_calculate_scripts(&mut self, field_path: &SomPath) -> Result<(), String> {
        let path_str = field_path.as_str().to_string();
        let mut pos = path_str.len();
        while let Some(dot) = path_str[..pos].rfind('.') {
            let ancestor = &path_str[..dot];
            let ancestor_path = SomPath::new(ancestor);

            let has_calc = !self
                .script_registry
                .get_event_scripts(&ancestor_path, &EventActivity::Calculate)
                .is_empty();

            if has_calc {
                // Use execute_event to correctly handle presence changes,
                // value side-effects, and script object calls.
                let _ = self.execute_event(ancestor, EventActivity::Calculate, None);
            }

            pos = dot;
        }

        Ok(())
    }

    /// Run all Calculate scripts registered in the form.
    ///
    /// Per XFA 3.3, Calculate scripts should re-run whenever any dependent value
    /// changes. This method runs every Calculate script in the form, which handles
    /// cross-branch dependencies (e.g., a Calculate script on `Agreement.SectionTitle`
    /// that reads `RB_Group_NeW.rawValue` from a different branch of the XFA tree).
    ///
    /// Called by `refresh()` to ensure all Calculate-driven visibility changes
    /// are resolved before re-flattening.
    fn run_all_calculate_scripts(&mut self) {
        // The registry holds template paths; each runs once per instance.
        let owners: Vec<SomPath> = self
            .script_registry
            .get_owners_with_activity(&EventActivity::Calculate)
            .into_iter()
            .flat_map(|template| self.som_resolver.expand_template(template))
            .collect();

        for owner_path in owners {
            let _ = self.execute_event(owner_path.as_str(), EventActivity::Calculate, None);
        }
    }

    /// Find the parent exclGroup for a given field (public API)
    pub fn find_excl_group_for_field(&self, field_name_or_path: &str) -> Option<SomPath> {
        if field_name_or_path.contains('.') {
            self.find_parent_excl_group_by_path(field_name_or_path)
        } else {
            self.find_parent_excl_group(field_name_or_path)
        }
    }

    /// Find the parent exclGroup for a given SOM path
    fn find_parent_excl_group_by_path(&self, som_path: &str) -> Option<SomPath> {
        let parts = concrete_segments(som_path)?;

        fn walk_path_for_excl_group(
            nodes: &[XfaNode],
            parts: &[(&str, usize)],
            idx: usize,
            current_excl_group_path: Option<String>,
            current_path: &str,
        ) -> Option<String> {
            if idx >= parts.len() {
                return current_excl_group_path;
            }

            let target = parts[idx];

            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                let node_name = node.name.as_deref();
                let is_excl_group = node.kind.is_exclgroup();

                let node_path = node_name.map(|name| child_som_path(current_path, name, index));

                let excl_group_for_children = if is_excl_group && node_path.is_some() {
                    node_path.clone()
                } else {
                    current_excl_group_path.clone()
                };

                if node_name.map(|n| (n, index)) == Some(target) {
                    if idx == parts.len() - 1 {
                        return current_excl_group_path;
                    }
                    let next_path = node_path.unwrap_or_else(|| current_path.to_string());
                    return walk_path_for_excl_group(
                        &node.children,
                        parts,
                        idx + 1,
                        excl_group_for_children,
                        &next_path,
                    );
                } else if node_name.is_none()
                    && let Some(found) = walk_path_for_excl_group(
                        &node.children,
                        parts,
                        idx,
                        excl_group_for_children,
                        current_path,
                    )
                {
                    return Some(found);
                }
            }

            None
        }

        walk_path_for_excl_group(&self.nodes, &parts, 0, None, "").map(SomPath::new)
    }

    fn find_parent_excl_group(&self, field_path: &str) -> Option<SomPath> {
        fn find_excl_group_parent(
            nodes: &[XfaNode],
            target_name: &str,
            current_excl_group: Option<&str>,
        ) -> Option<String> {
            for node in nodes {
                let is_excl_group = node.kind.is_exclgroup();

                let excl_group_for_children = if is_excl_group {
                    node.name.as_deref()
                } else {
                    current_excl_group
                };

                if node.name.as_deref() == Some(target_name) {
                    return current_excl_group.map(|s| s.to_string());
                }

                if let Some(found) =
                    find_excl_group_parent(&node.children, target_name, excl_group_for_children)
                {
                    return Some(found);
                }
            }
            None
        }

        let target_name = field_path.rsplit('.').next().unwrap_or(field_path);
        find_excl_group_parent(&self.nodes, target_name, None).map(SomPath::new)
    }

    /// What a person may do with the node at `som_path`, per XFA 3.3: its
    /// own `access` combined with every enclosing subform's and exclusion
    /// group's, the most restrictive winning, because an object inherits its
    /// containers' access and may only tighten it (§2 "Access
    /// Restrictions"; §17 `access`, precedence nonInteractive, protected,
    /// readOnly, open). `None` when the path names no node.
    pub fn effective_access(&self, som_path: &str) -> Option<EffectiveAccess> {
        let resolved = self.som_resolver.resolve_node(som_path, None)?;
        let route = super::som::walk_som_route(&self.nodes, resolved.as_str())?;
        let (target, containers) = route.split_last()?;
        let segments: Vec<&str> = resolved.as_str().split('.').collect();

        // Each container with the path of the nearest named node at or above
        // it: a named one's own path, an unnamed one's enclosing path.
        let mut named = 0usize;
        let mut strictest: Option<(FieldAccess, SomPath)> = None;
        for node in containers {
            if node.name.is_some() {
                named += 1;
            }
            if !node.holds_access() || named == 0 {
                continue;
            }
            let access = node.get_access();
            // At least as restrictive replaces, walking outside in, so the
            // innermost container wins a tie and the report names the
            // nearest one that imposes the level.
            if strictest
                .as_ref()
                .is_none_or(|(held, _)| access.most_restrictive(*held) == access)
            {
                strictest = Some((access, SomPath::new(segments[..named].join("."))));
            }
        }

        let own = target.get_access();
        Some(match strictest {
            Some((inherited, from)) if own.most_restrictive(inherited) != own => EffectiveAccess {
                access: inherited,
                inherited_from: Some(from),
            },
            _ => EffectiveAccess {
                access: own,
                inherited_from: None,
            },
        })
    }

    /// Why a person could not change or press the field at `field_path`, or
    /// `None` when they could. Anything but `open` refuses: `readOnly`,
    /// `protected` and `nonInteractive` all forbid direct changes by the
    /// person filling the form (XFA 3.3 §17), while still letting the form's
    /// own scripts change the field.
    pub fn access_refusal(&self, field_path: &str) -> Option<String> {
        let effective = self.effective_access(field_path)?;
        if effective.access.is_interactive() {
            return None;
        }
        let source = match &effective.inherited_from {
            Some(container) => format!("inherited from {container}"),
            None => "its own access".to_string(),
        };
        Some(format!(
            "{field_path} is {} ({source}), so it cannot be changed or pressed directly; only \
             the form's own scripts can change it, often in response to another field",
            effective.access.as_str()
        ))
    }

    /// Check if a node at the given SOM path is visible.
    pub fn is_path_visible(&self, som_path: &str) -> bool {
        let Some(parts) = concrete_segments(som_path) else {
            return true;
        };

        fn walk_path(nodes: &[XfaNode], parts: &[(&str, usize)], idx: usize) -> bool {
            if idx >= parts.len() {
                return true;
            }

            let target = parts[idx];

            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                // Skip PageSet nodes and the XDP "form" data tree — they
                // contain duplicates of template subform names with different
                // (or default) presence values that would produce false
                // positives.
                if matches!(&node.kind, XfaNodeKind::PageSet) {
                    continue;
                }
                if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
                    if tag_name == "form" {
                        continue;
                    }
                }

                let node_presence = node.get_presence();
                let is_hidden = node_presence.should_skip_layout();

                if node.name.as_deref().map(|n| (n, index)) == Some(target) {
                    if is_hidden {
                        return false;
                    }
                    return walk_path(&node.children, parts, idx + 1);
                }

                if !is_hidden && walk_path(&node.children, parts, idx) {
                    return true;
                }
            }

            false
        }

        walk_path(&self.nodes, &parts, 0)
    }

    /// Register a dependency between fields.
    pub fn add_dependency(&mut self, dependent_field: &str, source_field: &str) {
        self.dependency_tracker
            .add_dependency(&SomPath::new(dependent_field), &SomPath::new(source_field));
    }

    /// Get the script registry (read-only)
    pub fn script_registry(&self) -> &ScriptRegistry {
        &self.script_registry
    }

    /// Get a shared reference to the script registry `Arc`.
    pub fn script_registry_arc(&self) -> Arc<ScriptRegistry> {
        Arc::clone(&self.script_registry)
    }

    /// Check if the form has uncommitted changes that require refresh
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Get a computed value by field name or path
    pub fn get_computed_value(&mut self, name: &str) -> Option<String> {
        self.script_engine.get_field_value(&SomPath::new(name))
    }

    /// Get the page dimensions
    pub fn page_size(&self) -> (Num, Num) {
        (self.flattened.page.width, self.flattened.page.height)
    }

    /// Get an iterator over all flattened nodes (read-only)
    pub fn flattened_nodes(&self) -> impl Iterator<Item = &FlattenedNode> {
        self.flattened.iter_nodes()
    }

    /// Get the underlying Flattened struct
    pub fn flattened(&self) -> &Flattened {
        &self.flattened
    }

    /// Get the underlying Flattened struct (mutable)
    pub fn flattened_mut(&mut self) -> &mut Flattened {
        &mut self.flattened
    }

    /// Get all field names in the form
    pub fn field_names(&self) -> Vec<String> {
        self.field_index_cache.keys().cloned().collect()
    }

    /// Get access to the underlying XFA nodes (read-only)
    pub fn xfa_nodes(&self) -> &[XfaNode] {
        &self.nodes
    }

    /// Get the current field values from the persistent script engine.
    ///
    /// Returns the same map used by [`refresh`] for flattening: each field's
    /// value under its full SOM path (and, for lookups by name, its bare
    /// name).
    /// Every field's current value, once each, under its canonical SOM path
    /// (instance indices included) -- what an interaction's before/after
    /// comparison should read.
    pub fn canonical_field_values(&mut self) -> HashMap<SomPath, String> {
        self.script_engine.get_field_values_by_canonical_path()
    }

    pub fn current_field_values(&mut self) -> HashMap<SomPath, String> {
        self.script_engine.get_all_field_values_for_flattening()
    }

    // ========================================================================
    // Private helper methods
    // ========================================================================

    fn build_script_registry(nodes: &[XfaNode]) -> ScriptRegistry {
        let mut registry = ScriptRegistry::new();

        fn build_parent_child_map(nodes: &[XfaNode]) -> HashMap<String, Vec<(String, String)>> {
            let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();

            fn collect(
                nodes: &[XfaNode],
                parent: Option<&str>,
                map: &mut HashMap<String, Vec<(String, String)>>,
            ) {
                for node in nodes {
                    let name = node.name.clone().unwrap_or_default();
                    let id = node.attributes.get("id").cloned().unwrap_or_default();

                    let is_field = node.kind.is_field();
                    let is_subform = node.kind.is_subform();
                    let is_excl_group = node.kind.is_exclgroup();

                    if is_field
                        && !name.is_empty()
                        && let Some(p) = parent
                    {
                        map.entry(p.to_string())
                            .or_default()
                            .push((name.clone(), id.clone()));
                    }

                    let next_parent = if (is_subform || is_excl_group) && !name.is_empty() {
                        Some(name.as_str())
                    } else {
                        parent
                    };

                    collect(&node.children, next_parent, map);
                }
            }

            collect(nodes, None, &mut map);
            map
        }

        let parent_child_map = build_parent_child_map(nodes);

        fn collect_scripts(
            nodes: &[XfaNode],
            parent_path: &str,
            registry: &mut ScriptRegistry,
            parent_child_map: &HashMap<String, Vec<(String, String)>>,
        ) {
            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                let name = node.name.clone().unwrap_or_default();
                let node_path = if name.is_empty() {
                    parent_path.to_string()
                } else {
                    child_som_path(parent_path, &name, index)
                };

                let child_fields = parent_child_map.get(&name).cloned().unwrap_or_default();

                let scripts = parse_events_from_node(&node.children);
                for script in scripts {
                    let script_type = ScriptType::from_activity(&script.activity);

                    registry.register(RegisteredScript {
                        script,
                        owner_path: SomPath::new(node_path.clone()),
                        owner_name: name.clone(),
                        child_fields: child_fields.clone(),
                        script_type,
                    });
                }

                collect_scripts(&node.children, &node_path, registry, parent_child_map);
            }
        }

        collect_scripts(nodes, "", &mut registry, &parent_child_map);
        registry
    }

    fn build_field_index_cache(flattened: &Flattened) -> HashMap<String, usize> {
        let mut cache = HashMap::new();
        for (idx, node) in flattened.iter_nodes().enumerate() {
            match &node.kind {
                FlattenedNodeKind::Field { name, .. } => {
                    cache.insert(name.clone(), idx);
                }
                FlattenedNodeKind::Text {
                    source_name: Some(name),
                    ..
                } => {
                    cache.insert(name.clone(), idx);
                }
                _ => {}
            }
        }
        cache
    }

    fn find_xfa_node_by_path<'a>(nodes: &'a [XfaNode], path: &str) -> Option<&'a XfaNode> {
        super::som::walk_som_path(nodes, path)
    }

    fn find_xfa_node_by_path_mut<'a>(
        nodes: &'a mut [XfaNode],
        path: &str,
    ) -> Option<&'a mut XfaNode> {
        super::som::walk_som_path_mut(nodes, path)
    }

    /// Write every `access` a script changed since the last sync onto the
    /// node tree, which is what both layout (an open field gets its fill-in
    /// line) and [`Self::effective_access`] read. True when anything changed.
    fn sync_access_from_engine(&mut self) -> bool {
        let changes = self.script_engine.take_access_changes();
        crate::xfa::script_executor::ScriptExecutor::apply_access_changes(&mut self.nodes, &changes);
        !changes.is_empty()
    }

    fn apply_presence_by_path(nodes: &mut [XfaNode], som_path: &str, presence: Presence) {
        if let Some(node) = super::som::walk_som_path_mut(nodes, som_path) {
            node.set_presence(presence);
        }
    }

    fn find_node_scripts(&self, path: &str, activity: &EventActivity) -> Vec<XfaScript> {
        if let Some(node) = Self::find_xfa_node_by_path(&self.nodes, path) {
            parse_events_from_node(&node.children)
                .into_iter()
                .filter(|script| &script.activity == activity)
                .collect()
        } else {
            vec![]
        }
    }

    /// Find scripts on an ancestor node that have `listen="refAndDescendents"`
    /// matching the given activity.
    /// Per XFA 3.3 §10 p.387: these scripts fire when a descendant triggers
    /// the same activity.
    fn find_propagating_scripts(
        &self,
        ancestor_path: &SomPath,
        activity: &EventActivity,
    ) -> Vec<XfaScript> {
        if let Some(node) = Self::find_xfa_node_by_path(&self.nodes, ancestor_path) {
            parse_events_from_node(&node.children)
                .into_iter()
                .filter(|script| {
                    &script.activity == activity && script.listen == ListenScope::RefAndDescendents
                })
                .collect()
        } else {
            vec![]
        }
    }

    fn extract_and_register_translations(nodes: &[XfaNode], engine: &mut XfaScriptEngine) {
        // Register <text> variables as fields using the shared helper
        let text_vars = crate::xfa::collect_text_variables(nodes);
        for (name, value) in &text_vars {
            engine.register_field(name, name, value);
        }

        // Collect and register <script> variables using the shared helper
        let scripts = crate::xfa::collect_variable_scripts(nodes);

        for (name, content) in &scripts {
            // Per XFA 3.3 §10 pp. 376-378: named script objects expose all
            // top-level variables and functions as properties/methods.
            let script_src = super::script_object::wrap_script_object(name, content, true);

            let _ = engine.execute_script(&XfaScript {
                source: script_src,
                content_type: ScriptContentType::JavaScript,
                activity: EventActivity::Initialize,
                event_ref: EventRef::Form,
                name: Some(name.clone()),
                run_at: RunAt::Client,
                listen: ListenScope::default(),
            });
        }
    }

    fn build_som_hierarchy_with_values(
        nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        prototypes: &Prototypes,
        engine: &mut XfaScriptEngine,
    ) {
        /// Second pass: walk `Element { tag_name: "form" }` subtrees and
        /// update initial_presence + form_state values on entries that were
        /// already registered by the template pass.  Does NOT create new JS
        /// objects or touch the SOM hierarchy.
        ///
        /// Per XFA 3.3 §3: the `<form>` packet is a saved snapshot of the
        /// Form DOM.  On reload the Form DOM is rebuilt from the Template DOM
        /// and then the saved content is applied as updates.
        fn update_from_form_dom(
            nodes: &[XfaNode],
            path: &str,
            computed_values: &HashMap<SomPath, String>,
            engine: &mut XfaScriptEngine,
        ) {
            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                let node_path = match &node.name {
                    Some(name) => child_som_path(path, name, index),
                    None => path.to_string(),
                };

                let is_registrable = matches!(
                    node.kind,
                    XfaNodeKind::Field
                        | XfaNodeKind::Subform
                        | XfaNodeKind::ExclGroup
                        | XfaNodeKind::Draw
                ) || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "draw");

                if is_registrable && node.name.is_some() {
                    let value = get_node_value(node, &node_path, computed_values);
                    let presence = node.get_presence().as_str();
                    let som_path = SomPath::new(&node_path);
                    engine.update_field_presence_baseline(&som_path, &value, presence);
                }

                // Recurse into children of the form DOM subtree.
                update_from_form_dom(&node.children, &node_path, computed_values, engine);
            }
        }

        // Pass 1: Register Template DOM nodes (skip <form> subtrees).
        register_fields(nodes, "", computed_values, prototypes, engine, false);

        // Pass 2: Apply Form DOM state to already-registered entries.
        for node in nodes {
            if matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "form") {
                update_from_form_dom(&node.children, "", computed_values, engine);
            }
        }
    }
}

fn get_node_value(
    node: &XfaNode,
    path: &str,
    computed_values: &HashMap<SomPath, String>,
) -> String {
    if let Some(value) = computed_values.get(path) {
        return value.clone();
    }
    if let Some(name) = &node.name
        && let Some(value) = computed_values.get(name.as_str())
    {
        return value.clone();
    }
    if let Some(raw) = node.attributes.get("rawValue") {
        return raw.clone();
    }
    for child in &node.children {
        if matches!(child.kind, XfaNodeKind::Value) {
            for text_child in &child.children {
                if let XfaNodeKind::Text { content } = &text_child.kind
                    && !content.is_empty()
                {
                    return content.clone();
                }
                if let XfaNodeKind::Element {
                    text_content: Some(content),
                    ..
                } = &text_child.kind
                    && !content.is_empty()
                {
                    return content.clone();
                }
            }
        }
    }
    String::new()
}

/// First pass: register Template DOM nodes in the SOM hierarchy.
/// Skips `Element { tag_name: "form" }` subtrees — those are handled
/// by the second pass below.
fn register_fields(
    nodes: &[XfaNode],
    path: &str,
    computed_values: &HashMap<SomPath, String>,
    prototypes: &Prototypes,
    engine: &mut XfaScriptEngine,
    parent_is_exclgroup: bool,
) {
    for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
        // Skip the Form DOM packet entirely — it is processed in a
        // dedicated second pass that only updates existing entries.
        if matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "form") {
            continue;
        }

        let node_path = match &node.name {
            Some(name) => child_som_path(path, name, index),
            None => path.to_string(),
        };
        register_node_at(
            node,
            &node_path,
            path,
            computed_values,
            prototypes,
            engine,
            parent_is_exclgroup,
        );
    }
}

/// Register one node at `node_path` (its parent at `path`), then its
/// descendants. Split from [`register_fields`] so a single new instance of a
/// repeated section can be registered at its own index.
fn register_node_at(
    node: &XfaNode,
    node_path: &str,
    path: &str,
    computed_values: &HashMap<SomPath, String>,
    prototypes: &Prototypes,
    engine: &mut XfaScriptEngine,
    parent_is_exclgroup: bool,
) {
    let node_path = node_path.to_string();

    let is_excl_group = node.kind.is_exclgroup();

    let is_draw = node.kind.is_draw();

    let is_field = node.kind.is_field();
    let is_subform = node.kind.is_subform();

    if (is_field || is_subform || is_excl_group || is_draw)
        && let Some(name) = &node.name
    {
        let value = get_node_value(node, &node_path, computed_values);
        let initial_presence = node.get_presence().as_str();

        if parent_is_exclgroup {
            // Extract item values from <items> for exclGroup
            // parent→child propagation (XFA 3.3 §4 pp.195-197,
            // §17 pp.758-759).
            let (item_key, off_value) = node.extract_item_values();
            // A choice-list field's own <items>, seeding its
            // addItem/clearItems/... methods (§6) -- unusual
            // inside an exclGroup, but harmless to keep populated
            // exactly like the non-exclGroup path below.
            let choice_items = if is_field {
                node.extract_choice_list_items()
            } else {
                Vec::new()
            };

            // Use register_xfa_node with structural exclGroup info
            // so _exclGroupParent linkage is set up correctly.
            engine.register_xfa_node(
                name,
                &node_path,
                if path.is_empty() { None } else { Some(path) },
                is_field,
                &value,
                true,
                item_key.as_deref(),
                off_value.as_deref(),
                initial_presence,
                &choice_items,
            );
        } else {
            engine.register_field_with_presence(
                &node_path,
                name,
                &value,
                initial_presence,
                is_subform,
            );
        }

        // `access` is a property of field, exclGroup and subform only
        // (XFA 3.3 §17); a draw has none.
        if !is_draw {
            engine.init_access(&node_path, node.get_access());
        }

        // Every repeatable declared under this subform gets its
        // occurrence limits, even one with no instance yet.
        if is_subform {
            let parent = SomPath::new(&node_path);
            for (child, prototype) in prototypes.repeatables_under(&parent) {
                engine.install_instance_manager(&parent, child, &prototype.occur);
            }
        }
    }

    register_fields(
        &node.children,
        &node_path,
        computed_values,
        prototypes,
        engine,
        is_excl_group,
    );
}
