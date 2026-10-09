//! The form scaffold as data: where the writer's own generated elements go
//! among a real package's raw ones, which attributes win, which defaults are
//! written, and where names must be unique. A profile whose packages differ
//! from this crate's own defaults (other attributes on `items`, a `layout`
//! before `rootPanel`, panels around the steps) describes the difference in
//! the form instead of needing a writer of its own.

use serde_json::{Value, json};
use u2s_aem::model::{AemForm, NameScope, ValidateOptions};
use u2s_mapper_aem::jcr::tree::{JcrNode, parse_jcr_xml};
use u2s_mapper_aem::xml_writer::{WriteCtx, write_form_xml};

fn raw(tag: &str) -> Value {
    json!({ "tag_name": tag, "attributes": { "jcr:primaryType": "nt:unstructured" } })
}

fn form(page_children: Value, metadata_extra: Value) -> Value {
    let mut metadata = json!({
        "form_name": "Scaffold",
        "title": { "en": "Scaffold" },
        "master_language": "en",
        "languages": ["en"],
        "dor": "none",
        "data_model": { "kind": "unbound" },
        "toolbar": []
    });
    for (k, v) in metadata_extra.as_object().unwrap() {
        metadata[k] = v.clone();
    }
    json!({
        "metadata": metadata,
        "pages": [{
            "name": "PageOne",
            "properties": {},
            "children": page_children
        }]
    })
}

fn field(name: &str) -> Value {
    json!({
        "type": "TextField",
        "common": { "name": name, "resource_type": "fd/af/components/controls/textbox" },
        "field": { "label": { "en": name } },
        "layout": { "width": 12 },
        "input": "single_line"
    })
}

fn write(json: Value, scope: NameScope, spell_defaults: bool) -> Result<JcrNode, String> {
    let form: AemForm = serde_json::from_value(json).map_err(|e| e.to_string())?;
    let form = form
        .validate_with(ValidateOptions { names: scope, ..ValidateOptions::default() })
        .map_err(|v| format!("{v:?}"))?;
    let master = form.form().metadata.master_language.clone();
    let xml = write_form_xml(
        &form,
        &WriteCtx { master: &master, bind_refs: &Default::default(), spell_defaults },
    )
    .map_err(|e| e.to_string())?;
    Ok(parse_jcr_xml(&xml).expect("the writer writes well-formed XML"))
}

fn find<'a>(node: &'a JcrNode, tag: &str) -> &'a JcrNode {
    fn walk<'a>(node: &'a JcrNode, tag: &str) -> Option<&'a JcrNode> {
        if node.tag_name == tag {
            return Some(node);
        }
        node.children.iter().find_map(|c| walk(c, tag))
    }
    walk(node, tag).unwrap_or_else(|| panic!("no <{tag}>"))
}

fn tags(node: &JcrNode) -> Vec<&str> {
    node.children.iter().map(|c| c.tag_name.as_str()).collect()
}

#[test]
fn a_components_items_go_at_its_slot_with_their_own_attributes() {
    let panel = json!({
        "type": "Component",
        "common": {
            "name": "PN_Panel", "resource_type": "fd/af/components/panel",
            "passthrough": {
                "raw_children": [raw("layout"), raw("fd:rules")],
                "slot": 1,
                "items": { "raw_attributes": { "sling:resourceType": "grid" } }
            }
        },
        "properties": {},
        "children": [field("TXT_A")]
    });
    let root = write(form(json!([panel]), json!({})), NameScope::Form, true).unwrap();
    let panel = find(&root, "PN_Panel");
    assert_eq!(tags(panel), ["layout", "items", "fd:rules"]);
    assert_eq!(find(panel, "items").attr("sling:resourceType"), Some("grid"));
}

#[test]
fn items_chrome_writes_items_even_without_typed_children() {
    let panel = json!({
        "type": "Component",
        "common": {
            "name": "PN_Empty", "resource_type": "fd/af/components/panel",
            "passthrough": { "items": { "raw_attributes": { "jcr:primaryType": "nt:unstructured" } } }
        },
        "properties": {},
        "children": []
    });
    let root = write(form(json!([panel, field("TXT_A")]), json!({})), NameScope::Form, true).unwrap();
    assert_eq!(tags(find(&root, "PN_Empty")), ["items"]);
}

#[test]
fn an_authored_attribute_replaces_the_derived_one_once() {
    let panel = json!({
        "type": "Component",
        "common": {
            "name": "PN_Panel", "resource_type": "fd/af/components/panel",
            "passthrough": { "raw_attributes": { "sling:resourceType": "customer/panel" } }
        },
        "properties": { "visible": { "kind": "single", "value": "{Boolean}false" } },
        "children": [field("TXT_A")]
    });
    let root = write(form(json!([panel]), json!({})), NameScope::Form, true).unwrap();
    let panel = find(&root, "PN_Panel");
    let count = |k: &str| panel.attributes.iter().filter(|(key, _)| key == k).count();
    assert_eq!((count("sling:resourceType"), count("visible")), (1, 1));
    assert_eq!(panel.attr("sling:resourceType"), Some("customer/panel"));
    assert_eq!(panel.attr("visible"), Some("{Boolean}false"));
}

#[test]
fn defaults_are_left_out_when_the_profile_says_so() {
    let root = write(form(json!([field("TXT_A")]), json!({})), NameScope::Form, false).unwrap();
    let text = find(&root, "TXT_A");
    for default in ["visible", "enabled", "dorExclusion", "summaryExclusion"] {
        assert_eq!(text.attr(default), None, "{default} is written");
    }
    let root = write(form(json!([field("TXT_A")]), json!({})), NameScope::Form, true).unwrap();
    assert_eq!(find(&root, "TXT_A").attr("visible"), Some("{Boolean}true"));
}

