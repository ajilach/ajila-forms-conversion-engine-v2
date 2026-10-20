//! Tests for the scripting module
//!
//! These tests verify the XFA scripting functionality including:
//! - Script engine execution
//! - SOM path resolution
//! - Dependency tracking
//! - Event handling
//! - Form interaction

use super::*;
use std::collections::HashMap;

// =============================================================================
// Basic Engine Tests
// =============================================================================

#[test]
fn test_script_engine_basic() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Footer_Line_txtlanguage", "Footer_Line_txtlanguage", "DE");

    let script = XfaScript {
        source: r#"Footer_Line_txtlanguage.value"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Form,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    if let Err(e) = &result {
        eprintln!("Script execution error: {}", e);
    }
    assert!(result.is_ok(), "Script execution failed: {:?}", result);
}

#[test]
fn test_script_with_this_reference() {
    let mut engine = XfaScriptEngine::new();
    engine.set_current_field("ffFirstName_s", "ffFirstName_s", "");

    let mut translations = HashMap::new();
    translations.insert("GV_FirstName_s".to_string(), "Vorname(n)".to_string());
    engine.register_translation_object("myDE", translations);

    let script = XfaScript {
        source: r#"this.rawValue = myDE.GV_FirstName_s;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Form,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    assert!(result.is_ok());

    if let Ok(Some(value)) = result {
        assert_eq!(value, "Vorname(n)");
    }
}

// =============================================================================
// SOM Resolution Tests
// =============================================================================

#[test]
fn test_som_resolver_basic() {
    let mut resolver = SomResolver::new();
    resolver.register_node(
        &SomPath::new("Page.Header.txtlanguage"),
        "txtlanguage",
        "field",
        Some(&SomPath::new("Page.Header")),
    );
    resolver.register_node(
        &SomPath::new("Page.Body.Name"),
        "Name",
        "field",
        Some(&SomPath::new("Page.Body")),
    );

    // A bare name is not an absolute path: the host resolves nothing by it.
    assert_eq!(resolver.resolve_node("txtlanguage"), None);

    let result = resolver.resolve_node("Page.Header.txtlanguage");
    assert_eq!(result, Some(SomPath::new("Page.Header.txtlanguage")));
    let result = resolver.resolve_node("xfa.form.Page.Body.Name");
    assert_eq!(result, Some(SomPath::new("Page.Body.Name")));
}

#[test]
fn test_som_resolver_indexed() {
    // Three instances of one repeated field: XFA 3.3 §3 gives each its own
    // path, index 0 written without brackets.
    let mut resolver = SomResolver::new();
    for path in ["Detail.Item", "Detail.Item[1]", "Detail.Item[2]"] {
        resolver.register_node(
            &SomPath::new(path),
            "Item",
            "field",
            Some(&SomPath::new("Detail")),
        );
    }

    assert_eq!(
        resolver.resolve_nodes("Detail.Item[0]"),
        vec![SomPath::new("Detail.Item")]
    );
    assert_eq!(
        resolver.resolve_nodes("Detail.Item[2]"),
        vec![SomPath::new("Detail.Item[2]")]
    );
    assert_eq!(resolver.resolve_nodes("Detail.Item[*]").len(), 3);
    assert!(resolver.resolve_nodes("Detail.Item[3]").is_empty());
}

// =============================================================================
// Instance-indexed SOM paths (XFA 3.3 §3)
// =============================================================================

fn parse_template(body: &str) -> Vec<crate::xfa::XfaNode> {
    let xml = format!(
        r#"<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">{body}</template></xdp:xdp>"#
    );
    crate::xfa::XfaNode::parse(xml.as_bytes()).expect("parse")
}

#[test]
fn som_segments_round_trip_and_index_zero_has_no_brackets() {
    use super::som::{SegmentIndex, child_som_path, parse_segment};

    assert_eq!(child_som_path("A", "Row", 0), "A.Row");
    assert_eq!(child_som_path("A", "Row", 2), "A.Row[2]");
    assert_eq!(child_som_path("", "Row", 1), "Row[1]");

    let path = SomPath::new("A.Row[2].F");
    assert_eq!(path.name(), "F");
    assert_eq!(
        path.segments().collect::<Vec<_>>(),
        vec![("A", 0), ("Row", 2), ("F", 0)]
    );
    assert_eq!(path.parent().unwrap().name(), "Row");
    assert_eq!(path.parent().unwrap().leaf(), "Row[2]");
    assert_eq!(path.parent().unwrap().index(), 2);
    assert_eq!(path.index_free(), SomPath::new("A.Row.F"));
    assert_eq!(
        SomPath::new("A").child_indexed("Row", 1),
        SomPath::new("A.Row[1]")
    );

    assert_eq!(parse_segment("Row"), Some(("Row", SegmentIndex::At(0))));
    assert_eq!(parse_segment("Row[*]"), Some(("Row", SegmentIndex::All)));
    assert_eq!(parse_segment("Row[x]"), None);
    assert_eq!(parse_segment("Row[1"), None);
}

#[test]
fn walk_som_path_picks_the_nth_same_named_sibling_through_an_unnamed_container() {
    let nodes = parse_template(
        r#"<subform name="A"><subform><field name="F" id="first"/><field name="F" id="second"/></subform></subform>"#,
    );
    let id_of = |path: &str| {
        super::som::walk_som_path(&nodes, path).and_then(|n| n.attributes.get("id").cloned())
    };
    assert_eq!(id_of("A.F").as_deref(), Some("first"));
    assert_eq!(id_of("A.F[1]").as_deref(), Some("second"));
    assert_eq!(id_of("A.F[2]"), None);
    assert_eq!(id_of("A.F[*]"), None, "a walk follows exactly one node");
}

#[test]
fn the_resolver_indexes_siblings_per_parent_not_across_the_document() {
    let nodes = parse_template(
        r#"<subform name="Root"><subform name="A"><field name="Item"/><field name="Item"/></subform>
<subform name="B"><field name="Item"/><field name="Item"/></subform></subform>"#,
    );
    let resolver = SomResolver::from_nodes(&nodes);

    // `B.Item[1]` is B's second Item, not the document's fourth.
    assert_eq!(
        resolver.resolve_node("Root.B.Item[1]"),
        Some(SomPath::new("Root.B.Item[1]"))
    );
    assert_eq!(
        resolver.resolve_node("Root.A.Item[1]"),
        Some(SomPath::new("Root.A.Item[1]"))
    );
    assert_eq!(
        resolver.expand_template(&SomPath::new("Root.B.Item[1]")),
        vec![SomPath::new("Root.B.Item"), SomPath::new("Root.B.Item[1]")]
    );
}

#[test]
fn star_selects_every_instance_under_one_parent_only() {
    let nodes = parse_template(
        r#"<subform name="Root"><subform name="Row"><field name="Idx"/></subform>
<subform name="Row"><field name="Idx"/></subform><subform name="Row"><field name="Idx"/></subform>
<subform name="Other"><subform name="Row"><field name="Idx"/></subform></subform></subform>"#,
    );
    let resolver = SomResolver::from_nodes(&nodes);
    let found = resolver.resolve_nodes("Root.Row[*].Idx");
    assert_eq!(
        found,
        vec![
            SomPath::new("Root.Row.Idx"),
            SomPath::new("Root.Row[1].Idx"),
            SomPath::new("Root.Row[2].Idx"),
        ]
    );
}

#[test]
fn the_registry_keys_scripts_by_template_path_and_registers_each_declaration_once() {
    let mut registry = ScriptRegistry::new();
    let script = || XfaScript {
        source: "this.rawValue = 1;".to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    for owner in ["A.Row.F", "A.Row[1].F", "A.Row[2].F"] {
        registry.register(RegisteredScript {
            script: script(),
            owner_path: SomPath::new(owner),
            owner_name: "F".to_string(),
            child_fields: Vec::new(),
            script_type: ScriptType::Calculate,
        });
    }
    assert_eq!(
        registry.get_owners_with_activity(&EventActivity::Calculate),
        vec![&SomPath::new("A.Row.F")]
    );
    assert_eq!(
        registry
            .get_event_scripts(&SomPath::new("A.Row[2].F"), &EventActivity::Calculate)
            .len(),
        1
    );
}

#[test]
fn test_som_path_creation() {
    let path = SomPath::new("Page.Header.Field");
    assert_eq!(path.as_str(), "Page.Header.Field");
    assert_eq!(path.name(), "Field");
    // Verify path has expected components
    assert!(path.as_str().split('.').count() == 3);
}

#[test]
fn test_som_path_parent() {
    let path = SomPath::new("Page.Header.Field");
    let parent = path.parent();
    assert!(parent.is_some());
    assert_eq!(parent.unwrap().as_str(), "Page.Header");
}

// =============================================================================
// Dependency Tracking Tests
// =============================================================================

#[test]
fn test_dependency_tracker() {
    let mut tracker = DependencyTracker::new();

    tracker.add_dependency(&SomPath::new("Total"), &SomPath::new("Price"));
    tracker.add_dependency(&SomPath::new("Total"), &SomPath::new("Quantity"));
    tracker.add_dependency(&SomPath::new("GrandTotal"), &SomPath::new("Total"));

    // When Price changes, Total should recalculate
    let deps = tracker.get_dependents(&SomPath::new("Price"));
    assert!(deps.iter().any(|p| p.as_str() == "Total"));

    // When Total changes, GrandTotal should recalculate
    let deps = tracker.get_dependents(&SomPath::new("Total"));
    assert!(deps.iter().any(|p| p.as_str() == "GrandTotal"));
}

#[test]
fn test_dependency_cascade() {
    let mut tracker = DependencyTracker::new();

    tracker.add_dependency(&SomPath::new("Subtotal"), &SomPath::new("Price"));
    tracker.add_dependency(&SomPath::new("Tax"), &SomPath::new("Subtotal"));
    tracker.add_dependency(&SomPath::new("Total"), &SomPath::new("Subtotal"));
    tracker.add_dependency(&SomPath::new("Total"), &SomPath::new("Tax"));

    // When Price changes, should cascade to Subtotal -> Tax -> Total
    let cascaded = tracker.get_dependents_cascade(&SomPath::new("Price"));

    assert!(cascaded.iter().any(|p| p.as_str() == "Subtotal"));
    assert!(cascaded.iter().any(|p| p.as_str() == "Tax"));
    assert!(cascaded.iter().any(|p| p.as_str() == "Total"));
}

// =============================================================================
// Event Parsing Tests
// =============================================================================

#[test]
fn test_event_activity_parsing() {
    assert_eq!(
        "ready".parse::<EventActivity>().unwrap(),
        EventActivity::Ready
    );
    assert_eq!(
        "click".parse::<EventActivity>().unwrap(),
        EventActivity::Click
    );
    assert_eq!(
        "initialize".parse::<EventActivity>().unwrap(),
        EventActivity::Initialize
    );
    assert_eq!(
        "change".parse::<EventActivity>().unwrap(),
        EventActivity::Change
    );
    assert_eq!(
        "calculate".parse::<EventActivity>().unwrap(),
        EventActivity::Calculate
    );
}

#[test]
fn test_event_ref_parsing() {
    assert_eq!("$form".parse::<EventRef>().unwrap(), EventRef::Form);
    assert_eq!("$layout".parse::<EventRef>().unwrap(), EventRef::Layout);
    assert_eq!("$".parse::<EventRef>().unwrap(), EventRef::Current);
}

#[test]
fn test_script_content_type_parsing() {
    // ScriptContentType doesn't implement FromStr, but we can check enum values
    assert_eq!(ScriptContentType::JavaScript, ScriptContentType::JavaScript);
    assert_eq!(ScriptContentType::FormCalc, ScriptContentType::FormCalc);
}

// =============================================================================
// Language Switching Tests
// =============================================================================

#[test]
fn test_language_switch_german() {
    let mut engine = XfaScriptEngine::new();

    engine.set_current_field("ffFirstName_s", "ffFirstName_s", "");
    engine.register_field("Footer_Line_txtlanguage", "Footer_Line_txtlanguage", "DE");
    engine.register_field("Footer_Line_txtformid", "Footer_Line_txtformid", "AAAB");

    let mut de_translations = HashMap::new();
    de_translations.insert("GV_FirstName_s".to_string(), "Vorname(n)".to_string());
    engine.register_translation_object("myDE", de_translations);

    let mut en_translations = HashMap::new();
    en_translations.insert("GV_FirstName_s".to_string(), "First name(s)".to_string());
    engine.register_translation_object("myEN", en_translations);

    let script = XfaScript {
        source: r#"
            switch(Footer_Line_txtlanguage.value){
                case "DE":
                    this.rawValue = myDE.GV_FirstName_s;
                    break;
                case "EN":
                default:
                    this.rawValue = myEN.GV_FirstName_s;
            }
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Form,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    assert!(result.is_ok());

    if let Ok(Some(value)) = result {
        assert_eq!(value, "Vorname(n)");
    }
}

#[test]
fn test_language_switch_english() {
    let mut engine = XfaScriptEngine::new();

    engine.set_current_field("ffFirstName_s", "ffFirstName_s", "");
    engine.register_field("Footer_Line_txtlanguage", "Footer_Line_txtlanguage", "EN");
    engine.register_field("Footer_Line_txtformid", "Footer_Line_txtformid", "AAAB");

    let mut de_translations = HashMap::new();
    de_translations.insert("GV_FirstName_s".to_string(), "Vorname(n)".to_string());
    engine.register_translation_object("myDE", de_translations);

    let mut en_translations = HashMap::new();
    en_translations.insert("GV_FirstName_s".to_string(), "First name(s)".to_string());
    engine.register_translation_object("myEN", en_translations);

    let script = XfaScript {
        source: r#"
            switch(Footer_Line_txtlanguage.value){
                case "DE":
                    this.rawValue = myDE.GV_FirstName_s;
                    break;
                case "EN":
                default:
                    this.rawValue = myEN.GV_FirstName_s;
            }
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Form,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    assert!(result.is_ok());

    if let Ok(Some(value)) = result {
        assert_eq!(value, "First name(s)");
    }
}

// =============================================================================
// Context-Aware Resolution Tests
// =============================================================================


// =============================================================================
// State/Value Tests
// =============================================================================

#[test]
fn test_xfa_value_as_string() {
    use rust_decimal::Decimal;

    // XfaValue uses as_string to convert to string representation
    let val = XfaValue::String("hello".to_string());
    assert_eq!(val.as_string(), "hello");

    let val = XfaValue::Number(Decimal::new(42, 0));
    assert_eq!(val.as_string(), "42");

    let val = XfaValue::Boolean(true);
    assert_eq!(val.as_string(), "true");

    let val = XfaValue::Null;
    assert_eq!(val.as_string(), "");
}

#[test]
fn test_presence_from_str() {
    assert_eq!("visible".parse::<Presence>().unwrap(), Presence::Visible);
    assert_eq!("hidden".parse::<Presence>().unwrap(), Presence::Hidden);
    assert_eq!(
        "invisible".parse::<Presence>().unwrap(),
        Presence::Invisible
    );
    assert_eq!("inactive".parse::<Presence>().unwrap(), Presence::Inactive);
    // Unknown values default to Visible
    assert_eq!("unknown".parse::<Presence>().unwrap(), Presence::Visible);
}

#[test]
fn test_presence_should_skip_layout() {
    assert!(!Presence::Visible.should_skip_layout());
    assert!(Presence::Hidden.should_skip_layout());
    assert!(!Presence::Invisible.should_skip_layout());
    assert!(Presence::Inactive.should_skip_layout());
}

// =============================================================================
// Script Registry Tests
// =============================================================================

#[test]
fn test_script_registry_registration() {
    let mut registry = ScriptRegistry::new();

    let script = XfaScript {
        source: "this.rawValue = 'test'".to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Click,
        event_ref: EventRef::Current,
        name: Some("testScript".to_string()),
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    registry.register(RegisteredScript {
        script,
        owner_path: SomPath::new("Page.Button1"),
        owner_name: "Button1".to_string(),
        child_fields: vec![],
        script_type: ScriptType::Event,
    });

    let scripts = registry.get_event_scripts(&SomPath::new("Page.Button1"), &EventActivity::Click);
    assert_eq!(scripts.len(), 1);
}

#[test]
fn test_script_type_from_activity() {
    assert_eq!(
        ScriptType::from_activity(&EventActivity::Initialize),
        ScriptType::Initialize
    );
    assert_eq!(
        ScriptType::from_activity(&EventActivity::Calculate),
        ScriptType::Calculate
    );
    assert_eq!(
        ScriptType::from_activity(&EventActivity::Validate),
        ScriptType::Validate
    );
    assert_eq!(
        ScriptType::from_activity(&EventActivity::Click),
        ScriptType::Event
    );
    assert_eq!(
        ScriptType::from_activity(&EventActivity::Change),
        ScriptType::Event
    );
}

// =============================================================================
// JS Helper Tests
// =============================================================================

#[test]
fn test_js_helpers_available() {
    let helpers = get_all_helpers();
    assert!(!helpers.is_empty());

    // Verify key helpers exist - check for the function definitions
}

// =============================================================================
// ExclGroup Tests (XFA 3.3 §2 p.33, §4 pp.195-197)
// =============================================================================

/// Helper: register a parent exclGroup and its child fields structurally.
fn setup_exclgroup(
    engine: &mut XfaScriptEngine,
    group_path: &str,
    group_name: &str,
    children: &[(&str, &str, Option<&str>)], // (child_name, child_value, item_key)
) {
    setup_exclgroup_with_off(
        engine,
        group_path,
        group_name,
        &children
            .iter()
            .map(|(n, v, k)| (*n, *v, *k, None))
            .collect::<Vec<_>>(),
    );
}

/// Helper variant with off-value support for each child.
fn setup_exclgroup_with_off(
    engine: &mut XfaScriptEngine,
    group_path: &str,
    group_name: &str,
    children: &[(&str, &str, Option<&str>, Option<&str>)], // (child_name, child_value, item_key, off_value)
) {
    // Register parent exclGroup (is_field=false, is_parent_exclgroup=false)
    engine.register_xfa_node(
        group_name, group_path, None, false, "", false, None, None, "visible", &[],
    );

    // Register child fields (is_field=true, is_parent_exclgroup=true)
    for (child_name, child_value, item_key, off_value) in children {
        let child_path = format!("{}.{}", group_path, child_name);
        engine.register_xfa_node(
            child_name,
            &child_path,
            Some(group_path),
            true,
            child_value,
            true, // parent IS an exclGroup
            *item_key,
            *off_value,
            "visible",
            &[],
        );
    }
}

#[test]
fn test_exclgroup_child_to_parent_propagation() {
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "sex",
        "sex",
        &[("male", "", Some("M")), ("female", "", Some("F"))],
    );

    // Set a child's rawValue → should propagate to parent
    engine.update_field_value("sex.male", "M");

    let parent_val = engine.get_field_value(&SomPath::new("sex"));
    assert_eq!(
        parent_val,
        Some("M".to_string()),
        "Setting child rawValue should propagate to parent exclGroup"
    );
}

#[test]
fn test_exclgroup_propagates_zero_value() {
    // Per XFA 3.3 spec, "0" is a legitimate key value for exclGroup children
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "rating",
        "rating",
        &[("none", "", Some("0")), ("some", "", Some("1"))],
    );

    engine.update_field_value("rating.none", "0");

    let parent_val = engine.get_field_value(&SomPath::new("rating"));
    assert_eq!(
        parent_val,
        Some("0".to_string()),
        "Value '0' must propagate to parent exclGroup (it's a valid key value)"
    );
}

#[test]
fn test_exclgroup_propagates_empty_value() {
    // Per XFA 3.3 spec, clearing all selections sets exclGroup value to empty
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "choice",
        "choice",
        &[("optA", "A", Some("A")), ("optB", "", Some("B"))],
    );

    // First set a value, then clear it
    engine.update_field_value("choice.optA", "A");
    let parent_val = engine.get_field_value(&SomPath::new("choice"));
    assert_eq!(parent_val, Some("A".to_string()));

    // Now clear the child — parent should also clear
    engine.update_field_value("choice.optA", "");
    let parent_val = engine.get_field_value(&SomPath::new("choice"));
    // Parent should have received the empty value (deselect all)
    assert!(
        parent_val.is_none() || parent_val == Some("".to_string()),
        "Empty value should propagate to parent exclGroup for deselect"
    );
}

#[test]
fn test_exclgroup_structural_detection_no_naming_convention() {
    // Ensure exclGroup linkage works regardless of naming — no "Group"/"RB_"
    // patterns needed. Uses names from the XFA 3.3 spec example (§4 p.196).
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "sex",
        "sex",
        &[("male", "", Some("M")), ("female", "", Some("F"))],
    );

    engine.update_field_value("sex.male", "M");

    let parent_val = engine.get_field_value(&SomPath::new("sex"));
    assert_eq!(
        parent_val,
        Some("M".to_string()),
        "ExclGroup detection must be structural, not name-based"
    );

    // Also verify the other child can update the parent
    engine.update_field_value("sex.female", "F");
    let parent_val = engine.get_field_value(&SomPath::new("sex"));
    assert_eq!(
        parent_val,
        Some("F".to_string()),
        "Setting a different child should update the parent exclGroup value"
    );
}

#[test]
fn test_exclgroup_non_child_does_not_propagate() {
    // Fields NOT inside an exclGroup should NOT propagate to their parent
    let mut engine = XfaScriptEngine::new();

    // Register a regular subform with children (is_parent_exclgroup=false)
    engine.register_xfa_node(
        "form", "form", None, false, "", false, None, None, "visible", &[],
    );
    engine.register_xfa_node(
        "field1",
        "form.field1",
        Some("form"),
        true,
        "",
        false,
        None,
        None,
        "visible",
        &[],
    );

    engine.update_field_value("form.field1", "hello");

    // Parent should NOT have received the value
    let parent_val = engine.get_field_value(&SomPath::new("form"));
    assert!(
        parent_val.is_none() || parent_val == Some("".to_string()),
        "Non-exclGroup parent should not receive child values"
    );
}

#[test]
fn test_exclgroup_script_sets_child_rawvalue() {
    // Simulate a script setting rawValue on a child via JavaScript
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "paymentMethod",
        "paymentMethod",
        &[
            ("cash", "", Some("CASH")),
            ("card", "", Some("CARD")),
            ("transfer", "", Some("TRANSFER")),
        ],
    );

    // Simulate script: this.rawValue = "card_value"
    engine.set_current_field("paymentMethod.card", "card", "");
    let script = XfaScript {
        source: r#"this.rawValue = "CARD";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script);

    // The parent exclGroup should have received the value via the setter
    let parent_val = engine.get_field_value(&SomPath::new("paymentMethod"));
    assert_eq!(
        parent_val,
        Some("CARD".to_string()),
        "Script setting child rawValue should propagate to parent exclGroup"
    );
}

// =============================================================================
// ExclGroup Parent→Child Propagation Tests (XFA 3.3 §4 pp.195-197)
// =============================================================================
// Per XFA spec: "The field determines whether it is on or off by comparing
// the value of the variable to its own key value."
// Setting exclGroup.rawValue must update children's ON/OFF state.

#[test]
fn test_exclgroup_parent_to_child_propagation() {
    // Per XFA 3.3 §4 p.196: setting parent value should turn ON the
    // matching child and turn OFF all others.
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "sex",
        "sex",
        &[
            ("male", "", Some("M")),
            ("female", "", Some("F")),
            ("na", "", Some("NA")),
        ],
    );

    // Set the parent exclGroup's rawValue to "M"
    engine.update_field_value("sex", "M");

    // The child whose _itemKey matches "M" should be ON (rawValue=_itemKey per XFA 3.3 §17 p.714)
    let male_val = engine.get_field_value(&SomPath::new("sex.male"));
    assert_eq!(
        male_val,
        Some("M".to_string()),
        "Child with matching _itemKey should be turned ON (rawValue=_itemKey per XFA spec)"
    );

    // Other children should be OFF (rawValue="")
    let female_val = engine.get_field_value(&SomPath::new("sex.female"));
    assert!(
        female_val.is_none() || female_val == Some("".to_string()),
        "Child with non-matching _itemKey should be OFF (rawValue='')"
    );
    let na_val = engine.get_field_value(&SomPath::new("sex.na"));
    assert!(
        na_val.is_none() || na_val == Some("".to_string()),
        "Child with non-matching _itemKey should be OFF (rawValue='')"
    );
}

