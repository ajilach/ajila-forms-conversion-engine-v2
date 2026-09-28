//! XSD generation and `bindRef` assignment, in one mechanical walk.
//!
//! The reference engine derives `bindRef` by matching each panel against a
//! fragment library's own declared XSD types -- a business judgment this
//! crate deliberately does not make (see the crate's own module doc).
//! `u2s-aem`'s model removes the need for
//! that: every [`u2s_aem::model::ComponentName`] is already unique
//! form-wide (`AemForm::validate`), so a `bindRef` can be a pure function
//! of a node's position in the tree -- no matching, no external data, no
//! two-phase derivation.
//!
//! **Design note on the exact path syntax.** `AEM.md` (this workspace's own
//! reverse-engineered AEM format reference) does not specify a `bindRef`
//! grammar -- its `formmodel`/`bindRef` sections are aspirational
//! (`u2s-aem`'s doc comments cite a "§19" that does not exist in the
//! current spec). Absent an authoritative grammar, this module uses the
//! most defensible mechanical choice: a `bindRef` is the `/`-joined path of
//! [`ComponentName`]s from the schema root to the node, which is both a
//! standard XML/XPath-shaped binding and trivially re-derivable from the
//! tree alone. If a real target AEM instance requires a different SOM
//! syntax, that is a schema-format decision to revisit here, in one place
//! -- it does not touch the writer, which only ever asks "does this node
//! have a bindRef" and, if so, "what is it".
//!
//! **What gets an XSD element and a `bindRef`, and what does not.** A page
//! is a wizard step, not a data grouping, so pages do not nest the schema
//! -- their children bind directly under whatever their parent bound to.
//! [`Node::StaticText`] carries no value, and a `Component` carrying a
//! `fragRef` property has a data model external to this tree (its own
//! referenced content owns it, the same reasoning that used to justify a
//! dedicated `Node::Fragment` variant before this crate's redesign
//! collapsed structural nodes into `Component` -- see `u2s-aem`'s own
//! `Node` doc) -- neither gets an element or a `bindRef`. Every other node
//! gets exactly one; a childless, frag-ref-less `Component` (a leaf-like
//! generic node -- a `messagebox`, a `summary`, ...) gets a plain string
//! leaf, the same as the fallback arm below.

use std::fmt::Write as _;

use u2s_aem::model::{ComponentName, DataModel, Node, Page, ValidForm, XmlName};

/// The generated schema plus every `bindRef` it assigns, keyed by the node
/// whose element it names -- `None` when the form has no data model at all
/// ([`DataModel::Unbound`]), which is a legitimate, ordinary state (AEM.md
/// §5.3's `formmodel="none"`), not an error.
pub struct XsdResult {
    /// The XSD document, or `None` for [`DataModel::Unbound`].
    pub schema_xml: Option<String>,
    /// One entry per node that received a `bindRef`, keyed by
    /// [`ComponentName`] -- unique form-wide, so this is an unambiguous
    /// lookup for the XML writer.
    pub bind_refs: std::collections::HashMap<ComponentName, String>,
}

/// Walks a validated form once, generating its XSD (when bound) and every
/// node's `bindRef` in the same pass -- so the two can never disagree by
/// construction, unlike the reference engine's own *two*-phase derivation,
/// which risks exactly that.
pub fn generate(form: &ValidForm) -> XsdResult {
    let mut bind_refs = std::collections::HashMap::new();

    let DataModel::XmlSchema { root_element } = &form.form().metadata.data_model else {
        return XsdResult { schema_xml: None, bind_refs };
    };

    let mut body = String::new();
    let root_path = format!("/{root_element}");
    for page in &form.form().pages {
        walk_children(&page.children, &root_path, &mut bind_refs, &mut body);
    }

    let schema_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" elementFormDefault="qualified">
  <xs:element name="{root_element}">
    <xs:complexType>
      <xs:sequence>
{body}      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>
"#
    );

    XsdResult {
        schema_xml: Some(schema_xml),
        bind_refs,
    }
}

/// `Page` isn't a [`Node`] (it cannot appear anywhere but as a direct root
/// child, by the type's own shape -- see `u2s-aem`'s doc comment on
/// [`Page`]), so its children are walked directly rather than through
/// [`walk_node`].
fn walk_children(
    nodes: &[Node],
    parent_path: &str,
    bind_refs: &mut std::collections::HashMap<ComponentName, String>,
    body: &mut String,
) {
    for node in nodes {
        walk_node(node, parent_path, bind_refs, body);
    }
}

fn walk_node(
    node: &Node,
    parent_path: &str,
    bind_refs: &mut std::collections::HashMap<ComponentName, String>,
    body: &mut String,
) {
    if matches!(node, Node::StaticText { .. }) {
        return;
    }
    // A `Component` carrying a `fragRef` property has a data model
    // external to this tree -- see the module doc.
    if let Node::Component { properties, .. } = node
        && properties
            .keys()
            .any(|key| key.as_str() == "fragRef")
    {
        return;
    }

    let name = node.name();
    let path = format!("{parent_path}/{name}");
    bind_refs.insert(name.clone(), path.clone());

    match node {
        Node::Component { children, .. } if !children.is_empty() => {
            let _ = writeln!(body, "        <xs:element name=\"{name}\">");
            let _ = writeln!(body, "          <xs:complexType>");
            let _ = writeln!(body, "            <xs:sequence>");
            let mut inner = String::new();
            walk_children(children, &path, bind_refs, &mut inner);
            body.push_str(&inner);
            let _ = writeln!(body, "            </xs:sequence>");
            let _ = writeln!(body, "          </xs:complexType>");
            let _ = writeln!(body, "        </xs:element>");
        }
        Node::NumberField { .. } => {
            let _ = writeln!(body, "        <xs:element name=\"{name}\" type=\"xs:decimal\" minOccurs=\"0\"/>");
        }
        Node::DatePicker { .. } => {
            let _ = writeln!(body, "        <xs:element name=\"{name}\" type=\"xs:date\" minOccurs=\"0\"/>");
        }
        // A childless `Component` (a leaf-like generic node), TextField,
        // Dropdown, Checkbox, RadioButton, Signature: a plain string leaf.
        // Distinguishing a choice's declared option values into an
        // `xs:restriction` enumeration is a real refinement, but not one
        // that changes whether the tree binds correctly -- left for the
        // rule triage (Stage 4) to decide is worth the extra XSD surface,
        // rather than assumed here.
        _ => {
            let _ = writeln!(body, "        <xs:element name=\"{name}\" type=\"xs:string\" minOccurs=\"0\"/>");
        }
    }
}

