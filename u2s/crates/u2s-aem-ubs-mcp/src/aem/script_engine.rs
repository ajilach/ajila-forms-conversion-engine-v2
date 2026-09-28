//! AEM Script Engine
//!
//! Executes JavaScript scripts extracted from AEM Adaptive Forms (`fd:scripts`
//! and `fd:rules`). Uses Boa Engine to evaluate scripts and determine field
//! visibility and values.
//!
//! This engine is much simpler than the XFA script engine because AEM uses
//! flat name-based field references (no SOM paths, no exclGroups).

use std::collections::HashMap;

use boa_engine::{Context, Source};

use super::parser::AemScript;

/// Tracks the runtime state of AEM form fields after script execution.
#[derive(Debug, Clone)]
pub struct AemFieldState {
    /// Whether the field/panel is visible.
    pub visible: bool,
    /// The current field value (if set by scripts).
    pub value: Option<String>,
}

impl Default for AemFieldState {
    fn default() -> Self {
        Self {
            visible: true,
            value: None,
        }
    }
}

/// The AEM script engine evaluates `fd:scripts` JavaScript in a Boa context.
///
/// It creates a lightweight JS environment where field names resolve to
/// proxy objects with `.visible` and `.value` properties. After executing
/// all scripts, you can read back the resulting field states.
pub struct AemScriptEngine {
    context: Context,
}

impl AemScriptEngine {
    /// Create a new engine and register all known fields.
    ///
    /// `field_names` is a list of (component_name, initially_visible) pairs.
    /// `initial_values` maps component_name → initial value string.
    pub fn new(field_names: &[(String, bool)], initial_values: &HashMap<String, String>) -> Self {
        let mut context = Context::default();
        setup_environment(&mut context, field_names, initial_values);
        Self { context }
    }

    /// Execute a single script in the engine context.
    ///
    /// Errors are logged but do not propagate — AEM scripts often reference
    /// browser APIs that we don't implement.
    pub fn execute(&mut self, script: &AemScript) {
        if !script.enabled {
            return;
        }

        // Skip scripts that call browser/runtime APIs we can't simulate
        if should_skip_script(&script.content) {
            return;
        }

        let source = Source::from_bytes(script.content.as_bytes());
        if let Err(e) = self.context.eval(source) {
            log::debug!(
                "AEM script error (event={}, field={}): {}",
                script.event,
                script.field,
                e
            );
        }
    }

    /// Execute all scripts, ordered by event phase.
    ///
    /// Phase order: Initialize → Calculate → Value Commit → Visibility → other
    pub fn execute_all(&mut self, scripts: &[AemScript]) {
        let phase_order = |event: &str| -> u8 {
            match event {
                "Initialize" => 0,
                "Calculate" => 1,
                "Value Commit" => 2,
                "Visibility" => 3,
                _ => 4,
            }
        };

        let mut sorted: Vec<&AemScript> = scripts.iter().collect();
        sorted.sort_by_key(|s| phase_order(&s.event));

        for script in sorted {
            self.execute(script);
        }
    }

    /// Read back the visibility state of all fields after script execution.
    ///
    /// Returns a map of component_name → visible.
    pub fn read_visibility(&mut self) -> HashMap<String, bool> {
        let mut result = HashMap::new();

        let source = Source::from_bytes(b"JSON.stringify(_aem_fields_)");
        if let Ok(val) = self.context.eval(source)
            && let Some(s) = val.as_string()
        {
            let json_str = s.to_std_string_escaped();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str)
                && let Some(obj) = parsed.as_object()
            {
                for (name, field_obj) in obj {
                    if let Some(vis) = field_obj.get("visible").and_then(|v| v.as_bool()) {
                        result.insert(name.clone(), vis);
                    }
                }
            }
        }

        result
    }

    /// Read back the value state of all fields after script execution.
    ///
    /// Returns a map of component_name → value (only for fields whose value was set).
    pub fn read_values(&mut self) -> HashMap<String, String> {
        let mut result = HashMap::new();

        let source = Source::from_bytes(b"JSON.stringify(_aem_fields_)");
        if let Ok(val) = self.context.eval(source)
            && let Some(s) = val.as_string()
        {
            let json_str = s.to_std_string_escaped();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str)
                && let Some(obj) = parsed.as_object()
            {
                for (name, field_obj) in obj {
                    if let Some(v) = field_obj.get("value").and_then(|v| v.as_str())
                        && !v.is_empty()
                    {
                        result.insert(name.clone(), v.to_string());
                    }
                }
            }
        }

        result
    }
}

// ============================================================================
// JS environment setup
// ============================================================================