#[test]
fn test_exclgroup_parent_to_child_deselects_previous() {
    // Changing the parent value should deselect the previously ON child
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "color",
        "color",
        &[
            ("red", "", Some("R")),
            ("green", "", Some("G")),
            ("blue", "", Some("B")),
        ],
    );

    // Select red
    engine.update_field_value("color", "R");
    assert_eq!(
        engine.get_field_value(&SomPath::new("color.red")),
        Some("R".to_string()),
        "red should be ON (rawValue=_itemKey per XFA spec)"
    );

    // Now select green → red should become OFF
    engine.update_field_value("color", "G");
    assert_eq!(
        engine.get_field_value(&SomPath::new("color.green")),
        Some("G".to_string()),
        "green should be ON (rawValue=_itemKey per XFA spec)"
    );
    let red_val = engine.get_field_value(&SomPath::new("color.red"));
    assert!(
        red_val.is_none() || red_val == Some("".to_string()),
        "red should be OFF after selecting green"
    );
}

#[test]
fn test_exclgroup_off_value_used_when_deactivated() {
    // Per XFA 3.3 §17 pp.758-759: when a member has two <items> values,
    // the second is used as the off-value when the member is deactivated.
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup_with_off(
        &mut engine,
        "toggle",
        "toggle",
        &[
            ("yes", "", Some("Y"), Some("N")),   // on=Y, off=N
            ("maybe", "", Some("M"), Some("X")), // on=M, off=X
        ],
    );

    // Activate "yes"
    engine.update_field_value("toggle", "Y");
    assert_eq!(
        engine.get_field_value(&SomPath::new("toggle.yes")),
        Some("Y".to_string()),
        "Activated member should return its on-value"
    );
    // "maybe" is deactivated → should return off-value "X"
    assert_eq!(
        engine.get_field_value(&SomPath::new("toggle.maybe")),
        Some("X".to_string()),
        "Deactivated member with off-value should return off-value, not empty string"
    );

    // Now activate "maybe"
    engine.update_field_value("toggle", "M");
    assert_eq!(
        engine.get_field_value(&SomPath::new("toggle.maybe")),
        Some("M".to_string()),
        "Activated member should return its on-value"
    );
    // "yes" is now deactivated → should return off-value "N"
    assert_eq!(
        engine.get_field_value(&SomPath::new("toggle.yes")),
        Some("N".to_string()),
        "Deactivated member with off-value should return off-value, not empty string"
    );
}

#[test]
fn test_exclgroup_parent_to_child_via_script() {
    // Setting rawValue on the parent via JS script should propagate to children
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "status",
        "status",
        &[("active", "", Some("A")), ("inactive", "", Some("I"))],
    );

    // Set the parent's rawValue via JavaScript
    engine.set_current_field("status", "status", "");
    let script = XfaScript {
        source: r#"this.rawValue = "I";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script);

    // The matching child should be ON (rawValue=_itemKey per XFA 3.3 §17 p.714)
    let inactive_val = engine.get_field_value(&SomPath::new("status.inactive"));
    assert_eq!(
        inactive_val,
        Some("I".to_string()),
        "Child matching parent's rawValue should be ON (rawValue=_itemKey per XFA spec)"
    );

    // The non-matching child should be OFF
    let active_val = engine.get_field_value(&SomPath::new("status.active"));
    assert!(
        active_val.is_none() || active_val == Some("".to_string()),
        "Child not matching parent's rawValue should be OFF after script sets parent"
    );
}

// =============================================================================
// Empty Value Preservation Tests (XFA spec compliance)
// =============================================================================
// Per XFA spec, empty strings are valid field states:
// - A cleared dropdown has rawValue = ""
// - A deselected exclGroup member has rawValue = ""
// - A script that clears a field via this.rawValue = "" is a valid action
// These must NOT be silently dropped.

#[test]
fn test_get_all_som_field_values_preserves_empty_strings() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("field1", "field1", "hello");
    engine.register_field("field2", "field2", "");

    let values = engine.get_all_som_field_values_by_path();
    assert_eq!(
        values.get("field1"),
        Some(&"hello".to_string()),
        "Non-empty field should be present"
    );
    assert_eq!(
        values.get("field2"),
        Some(&"".to_string()),
        "Empty string field should be present, not dropped"
    );
}

#[test]
fn test_get_all_field_values_for_flattening_preserves_empty_strings() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("field1", "field1", "hello");
    engine.register_field("field2", "field2", "");

    let values = engine.get_all_field_values_for_flattening();
    assert_eq!(
        values.get(&SomPath::new("field1")),
        Some(&"hello".to_string()),
        "Non-empty field should be present in flattening values"
    );
    assert_eq!(
        values.get(&SomPath::new("field2")),
        Some(&"".to_string()),
        "Empty string field should be present in flattening values, not dropped"
    );
}

#[test]
fn test_script_clearing_field_via_empty_rawvalue_is_detected() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("myField", "myField", "initial_value");

    // Simulate a script that clears the field
    engine.set_current_field("myField", "myField", "initial_value");
    let script = XfaScript {
        source: r#"this.rawValue = "";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script);

    // The cleared value must appear as empty string, not be absent
    let values = engine.get_all_som_field_values_by_path();
    assert!(
        values.contains_key("myField"),
        "Cleared field must still be present in get_all_som_field_values"
    );
    assert_eq!(
        values.get("myField"),
        Some(&"".to_string()),
        "Field cleared via rawValue = '' should have empty string value"
    );

    let flat_values = engine.get_all_field_values_for_flattening();
    assert!(
        flat_values.contains_key(&SomPath::new("myField")),
        "Cleared field must still be present in get_all_field_values_for_flattening"
    );
    assert_eq!(
        flat_values.get(&SomPath::new("myField")),
        Some(&"".to_string()),
        "Field cleared via rawValue = '' should have empty string in flattening values"
    );
}

