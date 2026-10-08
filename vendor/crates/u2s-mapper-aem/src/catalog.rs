//! The stock AEM Adaptive Forms Foundation-component catalogue
//! (`specs/AEM.md` §6 and §14's own component-family tables), offered to
//! the Conversion Agent as a browsable reference the same way
//! [`crate::fragment_library`] offers a deployment's own fragment corpus.
//!
//! **This module is not an encoder either**, for the same reason
//! [`crate::fragment_library`]'s own module doc gives: `sling:resourceType`
//! and `guideNodeClass` are agent-authored fields on
//! [`u2s_aem::model::Common`], never chosen by this crate (see that type's
//! own doc). What this module does is teach the vocabulary those fields
//! draw from -- which resource type an agent should write for a text
//! field, a panel, a toolbar button -- so the generic `aem` format's
//! `component_search` tool has something real to search, rather than the
//! agent having to already know AEM's own naming by heart.
//!
//! Every entry here is a stock, undecorated Foundation component
//! (`fd/af/components/**`, `fd/af/layouts/**`). A UBS deployment's own
//! overlay catalogue (`ajila-forms-customers/ajila-forms-ubs/...`) is not
//! catalogued here: an overlay is recognised by the decoder as a resource-
//! type *suffix* match against exactly these same entries (see
//! `crate::decode::form::decode_node`'s own doc on AEM.md §14's overlay
//! convention), so one table already covers both -- a customer profile's
//! own catalogue is that same suffix carried by a project-specific prefix,
//! never a distinct component family.
//!
//! `specs/AEM.md`'s own "Observed Deviations" appendix records two
//! documented spelling disagreements this table deliberately keeps both
//! sides of rather than picking a winner:
//!
//! - **Toolbar actions**: §6.11/§14 document
//!   `fd/af/components/{submit,previtemnav,nextitemnav}`; the real UBS
//!   fixture's own toolbar nodes sit under `fd/af/components/actions/*`
//!   instead. Both spellings are entries here, each saying so.
//! - **Toolbar layout**: §14 documents `fd/af/layouts/toolbarCommonLayout`;
//!   the fixture uses `fd/af/layouts/toolbar/defaultToolbarLayout`. Both
//!   are entries.
//!
//! Kept in step with `specs/AEM.md` by
//! `tests/catalog_matches_spec.rs`: every resource type below must appear
//! literally in that spec file, and vice versa (modulo a short, explicit
//! exclusion list for spec strings that are not components at all --
//! `sling:redirect`, `fd/fm/af/render`, and so on).

/// What kind of tree position a catalogue entry occupies -- not an AEM
/// vocabulary term itself, just enough shape for
/// [`crate::xml_writer`]'s own callers (and `component_search`'s result)
/// to know whether an entry nests children, is a single leaf, or has no
/// `guideNodeClass` at all (a layout node).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// The form-wide container (`guideContainer`) and its root panel --
    /// exactly one of each per form.
    Container,
    /// An ordinary nesting panel, including a wizard step's own panel and
    /// a fragment reference (AEM.md §6.12: a fragment is a panel carrying
    /// `fragRef`, not a distinct resource type).
    Panel,
    /// A field an end user fills in or reads -- one of
    /// [`u2s_aem::model::Node`]'s seven typed leaf variants.
    Leaf,
    /// An action button: submit, wizard navigation, or a repeatable's own
    /// add/remove pair.
    Button,
    /// A layout node (`fd/af/layouts/**`) -- carries no `guideNodeClass`
    /// at all (AEM.md §7), since it configures *how* its sibling renders
    /// rather than being a component in its own right.
    Layout,
}