/// The levels above the steps follow their chrome: `jcr:content` around the
/// container, the container's own `layout` replaced and placed first,
/// `rootPanel`'s `items` holding panels around the steps, the toolbar's
/// attributes.
#[test]
fn the_scaffold_above_the_steps_follows_its_chrome() {
    let toolbar = json!([{
        "type": "Component",
        "common": { "name": "submit", "resource_type": "fd/af/components/actions/submit" },
        "properties": {},
        "children": []
    }]);
    let root = write(
        form(
            json!([field("TXT_A")]),
            json!({
                "page_content": {
                    "raw_attributes": { "cq:template": "/conf/t" },
                    "raw_children": [raw("parsys1"), raw("parsys2")],
                    "slot": 1
                },
                "chrome": {
                    "raw_children": [raw("layout"), raw("autoSaveInfo")],
                    "slot": 1
                },
                "root_panel": {
                    "raw_children": [raw("layout")],
                    "slot": 1,
                    "items": {
                        "raw_children": [raw("fragment_formmetadata"), raw("summarypanel")],
                        "slot": 1
                    }
                },
                "toolbar": toolbar,
                "toolbar_chrome": { "raw_attributes": { "name": "toolbar" } }
            }),
        ),
        NameScope::Form,
        true,
    )
    .unwrap();
    let content = find(&root, "jcr:content");
    assert_eq!(tags(content), ["parsys1", "guideContainer", "parsys2"]);
    assert_eq!(content.attr("cq:template"), Some("/conf/t"));
    assert_eq!(tags(find(&root, "guideContainer")), ["layout", "rootPanel", "autoSaveInfo"]);
    let root_panel = find(&root, "rootPanel");
    assert_eq!(tags(root_panel), ["layout", "items", "toolbar"]);
    assert_eq!(
        tags(find(root_panel, "items")),
        ["fragment_formmetadata", "PageOne", "summarypanel"]
    );
    assert_eq!(find(&root, "toolbar").attr("name"), Some("toolbar"));
}

#[test]
fn a_slot_past_the_raw_children_is_refused() {
    let panel = json!({
        "type": "Component",
        "common": {
            "name": "PN_Panel", "resource_type": "fd/af/components/panel",
            "passthrough": { "raw_children": [raw("layout")], "slot": 2 }
        },
        "properties": {},
        "children": [field("TXT_A")]
    });
    let error = write(form(json!([panel]), json!({})), NameScope::Form, true).unwrap_err();
    assert!(error.contains("slot of 2"), "{error}");
}

/// A name may repeat in different places when names need only be unique
/// among siblings, never twice in one list.
#[test]
fn names_are_unique_within_their_scope() {
    let panel = |name: &str, children: Value| {
        json!({
            "type": "Component",
            "common": { "name": name, "resource_type": "fd/af/components/panel" },
            "properties": {},
            "children": children
        })
    };
    let repeated = form(
        json!([panel("PN_A", json!([field("BT_Add")])), panel("PN_B", json!([field("BT_Add")]))]),
        json!({}),
    );
    assert!(write(repeated.clone(), NameScope::Form, true).is_err());
    assert!(write(repeated, NameScope::Siblings, true).is_ok());
    let twice = form(json!([field("BT_Add"), field("BT_Add")]), json!({}));
    assert!(write(twice, NameScope::Siblings, true).is_err());
}

/// Two siblings the writer would write as the same element are refused under
/// sibling scoping, whatever their `name`s: JCR holds one child per name.
#[test]
fn siblings_writing_the_same_element_are_refused() {
    let named = |name: &str, element: &str| {
        let mut node = field(name);
        node["common"]["jcr_name"] = json!(element);
        node
    };
    let clash = form(json!([named("TXT_A", "textbox_1"), named("TXT_B", "textbox_1")]), json!({}));
    let error = write(clash, NameScope::Siblings, true).unwrap_err();
    assert!(error.contains("textbox_1"), "{error}");
    let apart = form(json!([named("TXT_A", "textbox_1"), named("TXT_B", "textbox_2")]), json!({}));
    assert!(write(apart, NameScope::Siblings, true).is_ok());
}

/// Passthrough the writer has no place for is refused, not dropped: a leaf's
/// `items` slot, a field's own `cq:responsive`, `items` on a level that writes
/// none, and toolbar chrome on a form without a toolbar.
#[test]
fn passthrough_the_writer_cannot_place_is_refused() {
    let mut slotted = field("TXT_A");
    slotted["common"]["passthrough"] = json!({ "slot": 0 });
    let error = write(form(json!([slotted]), json!({})), NameScope::Form, true).unwrap_err();
    assert!(error.contains("takes no `slot`"), "{error}");

    let mut responsive = field("TXT_A");
    responsive["common"]["passthrough"] = json!({ "raw_children": [raw("cq:responsive")] });
    let error = write(form(json!([responsive]), json!({})), NameScope::Form, true).unwrap_err();
    assert!(error.contains("cq:responsive"), "{error}");

    let content_items = json!({ "page_content": { "items": {} } });
    let error = write(form(json!([field("TXT_A")]), content_items), NameScope::Form, true).unwrap_err();
    assert!(error.contains("page_content/items"), "{error}");

    let toolbar_chrome = json!({ "toolbar_chrome": { "raw_attributes": { "name": "toolbar" } } });
    let error = write(form(json!([field("TXT_A")]), toolbar_chrome), NameScope::Form, true).unwrap_err();
    assert!(error.contains("toolbar chrome"), "{error}");
}