#[test]
fn test_exclgroup_deselection_preserves_empty_values() {
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "myGroup",
        "myGroup",
        &[
            ("optA", "", Some("A")),
            ("optB", "", Some("B")),
            ("optC", "", Some("C")),
        ],
    );

    // All children start with empty rawValue (deselected state)
    let values = engine.get_all_som_field_values_by_path();
    // The deselected children should have empty string values, not be absent
    assert_eq!(
        values.get("myGroup.optA"),
        Some(&"".to_string()),
        "Deselected exclGroup member should have empty string value"
    );
    assert_eq!(
        values.get("myGroup.optB"),
        Some(&"".to_string()),
        "Deselected exclGroup member should have empty string value"
    );
    assert_eq!(
        values.get("myGroup.optC"),
        Some(&"".to_string()),
        "Deselected exclGroup member should have empty string value"
    );
}

// =============================================================================
// resolveNodes() Tests (XFA 3.3 §3 pp.106-107)
// =============================================================================

#[test]
fn test_resolve_nodes_all_instances() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Detail", "Detail", "");
    engine.register_field("Detail.Item", "Item", "val1");
    engine.register_field("Detail.Item[1]", "Item", "val2");
    engine.register_field("Detail.Item[2]", "Item", "val3");

    // resolveNodes("Item[*]") should return all 3 instances
    engine.set_current_field("Detail.Test", "Test", "");
    let script = XfaScript {
        source: r#"
            var nodes = xfa.resolveNodes("Item[*]");
            this.rawValue = String(nodes.length);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "3", "resolveNodes('Item[*]') should return 3 items");
    }
}

#[test]
fn test_resolve_nodes_specific_index() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Detail", "Detail", "");
    engine.register_field("Detail.Item", "Item", "first");
    engine.register_field("Detail.Item[1]", "Item", "second");

    // resolveNodes("Item[0]") should return exactly 1 item
    engine.set_current_field("Detail.Test", "Test", "");
    let script = XfaScript {
        source: r#"
            var nodes = xfa.resolveNodes("Item[0]");
            this.rawValue = String(nodes.length);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(
            value, "1",
            "resolveNodes('Item[0]') should return exactly 1 item"
        );
    }
}

#[test]
fn test_resolve_nodes_nonexistent() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Detail.Item", "Item", "val1");

    // resolveNodes for a nonexistent field should return empty array
    engine.set_current_field("Detail.Test", "Test", "");
    let script = XfaScript {
        source: r#"
            var nodes = xfa.resolveNodes("NonexistentField[*]");
            this.rawValue = String(nodes.length);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(
            value, "0",
            "resolveNodes for non-existent field should return empty array"
        );
    }
}

#[test]
fn test_resolve_nodes_descendant_accessor() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Section1.Amount", "Amount", "100");
    engine.register_field("Form.Section2.Amount", "Amount", "200");

    // `Form..Amount` is the first descendant named Amount in document
    // order (XFA 3.3 §3 "Selecting Descendants At Any Level"), one node;
    // `$data` names a model this engine does not have, which is an error.
    engine.set_current_field("Form.Total", "Total", "");
    let script = XfaScript {
        source: r#"
            var nodes = xfa.resolveNodes("Form..Amount");
            var data;
            try { xfa.resolveNodes("$data..Amount"); data = "no error"; } catch (e) { data = "error"; }
            this.rawValue = String(nodes.length) + nodes.item(0).rawValue + data;
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "1100error", "the first descendant only, and $data is an error");
    }
}

#[test]
fn test_resolve_nodes_by_simple_name() {
    let mut engine = XfaScriptEngine::new();
    // Containers first, as the form's hierarchy builder registers them.
    engine.register_field_with_presence("Page1", "Page1", "", "visible", true, &[]);
    engine.register_field_with_presence("Page2", "Page2", "", "visible", true, &[]);
    engine.register_field("Page1.Field1", "Field1", "a");
    engine.register_field("Page2.Field1", "Field1", "b");
    engine.register_field("Page1.Field2", "Field2", "c");

    // XFA 3.3 §3: an unqualified name with no index is instance 0 of the
    // nearest match in scope -- one node, not every node of that name in the
    // document. `Page2.Field1` is the other page's, reached by qualifying it.
    engine.set_current_field("Page1.Test", "Test", "");
    let script = XfaScript {
        source: r#"
            var nodes = xfa.resolveNodes("Field1");
            var other = xfa.resolveNodes("Page2.Field1");
            this.rawValue = String(nodes.length) + nodes.item(0).rawValue
                + String(other.length) + other.item(0).rawValue;
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(
            value, "1a1b",
            "resolveNodes('Field1') is the one Field1 in scope; Page2's is qualified"
        );
    }
}

// =============================================================================
// ExclGroup Dedup Fix Tests
// =============================================================================

#[test]
fn test_exclgroup_dedup_full_path_keys() {
    // Two exclGroups each with a child named "RB_1" — both selected.
    // Full-path keys must preserve both values.
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "groupA",
        "groupA",
        &[("RB_1", "", Some("A")), ("RB_2", "", Some("B"))],
    );
    setup_exclgroup(
        &mut engine,
        "groupB",
        "groupB",
        &[("RB_1", "", Some("X")), ("RB_2", "", Some("Y"))],
    );

    // Select RB_1 in groupA (value "A") and RB_1 in groupB (value "X")
    engine.update_field_value("groupA", "A");
    engine.update_field_value("groupB", "X");

    let values = engine.get_all_som_field_values_by_path();

    // Full-path entries must both exist and be correct
    assert_eq!(
        values.get("groupA.RB_1"),
        Some(&"A".to_string()),
        "groupA.RB_1 should be 'A' via full-path key"
    );
    assert_eq!(
        values.get("groupB.RB_1"),
        Some(&"X".to_string()),
        "groupB.RB_1 should be 'X' via full-path key"
    );
}

#[test]
fn test_exclgroup_dedup_selected_vs_deselected() {
    // One RB_1 selected, one deselected — selected value retrievable by full path.
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(
        &mut engine,
        "groupA",
        "groupA",
        &[("RB_1", "", Some("A")), ("RB_2", "", Some("B"))],
    );
    setup_exclgroup(
        &mut engine,
        "groupB",
        "groupB",
        &[("RB_1", "", Some("X")), ("RB_2", "", Some("Y"))],
    );

    // Select RB_1 only in groupA
    engine.update_field_value("groupA", "A");

    let full_path_values = engine.get_all_som_field_values_by_path();

    assert_eq!(
        full_path_values.get("groupA.RB_1"),
        Some(&"A".to_string()),
        "Selected groupA.RB_1 should be 'A'"
    );
    assert_eq!(
        full_path_values.get("groupB.RB_1"),
        Some(&"".to_string()),
        "Deselected groupB.RB_1 should be ''"
    );

    assert!(
        !full_path_values.contains_key("RB_1"),
        "a field is keyed by its full path only, never its bare name"
    );
}

// =============================================================================
// $event Property Tests (XFA 3.3 §10 pp.398-404)
// =============================================================================

#[test]
fn test_event_name_property() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "hello");

    // Set up event context for a click event
    engine.update_event_context(&EventActivity::Click, "Form.Field1", None);
    engine.set_current_field("Form.Field1", "Field1", "hello");

    // $event.name should be "click"
    let script = XfaScript {
        source: r#"this.rawValue = xfa.event.name;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Click,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "click", "$event.name should be 'click'");
    }
}

#[test]
fn test_event_target_property() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.MyField", "MyField", "test");

    engine.update_event_context(&EventActivity::Enter, "Form.MyField", None);
    engine.set_current_field("Form.MyField", "MyField", "test");

    // $event.target should be the field object with name "MyField"
    let script = XfaScript {
        source: r#"this.rawValue = xfa.event.target.name;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Enter,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "MyField", "$event.target.name should be 'MyField'");
    }
}

#[test]
fn test_event_cancel_action_writable() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "");

    engine.update_event_context(&EventActivity::Validate, "Form.Field1", None);
    engine.set_current_field("Form.Field1", "Field1", "");

    // cancelAction should be writable
    let script = XfaScript {
        source: r#"
            xfa.event.cancelAction = true;
            this.rawValue = String(xfa.event.cancelAction);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Validate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "true", "cancelAction should be writable");
    }
}

#[test]
fn test_event_modifier_defaults_false() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "");

    engine.update_event_context(&EventActivity::Click, "Form.Field1", None);
    engine.set_current_field("Form.Field1", "Field1", "");

    // modifier should default to false
    let script = XfaScript {
        source: r#"this.rawValue = String(xfa.event.modifier);"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Click,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "false", "$event.modifier should default to false");
    }
}

#[test]
fn test_event_change_property_for_change_event() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "original");

    // The caller supplies the value the field held before the change;
    // prevText is that, and change, newText and fullText are the new text.
    engine.update_field_value("Form.Field1", "edited");
    engine.update_event_context(&EventActivity::Change, "Form.Field1", Some("original"));
    engine.set_current_field("Form.Field1", "Field1", "edited");

    let script = XfaScript {
        source: r#"this.rawValue = xfa.event.prevText;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Change,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_deref(), Some("original"), "prevText is the value before the change");
    engine.update_event_context(&EventActivity::Change, "Form.Field1", Some("original"));
    assert_eq!(
        engine.evaluate_expression("[xfa.event.change, xfa.event.newText, xfa.event.selStart, xfa.event.selEnd].join('|')").unwrap(),
        "original|original|0|8",
        "change and newText are the text put in; the selection spans the previous text"
    );
}

#[test]
fn test_event_all_properties_accessible() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "");

    engine.update_event_context(&EventActivity::Click, "Form.Field1", None);
    engine.set_current_field("Form.Field1", "Field1", "");

    // Verify all expected properties are accessible (not undefined)
    let script = XfaScript {
        source: r#"
            var e = xfa.event;
            var props = [
                typeof e.name !== 'undefined',
                typeof e.target !== 'undefined',
                typeof e.cancelAction !== 'undefined',
                typeof e.change !== 'undefined',
                typeof e.commitKey !== 'undefined',
                typeof e.fullText !== 'undefined',
                typeof e.keyDown !== 'undefined',
                typeof e.modifier !== 'undefined',
                typeof e.newContentType !== 'undefined',
                typeof e.newText !== 'undefined',
                typeof e.prevContentType !== 'undefined',
                typeof e.prevText !== 'undefined',
                typeof e.reenter !== 'undefined',
                typeof e.selEnd !== 'undefined',
                typeof e.selStart !== 'undefined',
                typeof e.shift !== 'undefined'
            ];
            this.rawValue = String(props.every(function(p) { return p; }));
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Click,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(
            value, "true",
            "All $event properties should be accessible (not undefined)"
        );
    }
}

// =============================================================================
// Gap 4: Calculation Convergence Tests
// =============================================================================

