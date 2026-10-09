//! XFA Script Engine
//!
//! This module implements the core JavaScript execution engine for XFA forms,
//! providing XFA 3.3 spec compliance for scripting.
//!
//! ## XFA 3.3 Spec Implementation:
//! - Chapter 3: Scripting Object Model (SOM)
//! - Chapter 10: Automation Objects
//! - Chapter 11: Scripting

use super::dependency::DependencyTracker;
use super::events::{EventActivity, ScriptContentType, XfaScript};
use super::js_helpers;
use super::som::{SomPath, SomResolver};
use crate::flattened::FieldAccess;
use super::state::{FormState, Presence, SharedFormState, XfaValue};

use boa_engine::{
    Context, JsArgs, JsString, JsValue, NativeFunction, Source, js_string,
    object::{JsObject, ObjectInitializer},
    property::{Attribute, PropertyKey},
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Read a string property from a JS object, returning `None` if the property
/// is undefined, null, or cannot be converted to a string.
///
/// This replaces the repeated pattern:
/// ```ignore
/// obj.get(PropertyKey::from(js_string!("prop")), ctx)
///     .ok()
///     .filter(|v| !v.is_undefined() && !v.is_null())
///     .and_then(|v| v.to_string(ctx).ok())
///     .map(|s| s.to_std_string_escaped())
/// ```
/// What [`XfaScriptEngine::reconcile_instances`] found.
#[derive(Debug, Default)]
pub struct Reconciled {
    /// Instances that survived at a new index: `(old path, new path)`.
    pub moved: Vec<(SomPath, SomPath)>,
    /// Instances the scripts removed.
    pub removed: Vec<SomPath>,
    /// New instances still standing as placeholders: their path, and the
    /// values scripts wrote into them, by path relative to the instance.
    pub placeholders: Vec<(SomPath, HashMap<String, String>)>,
}

/// How many leading segments two paths share.
fn shared_prefix_len(a: &SomPath, b: &SomPath) -> usize {
    a.as_str()
        .split('.')
        .zip(b.as_str().split('.'))
        .take_while(|(x, y)| x == y)
        .count()
}

fn read_js_string_prop(obj: &JsObject, prop: &str, context: &mut Context) -> Option<String> {
    let val = obj
        .get(PropertyKey::from(JsString::from(prop)), context)
        .ok()?;
    if val.is_undefined() || val.is_null() {
        return None;
    }
    val.to_string(context)
        .ok()
        .map(|s| s.to_std_string_escaped())
}

/// XFA Scripting Engine with XFA 3.3 spec compliance
pub struct XfaScriptEngine {
    context: Context,
    form_state: SharedFormState,
    current_field_path: Option<SomPath>,
    /// Current script execution context path
    current_context_path: Option<SomPath>,
    /// SOM resolver for resolveNode()/resolveNodes()
    som_resolver: SomResolver,
    /// Dependency tracker for cascading calculations
    dependencies: DependencyTracker,
    /// Registered field JS objects for resolveNode results, keyed by FULL SOM path
    field_objects: HashMap<SomPath, JsObject>,
    /// Maps field NAME to list of FULL SOM paths that have that name
    field_objects_by_name: HashMap<String, Vec<SomPath>>,
    /// For each repeatable subform name, every parent that holds its
    /// instance manager `_name`: what a bare `_name` in a script is
    /// resolved against (XFA 3.3 §9: the manager is a peer of its subforms).
    instance_manager_parents: HashMap<String, Vec<SomPath>>,
    /// Maps child field names to their unique IDs in the current context
    child_name_to_id: HashMap<String, String>,
    /// Tracks the INITIAL presence value from the XFA tree for each field object
    initial_presence: HashMap<SomPath, String>,
    /// Registered `pageArea` objects, whose `index` follows the page being
    /// laid out. See [`XfaScriptEngine::set_layout_context`].
    page_area_objects: Vec<JsObject>,
    /// Registered `pageArea` objects keyed by name, so a script that reaches
    /// one through `pageSet.<name>` or `form1.pageSet.<name>` (XFA 3.3 §10,
    /// e.g. `mp.FIM_On.presence = "hidden"`) can find the same object that
    /// `set_layout_context` updates.
    page_area_objects_by_name: HashMap<String, JsObject>,
    /// Registered `pageSet` objects keyed by name.
    page_set_objects: HashMap<String, JsObject>,
}

impl XfaScriptEngine {
    pub fn new() -> Self {
        let context = Context::default();
        let form_state = Arc::new(RwLock::new(FormState::new()));

        let mut engine = XfaScriptEngine {
            context,
            form_state,
            current_field_path: None,
            current_context_path: None,
            som_resolver: SomResolver::new(),
            dependencies: DependencyTracker::new(),
            field_objects: HashMap::new(),
            field_objects_by_name: HashMap::new(),
            child_name_to_id: HashMap::new(),
            initial_presence: HashMap::new(),
            page_area_objects: Vec::new(),
            page_area_objects_by_name: HashMap::new(),
            page_set_objects: HashMap::new(),
            instance_manager_parents: HashMap::new(),
        };

        engine.setup_environment();
        engine
    }

    pub fn with_state(form_state: SharedFormState) -> Self {
        let context = Context::default();

        let mut engine = XfaScriptEngine {
            context,
            form_state,
            current_field_path: None,
            current_context_path: None,
            som_resolver: SomResolver::new(),
            dependencies: DependencyTracker::new(),
            field_objects: HashMap::new(),
            field_objects_by_name: HashMap::new(),
            child_name_to_id: HashMap::new(),
            initial_presence: HashMap::new(),
            page_area_objects: Vec::new(),
            page_area_objects_by_name: HashMap::new(),
            page_set_objects: HashMap::new(),
            instance_manager_parents: HashMap::new(),
        };

        engine.setup_environment();
        engine
    }

    fn setup_environment(&mut self) {
        self.setup_xfa_object();
        self.setup_shortcuts();
        self.setup_field_registry();
        self.setup_som_fallback();
        self.setup_console();
    }

    /// Get the rawValue of a specific field by its SOM path.
    /// Falls back to looking up by the short field name if the exact path isn't found.
    pub fn get_field_value(&mut self, path: &SomPath) -> Option<String> {
        // Try exact path first
        if let Some(obj) = self.field_objects.get(path) {
            let obj = obj.clone();
            if let Some(value) = read_js_string_prop(&obj, "rawValue", &mut self.context)
                && !value.is_empty()
            {
                return Some(value);
            }
        }
        // Fallback: try by field name
        let name = path.name().to_string();
        if let Some(paths) = self.field_objects_by_name.get(&name)
            && let Some(first_path) = paths.first().cloned()
            && let Some(obj) = self.field_objects.get(&first_path)
        {
            let obj = obj.clone();
            if let Some(value) = read_js_string_prop(&obj, "rawValue", &mut self.context)
                && !value.is_empty()
            {
                return Some(value);
            }
        }
        None
    }

    /// Get all field values as a HashMap keyed by both full SOM path and short name.
    /// This produces the same dual-keyed map that `computed_values` previously maintained,
    /// suitable for passing to `Flattened::from_xfa`.
    pub fn get_all_field_values_for_flattening(&mut self) -> HashMap<SomPath, String> {
        let mut map = HashMap::new();
        // Sort paths for deterministic iteration order. HashMap iteration is
        // non-deterministic across runs (random hash seed), and when multiple
        // fields share the same short name, the last insert wins — making the
        // result depend on iteration order.
        let mut paths: Vec<(SomPath, JsObject)> = self
            .field_objects
            .iter()
            .map(|(p, o)| (p.clone(), o.clone()))
            .collect();
        paths.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        for (path, obj) in paths {
            if let Some(value) = read_js_string_prop(&obj, "rawValue", &mut self.context) {
                // Store under full SOM path (empty strings are valid per XFA spec,
                // e.g. cleared dropdowns, deselected exclGroups)
                map.insert(path.clone(), value.clone());
                // Also store under short name for backward compat lookups
                map.insert(SomPath::new(path.name()), value);
            }
        }
        map
    }

    /// Create a global field registry that resolveNode can access
    fn setup_field_registry(&mut self) {
        // Create _xfa_fields_ global object that maps field names to their JS objects
        let registry = ObjectInitializer::new(&mut self.context).build();
        self.context
            .register_global_property(js_string!("_xfa_fields_"), registry, Attribute::all())
            .ok();

        // Create _xfa_fields_by_path_ that maps FULL SOM paths to JS objects
        let registry_by_path = ObjectInitializer::new(&mut self.context).build();
        self.context
            .register_global_property(
                js_string!("_xfa_fields_by_path_"),
                registry_by_path,
                Attribute::all(),
            )
            .ok();

        // Create _xfa_paths_by_name_ that maps field names to arrays of full paths
        let paths_by_name = ObjectInitializer::new(&mut self.context).build();
        self.context
            .register_global_property(
                js_string!("_xfa_paths_by_name_"),
                paths_by_name,
                Attribute::all(),
            )
            .ok();

        // Store current context path for relative resolution
        self.context
            .register_global_property(
                js_string!("_xfa_current_context_"),
                JsValue::from(js_string!("")),
                Attribute::all(),
            )
            .ok();

        // Create _xfa_event_scripts_ registry for execEvent():
        // Maps "{som_path}:{activity}" → script source string
        let event_scripts = ObjectInitializer::new(&mut self.context).build();
        self.context
            .register_global_property(
                js_string!("_xfa_event_scripts_"),
                event_scripts,
                Attribute::all(),
            )
            .ok();
    }

    /// Set up JavaScript helpers for SOM resolution
    fn setup_som_fallback(&mut self) {
        let helpers_js = js_helpers::get_all_helpers();
        let _ = self.execute_variable_script(&helpers_js);
    }

    /// Create the xfa root object with resolveNode/resolveNodes
    fn setup_xfa_object(&mut self) {
        let xfa = ObjectInitializer::new(&mut self.context).build();

        // Create xfa.form (Form DOM)
        let form = ObjectInitializer::new(&mut self.context).build();

        // Create xfa.datasets and xfa.datasets.data (Data DOM)
        let data = ObjectInitializer::new(&mut self.context).build();
        let datasets = ObjectInitializer::new(&mut self.context)
            .property(js_string!("data"), data.clone(), Attribute::all())
            .build();

        let template = ObjectInitializer::new(&mut self.context).build();

        // Create the layout object. `page`, `absPage` and `pageCount`/`pageSpan`
        // are what page-dependent master-page scripts ask for ("Pagina 2 di 3",
        // a barcode that encodes the page number). They read the page being
        // laid out from `_xfa_page_index_` / `_xfa_page_count_`, which
        // `set_layout_context` rewrites between pages; before any layout pass
        // they say page 1 of 1, so a script that runs early still gets numbers
        // rather than an exception.
        let page_fn = NativeFunction::from_fn_ptr(|_this, _args, context| {
            Ok(JsValue::from(
                read_page_global(context, "_xfa_page_index_") + 1,
            ))
        });
        let abs_page_fn = NativeFunction::from_fn_ptr(|_this, _args, context| {
            Ok(JsValue::from(
                read_page_global(context, "_xfa_page_index_") + 1,
            ))
        });
        let page_count_fn = NativeFunction::from_fn_ptr(|_this, _args, context| {
            Ok(JsValue::from(read_page_global(context, "_xfa_page_count_")))
        });
        let page_span_fn = NativeFunction::from_fn_ptr(|_this, _args, context| {
            Ok(JsValue::from(read_page_global(context, "_xfa_page_count_")))
        });
        let relayout_fn =
            NativeFunction::from_fn_ptr(|_this, _args, _context| Ok(JsValue::undefined()));
        let layout = ObjectInitializer::new(&mut self.context)
            .function(page_fn, js_string!("page"), 1)
            .function(abs_page_fn, js_string!("absPage"), 1)
            .function(page_count_fn, js_string!("pageCount"), 0)
            .function(page_span_fn, js_string!("pageSpan"), 1)
            .function(relayout_fn, js_string!("relayout"), 0)
            .build();

        let host = self.create_host_object();

        // Create event object with all XFA 3.3 spec §10 pp.398-404 properties.
        // Writable: cancelAction, change, selStart, selEnd.
        // Read-only: all others.
        let event = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("name"),
                JsValue::from(js_string!("")),
                Attribute::all(),
            )
            .property(js_string!("target"), JsValue::null(), Attribute::all())
            .property(
                js_string!("cancelAction"),
                JsValue::from(false),
                Attribute::all(),
            )
            .property(
                js_string!("change"),
                JsValue::from(js_string!("")),
                Attribute::all(),
            )
            .property(
                js_string!("commitKey"),
                JsValue::from(0),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("fullText"),
                JsValue::from(js_string!("")),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("keyDown"),
                JsValue::from(false),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("modifier"),
                JsValue::from(false),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("newContentType"),
                JsValue::from(js_string!("")),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("newText"),
                JsValue::from(js_string!("")),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("prevContentType"),
                JsValue::from(js_string!("")),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("prevText"),
                JsValue::from(js_string!("")),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(
                js_string!("reenter"),
                JsValue::from(false),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .property(js_string!("selEnd"), JsValue::from(0), Attribute::all())
            .property(js_string!("selStart"), JsValue::from(0), Attribute::all())
            .property(
                js_string!("shift"),
                JsValue::from(false),
                Attribute::CONFIGURABLE | Attribute::ENUMERABLE,
            )
            .build();

        xfa.set(
            PropertyKey::from(js_string!("form")),
            form,
            false,
            &mut self.context,
        )
        .ok();
        xfa.set(
            PropertyKey::from(js_string!("datasets")),
            datasets,
            false,
            &mut self.context,
        )
        .ok();
        xfa.set(
            PropertyKey::from(js_string!("data")),
            data.clone(),
            false,
            &mut self.context,
        )
        .ok();
        xfa.set(
            PropertyKey::from(js_string!("template")),
            template,
            false,
            &mut self.context,
        )
        .ok();
        xfa.set(
            PropertyKey::from(js_string!("layout")),
            layout,
            false,
            &mut self.context,
        )
        .ok();
        xfa.set(
            PropertyKey::from(js_string!("host")),
            host,
            false,
            &mut self.context,
        )
        .ok();
        xfa.set(
            PropertyKey::from(js_string!("event")),
            event,
            false,
            &mut self.context,
        )
        .ok();

        // The name-based lookups. `xfa.resolveNode`/`resolveNodes` themselves
        // are defined in `js_helpers::XFA_SOM_WALKER`, which resolves against
        // the Form DOM's parent/child links first (XFA 3.3 §3) and falls back
        // to these only for syntax the walker does not handle or names the
        // object tree does not reach (script objects, variables).
        let resolve_node_fn = NativeFunction::from_fn_ptr(Self::resolve_node_impl);
        xfa.set(
            PropertyKey::from(js_string!("_resolveNodeByName")),
            resolve_node_fn.to_js_function(self.context.realm()),
            false,
            &mut self.context,
        )
        .ok();

        // Add resolveNodes function (XFA 3.3 spec §3 pp.106-107)
        // Returns a JsArray of all matching nodes for a SOM expression.
        let resolve_nodes_fn = NativeFunction::from_fn_ptr(Self::resolve_nodes_impl);
        xfa.set(
            PropertyKey::from(js_string!("_resolveNodesByName")),
            resolve_nodes_fn.to_js_function(self.context.realm()),
            false,
            &mut self.context,
        )
        .ok();

        self.context
            .register_global_property(js_string!("xfa"), xfa, Attribute::all())
            .ok();
    }

    /// Get the path-by-path registry (`_xfa_fields_by_path_`) object.
    fn get_path_registry(context: &mut Context) -> Option<JsObject> {
        context
            .global_object()
            .get(
                PropertyKey::from(js_string!("_xfa_fields_by_path_")),
                context,
            )
            .ok()
            .and_then(|v| v.as_object().cloned())
    }

    /// Look up a field object from the path registry, returning `None` if
    /// undefined or null.
    fn lookup_field_by_path(
        registry: &JsObject,
        path: &str,
        context: &mut Context,
    ) -> Option<JsValue> {
        registry
            .get(PropertyKey::from(JsString::from(path)), context)
            .ok()
            .filter(|v| !v.is_undefined() && !v.is_null())
    }

    /// Read all string paths stored in a `_xfa_paths_by_name_[name]` JS array.
    fn collect_paths_for_name(name: &str, context: &mut Context) -> Vec<String> {
        let Ok(paths_by_name) = context.global_object().get(
            PropertyKey::from(js_string!("_xfa_paths_by_name_")),
            context,
        ) else {
            return Vec::new();
        };
        let Some(paths_obj) = paths_by_name.as_object() else {
            return Vec::new();
        };
        let Ok(paths_array) = paths_obj.get(PropertyKey::from(JsString::from(name)), context)
        else {
            return Vec::new();
        };
        if paths_array.is_undefined() || paths_array.is_null() {
            return Vec::new();
        }
        let Some(arr_obj) = paths_array.as_object() else {
            return Vec::new();
        };

        let length = arr_obj
            .get(PropertyKey::from(js_string!("length")), context)
            .ok()
            .and_then(|v| v.to_number(context).ok())
            .unwrap_or(0.0) as usize;

        let mut paths = Vec::with_capacity(length);
        for i in 0..length {
            if let Ok(path_val) = arr_obj.get(PropertyKey::from(i), context)
                && let Ok(path_str) = path_val.to_string(context)
            {
                paths.push(path_str.to_std_string_escaped());
            }
        }
        paths
    }

    /// Push all field objects matching the given paths into `result_array`.
    fn push_fields_for_paths(
        result_array: &boa_engine::object::builtins::JsArray,
        registry: &JsObject,
        paths: &[String],
        context: &mut Context,
    ) {
        for p in paths {
            if let Some(field_obj) = Self::lookup_field_by_path(registry, p, context) {
                result_array.push(field_obj, context).ok();
            }
        }
    }

    /// Implementation of resolveNode for the JavaScript environment
    fn resolve_node_impl(
        _this: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> boa_engine::JsResult<JsValue> {
        let expr = args.get_or_undefined(0).to_string(context)?;
        let expr_str = expr.to_std_string_escaped();

        // If the expression is a full path (contains dots), try direct lookup first
        if expr_str.contains('.') {
            if let Some(registry) = Self::get_path_registry(context)
                && let Some(field_obj) = Self::lookup_field_by_path(&registry, &expr_str, context)
            {
                return Ok(field_obj);
            }
        }

        // Extract the field name from the expression (last component)
        let field_name = expr_str.rsplit('.').next().unwrap_or(&expr_str);

        // Get the current execution context path
        let current_context = context
            .global_object()
            .get(
                PropertyKey::from(js_string!("_xfa_current_context_")),
                context,
            )
            .ok()
            .and_then(|v| v.to_string(context).ok())
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default();

        // Look up all paths that have this field name and find best match
        let all_paths = Self::collect_paths_for_name(field_name, context);
        if !all_paths.is_empty() {
            let best_path = Self::find_best_path_for_context(&all_paths, &current_context);
            if !best_path.is_empty() {
                if let Some(registry) = Self::get_path_registry(context)
                    && let Some(field_obj) =
                        Self::lookup_field_by_path(&registry, &best_path, context)
                {
                    return Ok(field_obj);
                }
            }
        }

        // Fallback: look up in the legacy _xfa_fields_ registry
        if let Ok(registry) = context
            .global_object()
            .get(PropertyKey::from(js_string!("_xfa_fields_")), context)
            && let Some(registry_obj) = registry.as_object()
            && let Ok(field_obj) =
                registry_obj.get(PropertyKey::from(JsString::from(field_name)), context)
            && !field_obj.is_undefined()
            && !field_obj.is_null()
        {
            return Ok(field_obj);
        }

        // Also try looking up as a global (for backward compatibility)
        if let Ok(global_field) = context
            .global_object()
            .get(PropertyKey::from(JsString::from(field_name)), context)
            && !global_field.is_undefined()
            && !global_field.is_null()
            && let Some(obj) = global_field.as_object()
            && let Ok(raw) = obj.get(PropertyKey::from(js_string!("rawValue")), context)
            && !raw.is_undefined()
        {
            return Ok(global_field);
        }

        // Return null if not found
        Ok(JsValue::null())
    }

    /// Implementation of resolveNodes for the JavaScript environment.
    /// Per XFA 3.3 §3 pp.106-107: returns a JsArray of all matching nodes
    /// for a SOM expression, sorted in document order.
    fn resolve_nodes_impl(
        _this: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> boa_engine::JsResult<JsValue> {
        let expr = args.get_or_undefined(0).to_string(context)?;
        let expr_str = expr.to_std_string_escaped();

        let result_array = boa_engine::object::builtins::JsArray::new(context);

        let Some(registry_obj) = Self::get_path_registry(context) else {
            return Ok(JsValue::from(result_array));
        };

        // Extract the field name (last component) from the expression
        let field_name = expr_str.rsplit('.').next().unwrap_or(&expr_str);

        // Handle indexed expressions: Name[*] or Name[n]
        if let Some(bracket_pos) = field_name.find('[') {
            let base_name = &field_name[..bracket_pos];
            let index_part = &field_name[bracket_pos + 1..field_name.len() - 1];

            let all_paths = Self::collect_paths_for_name(base_name, context);
            if index_part == "*" {
                Self::push_fields_for_paths(&result_array, &registry_obj, &all_paths, context);
            } else if let Ok(idx) = index_part.parse::<usize>() {
                if let Some(p) = all_paths.get(idx) {
                    if let Some(field_obj) = Self::lookup_field_by_path(&registry_obj, p, context) {
                        result_array.push(field_obj, context).ok();
                    }
                }
            }
            return Ok(JsValue::from(result_array));
        }

        // Handle descendant accessor (..)
        if expr_str.contains("..") {
            let parts: Vec<&str> = expr_str.split("..").collect();
            if parts.len() == 2 {
                let all_paths = Self::collect_paths_for_name(parts[1], context);
                Self::push_fields_for_paths(&result_array, &registry_obj, &all_paths, context);
            }
            return Ok(JsValue::from(result_array));
        }

        // For full path expressions: try direct lookup
        if expr_str.contains('.') {
            if let Some(field_obj) = Self::lookup_field_by_path(&registry_obj, &expr_str, context) {
                result_array.push(field_obj, context).ok();
            }
            return Ok(JsValue::from(result_array));
        }

        // Simple name: return all nodes with this name
        let all_paths = Self::collect_paths_for_name(field_name, context);
        Self::push_fields_for_paths(&result_array, &registry_obj, &all_paths, context);

        Ok(JsValue::from(result_array))
    }

    /// Find the best matching path based on context
    fn find_best_path_for_context(all_paths: &[String], current_context: &str) -> String {
        if all_paths.is_empty() {
            return String::new();
        }

        if current_context.is_empty() || all_paths.len() == 1 {
            return all_paths.first().cloned().unwrap_or_default();
        }

        // Try to find a path that's a child of the current context
        if let Some(path) = all_paths
            .iter()
            .find(|p| p.starts_with(&format!("{}.", current_context)))
        {
            return path.clone();
        }

        // Try to find one that shares a common ancestor with context
        let context_parts: Vec<&str> = current_context.split('.').collect();
        all_paths
            .iter()
            .filter(|p| {
                let path_parts: Vec<&str> = p.split('.').collect();
                context_parts
                    .iter()
                    .zip(path_parts.iter())
                    .take_while(|(a, b)| a == b)
                    .count()
                    > 0
            })
            .max_by_key(|p| {
                let path_parts: Vec<&str> = p.split('.').collect();
                context_parts
                    .iter()
                    .zip(path_parts.iter())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .cloned()
            .unwrap_or_else(|| all_paths.first().cloned().unwrap_or_default())
    }

    fn create_host_object(&mut self) -> JsObject {
        let message_box = NativeFunction::from_fn_ptr(|_this, args, context| {
            let message = args.get_or_undefined(0).to_string(context)?;
            log::debug!("[XFA messageBox]: {}", message.to_std_string_escaped());
            Ok(JsValue::undefined())
        });

        let set_focus =
            NativeFunction::from_fn_ptr(|_this, _args, _context| Ok(JsValue::undefined()));

        ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("name"),
                JsValue::from(js_string!("Blueprint")),
                Attribute::READONLY,
            )
            .property(
                js_string!("version"),
                JsValue::from(js_string!("1.0")),
                Attribute::READONLY,
            )
            .function(message_box, js_string!("messageBox"), 1)
            .function(set_focus, js_string!("setFocus"), 1)
            .build()
    }

    fn setup_shortcuts(&mut self) {
        let xfa = self
            .context
            .global_object()
            .get(PropertyKey::from(js_string!("xfa")), &mut self.context)
            .unwrap_or(JsValue::undefined());

        if let Some(xfa_obj) = xfa.as_object() {
            if let Ok(form) = xfa_obj.get(PropertyKey::from(js_string!("form")), &mut self.context)
            {
                self.context
                    .register_global_property(js_string!("$form"), form, Attribute::all())
                    .ok();
            }
            if let Ok(datasets) =
                xfa_obj.get(PropertyKey::from(js_string!("datasets")), &mut self.context)
                && let Some(ds_obj) = datasets.as_object()
                && let Ok(data) =
                    ds_obj.get(PropertyKey::from(js_string!("data")), &mut self.context)
            {
                self.context
                    .register_global_property(js_string!("$data"), data, Attribute::all())
                    .ok();
            }
            if let Ok(template) =
                xfa_obj.get(PropertyKey::from(js_string!("template")), &mut self.context)
            {
                self.context
                    .register_global_property(js_string!("$template"), template, Attribute::all())
                    .ok();
            }
            if let Ok(layout) =
                xfa_obj.get(PropertyKey::from(js_string!("layout")), &mut self.context)
            {
                self.context
                    .register_global_property(js_string!("$layout"), layout, Attribute::all())
                    .ok();
            }
            if let Ok(host) = xfa_obj.get(PropertyKey::from(js_string!("host")), &mut self.context)
            {
                self.context
                    .register_global_property(js_string!("$host"), host, Attribute::all())
                    .ok();
            }
            if let Ok(event) =
                xfa_obj.get(PropertyKey::from(js_string!("event")), &mut self.context)
            {
                self.context
                    .register_global_property(js_string!("$event"), event, Attribute::all())
                    .ok();
            }
            self.context
                .register_global_property(js_string!("$xfa"), xfa, Attribute::all())
                .ok();
        }
    }

    /// Register a `console` object with no-op logging methods.
    ///
    /// Adobe Acrobat's XFA JavaScript environment provides `console.println()` for
    /// debug output to the Acrobat JavaScript console.  Some forms use it inside
    /// script objects; without a `console` stub those calls throw a TypeError that
    /// propagates through the call stack and can silently abort layout-affecting
    /// code that is wrapped in a `try/catch`.
    ///
    /// We register `console` with all common methods (`log`, `warn`, `error`,
    /// `info`, `debug`, `println`) as no-ops so that debug prints in form scripts
    /// are silently ignored instead of aborting execution.
    fn setup_console(&mut self) {
        let noop = NativeFunction::from_fn_ptr(|_this, args, context| {
            // Optionally log at trace level so developers can see the output
            let parts: Vec<String> = args
                .iter()
                .filter_map(|a| a.to_string(context).ok().map(|s| s.to_std_string_escaped()))
                .collect();
            log::trace!("[XFA console]: {}", parts.join(" "));
            Ok(JsValue::undefined())
        });

        let console = ObjectInitializer::new(&mut self.context)
            .function(noop.clone(), js_string!("log"), 0)
            .function(noop.clone(), js_string!("warn"), 0)
            .function(noop.clone(), js_string!("error"), 0)
            .function(noop.clone(), js_string!("info"), 0)
            .function(noop.clone(), js_string!("debug"), 0)
            .function(noop, js_string!("println"), 0)
            .build();

        self.context
            .register_global_property(js_string!("console"), console, Attribute::all())
            .ok();
    }

    /// Register a field with SOM resolver
    pub fn register_field(&mut self, path: &str, name: &str, value: &str) {
        self.register_field_with_presence(path, name, value, "visible", false);
    }

    /// Register a field with SOM resolver and explicit initial presence
    pub fn register_field_with_presence(
        &mut self,
        path: &str,
        name: &str,
        value: &str,
        initial_presence: &str,
        is_subform: bool,
    ) {
        let som_path = SomPath::new(path);
        let parent_path = som_path.parent();

        // Register in SOM resolver
        let node_type = if is_subform { "subform" } else { "field" };
        self.som_resolver
            .register_node(&som_path, name, node_type, parent_path.as_ref());

        // Store initial presence for change detection
        self.initial_presence
            .insert(som_path.clone(), initial_presence.to_string());

        // Store in form state
        {
            let mut state = self.form_state.write().unwrap();
            state.set_value(som_path.clone(), XfaValue::String(value.to_string()));
        }

        // Create JavaScript object with the actual initial presence
        let field_obj =
            self.create_field_object_with_presence(name, path, value, initial_presence, &[]);
        self.field_objects
            .insert(som_path.clone(), field_obj.clone());

        self.link_child(
            parent_path.as_ref(),
            name,
            som_path.index(),
            &field_obj,
            is_subform,
        );

        // Track name -> paths mapping for context-aware resolution
        self.field_objects_by_name
            .entry(name.to_string())
            .or_default()
            .push(som_path.clone());

        // Register globally for naked references (legacy). Only the first
        // instance: a bare name means instance 0 (XFA 3.3 §3), and a later
        // instance must not take the name over from it.
        let first_instance = som_path.is_first_instance();
        if first_instance {
            self.context
                .register_global_property(JsString::from(name), field_obj.clone(), Attribute::all())
                .ok();
        }

        // Register in _xfa_fields_ registry for resolveNode() lookups (legacy)
        if first_instance
            && let Ok(registry) = self.context.global_object().get(
                PropertyKey::from(js_string!("_xfa_fields_")),
                &mut self.context,
            )
            && let Some(registry_obj) = registry.as_object()
        {
            registry_obj
                .set(
                    PropertyKey::from(JsString::from(name)),
                    field_obj.clone(),
                    false,
                    &mut self.context,
                )
                .ok();
        }

        // Register in _xfa_fields_by_path_ for full-path lookups
        if let Ok(registry) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_fields_by_path_")),
            &mut self.context,
        ) && let Some(registry_obj) = registry.as_object()
        {
            registry_obj
                .set(
                    PropertyKey::from(JsString::from(path)),
                    field_obj.clone(),
                    false,
                    &mut self.context,
                )
                .ok();
        }

        // Register in _xfa_paths_by_name_ to map name -> array of full paths
        if let Ok(paths_by_name) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_paths_by_name_")),
            &mut self.context,
        ) && let Some(paths_obj) = paths_by_name.as_object()
        {
            // Get or create the array for this name
            let paths_array = if let Ok(existing) =
                paths_obj.get(PropertyKey::from(JsString::from(name)), &mut self.context)
            {
                if !existing.is_undefined() && !existing.is_null() {
                    if let Some(arr) = existing.as_object() {
                        arr.clone()
                    } else {
                        self.create_new_paths_array(paths_obj, name)
                    }
                } else {
                    self.create_new_paths_array(paths_obj, name)
                }
            } else {
                self.create_new_paths_array(paths_obj, name)
            };

            // Add this path to the array
            let length = paths_array
                .get(PropertyKey::from(js_string!("length")), &mut self.context)
                .ok()
                .and_then(|v| v.to_number(&mut self.context).ok())
                .unwrap_or(0.0) as u32;

            paths_array
                .set(
                    PropertyKey::from(length),
                    JsValue::from(js_string!(path)),
                    false,
                    &mut self.context,
                )
                .ok();
        }

        // Register on $form
        let xfa = self
            .context
            .global_object()
            .get(PropertyKey::from(js_string!("xfa")), &mut self.context)
            .unwrap_or(JsValue::undefined());

        if let Some(xfa_obj) = xfa.as_object()
            && let Ok(form) = xfa_obj.get(PropertyKey::from(js_string!("form")), &mut self.context)
            && let Some(form_obj) = form.as_object()
        {
            self.register_path_on_object(form_obj, path, field_obj.clone());

            // Also register without root subform prefix
            if let Some(dot_pos) = path.find('.') {
                let stripped_path = &path[dot_pos + 1..];
                self.register_path_on_object(form_obj, stripped_path, field_obj.clone());
            }
        }

        // Register first component as global
        if path.contains('.') {
            let global_obj = self.context.global_object();

            // Register the full SOM path on the global object so that
            // scripts accessing `RootSubform.Child.Field` as nested
            // properties on a global variable resolve correctly.
            self.register_path_on_object(&global_obj, path, field_obj.clone());

            if let Some(dot_pos) = path.find('.') {
                let stripped_path = &path[dot_pos + 1..];
                if stripped_path.contains('.') {
                    self.register_path_on_object(&global_obj, stripped_path, field_obj);
                }
            }
        }
    }

    fn create_new_paths_array(&mut self, paths_obj: &JsObject, name: &str) -> JsObject {
        let new_arr = boa_engine::object::builtins::JsArray::new(&mut self.context);
        let new_arr_value: JsValue = new_arr.clone().into();
        paths_obj
            .set(
                PropertyKey::from(JsString::from(name)),
                new_arr_value,
                false,
                &mut self.context,
            )
            .ok();
        new_arr.into()
    }

    /// Update the rawValue of an existing field object in the engine.
    pub fn update_field_value(&mut self, path: &str, value: &str) {
        let som_path = SomPath::new(path);

        // Update form_state by path only
        {
            let mut state = self.form_state.write().unwrap();
            state.set_value(som_path.clone(), XfaValue::String(value.to_string()));
        }

        // Update the field object's rawValue property by PATH ONLY
        if let Some(field_obj) = self.field_objects.get(&som_path) {
            field_obj
                .set(
                    PropertyKey::from(js_string!("rawValue")),
                    JsValue::from(js_string!(value)),
                    false,
                    &mut self.context,
                )
                .ok();
        }
    }

    fn create_field_object(&mut self, name: &str, path: &str, initial_value: &str) -> JsObject {
        self.create_field_object_with_presence(name, path, initial_value, "visible", &[])
    }

    #[allow(clippy::too_many_arguments)]
    fn create_field_object_with_presence(
        &mut self,
        name: &str,
        path: &str,
        initial_value: &str,
        initial_presence: &str,
        // This field's `<items>` as (display, save) pairs -- see
        // `register_xfa_node`'s `items` parameter. Empty for a subform, or a
        // field with no `<items>`.
        items: &[(String, String)],
    ) -> JsObject {
        let name_js = js_string!(name);
        let path_js = js_string!(path);

        let field = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("name"),
                JsValue::from(name_js.clone()),
                Attribute::READONLY,
            )
            // Configurable so a repeated instance can be re-keyed when an
            // instance before it is added or removed (XFA 3.3 §9); still not
            // writable from a script.
            .property(
                js_string!("somExpression"),
                JsValue::from(path_js),
                Attribute::CONFIGURABLE,
            )
            .build();

        // Set _rawValue as the internal backing property
        field
            .set(
                PropertyKey::from(js_string!("_rawValue")),
                JsValue::from(js_string!(initial_value)),
                false,
                &mut self.context,
            )
            .ok();

        // Define rawValue with getter/setter for exclGroup propagation.
        //
        // Per XFA 3.3 §4 p.196: "The field determines whether it is on or off
        // by comparing the value of the variable to its own key value."
        //
        // Getter: If this field is an exclGroup child with an _itemKey, the
        //   ON/OFF state is DERIVED at read-time by comparing the parent
        //   exclGroup's value to this field's key.  This is spec-compliant
        //   and avoids write-side propagation issues.
        // Setter: When a value is written, propagate the field's _itemKey
        //   (not the raw value) to the parent exclGroup, so the parent
        //   always stores the selected child's key.
        self.context
            .global_object()
            .set(
                PropertyKey::from(js_string!("_xfa_tmp_")),
                JsValue::from(field.clone()),
                false,
                &mut self.context,
            )
            .ok();
        let _ = self.context.eval(Source::from_bytes(
            r#"Object.defineProperty(_xfa_tmp_, 'rawValue', {
                get: function() {
                    if (this._exclGroupParent && this._itemKey !== undefined) {
                        var pv = this._exclGroupParent._rawValue;
                        if (pv !== undefined && pv !== null) {
                            // Per XFA 3.3 §17 p.714: activated member assumes its 'on' value.
                            return (String(pv) === String(this._itemKey)) ? String(this._itemKey) : (this._offValue !== undefined ? this._offValue : '');
                        }
                        return (this._offValue !== undefined ? this._offValue : '');
                    }
                    var v = this._rawValue;
                    return (v !== undefined && v !== null) ? v : '';
                },
                set: function(v) {
                    this._rawValue = v;
                    if (this._exclGroupParent) {
                        if (this._itemKey !== undefined) {
                            this._exclGroupParent._rawValue = v ? this._itemKey : '';
                        } else {
                            this._exclGroupParent._rawValue = v;
                        }
                    }
                },
                configurable: true,
                enumerable: true
            });"#,
        ));

        field
            .set(
                PropertyKey::from(js_string!("value")),
                JsValue::from(js_string!(initial_value)),
                false,
                &mut self.context,
            )
            .ok();

        field
            .set(
                PropertyKey::from(js_string!("presence")),
                JsValue::from(js_string!(initial_presence)),
                false,
                &mut self.context,
            )
            .ok();

        // Store initial presence for change detection
        field
            .set(
                PropertyKey::from(js_string!("_initialPresence")),
                JsValue::from(js_string!(initial_presence)),
                false,
                &mut self.context,
            )
            .ok();

        // ====================================================================
        // Property stub objects (Approach A — XFA 3.3 §10 Rule 1 / §10 p.395)
        // ====================================================================
        // Per XFA 3.3 §10 Example 10.13: scripts may access property sub-trees
        // like `this.border.edge.color.value` or `this.font.typeface`.
        // Create stub objects so these accesses don't error. The values are not
        // propagated to the output — this prevents JS errors only.

        // border.edge.color.value  /  border.fill.color.value
        let border_color = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("value"),
                JsValue::from(js_string!("0,0,0")),
                Attribute::all(),
            )
            .build();
        let border_fill_color = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("value"),
                JsValue::from(js_string!("255,255,255")),
                Attribute::all(),
            )
            .build();
        let border_fill = ObjectInitializer::new(&mut self.context)
            .property(js_string!("color"), border_fill_color, Attribute::all())
            .build();
        let border_edge = ObjectInitializer::new(&mut self.context)
            .property(js_string!("color"), border_color, Attribute::all())
            .property(
                js_string!("presence"),
                JsValue::from(js_string!("visible")),
                Attribute::all(),
            )
            .property(
                js_string!("thickness"),
                JsValue::from(js_string!("0.5pt")),
                Attribute::all(),
            )
            .build();
        let border_obj = ObjectInitializer::new(&mut self.context)
            .property(js_string!("edge"), border_edge, Attribute::all())
            .property(js_string!("fill"), border_fill, Attribute::all())
            .property(
                js_string!("presence"),
                JsValue::from(js_string!("visible")),
                Attribute::all(),
            )
            .build();
        field
            .set(
                PropertyKey::from(js_string!("border")),
                border_obj,
                false,
                &mut self.context,
            )
            .ok();

        // font.typeface / font.size / font.weight / font.fill.color.value
        let font_fill_color = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("value"),
                JsValue::from(js_string!("0,0,0")),
                Attribute::all(),
            )
            .build();
        let font_fill = ObjectInitializer::new(&mut self.context)
            .property(js_string!("color"), font_fill_color, Attribute::all())
            .build();
        let font_obj = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("typeface"),
                JsValue::from(js_string!("")),
                Attribute::all(),
            )
            .property(
                js_string!("size"),
                JsValue::from(js_string!("10pt")),
                Attribute::all(),
            )
            .property(
                js_string!("weight"),
                JsValue::from(js_string!("normal")),
                Attribute::all(),
            )
            .property(
                js_string!("posture"),
                JsValue::from(js_string!("normal")),
                Attribute::all(),
            )
            .property(js_string!("fill"), font_fill, Attribute::all())
            .build();
        field
            .set(
                PropertyKey::from(js_string!("font")),
                font_obj,
                false,
                &mut self.context,
            )
            .ok();

        // caption.value
        let caption_obj = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("value"),
                JsValue::from(js_string!("")),
                Attribute::all(),
            )
            .property(
                js_string!("presence"),
                JsValue::from(js_string!("visible")),
                Attribute::all(),
            )
            .build();
        field
            .set(
                PropertyKey::from(js_string!("caption")),
                caption_obj,
                false,
                &mut self.context,
            )
            .ok();

        // assist.toolTip
        let assist_obj = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("toolTip"),
                JsValue::from(js_string!("")),
                Attribute::all(),
            )
            .build();
        field
            .set(
                PropertyKey::from(js_string!("assist")),
                assist_obj,
                false,
                &mut self.context,
            )
            .ok();

        // Add execEvent() method (XFA 3.3 §10 pp.407-409)
        self.add_exec_event_method(&field);
        self.call_helper(
            "_xfa_install_node_methods_",
            &[JsValue::from(field.clone())],
        );

        // instanceIndex: 0-based index among same-named sibling instances.
        // Initially 0 for a single instance.
        field
            .set(
                PropertyKey::from(js_string!("instanceIndex")),
                JsValue::from(0),
                false,
                &mut self.context,
            )
            .ok();

        // A subform's instanceManager is attached when it is linked into its
        // parent (`link_child`, XFA 3.3 §9); fields never get one.

        // XFA 3.3 §6.16: `.all` returns a collection of all instances with
        // the same name in the same scope.  When `setInstances(N)` has been
        // called, `_instances` holds the N objects; otherwise fall back
        // to a single-element collection.
        self.add_all_property(&field);

        // XFA 3.3 §6 "Scripting Methods": a choice-list field's own
        // `addItem`/`clearItems`/`deleteItem`/`getDisplayItem`/`getSaveItem`/
        // `getItemState`/`setItemState`/`boundItem`, seeded from the
        // template's `<items>` and mutable from script the same way Designer
        // output uses them (`this.clearItems(); this.addItem(x);`).
        self.add_choice_list_methods(&field, items);

        field
    }

    /// Seed `field._items` from the template's `<items>` (as `{d, s}` pairs)
    /// and define the XFA §6 choice-list methods on it. `items` is empty for
    /// anything without a choice list, in which case the field still gets
    /// working (empty) methods rather than none at all -- a script that
    /// blindly calls `clearItems()`/`addItem()` on a plain text field must
    /// not throw, matching how these calls behave in Designer-authored forms
    /// that reuse one script across field types.
    fn add_choice_list_methods(&mut self, field: &JsObject, items: &[(String, String)]) {
        let items_array = boa_engine::object::builtins::JsArray::new(&mut self.context);
        for (display, save) in items {
            let pair = ObjectInitializer::new(&mut self.context)
                .property(js_string!("d"), JsValue::from(js_string!(display.as_str())), Attribute::all())
                .property(js_string!("s"), JsValue::from(js_string!(save.as_str())), Attribute::all())
                .build();
            items_array.push(pair, &mut self.context).ok();
        }
        field
            .set(
                PropertyKey::from(js_string!("_items")),
                JsValue::from(items_array),
                false,
                &mut self.context,
            )
            .ok();

        self.context
            .global_object()
            .set(
                PropertyKey::from(js_string!("_xfa_tmp_")),
                JsValue::from(field.clone()),
                false,
                &mut self.context,
            )
            .ok();
        let _ = self.context.eval(Source::from_bytes(
            r#"
_xfa_tmp_.addItem = function(display, save) {
    if (!this._items) this._items = [];
    var d = String(display);
    this._items.push({ d: d, s: (save !== undefined && save !== null) ? String(save) : d });
};
_xfa_tmp_.clearItems = function() { this._items = []; };
_xfa_tmp_.deleteItem = function(i) {
    if (this._items && i >= 0 && i < this._items.length) this._items.splice(i, 1);
};
_xfa_tmp_.getDisplayItem = function(i) {
    return (this._items && this._items[i]) ? this._items[i].d : '';
};
_xfa_tmp_.getSaveItem = function(i) {
    return (this._items && this._items[i]) ? this._items[i].s : '';
};
// getItemState/setItemState address an item by index, per XFA 3.3 §6:
// "whether the item at the given index is currently selected."
_xfa_tmp_.getItemState = function(i) {
    if (!this._items || !this._items[i]) return 0;
    return (String(this.rawValue) === this._items[i].s) ? 1 : 0;
};
_xfa_tmp_.setItemState = function(i, state) {
    if (this._items && this._items[i] && state) this.rawValue = this._items[i].s;
};
// boundItem: the save value bound to a given display value (falls back to
// the value unchanged when it does not match any item, per XFA 3.3 §6).
_xfa_tmp_.boundItem = function(value) {
    if (!this._items) return value;
    for (var k = 0; k < this._items.length; k++) {
        if (this._items[k].d === value) return this._items[k].s;
    }
    return value;
};
"#,
        ));
    }

    /// Add XFA `.all` collection property to a JS object (XFA 3.3 §6.16).
    ///
    /// `.all` returns a collection `{length: N, item(i)}` of every instance
    /// managed by the node's instance manager (XFA 3.3 §9).
    fn add_all_property(&mut self, obj: &JsObject) {
        self.context
            .global_object()
            .set(
                PropertyKey::from(js_string!("_xfa_tmp_")),
                JsValue::from(obj.clone()),
                false,
                &mut self.context,
            )
            .ok();
        let _ = self.context.eval(Source::from_bytes(
            r#"Object.defineProperty(_xfa_tmp_, 'all', {
                get: function() {
                    // XFA 3.3 §9: every instance this node's manager holds,
                    // or just the node for one without a manager (a field).
                    var m = this.instanceManager;
                    return _xfa_collection_(m ? m._instances.slice() : [this]);
                },
                configurable: true,
                enumerable: true
            });"#,
        ));
    }

    /// Call one of the global JS helpers from `js_helpers`. They are defined
    /// by `setup_environment`, so a missing one is an engine construction bug.
    fn call_helper(&mut self, name: &str, args: &[JsValue]) -> JsValue {
        let helper = self
            .context
            .global_object()
            .get(PropertyKey::from(JsString::from(name)), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_else(|| panic!("JS helper {name} is not defined"));
        helper
            .call(&JsValue::undefined(), args, &mut self.context)
            .unwrap_or_else(|e| panic!("JS helper {name} threw: {e}"))
    }

    /// Link `child` into its parent as the instance at `index` of its name
    /// (XFA 3.3 §3): `child.parent` points up, `parent[name]` is instance 0,
    /// and every instance stays reachable through `Name[n]` resolution. A
    /// subform also gets its instance manager (§9). A node with no parent
    /// path is a root subform, whose parent is the form model, `xfa.form`.
    fn link_child(
        &mut self,
        parent_path: Option<&SomPath>,
        name: &str,
        index: usize,
        child: &JsObject,
        is_subform: bool,
    ) {
        let parent = match parent_path {
            Some(path) => match self.field_objects.get(path) {
                Some(obj) => obj.clone(),
                // A parent that was never registered (a test registering a
                // bare leaf) has no object to link into.
                None => return,
            },
            None => self.form_root(),
        };
        self.call_helper(
            "_xfa_link_child_",
            &[
                JsValue::from(parent),
                JsValue::from(JsString::from(name)),
                JsValue::from(index as u32),
                JsValue::from(child.clone()),
                JsValue::from(is_subform),
            ],
        );
    }

    /// `xfa.form`, the Form DOM's root object.
    fn form_root(&mut self) -> JsObject {
        let xfa = self
            .context
            .global_object()
            .get(PropertyKey::from(js_string!("xfa")), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .expect("xfa is defined by setup_xfa_object");
        xfa.get(PropertyKey::from(js_string!("form")), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .expect("xfa.form is defined by setup_xfa_object")
    }

    /// Give the repeatable subform `name` under `parent_path` its declared
    /// occurrence limits (XFA 3.3 §9). Called for every repeatable,
    /// including one with no instance yet, so its manager `_name` exists and
    /// `_name.addInstance()` works from zero.
    pub fn install_instance_manager(
        &mut self,
        parent_path: &SomPath,
        name: &str,
        occur: &crate::xfa::instances::Occur,
    ) {
        let Some(parent) = self.field_objects.get(parent_path).cloned() else {
            return;
        };
        let parents = self
            .instance_manager_parents
            .entry(name.to_string())
            .or_default();
        if !parents.contains(parent_path) {
            parents.push(parent_path.clone());
        }
        self.call_helper(
            "_xfa_install_manager_",
            &[
                JsValue::from(parent),
                JsValue::from(JsString::from(parent_path.as_str())),
                JsValue::from(JsString::from(name)),
                JsValue::from(occur.min),
                JsValue::from(occur.max_attr() as f64),
            ],
        );
        // Until a script context picks the nearest one, a bare `_name` is
        // the first manager of that name in document order.
        if self.instance_manager_parents[name].len() == 1 {
            self.bind_manager_global(name, parent_path);
        }
    }

    /// Bind the global `_name` to the manager held by `parent`.
    fn bind_manager_global(&mut self, name: &str, parent: &SomPath) {
        let Some(parent_obj) = self.field_objects.get(parent).cloned() else {
            return;
        };
        let key = format!("_{name}");
        if let Ok(manager) = parent_obj.get(
            PropertyKey::from(JsString::from(key.as_str())),
            &mut self.context,
        ) && !manager.is_undefined()
        {
            self.context
                .register_global_property(JsString::from(key.as_str()), manager, Attribute::all())
                .ok();
        }
    }

    /// Bring the engine in line with the instances of `name` under `parent`
    /// as the scripts left them (XFA 3.3 §9).
    ///
    /// A surviving instance keeps its JS object -- a script-object variable
    /// holding it stays valid -- and every registry entry under it moves to
    /// its new index. A removed instance's entries are dropped. A
    /// placeholder is left in place for the caller to replace with a fully
    /// registered instance; its index and the values scripts wrote into it
    /// are returned.
    pub fn reconcile_instances(&mut self, parent: &SomPath, name: &str) -> Reconciled {
        let Some(parent_obj) = self.field_objects.get(parent).cloned() else {
            return Reconciled::default();
        };
        let state = self.call_helper(
            "_xfa_instance_state_",
            &[
                JsValue::from(parent_obj),
                JsValue::from(JsString::from(name)),
            ],
        );
        let entries = state
            .as_object()
            .cloned()
            .expect("instance state is an array");
        let length = self.js_len(&entries);

        let registered: Vec<SomPath> = self
            .field_objects
            .keys()
            .filter(|k| k.parent().as_ref() == Some(parent) && k.name() == name)
            .cloned()
            .collect();

        let mut reconciled = Reconciled::default();
        let mut survivors: Vec<SomPath> = Vec::new();
        for i in 0..length {
            let entry = entries
                .get(PropertyKey::from(i as u32), &mut self.context)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .expect("instance state entry");
            let placeholder = entry
                .get(
                    PropertyKey::from(js_string!("placeholder")),
                    &mut self.context,
                )
                .map(|v| v.to_boolean())
                .unwrap_or(false);
            let new_path = parent.child_indexed(name, i);
            if placeholder {
                let values_obj = entry
                    .get(PropertyKey::from(js_string!("values")), &mut self.context)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .expect("placeholder values");
                let mut values = HashMap::new();
                for key in values_obj
                    .own_property_keys(&mut self.context)
                    .unwrap_or_default()
                {
                    let PropertyKey::String(rel) = &key else {
                        continue;
                    };
                    let rel = rel.to_std_string_escaped();
                    if let Some(v) = read_js_string_prop(&values_obj, &rel, &mut self.context) {
                        values.insert(rel, v);
                    }
                }
                reconciled.placeholders.push((new_path, values));
            } else {
                let old_path = SomPath::new(
                    read_js_string_prop(&entry, "path", &mut self.context).unwrap_or_default(),
                );
                if old_path != new_path {
                    reconciled.moved.push((old_path.clone(), new_path));
                }
                survivors.push(old_path);
            }
        }
        reconciled.removed = registered
            .into_iter()
            .filter(|p| !survivors.contains(p))
            .collect();
        self.rekey(&reconciled.moved, &reconciled.removed);
        reconciled
    }

    fn js_len(&mut self, array: &JsObject) -> usize {
        array
            .get(PropertyKey::from(js_string!("length")), &mut self.context)
            .ok()
            .and_then(|v| v.to_number(&mut self.context).ok())
            .unwrap_or(0.0) as usize
    }

    /// Move every path-keyed registry entry under each `moved.0` to the same
    /// place under `moved.1`, and drop every entry under a `removed` path.
    /// Both canonical paths and the root-stripped aliases
    /// `register_path_on_object` adds are covered. Moves are applied all at
    /// once, so a swap (`moveInstance`) cannot collide with itself.
    fn rekey(&mut self, moved: &[(SomPath, SomPath)], removed: &[SomPath]) {
        if moved.is_empty() && removed.is_empty() {
            return;
        }
        let strip_root = |p: &SomPath| {
            p.as_str()
                .split_once('.')
                .map(|(_, rest)| SomPath::new(rest))
        };
        let mut renames: Vec<(SomPath, Option<SomPath>)> = Vec::new();
        for (old, new) in moved {
            renames.push((old.clone(), Some(new.clone())));
            if let (Some(o), Some(n)) = (strip_root(old), strip_root(new)) {
                renames.push((o, Some(n)));
            }
        }
        for gone in removed {
            renames.push((gone.clone(), None));
            if let Some(o) = strip_root(gone) {
                renames.push((o, None));
            }
        }
        let remap = |key: &SomPath| -> Option<Option<SomPath>> {
            renames
                .iter()
                .find(|(old, _)| key.starts_with(old))
                .map(|(old, new)| {
                    new.as_ref().map(|new| {
                        SomPath::new(format!("{}{}", new, &key.as_str()[old.as_str().len()..]))
                    })
                })
        };

        // Take every affected entry out first, then put the survivors back.
        let affected: Vec<SomPath> = self
            .field_objects
            .keys()
            .filter(|k| remap(k).is_some())
            .cloned()
            .collect();
        let taken: Vec<(SomPath, JsObject, Option<String>)> = affected
            .iter()
            .map(|k| {
                let obj = self.field_objects.remove(k).expect("affected key");
                let presence = self.initial_presence.remove(k);
                (k.clone(), obj, presence)
            })
            .collect();
        let taken_values: Vec<(SomPath, XfaValue)> = {
            let mut state = self.form_state.write().unwrap();
            let keys: Vec<SomPath> = state
                .values
                .keys()
                .filter(|k| remap(k).is_some())
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| state.values.remove(&k).map(|v| (k, v)))
                .collect()
        };
        let registry = Self::get_path_registry(&mut self.context);
        if let Some(registry) = &registry {
            for key in &affected {
                registry
                    .delete_property_or_throw(
                        PropertyKey::from(JsString::from(key.as_str())),
                        &mut self.context,
                    )
                    .ok();
            }
        }

        for (key, obj, presence) in taken {
            let Some(Some(new_key)) = remap(&key) else {
                continue;
            };
            let canonical = self.som_expression_of(&obj).as_ref() == Some(&key);
            if canonical {
                obj.define_property_or_throw(
                    PropertyKey::from(js_string!("somExpression")),
                    boa_engine::property::PropertyDescriptor::builder()
                        .value(JsValue::from(JsString::from(new_key.as_str())))
                        .writable(false)
                        .enumerable(false)
                        .configurable(true),
                    &mut self.context,
                )
                .ok();
                if new_key.is_first_instance() && !key.is_first_instance() {
                    // Instance 0 changed hands: the bare name is now this one.
                    let name = new_key.name().to_string();
                    self.context
                        .register_global_property(
                            JsString::from(name.as_str()),
                            obj.clone(),
                            Attribute::all(),
                        )
                        .ok();
                }
            }
            if let Some(registry) = &registry {
                registry
                    .set(
                        PropertyKey::from(JsString::from(new_key.as_str())),
                        obj.clone(),
                        false,
                        &mut self.context,
                    )
                    .ok();
            }
            if let Some(presence) = presence {
                self.initial_presence.insert(new_key.clone(), presence);
            }
            self.field_objects.insert(new_key, obj);
        }
        {
            let mut state = self.form_state.write().unwrap();
            for (key, value) in taken_values {
                if let Some(Some(new_key)) = remap(&key) {
                    state.values.insert(new_key, value);
                }
            }
        }

        let mut touched_names: Vec<String> = Vec::new();
        for (name, paths) in self.field_objects_by_name.iter_mut() {
            let before = paths.len();
            let mut changed = false;
            *paths = paths
                .drain(..)
                .filter_map(|p| match remap(&p) {
                    None => Some(p),
                    Some(new) => {
                        changed = true;
                        new
                    }
                })
                .collect();
            if changed || paths.len() != before {
                touched_names.push(name.clone());
            }
        }
        for paths in self.instance_manager_parents.values_mut() {
            *paths = paths
                .drain(..)
                .filter_map(|p| match remap(&p) {
                    None => Some(p),
                    Some(new) => new,
                })
                .collect();
        }
        for field in [&mut self.current_field_path, &mut self.current_context_path] {
            if let Some(path) = field.as_ref()
                && let Some(new) = remap(path)
            {
                *field = new;
            }
        }
        for name in touched_names {
            self.rebuild_paths_by_name(&name);
        }
    }

    fn som_expression_of(&mut self, obj: &JsObject) -> Option<SomPath> {
        read_js_string_prop(obj, "somExpression", &mut self.context).map(SomPath::new)
    }

    /// Rewrite `_xfa_paths_by_name_[name]` from `field_objects_by_name`.
    fn rebuild_paths_by_name(&mut self, name: &str) {
        let paths: Vec<JsValue> = self
            .field_objects_by_name
            .get(name)
            .map(|v| {
                v.iter()
                    .map(|p| JsValue::from(JsString::from(p.as_str())))
                    .collect()
            })
            .unwrap_or_default();
        let array = boa_engine::object::builtins::JsArray::from_iter(paths, &mut self.context);
        if let Ok(by_name) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_paths_by_name_")),
            &mut self.context,
        ) && let Some(by_name) = by_name.as_object()
        {
            by_name
                .set(
                    PropertyKey::from(JsString::from(name)),
                    array,
                    false,
                    &mut self.context,
                )
                .ok();
        }
    }

    /// Replace the engine's SOM resolver, after the Form DOM it mirrors has
    /// changed shape.
    pub fn replace_som_resolver(&mut self, resolver: SomResolver) {
        self.som_resolver = resolver;
    }

    /// Take every instance-manager change queued since the last call, in
    /// the order the scripts made them.
    pub fn drain_instance_ops(&mut self) -> Vec<crate::xfa::instances::InstanceOp> {
        let queued = self.call_helper("_xfa_drain_instance_ops_", &[]);
        let Some(list) = queued.as_object().cloned() else {
            return Vec::new();
        };
        let length = list
            .get(PropertyKey::from(js_string!("length")), &mut self.context)
            .ok()
            .and_then(|v| v.to_number(&mut self.context).ok())
            .unwrap_or(0.0) as u32;
        (0..length)
            .filter_map(|i| {
                let entry = list.get(PropertyKey::from(i), &mut self.context).ok()?;
                let entry = entry.as_object()?.clone();
                crate::xfa::instances::InstanceOp::from_js(&entry, &mut self.context)
            })
            .collect()
    }

    /// Register a path on a JS object, creating intermediate objects as needed.
    fn register_path_on_object(&mut self, root: &JsObject, path: &str, field_obj: JsObject) {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = root.clone();
        let mut current_path = String::new();

        for (i, part) in parts.iter().enumerate() {
            let (name, index) = super::som::split_segment(part);
            let key = PropertyKey::from(js_string!(name));

            // Build the current path for this component
            if current_path.is_empty() {
                current_path = part.to_string();
            } else {
                current_path = format!("{}.{}", current_path, part);
            }

            // Only instance 0 is a plain property (`Row` is `Row[0]`); a later
            // instance is reached through its parent's child links, never
            // as a property literally named `Row[1]`.
            if index > 0 {
                if i == parts.len() - 1 {
                    self.field_objects
                        .insert(SomPath::new(&current_path), field_obj.clone());
                    break;
                }
                match self.field_objects.get(&SomPath::new(&current_path)) {
                    Some(obj) => {
                        current = obj.clone();
                        continue;
                    }
                    None => break,
                }
            }

            if i == parts.len() - 1 {
                // Final component - set the actual field object
                current
                    .set(key, field_obj.clone(), false, &mut self.context)
                    .ok();

                // Store in field_objects by the shortened path
                let som_path = SomPath::new(&current_path);
                self.field_objects
                    .insert(som_path.clone(), field_obj.clone());

                // Per XFA 3.3 §3: shortened SOM paths are aliases that
                // resolve to the same object.  Mirror the field's initial
                // presence so get_all_som_presence_changes() does not
                // default to "visible" and report false positives.
                let initial_pres =
                    read_js_string_prop(&field_obj, "_initialPresence", &mut self.context)
                        .unwrap_or_else(|| "visible".to_string());
                self.initial_presence.insert(som_path.clone(), initial_pres);

                // Also track in field_objects_by_name
                self.field_objects_by_name
                    .entry(name.to_string())
                    .or_default()
                    .push(som_path);
            } else {
                let existing = current
                    .get(key.clone(), &mut self.context)
                    .unwrap_or(JsValue::undefined());

                if existing.is_undefined() {
                    let som_path = SomPath::new(&current_path);
                    let (intermediate, _reused) =
                        if let Some(existing_obj) = self.field_objects.get(&som_path) {
                            (existing_obj.clone(), true)
                        } else {
                            // Create new intermediate object with presence property
                            let new_obj = ObjectInitializer::new(&mut self.context)
                                .property(
                                    js_string!("name"),
                                    JsValue::from(js_string!(name)),
                                    Attribute::READONLY,
                                )
                                .property(
                                    js_string!("somExpression"),
                                    JsValue::from(js_string!(current_path.as_str())),
                                    Attribute::READONLY,
                                )
                                .property(
                                    js_string!("presence"),
                                    JsValue::from(js_string!("visible")),
                                    Attribute::all(),
                                )
                                .build();

                            // XFA 3.3 §6.16: `.all` on intermediates as well
                            self.add_all_property(&new_obj);
                            self.call_helper(
                                "_xfa_install_node_methods_",
                                &[JsValue::from(new_obj.clone())],
                            );

                            self.field_objects.insert(som_path.clone(), new_obj.clone());
                            self.initial_presence
                                .insert(som_path.clone(), "visible".to_string());

                            self.field_objects_by_name
                                .entry(name.to_string())
                                .or_default()
                                .push(som_path.clone());

                            (new_obj, false)
                        };

                    current
                        .set(key.clone(), intermediate.clone(), false, &mut self.context)
                        .ok();
                    current = intermediate;
                } else if let Some(obj) = existing.as_object() {
                    let som_path = SomPath::new(&current_path);
                    let in_field_objects = self.field_objects.contains_key(&som_path);

                    if !in_field_objects {
                        self.field_objects.insert(som_path.clone(), obj.clone());
                        self.initial_presence
                            .insert(som_path.clone(), "visible".to_string());
                    }

                    current = obj.clone();
                } else {
                    break;
                }
            }
        }
    }

    pub fn register_global_variable(&mut self, name: &str, value: JsObject) {
        self.context
            .register_global_property(JsString::from(name), value, Attribute::all())
            .ok();
    }

    pub fn register_translation_object(
        &mut self,
        name: &str,
        translations: HashMap<String, String>,
    ) {
        let obj = ObjectInitializer::new(&mut self.context).build();

        for (key, value) in translations {
            obj.set(
                PropertyKey::from(JsString::from(key.as_str())),
                JsValue::from(js_string!(value.as_str())),
                false,
                &mut self.context,
            )
            .ok();
        }

        self.context
            .register_global_property(JsString::from(name), obj, Attribute::all())
            .ok();
    }

    /// Record a dependency for cascading calculations
    pub fn add_dependency(&mut self, dependent_field: &SomPath, source_field: &SomPath) {
        self.dependencies
            .add_dependency(dependent_field, source_field);
    }

    /// Get fields that need recalculation when a value changes
    pub fn get_fields_to_recalculate(&self, changed_field: &SomPath) -> Vec<SomPath> {
        self.dependencies.get_dependents(changed_field)
    }

    /// Resolve a SOM expression (for use from Rust side)
    pub fn resolve_node(&self, som_expression: &str) -> Option<SomPath> {
        self.som_resolver
            .resolve_node(som_expression, self.current_field_path.as_ref())
    }

    /// Resolve a SOM expression to multiple nodes
    pub fn resolve_nodes(&self, som_expression: &str) -> Vec<SomPath> {
        self.som_resolver
            .resolve_nodes(som_expression, self.current_field_path.as_ref())
    }

    /// Resolve a field name to its full SOM path using context-aware resolution.
    pub fn resolve_field_by_name_with_context(&self, field_name: &str) -> Option<SomPath> {
        // If it's already a full path, just return it
        if field_name.contains('.') {
            let som = SomPath::new(field_name);
            if self.field_objects.contains_key(&som) {
                return Some(som);
            }
            // Try as a multi-part unqualified reference via scope walk
            if let Some(ctx) = &self.current_context_path {
                if let Some(resolved) = self.som_resolver.resolve_unqualified(field_name, ctx) {
                    return Some(resolved);
                }
            }
            return Some(som);
        }

        // Get all paths that have this field name
        let paths = self.field_objects_by_name.get(field_name)?;

        if paths.is_empty() {
            return None;
        }

        // If there's only one, return it
        if paths.len() == 1 {
            return Some(paths[0].clone());
        }

        // Multiple paths exist - use XFA 3.3 §3 pp.110-114 scope walk
        if let Some(ctx) = &self.current_context_path {
            if let Some(resolved) = self.som_resolver.resolve_unqualified(field_name, ctx) {
                // Verify the resolved path has a registered field object
                if self.field_objects.contains_key(&resolved) {
                    return Some(resolved);
                }
            }
        }

        // Fallback: use heuristic prefix matching
        let context_path = self
            .current_context_path
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_default();

        if context_path.is_empty() {
            return Some(paths[0].clone());
        }

        // Try to find a path that's a child of the current context
        for path in paths {
            let path_str = path.to_string();
            if path_str.starts_with(&format!("{}.", context_path)) {
                return Some(path.clone());
            }
        }

        // Try to find one that shares a common ancestor with context
        let context_parts: Vec<&str> = context_path.split('.').collect();
        let mut best_match: Option<(&SomPath, usize)> = None;

        for path in paths {
            let path_str = path.to_string();
            let path_parts: Vec<&str> = path_str.split('.').collect();

            let shared = context_parts
                .iter()
                .zip(path_parts.iter())
                .take_while(|(a, b)| a == b)
                .count();

            if shared > 0 {
                match &best_match {
                    Some((_, best_score)) if shared > *best_score => {
                        best_match = Some((path, shared));
                    }
                    None => {
                        best_match = Some((path, shared));
                    }
                    _ => {}
                }
            }
        }

        best_match
            .map(|(path, _)| path.clone())
            .or_else(|| Some(paths[0].clone()))
    }

    /// Get the JavaScript field object for a field, using context-aware resolution.
    pub fn get_field_object_by_name(&self, field_name: &str) -> Option<&JsObject> {
        let resolved_path = self.resolve_field_by_name_with_context(field_name)?;
        self.field_objects.get(&resolved_path)
    }

    pub fn execute_variable_script(&mut self, source: &str) -> Result<(), String> {
        match self.context.eval(Source::from_bytes(source)) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Variable script error: {}", e)),
        }
    }

    pub fn evaluate_expression(&mut self, source: &str) -> Result<String, String> {
        match self.context.eval(Source::from_bytes(source)) {
            Ok(val) => {
                let s = val
                    .to_string(&mut self.context)
                    .map(|js_str| js_str.to_std_string_escaped())
                    .unwrap_or_else(|_| "<<error>>".to_string());
                Ok(s)
            }
            Err(e) => Err(format!("Evaluation error: {}", e)),
        }
    }

    pub fn set_current_field(&mut self, path: &str, name: &str, value: &str) {
        self.current_field_path = Some(SomPath::new(path));
        self.current_context_path = Some(SomPath::new(path));

        // Update the JS global _xfa_current_context_
        self.context
            .register_global_property(
                js_string!("_xfa_current_context_"),
                JsValue::from(js_string!(path)),
                Attribute::all(),
            )
            .ok();

        // Reuse the already-registered field object so that properties like
        // _exclGroupParent (set during register_xfa_node) are preserved.
        let som_path = SomPath::new(path);
        let this_obj = if let Some(existing) = self.field_objects.get(&som_path) {
            existing.clone()
        } else {
            self.create_field_object(name, path, value)
        };
        self.context
            .register_global_property(js_string!("_xfa_this_"), this_obj, Attribute::all())
            .ok();

        // Rebind ambiguous global names to the scope-correct field.
        // Per XFA 3.3 §3 pp.110-114: when a script uses a naked name like
        // "Units", it should resolve to the field closest in scope to the
        // current context, not whichever was registered last.
        self.rebind_globals_for_context(&som_path);
    }

    /// For each field name that has multiple registrations, use the scope walk
    /// to find the contextually correct one and rebind the JS global property.
    fn rebind_globals_for_context(&mut self, context_path: &SomPath) {
        // A bare `_name` is the manager under the parent nearest the script:
        // the one sharing the longest path prefix with its context.
        let managers: Vec<(String, SomPath)> = self
            .instance_manager_parents
            .iter()
            .filter(|(_, parents)| parents.len() > 1)
            .filter_map(|(name, parents)| {
                parents
                    .iter()
                    .max_by_key(|p| shared_prefix_len(p, context_path))
                    .map(|p| (name.clone(), p.clone()))
            })
            .collect();
        for (name, parent) in managers {
            self.bind_manager_global(&name, &parent);
        }

        // Collect ambiguous names that need rebinding
        let ambiguous_names: Vec<String> = self
            .field_objects_by_name
            .iter()
            .filter(|(_, paths)| paths.len() > 1)
            .map(|(name, _)| name.clone())
            .collect();

        for field_name in &ambiguous_names {
            // Use the scope walk to find the best match
            if let Some(resolved) = self
                .som_resolver
                .resolve_unqualified(field_name, context_path)
            {
                if let Some(obj) = self.field_objects.get(&resolved) {
                    let obj_clone = obj.clone();
                    self.context
                        .register_global_property(
                            JsString::from(field_name.as_str()),
                            obj_clone,
                            Attribute::all(),
                        )
                        .ok();
                }
            }
        }
    }

    /// Update `$event` / `xfa.event` properties before executing a script.
    ///
    /// Per XFA 3.3 §10 pp.398–404: the `$event` object carries context about
    /// the event being processed. Properties are set according to the event type.
    ///
    /// For change events, `prev_value` should carry the field's value BEFORE
    /// the change so that `prevText`, `newText`, and `fullText` are correct.
    pub fn update_event_context(
        &mut self,
        activity: &EventActivity,
        target_path: &str,
        prev_value: Option<&str>,
    ) {
        let activity_name = match activity {
            EventActivity::Ready => "ready",
            EventActivity::Initialize => "initialize",
            EventActivity::Enter => "enter",
            EventActivity::Exit => "exit",
            EventActivity::Change => "change",
            EventActivity::Click => "click",
            EventActivity::Calculate => "calculate",
            EventActivity::Validate => "validate",
            EventActivity::PreSubmit => "preSubmit",
            EventActivity::PostSubmit => "postSubmit",
            EventActivity::DocReady => "docReady",
            EventActivity::IndexChange => "indexChange",
            EventActivity::Other(s) => s.as_str(),
        };

        // Get the xfa.event object
        let event_obj = self
            .context
            .global_object()
            .get(PropertyKey::from(js_string!("xfa")), &mut self.context)
            .ok()
            .and_then(|xfa| xfa.as_object().cloned())
            .and_then(|xfa_obj| {
                xfa_obj
                    .get(PropertyKey::from(js_string!("event")), &mut self.context)
                    .ok()
            })
            .and_then(|e| e.as_object().cloned());

        let Some(event) = event_obj else {
            return;
        };

        // Set event name
        event
            .set(
                PropertyKey::from(js_string!("name")),
                JsValue::from(js_string!(activity_name)),
                false,
                &mut self.context,
            )
            .ok();

        // Set target to the field/subform JS object.
        let target_som = SomPath::new(target_path);
        let target_val = self
            .field_objects
            .get(&target_som)
            .map(|obj| JsValue::from(obj.clone()))
            .unwrap_or(JsValue::null());
        event
            .set(
                PropertyKey::from(js_string!("target")),
                target_val,
                false,
                &mut self.context,
            )
            .ok();

        // Reset mutable properties to defaults
        event
            .set(
                PropertyKey::from(js_string!("cancelAction")),
                JsValue::from(false),
                false,
                &mut self.context,
            )
            .ok();
        event
            .set(
                PropertyKey::from(js_string!("change")),
                JsValue::from(js_string!("")),
                false,
                &mut self.context,
            )
            .ok();
        event
            .set(
                PropertyKey::from(js_string!("selStart")),
                JsValue::from(0),
                false,
                &mut self.context,
            )
            .ok();
        event
            .set(
                PropertyKey::from(js_string!("selEnd")),
                JsValue::from(0),
                false,
                &mut self.context,
            )
            .ok();

        // Set event-type-specific properties for change events.
        // Per XFA 3.3 §10 pp.398-404:
        //   prevText  – the field value BEFORE the change
        //   newText   – the new content being inserted / selected
        //   fullText  – the resulting complete text after the change
        let is_change = matches!(activity, EventActivity::Change);
        if is_change {
            let current_value = self.get_field_value(&target_som).unwrap_or_default();
            // If the caller captured the previous value before updating the
            // field, use it for prevText.  Otherwise fall back to the current
            // value (best-effort for callers that don't track the old value).
            let prev = prev_value.unwrap_or(&current_value);
            event
                .define_property_or_throw(
                    PropertyKey::from(js_string!("prevText")),
                    boa_engine::property::PropertyDescriptor::builder()
                        .value(JsValue::from(js_string!(prev)))
                        .configurable(true)
                        .enumerable(true)
                        .build(),
                    &mut self.context,
                )
                .ok();
            event
                .define_property_or_throw(
                    PropertyKey::from(js_string!("newText")),
                    boa_engine::property::PropertyDescriptor::builder()
                        .value(JsValue::from(js_string!(current_value.as_str())))
                        .configurable(true)
                        .enumerable(true)
                        .build(),
                    &mut self.context,
                )
                .ok();
            event
                .define_property_or_throw(
                    PropertyKey::from(js_string!("fullText")),
                    boa_engine::property::PropertyDescriptor::builder()
                        .value(JsValue::from(js_string!(current_value.as_str())))
                        .configurable(true)
                        .enumerable(true)
                        .build(),
                    &mut self.context,
                )
                .ok();
        }
    }

    /// Set up the current field context with child fields as properties of `this`.
    pub fn set_current_field_with_children(
        &mut self,
        path: &str,
        name: &str,
        value: &str,
        children: &[(String, String)],
    ) {
        self.current_field_path = Some(SomPath::new(path));
        self.current_context_path = Some(SomPath::new(path));
        self.context
            .register_global_property(
                js_string!("_xfa_current_context_"),
                JsValue::from(js_string!(path)),
                Attribute::all(),
            )
            .ok();

        let this_obj = {
            let som_path = SomPath::new(path);
            if let Some(existing) = self.field_objects.get(&som_path) {
                existing.clone()
            } else {
                self.create_field_object(name, path, value)
            }
        };

        // Track which child names map to which IDs
        self.child_name_to_id.clear();

        // Add child fields as properties of `this`
        for (child_name, child_id) in children {
            let child_path = format!("{}.{}", path, child_name);
            let child_som_path = SomPath::new(&child_path);

            // Reuse existing field object if available
            let child_obj = if let Some(existing) = self.field_objects.get(&child_som_path) {
                existing.clone()
            } else {
                let new_obj = self.create_field_object(child_name, &child_path, "");
                self.field_objects.insert(child_som_path, new_obj.clone());
                new_obj
            };

            self.child_name_to_id
                .insert(child_name.clone(), child_id.clone());

            let property_key = PropertyKey::from(JsString::from(child_name.as_str()));

            this_obj
                .define_property_or_throw(
                    property_key.clone(),
                    boa_engine::property::PropertyDescriptor::builder()
                        .value(child_obj.clone())
                        .writable(true)
                        .enumerable(true)
                        .configurable(true)
                        .build(),
                    &mut self.context,
                )
                .ok();
        }

        self.context
            .register_global_property(js_string!("_xfa_this_"), this_obj.clone(), Attribute::all())
            .ok();
    }

    /// Get the value of a child field that was set via `this.childName.rawValue = ...`
    pub fn get_child_field_value(&mut self, child_name: &str) -> Option<(String, String)> {
        let child_id = self
            .child_name_to_id
            .get(child_name)
            .cloned()
            .unwrap_or_default();

        if let Ok(this_val) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_this_")),
            &mut self.context,
        ) && let Some(this_obj) = this_val.as_object()
            && let Ok(child_val) = this_obj.get(
                PropertyKey::from(JsString::from(child_name)),
                &mut self.context,
            )
            && let Some(child_obj) = child_val.as_object()
        {
            if let Some(value) = read_js_string_prop(child_obj, "rawValue", &mut self.context) {
                return Some((child_id, value));
            }
        }

        // Fallback: check form state
        let state = self.form_state.read().ok()?;
        let child_path = SomPath::new(child_name);
        state
            .get_value(&child_path)
            .map(|v| (child_id, v.as_string()))
    }

    /// Get the value of a field from the SOM hierarchy by its full path.
    pub fn get_som_field_value(&mut self, path: &str) -> Option<String> {
        let som_path = SomPath::new(path);
        let obj = self.field_objects.get(&som_path)?;
        read_js_string_prop(obj, "rawValue", &mut self.context)
    }

    /// Get all field values from the SOM hierarchy.
    ///
    /// Returns entries keyed by short field name for backward compatibility.
    /// When multiple fields share the same short name (e.g. `RB_1` in two
    /// different exclGroups), non-empty values take priority over empty ones
    /// to avoid the non-deterministic HashMap iteration bug where two fields
    /// would overwrite each other unpredictably.
    pub fn get_all_som_field_values(&mut self) -> HashMap<String, String> {
        let mut values = HashMap::new();

        for (path, obj) in &self.field_objects {
            if let Some(value) = read_js_string_prop(obj, "rawValue", &mut self.context) {
                // Store under short name. Non-empty values take priority over
                // empty ones for the same short name, avoiding the dedup bug
                // where HashMap iteration order determined which value survived.
                let field_name = path.name();
                if value.is_empty() {
                    values.entry(field_name.to_string()).or_insert(value);
                } else {
                    values
                        .entry(field_name.to_string())
                        .and_modify(|existing| {
                            if existing.is_empty() {
                                *existing = value.clone();
                            }
                        })
                        .or_insert(value);
                }
            }
        }

        values
    }

    /// Get all field values keyed by FULL SOM path.
    ///
    /// Unlike `get_all_som_field_values()` which uses short names, this method
    /// returns entries keyed by the complete SOM path, ensuring no collisions
    /// between fields with the same leaf name in different subforms.
    pub fn get_all_som_field_values_by_path(&mut self) -> HashMap<String, String> {
        let mut values = HashMap::new();

        for (path, obj) in &self.field_objects {
            if let Some(value) = read_js_string_prop(obj, "rawValue", &mut self.context) {
                values.insert(path.to_string(), value);
            }
        }

        values
    }

    /// Every field's value under its canonical SOM path only: the path its
    /// object reports as its `somExpression`. The root-stripped aliases
    /// registration also stores (`Body.F` beside `form1.Body.F`) and bare
    /// names are left out, so each field appears exactly once.
    pub fn get_field_values_by_canonical_path(&mut self) -> HashMap<SomPath, String> {
        let entries: Vec<(SomPath, JsObject)> = self
            .field_objects
            .iter()
            .map(|(p, o)| (p.clone(), o.clone()))
            .collect();
        let mut values = HashMap::new();
        for (path, obj) in entries {
            if self.som_expression_of(&obj).as_ref() != Some(&path) {
                continue;
            }
            if let Some(value) = read_js_string_prop(&obj, "rawValue", &mut self.context) {
                values.insert(path, value);
            }
        }
        values
    }

    /// Get all presence changes from the SOM hierarchy that have been modified.
    pub fn get_all_som_presence_changes(&mut self) -> HashMap<String, String> {
        let mut changes = HashMap::new();

        for (path, obj) in &self.field_objects {
            if let Some(presence_value) = read_js_string_prop(obj, "presence", &mut self.context)
                && !presence_value.is_empty()
            {
                let initial = self
                    .initial_presence
                    .get(path)
                    .map(|s| s.as_str())
                    .unwrap_or("visible");

                if presence_value.to_lowercase() != initial.to_lowercase() {
                    changes.insert(path.to_string(), presence_value);
                }
            }
        }

        changes
    }

    /// Update the initial presence for a field so subsequent change detection
    /// correctly recognizes reverts back to the "current" baseline.
    pub fn update_initial_presence(&mut self, path: &SomPath, presence: &str) {
        self.initial_presence
            .insert(path.clone(), presence.to_string());
    }

    /// Give the object registered at `path` its `access` property (XFA 3.3
    /// §17), so a script can read it and assign it (`this.access =
    /// "protected"`). `access` is the node's own declared value; the
    /// `_initialAccess` beside it is the baseline [`Self::take_access_changes`]
    /// compares against, kept on the object itself so every alias under
    /// which the object is registered shares it.
    ///
    /// A path with no registered object is ignored: only registered
    /// containers and fields are reachable from a script at all.
    pub fn init_access(&mut self, path: &str, access: FieldAccess) {
        let Some(obj) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        for key in ["access", "_initialAccess"] {
            obj.set(
                PropertyKey::from(JsString::from(key)),
                JsValue::from(js_string!(access.as_str())),
                false,
                &mut self.context,
            )
            .ok();
        }
    }

    /// Every `access` a script has changed since the last call, under each
    /// object's canonical path (its `somExpression`), and the baseline moved
    /// to the new value so the next call reports only what changes after
    /// this one.
    ///
    /// A value that is not one of the four XFA keywords is not applied: the
    /// object's `access` is put back to its baseline and the write is logged.
    /// Reading it as the spec default, `open`, would unlock a field a script
    /// meant to lock.
    pub fn take_access_changes(&mut self) -> Vec<(SomPath, FieldAccess)> {
        let entries: Vec<(SomPath, JsObject)> = self
            .field_objects
            .iter()
            .map(|(p, o)| (p.clone(), o.clone()))
            .collect();
        let mut changes = Vec::new();
        for (path, obj) in entries {
            if self.som_expression_of(&obj).as_ref() != Some(&path) {
                continue;
            }
            let Some(current) = read_js_string_prop(&obj, "access", &mut self.context) else {
                continue;
            };
            let baseline = read_js_string_prop(&obj, "_initialAccess", &mut self.context)
                .unwrap_or_else(|| FieldAccess::Open.as_str().to_string());
            if current == baseline {
                continue;
            }
            let (key, value) = match FieldAccess::parse_strict(&current) {
                Some(access) => {
                    changes.push((path.clone(), access));
                    ("_initialAccess", access.as_str().to_string())
                }
                None => {
                    log::warn!(
                        "ignoring access = {current:?} on {path}: not one of open, \
                         nonInteractive, protected, readOnly (XFA 3.3 §17); it stays {baseline}"
                    );
                    ("access", baseline)
                }
            };
            obj.set(
                PropertyKey::from(JsString::from(key)),
                JsValue::from(js_string!(value)),
                false,
                &mut self.context,
            )
            .ok();
        }
        changes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        changes
    }

    /// Reset all registered field values and presence to match a snapshot.
    ///
    /// Reuses the existing JS objects — only updates `rawValue` + `presence`
    /// properties and the `initial_presence` baseline.  Much cheaper than
    /// clearing and rebuilding via `build_som_hierarchy_with_values` because
    /// no new JS objects are created and the Boa `Context` is reused.
    /// Tell the layout host object which page is being laid out.
    ///
    /// `xfa.layout.page()` and `xfa.layout.pageCount()` read these, so master
    /// page scripts evaluated once per page see their own page number. Also
    /// updates `index` on every registered pageArea, which is the other way a
    /// template asks the same question (`if (MP.index < 1) ...`).
    pub fn set_layout_context(&mut self, page_index: usize, page_count: usize) {
        for (name, value) in [
            ("_xfa_page_index_", page_index as i32),
            ("_xfa_page_count_", page_count.max(1) as i32),
        ] {
            // register_global_property replaces an existing property, so this
            // is also how the value is updated between pages.
            self.context
                .register_global_property(
                    js_string!(name.to_string()),
                    JsValue::from(value),
                    Attribute::all(),
                )
                .ok();
        }

        for page_area in &self.page_area_objects {
            page_area
                .set(
                    js_string!("index"),
                    JsValue::from(page_index as i32),
                    false,
                    &mut self.context,
                )
                .ok();
        }
    }

    /// Register a `pageSet` as a script object (XFA 3.3 §3 "Reference by
    /// Class": a template addresses its page sets and page areas by name, or
    /// by class name — `form1.pageSet.MP_Last` — when only one is unnamed or
    /// the caller does not care which). Without this, a script written the
    /// way Designer emits it (`UBSForms_66420.pageSet.MP_Last`) throws on
    /// `.pageSet` being undefined and the whole script — including presence
    /// writes to master-page content — is lost.
    pub fn register_page_set(&mut self, name: &str, parent_path: Option<&str>) {
        let object = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("name"),
                JsValue::from(js_string!(name.to_string())),
                Attribute::all(),
            )
            .property(
                js_string!("somExpression"),
                JsValue::from(js_string!(name.to_string())),
                Attribute::all(),
            )
            .build();

        let parent = parent_path.and_then(|p| self.field_objects.get(&SomPath::new(p)).cloned());
        if let Some(parent) = parent {
            Self::attach_child_property(&parent, name, "pageSet", &object, &mut self.context);
        } else {
            self.context
                .register_global_property(
                    js_string!(name.to_string()),
                    object.clone(),
                    Attribute::all(),
                )
                .ok();
        }
        self.page_set_objects.insert(name.to_string(), object);
    }

    /// The `pageSet` object registered under `name`, if any — so a caller can
    /// attach the `pageArea`s it contains.
    pub fn page_set_object(&self, name: &str) -> Option<JsObject> {
        self.page_set_objects.get(name).cloned()
    }

    /// Register a `pageArea` as a script object.
    ///
    /// A pageArea is not a field, but templates address it by name to ask which
    /// page they are on, so it needs an object with a `name` and an `index`.
    /// Without one the reference throws and the whole script is lost.
    ///
    /// `page_set` attaches the new object onto its enclosing pageSet object
    /// (by name, falling back to the class name `pageArea`), so a script that
    /// reaches it as `form1.pageSet.MP_Last` finds the same object whose
    /// `index` `set_layout_context` updates.
    pub fn register_page_area(&mut self, name: &str, page_set: Option<&str>) {
        let object = ObjectInitializer::new(&mut self.context)
            .property(
                js_string!("name"),
                JsValue::from(js_string!(name.to_string())),
                Attribute::all(),
            )
            .property(js_string!("index"), JsValue::from(0), Attribute::all())
            .property(
                js_string!("somExpression"),
                JsValue::from(js_string!(name.to_string())),
                Attribute::all(),
            )
            .build();

        self.context
            .register_global_property(
                js_string!(name.to_string()),
                object.clone(),
                Attribute::all(),
            )
            .ok();

        if let Some(page_set_name) = page_set
            && let Some(page_set_obj) = self.page_set_objects.get(page_set_name).cloned()
        {
            Self::attach_child_property(&page_set_obj, name, "pageArea", &object, &mut self.context);
        }

        self.page_area_objects_by_name
            .insert(name.to_string(), object.clone());
        self.page_area_objects.push(object);
    }

    /// A node placed directly inside a `pageArea` (a header, a footer, a
    /// conditional alternate) as a property of that pageArea's script object,
    /// so `mp.FIM_On.presence = "hidden"` (`mp` resolved from `pageSet.MP_Last`)
    /// reaches the same object the flattener's presence lookup later reads.
    pub fn attach_to_page_area(&mut self, page_area: &str, child_name: &str, child_path: &str) {
        let Some(page_area_obj) = self.page_area_objects_by_name.get(page_area).cloned() else {
            return;
        };
        let Some(child_obj) = self.field_objects.get(&SomPath::new(child_path)).cloned() else {
            return;
        };
        page_area_obj
            .set(
                PropertyKey::from(JsString::from(child_name)),
                child_obj,
                false,
                &mut self.context,
            )
            .ok();
    }

    /// Set `parent[name] = value`, and also `parent[class_name] = value` when
    /// `class_name` is not already taken — the "Reference by Class" fallback
    /// (XFA 3.3 §3) for the common case of exactly one child of that class.
    fn attach_child_property(
        parent: &JsObject,
        name: &str,
        class_name: &str,
        value: &JsObject,
        context: &mut Context,
    ) {
        parent
            .set(
                PropertyKey::from(JsString::from(name)),
                value.clone(),
                false,
                context,
            )
            .ok();
        let has_class = parent
            .get(PropertyKey::from(JsString::from(class_name)), context)
            .map(|v| !v.is_undefined())
            .unwrap_or(false);
        if !has_class {
            parent
                .set(
                    PropertyKey::from(JsString::from(class_name)),
                    value.clone(),
                    false,
                    context,
                )
                .ok();
        }
    }

    /// Expose the global a named `<variables><script>` object created
    /// (`wrap_script_object`) as a property of its owning subform's JS
    /// object, per XFA 3.3 §10 "Instantiation of Named Script Objects": "the
    /// contents of the object are compiled into a script object and that
    /// object is then registered with the subform". Scripts elsewhere in the
    /// document keep working through the existing global.
    pub fn attach_script_object_to_subform(&mut self, subform_path: &str, script_name: &str) {
        let Some(subform_obj) = self.field_objects.get(&SomPath::new(subform_path)).cloned()
        else {
            return;
        };
        if let Ok(global) = self
            .context
            .global_object()
            .get(PropertyKey::from(JsString::from(script_name)), &mut self.context)
            && !global.is_undefined()
        {
            subform_obj
                .set(
                    PropertyKey::from(JsString::from(script_name)),
                    global,
                    false,
                    &mut self.context,
                )
                .ok();
        }
    }

    /// Reset a master-page node's `presence` back to what the template
    /// declared, before re-running that node's initialize/calculate scripts
    /// for one page. Without this, a presence write from a previous page (or
    /// from a sibling's script, in the same evaluation) would leak into the
    /// next page's answer.
    pub fn reset_presence_to_template(&mut self, path: &str) {
        let Some(obj) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        if let Some(initial) = read_js_string_prop(&obj, "_initialPresence", &mut self.context) {
            obj.set(
                PropertyKey::from(js_string!("presence")),
                JsValue::from(js_string!(initial)),
                false,
                &mut self.context,
            )
            .ok();
        }
    }

    /// Every one of `paths` whose current `presence` differs from the
    /// template's declared value — the master-page analogue of
    /// [`Self::get_all_som_presence_changes`], but scoped to a caller-given
    /// set (the nodes inside the pageArea being evaluated) instead of every
    /// field ever registered, and read fresh rather than diffed against a
    /// baseline the document-wide pass may have already rebased (see
    /// `ScriptExecutor::execute_internal`'s `update_initial_presence` call).
    pub fn presence_vs_template(&mut self, paths: &[String]) -> Vec<(String, String)> {
        let mut changes = Vec::new();
        for path in paths {
            let Some(obj) = self.field_objects.get(&SomPath::new(path.as_str())).cloned() else {
                continue;
            };
            let Some(current) = read_js_string_prop(&obj, "presence", &mut self.context) else {
                continue;
            };
            let initial = read_js_string_prop(&obj, "_initialPresence", &mut self.context)
                .unwrap_or_else(|| "visible".to_string());
            if current.to_lowercase() != initial.to_lowercase() {
                changes.push((path.clone(), current));
            }
        }
        changes
    }

    /// Update initial_presence baseline and form_state value for an
    /// already-registered SOM path.  Used by the Form DOM second pass to
    /// overlay saved runtime state (presence / values) from the `<form>`
    /// packet without creating new JS objects or touching the SOM hierarchy.
    ///
    /// Per XFA 3.3 §3: the `<form>` packet is a saved snapshot of the Form
    /// DOM.  On reload the Form DOM is rebuilt from the Template DOM and then
    /// the saved content is applied as updates.
    pub fn update_field_presence_baseline(&mut self, path: &SomPath, value: &str, presence: &str) {
        // Only update entries that were already registered by the template pass.
        let Some(obj) = self.field_objects.get(path).cloned() else {
            return;
        };

        // Update the initial-presence baseline used by
        // get_all_som_presence_changes() for change detection.
        self.initial_presence
            .insert(path.clone(), presence.to_string());

        // Update the JS object's presence property so the runtime state
        // matches the new baseline (prevents false positives in change
        // detection).
        obj.set(
            PropertyKey::from(js_string!("presence")),
            JsValue::from(js_string!(presence)),
            false,
            &mut self.context,
        )
        .ok();

        // Update form_state value.
        {
            let mut state = self.form_state.write().unwrap();
            state.set_value(path.clone(), XfaValue::String(value.to_string()));
        }
    }

    pub fn execute_script(&mut self, script: &XfaScript) -> Result<Option<String>, String> {
        match script.content_type {
            ScriptContentType::JavaScript => self.execute_javascript(&script.source),
            ScriptContentType::FormCalc => {
                Err("FormCalc scripts require transpilation (not yet implemented).".to_string())
            }
        }
    }

    fn execute_javascript(&mut self, source: &str) -> Result<Option<String>, String> {
        let this_obj = self
            .context
            .global_object()
            .get(
                PropertyKey::from(js_string!("_xfa_this_")),
                &mut self.context,
            )
            .ok();

        let has_this_context = this_obj
            .as_ref()
            .map(|v| !v.is_undefined())
            .unwrap_or(false);

        let initial_raw_value = if let Some(ref this_val) = this_obj {
            if let Some(obj) = this_val.as_object() {
                obj.get(PropertyKey::from(js_string!("rawValue")), &mut self.context)
                    .ok()
                    .and_then(|v| v.to_string(&mut self.context).ok())
                    .map(|s| s.to_std_string_escaped())
            } else {
                None
            }
        } else {
            None
        };

        // Wrap the script in a function with proper `this` binding
        let wrapped_source = if has_this_context {
            format!("(function() {{ {} }}).call(_xfa_this_)", source)
        } else {
            format!("(function() {{ {} }})()", source)
        };

        match self.context.eval(Source::from_bytes(&wrapped_source)) {
            Ok(result) => {
                if let Ok(this_val) = self.context.global_object().get(
                    PropertyKey::from(js_string!("_xfa_this_")),
                    &mut self.context,
                ) && let Some(this_obj) = this_val.as_object()
                    && let Ok(raw_value) =
                        this_obj.get(PropertyKey::from(js_string!("rawValue")), &mut self.context)
                    && !raw_value.is_undefined()
                    && !raw_value.is_null()
                {
                    let value_str = raw_value
                        .to_string(&mut self.context)
                        .map(|s| s.to_std_string_escaped())
                        .unwrap_or_default();

                    let changed = initial_raw_value.as_ref() != Some(&value_str);

                    if changed {
                        if let Some(ref path) = self.current_field_path {
                            let mut state = self.form_state.write().unwrap();
                            state.set_value(path.clone(), XfaValue::String(value_str.clone()));
                        }
                        return Ok(Some(value_str));
                    }
                }

                if result.is_undefined() || result.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(
                        result
                            .to_string(&mut self.context)
                            .map(|s| s.to_std_string_escaped())
                            .unwrap_or_default(),
                    ))
                }
            }
            Err(e) => Err(format!("JavaScript error: {}", e)),
        }
    }

    pub fn form_state(&self) -> &SharedFormState {
        &self.form_state
    }

    /// Access to dependency tracker
    pub fn dependencies(&self) -> &DependencyTracker {
        &self.dependencies
    }

    /// Access to SOM resolver
    pub fn som_resolver(&self) -> &SomResolver {
        &self.som_resolver
    }

    /// Register an XFA node (subform or field) in the SOM hierarchy.
    ///
    /// `is_parent_exclgroup` should be `true` when this node's parent is an
    /// `<exclGroup>` element.  The caller determines this from the XFA tree
    /// structure so that we don't need naming-convention heuristics.
    ///
    /// `item_key` is the key value from `<items><text>…</text></items>` for
    /// exclGroup children. When the parent exclGroup's rawValue is set, children
    /// whose `_itemKey` matches the new value are turned ON (rawValue=on-value),
    /// others OFF (rawValue=off-value). Per XFA 3.3 §4 pp.195-197.
    ///
    /// `off_value` is the second value from `<items>`. Per XFA 3.3 §17 pp.758-759,
    /// when a member is deactivated it assumes its off-value. Defaults to empty
    /// string if not provided.
    #[allow(clippy::too_many_arguments)]
    pub fn register_xfa_node(
        &mut self,
        name: &str,
        path: &str,
        parent_path: Option<&str>,
        is_field: bool,
        value: &str,
        is_parent_exclgroup: bool,
        item_key: Option<&str>,
        off_value: Option<&str>,
        initial_presence: &str,
        // This field's `<items>` (XFA 3.3 §17), as (display, save) pairs --
        // the seed `addItem`/`clearItems`/`getDisplayItem`/... (§6 "Scripting
        // Methods") operate on. Empty for anything that is not a field, or a
        // field with no `<items>`.
        items: &[(String, String)],
    ) {
        let is_exclgroup_child = is_parent_exclgroup;

        // Create the JavaScript object for this node
        let node_obj = if is_field {
            let obj =
                self.create_field_object_with_presence(name, path, value, initial_presence, items);
            // Store the item key for exclGroup parent→child propagation.
            // Per XFA 3.3 §4 pp.195-197: each child in an exclGroup has a key
            // value from <items>. When the parent's rawValue is set, children
            // compare their key to determine ON/OFF state.
            if let Some(key) = item_key {
                obj.set(
                    PropertyKey::from(js_string!("_itemKey")),
                    JsValue::from(js_string!(key)),
                    false,
                    &mut self.context,
                )
                .ok();
            }
            // Store the off-value for deactivation.
            // Per XFA 3.3 §17 pp.758-759: when a member is deactivated it
            // assumes its off-value (second item). Defaults to empty string.
            if let Some(ov) = off_value {
                obj.set(
                    PropertyKey::from(js_string!("_offValue")),
                    JsValue::from(js_string!(ov)),
                    false,
                    &mut self.context,
                )
                .ok();
            }
            obj
        } else {
            // For subforms, create an object that can have children
            let subform_obj = ObjectInitializer::new(&mut self.context)
                .property(
                    js_string!("name"),
                    JsValue::from(js_string!(name)),
                    Attribute::READONLY,
                )
                .property(
                    js_string!("somExpression"),
                    JsValue::from(js_string!(path)),
                    Attribute::CONFIGURABLE,
                )
                .property(
                    js_string!("presence"),
                    JsValue::from(js_string!(initial_presence)),
                    Attribute::all(),
                )
                .property(
                    js_string!("_initialPresence"),
                    JsValue::from(js_string!(initial_presence)),
                    Attribute::READONLY,
                )
                .build();

            // All containers can have rawValue per XFA spec (exclGroups need it
            // for child→parent value propagation).
            subform_obj
                .set(
                    PropertyKey::from(js_string!("_rawValue")),
                    JsValue::from(js_string!(value)),
                    false,
                    &mut self.context,
                )
                .ok();

            self.context
                .global_object()
                .set(
                    PropertyKey::from(js_string!("_xfa_tmp_")),
                    JsValue::from(subform_obj.clone()),
                    false,
                    &mut self.context,
                )
                .ok();
            let _ = self.context.eval(Source::from_bytes(
                r#"Object.defineProperty(_xfa_tmp_, 'rawValue', {
                    get: function() {
                        var v = this._rawValue;
                        return (v !== undefined && v !== null) ? v : '';
                    },
                    set: function(v) {
                        this._rawValue = v;
                        if (this._exclGroupParent) {
                            this._exclGroupParent._rawValue = v;
                        }
                    },
                    configurable: true,
                    enumerable: true
                });"#,
            ));

            // Add execEvent() method (XFA 3.3 §10 pp.407-409)
            self.add_exec_event_method(&subform_obj);
            self.call_helper(
                "_xfa_install_node_methods_",
                &[JsValue::from(subform_obj.clone())],
            );

            // XFA 3.3 §6.16: `.all` returns a collection of every instance
            // sharing this name/scope -- for a subform that is not
            // repeatable (no `setInstances` ever called) that is just
            // itself, but the property has to exist regardless: a script
            // written against a repeatable subform commonly reads `.all`
            // unconditionally (e.g. AAOV's `soSignatureLabel._changeLabel`
            // walking `DYN_Signature.all` even with exactly one instance),
            // and without it `X.all.length` throws instead of returning 1.
            self.add_all_property(&subform_obj);

            subform_obj
        };

        let som_path = SomPath::new(path);
        let parent_som_path = parent_path.map(SomPath::new);

        // Skip re-registration if this path is already registered (e.g., duplicate
        // pageArea children in pageSet). Re-registering would create a new JS object
        // that loses child properties set up on the first registration.
        if self.field_objects.contains_key(&som_path) {
            return;
        }

        // Store in field_objects for later lookup
        log::trace!("[REG] path={path} name={name} is_field={is_field} parent={parent_path:?}");
        self.field_objects
            .insert(som_path.clone(), node_obj.clone());

        // Link child to parent exclGroup for rawValue derivation.
        // Per XFA 3.3 §4 p.196: the field determines ON/OFF by comparing
        // the parent exclGroup's value to its own key value at read-time.
        // The child's rawValue getter uses _exclGroupParent to derive state.
        if is_exclgroup_child && let Some(parent) = parent_path {
            let parent_som = SomPath::new(parent);
            if let Some(parent_obj) = self.field_objects.get(&parent_som) {
                node_obj
                    .set(
                        PropertyKey::from(js_string!("_exclGroupParent")),
                        JsValue::from(parent_obj.clone()),
                        false,
                        &mut self.context,
                    )
                    .ok();
            }
        }

        // Register in SOM resolver
        self.som_resolver.register_node(
            &som_path,
            name,
            if is_field { "field" } else { "subform" },
            parent_som_path.as_ref(),
        );

        // Link this node in as one of its parent's children. This path
        // builds every non-field container (subform, exclGroup, draw) as the
        // same subform-shaped object, so each gets an instance manager; for
        // anything without `<occur>` it is a 1/1 manager whose every change
        // is refused, which is what a script calling it on such a node gets
        // in Acrobat too. Which nodes really repeat is decided on the Form
        // DOM side, by node kind and `<occur>`, never by this flag.
        self.link_child(
            parent_som_path.as_ref(),
            name,
            som_path.index(),
            &node_obj,
            !is_field,
        );

        // Per XFA 3.3 §3 pp.110-114: unqualified references in scripts resolve
        // by searching children, siblings, ancestors, etc. To support direct
        // JavaScript property chain access (e.g. `Page.Section.Field`), all
        // named containers must be accessible as globals. Only register if no
        // global with this name exists yet (first-registered wins: the node
        // closest to the root in document order, which matches XFA tree order).
        if let Ok(existing) = self
            .context
            .global_object()
            .get(PropertyKey::from(JsString::from(name)), &mut self.context)
        {
            if existing.is_undefined() || existing.is_null() {
                self.context
                    .register_global_property(
                        JsString::from(name),
                        node_obj.clone(),
                        Attribute::all(),
                    )
                    .ok();
            }
        }

        // Also register in the _xfa_fields_ registry for resolveNode() lookups
        if let Ok(registry) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_fields_")),
            &mut self.context,
        ) && let Some(registry_obj) = registry.as_object()
        {
            registry_obj
                .set(
                    PropertyKey::from(JsString::from(name)),
                    node_obj.clone(),
                    false,
                    &mut self.context,
                )
                .ok();
        }

        // For floating fields (registered without parent), also add as property on all existing subforms
        if is_field && parent_path.is_none() {
            for subform_obj in self.field_objects.values() {
                if let Ok(som) = subform_obj.get(
                    PropertyKey::from(js_string!("somExpression")),
                    &mut self.context,
                ) && !som.is_undefined()
                {
                    subform_obj
                        .set(
                            PropertyKey::from(JsString::from(name)),
                            node_obj.clone(),
                            false,
                            &mut self.context,
                        )
                        .ok();
                }
            }
        }
    }

    /// Register an event script for a node so `execEvent()` can find it at runtime.
    ///
    /// Per XFA 3.3 §10 pp.407-409: `execEvent()` allows scripts to
    /// programmatically trigger events on other containers. The script sources
    /// are stored in `_xfa_event_scripts_["{path}:{activity}"]`.
    pub fn register_event_script(&mut self, som_path: &str, activity: &str, source: &str) {
        let key = format!("{}:{}", som_path, activity);
        if let Ok(registry) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_event_scripts_")),
            &mut self.context,
        ) && let Some(registry_obj) = registry.as_object()
        {
            registry_obj
                .set(
                    PropertyKey::from(JsString::from(key.as_str())),
                    JsValue::from(js_string!(source)),
                    false,
                    &mut self.context,
                )
                .ok();
        }
    }

    /// Add the `execEvent(activityName)` method to a JS object (field or subform).
    ///
    /// Per XFA 3.3 §10 pp.407-409 Rule 3: the handler executes right away
    /// (not queued). If the container has `presence="inactive"`, the call
    /// fails silently.
    fn add_exec_event_method(&mut self, obj: &JsObject) {
        // execEvent implementation as a JS function that uses the global
        // _xfa_event_scripts_ registry and _xfa_fields_by_path_ for `this` binding.
        let exec_event_src = r#"
            Object.defineProperty(_xfa_tmp_, 'execEvent', {
                value: function(activityName) {
                    // Per XFA 3.3 §10 Rule 3: if presence is inactive, fail silently
                    if (this.presence === 'inactive') return;

                    var somPath = this.somExpression || '';
                    var key = somPath + ':' + activityName;
                    var scriptSrc = _xfa_event_scripts_[key];
                    if (!scriptSrc) return;

                    // Guard against infinite re-entrancy
                    if (typeof _xfa_exec_depth_ === 'undefined') _xfa_exec_depth_ = 0;
                    _xfa_exec_depth_++;
                    if (_xfa_exec_depth_ > 50) {
                        _xfa_exec_depth_--;
                        return;
                    }

                    try {
                        // Execute with `this` bound to the target object
                        var fn = new Function(scriptSrc);
                        fn.call(this);
                    } finally {
                        _xfa_exec_depth_--;
                    }
                },
                writable: false,
                enumerable: false,
                configurable: false
            });
        "#;

        self.context
            .global_object()
            .set(
                PropertyKey::from(js_string!("_xfa_tmp_")),
                JsValue::from(obj.clone()),
                false,
                &mut self.context,
            )
            .ok();
        let _ = self.context.eval(Source::from_bytes(exec_event_src));
    }

    /// Get the current presence value set on `this` by a script.
    /// Only returns a value if the presence was actually changed from its initial value.
    pub fn get_current_field_presence(&mut self) -> Option<Presence> {
        if let Ok(this_val) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_this_")),
            &mut self.context,
        ) && let Some(this_obj) = this_val.as_object()
        {
            let presence_str = read_js_string_prop(this_obj, "presence", &mut self.context)?;
            let initial = read_js_string_prop(this_obj, "_initialPresence", &mut self.context)
                .unwrap_or_else(|| "visible".to_string());
            if presence_str != initial
                && matches!(
                    presence_str.as_str(),
                    "visible" | "invisible" | "hidden" | "inactive"
                )
            {
                return presence_str.parse().ok();
            }
        }
        None
    }

    /// Get the presence value of a child field that was set via `this.childName.presence = ...`
    /// Only returns a value if the presence was actually changed from its initial value.
    pub fn get_child_field_presence(&mut self, child_name: &str) -> Option<(String, Presence)> {
        let child_id = self
            .child_name_to_id
            .get(child_name)
            .cloned()
            .unwrap_or_default();

        if let Ok(this_val) = self.context.global_object().get(
            PropertyKey::from(js_string!("_xfa_this_")),
            &mut self.context,
        ) && let Some(this_obj) = this_val.as_object()
            && let Ok(child_val) = this_obj.get(
                PropertyKey::from(JsString::from(child_name)),
                &mut self.context,
            )
            && let Some(child_obj) = child_val.as_object()
        {
            let presence_str = read_js_string_prop(child_obj, "presence", &mut self.context)?;
            let initial = read_js_string_prop(child_obj, "_initialPresence", &mut self.context)
                .unwrap_or_else(|| "visible".to_string());
            if presence_str != initial
                && matches!(
                    presence_str.as_str(),
                    "visible" | "invisible" | "hidden" | "inactive"
                )
            {
                return presence_str.parse().ok().map(|p| (child_id, p));
            }
        }
        None
    }
}

