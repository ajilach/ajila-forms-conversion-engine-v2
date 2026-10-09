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
use crate::xfa::scripting::som::{child_som_path, sibling_indices};
use crate::xfa::scripting::{SomPath, XfaForm};
use crate::flattened::{Flattened, WidgetKind};
use crate::xfa::{XfaNode, XfaNodeKind};

/// The kind of interactive field found in the XFA tree: its `<ui>` widget
/// (XFA 3.3 §17 `ui`).
// ADDITION: public so the on-demand state layer can list a form's controls
// without enumerating its state space.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractiveFieldKind {
    /// Radio button (checkButton with shape="round", inside an exclGroup)
    Radio,
    /// Checkbox (checkButton with shape="square")
    Checkbox,
    /// Dropdown (choiceList). Options are resolved dynamically from the live form
    /// at exploration time, since they may come from merged data or scripts.
    Dropdown,
    /// Button (`<ui><button/>`): no value, it is pressed. Its click script
    /// is what it does -- on a repeatable section, commonly adding or
    /// removing an instance (XFA 3.3 §9).
    Button,
    /// Single-line text (`textEdit`).
    Text,
    /// Multi-line text (`textEdit multiLine="1"`).
    TextArea,
    /// `dateTimeEdit` with a date, time or date-and-time picker.
    Date,
    Time,
    DateTime,
    /// `numericEdit`.
    Numeric,
    /// `passwordEdit`.
    Password,
    /// `signature`.
    Signature,
    /// `barcode`: its value is drawn as bars.
    Barcode,
    /// `imageEdit`.
    Image,
}

impl From<WidgetKind> for InteractiveFieldKind {
    fn from(kind: WidgetKind) -> Self {
        match kind {
            WidgetKind::Radio => InteractiveFieldKind::Radio,
            WidgetKind::Checkbox => InteractiveFieldKind::Checkbox,
            WidgetKind::Dropdown => InteractiveFieldKind::Dropdown,
            WidgetKind::Button => InteractiveFieldKind::Button,
            WidgetKind::Text => InteractiveFieldKind::Text,
            WidgetKind::TextArea => InteractiveFieldKind::TextArea,
            WidgetKind::Date => InteractiveFieldKind::Date,
            WidgetKind::Time => InteractiveFieldKind::Time,
            WidgetKind::DateTime => InteractiveFieldKind::DateTime,
            WidgetKind::Numeric => InteractiveFieldKind::Numeric,
            WidgetKind::Password => InteractiveFieldKind::Password,
            WidgetKind::Signature => InteractiveFieldKind::Signature,
            WidgetKind::Barcode => InteractiveFieldKind::Barcode,
            WidgetKind::Image => InteractiveFieldKind::Image,
        }
    }
}

/// An interactive field (radio button, checkbox, dropdown or button) with its
/// SOM path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractiveField {
    /// The SOM path uniquely identifying this field
    pub path: SomPath,
    /// The kind of selectable field
    pub kind: InteractiveFieldKind,
}

impl InteractiveField {
    fn new(path: SomPath, kind: InteractiveFieldKind) -> Self {
        Self { path, kind }
    }

    /// Returns true if this is a radio button
    fn is_radio(&self) -> bool {
        matches!(self.kind, InteractiveFieldKind::Radio)
    }
}

/// Get every field of the form that has a widget, in SOM path order.
///
/// Every one, whatever its `access`: a field a person cannot change right
/// now is still on the form, a script can unlock it (XFA 3.3 §17 lets
/// scripts change `access`), and whether it is locked is reported as data --
/// see [`XfaForm::effective_access`] -- rather than by leaving it out. The
/// same goes for whether a script reads it; see [`field_affects_layout`].
pub fn get_all_interactive_fields_ordered(form: &XfaForm) -> Vec<InteractiveField> {
    let mut results = Vec::new();
    search_interactive_fields(form.xfa_nodes(), "", &mut results);

    // Sort by SOM path to ensure consistent global ordering
    results.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    results
}

/// Whether the form's own scripts react to this field: exactly the predicate
/// that used to gate [`get_all_interactive_fields_ordered`]'s result, before
/// interaction needed every control listed regardless. Kept as its own
/// function so a caller can still tell which controls are worth trying first
/// on a form with many.
pub fn field_affects_layout(form: &XfaForm, field: &InteractiveField) -> bool {
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

/// Search for every field with a widget in the XFA tree, classified by
/// [`Flattened::extract_widget_kind`], the same reading layout uses. The
/// `<form>` data packet is skipped: it repeats the template's field names to
/// carry their saved values, and is not a second set of fields.
fn search_interactive_fields(
    nodes: &[XfaNode],
    current_path: &str,
    results: &mut Vec<InteractiveField>,
) {
    for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
        if matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "form") {
            continue;
        }
        // A name with a dot in it (AANE_019_SP has a field named
        // `ffmySP.GV_AccountHolder`) cannot be written as one SOM segment, so
        // no path reaches the node or anything inside it: it cannot be set,
        // looked up or reported on, and listing it would only hand out a path
        // that fails everywhere it is used.
        if node.name.as_deref().is_some_and(|n| n.contains('.')) {
            continue;
        }

        // Build the SOM path for this node
        let node_path = match &node.name {
            Some(name) => child_som_path(current_path, name, index),
            None => current_path.to_string(),
        };

        if matches!(&node.kind, XfaNodeKind::Field)
            && node.name.as_deref().is_some_and(|n| !n.is_empty())
            && let Some(widget) = Flattened::extract_widget_kind(node)
        {
            results.push(InteractiveField::new(
                SomPath::new(node_path.clone()),
                widget.into(),
            ));
        }

        // Recurse into children
        search_interactive_fields(&node.children, &node_path, results);
    }
}