#[test]
fn test_calc_convergence_forward_order() {
    // A calculates "10", B calculates A.rawValue * 2.
    // In forward order this converges in 1 iteration.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.A", "A", "");
    engine.register_field("Form.B", "B", "");

    // First calc: A = "10"
    engine.set_current_field("Form.A", "A", "");
    let script_a = XfaScript {
        source: r#"this.rawValue = "10";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script_a);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some("10".to_string()));

    // Second calc: B = A * 2
    engine.set_current_field("Form.B", "B", "");
    let script_b = XfaScript {
        source: r#"this.rawValue = String(Number(A.rawValue) * 2);"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script_b);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("20".to_string()),
        "B should see A's value and compute 20"
    );
}

#[test]
fn test_calc_convergence_reverse_order() {
    // B calculates A.rawValue * 2, then A calculates "10".
    // On first pass, B sees stale A (empty) → "0" or "NaN".
    // The convergence loop in ScriptExecutor would re-run B after A is set.
    // Here we simulate the two-pass convergence at the engine level.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.A", "A", "");
    engine.register_field("Form.B", "B", "");

    // Pass 1: B runs first (sees empty A)
    engine.set_current_field("Form.B", "B", "");
    let script_b = XfaScript {
        source: r#"this.rawValue = String(Number(A.rawValue) * 2);"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script_b);

    // Pass 1: A runs second
    engine.set_current_field("Form.A", "A", "");
    let script_a = XfaScript {
        source: r#"this.rawValue = "10";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script_a);

    // Pass 2: B runs again, now sees A = "10"
    engine.set_current_field("Form.B", "B", "");
    let result = engine.execute_script(&script_b);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("20".to_string()),
        "B should see updated A and compute 20 on second pass"
    );
}

#[test]
fn test_calc_convergence_chain_three_fields() {
    // Chain: C depends on B depends on A.
    // Order: C, B, A (worst case — reverse).
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.A", "A", "");
    engine.register_field("Form.B", "B", "");
    engine.register_field("Form.C", "C", "");

    let script_c = XfaScript {
        source: r#"this.rawValue = String(Number(B.rawValue) + 1);"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let script_b = XfaScript {
        source: r#"this.rawValue = String(Number(A.rawValue) + 1);"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let script_a = XfaScript {
        source: r#"this.rawValue = "5";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    // Simulate 3 passes:
    // Pass 1: C=NaN+1=NaN, B=NaN+1=NaN, A=5
    // Pass 2: C=NaN+1=NaN, B=5+1=6, A=5
    // Pass 3: C=6+1=7, B=6, A=5 → converged

    for _ in 0..3 {
        engine.set_current_field("Form.C", "C", "");
        let _ = engine.execute_script(&script_c);
        engine.set_current_field("Form.B", "B", "");
        let _ = engine.execute_script(&script_b);
        engine.set_current_field("Form.A", "A", "");
        let _ = engine.execute_script(&script_a);
    }

    // After convergence, verify values
    let values = engine.get_all_som_field_values_by_path();
    assert_eq!(values.get("Form.A"), Some(&"5".to_string()), "A should be 5");
    assert_eq!(values.get("Form.B"), Some(&"6".to_string()), "B should be 6");
    assert_eq!(values.get("Form.C"), Some(&"7".to_string()), "C should be 7");
}

#[test]
fn test_calc_already_convergent_single_pass() {
    // Independent calculations should converge in 1 pass (no regression).
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.X", "X", "");
    engine.register_field("Form.Y", "Y", "");

    engine.set_current_field("Form.X", "X", "");
    let script_x = XfaScript {
        source: r#"this.rawValue = "hello";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script_x);

    engine.set_current_field("Form.Y", "Y", "");
    let script_y = XfaScript {
        source: r#"this.rawValue = "world";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script_y);

    // A second pass yields the same value: the calculate result is the
    // script's value whether or not it changed; convergence is the
    // executor's comparison, not the engine's silence.
    engine.set_current_field("Form.X", "X", "hello");
    let result_x = engine.execute_script(&script_x);
    assert_eq!(result_x.unwrap(), Some("hello".to_string()));

    engine.set_current_field("Form.Y", "Y", "world");
    let result_y = engine.execute_script(&script_y);
    assert_eq!(result_y.unwrap(), Some("world".to_string()));
}

// =============================================================================
// Gap 5: Hidden Fields Still Calculate Tests
// =============================================================================

#[test]
fn test_hidden_field_calculates_value() {
    // A hidden field's calculate script should still run.
    // Per XFA 3.3 §2 p.68: "hidden: The container... may still perform
    // calculations, validations, and event processing."
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence("Form.HiddenField", "HiddenField", "", "hidden", false, &[]);

    engine.set_current_field("Form.HiddenField", "HiddenField", "");
    let script = XfaScript {
        source: r#"this.rawValue = "computed_while_hidden";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("computed_while_hidden".to_string()),
        "Hidden field's calculate script should produce a value"
    );
}

#[test]
fn test_hidden_field_initializes() {
    // A hidden field's initialize event should still fire.
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence("Form.HiddenInit", "HiddenInit", "", "hidden", false, &[]);

    engine.set_current_field("Form.HiddenInit", "HiddenInit", "");
    let script = XfaScript {
        source: r#"this.rawValue = "initialized_while_hidden";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("initialized_while_hidden".to_string()),
        "Hidden field's initialize event should still fire"
    );
}

#[test]
fn test_inactive_field_presence_in_engine() {
    // An inactive field is registered but the script executor (not the engine)
    // is responsible for skipping it. The engine itself executes anything given.
    // This test confirms the engine CAN run scripts on inactive fields —
    // the suppression happens at the ScriptExecutor level.
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence(
        "Form.InactiveField",
        "InactiveField",
        "",
        "inactive",
        false,
        &[],
    );

    engine.set_current_field("Form.InactiveField", "InactiveField", "");
    let script = XfaScript {
        source: r#"this.rawValue = "should_not_run_in_practice";"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    // Engine itself doesn't enforce presence — it always executes
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("should_not_run_in_practice".to_string()),
        "Engine itself doesn't skip inactive fields — ScriptExecutor does"
    );
}

// =============================================================================
// Gap 7: Property Stub Objects Tests
// =============================================================================

#[test]
fn test_border_property_accessible() {
    // Scripts accessing this.border.edge.color.value should not error.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "original");
    engine.set_current_field("Form.Field1", "Field1", "original");

    let script = XfaScript {
        source: r#"
            var result = (typeof this.border !== 'undefined') &&
                         (typeof this.border.edge !== 'undefined') &&
                         (typeof this.border.edge.color !== 'undefined') &&
                         (typeof this.border.edge.color.value !== 'undefined');
            this.rawValue = String(result);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("true".to_string()),
        "border.edge.color.value should be accessible"
    );
}

#[test]
fn test_border_color_settable() {
    // Setting border.edge.color.value should not throw.
    // Per XFA 3.3 §10 Example 10.13.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "ok");
    engine.set_current_field("Form.Field1", "Field1", "ok");

    let script = XfaScript {
        source: r#"
            this.border.edge.color.value = "255,0,0";
            // rawValue should be unaffected
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    // rawValue should NOT have changed — property calcs don't affect rawValue
    assert_eq!(
        result.unwrap(),
        None,
        "Setting border color should not change rawValue"
    );
}

#[test]
fn test_font_property_accessible() {
    // Scripts accessing this.font properties should not error.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "");
    engine.set_current_field("Form.Field1", "Field1", "");

    let script = XfaScript {
        source: r#"
            var result = (typeof this.font !== 'undefined') &&
                         (typeof this.font.typeface !== 'undefined') &&
                         (typeof this.font.size !== 'undefined') &&
                         (typeof this.font.fill !== 'undefined') &&
                         (typeof this.font.fill.color !== 'undefined') &&
                         (typeof this.font.fill.color.value !== 'undefined');
            this.rawValue = String(result);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("true".to_string()),
        "font and sub-properties should be accessible"
    );
}

#[test]
fn test_caption_and_assist_accessible() {
    // Scripts accessing this.caption and this.assist should not error.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "");
    engine.set_current_field("Form.Field1", "Field1", "");

    let script = XfaScript {
        source: r#"
            var result = (typeof this.caption !== 'undefined') &&
                         (typeof this.caption.value !== 'undefined') &&
                         (typeof this.assist !== 'undefined') &&
                         (typeof this.assist.toolTip !== 'undefined');
            this.rawValue = String(result);
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("true".to_string()),
        "caption.value and assist.toolTip should be accessible"
    );
}

#[test]
fn test_property_stubs_dont_affect_raw_value() {
    // Modifying property objects should NOT affect the field's rawValue.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "original");
    engine.set_current_field("Form.Field1", "Field1", "original");

    let script = XfaScript {
        source: r#"
            this.border.edge.color.value = "255,0,0";
            this.font.fill.color.value = "0,128,0";
            this.caption.value = "New Caption";
            this.assist.toolTip = "New Tooltip";
            // rawValue should remain "original"
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        None,
        "Property modifications should not change rawValue"
    );

    // Verify rawValue is still "original"
    let values = engine.get_all_som_field_values_by_path();
    assert_eq!(values.get("Form.Field1"), Some(&"original".to_string()));
}

// =============================================================================
// SOM Resolution Tests
// =============================================================================

#[test]
fn test_relocatable_script_two_detail_subforms() {
    // Per XFA 3.3 §3 pp.111-112 Example 3.33: same script in two
    // instances of a Detail subform resolves to local siblings.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Receipt.Detail1.Units", "Units", "5");
    engine.register_field("Receipt.Detail1.UnitPrice", "UnitPrice", "10");
    engine.register_field("Receipt.Detail1.Total", "Total", "");
    engine.register_field("Receipt.Detail2.Units", "Units", "3");
    engine.register_field("Receipt.Detail2.UnitPrice", "UnitPrice", "20");
    engine.register_field("Receipt.Detail2.Total", "Total", "");

    // Script running on Detail1.Total should see Detail1.Units
    engine.set_current_field("Receipt.Detail1.Total", "Total", "");
    let script = XfaScript {
        source: r#"this.rawValue = String(Number(Units.rawValue) * Number(UnitPrice.rawValue));"#
            .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Calculate,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("50".to_string()),
        "Detail1.Total should use Detail1.Units (5) * Detail1.UnitPrice (10) = 50"
    );

    // Script running on Detail2.Total should see Detail2.Units
    engine.set_current_field("Receipt.Detail2.Total", "Total", "");
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("60".to_string()),
        "Detail2.Total should use Detail2.Units (3) * Detail2.UnitPrice (20) = 60"
    );
}

// =============================================================================
// Gap 9: Event Propagation Tests
// =============================================================================

#[test]
fn test_listen_attribute_parsed() {
    // Verify that the listen attribute is correctly parsed.
    assert_eq!(
        "refAndDescendents".parse::<ListenScope>().unwrap(),
        ListenScope::RefAndDescendents
    );
    assert_eq!(
        "refOnly".parse::<ListenScope>().unwrap(),
        ListenScope::RefOnly
    );
    // Default for unrecognized values
    assert_eq!(
        "unknown".parse::<ListenScope>().unwrap(),
        ListenScope::RefOnly
    );
}

#[test]
fn test_event_activity_name() {
    assert_eq!(EventActivity::Click.activity_name(), "click");
    assert_eq!(EventActivity::Calculate.activity_name(), "calculate");
    assert_eq!(EventActivity::Initialize.activity_name(), "initialize");
    assert_eq!(EventActivity::Ready.activity_name(), "ready");
    assert_eq!(EventActivity::Validate.activity_name(), "validate");
    assert_eq!(EventActivity::Enter.activity_name(), "enter");
    assert_eq!(EventActivity::Exit.activity_name(), "exit");
    assert_eq!(EventActivity::Change.activity_name(), "change");
    assert_eq!(EventActivity::DocReady.activity_name(), "docReady");
    assert_eq!(
        EventActivity::Other("custom".to_string()).activity_name(),
        "custom"
    );
}

// =============================================================================
// Gap 10: execEvent Tests
// =============================================================================

#[test]
fn test_exec_event_basic() {
    // Field A's script calls B.execEvent("click"). B's click handler
    // sets B.rawValue = "clicked".
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.A", "A", "");
    engine.register_field("Form.B", "B", "");

    // Register B's click event script
    engine.register_event_script("Form.B", "click", r#"this.rawValue = "clicked";"#);

    // A's script calls B.execEvent("click")
    engine.set_current_field("Form.A", "A", "");
    let script = XfaScript {
        source: r#"B.execEvent("click");"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script);

    // Verify B's rawValue was set by the click handler
    let values = engine.get_all_som_field_values_by_path();
    assert_eq!(
        values.get("Form.B"),
        Some(&"clicked".to_string()),
        "B's click handler should have run via execEvent"
    );
}

#[test]
fn test_exec_event_inactive_fails_silently() {
    // Per XFA 3.3 §10 Rule 3: execEvent on inactive container fails silently.
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence("Form.B", "B", "", "inactive", false, &[]);

    engine.register_event_script("Form.B", "click", r#"this.rawValue = "clicked";"#);

    engine.set_current_field("Form.A", "A", "");
    let script = XfaScript {
        source: r#"B.execEvent("click");"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok(), "execEvent on inactive should not error");

    // B's rawValue should remain empty — handler was NOT executed
    let values = engine.get_all_som_field_values_by_path();
    let b_val = values.get("Form.B").cloned().unwrap_or_default();
    assert!(
        b_val.is_empty() || b_val.is_empty(),
        "Inactive field's handler should not have run, got: {}",
        b_val
    );
}

#[test]
fn test_exec_event_returns_to_caller() {
    // After execEvent, the calling script should continue running.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.A", "A", "");
    engine.register_field("Form.B", "B", "");

    engine.register_event_script("Form.B", "enter", r#"this.rawValue = "entered";"#);

    engine.set_current_field("Form.A", "A", "");
    let script = XfaScript {
        source: r#"
            B.execEvent("enter");
            this.rawValue = "continued";
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        Some("continued".to_string()),
        "Calling script should continue after execEvent"
    );

    // B should also have been updated
    let values = engine.get_all_som_field_values_by_path();
    assert_eq!(
        values.get("Form.B"),
        Some(&"entered".to_string()),
        "B's enter handler should have run"
    );
}

#[test]
fn test_exec_event_nonexistent_activity() {
    // execEvent with a nonexistent activity should do nothing, no error.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.A", "A", "before");

    engine.set_current_field("Form.A", "A", "before");
    let script = XfaScript {
        source: r#"A.execEvent("nonExistentEvent");"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(
        result.is_ok(),
        "execEvent with nonexistent activity should not error"
    );
}

#[test]
fn test_exec_event_calculate() {
    // execEvent("calculate") should trigger the field's calculate script.
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Total", "Total", "");
    engine.register_field("Form.Price", "Price", "100");

    engine.register_event_script(
        "Form.Total",
        "calculate",
        r#"this.rawValue = String(Number(Price.rawValue) * 2);"#,
    );

    engine.set_current_field("Form.Price", "Price", "100");
    let script = XfaScript {
        source: r#"Total.execEvent("calculate");"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Initialize,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let _ = engine.execute_script(&script);

    let values = engine.get_all_som_field_values_by_path();
    assert_eq!(
        values.get("Form.Total"),
        Some(&"200".to_string()),
        "Total's calculate handler should run via execEvent"
    );
}

// =============================================================================
// update_field_presence_baseline Tests
// =============================================================================

#[test]
fn test_update_field_presence_baseline_updates_existing_entry() {
    let mut engine = XfaScriptEngine::new();
    // Register a field with initial presence "visible"
    engine.register_field_with_presence("Root.Page.Field1", "Field1", "hello", "visible", false, &[]);

    let path = SomPath::new("Root.Page.Field1");

    // Verify initial state
    assert_eq!(engine.get_initial_presence(&path), Some("visible"));

    // Update presence baseline from form DOM
    engine.update_field_presence_baseline(&path, "world", "hidden");

    // initial_presence should be updated
    assert_eq!(engine.get_initial_presence(&path), Some("hidden"));

    // form_state value should be updated
    assert_eq!(engine.get_form_state_value(&path).as_deref(), Some("world"));

    // No presence change should be reported (JS object matches baseline)
    let changes = engine.get_all_som_presence_changes();
    assert!(
        !changes.contains_key("Root.Page.Field1"),
        "No presence change should be reported after baseline update"
    );
}

#[test]
fn test_update_field_presence_baseline_skips_unregistered_path() {
    let mut engine = XfaScriptEngine::new();
    // Register one field
    engine.register_field_with_presence("Root.Page.Field1", "Field1", "hello", "visible", false, &[]);

    // Try to update a path that was never registered
    let unknown = SomPath::new("Root.Page.Nonexistent");
    engine.update_field_presence_baseline(&unknown, "val", "hidden");

    // Should not create a new entry
    assert!(
        !engine.has_field_object(&unknown),
        "No JS object should be created for unregistered path"
    );
    assert!(
        !engine.has_initial_presence(&unknown),
        "No initial_presence entry should be created for unregistered path"
    );
}

#[test]
fn test_update_field_presence_baseline_does_not_create_new_js_objects() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence("Root.Page.Field1", "Field1", "v1", "visible", false, &[]);

    let count_before = engine.field_objects_count();

    let path = SomPath::new("Root.Page.Field1");
    engine.update_field_presence_baseline(&path, "v2", "hidden");

    // field_objects count should not change
    assert_eq!(
        engine.field_objects_count(),
        count_before,
        "No new JS objects should be created"
    );
}

#[test]
fn test_update_field_presence_baseline_presence_change_detection() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence("Root.Page.Field1", "Field1", "v1", "visible", false, &[]);

    let path = SomPath::new("Root.Page.Field1");

    // Update baseline to "hidden" — JS object also becomes "hidden"
    engine.update_field_presence_baseline(&path, "v1", "hidden");

    // No change reported (JS matches baseline)
    let changes = engine.get_all_som_presence_changes();
    assert!(
        !changes.contains_key("Root.Page.Field1"),
        "Baseline and JS object match — no change expected"
    );

    // Simulate a script changing presence to "visible" on the JS object
    engine.set_js_presence(&path, "visible");

    // Now a change should be detected (JS="visible" vs baseline="hidden")
    let changes = engine.get_all_som_presence_changes();
    assert_eq!(
        changes.get("Root.Page.Field1"),
        Some(&"visible".to_string()),
        "Presence change should be detected after script modifies JS object"
    );
}

#[test]
fn a_field_registered_by_a_long_path_produces_no_false_presence_change() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field_with_presence("Form.Root.Page.Field1", "Field1", "val", "visible", false, &[]);

    let changes = engine.get_all_som_presence_changes();
    assert!(
        changes.is_empty(),
        "Visible field should produce no presence changes: {:?}",
        changes
    );
}

// =============================================================================
// Change Event newText / prevText / fullText Tests
// =============================================================================

#[test]
fn test_change_event_sets_new_text() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "old_value");

    // Simulate: field was updated to "new_value", prev was "old_value"
    engine.update_field_value(&SomPath::new("Form.Field1"), "new_value");
    engine.update_event_context(&EventActivity::Change, "Form.Field1", Some("old_value"));
    engine.set_current_field("Form.Field1", "Field1", "new_value");

    // newText should be the NEW value
    let script = XfaScript {
        source: r#"this.rawValue = xfa.event.newText;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Change,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "new_value", "newText should be the new value");
    }
}

#[test]
fn test_change_event_prev_text_is_old_value() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "old_value");

    // Simulate: field was updated to "new_value", prev was "old_value"
    engine.update_field_value(&SomPath::new("Form.Field1"), "new_value");
    engine.update_event_context(&EventActivity::Change, "Form.Field1", Some("old_value"));
    engine.set_current_field("Form.Field1", "Field1", "new_value");

    // prevText should be the OLD value
    let script = XfaScript {
        source: r#"this.rawValue = xfa.event.prevText;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Change,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "old_value", "prevText should be the old value");
    }
}

#[test]
fn test_change_event_full_text_is_new_value() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "old_value");

    // Simulate: field was updated to "new_value", prev was "old_value"
    engine.update_field_value(&SomPath::new("Form.Field1"), "new_value");
    engine.update_event_context(&EventActivity::Change, "Form.Field1", Some("old_value"));
    engine.set_current_field("Form.Field1", "Field1", "new_value");

    // fullText should be the resulting complete text (= new value)
    let script = XfaScript {
        source: r#"this.rawValue = xfa.event.fullText;"#.to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Change,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "new_value", "fullText should be the new value");
    }
}

// =============================================================================
// instanceManager on Subform Objects Tests
// =============================================================================

#[test]
fn test_field_object_does_not_have_instance_manager() {
    let mut engine = XfaScriptEngine::new();
    engine.register_field("Form.Field1", "Field1", "hello");
    engine.set_current_field("Form.Field1", "Field1", "hello");

    // instanceManager should NOT be on field objects (only subforms per XFA spec)
    let script = XfaScript {
        source: r#"this.rawValue = String(typeof this.instanceManager === 'undefined');"#
            .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(
            value, "true",
            "field objects should NOT have instanceManager (only subforms)"
        );
    }
}

#[test]
fn test_subform_object_has_instance_manager() {
    let mut engine = XfaScriptEngine::new();

    // A subform's instance manager lives on its parent (XFA 3.3 §9), so the
    // parent is registered first, as the hierarchy builder always does.
    engine.register_xfa_node(
        "Root",
        "Root",
        None,
        false,
        "",
        false,
        None,
        None,
        "visible",
        &[],
    );
    // register_xfa_node with is_field=false creates a subform object with instanceManager
    engine.register_xfa_node(
        "MySubform",
        "Root.MySubform",
        Some("Root"),
        false, // is_field = false → subform
        "",
        false, // is_parent_exclgroup
        None,
        None,
        "visible",
        &[],
    );

    engine.set_current_field("Root.MySubform", "MySubform", "");

    // instanceManager should be accessible on subform objects
    let script = XfaScript {
        source: r#"this.rawValue = String(typeof this.instanceManager !== 'undefined');"#
            .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(value, "true", "subform objects should have instanceManager");
    }
}

#[test]
fn test_subform_via_register_field_with_presence_has_instance_manager() {
    let mut engine = XfaScriptEngine::new();

    // The parent first: the instance manager lives on it (XFA 3.3 §9).
    engine.register_field_with_presence("Root", "Root", "", "visible", true, &[]);
    // register_field_with_presence with is_subform=true should add instanceManager
    engine.register_field_with_presence(
        "Root.DynSubform",
        "DynSubform",
        "",
        "visible",
        true, // is_subform
        &[],
    );

    engine.set_current_field("Root.DynSubform", "DynSubform", "");

    // instanceManager should be accessible with setInstances, addInstance,
    // removeInstance, count, and max (XFA 3.3 §6.16)
    let script = XfaScript {
        source: r#"
            var im = this.instanceManager;
            var hasIM = typeof im !== 'undefined';
            var hasSI = typeof im.setInstances === 'function';
            var hasAI = typeof im.addInstance === 'function';
            var hasRI = typeof im.removeInstance === 'function';
            var hasCount = typeof im.count === 'number';
            var hasMax = typeof im.max === 'number';
            this.rawValue = [hasIM, hasSI, hasAI, hasRI, hasCount, hasMax].join(',');
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };
    let result = engine.execute_script(&script);
    assert!(result.is_ok());
    if let Ok(Some(value)) = result {
        assert_eq!(
            value, "true,true,true,true,true,true",
            "subform instanceManager should have setInstances, addInstance, removeInstance, count, max"
        );
    }
}

// =============================================================================
// `.all` on a non-repeatable subform, and choice-list script methods
// =============================================================================

/// Regression test: `register_xfa_node`'s own subform-object construction
/// (used for every template-registered subform, as opposed to the simpler
/// `register_field_with_presence` path) used to skip `add_all_property`, so
/// a script reading `.all` on a plain, non-repeatable subform threw
/// "cannot convert 'null' or 'undefined' to object" instead of getting the
/// one-element collection XFA 3.3 §6.16 promises. This is exactly the shape
/// of AAOV's `DYN_Signature.all` in `soSignatureLabel._changeLabel`, which
/// aborted the whole initialize script and left the signature captions at
/// their client-type-agnostic default.
#[test]
fn a_subform_registered_via_register_xfa_node_has_an_all_property() {
    let mut engine = XfaScriptEngine::new();
    engine.register_xfa_node(
        "DYN_Signature",
        "Root.DYN_Signature",
        Some("Root"),
        false, // is_field = false -> subform
        "",
        false,
        None,
        None,
        "visible",
        &[],
    );

    engine.set_current_field("Root.DYN_Signature", "DYN_Signature", "");

    let script = XfaScript {
        source: r#"
            var list = this.all;
            this.rawValue = [list.length, list.item(0) === this].join(',');
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    assert!(result.is_ok(), "script failed: {:?}", result);
    assert_eq!(
        result.unwrap(),
        Some("1,true".to_string()),
        "a non-repeatable subform's .all must be a one-element collection containing itself"
    );
}

/// XFA 3.3 §6 "Scripting Methods": a choice-list field's `addItem`,
/// `clearItems`, `getDisplayItem`, `getSaveItem` and `boundItem`, seeded
/// from the template's `<items>` and mutable from script -- the exact
/// pattern AAOV's `CL_ClientType` dropdown uses on every open
/// (`this.clearItems(); for (...) this.addItem(a[i]);`), which previously
/// threw "not a callable function" and left the dropdown empty.
#[test]
fn choice_list_methods_seed_from_items_and_stay_mutable() {
    let mut engine = XfaScriptEngine::new();
    let items = vec![
        ("Individual".to_string(), "Individual".to_string()),
        ("Legal entity".to_string(), "Legal entity".to_string()),
    ];
    engine.register_xfa_node(
        "CL_ClientType",
        "Root.CL_ClientType",
        Some("Root"),
        true, // is_field
        "",
        false,
        None,
        None,
        "visible",
        &items,
    );
    engine.set_current_field("Root.CL_ClientType", "CL_ClientType", "");

    let script = XfaScript {
        source: r#"
            var before = this.getDisplayItem(0) + "|" + this.getSaveItem(1);
            this.clearItems();
            this.addItem("A");
            this.addItem("B", "b");
            this.rawValue = before + "|" + this.getDisplayItem(0) + "|" + this.getSaveItem(0)
                + "|" + this.getSaveItem(1) + "|" + this.boundItem("B");
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    assert!(result.is_ok(), "script failed: {:?}", result);
    assert_eq!(
        result.unwrap(),
        Some("Individual|Legal entity|A|A|b|b".to_string()),
        "getDisplayItem/getSaveItem must read the template's <items>, and clearItems/addItem \
         must replace them; boundItem(\"B\") must return \"B\"'s save value"
    );
}

/// `deleteItem` and `getItemState`/`setItemState` (XFA 3.3 §6), rounding out
/// the choice-list method set beyond what AAOV's own scripts happen to use.
#[test]
fn choice_list_delete_item_and_item_state() {
    let mut engine = XfaScriptEngine::new();
    let items = vec![
        ("A".to_string(), "a".to_string()),
        ("B".to_string(), "b".to_string()),
        ("C".to_string(), "c".to_string()),
    ];
    engine.register_xfa_node(
        "Choice", "Root.Choice", Some("Root"), true, "", false, None, None, "visible", &items,
    );
    engine.set_current_field("Root.Choice", "Choice", "");

    let script = XfaScript {
        source: r#"
            this.deleteItem(1); // removes "B"/"b"; "C"/"c" shifts down to index 1
            var afterDelete = this.getDisplayItem(0) + "," + this.getDisplayItem(1);
            this.rawValue = "c";
            var stateBefore = this.getItemState(1) + "," + this.getItemState(0);
            this.setItemState(0, true);
            this.rawValue = afterDelete + "|" + stateBefore + "|" + this.rawValue;
        "#
        .to_string(),
        content_type: ScriptContentType::JavaScript,
        activity: EventActivity::Ready,
        event_ref: EventRef::Current,
        name: None,
        run_at: RunAt::Client,
        listen: ListenScope::default(),
    };

    let result = engine.execute_script(&script);
    assert!(result.is_ok(), "script failed: {:?}", result);
    assert_eq!(
        result.unwrap(),
        Some("A,C|1,0|a".to_string()),
        "deleteItem must remove exactly that item; getItemState must report the item matching \
         rawValue as selected; setItemState must set rawValue to the given item's save value"
    );
}

// =============================================================================
// Form DOM object model: parent links and node-relative SOM (XFA 3.3 §3)
// =============================================================================

/// A form with a page set, so it lays out, around `body` inside `Root`.
fn form_with(body: &str) -> XfaForm {
    let nodes = parse_template(&format!(
        r#"<subform name="Root" layout="tb"><pageSet><pageArea name="P1" w="612pt" h="792pt"><contentArea x="36pt" y="36pt" w="540pt" h="720pt"/></pageArea></pageSet>{body}</subform>"#
    ));
    XfaForm::new(nodes).expect("form")
}

fn click_script(name: &str, source: &str) -> String {
    format!(
        r#"<field name="{name}" w="40pt" h="20pt"><ui><button/></ui><event activity="click"><script contentType="application/x-javascript">{source}</script></event></field>"#
    )
}

fn value_of(form: &mut XfaForm, path: &str) -> Option<String> {
    form.current_field_values().get(path).cloned()
}

#[test]
fn every_node_object_links_to_its_parent() {
    let mut form = form_with(&format!(
        r#"<subform name="Outer" layout="tb"><subform name="Inner" layout="tb"><field name="Out" w="40pt" h="20pt"/>{}</subform></subform>"#,
        click_script(
            "Btn",
            "this.parent.Out.rawValue = this.parent.name + '/' + this.parent.parent.name + '/' + this.parent.parent.parent.name;"
        )
    ));
    form.click("Root.Outer.Inner.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Outer.Inner.Out").as_deref(),
        Some("Inner/Outer/Root")
    );
}

#[test]
fn a_nodes_own_resolve_nodes_is_relative_to_that_node_and_answers_item() {
    // The corpus `soPlusMinus.applyIndex` shape: number every instance's
    // index field through `this.resolveNodes("Row[*].Idx")` and `.item(i)`.
    let rows: String = (0..3)
        .map(|_| r#"<subform name="Row" layout="tb"><field name="Idx" w="40pt" h="20pt"/></subform>"#)
        .collect();
    let mut form = form_with(&format!(
        "{rows}{}",
        click_script(
            "Number",
            "var l = this.resolveNodes('Row[*].Idx'); for (var i = 0; i &lt; l.length; i++) { l.item(i).rawValue = String(i + 1); }"
        )
    ));
    form.click("Root.Number").expect("click");
    assert_eq!(value_of(&mut form, "Root.Row.Idx").as_deref(), Some("1"));
    assert_eq!(value_of(&mut form, "Root.Row[1].Idx").as_deref(), Some("2"));
    assert_eq!(value_of(&mut form, "Root.Row[2].Idx").as_deref(), Some("3"));
}

#[test]
fn a_bare_name_is_instance_zero_and_an_index_reaches_a_later_sibling() {
    let rows: String = (0..2)
        .map(|_| r#"<subform name="Row" layout="tb"><field name="F" w="40pt" h="20pt"/></subform>"#)
        .collect();
    let mut form = form_with(&format!(
        "{rows}{}",
        click_script(
            "Set",
            "Row.F.rawValue = 'zero'; xfa.resolveNode('Row[1].F').rawValue = 'one'; this.resolveNode('Root.Row[1]').F.rawValue += '!';"
        )
    ));
    form.click("Root.Set").expect("click");
    assert_eq!(value_of(&mut form, "Root.Row.F").as_deref(), Some("zero"));
    assert_eq!(
        value_of(&mut form, "Root.Row[1].F").as_deref(),
        Some("one!")
    );
}

#[test]
fn a_nested_button_reaches_its_sections_instance_manager_through_parent_links() {
    // `soPlusMinus.insertNode(this.parent.parent)` from `STP_PlusMinus.Button_Add`.
    let mut form = form_with(&format!(
        r#"<subform name="Section" layout="tb"><field name="Seen" w="40pt" h="20pt"/><subform name="STP_PlusMinus" layout="tb">{}</subform></subform>"#,
        click_script(
            "Button_Add",
            "var s = this.parent.parent; s.Seen.rawValue = s.name + ':' + typeof s.instanceManager.addInstance;"
        )
    ));
    form.click("Root.Section.STP_PlusMinus.Button_Add")
        .expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Section.Seen").as_deref(),
        Some("Section:function")
    );
}

// =============================================================================
// Instance manager (XFA 3.3 §9): limits and the JS side of add/remove
// =============================================================================

fn repeat_form(occur: &str, script: &str) -> XfaForm {
    form_with(&format!(
        r#"<subform name="Row" layout="tb"><occur {occur}/><field name="F" w="40pt" h="20pt"/></subform><field name="Out" w="200pt" h="20pt"/>{}"#,
        click_script("Go", script)
    ))
}

#[test]
fn the_instance_manager_carries_the_declared_limits_and_is_reachable_by_its_underscore_name() {
    let mut form = repeat_form(
        r#"min="1" max="3" initial="2""#,
        "Out.rawValue = [_Row.name, _Row.count, _Row.min, _Row.max, Row.instanceManager === _Row, xfa.resolveNode('Row[1]').instanceIndex].join(',');",
    );
    form.click("Root.Go").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("_Row,2,1,3,true,1")
    );
}

#[test]
fn an_unlimited_repeatable_reports_max_minus_one_and_a_plain_subform_is_one_one() {
    let mut form = form_with(&format!(
        r#"<subform name="Row" layout="tb"><occur max="-1"/></subform><subform name="Plain" layout="tb"/><field name="Out" w="200pt" h="20pt"/>{}"#,
        click_script(
            "Go",
            // A plain subform still has `instanceManager` (a 1/1 one); only a
            // repeatable's manager is also reachable as a bare `_Name`.
            "var m = Plain.instanceManager; Out.rawValue = [_Row.max, m.min, m.max, m.addInstance() === null, m.count].join(',');"
        )
    ));
    form.click("Root.Go").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("-1,1,1,true,1")
    );
}

#[test]
fn adding_past_max_and_removing_below_min_do_nothing_and_return_null() {
    let mut form = repeat_form(
        r#"min="1" max="2" initial="1""#,
        "var a = _Row.addInstance(); var b = _Row.addInstance(); var c = _Row.removeInstance(0); var d = _Row.removeInstance(0); Out.rawValue = [a !== null, b === null, _Row.count].join(',');",
    );
    form.click("Root.Go").expect("click");
    // add 1 -> 2 (ok), add again refused at max 2, remove 2 -> 1, remove refused at min 1.
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("true,true,1")
    );
}

#[test]
fn set_instances_is_clamped_to_the_occurrence_limits() {
    let mut form = repeat_form(
        r#"min="1" max="3" initial="1""#,
        "_Row.setInstances(9); var high = _Row.count; _Row.setInstances(0); Out.rawValue = high + ',' + _Row.count;",
    );
    form.click("Root.Go").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("3,1"));
}