/// One catalogue entry: everything `component_search` has to say about a
/// stock resource type, plus enough cross-reference (`node_type`,
/// `spec_section`) that a hit can be checked against both this crate's own
/// model and the spec it was transcribed from.
#[derive(Debug, Clone, Copy)]
pub struct ComponentEntry {
    pub resource_type: &'static str,
    /// `None` only for [`ComponentKind::Layout`] -- AEM.md §7 never gives
    /// a layout node its own `guideNodeClass`.
    pub guide_node_class: Option<&'static str>,
    /// The `u2s_aem::model::Node` variant this resource type is written
    /// under, when one exists. `None` for a layout node (never a `Node`
    /// itself -- see [`u2s_aem::model::FieldLayout`]/`PanelLayout`
    /// instead) and for a button (every button is a plain
    /// `Node::Component`, per that enum's own doc on why only structural
    /// judgment calls collapse into `Component`).
    pub node_type: Option<&'static str>,
    pub kind: ComponentKind,
    /// Whether this component needs a `<cq:responsive>` child (AEM.md
    /// §7.3) -- true for every leaf field, false for panels, containers,
    /// buttons and layout nodes, none of which carry one.
    pub needs_responsive: bool,
    /// Real prompt surface: what an agent reads before choosing this
    /// entry. Names the deviation directly when this table's own module
    /// doc documents one for this resource type.
    pub description: &'static str,
    /// The resource-type-specific attributes AEM.md documents for this
    /// component, beyond the common set (§6.1: `name`, `jcr:title`,
    /// `visible`, `enabled`, `mandatory`, `css`, ...) every component
    /// already carries via `Common`/`FieldCommon`.
    pub attributes: &'static [&'static str],
    /// The `specs/AEM.md` section this entry was transcribed from, e.g.
    /// `"6.3"` -- what `tests/catalog_matches_spec.rs` cross-checks
    /// against, and what a search hit's own citation points a reader back
    /// to.
    pub spec_section: &'static str,
}

