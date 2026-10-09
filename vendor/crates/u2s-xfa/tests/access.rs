//! Field access (XFA 3.3 §17 `access`; §2 "Access Restrictions"): what a
//! person filling the form may do with a field, set by the template, by an
//! enclosing container, or by the form's own scripts, and enforced on every
//! interaction that simulates a person.

use u2s_xfa::flattened::FieldAccess;
use u2s_xfa::states::{ControlKind, SelectionSpec, StateSpec, Step, controls, materialize};
use u2s_xfa::xfa::script_executor::ScriptExecutor;
use u2s_xfa::xfa::scripting::{SomPath, XfaForm};
use u2s_xfa::{FlattenedNodeKind, XfaNode, prepare_default};

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

fn text_field(name: &str, attrs: &str, inner: &str) -> String {
    format!(r#"<field name="{name}" {attrs} w="200pt" h="20pt"><ui><textEdit/></ui>{inner}</field>"#)
}

fn script(activity: &str, source: &str) -> String {
    format!(
        r#"<event activity="{activity}"><script contentType="application/x-javascript">{source}</script></event>"#
    )
}

/// The AAGS pattern: selecting `RB_1` prefills `Sheet` and locks it,
/// selecting `RB_2` clears and unlocks it.
fn sheet_form() -> Vec<XfaNode> {
    template(&format!(
        r#"<exclGroup name="RB_Sheet" layout="tb" w="400pt">
<field name="RB_1" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>1</text></items></field>
<field name="RB_2" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>2</text></items></field>
{}</exclGroup>{}"#,
        script(
            "change",
            r#"if (this.rawValue == "1") { Body.Sheet.rawValue = "1"; Body.Sheet.access = "protected"; } else { Body.Sheet.rawValue = ""; Body.Sheet.access = "open"; }"#
        ),
        text_field("Sheet", "", "")
    ))
}

fn access_of(form: &XfaForm, path: &str) -> (FieldAccess, Option<String>) {
    let effective = form
        .effective_access(path)
        .unwrap_or_else(|| panic!("{path} has no node"));
    (
        effective.access,
        effective.inherited_from.map(|p| p.as_str().to_string()),
    )
}

#[test]
fn levels_combine_by_the_spec_precedence() {
    use FieldAccess::*;
    // nonInteractive > protected > readOnly > open, whichever side asks.
    for (a, b, strictest) in [
        (Open, ReadOnly, ReadOnly),
        (ReadOnly, Protected, Protected),
        (Protected, NonInteractive, NonInteractive),
        (Open, NonInteractive, NonInteractive),
        (ReadOnly, ReadOnly, ReadOnly),
    ] {
        assert_eq!(a.most_restrictive(b), strictest, "{a:?} with {b:?}");
        assert_eq!(b.most_restrictive(a), strictest, "{b:?} with {a:?}");
    }
}

#[test]
fn levels_travel_under_the_spec_keywords() {
    for access in [
        FieldAccess::Open,
        FieldAccess::NonInteractive,
        FieldAccess::Protected,
        FieldAccess::ReadOnly,
    ] {
        assert_eq!(
            serde_json::to_value(access).unwrap(),
            serde_json::Value::from(access.as_str())
        );
        assert_eq!(FieldAccess::parse_strict(access.as_str()), Some(access));
    }
    assert_eq!(FieldAccess::parse_strict("locked"), None);
    assert_eq!(FieldAccess::parse_strict("readonly"), None, "keywords are case-sensitive");
}

#[test]
fn a_change_script_prefills_and_locks_another_field_and_can_unlock_it() {
    let mut form = XfaForm::new(sheet_form()).expect("form");
    let sheet = "Root.Body.Sheet";
    assert_eq!(access_of(&form, sheet), (FieldAccess::Open, None));

    let result = form
        .interact("Root.Body.RB_Sheet.RB_1", "1")
        .expect("select RB_1");
    assert!(result.access_changed, "the script's access write must be reported");
    form.refresh().expect("refresh");
    assert_eq!(access_of(&form, sheet), (FieldAccess::Protected, None));
    assert_eq!(
        form.canonical_field_values().get(&SomPath::new(sheet)).map(String::as_str),
        Some("1")
    );

    let refusal = form
        .interact(sheet, "7")
        .expect_err("a protected field cannot be set");
    assert!(refusal.contains("protected"), "{refusal}");
    assert!(refusal.contains("its own access"), "{refusal}");

    form.interact("Root.Body.RB_Sheet.RB_2", "2")
        .expect("select RB_2");
    form.refresh().expect("refresh");
    assert_eq!(access_of(&form, sheet), (FieldAccess::Open, None));
    form.interact(sheet, "7").expect("open again, so it can be set");
}

#[test]
fn an_initialize_script_locks_its_field_at_open_on_both_paths() {
    let nodes = template(&text_field(
        "InitLocked",
        "",
        &script("initialize", r#"this.access = "protected";"#),
    ));

    // The live form, which sessions use.
    let form = XfaForm::new(nodes.clone()).expect("form");
    assert_eq!(
        access_of(&form, "Root.Body.InitLocked"),
        (FieldAccess::Protected, None)
    );

    // The stateless render, which draws a field by its access.
    let prepared = prepare_default(&nodes).expect("prepare");
    let drawn = prepared
        .flattened
        .iter_nodes()
        .find(|n| matches!(&n.kind, FlattenedNodeKind::Field { name, .. } if name == "InitLocked"))
        .expect("drawn");
    assert_eq!(
        drawn.field_behavior().map(|(access, ..)| access),
        Some(FieldAccess::Protected)
    );
}

#[test]
fn a_value_that_is_not_a_keyword_changes_nothing() {
    let form = XfaForm::new(template(&text_field(
        "Bad",
        r#"access="readOnly""#,
        &script("initialize", r#"this.access = "locked";"#),
    )))
    .expect("form");
    assert_eq!(access_of(&form, "Root.Body.Bad"), (FieldAccess::ReadOnly, None));
}

#[test]
fn a_calculate_script_can_lock_a_field_from_another_ones_value() {
    let mut form = XfaForm::new(template(&format!(
        r#"<field name="Toggle" w="200pt" h="20pt"><ui><checkButton/></ui><items><text>1</text><text>0</text></items></field>{}"#,
        text_field(
            "Dependent",
            "",
            r#"<calculate><script contentType="application/x-javascript">this.access = (Body.Toggle.rawValue == "1") ? "readOnly" : "open";</script></calculate>"#
        )
    )))
    .expect("form");
    assert_eq!(access_of(&form, "Root.Body.Dependent").0, FieldAccess::Open);

    form.interact("Root.Body.Toggle", "1").expect("check");
    form.refresh().expect("refresh");
    assert_eq!(access_of(&form, "Root.Body.Dependent").0, FieldAccess::ReadOnly);
}

#[test]
fn a_container_lock_is_inherited_and_only_ever_tightened() {
    let form = XfaForm::new(template(&format!(
        r#"<subform name="Locked" access="protected" layout="tb" w="540pt">{}{}</subform>
<subform name="Viewable" access="readOnly" layout="tb" w="540pt">{}</subform>
<exclGroup name="Group" access="readOnly" layout="tb" w="400pt">
<field name="RB_1" w="200pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>1</text></items></field>
</exclGroup>"#,
        text_field("Inner", r#"access="open""#, ""),
        text_field("Stricter", r#"access="nonInteractive""#, ""),
        text_field("OwnProtected", r#"access="protected""#, ""),
    )))
    .expect("form");

    // An open field cannot loosen what its subform imposes.
    assert_eq!(
        access_of(&form, "Root.Body.Locked.Inner"),
        (FieldAccess::Protected, Some("Root.Body.Locked".to_string()))
    );
    // A field may assert something stricter than it inherits.
    assert_eq!(
        access_of(&form, "Root.Body.Locked.Stricter"),
        (FieldAccess::NonInteractive, None)
    );
    assert_eq!(
        access_of(&form, "Root.Body.Viewable.OwnProtected"),
        (FieldAccess::Protected, None)
    );
    // An exclusion group's access governs its buttons.
    assert_eq!(
        access_of(&form, "Root.Body.Group.RB_1"),
        (FieldAccess::ReadOnly, Some("Root.Body.Group".to_string()))
    );
}

#[test]
fn every_field_is_listed_with_its_kind_and_effective_access() {
    let listed = controls(&template(&format!(
        r#"<subform name="Locked" access="protected" layout="tb" w="540pt">{}</subform>{}{}
<field name="Amount" w="200pt" h="20pt"><ui><numericEdit/></ui></field>
<field name="When" w="200pt" h="20pt"><ui><dateTimeEdit picker="date"/></ui></field>
<field name="dotted.Name" w="200pt" h="20pt"><ui><textEdit/></ui></field>"#,
        text_field("Inner", "", ""),
        text_field("Fixed", r#"access="readOnly""#, ""),
        text_field("Free", "", ""),
    )))
    .expect("controls");

    let row = |path: &str| {
        listed
            .controls
            .iter()
            .find(|c| c.field == path)
            .unwrap_or_else(|| panic!("{path} not listed"))
    };
    let inner = row("Root.Body.Locked.Inner");
    assert_eq!((inner.kind, inner.access), (ControlKind::Text, FieldAccess::Protected));
    assert_eq!(inner.access_from.as_deref(), Some("Root.Body.Locked"));
    assert_eq!(row("Root.Body.Fixed").access, FieldAccess::ReadOnly);
    assert_eq!(row("Root.Body.Free").access, FieldAccess::Open);
    assert!(row("Root.Body.Free").options.is_empty(), "a text field takes any value");
    assert_eq!(row("Root.Body.Amount").kind, ControlKind::Numeric);
    assert_eq!(row("Root.Body.When").kind, ControlKind::Date);
    // A dotted name cannot be addressed by any SOM path, so it is not handed out.
    assert!(
        listed.controls.iter().all(|c| !c.field.contains("dotted")),
        "{:?}",
        listed.controls.iter().map(|c| &c.field).collect::<Vec<_>>()
    );
}

#[test]
fn a_state_that_sets_a_locked_field_is_refused() {
    let nodes = sheet_form();
    let spec = StateSpec {
        steps: vec![
            Step::Set(SelectionSpec {
                field: "Root.Body.RB_Sheet.RB_1".into(),
                value: "1".into(),
            }),
            Step::Set(SelectionSpec {
                field: "Root.Body.Sheet".into(),
                value: "7".into(),
            }),
        ],
    };
    // `canonical` sorts consecutive sets by field, and RB_Sheet sorts before
    // Sheet, so the radio is selected first and locks the field.
    let err = match materialize(&nodes, &spec) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("setting a field the form just locked must fail"),
    };
    assert!(err.contains("protected"), "{err}");
}

#[test]
fn a_locked_button_cannot_be_pressed() {
    let mut form = XfaForm::new(template(&format!(
        r#"<field name="Go" access="protected" w="80pt" h="22pt"><ui><button/></ui>{}</field>{}"#,
        script("click", r#"Body.Out.rawValue = "pressed";"#),
        text_field("Out", "", "")
    )))
    .expect("form");
    let err = form.click("Root.Body.Go").expect_err("protected");
    assert!(err.contains("protected"), "{err}");
}

#[test]
fn an_access_change_off_its_path_lands_only_on_an_unambiguous_name() {
    let unique = template(&text_field("Once", "", ""));
    let mut nodes = unique.clone();
    // A master-page script path skips its pageSet and pageArea, so the walk
    // misses and the leaf name decides.
    ScriptExecutor::apply_access_changes(
        &mut nodes,
        &[(SomPath::new("Root.Elsewhere.Once"), FieldAccess::ReadOnly)],
    );
    let form = XfaForm::new(nodes).expect("form");
    assert_eq!(access_of(&form, "Root.Body.Once").0, FieldAccess::ReadOnly);

    let mut twice = template(&format!(
        r#"<subform name="A" layout="tb" w="540pt">{}</subform><subform name="B" layout="tb" w="540pt">{}</subform>"#,
        text_field("Same", "", ""),
        text_field("Same", "", ""),
    ));
    ScriptExecutor::apply_access_changes(
        &mut twice,
        &[(SomPath::new("Root.Elsewhere.Same"), FieldAccess::ReadOnly)],
    );
    let form = XfaForm::new(twice).expect("form");
    assert_eq!(access_of(&form, "Root.Body.A.Same").0, FieldAccess::Open);
    assert_eq!(access_of(&form, "Root.Body.B.Same").0, FieldAccess::Open);
}

#[test]
fn a_master_page_access_change_lands_on_every_page_area_copy() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="Root" layout="tb"><pageSet>
<pageArea name="P1" w="612pt" h="792pt"><contentArea x="36pt" y="36pt" w="540pt" h="720pt"/>
<field name="Watermark" x="0pt" y="0pt" w="100pt" h="20pt"><ui><textEdit/></ui></field></pageArea>
<pageArea name="P2" w="612pt" h="792pt"><contentArea x="36pt" y="36pt" w="540pt" h="720pt"/>
<field name="Watermark" x="0pt" y="0pt" w="100pt" h="20pt"><ui><textEdit/></ui></field></pageArea>
</pageSet><subform name="Body" layout="tb" w="540pt"/></subform></template></xdp:xdp>"#;
    let mut nodes = XfaNode::parse(xml.as_bytes()).expect("parse");
    // The script path a master-page node's events run under skips the
    // pageSet and pageArea.
    ScriptExecutor::apply_access_changes(
        &mut nodes,
        &[(SomPath::new("Root.Watermark"), FieldAccess::NonInteractive)],
    );
    let listed = controls(&nodes).expect("controls");
    let watermarks: Vec<_> = listed
        .controls
        .iter()
        .filter(|c| c.field.ends_with("Watermark"))
        .map(|c| c.access)
        .collect();
    assert_eq!(watermarks, [FieldAccess::NonInteractive; 2]);
}