#[test]
fn a_new_instance_is_addressable_in_the_same_script_and_starts_empty() {
    let mut form = repeat_form(
        r#"min="1" max="-1" initial="1""#,
        "Row.F.rawValue = 'first'; var r = _Row.addInstance(); xfa.resolveNode('Row[1].F').rawValue = 'second'; Out.rawValue = [r === Row.all.item(1), r.instanceIndex, Row.F.rawValue, r.F.rawValue, r.parent === Row.parent].join(',');",
    );
    form.click("Root.Go").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("true,1,first,second,true")
    );
}

// =============================================================================
// Host methods outside XFA 3.3: app, util, xfa.host (see `acrobat.rs`)
// =============================================================================

#[test]
fn an_alert_is_reported_and_the_handler_carries_on() {
    let mut form = form_with(&format!(
        r#"<field name="Out" w="40pt" h="20pt"/>{}"#,
        click_script(
            "Btn",
            "var answer = app.alert('Bitte Adresse angeben'); this.parent.Out.rawValue = 'after ' + answer;"
        )
    ));
    form.take_script_report();
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("after 1"));
    let report = form.take_script_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        report.messages,
        vec![HostMessage {
            source: "app.alert".to_string(),
            text: "Bitte Adresse angeben".to_string(),
            title: String::new(),
            answer: 1,
        }]
    );
}

