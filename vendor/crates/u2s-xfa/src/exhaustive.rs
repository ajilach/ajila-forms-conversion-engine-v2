//! Discovering a form's selectable controls.
//!
//! This module used to also walk the combinatorial product of every control
//! to enumerate distinct-looking states -- forty controls is a trillion
//! states, and the walk existed anyway because materializing one state
//! directly (see [`crate::states::materialize`]) was not always available.
//! Interaction replaced that need: an agent opens a session and sets one
//! control at a time, the way a person would, and never has to enumerate
//! anything to reach a state. What remains here is just the discovery half
//! that listing controls was always built on.

use crate::xfa::scripting::events::EventActivity;
use crate::xfa::scripting::{SomPath, XfaForm};
use crate::xfa::{XfaNode, XfaNodeKind};

/// The kind of selectable field found in the XFA tree.
// ADDITION: public so the on-demand state layer can list a form's controls
// without enumerating its state space.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelectableFieldKind {
    /// Radio button (checkButton with shape="round", inside an exclGroup)
    Radio,
    /// Checkbox (checkButton with shape="square")
    Checkbox,
    /// Dropdown (choiceList). Options are resolved dynamically from the live form
    /// at exploration time, since they may come from merged data or scripts.
    Dropdown,
}

/// A selectable field (radio button, checkbox, or dropdown) with its SOM path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectableField {
    /// The SOM path uniquely identifying this field
    pub path: SomPath,
    /// The kind of selectable field
    pub kind: SelectableFieldKind,
}

impl SelectableField {
    fn new(path: SomPath, kind: SelectableFieldKind) -> Self {
        Self { path, kind }
    }

    /// Returns true if this is a radio button
    fn is_radio(&self) -> bool {
        matches!(self.kind, SelectableFieldKind::Radio)
    }
}

/// Get every selectable field (radio button, checkbox, dropdown) a person
/// could actually reach.
///
/// Only the XFA 3.3 §17 access rule is applied: a field, or its parent
/// exclGroup for a radio or checkbox, whose `access` is `protected`,
/// `readOnly` or `nonInteractive` is excluded, because a person cannot touch
/// it either. Everything else is included, whether or not a script reads it
/// -- a checkbox nothing reacts to is still a field a person can click, and
/// hiding it for that reason was only ever right for the exhaustive walk
/// this list used to feed, where an unread control multiplied the space for
/// no visual difference. See [`field_affects_layout`] for that same
/// information kept as data instead of as a filter.
pub fn get_all_selectable_fields_ordered(form: &XfaForm) -> Vec<SelectableField> {
    let mut results = Vec::new();
    search_selectable_fields(form.xfa_nodes(), "", &mut results);

    results.retain(|field| {
        // Per XFA 3.3 §17: skip fields whose access (or parent exclGroup's access)
        // prevents user interaction.
        // - "protected": no events generated at all.
        // - "readOnly": no direct user changes allowed.
        // - "nonInteractive": behaves as rendering to paper.
        // Only "open" (the default) allows full user interaction.
        if let Some(resolved) = form.resolve(field.path.as_str()) {
            let access = resolved
                .xfa_node()
                .attributes
                .get("access")
                .map(|s| s.as_str());
            if matches!(access, Some("protected" | "readOnly" | "nonInteractive")) {
                return false;
            }
        }

        // For radio/checkbox in exclGroup, also check the parent exclGroup's access
        if field.is_radio() || matches!(field.kind, SelectableFieldKind::Checkbox) {
            if let Some(excl_group_path) = form.find_excl_group_for_field(field.path.as_str()) {
                if let Some(eg) = form.resolve(excl_group_path.as_str()) {
                    let eg_access = eg.xfa_node().attributes.get("access").map(|s| s.as_str());
                    if matches!(eg_access, Some("protected" | "readOnly" | "nonInteractive")) {
                        return false;
                    }
                }
            }
        }

        true
    });

    // Sort by SOM path to ensure consistent global ordering
    results.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    results
}