pub fn root_element_name(form: &ValidForm) -> Option<&XmlName> {
    match &form.form().metadata.data_model {
        DataModel::XmlSchema { root_element } => Some(root_element),
        DataModel::Unbound => None,
    }
}

/// So the writer never has to re-derive `Page`'s own binding path
/// (`Page` is never given a `bindRef` of its own -- see the module doc on
/// why pages do not nest the schema).
pub fn page_bind_prefix(form: &ValidForm) -> Option<String> {
    root_element_name(form).map(|root| format!("/{root}"))
}

#[allow(dead_code)]
fn touch_page(_: &Page) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn unbound_forms_get_no_schema_and_no_bind_refs() {
        let form = build_form(DataModel::Unbound, vec![a_page_with(vec![a_text_field("Name")])]);
        let result = generate(&form);
        assert!(result.schema_xml.is_none());
        assert!(result.bind_refs.is_empty());
    }

    #[test]
    fn a_bound_forms_leaf_gets_a_bind_ref_under_the_root() {
        let form = build_form(xml_schema("Root"), vec![a_page_with(vec![a_text_field("Name")])]);
        let result = generate(&form);
        assert!(result.schema_xml.as_deref().unwrap().contains("name=\"Root\""));
        assert_eq!(
            result.bind_refs.get(&component_name("Name")).map(String::as_str),
            Some("/Root/Name")
        );
    }

    #[test]
    fn panels_nest_the_schema_and_the_bind_ref() {
        let form = build_form(
            xml_schema("Root"),
            vec![a_page_with(vec![a_panel("Address", vec![a_text_field("Street")])])],
        );
        let result = generate(&form);
        assert_eq!(
            result.bind_refs.get(&component_name("Street")).map(String::as_str),
            Some("/Root/Address/Street")
        );
        let schema = result.schema_xml.unwrap();
        assert!(schema.contains("name=\"Address\""));
        assert!(schema.contains("name=\"Street\""));
    }

    // `repeatables_carry_their_min_and_max_occur_into_the_schema` used to
    // exercise the typed `Node::Repeatable::{min_occur,max_occur}` fields
    // feeding this module's own `minOccurs`/`maxOccurs` derivation. A
    // repeatable is now two ordinary `Component` panels the agent authors
    // directly, with `minOccur`/`maxOccur` as plain string properties --
    // there is no longer a typed pair for this module to read, so the
    // XSD it emits for a `Component`-shaped repeatable is the same
    // structural nesting any other panel gets (covered by
    // `panels_nest_the_schema_and_the_bind_ref` below), without the
    // `minOccurs`/`maxOccurs` attributes. Removed, not replaced: giving
    // this module a hardcoded `minOccur`/`maxOccur` property-name lookup
    // to restore them would be exactly the kind of per-attribute
    // special-casing this crate's redesign moved out of the mapper.

    #[test]
    fn a_component_with_children_nests_the_schema_like_a_panel_used_to() {
        let form = build_form(
            xml_schema("Root"),
            vec![a_page_with(vec![a_panel(
                "Owners",
                vec![a_text_field("OwnerName")],
            )])],
        );
        let result = generate(&form);
        let schema = result.schema_xml.unwrap();
        assert!(schema.contains(r#"name="Owners""#));
        assert_eq!(
            result.bind_refs.get(&component_name("OwnerName")).map(String::as_str),
            Some("/Root/Owners/OwnerName")
        );
    }

    #[test]
    fn static_text_and_fragments_get_neither_an_element_nor_a_bind_ref() {
        let form = build_form(
            xml_schema("Root"),
            vec![a_page_with(vec![a_static_text("Intro"), a_fragment("Frag")])],
        );
        let result = generate(&form);
        assert!(!result.bind_refs.contains_key(&component_name("Intro")));
        assert!(!result.bind_refs.contains_key(&component_name("Frag")));
        let schema = result.schema_xml.unwrap();
        assert!(!schema.contains("name=\"Intro\""));
        assert!(!schema.contains("name=\"Frag\""));
    }

    #[test]
    fn pages_do_not_nest_the_schema() {
        // Two pages, each with one field: both bind directly under the
        // root, not under a per-page element -- a page is a wizard step,
        // not a data grouping.
        let form = build_form(
            xml_schema("Root"),
            vec![
                a_page_with(vec![a_text_field("First")]),
                a_page_with(vec![a_text_field("Second")]),
            ],
        );
        let result = generate(&form);
        assert_eq!(
            result.bind_refs.get(&component_name("First")).map(String::as_str),
            Some("/Root/First")
        );
        assert_eq!(
            result.bind_refs.get(&component_name("Second")).map(String::as_str),
            Some("/Root/Second")
        );
    }
}