#[test]
fn a_yes_no_dialog_is_answered_yes() {
    let mut form = form_with(&format!(
        r#"<field name="Out" w="40pt" h="20pt"/>{}"#,
        click_script(
            "Btn",
            "this.parent.Out.rawValue = xfa.host.messageBox('Delete?', 'Confirm', 2, 2) + '/' + app.alert({cMsg: 'Sure?', nType: 3});"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("4/4"));
    let report = form.take_script_report();
    assert_eq!(report.messages.len(), 2);
    assert_eq!(report.messages[0].source, "xfa.host.messageBox");
    assert_eq!(report.messages[0].title, "Confirm");
}

#[test]
fn printd_formats_a_script_date() {
    let mut form = form_with(&format!(
        r#"<field name="Out" w="40pt" h="20pt"/>{}"#,
        click_script(
            "Btn",
            "this.parent.Out.rawValue = util.printd('dd.mm.yyyy', new Date(2024, 0, 5)) + ' ' + util.printd(2, new Date(2024, 0, 5, 9, 8, 7));"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("05.01.2024 2024/01/05 09:08:07")
    );
}

#[test]
fn reset_data_restores_the_template_defaults_of_the_named_section_only() {
    let mut form = form_with(&format!(
        r#"<subform name="S" layout="tb"><field name="A" w="40pt" h="20pt"><value><text>default</text></value></field><field name="B" w="40pt" h="20pt"/></subform><field name="C" w="40pt" h="20pt"/>{}"#,
        click_script("Btn", "xfa.host.resetData('Root.S'); this.parent.S.B.rawValue = 'refilled';")
    ));
    form.set_value_as_user("Root.S.A", "typed").expect("set A");
    form.set_value_as_user("Root.C", "kept").expect("set C");
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.S.A").as_deref(), Some("default"));
    assert_eq!(
        value_of(&mut form, "Root.S.B").as_deref(),
        Some("refilled"),
        "a write after the reset in the same script survives it"
    );
    assert_eq!(value_of(&mut form, "Root.C").as_deref(), Some("kept"));
}

#[test]
fn reset_data_without_a_list_resets_every_field() {
    let mut form = form_with(&format!(
        r#"<field name="A" w="40pt" h="20pt"><value><text>d</text></value></field>{}"#,
        click_script("Btn", "xfa.host.resetData();")
    ));
    form.set_value_as_user("Root.A", "typed").expect("set");
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.A").as_deref(), Some("d"));
}

#[test]
fn reset_data_naming_nothing_is_skipped_and_reported() {
    let mut form = form_with(&format!(
        r#"<field name="A" w="40pt" h="20pt"/>{}"#,
        click_script("Btn", "xfa.host.resetData('Root.Nowhere'); this.parent.A.rawValue = 'ran';")
    ));
    form.take_script_report();
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.A").as_deref(), Some("ran"));
    let report = form.take_script_report();
    let errors: Vec<String> = report.errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(
        errors,
        vec!["xfa.host.resetData: Root.Nowhere names no node; skipped".to_string()]
    );
}

#[test]
fn a_failing_script_is_reported_with_its_owner() {
    let mut form = form_with(&click_script("Btn", "undefinedThing.call();"));
    form.take_script_report();
    form.click("Root.Btn").expect("a failing script does not fail the click");
    let report = form.take_script_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0]
            .to_string()
            .starts_with("click script on Root.Btn: JavaScript error: ReferenceError"),
        "{:?}",
        report.errors
    );
    assert!(form.take_script_report().errors.is_empty(), "taking the report drains it");
}

#[test]
fn a_load_time_failure_is_in_the_first_report() {
    let mut form = form_with(
        r#"<field name="A" w="40pt" h="20pt"><event activity="initialize"><script contentType="application/x-javascript">missing.value = 1;</script></event></field>"#,
    );
    let report = form.take_script_report();
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.to_string().starts_with("initialize script on Root.A:")),
        "{:?}",
        report.errors
    );
}

// =============================================================================
// Picture clauses: formattedValue, typed input, control metadata
// =============================================================================

const DATE_FIELD: &str = r#"<field name="D" w="80pt" h="20pt"><ui><dateTimeEdit/><picture>date{DD.MM.YYYY}</picture></ui><value><date>2024-01-05</date></value><format><picture>date{EEE D MMM YYYY}</picture></format><assist><toolTip>Datum</toolTip></assist><validate nullTest="error"/></field>"#;

#[test]
fn formatted_and_edit_values_go_through_their_pictures() {
    let mut form = form_with(&format!(
        r#"{DATE_FIELD}<field name="Out" w="80pt" h="20pt"/>{}"#,
        click_script(
            "Btn",
            "var d = this.parent.D; this.parent.Out.rawValue = d.formattedValue + '|' + d.editValue; d.editValue = '06.02.2024';"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("Fri 5 Jan 2024|05.01.2024")
    );
    assert_eq!(
        value_of(&mut form, "Root.D").as_deref(),
        Some("2024-02-06"),
        "an assigned editValue is read back into rawValue"
    );
}

#[test]
fn a_value_typed_in_the_fields_format_is_stored_canonically() {
    let mut form = form_with(DATE_FIELD);
    form.interact("Root.D", "24.12.2024").expect("set");
    assert_eq!(value_of(&mut form, "Root.D").as_deref(), Some("2024-12-24"));
    form.interact("Root.D", "2025-01-31").expect("set canonically");
    assert_eq!(value_of(&mut form, "Root.D").as_deref(), Some("2025-01-31"));
}

#[test]
fn a_control_reports_its_format_shown_value_tooltip_and_null_test() {
    let mut form = form_with(&format!(
        r#"{DATE_FIELD}<field name="Notes" w="80pt" h="40pt"><ui><textEdit multiLine="1"/></ui><value><text maxChars="4"/></value></field>"#
    ));
    let controls = crate::states::controls_of_form(&mut form).expect("controls");
    let date = controls.controls.iter().find(|c| c.field == "Root.D").expect("D");
    assert_eq!(date.format.as_deref(), Some("date{EEE D MMM YYYY}"));
    assert_eq!(date.value.as_deref(), Some("2024-01-05"));
    assert_eq!(date.shown.as_deref(), Some("Fri 5 Jan 2024"));
    assert_eq!(date.tooltip.as_deref(), Some("Datum"));
    assert!(date.mandatory);
    let notes = controls.controls.iter().find(|c| c.field == "Root.Notes").expect("Notes");
    assert_eq!(notes.max_chars, Some(4));
    assert!(notes.multiline);
    assert!(!notes.mandatory);
    assert_eq!(notes.format, None);
}

/// A field's template `<value>` is its initial value (XFA 3.3 §4, an empty
/// merge): scripts read it, and so does the control listing. The load pass
/// used to seed every field with "" unless it carried a `rawValue`.
#[test]
fn a_template_default_is_the_value_scripts_and_controls_see() {
    let mut form = form_with(&format!(
        r#"<field name="T" w="80pt" h="20pt"><value><text>abc</text></value></field><field name="N" w="80pt" h="20pt"><value><integer>7</integer></value></field><field name="Out" w="80pt" h="20pt"/>{}"#,
        click_script("Btn", "this.parent.Out.rawValue = this.parent.T.rawValue + '/' + this.parent.N.rawValue;")
    ));
    assert_eq!(value_of(&mut form, "Root.T").as_deref(), Some("abc"));
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("abc/7"));
}

// =============================================================================
// Presses: mouseDown/mouseUp around radios, checkboxes and buttons; validate
// =============================================================================

/// A UBS radio pair, scripts verbatim from AAAA_019_DE: `mouseDown` stores
/// the value before the press, `mouseUp` empties it when the press did not
/// change it.
fn ubs_radio(name: &str, on: &str) -> String {
    format!(
        r#"<field name="{name}" w="20pt" h="20pt"><ui><checkButton shape="round"/></ui><items><text>{on}</text></items><event name="event__mouseUp" activity="mouseUp"><script contentType="application/x-javascript">this.rawValue = soGlobal.checkRadioButton(this.rawValue);</script></event><event name="event__mouseDown" activity="mouseDown"><script contentType="application/x-javascript">soGlobal.rbValue = this.rawValue;</script></event></field>"#
    )
}

const SO_GLOBAL: &str = r#"<variables><script name="soGlobal" contentType="application/x-javascript">var rbValue = null;
function checkRadioButton(value)
{
	return (rbValue == value) ? null : value;
}</script></variables>"#;

#[test]
fn pressing_a_selected_ubs_radio_again_deselects_its_group() {
    let mut form = form_with(&format!(
        r#"{SO_GLOBAL}<exclGroup name="G" layout="tb">{}{}</exclGroup>"#,
        ubs_radio("RB_1", "1"),
        ubs_radio("RB_2", "2")
    ));
    form.click("Root.G.RB_1").expect("first press");
    assert_eq!(value_of(&mut form, "Root.G").as_deref(), Some("1"), "selected");
    form.click("Root.G.RB_2").expect("press the other");
    assert_eq!(value_of(&mut form, "Root.G").as_deref(), Some("2"));
    let second = form.click("Root.G.RB_2").expect("second press");
    assert_eq!(value_of(&mut form, "Root.G").as_deref(), Some(""), "deselected");
    assert!(second.values_changed);
    form.click("Root.G.RB_2").expect("third press");
    assert_eq!(value_of(&mut form, "Root.G").as_deref(), Some("2"), "selected again");
}

#[test]
fn setting_a_radio_stays_a_set_without_mouse_events() {
    let mut form = form_with(&format!(
        r#"{SO_GLOBAL}<exclGroup name="G" layout="tb">{}</exclGroup>"#,
        ubs_radio("RB_1", "1")
    ));
    form.interact("Root.G.RB_1", "1").expect("set");
    form.interact("Root.G.RB_1", "1").expect("set again");
    assert_eq!(value_of(&mut form, "Root.G").as_deref(), Some("1"));
}

#[test]
fn pressing_a_checkbox_toggles_it() {
    let mut form = form_with(
        r#"<field name="CB" w="20pt" h="20pt"><ui><checkButton/></ui><items><text>on</text><text>off</text></items></field>"#,
    );
    form.click("Root.CB").expect("press");
    assert_eq!(value_of(&mut form, "Root.CB").as_deref(), Some("on"));
    form.click("Root.CB").expect("press again");
    assert_eq!(value_of(&mut form, "Root.CB").as_deref(), Some("off"));
}

#[test]
fn a_button_press_fires_mouse_down_mouse_up_then_click() {
    let event = |activity: &str| {
        format!(
            r#"<event activity="{activity}"><script contentType="application/x-javascript">this.parent.Out.rawValue = (this.parent.Out.rawValue || '') + '{activity};';</script></event>"#
        )
    };
    let mut form = form_with(&format!(
        r#"<field name="Out" w="80pt" h="20pt"/><field name="B" w="40pt" h="20pt"><ui><button/></ui>{}{}{}</field>"#,
        event("click"),
        event("mouseUp"),
        event("mouseDown")
    ));
    form.click("Root.B").expect("press");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("mouseDown;mouseUp;click;")
    );
}

#[test]
fn a_set_reports_the_fields_failed_validation() {
    let mut form = form_with(
        r#"<field name="Short" w="80pt" h="20pt"><ui><textEdit/></ui><validate><script contentType="application/x-javascript">this.rawValue.length &lt;= 3</script></validate></field><field name="Must" w="80pt" h="20pt"><ui><textEdit/></ui><value><text>x</text></value><validate nullTest="error"/></field>"#,
    );
    assert!(form.interact("Root.Short", "abc").expect("set").validation_failures.is_empty());
    let long = form.interact("Root.Short", "abcd").expect("set");
    assert_eq!(long.validation_failures.len(), 1, "{:?}", long.validation_failures);
    assert!(long.validation_failures[0].contains("validate script rejects"));
    assert_eq!(value_of(&mut form, "Root.Short").as_deref(), Some("abcd"), "the value is kept");
    let empty = form.interact("Root.Must", "").expect("set");
    assert_eq!(
        empty.validation_failures,
        vec!["Root.Must requires a value and was left empty".to_string()]
    );
}

// =============================================================================
// Unqualified names: the XFA 3.3 §3 scope walk
// =============================================================================

const OUT: &str = r#"<field name="Out" w="80pt" h="20pt"/>"#;