/// Read one of the layout globals, defaulting to a single-page document.
///
/// `_xfa_page_index_` is 0-based and `_xfa_page_count_` is a count, so the
/// defaults differ: page 0 of 1 page.
fn read_page_global(context: &mut Context, name: &str) -> i32 {
    let default = if name == "_xfa_page_count_" { 1 } else { 0 };
    context
        .global_object()
        .get(js_string!(name.to_string()), context)
        .ok()
        .and_then(|v| v.as_number())
        .map(|n| n as i32)
        .unwrap_or(default)
}

impl Default for XfaScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl XfaScriptEngine {
    /// Number of registered field objects (test helper).
    pub fn field_objects_count(&self) -> usize {
        self.field_objects.len()
    }

    /// Whether a SOM path has an initial-presence entry (test helper).
    pub fn has_initial_presence(&self, path: &SomPath) -> bool {
        self.initial_presence.contains_key(path)
    }

    /// Whether a SOM path has a registered JS object (test helper).
    pub fn has_field_object(&self, path: &SomPath) -> bool {
        self.field_objects.contains_key(path)
    }

    /// Get the initial-presence value for a path (test helper).
    pub fn get_initial_presence(&self, path: &SomPath) -> Option<&str> {
        self.initial_presence.get(path).map(|s| s.as_str())
    }

    /// Set the JS `presence` property on an existing field object (test helper).
    pub fn set_js_presence(&mut self, path: &SomPath, presence: &str) {
        if let Some(obj) = self.field_objects.get(path) {
            obj.set(
                PropertyKey::from(js_string!("presence")),
                JsValue::from(js_string!(presence)),
                false,
                &mut self.context,
            )
            .ok();
        }
    }

    /// Read the form-state value for a path (test helper).
    pub fn get_form_state_value(&self, path: &SomPath) -> Option<String> {
        let state = self.form_state.read().unwrap();
        state.get_value(path).map(|v| v.as_string())
    }
}