/// Set up the global JS environment for AEM script execution.
///
/// Creates:
/// - `_aem_fields_` global: a plain object keyed by component name, each with
///   `.visible` and `.value` properties
/// - For each field, a global variable with the component name that is an alias
///   to `_aem_fields_[name]`
/// - Stub objects for `window`, `guideBridge`, `guidelib`, `com`, etc.
fn setup_environment(
    context: &mut Context,
    field_names: &[(String, bool)],
    initial_values: &HashMap<String, String>,
) {
    // Build the initialization script that creates a plain-object registry
    // and aliases for each field. We use a plain JS object approach rather
    // than Proxy (Boa's Proxy support is limited) — scripts read/write
    // properties like `fieldName.visible = false` which works on plain objects.
    let mut init_js = String::from("var _aem_fields_ = {};\n");

    for (name, visible) in field_names {
        let value = initial_values
            .get(name)
            .map(|v| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")))
            .unwrap_or_else(|| "\"\"".into());
        let vis = if *visible { "true" } else { "false" };

        init_js.push_str(&format!(
            "_aem_fields_[\"{name}\"] = {{ visible: {vis}, value: {value} }};\n\
             var {name} = _aem_fields_[\"{name}\"];\n",
        ));
    }

    // Stub objects for browser/runtime APIs that scripts may reference
    init_js.push_str(
        r#"
var window = { forms: { ubs: {
    hideAFHideDor: function(obj) { if(obj) obj.visible = false; },
    showAFHideDor: function(obj) { if(obj) obj.visible = true; },
    showAFShowDor: function(obj) { if(obj) obj.visible = true; },
    hideAFShowDor: function(obj) { if(obj) obj.visible = false; },
    getFormMetadata: function() { return {}; },
    setReadonly: function() {},
}}};
var guideBridge = { submit: function(){}, validate: function(){ return true; } };
var guidelib = { util: { GuideUtil: { navigateToURL: function(){} } } };
var com = { ajila: { forms: { control: {
    messagebox: { initialize: function(){} },
    carousel: { initializeForPreview: function(){}, initializeForFill: function(){} },
    summary: { setSummaryData: function(){} },
}}}};
var guideRootPanel = _aem_fields_;
"#,
    );

    let source = Source::from_bytes(init_js.as_bytes());
    if let Err(e) = context.eval(source) {
        log::warn!("AEM script engine init error: {e}");
    }
}

/// Whether a script should be skipped because it depends on browser APIs
/// we can't meaningfully simulate.
fn should_skip_script(content: &str) -> bool {
    // Skip scripts that:
    // - Navigate to URLs (side effect)
    // - Call submit (side effect)
    // - Reference DOM elements
    let skip_patterns = [
        "navigateToURL",
        "guideBridge.submit",
        "document.getElementById",
        "document.querySelector",
        "window.open",
        "window.location",
        "alert(",
        "confirm(",
        "prompt(",
    ];

    skip_patterns.iter().any(|pat| content.contains(pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_visibility() {
        let fields = vec![
            ("PN_Panel1".to_string(), true),
            ("PN_Panel2".to_string(), true),
        ];
        let mut engine = AemScriptEngine::new(&fields, &HashMap::new());

        let script = AemScript {
            event: "Initialize".to_string(),
            content: "PN_Panel1.visible = false;".to_string(),
            field: "PN_Panel1".to_string(),
            enabled: true,
        };

        engine.execute(&script);

        let visibility = engine.read_visibility();
        assert_eq!(visibility.get("PN_Panel1"), Some(&false));
        assert_eq!(visibility.get("PN_Panel2"), Some(&true));
    }

    #[test]
    fn test_value_setting() {
        let fields = vec![("APPCode".to_string(), true)];
        let mut engine = AemScriptEngine::new(&fields, &HashMap::new());

        let script = AemScript {
            event: "Initialize".to_string(),
            content: r#"APPCode.value = "PHD";"#.to_string(),
            field: "APPCode".to_string(),
            enabled: true,
        };

        engine.execute(&script);

        let values = engine.read_values();
        assert_eq!(values.get("APPCode"), Some(&"PHD".to_string()));
    }

    #[test]
    fn test_conditional_visibility() {
        let fields = vec![
            ("APPCode".to_string(), true),
            ("PN_DDChecklist".to_string(), true),
        ];
        let mut initial_values = HashMap::new();
        initial_values.insert("APPCode".to_string(), "PHD".to_string());

        let mut engine = AemScriptEngine::new(&fields, &initial_values);

        let script = AemScript {
            event: "Initialize".to_string(),
            content: r#"if(APPCode.value == "PHD") { PN_DDChecklist.visible = false; } else { PN_DDChecklist.visible = true; }"#.to_string(),
            field: "PN_DDChecklist".to_string(),
            enabled: true,
        };

        engine.execute(&script);

        let visibility = engine.read_visibility();
        assert_eq!(visibility.get("PN_DDChecklist"), Some(&false));
    }

    #[test]
    fn test_disabled_script_skipped() {
        let fields = vec![("PN_Panel1".to_string(), true)];
        let mut engine = AemScriptEngine::new(&fields, &HashMap::new());

        let script = AemScript {
            event: "Initialize".to_string(),
            content: "PN_Panel1.visible = false;".to_string(),
            field: "PN_Panel1".to_string(),
            enabled: false,
        };

        engine.execute(&script);

        let visibility = engine.read_visibility();
        assert_eq!(visibility.get("PN_Panel1"), Some(&true));
    }

    #[test]
    fn test_ubs_hide_helper() {
        let fields = vec![("PN_Panel1".to_string(), true)];
        let mut engine = AemScriptEngine::new(&fields, &HashMap::new());

        let script = AemScript {
            event: "Initialize".to_string(),
            content: "window.forms.ubs.hideAFHideDor(PN_Panel1);".to_string(),
            field: "PN_Panel1".to_string(),
            enabled: true,
        };

        engine.execute(&script);

        let visibility = engine.read_visibility();
        assert_eq!(visibility.get("PN_Panel1"), Some(&false));
    }
}