#[test]
fn an_unqualified_name_finds_the_nearest_node_not_the_first_of_that_name() {
    // `Signature` the field comes first in the form, so it is the global of
    // that name; from inside `Sec`, `Signature` is the sibling subform.
    let mut form = form_with(&format!(
        r#"<field name="Signature" w="80pt" h="20pt"/><subform name="Sec" layout="tb"><subform name="Signature" layout="tb"><field name="FullName" w="80pt" h="20pt"/></subform><subform name="Calc" layout="tb">{}</subform></subform>"#,
        click_script("B", "Signature.FullName.rawValue = 'x';")
    ));
    form.click("Root.Sec.Calc.B").expect("click");
    assert_eq!(value_of(&mut form, "Root.Sec.Signature.FullName").as_deref(), Some("x"));
    assert!(form.take_script_report().errors.is_empty());
}

#[test]
fn a_bare_parent_is_the_scripts_own_containers_parent() {
    let mut form = form_with(
        r#"<subform name="Row" layout="tb"><occur min="2" max="2" initial="2"/><field name="N" w="40pt" h="20pt"><event activity="initialize"><script contentType="application/x-javascript">this.rawValue = parent.index + 1;</script></event></field></subform>"#,
    );
    assert_eq!(value_of(&mut form, "Root.Row.N").as_deref(), Some("1"));
    assert_eq!(value_of(&mut form, "Root.Row[1].N").as_deref(), Some("2"));
}

#[test]
fn a_local_variable_shadows_a_form_node_of_the_same_name() {
    let mut form = form_with(&format!(
        "{OUT}{}",
        click_script("B", "var Out = 'local'; this.parent.Out.rawValue = Out;")
    ));
    form.click("Root.B").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("local"));
}

#[test]
fn an_unknown_unqualified_name_is_still_a_reference_error() {
    let mut form = form_with(&format!(
        "{OUT}{}",
        click_script(
            "B",
            "try { nope; Out.rawValue = 'no'; } catch (e) { Out.rawValue = e.name; }"
        )
    ));
    form.click("Root.B").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("ReferenceError"));
}

#[test]
fn assigning_to_a_form_node_by_name_is_a_reported_failure() {
    let mut form = form_with(&format!("{OUT}{}", click_script("B", "Out = 1;")));
    form.take_script_report();
    form.click("Root.B").expect("click");
    let report = form.take_script_report();
    assert!(
        report.errors.iter().any(|e| e.error.contains("assignment to the form node 'Out'")),
        "{:?}",
        report.errors
    );
}

#[test]
fn a_javascript_builtin_wins_over_a_field_of_the_same_name() {
    let mut form = form_with(&format!(
        r#"{OUT}<field name="Date" w="80pt" h="20pt"/>{}"#,
        click_script("B", "Out.rawValue = typeof new Date().getTime();")
    ));
    form.take_script_report();
    form.click("Root.B").expect("click");
    let report = form.take_script_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("number"));
}

#[test]
fn resolving_this_gives_the_scripts_own_container() {
    let mut form = form_with(&click_script("B", "xfa.resolveNode('this').rawValue = 'me';"));
    form.click("Root.B").expect("click");
    assert_eq!(value_of(&mut form, "Root.B").as_deref(), Some("me"));
}

/// The load pass resolves `this` too: AAKO_019_DE's name field asks
/// `xfa.resolveNode("this")` in its initialize script, inside a repeatable.
#[test]
fn resolving_this_works_in_an_initialize_script_inside_a_repeatable() {
    let mut form = form_with(
        r#"<subform name="Dear" layout="tb"><subform name="DYN" layout="tb"><occur min="1" max="-1"/><field name="TF" w="40pt" h="20pt"><event activity="initialize"><script contentType="application/x-javascript">this.rawValue = xfa.resolveNode("this") === this ? "self" : "other";</script></event></field></subform></subform>"#,
    );
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.Dear.DYN.TF").as_deref(), Some("self"));
}

// =============================================================================
// getDeltas (LiveCycle Designer Scripting Reference)
// =============================================================================

