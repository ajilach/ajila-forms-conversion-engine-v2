//! Repeatable subforms, end to end through the default render: occurrence
//! limits materialise as real Form DOM instances (XFA 3.3 §9), each with its
//! own indexed SOM path, its own scripts and its own place in the layout.

use u2s_xfa::xfa::scripting::XfaForm;
use u2s_xfa::{Flattened, FlattenedNodeKind, XfaNode, prepare_default};

fn template(body: &str) -> Vec<XfaNode> {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="Root" layout="tb"><pageSet><pageArea name="P1" w="612pt" h="792pt">
<contentArea x="36pt" y="36pt" w="540pt" h="720pt"/></pageArea></pageSet>
<subform name="Body" layout="tb" w="540pt">{body}</subform></subform></template></xdp:xdp>"#
    );
    XfaNode::parse(xml.as_bytes()).expect("parse")
}

/// The laid-out field with exactly this SOM path, as (page, y, value).
fn field_at(flat: &Flattened, path: &str) -> Option<(u32, f64, String)> {
    flat.iter_nodes().find_map(|node| {
        let FlattenedNodeKind::Field { value, .. } = &node.kind else {
            return None;
        };
        if node.som_path()?.as_str() != path {
            return None;
        }
        let (page, bounds) = flat.locate(node)?;
        Some((page, bounds.y.to_string().parse().unwrap(), value.clone()))
    })
}

fn texts(flat: &Flattened) -> Vec<String> {
    flat.iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Text { content, .. } => Some(content.trim().to_string()),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// A row whose field is set by its own `initialize` script from its own
/// instance index, and whose label embeds that field's value.
const ROW: &str = r##"<subform name="Row" layout="tb" w="540pt">
<occur min="1" max="-1" initial="3"/>
<field name="F" id="fieldF" w="200pt" h="20pt"><ui><textEdit/></ui>
<event activity="initialize"><script contentType="application/x-javascript">this.rawValue = "v" + this.parent.instanceIndex;</script></event></field>
<draw name="Label" w="200pt" h="20pt"><value><exData contentType="text/html"><body xmlns="http://www.w3.org/1999/xhtml"><p>Row <span xfa:embed="#fieldF" xmlns:xfa="http://www.xfa.org/schema/xfa-data/1.0/"/></p></body></exData></value></draw>
</subform>"##;

#[test]
fn initial_instances_are_laid_out_one_below_the_other_with_their_own_paths() {
    let prepared = prepare_default(&template(ROW)).expect("prepare");
    let flat = &prepared.flattened;

    let rows: Vec<_> = [
        "Root.Body.Row.F",
        "Root.Body.Row[1].F",
        "Root.Body.Row[2].F",
    ]
    .iter()
    .map(|p| field_at(flat, p).unwrap_or_else(|| panic!("{p} is not laid out")))
    .collect();
    assert!(
        field_at(flat, "Root.Body.Row[3].F").is_none(),
        "initial is 3"
    );

    // A flowed (tb) parent stacks its instances like any other siblings.
    assert!(rows[0].1 < rows[1].1 && rows[1].1 < rows[2].1, "{rows:?}");
    // Each instance's own initialize script ran with its own instanceIndex.
    let values: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    assert_eq!(values, ["v0", "v1", "v2"]);
}

#[test]
fn an_embedded_label_shows_its_own_instances_value() {
    let prepared = prepare_default(&template(ROW)).expect("prepare");
    // The embedded value per label; the rich-text spacing around it is the
    // existing text layout's business, not what this test is about.
    let embedded: Vec<String> = texts(&prepared.flattened)
        .into_iter()
        .filter_map(|t| t.strip_prefix("Row").map(|v| v.trim().to_string()))
        .collect();
    assert_eq!(embedded, ["v0", "v1", "v2"]);
}

#[test]
fn an_initial_of_zero_draws_no_instance() {
    let body = r#"<subform name="Row" layout="tb"><occur min="0" max="-1" initial="0"/>
<field name="F" w="200pt" h="20pt"><ui><textEdit/></ui></field></subform>
<field name="After" w="200pt" h="20pt"><ui><textEdit/></ui></field>"#;
    let prepared = prepare_default(&template(body)).expect("prepare");
    assert!(field_at(&prepared.flattened, "Root.Body.Row.F").is_none());
    assert!(field_at(&prepared.flattened, "Root.Body.After").is_some());
}

#[test]
fn the_interactive_form_starts_from_the_same_form_dom_as_the_default_render() {
    let nodes = template(ROW);
    let mut form = XfaForm::new(nodes.clone()).expect("form");
    let values = form.current_field_values();
    for (i, path) in [
        "Root.Body.Row.F",
        "Root.Body.Row[1].F",
        "Root.Body.Row[2].F",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            values.get(*path).map(String::as_str),
            Some(format!("v{i}").as_str()),
            "{path}"
        );
        assert!(
            field_at(form.flattened(), path).is_some(),
            "{path} laid out"
        );
    }
}