/// The stock Foundation-component table, `specs/AEM.md` §6/§14.
pub static FOUNDATION: &[ComponentEntry] = &[
    ComponentEntry {
        resource_type: "fd/af/components/guideContainer",
        guide_node_class: Some("guideContainerNode"),
        node_type: None,
        kind: ComponentKind::Container,
        needs_responsive: false,
        description: "The form-wide container -- exactly one per form, wrapping the root panel and the toolbar.",
        attributes: &["fd:version", "dorType"],
        spec_section: "5.3",
    },
    ComponentEntry {
        resource_type: "fd/af/components/rootPanel",
        guide_node_class: Some("guideRootPanel"),
        node_type: None,
        kind: ComponentKind::Container,
        needs_responsive: false,
        description: "The form's own root panel -- the direct parent of every wizard step (Page).",
        attributes: &[],
        spec_section: "5.4",
    },
    ComponentEntry {
        resource_type: "fd/af/components/panel",
        guide_node_class: Some("guidePanel"),
        node_type: Some("Component"),
        kind: ComponentKind::Panel,
        needs_responsive: false,
        description: "A generic container panel. Nests other components; also the resource type a fragment reference uses (a fragment is a panel carrying a fragRef property, not a distinct component).",
        attributes: &[
            "panelSetType",
            "validateOnStepCompletion",
            "dorColspan",
            "dorLayoutType",
            "dorNumCols",
            "fragRef",
        ],
        spec_section: "6.2",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/textbox",
        guide_node_class: Some("guideTextBox"),
        node_type: Some("TextField"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Single-line or multi-line text input.",
        attributes: &["maxChars", "multiLine", "placeholderText"],
        spec_section: "6.3",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/numericbox",
        guide_node_class: Some("guideNumericBox"),
        node_type: Some("NumberField"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Numeric input field, with an optional validate/display picture clause.",
        attributes: &[
            "validatePictureClause",
            "displayPictureClause",
            "displayPatternType",
            "displayIsSameAsValidate",
        ],
        spec_section: "6.4",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/datepicker",
        guide_node_class: Some("guideDatePicker"),
        node_type: Some("DatePicker"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Date input with a calendar picker.",
        attributes: &[
            "defaultToCurrentDate",
            "validatePictureClause",
            "validatePictureClauseMessage",
            "validationPatternType",
            "displayPictureClause",
            "yearRangeFrom",
            "yearRangeTo",
        ],
        spec_section: "6.5",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/dropdownlist",
        guide_node_class: Some("guideDropDownList"),
        node_type: Some("Dropdown"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Select / dropdown input over a fixed option list.",
        attributes: &["options", "filteringAllowed", "sort"],
        spec_section: "6.6",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/checkbox",
        guide_node_class: Some("guideCheckBox"),
        node_type: Some("Checkbox"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Checkbox input, single or group.",
        attributes: &["options", "alignment", "richTextOptions", "hideTitle"],
        spec_section: "6.7",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/radiobutton",
        guide_node_class: Some("guideRadioButton"),
        node_type: Some("RadioButton"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Radio button group over a fixed option list.",
        attributes: &["options", "alignment", "richTextOptions"],
        spec_section: "6.8",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/textdraw",
        guide_node_class: Some("guideTextDraw"),
        node_type: Some("StaticText"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Static text or a heading -- not an input. Content is rich text carried in _value.",
        attributes: &["_value", "headingLevel"],
        spec_section: "6.9",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/scribble",
        guide_node_class: Some("guideScribble"),
        node_type: Some("Signature"),
        kind: ComponentKind::Leaf,
        needs_responsive: true,
        description: "Signature / drawing pad input.",
        attributes: &[],
        spec_section: "6.10",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/removebutton",
        guide_node_class: Some("guideButton"),
        node_type: None,
        kind: ComponentKind::Button,
        needs_responsive: false,
        description: "Removes one instance of a repeatable panel. Sits inside the repeatable's own toolbar, not its items.",
        attributes: &[],
        spec_section: "6.11 / Appendix: Repeatable Panels",
    },
    ComponentEntry {
        resource_type: "fd/af/components/controls/tertiarybutton",
        guide_node_class: Some("guideButton"),
        node_type: None,
        kind: ComponentKind::Button,
        needs_responsive: false,
        description: "Adds a new instance of a repeatable panel. Sits outside the repeatable's own inner panel, alongside it.",
        attributes: &["type"],
        spec_section: "6.11 / Appendix: Repeatable Panels",
    },
    ComponentEntry {
        resource_type: "fd/af/components/submit",
        guide_node_class: Some("guideButton"),
        node_type: None,
        kind: ComponentKind::Button,
        needs_responsive: false,
        description: "The form's submit button. The real UBS fixture instead nests toolbar actions under fd/af/components/actions/submit -- both are documented spellings; follow whichever this deployment's own real packages use.",
        attributes: &[],
        spec_section: "6.11",
    },
    ComponentEntry {
        resource_type: "fd/af/components/previtemnav",
        guide_node_class: Some("guideButton"),
        node_type: None,
        kind: ComponentKind::Button,
        needs_responsive: false,
        description: "Previous wizard step navigation. The real UBS fixture instead nests toolbar actions under fd/af/components/actions/previtemnav -- both are documented spellings; follow whichever this deployment's own real packages use.",
        attributes: &[],
        spec_section: "6.11 / 16",
    },
    ComponentEntry {
        resource_type: "fd/af/components/nextitemnav",
        guide_node_class: Some("guideButton"),
        node_type: None,
        kind: ComponentKind::Button,
        needs_responsive: false,
        description: "Next wizard step navigation. The real UBS fixture instead nests toolbar actions under fd/af/components/actions/nextitemnav -- both are documented spellings; follow whichever this deployment's own real packages use.",
        attributes: &[],
        spec_section: "6.11 / 16",
    },
    ComponentEntry {
        resource_type: "fd/af/layouts/gridFluidLayout2",
        guide_node_class: None,
        node_type: None,
        kind: ComponentKind::Layout,
        needs_responsive: false,
        description: "Responsive grid layout, v2 -- the standard child of a panel's or root panel's own items and layout nodes.",
        attributes: &[
            "enableLayoutOptimization",
            "nonNavigable",
            "toolbarPosition",
        ],
        spec_section: "7 / 14",
    },
    ComponentEntry {
        resource_type: "fd/af/layouts/gridFluidLayout",
        guide_node_class: None,
        node_type: None,
        kind: ComponentKind::Layout,
        needs_responsive: false,
        description: "Responsive grid layout, v1 -- superseded by gridFluidLayout2 for new forms; catalogued for completeness against real packages that still carry it.",
        attributes: &[],
        spec_section: "14",
    },
    ComponentEntry {
        resource_type: "fd/af/layouts/defaultGuideLayout",
        guide_node_class: None,
        node_type: None,
        kind: ComponentKind::Layout,
        needs_responsive: false,
        description: "guideContainer's own layout child, a sibling of rootPanel -- distinct from rootPanel's own items/layout pair.",
        attributes: &[],
        spec_section: "Appendix: Observed Deviations",
    },
    ComponentEntry {
        resource_type: "fd/af/layouts/toolbarCommonLayout",
        guide_node_class: None,
        node_type: None,
        kind: ComponentKind::Layout,
        needs_responsive: false,
        description: "The toolbar's own layout, per §14's component-family table. The real UBS fixture instead uses fd/af/layouts/toolbar/defaultToolbarLayout -- both are documented spellings; follow whichever this deployment's own real packages use.",
        attributes: &[],
        spec_section: "14",
    },
    ComponentEntry {
        resource_type: "fd/af/layouts/toolbar/defaultToolbarLayout",
        guide_node_class: None,
        node_type: None,
        kind: ComponentKind::Layout,
        needs_responsive: false,
        description: "The toolbar's own layout, per the real UBS fixture -- see toolbarCommonLayout's own entry for the spec's documented alternative spelling.",
        attributes: &[],
        spec_section: "Appendix: Observed Deviations",
    },
];

/// Every entry whose resource type, `guideNodeClass` or description
/// matches `query` (case-insensitive substring). An empty or absent query
/// returns the *whole* catalogue rather than nothing -- unlike
/// [`crate::fragment_library::FragmentLibrary::search`], this table is
/// small and closed, so browsing it in full is the point rather than
/// something a query needs to unlock.
pub fn search(query: &str) -> Vec<&'static ComponentEntry> {
    if query.trim().is_empty() {
        return FOUNDATION.iter().collect();
    }
    FOUNDATION
        .iter()
        .filter(|entry| {
            crate::search::matches(
                query,
                &[
                    entry.resource_type,
                    entry.guide_node_class.unwrap_or(""),
                    entry.description,
                ],
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_returns_the_whole_catalogue() {
        assert_eq!(search("").len(), FOUNDATION.len());
        assert_eq!(search("   ").len(), FOUNDATION.len());
    }

    #[test]
    fn a_matching_query_finds_the_text_box() {
        let hits = search("textbox");
        assert!(
            hits.iter()
                .any(|e| e.resource_type == "fd/af/components/controls/textbox")
        );
    }

    #[test]
    fn a_query_matches_by_guide_node_class_too() {
        let hits = search("guideDropDownList");
        assert!(
            hits.iter()
                .any(|e| e.resource_type == "fd/af/components/controls/dropdownlist")
        );
    }

    #[test]
    fn a_nonsense_query_finds_nothing() {
        assert!(search("no such component exists in this catalogue").is_empty());
    }

    #[test]
    fn every_entry_has_a_resource_type_starting_with_fd_af() {
        for entry in FOUNDATION {
            assert!(
                entry.resource_type.starts_with("fd/af/"),
                "{} is not a stock fd/af/ resource type",
                entry.resource_type
            );
        }
    }

    #[test]
    fn only_layout_entries_lack_a_guide_node_class() {
        for entry in FOUNDATION {
            match entry.kind {
                ComponentKind::Layout => assert!(entry.guide_node_class.is_none()),
                _ => assert!(
                    entry.guide_node_class.is_some(),
                    "{} should carry a guideNodeClass",
                    entry.resource_type
                ),
            }
        }
    }

    #[test]
    fn resource_types_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in FOUNDATION {
            assert!(
                seen.insert(entry.resource_type),
                "duplicate entry: {}",
                entry.resource_type
            );
        }
    }
}
