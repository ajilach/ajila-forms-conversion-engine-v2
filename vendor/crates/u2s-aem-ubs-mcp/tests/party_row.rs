//! A party row may hold, beside its partner generic, a content panel for the
//! fields the generic does not have (place of birth, say). A UBS fragment
//! cannot hold added content, so those fields sit in the row next to it
//! (feedback repository, `consistent-problems.md`: "A row that needs a field
//! the fragment does not have").
//!
//! The row keeps working as a party: its Add and Remove still drive the
//! signature twin, the panel repeats with the row, a load gives the row back
//! as it was written, and the schema names the row as a group holding the
//! fragment's element and the panel's fields.

use serde_json::json;
use u2s_aem_ubs_mcp::aem::AemNodeTranslated;
use u2s_aem_ubs_mcp::{UbsAemDocument, decode, encode};

mod support;
use support::package::unzip;

const ROW: &str = "/form/children/0/children/6/children/1/children/0";

/// `AABF_019` with a panel of date and place of birth beside the contracting
/// party's generic.
fn document() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden/AABF_019/document.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let row = json.pointer_mut(ROW).unwrap();
    assert_eq!(row["name"], "RCP_CPGRP");
    row["children"].as_array_mut().unwrap().push(json!({
        "type": "Panel", "uuid": "6d2b0f4e-0000-4000-8000-000000000001", "name": "PN_BirthDetails",
        "title": {"de": "", "en": "", "es": ""}, "is_page": false, "visible": true,
        "is_conditional": false, "dor_num_cols": null, "colspan": 12, "dor_colspan": null,
        "bind_ref": null, "frag_ref": null,
        "children": [
            {
                "type": "DatePicker", "uuid": "6d2b0f4e-0000-4000-8000-000000000002",
                "name": "DATE_BirthDate", "label": {"de": "Geburtsdatum", "en": "Date of birth", "es": "Fecha de nacimiento"},
                "mandatory": false, "visible": true, "colspan": 6, "dor_colspan": null, "bind_ref": null
            },
            {
                "type": "TextField", "uuid": "6d2b0f4e-0000-4000-8000-000000000003",
                "name": "TXT_BirthPlace", "kind": "Plain", "label": {"de": "Geburtsort", "en": "Place of birth", "es": "Lugar de nacimiento"},
                "mandatory": false, "max_chars": null, "visible": true, "colspan": 6, "dor_colspan": null, "bind_ref": null
            }
        ]
    }));
    json
}

/// The first element, depth first, whose `name` is `name`.
fn find<'a>(
    node: &'a u2s_mapper_aem::jcr::tree::JcrNode,
    name: &str,
) -> Option<&'a u2s_mapper_aem::jcr::tree::JcrNode> {
    if node.attr("name") == Some(name) {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, name))
}

fn form_xml(package: &[u8]) -> String {
    unzip(package)
        .into_values()
        .find(|text| text.contains("guideContainer"))
        .expect("the package holds the form")
}

#[test]
fn a_party_row_holds_a_content_panel_beside_its_fragment() {
    let doc = UbsAemDocument::from_json(&document()).unwrap();
    let build = encode(&doc).unwrap();
    let xml = form_xml(&build.package);

    // The party still drives its signature twin.
    for call in ["addInstance(RCP_SGN_CPGRP_repeat);", "removeInstance(RCP_SGN_CPGRP_repeat);"] {
        assert!(xml.contains(call), "the party's buttons must still call {call}");
    }
    // The panel is written inside the repeating row, right after the
    // fragment, so it repeats with it.
    let tree = u2s_mapper_aem::jcr::tree::parse_jcr_xml(&xml).expect("well-formed");
    let inner = find(&tree, "RCP_CPGRP_inner").expect("the party's repeating row");
    let items = inner
        .children
        .iter()
        .find(|c| c.tag_name == "items")
        .expect("the row lists its components under `items`");
    let names: Vec<&str> = items.children.iter().filter_map(|c| c.attr("name")).collect();
    let at = |n: &str| names.iter().position(|x| *x == n);
    let (fragment, panel) = (at("PN_CPGRP"), at("PN_BirthDetails"));
    assert!(
        fragment.is_some() && panel == fragment.map(|f| f + 1),
        "the row must hold the fragment, then the panel: {names:?}"
    );
    u2s_aem_ubs_mcp::aem::validate_aem_form_xml(&xml).unwrap_or_else(|e| panic!("{e:?}"));

    // A load gives the row back as written: the fragment, then the panel.
    let decoded = decode(&build.package).unwrap();
    let mut row = Vec::new();
    decoded.form.visit(&mut |node| {
        if let AemNodeTranslated::Repeatable { name, children, .. } = node
            && name == "RCP_CPGRP"
        {
            row = children
                .iter()
                .map(|c| match c {
                    AemNodeTranslated::Fragment { name, .. } => format!("Fragment {name}"),
                    AemNodeTranslated::Panel { name, .. } => format!("Panel {name}"),
                    _ => "other".into(),
                })
                .collect();
        }
    });
    assert_eq!(row, ["Fragment PN_CPGRP", "Panel PN_BirthDetails"]);
}

/// The schema names the row as a group: the fragment's element and the
/// panel's fields, repeating together, where a row of the fragment alone is
/// the fragment's element, repeated. The group takes the row's name, which
/// here is the fragment element's own (`AccountHolder`): a local element
/// beside the global one, as XSD allows.
#[test]
fn the_schema_groups_the_fragment_with_the_rows_fields() {
    let doc = UbsAemDocument::from_json(&document()).unwrap();
    let xsd = encode(&doc).unwrap().xsd.expect("the UBS profile has a schema");
    let compact: String = xsd.split_whitespace().collect::<Vec<_>>().join(" ");
    let group = concat!(
        r#"<xs:element name="AccountHolder" minOccurs="0" maxOccurs="50"> <xs:complexType> <xs:sequence> "#,
        r#"<xs:element ref="AccountHolder" minOccurs="0"/> "#,
        r#"<xs:element name="DateOfBirth" type="xs:date" minOccurs="0"/> "#,
        r#"<xs:element name="PlaceOfBirth" type="xs:string" minOccurs="0"/> "#,
        r#"</xs:sequence> </xs:complexType> </xs:element>"#
    );
    assert!(compact.contains(group), "the row must be one repeating group:\n{xsd}");
}

/// A row is one party: a second partner generic in it would make its Add and
/// Remove, and its signature twin, ambiguous.
#[test]
fn a_row_with_two_partner_generics_is_refused() {
    let mut json = document();
    let row = json.pointer_mut(ROW).unwrap();
    let mut second = row["children"][0].clone();
    second["uuid"] = json!("6d2b0f4e-0000-4000-8000-000000000004");
    second["name"] = json!("PN_CPGRP2");
    row["children"].as_array_mut().unwrap().push(second);
    let doc = UbsAemDocument::from_json(&json).unwrap();
    match encode(&doc) {
        Err(u2s_aem_ubs_mcp::Error::InvalidDocument(message)) => {
            assert!(message.contains("`RCP_CPGRP` holds 2 partner generics"), "{message}")
        }
        Err(other) => panic!("refused for another reason: {other}"),
        Ok(_) => panic!("a row with two partner generics must be refused"),
    }
}