// ---------------------------------------------------------------------------
// Adding, removing and moving instances at runtime (XFA 3.3 §9, §10)
// ---------------------------------------------------------------------------

fn button(name: &str, script: &str) -> String {
    format!(
        r#"<field name="{name}" w="40pt" h="20pt"><ui><button/></ui><event activity="click"><script contentType="application/x-javascript">{script}</script></event></field>"#
    )
}

/// `Body` holding a `Row` (with field `F`) under the given occur, plus `extra`.
fn rows_form(occur: &str, row_extra: &str, extra: &str) -> XfaForm {
    XfaForm::new(template(&format!(
        r#"<subform name="Row" layout="tb" w="540pt"><occur {occur}/><field name="F" w="200pt" h="20pt"><ui><textEdit/></ui></field>{row_extra}</subform>{extra}"#
    )))
    .expect("form")
}

fn value(form: &mut XfaForm, path: &str) -> Option<String> {
    form.current_field_values().get(path).cloned()
}

fn set(form: &mut XfaForm, path: &str, v: &str) {
    form.interact(path, v).expect("set");
}

fn row_values(form: &mut XfaForm, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let path = if i == 0 {
                "Root.Body.Row.F".to_string()
            } else {
                format!("Root.Body.Row[{i}].F")
            };
            value(form, &path).unwrap_or_else(|| panic!("{path} has no value"))
        })
        .collect()
}