/// Whether the form's own scripts react to this field: exactly the predicate
/// that used to gate [`get_all_selectable_fields_ordered`]'s result, before
/// interaction needed every control listed regardless. Kept as its own
/// function so a caller can still tell which controls are worth trying first
/// on a form with many.
pub fn field_affects_layout(form: &XfaForm, field: &SelectableField) -> bool {
    let registry = form.script_registry();

    if registry.has_interactive_scripts(&field.path) {
        return true;
    }

    // For radio buttons, also check the parent exclGroup
    if field.is_radio() {
        if let Some(excl_group_path) = form.find_excl_group_for_field(field.path.as_str()) {
            if registry.has_interactive_scripts(&excl_group_path) {
                return true;
            }
        }
    }

    // Check ancestor subforms for calculate scripts. In XFA, calculate
    // scripts on subforms fire when any descendant field value changes.
    // For example, a checkbox inside a subform with a calculate script
    // that reads the checkbox value and toggles visibility of other
    // sections affects layout even if the checkbox has no own scripts.
    {
        let path_str = field.path.as_str();
        let mut pos = path_str.len();
        while let Some(dot) = path_str[..pos].rfind('.') {
            let ancestor = &path_str[..dot];
            let ancestor_path = SomPath::new(ancestor);
            if registry.has_interactive_scripts(&ancestor_path) {
                return true;
            }
            pos = dot;
        }
    }

    // Reverse-dependency check: if any Calculate/Change/Click script in
    // the form references this field's name or its exclGroup's name
    // (e.g. `RB_Group_NeW.rawValue`), setting it will trigger those scripts
    // to recalculate.
    {
        use crate::xfa::scripting::registry::ScriptType;
        let calc_scripts = registry.get_scripts_of_type(ScriptType::Calculate);
        let event_scripts = registry.get_scripts_of_type(ScriptType::Event);
        let all_interactive_sources = calc_scripts.iter().chain(event_scripts.iter()).filter(|s| {
            matches!(
                s.script.activity,
                EventActivity::Calculate | EventActivity::Change | EventActivity::Click
            ) && !crate::xfa::scripting::registry::is_comment_only(&s.script.source)
        });

        let field_name = field.path.name();
        let excl_group_name = if field.is_radio() {
            form.find_excl_group_for_field(field.path.as_str())
                .map(|p| p.name().to_string())
        } else {
            None
        };

        for source in all_interactive_sources.map(|s| s.script.source.as_str()) {
            if source.contains(&format!("{}.rawValue", field_name))
                || source.contains(&format!("{}.value", field_name))
            {
                return true;
            }
            if let Some(ref eg_name) = excl_group_name {
                if source.contains(&format!("{}.rawValue", eg_name))
                    || source.contains(&format!("{}.value", eg_name))
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Search for all selectable fields in the XFA tree: checkButtons (radio/checkbox) and choiceLists (dropdown).
fn search_selectable_fields(
    nodes: &[XfaNode],
    current_path: &str,
    results: &mut Vec<SelectableField>,
) {
    for node in nodes {
        // Build the SOM path for this node
        let node_path = if let Some(name) = &node.name {
            if current_path.is_empty() {
                name.clone()
            } else {
                format!("{}.{}", current_path, name)
            }
        } else {
            current_path.to_string()
        };

        // Check if this is a Field node
        if matches!(&node.kind, XfaNodeKind::Field) {
            let name = node.name.clone().unwrap_or_default();
            if !name.is_empty() {
                // Look for <ui> child and check for checkButton or choiceList
                let field_kind = node.children.iter().find_map(|c| {
                    if let XfaNodeKind::Element { tag_name: t, .. } = &c.kind
                        && t == "ui"
                    {
                        return c.children.iter().find_map(|ui_c| {
                            if let XfaNodeKind::Element { tag_name: t2, .. } = &ui_c.kind {
                                match t2.as_str() {
                                    "checkButton" => {
                                        let shape = ui_c
                                            .attributes
                                            .get("shape")
                                            .cloned()
                                            .unwrap_or_else(|| "square".to_string());
                                        if shape == "round" {
                                            Some(SelectableFieldKind::Radio)
                                        } else {
                                            Some(SelectableFieldKind::Checkbox)
                                        }
                                    }
                                    "choiceList" => Some(SelectableFieldKind::Dropdown),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        });
                    }
                    None
                });

                if let Some(kind) = field_kind {
                    results.push(SelectableField::new(SomPath::new(node_path.clone()), kind));
                }
            }
        }

        // Recurse into children
        search_selectable_fields(&node.children, &node_path, results);
    }
}