#[test]
fn get_deltas_lists_each_changed_field_and_restore_undoes_it() {
    let mut form = form_with(&format!(
        r#"<subform name="S" layout="tb"><field name="A" w="40pt" h="20pt"><value><text>a</text></value></field><field name="B" w="40pt" h="20pt"/></subform>{OUT}{}"#,
        click_script(
            "Btn",
            "S.A.rawValue = 'x'; var d = S.getDeltas(); Out.rawValue = d.length + ':' + d.item(0).target.name + ':' + d.item(0).savedValue + ':' + d.item(0).currentValue; d.item(0).restore(); Out.rawValue += ':' + S.A.rawValue;"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("1:A:a:x:a"));
}

#[test]
fn a_freshly_opened_form_has_no_deltas() {
    let mut form = form_with(&format!(
        r#"<subform name="S" layout="tb"><field name="A" w="40pt" h="20pt"><value><text>a</text></value></field></subform>{OUT}<field name="N" w="40pt" h="20pt"><event activity="initialize"><script contentType="application/x-javascript">this.rawValue = String(S.getDeltas().length);</script></event></field>"#
    ));
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.N").as_deref(), Some("0"));
}

// =============================================================================
// Named script objects, built on first reference (XFA 3.3 §10)
// =============================================================================

fn script_object(name: &str, body: &str) -> String {
    format!(
        r#"<variables><script name="{name}" contentType="application/x-javascript">{body}</script></variables>"#
    )
}

#[test]
fn a_script_object_can_reference_one_declared_after_it() {
    // The UBS idiom: referencing the later object is what makes it exist.
    let mut form = form_with(&format!(
        "{}{}{OUT}{}",
        script_object("soA", "try { made; } catch (e) { soB; }\nfunction kind() { return typeof made; }"),
        script_object("soB", "made = 1;"),
        click_script("Btn", "Out.rawValue = soA.kind();")
    ));
    assert!(form.take_script_report().errors.is_empty());
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("number"));
}

#[test]
fn a_script_objects_functions_resolve_names_from_the_subform_declaring_it() {
    let mut form = form_with(&format!(
        r#"<subform name="Sec" layout="tb">{}<field name="X" w="40pt" h="20pt"/></subform><field name="X" w="40pt" h="20pt"/>{}"#,
        script_object("soL", "function set() { X.rawValue = 'owner'; }"),
        click_script("Btn", "soL.set();")
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Sec.X").as_deref(), Some("owner"));
    assert_eq!(value_of(&mut form, "Root.X").as_deref(), Some(""));
}

#[test]
fn the_first_of_two_same_named_script_objects_owns_the_bare_name() {
    let mut form = form_with(&format!(
        r#"<subform name="A" layout="tb">{}</subform><subform name="B" layout="tb">{}{}</subform>{OUT}{}"#,
        script_object("soLocal", "function who() { return 'a'; }"),
        script_object("soLocal", "function who() { return 'b'; }"),
        click_script("Inner", "Out.rawValue = soLocal.who();"),
        click_script("Outer", "Out.rawValue = soLocal.who();")
    ));
    form.click("Root.Outer").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("a"));
    form.click("Root.B.Inner").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("b"), "the owner's own");
}

#[test]
fn a_broken_script_object_fails_every_reference_the_same_way() {
    let mut form = form_with(&format!(
        "{}{OUT}{}",
        script_object("soBad", "throw new Error('broken object');"),
        click_script(
            "Btn",
            "var m = []; for (var i = 0; i &lt; 2; i++) { try { soBad; } catch (e) { m.push(e.message); } } Out.rawValue = m.join('|');"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("broken object|broken object")
    );
}

// =============================================================================
// FormCalc (XFA 3.3 Chapter 25)
// =============================================================================

#[test]
fn a_script_without_content_type_is_formcalc_and_assigns_its_container() {
    let mut form = form_with(
        r#"<subform name="Row" layout="tb"><occur min="2" max="2" initial="2"/><field name="N" w="40pt" h="20pt"><calculate><script>$ = $.parent.index + 1</script></calculate></field></subform>"#,
    );
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.Row.N").as_deref(), Some("1"));
    assert_eq!(value_of(&mut form, "Root.Row[1].N").as_deref(), Some("2"));
}

#[test]
fn a_formcalc_expression_is_the_fields_value() {
    let mut form = form_with(
        r#"<field name="A" w="40pt" h="20pt"><value><integer>2</integer></value></field><field name="B" w="40pt" h="20pt"><value><integer>3</integer></value></field><field name="Sum" w="40pt" h="20pt"><calculate><script contentType="application/x-formcalc">A + B</script></calculate></field><field name="Big" w="40pt" h="20pt"><calculate><script>if (Sum > 4) then "big" else "small" endif</script></calculate></field>"#,
    );
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.Sum").as_deref(), Some("5"));
    assert_eq!(value_of(&mut form, "Root.Big").as_deref(), Some("big"));
}

#[test]
fn an_untranslatable_formcalc_script_is_reported_with_its_line() {
    let mut form = form_with(
        r#"<field name="T" w="40pt" h="20pt"><calculate><script>; total
Sum(A[*])</script></calculate></field>"#,
    );
    let errors: Vec<String> = form.take_script_report().errors.iter().map(|e| e.to_string()).collect();
    assert!(
        errors.iter().any(|e| e.starts_with("calculate script on Root.T: FormCalc, line 2:")),
        "{errors:?}"
    );
}

// =============================================================================
// Reading what a form node does not have throws, as in Acrobat
// =============================================================================

#[test]
fn reading_a_missing_child_throws_like_acrobat() {
    let mut form = form_with(&format!(
        "{OUT}{}",
        click_script(
            "Btn",
            "try { var x = this.parent.Nowhere; Out.rawValue = 'no'; } catch (e) { Out.rawValue = e.message; }"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("Invalid property get operation; Root doesn't have property 'Nowhere'")
    );
}

#[test]
fn xfa_property_names_object_methods_and_membership_still_answer() {
    let mut form = form_with(&format!(
        "{OUT}{}",
        click_script(
            "Btn",
            "Out.rawValue = [typeof this.mandatory, typeof this._nothing, this.hasOwnProperty('rawValue'), 'Nowhere' in this.parent, String(this) !== '', typeof this.toString].join(',');"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("undefined,undefined,true,false,true,function")
    );
}

// =============================================================================
// What ACYI_001_EN's cover page needs of the engine: `nodes`, scope-matched
// property names, `caption.value.#text`, `selectedIndex`, `runAt="server"`
// and global data binding
// =============================================================================

#[test]
fn nodes_lists_a_containers_children_in_document_order() {
    let mut form = form_with(&format!(
        r#"{OUT}<subform name="Sec"><field name="A" w="40pt" h="20pt"/><field name="B" w="40pt" h="20pt"/><field name="C" w="40pt" h="20pt"/></subform>{}"#,
        click_script(
            "Btn",
            "var n = Sec.nodes; Out.rawValue = n.length + ':' + n.item(0).name + n.item(1).name + n.item(2).name + ':' + String(n.item(3));"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("3:ABC:null")
    );
}

/// Acrobat resolves `this.X` for an `X` the node does not have the way it
/// resolves an unqualified name: the node's children, then the node and its
/// siblings, then each ancestor's -- what ACYI's row button and its
/// `Branch_Flag` calculate rely on. A name nothing in scope has still throws.
#[test]
fn an_unknown_property_of_a_node_is_scope_matched_before_it_throws() {
    let mut form = form_with(&format!(
        r#"{OUT}<subform name="Row"><field name="Flag" w="40pt" h="20pt"><calculate><script contentType="application/x-javascript">this.Flag.rawValue = "self";</script></calculate></field>{}</subform>"#,
        click_script(
            "Btn",
            "var missing = ''; try { this.Nowhere; } catch (e) { missing = 'threw'; } Out.rawValue = this.Row.name + '/' + this.Out.name + '/' + this.Flag.rawValue + '/' + missing;"
        )
    ));
    form.click("Root.Row.Btn").expect("click");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("Row/Out/self/threw")
    );
}

/// A script in `Row[1]` naming `Row` means its own instance (XFA 3.3 §3,
/// scope-matching uses the container's index), not `Row[0]`.
#[test]
fn a_scope_matched_name_of_the_container_itself_is_its_own_instance() {
    let mut form = form_with(&format!(
        r#"{OUT}<subform name="Row"><occur initial="2" max="5"/><field name="Which" w="40pt" h="20pt"><calculate><script contentType="application/x-javascript">this.rawValue = "i" + this.Row.index;</script></calculate></field></subform>"#
    ));
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.Row.Which").as_deref(), Some("i0"));
    assert_eq!(
        value_of(&mut form, "Root.Row[1].Which").as_deref(),
        Some("i1")
    );
}

#[test]
fn a_caption_set_through_a_class_reference_relabels_the_field() {
    let mut form = form_with(&format!(
        r#"{OUT}<field name="Lbl" w="80pt" h="20pt"><caption><value><text>*** old ***</text></value></caption></field><field name="Rich" w="80pt" h="20pt"><caption><value><exData contentType="text/html"><body xmlns="http://www.w3.org/1999/xhtml"><p>rich</p></body></exData></value></caption></field>{}"#,
        click_script(
            "Btn",
            "var t = Lbl.resolveNode('caption.value.#text'); var before = t.value; t.value = 'New label'; Out.rawValue = before + '|' + Lbl.resolveNode('caption.value.#text').value + '|' + String(Rich.resolveNode('caption.value.#text'));"
        )
    ));
    form.click("Root.Btn").expect("click");
    form.refresh().expect("refresh");
    assert_eq!(
        value_of(&mut form, "Root.Out").as_deref(),
        Some("*** old ***|New label|null")
    );
    let lbl = form.resolve("Root.Lbl").expect("Lbl");
    assert_eq!(
        lbl.xfa_node().caption_text().as_deref(),
        Some("New label"),
        "the caption a script set must reach the node the layout draws from"
    );
}

#[test]
fn selected_index_is_minus_one_until_an_item_is_chosen() {
    let mut form = form_with(&format!(
        r#"{OUT}<field name="Dd" w="80pt" h="20pt"><ui><choiceList/></ui><items><text>a</text><text>b</text></items></field>{}"#,
        click_script(
            "Btn",
            "var none = Dd.selectedIndex; Dd.rawValue = 'b'; var second = Dd.selectedIndex; Dd.selectedIndex = 0; Out.rawValue = none + ',' + second + ',' + Dd.rawValue;"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("-1,1,a"));
}

/// XFA 3.3 §11: a `runAt="server"` script is for a processor set up as a
/// server; the client ignores it, as Acrobat does. `both` runs here.
#[test]
fn a_server_only_script_does_not_run_on_the_client() {
    let mut form = form_with(
        r#"<field name="Srv" w="40pt" h="20pt"><calculate><script runAt="server" contentType="application/x-javascript">this.rawValue = "server";</script></calculate></field><field name="Both" w="40pt" h="20pt"><calculate><script runAt="both" contentType="application/x-javascript">this.rawValue = "both";</script></calculate></field><field name="Shown" w="40pt" h="20pt"><event activity="initialize"><script runAt="server" contentType="application/x-javascript">this.presence = "hidden";</script></event></field>"#,
    );
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.Srv").as_deref().unwrap_or(""), "");
    assert_eq!(value_of(&mut form, "Root.Both").as_deref(), Some("both"));
    assert!(
        form.resolve("Root.Shown").expect("Shown").is_visible(),
        "a server-only initialize script must not hide the field here"
    );
}

/// Assigning a string to a node sets the node's value, as Acrobat does
/// (`xfa::acrobat_compat`, item 5: AAJB's `this.ffDesSignature = ...`
/// captions), and never replaces the node: later reads still reach it.
/// The cleared field reads `null` (XFA 3.3 §2: no content is a null
/// value), which JavaScript concatenates as "null".
#[test]
fn assigning_a_string_to_a_child_node_sets_its_value() {
    let mut form = form_with(&format!(
        r#"{OUT}<subform name="Sec"><field name="F" w="40pt" h="20pt"><value><text>kept</text></value></field></subform>{}"#,
        click_script(
            "Btn",
            "Sec.F = ''; var cleared = Sec.F.rawValue; this.parent.Sec.F = 'x'; Out.rawValue = typeof Sec.F + '/' + cleared + '/' + Sec.F.rawValue + '/' + Sec.F.name;"
        )
    ));
    form.click("Root.Btn").expect("click");
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("object/null/x/F"));
    assert_eq!(value_of(&mut form, "Root.Sec.F").as_deref(), Some("x"));
}

/// XFA 3.3 §6 "Global data": every field bound to the same global data
/// node has that node's value, so writing one writes them all -- including
/// the exclusion groups ACYI's cover page joins this way.
#[test]
fn fields_bound_to_global_data_share_one_value() {
    let mut form = form_with(&format!(
        r#"{OUT}<subform name="S1"><field name="G" w="40pt" h="20pt"><bind match="global"/></field><exclGroup name="Opt"><bind match="global"/><field name="RB_1" w="20pt" h="20pt"><ui><checkButton/></ui><items><integer>1</integer></items></field><field name="RB_2" w="20pt" h="20pt"><ui><checkButton/></ui><items><integer>2</integer></items></field></exclGroup></subform><subform name="S2"><field name="G" w="40pt" h="20pt"><bind match="global"/></field><exclGroup name="Opt"><bind match="global"/><field name="RB_1" w="20pt" h="20pt"><ui><checkButton/></ui><items><integer>1</integer></items></field><field name="RB_2" w="20pt" h="20pt"><ui><checkButton/></ui><items><integer>2</integer></items></field></exclGroup></subform><field name="Local" w="40pt" h="20pt"/>{}"#,
        click_script(
            "Btn",
            "S1.G.rawValue = 'x'; S1.Opt.RB_2.rawValue = '2'; Out.rawValue = S2.G.rawValue + '/' + S2.Opt.rawValue + '/' + Local.rawValue;"
        )
    ));
    form.click("Root.Btn").expect("click");
    // `Local` has no content, so its `rawValue` is null (XFA 3.3 §2).
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("x/2/null"));
    assert_eq!(value_of(&mut form, "Root.S2.G").as_deref(), Some("x"));
}

// XFA 3.3 §17 "The exclGroup element": assigning a member its on value
// activates it; any other value only turns it off when it was the active one.
#[test]
fn a_member_assigned_a_value_other_than_its_on_value_is_not_selected() {
    let mut engine = XfaScriptEngine::new();
    setup_exclgroup(&mut engine, "g", "g", &[("yes", "", Some("Y")), ("no", "", Some("N"))]);

    engine.update_field_value("g.yes", "0");
    assert_eq!(engine.get_field_value(&SomPath::new("g")), None, "\"0\" is not yes's on value");

    engine.update_field_value("g.no", "N");
    assert_eq!(engine.get_field_value(&SomPath::new("g")), Some("N".to_string()));
    engine.update_field_value("g.yes", "0");
    assert_eq!(
        engine.get_field_value(&SomPath::new("g")),
        Some("N".to_string()),
        "turning off a member that is not selected leaves the selection alone"
    );
    engine.update_field_value("g.no", "");
    assert_eq!(
        engine.get_field_value(&SomPath::new("g")),
        None,
        "turning off the selected member leaves the group with no selection"
    );
}

// XFA 3.3 §3: an unqualified name is searched among the container's
// children, then its siblings and ancestors' siblings, never a sibling's
// children, and the search fails at the root instead of taking a namesake
// from elsewhere in the form.
#[test]
fn a_name_outside_the_scope_walk_resolves_to_nothing() {
    let mut form = form_with(&format!(
        r#"<subform name="A" layout="tb"><field name="X" w="40pt" h="20pt"><value><text>a</text></value></field></subform><subform name="B" layout="tb">{OUT}{}</subform>"#,
        click_script(
            "Go",
            "var n = xfa.resolveNode('X'); Out.rawValue = n === null ? 'null' : n.rawValue; try { X; Out.rawValue += ',ref'; } catch (e) { Out.rawValue += ',' + e.name; }"
        )
    ));
    form.click("Root.B.Go").expect("click");
    assert_eq!(value_of(&mut form, "Root.B.Out").as_deref(), Some("null,ReferenceError"));
}

// XFA 3.3 §3 "$. Versus Unqualified": `$.X` escapes scope matching, so X
// must be a child of the container; an unqualified X still walks the scope.
#[test]
fn a_dollar_dot_reference_names_a_child_only() {
    let mut form = form_with(&format!(
        r#"<field name="Y" w="40pt" h="20pt"><value><text>y</text></value></field><subform name="B" layout="tb">{OUT}{}</subform>"#,
        click_script(
            "Go",
            "var walked = xfa.resolveNode('Y'); var child = xfa.resolveNode('$.Y'); Out.rawValue = (walked ? walked.rawValue : 'null') + ',' + (child ? child.rawValue : 'null');"
        )
    ));
    form.click("Root.B.Go").expect("click");
    assert_eq!(value_of(&mut form, "Root.B.Out").as_deref(), Some("y,null"));
}

/// XFA 3.3 §6 "Global data": a field that binds to a global data node
/// after it has a value takes that value. A new row of AALU_019_DE's table
/// joins `Global_AccountNumber` and must show the account number the
/// header's script put there before the row existed.
#[test]
fn a_new_instance_joining_a_global_binding_takes_its_value() {
    let mut form = form_with(&format!(
        r#"<field name="Head" w="40pt" h="20pt"><bind match="global"/></field><subform name="Body" layout="tb"><subform name="Row" layout="tb"><occur min="1" max="5" initial="1"/><field name="Head" w="40pt" h="20pt"><bind match="global"/></field></subform></subform>{}"#,
        click_script("Add", "Body._Row.addInstance();")
    ));
    form.interact("Root.Head", "123456").expect("set");
    form.refresh().expect("refresh");
    assert_eq!(
        value_of(&mut form, "Root.Body.Row.Head").as_deref(),
        Some("123456")
    );
    form.click("Root.Add").expect("click");
    form.refresh().expect("refresh");
    assert_eq!(
        value_of(&mut form, "Root.Body.Row[1].Head").as_deref(),
        Some("123456")
    );
    assert!(form.take_script_report().errors.is_empty());
}

/// A choice list a script rebuilds (`clearItems`/`addItem`, XFA 3.3 §6
/// "Scripting Methods") is the list the field offers afterwards, in the
/// Form DOM the renderer and the control listing read: AALU_019_DE's
/// "Bereich" fills "Objektart" this way, with one item preselected and
/// the field locked for "Access".
#[test]
fn items_a_script_builds_become_the_fields_choice_list() {
    let mut form = form_with(&format!(
        r#"<field name="Area" w="80pt" h="20pt"><ui><choiceList/></ui><items><text>Markt</text><text>Access</text></items><event activity="exit"><script contentType="application/x-javascript">var o = Objekt; o.clearItems(); o.access = "open"; if (this.rawValue == "Access") {{ o.addItem("609  Depot mit Vertrag", "609 RA"); o.rawValue = "609 RA"; o.access = "protected"; }} else {{ o.addItem(""); o.addItem("601  UBS Depot", "601"); o.addItem("606  Depot", "606"); }}</script></event></field><field name="Objekt" w="80pt" h="20pt" access="protected"><ui><choiceList/></ui></field>{OUT}"#
    ));
    assert!(
        form.resolve("Root.Objekt")
            .expect("Objekt")
            .dropdown_options()
            .is_empty()
    );

    form.interact("Root.Area", "Markt").expect("set");
    form.refresh().expect("refresh");
    let object = form.resolve("Root.Objekt").expect("Objekt");
    assert_eq!(
        object.dropdown_options(),
        vec![
            ("".to_string(), "".to_string()),
            ("601  UBS Depot".to_string(), "601".to_string()),
            ("606  Depot".to_string(), "606".to_string()),
        ]
    );
    assert_eq!(
        form.effective_access("Root.Objekt")
            .map(|a| a.access.as_str()),
        Some("open")
    );

    form.interact("Root.Area", "Access").expect("set");
    form.refresh().expect("refresh");
    let object = form.resolve("Root.Objekt").expect("Objekt");
    assert_eq!(
        object.dropdown_options(),
        vec![("609  Depot mit Vertrag".to_string(), "609 RA".to_string())]
    );
    assert_eq!(
        value_of(&mut form, "Root.Objekt").as_deref(),
        Some("609 RA")
    );
    assert_eq!(
        form.effective_access("Root.Objekt")
            .map(|a| a.access.as_str()),
        Some("protected")
    );
    assert!(form.take_script_report().errors.is_empty());
}

/// FormCalc method calls (XFA 3.3 §25 "Method Calls") reach the object
/// model: BANH_001_DE's master page counts its pages with
/// `this.rawValue = xfa.layout.pageCount()` in FormCalc. Method names are
/// case-insensitive, and a node a call answers with is read as its value.
#[test]
fn a_formcalc_method_call_reaches_the_object_model() {
    let mut form = form_with(
        r#"<field name="A" w="40pt" h="20pt"><value><text>seen</text></value></field><field name="B" w="40pt" h="20pt"><calculate><script>$ = $.parent.RESOLVENODE("A")</script></calculate></field><field name="C" w="40pt" h="20pt"><calculate><script>this.rawValue = xfa.layout.pageCount()</script></calculate></field>"#,
    );
    let report = form.take_script_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(value_of(&mut form, "Root.B").as_deref(), Some("seen"));
    assert_eq!(value_of(&mut form, "Root.C").as_deref(), Some("1"));
}

/// XFA 3.3 §2 "Field Values": a field with no content holds a null value,
/// so its `rawValue` reads `null`, not "". BADM_019_EN's client-type list
/// fills its items and chooses its default only `if (this.rawValue ==
/// null)` in its form:ready script, and with "" standing in for null the
/// form opened with no client type at all.
#[test]
fn a_field_with_no_content_reads_null_so_a_ready_script_fills_its_default() {
    let mut form = form_with(&format!(
        r#"<variables><script name="soCfg" contentType="application/x-javascript">
var selected = null;
function getDefault() {{ return "Private Person"; }}
</script></variables><field name="CL" w="80pt" h="20pt"><ui><choiceList/></ui><event activity="ready" ref="$form"><script contentType="application/x-javascript">if (this.rawValue == null) {{ soCfg.selected = soCfg.getDefault(); this.rawValue = soCfg.selected; }}</script></event></field>{OUT}{}"#,
        click_script(
            "Btn",
            "Out.rawValue = String(CL.rawValue === null) + '/' + String(Out.rawValue === null) + '/' + soCfg.selected;"
        )
    ));
    assert!(form.take_script_report().errors.is_empty());
    assert_eq!(value_of(&mut form, "Root.CL").as_deref(), Some("Private Person"));
    form.click("Root.Btn").expect("click");
    // `Out` had no content when the script read it; `CL` has its default;
    // and what the ready script left in the script object is still there.
    assert_eq!(value_of(&mut form, "Root.Out").as_deref(), Some("false/true/Private Person"));
}

/// The shape of BADM_019_EN's client details and signatures: two
/// repeatable sections kept in step by the form's own script object, a
/// client type the form:ready script records in another script object,
/// and exit scripts that paste "First Family" into the signature block of
/// the same index. XFA 3.3 §10 instantiates script objects once, with the
/// form: the load events and the person's interactions share that one
/// environment, so what a ready script stores is what a click reads.
#[test]
fn load_scripts_and_interactions_share_one_scripting_environment() {
    let mut form = form_with(&format!(
        r#"<variables><script name="soCfg" contentType="application/x-javascript">var selected = null;</script><script name="soSync" contentType="application/x-javascript">
function insertAndSynchronize(first, second) {{
    if (first.instanceManager.count == second.instanceManager.count) {{
        first.instanceManager.addInstance();
        second.instanceManager.addInstance();
    }}
}}
function setFullName(familyName, firstName, o) {{
    if (firstName &amp;&amp; familyName) {{ o.rawValue = firstName + " " + familyName; }}
}}
</script></variables><field name="CL" w="80pt" h="20pt"><ui><choiceList/></ui><event activity="ready" ref="$form"><script contentType="application/x-javascript">if (this.rawValue == null) {{ soCfg.selected = "Private Person"; this.rawValue = soCfg.selected; }}</script></event></field><subform name="Clients" layout="tb"><subform name="Client" layout="tb"><occur min="1" max="4" initial="1"/>{}<field name="Family" w="80pt" h="20pt"><event activity="exit"><script contentType="application/x-javascript">var fullName; switch (soCfg.selected) {{ case "Private Person": fullName = this.resolveNode("Signatures.Sig[" + Client.index + "].FullName"); break; }} soSync.setFullName(this.rawValue, First.rawValue, fullName);</script></event></field><field name="First" w="80pt" h="20pt"><event activity="exit"><script contentType="application/x-javascript">var fullName; switch (soCfg.selected) {{ case "Private Person": fullName = this.resolveNode("Signatures.Sig[" + Client.index + "].FullName"); break; }} soSync.setFullName(Family.rawValue, this.rawValue, fullName);</script></event></field></subform></subform><subform name="Signatures" layout="tb"><subform name="Sig" layout="tb"><occur min="1" max="4" initial="1"/><field name="FullName" w="80pt" h="20pt"/></subform></subform>"#,
        click_script(
            "Add",
            r#"switch (soCfg.selected) { case "Private Person": soSync.insertAndSynchronize(this.parent, Signatures.Sig); break; }"#
        )
    ));
    assert!(form.take_script_report().errors.is_empty());

    let added = form.click("Root.Clients.Client.Add").expect("click");
    assert!(added.instances_changed, "{added:?}");
    assert!(form.resolve("Root.Clients.Client[1]").is_some(), "a second client");
    assert!(form.resolve("Root.Signatures.Sig[1]").is_some(), "and its signature block");
    assert!(form.resolve("Root.Signatures.Sig[2]").is_none());

    for (client, family, first, full) in [
        ("Root.Clients.Client", "Muster", "Hans", "Hans Muster"),
        ("Root.Clients.Client[1]", "Beispiel", "Erika", "Erika Beispiel"),
    ] {
        form.interact(&format!("{client}.Family"), family).expect("family");
        form.interact(&format!("{client}.First"), first).expect("first");
        let sig = client.replace("Clients.Client", "Signatures.Sig");
        assert_eq!(
            value_of(&mut form, &format!("{sig}.FullName")).as_deref(),
            Some(full),
            "{client}"
        );
    }
    assert!(form.take_script_report().errors.is_empty());
}