#[test]
fn clicking_an_add_button_lays_out_a_new_instance_below_the_last() {
    let mut form = rows_form(r#"max="-1""#, "", &button("Add", "_Row.addInstance();"));
    let result = form.click("Root.Body.Add").expect("click");
    assert!(result.instances_changed);
    assert_eq!(result.instance_limit_hit, None);

    form.refresh().expect("refresh");
    let first = field_at(form.flattened(), "Root.Body.Row.F").expect("first row");
    let second = field_at(form.flattened(), "Root.Body.Row[1].F").expect("second row");
    assert!(second.1 > first.1, "{first:?} {second:?}");
}

#[test]
fn a_value_written_into_a_new_instance_in_the_same_script_is_kept() {
    let mut form = rows_form(
        r#"max="-1""#,
        "",
        &button("Add", "var r = _Row.addInstance(); r.F.rawValue = 'typed';"),
    );
    form.click("Root.Body.Add").expect("click");
    assert_eq!(
        value(&mut form, "Root.Body.Row[1].F").as_deref(),
        Some("typed")
    );
    form.refresh().expect("refresh");
    assert_eq!(
        field_at(form.flattened(), "Root.Body.Row[1].F")
            .map(|f| f.2)
            .as_deref(),
        Some("typed")
    );
}

#[test]
fn removing_an_instance_shifts_the_later_ones_down_with_their_values() {
    let mut form = rows_form(
        r#"max="-1" initial="3""#,
        "",
        &button("Remove", "_Row.removeInstance(0);"),
    );
    set(&mut form, "Root.Body.Row.F", "a");
    set(&mut form, "Root.Body.Row[1].F", "b");
    set(&mut form, "Root.Body.Row[2].F", "c");

    let result = form.click("Root.Body.Remove").expect("click");
    assert!(result.instances_changed);
    assert_eq!(row_values(&mut form, 2), ["b", "c"]);
    assert_eq!(value(&mut form, "Root.Body.Row[2].F"), None);
    form.refresh().expect("refresh");
    assert!(field_at(form.flattened(), "Root.Body.Row[2].F").is_none());
}

#[test]
fn moving_and_inserting_reorder_instances_with_their_values() {
    let mut form = rows_form(
        r#"max="-1" initial="3""#,
        "",
        &format!(
            "{}{}",
            button("Move", "_Row.moveInstance(0, 2);"),
            button("Insert", "_Row.insertInstance(1).F.rawValue = 'new';")
        ),
    );
    set(&mut form, "Root.Body.Row.F", "a");
    set(&mut form, "Root.Body.Row[1].F", "b");
    set(&mut form, "Root.Body.Row[2].F", "c");

    form.click("Root.Body.Move").expect("move");
    assert_eq!(row_values(&mut form, 3), ["b", "c", "a"]);
    form.click("Root.Body.Insert").expect("insert");
    assert_eq!(row_values(&mut form, 4), ["b", "new", "c", "a"]);
}

#[test]
fn a_button_inside_an_instance_acts_on_its_own_instance_only() {
    let mut form = rows_form(
        r#"max="-1" initial="3""#,
        &button(
            "Hit",
            "this.parent.F.rawValue = 'hit ' + this.parent.index;",
        ),
        "",
    );
    form.click("Root.Body.Row[2].Hit").expect("click");
    assert_eq!(
        value(&mut form, "Root.Body.Row[2].F").as_deref(),
        Some("hit 2")
    );
    assert_eq!(value(&mut form, "Root.Body.Row.F").as_deref(), Some(""));
    assert_eq!(value(&mut form, "Root.Body.Row[1].F").as_deref(), Some(""));
}

#[test]
fn a_remove_button_inside_an_instance_removes_that_instance() {
    let mut form = rows_form(
        r#"max="-1" initial="3""#,
        &button(
            "Remove",
            "this.parent.instanceManager.removeInstance(this.parent.index);",
        ),
        "",
    );
    set(&mut form, "Root.Body.Row.F", "a");
    set(&mut form, "Root.Body.Row[1].F", "b");
    set(&mut form, "Root.Body.Row[2].F", "c");
    form.click("Root.Body.Row[1].Remove").expect("click");
    assert_eq!(row_values(&mut form, 2), ["a", "c"]);
}

#[test]
fn a_new_instance_is_initialized_before_index_change_fires() {
    let row_events = r#"<event activity="initialize"><script contentType="application/x-javascript">Log.rawValue = String(Log.rawValue) + 'i' + this.instanceIndex;</script></event>
<event activity="indexChange"><script contentType="application/x-javascript">Log.rawValue = String(Log.rawValue) + 'x' + this.instanceIndex;</script></event>"#;
    let mut form = rows_form(
        r#"max="-1""#,
        row_events,
        &format!(
            r#"<field name="Log" w="200pt" h="20pt"><ui><textEdit/></ui></field>{}"#,
            button("Insert", "_Row.insertInstance(0);")
        ),
    );
    set(&mut form, "Root.Body.Log", "");
    form.click("Root.Body.Insert").expect("click");
    // The new instance 0 is initialized, then it (new) and the old instance,
    // now at 1, both receive indexChange (§10 "Instance Manager Events").
    assert_eq!(value(&mut form, "Root.Body.Log").as_deref(), Some("i0x0x1"));
}

#[test]
fn adding_to_a_section_with_no_instance_works_from_zero() {
    let mut form = rows_form(
        r#"min="0" max="-1" initial="0""#,
        "",
        &button("Add", "_Row.addInstance(); _Row.addInstance();"),
    );
    assert_eq!(value(&mut form, "Root.Body.Row.F"), None);
    form.click("Root.Body.Add").expect("click");
    form.refresh().expect("refresh");
    assert!(field_at(form.flattened(), "Root.Body.Row.F").is_some());
    assert!(field_at(form.flattened(), "Root.Body.Row[1].F").is_some());
}

#[test]
fn a_click_that_hits_the_occurrence_limit_changes_nothing_and_says_why() {
    let mut form = rows_form(
        r#"max="2" initial="2""#,
        "",
        &button("Add", "_Row.addInstance();"),
    );
    let result = form.click("Root.Body.Add").expect("click");
    assert!(!result.instances_changed);
    let why = result.instance_limit_hit.expect("the limit is reported");
    assert!(why.contains("max=\"2\""), "{why}");
    assert_eq!(value(&mut form, "Root.Body.Row[2].F"), None);
}

/// The corpus pattern (`soPlusMinus.insertNode` + `applyIndex`, e.g.
/// AACC_019_DE): the add button sits in `STP_PlusMinus` inside the section,
/// reaches the section with `this.parent.parent`, checks `count`/`max`,
/// adds, and renumbers every section through `resolveNodes(...[*]...)`.
#[test]
fn the_corpus_plus_minus_pattern_adds_a_section_and_renumbers_every_one() {
    let script_object = r#"<variables><script name="soPlusMinus" contentType="application/x-javascript">
function insertNode(first) {
    if (first.instanceManager.max > 0 &amp;&amp; first.instanceManager.count == first.instanceManager.max) return;
    first.instanceManager.addInstance();
}
function applyIndex(list) {
    for (var i = 0; i &lt; list.length; i++) { list.item(i).rawValue = i + 1; }
}
</script></variables>"#;
    let section = format!(
        r#"<subform name="Section_DYN" layout="tb" w="540pt"><occur max="5"/>
<subform name="STP_Client" layout="tb"><field name="ffIndex" w="40pt" h="20pt"><ui><textEdit/></ui></field></subform>
<subform name="STP_PlusMinus" layout="tb">{}</subform></subform>"#,
        button(
            "Button_Add",
            r#"soPlusMinus.insertNode(this.parent.parent); soPlusMinus.applyIndex(this.resolveNodes("Section_DYN[*].STP_Client.ffIndex"));"#
        )
    );
    let mut form = XfaForm::new(template(&format!("{script_object}{section}"))).expect("form");

    let result = form
        .click("Root.Body.Section_DYN.STP_PlusMinus.Button_Add")
        .expect("click");
    assert!(result.instances_changed);
    assert_eq!(
        value(&mut form, "Root.Body.Section_DYN.STP_Client.ffIndex").as_deref(),
        Some("1")
    );
    assert_eq!(
        value(&mut form, "Root.Body.Section_DYN[1].STP_Client.ffIndex").as_deref(),
        Some("2")
    );

    // The new section's own add button works the same way.
    form.click("Root.Body.Section_DYN[1].STP_PlusMinus.Button_Add")
        .expect("click in the new section");
    assert_eq!(
        value(&mut form, "Root.Body.Section_DYN[2].STP_Client.ffIndex").as_deref(),
        Some("3")
    );
}

// ---------------------------------------------------------------------------
// Instance changes made while the form loads
// ---------------------------------------------------------------------------

/// The BAUL_033_IT pattern: a container's `initialize` script adds rows in a
/// loop through `this.DYN_Row.instanceManager.addInstance()`, writing each new
/// row as it goes.
const LOAD_LOOP: &str = r#"<subform name="Nationalities" layout="tb" w="540pt">
<event activity="initialize"><script contentType="application/x-javascript">
for (var i = 0; i &lt; 2; i++) { var r = this.DYN_Row.instanceManager.addInstance(); r.Country.rawValue = 'C' + (i + 1); }
this.DYN_Row.Country.rawValue = 'C0';
</script></event>
<subform name="DYN_Row" layout="tb" w="540pt"><occur max="-1"/>
<field name="Country" w="200pt" h="20pt"><ui><textEdit/></ui></field></subform></subform>"#;

#[test]
fn instances_added_while_the_form_loads_are_rendered_with_their_values() {
    let prepared = prepare_default(&template(LOAD_LOOP)).expect("prepare");
    let flat = &prepared.flattened;
    let countries: Vec<String> = (0..3)
        .map(|i| {
            let path = if i == 0 {
                "Root.Body.Nationalities.DYN_Row.Country".to_string()
            } else {
                format!("Root.Body.Nationalities.DYN_Row[{i}].Country")
            };
            field_at(flat, &path)
                .unwrap_or_else(|| panic!("{path} not laid out"))
                .2
        })
        .collect();
    assert_eq!(countries, ["C0", "C1", "C2"]);
    assert!(field_at(flat, "Root.Body.Nationalities.DYN_Row[3].Country").is_none());
}

#[test]
fn the_interactive_form_sees_the_instances_the_load_pass_added() {
    let mut form = XfaForm::new(template(LOAD_LOOP)).expect("form");
    assert_eq!(
        value(&mut form, "Root.Body.Nationalities.DYN_Row[2].Country").as_deref(),
        Some("C2")
    );
    assert!(
        field_at(
            form.flattened(),
            "Root.Body.Nationalities.DYN_Row[2].Country"
        )
        .is_some()
    );
}

#[test]
fn an_instance_that_hides_itself_while_loading_hides_only_itself() {
    let body = r#"<subform name="Row" layout="tb" w="540pt"><occur max="-1" initial="3"/>
<event activity="initialize"><script contentType="application/x-javascript">if (this.instanceIndex == 1) this.presence = "hidden";</script></event>
<field name="F" w="200pt" h="20pt"><ui><textEdit/></ui></field></subform>"#;
    let prepared = prepare_default(&template(body)).expect("prepare");
    let flat = &prepared.flattened;
    assert!(field_at(flat, "Root.Body.Row.F").is_some());
    assert!(
        field_at(flat, "Root.Body.Row[1].F").is_none(),
        "instance 1 hid itself"
    );
    assert!(field_at(flat, "Root.Body.Row[2].F").is_some());
}
