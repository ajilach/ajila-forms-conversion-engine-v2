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
use super::report::{HostMessage, ScriptFailure, ScriptReport};
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

/// The host clock form scripts read (`new Date()`, `util.printd`): the real
/// one, unless `SOURCE_DATE_EPOCH` is set. That variable is the
/// reproducible-builds convention (seconds since the Unix epoch); when it is
/// set, every script sees that instant, in UTC, so a form that stamps today's
/// date renders the same on every day. A value that is not an integer is a
/// configuration error and panics rather than being ignored.
struct ScriptClock;

impl ScriptClock {
    fn source_date_epoch() -> Option<i64> {
        let raw = std::env::var("SOURCE_DATE_EPOCH").ok()?;
        Some(raw.trim().parse().unwrap_or_else(|_| {
            panic!("SOURCE_DATE_EPOCH must be whole seconds since the Unix epoch, got {raw:?}")
        }))
    }
}

impl boa_engine::context::HostHooks for ScriptClock {
    fn utc_now(&self) -> i64 {
        match Self::source_date_epoch() {
            Some(seconds) => seconds * 1000,
            None => boa_engine::context::DefaultHooks.utc_now(),
        }
    }

    fn local_timezone_offset_seconds(&self, unix_time_seconds: i64) -> i32 {
        match Self::source_date_epoch() {
            Some(_) => 0,
            None => boa_engine::context::DefaultHooks.local_timezone_offset_seconds(unix_time_seconds),
        }
    }
}

/// A fresh JavaScript context reading [`ScriptClock`].
fn new_context() -> Context {
    static CLOCK: ScriptClock = ScriptClock;
    Context::builder()
        .host_hooks(&CLOCK)
        .build()
        .expect("a default JavaScript context always builds")
}

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

/// What a script run answers with, besides any `this.rawValue` it changed
/// (which is always the answer when it changed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptResult {
    /// Nothing else: an event handler's result is not a value.
    Owner,
    /// The completion value of the script's code, as `eval` yields it: a
    /// calculate script's result, which "replaces the value of the field"
    /// (XFA 3.3 §10 "Result").
    Completion,
    /// The value the wrapped source `return`s: a validate script's verdict
    /// (see `evaluate_script`).
    Return,
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

/// A field object's value as the Rust side keeps it. The scripting model
/// reads a field with no content as `null` (`rawValue`, see
/// `define_raw_value_accessor`); this side holds values as strings, so that
/// null is the empty string here. `None` only when `obj` has no `rawValue`
/// at all, that is, when it is not a field, subform or exclusion group.
fn read_raw_value(obj: &JsObject, context: &mut Context) -> Option<String> {
    let val = obj
        .get(PropertyKey::from(js_string!("rawValue")), context)
        .ok()?;
    if val.is_undefined() {
        return None;
    }
    if val.is_null() {
        return Some(String::new());
    }
    val.to_string(context)
        .ok()
        .map(|s| s.to_std_string_escaped())
}

/// A node's value for layout: [`read_raw_value`], except for a draw whose
/// value is still its template content, which is `None`. A draw's content
/// is registered so scripts can read it (ACAV_001_DE's barcode reads its
/// form id from a draw), but the draw is laid out from its template -- its
/// rich text, paragraphs and all -- unless a script replaced the value.
fn read_layout_value(obj: &JsObject, context: &mut Context) -> Option<String> {
    let value = read_raw_value(obj, context)?;
    let template = obj
        .get(
            PropertyKey::from(js_string!("_xfa_drawTemplateValue_")),
            context,
        )
        .ok()
        .filter(|v| !v.is_undefined())
        .and_then(|v| v.to_string(context).ok())
        .map(|s| s.to_std_string_escaped());
    match template {
        Some(template) if template == value => None,
        _ => Some(value),
    }
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
    /// Maps child field names to their unique IDs in the current context
    child_name_to_id: HashMap<String, String>,
    /// Tracks the INITIAL presence value from the XFA tree for each field object
    initial_presence: HashMap<SomPath, String>,
    /// Registered `pageArea` objects, whose `index` follows the page being
    /// laid out. See [`XfaScriptEngine::set_layout_context`].
    page_area_objects: Vec<JsObject>,
    /// Every script failure since the last [`Self::take_script_errors`], at
    /// most [`MAX_RECORDED_SCRIPT_ERRORS`] of them. A failing script aborts
    /// its own handler only; the caller decides whether to report it, so it
    /// is recorded here instead of dropped.
    script_errors: Vec<ScriptFailure>,
    /// Failures beyond the cap, counted rather than kept.
    dropped_script_errors: usize,
}

/// How many script failures [`XfaScriptEngine`] keeps between two
/// [`XfaScriptEngine::take_script_errors`] calls. The master-page engine
/// re-runs its scripts for every page and is never drained per page, so the
/// list must be bounded.
pub const MAX_RECORDED_SCRIPT_ERRORS: usize = 100;


impl XfaScriptEngine {
    pub fn new() -> Self {
        let context = new_context();
        let form_state = Arc::new(RwLock::new(FormState::new()));

        let mut engine = XfaScriptEngine {
            context,
            form_state,
            current_field_path: None,
            current_context_path: None,
            som_resolver: SomResolver::new(),
            dependencies: DependencyTracker::new(),
            field_objects: HashMap::new(),
            child_name_to_id: HashMap::new(),
            initial_presence: HashMap::new(),
            page_area_objects: Vec::new(),
            script_errors: Vec::new(),
            dropped_script_errors: 0,
        };

        engine.setup_environment();
        engine
    }

    pub fn with_state(form_state: SharedFormState) -> Self {
        let context = new_context();

        let mut engine = XfaScriptEngine {
            context,
            form_state,
            current_field_path: None,
            current_context_path: None,
            som_resolver: SomResolver::new(),
            dependencies: DependencyTracker::new(),
            field_objects: HashMap::new(),
            child_name_to_id: HashMap::new(),
            initial_presence: HashMap::new(),
            page_area_objects: Vec::new(),
            script_errors: Vec::new(),
            dropped_script_errors: 0,
        };

        engine.setup_environment();
        engine
    }

    fn setup_environment(&mut self) {
        self.setup_xfa_object();
        self.setup_shortcuts();
        self.setup_field_registry();
        self.setup_host_natives();
        self.setup_som_fallback();
        self.setup_console();
    }

    /// The Rust halves of `js_helpers::XFA_HOST_EXTENSIONS`: `util.printd`'s
    /// formatting and a dialog's default answer, both pure functions in
    /// [`super::acrobat`].
    fn setup_host_natives(&mut self) {
        let printd = NativeFunction::from_fn_ptr(|_this, args, context| {
            let format_arg = args.get_or_undefined(0);
            let format = if format_arg.is_number() {
                let preset = format_arg.to_number(context)? as i64;
                super::acrobat::printd_preset(preset)
                    .ok_or_else(|| {
                        boa_engine::JsNativeError::range()
                            .with_message(format!("util.printd: no preset format {preset}"))
                    })?
                    .to_string()
            } else {
                format_arg.to_string(context)?.to_std_string_escaped()
            };
            let mut part = |i: usize| -> boa_engine::JsResult<f64> {
                let n = args.get_or_undefined(i).to_number(context)?;
                if n.is_finite() {
                    Ok(n)
                } else {
                    Err(boa_engine::JsNativeError::range()
                        .with_message("util.printd: the date is not a valid date")
                        .into())
                }
            };
            let date = super::acrobat::DateParts {
                year: part(1)? as i32,
                month: part(2)? as u32,
                day: part(3)? as u32,
                hour: part(4)? as u32,
                minute: part(5)? as u32,
                second: part(6)? as u32,
                weekday: part(7)? as u32,
            };
            Ok(JsValue::from(JsString::from(
                super::acrobat::printd(&format, &date).as_str(),
            )))
        });
        let dialog_answer = NativeFunction::from_fn_ptr(|_this, args, context| {
            let dialog_type = args.get_or_undefined(0).to_number(context)?;
            let dialog_type = if dialog_type.is_finite() { dialog_type as i64 } else { 0 };
            Ok(JsValue::from(super::acrobat::dialog_answer(dialog_type)))
        });
        // `_xfa_picture_native_(mode, picture, text, localeJson)`: `text`
        // formatted (`mode` "format") or read back ("parse") through the
        // picture clause, or `null` when it does not fit or the picture is
        // not supported. An empty locale is `en_US`.
        let picture = NativeFunction::from_fn_ptr(|_this, args, context| {
            let arg = |i: usize, context: &mut Context| -> boa_engine::JsResult<String> {
                Ok(args.get_or_undefined(i).to_string(context)?.to_std_string_escaped())
            };
            let (mode, source, text, locale_json) =
                (arg(0, context)?, arg(1, context)?, arg(2, context)?, arg(3, context)?);
            let locale = if locale_json.is_empty() {
                crate::xfa::locale::Locale::en_us()
            } else {
                serde_json::from_str(&locale_json).map_err(|e| {
                    boa_engine::JsNativeError::typ().with_message(format!("locale: {e}"))
                })?
            };
            let Ok(picture) = crate::xfa::picture::Picture::parse(&source) else {
                return Ok(JsValue::null());
            };
            let result = match mode.as_str() {
                "format" => picture.format(&text, &locale),
                _ => picture.parse_input(&text, &locale),
            };
            Ok(result.map_or(JsValue::null(), |r| JsValue::from(JsString::from(r.as_str()))))
        });
        for (name, function) in [
            ("_xfa_printd_native_", printd),
            ("_xfa_dialog_answer_native_", dialog_answer),
            ("_xfa_picture_native_", picture),
        ] {
            let function = function.to_js_function(self.context.realm());
            self.context
                .register_global_property(JsString::from(name), function, Attribute::all())
                .unwrap_or_else(|e| panic!("cannot register {name}: {e}"));
        }
    }

    /// The non-empty value of the field at exactly `path`; `None` when there
    /// is no such field or its value is empty. An empty value is never
    /// replaced by another field's: two fields of one name in different
    /// subforms are two fields.
    pub fn get_field_value(&mut self, path: &SomPath) -> Option<String> {
        let obj = self.field_objects.get(path)?.clone();
        read_raw_value(&obj, &mut self.context).filter(|v| !v.is_empty())
    }

    /// Every field's value under its full SOM path, empty values included
    /// (a cleared field or a deselected exclusion group holds ""), in a
    /// deterministic order.
    pub fn get_all_field_values_for_flattening(&mut self) -> HashMap<SomPath, String> {
        let mut map = HashMap::new();
        let mut paths: Vec<(SomPath, JsObject)> = self
            .field_objects
            .iter()
            .map(|(p, o)| (p.clone(), o.clone()))
            .collect();
        paths.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        for (path, obj) in paths {
            if let Some(value) = read_layout_value(&obj, &mut self.context) {
                map.insert(path, value);
            }
        }
        map
    }

    /// Index `obj` under `path` in `_xfa_fields_by_path_` (see
    /// [`Self::setup_som_fallback`]).
    fn index_by_path(&mut self, path: &str, obj: &JsObject) {
        if let Some(registry) = Self::get_path_registry(&mut self.context) {
            registry
                .set(PropertyKey::from(JsString::from(path)), obj.clone(), false, &mut self.context)
                .ok();
        }
    }

    /// `_xfa_fields_by_path_`: every registered node by its SOM path, for
    /// the running script's container lookup (`_xfa_context_node_`).
    fn setup_field_registry(&mut self) {
        // Create _xfa_fields_by_path_ that maps FULL SOM paths to JS objects
        let registry_by_path = ObjectInitializer::new(&mut self.context).build();
        self.context
            .register_global_property(
                js_string!("_xfa_fields_by_path_"),
                registry_by_path,
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

    /// Load the JavaScript helpers (the SOM walker, the instance manager,
    /// the host extensions). Evaluated as plain global code: the script
    /// scope every later evaluation runs in is one of them.
    fn setup_som_fallback(&mut self) {
        let helpers_js = js_helpers::get_all_helpers();
        // The helpers are this crate's own code: one that fails to load is a
        // bug, not a property of the form.
        self.context
            .eval(Source::from_bytes(&helpers_js))
            .unwrap_or_else(|e| panic!("the JS helpers failed to load: {e}"));
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

    /// `xfa.host`. `messageBox`, `resetData`, `print` and `numPages` are
    /// added by `js_helpers::XFA_HOST_EXTENSIONS`.
    fn create_host_object(&mut self) -> JsObject {
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
        self.register_field_with_presence(path, name, value, "visible", false, &[]);
    }

    /// Register a field with SOM resolver and explicit initial presence
    pub fn register_field_with_presence(
        &mut self,
        path: &str,
        name: &str,
        value: &str,
        initial_presence: &str,
        is_subform: bool,
        // The field's own `<items>` (XFA 3.3 §17), as (display, save)
        // pairs, for its choice-list methods and `selectedIndex` (§6);
        // empty for anything without a choice list.
        items: &[(String, String)],
    ) {
        let som_path = SomPath::new(path);
        let parent_path = som_path.parent();

        // Every ancestor exists before its child is linked in: a caller
        // registering a leaf by its path alone gets the containers above it
        // as plain subforms.
        if let Some(parent) = &parent_path
            && !self.field_objects.contains_key(parent)
        {
            self.register_field_with_presence(parent.as_str(), parent.name(), "", "visible", true, &[]);
        }

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
            self.create_field_object_with_presence(name, path, value, initial_presence, items);
        self.field_objects
            .insert(som_path.clone(), field_obj.clone());
        self.index_by_path(path, &field_obj);

        self.link_child(
            parent_path.as_ref(),
            name,
            som_path.index(),
            &field_obj,
            is_subform,
        );
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
        self.set_merged_value(&field, initial_value);

        // Define rawValue with getter/setter for exclGroup propagation.
        //
        // A field with no content reads `null`, not "": XFA 3.3 §2 "Field
        // Values" ("When no content is present, the content shall be
        // interpreted as representing a null value") and §6 "Null Data"
        // (empty content is `isNull` with `nullType="empty"`). Forms test
        // for an empty field with `this.rawValue == null` (BADM_019_EN's
        // client-type list fills its items and default only then), so the
        // empty string must not stand in for null; `_xfa_content_value_`
        // is the one rule.
        //
        // Per XFA 3.3 §4 p.196: "The field determines whether it is on or off
        // by comparing the value of the variable to its own key value."
        //
        // Getter: If this field is an exclGroup child with an _itemKey, the
        //   ON/OFF state is DERIVED at read-time by comparing the parent
        //   exclGroup's value to this field's key.  This is spec-compliant
        //   and avoids write-side propagation issues.
        // Setter: When a member is assigned its on value, the parent
        //   exclGroup takes that key; any other value deselects the group
        //   if this member was the selected one.
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
                            return (String(pv) === String(this._itemKey)) ? String(this._itemKey) : _xfa_content_value_(this._offValue);
                        }
                        return _xfa_content_value_(this._offValue);
                    }
                    return _xfa_content_value_(this._rawValue);
                },
                set: function(v) {
                    _xfa_set_raw_(this, v);
                    var group = this._exclGroupParent;
                    if (group) {
                        if (this._itemKey === undefined) {
                            _xfa_set_raw_(group, v);
                        } else if (String(v) === String(this._itemKey)) {
                            // XFA 3.3 §17 "The exclGroup element": assigning
                            // a member its on value activates it, which
                            // deactivates the others.
                            _xfa_set_raw_(group, this._itemKey);
                        } else if (String(group._rawValue) === String(this._itemKey)) {
                            // Any other value turns the active member off,
                            // leaving the group with no selection.
                            _xfa_set_raw_(group, '');
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

        // caption.value: the caption's `<value>` element, whose `<text>`
        // `init_caption` adds for a field that has one.
        let caption_value = ObjectInitializer::new(&mut self.context).build();
        let caption_obj = ObjectInitializer::new(&mut self.context)
            .property(js_string!("value"), caption_value, Attribute::all())
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
_xfa_hidden_(_xfa_tmp_, '_xfa_initialItems_', _xfa_items_key_(_xfa_tmp_));
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
    return (_xfa_value_text_(this.rawValue) === this._items[i].s) ? 1 : 0;
};
_xfa_tmp_.setItemState = function(i, state) {
    if (this._items && this._items[i] && state) this.rawValue = this._items[i].s;
};
// selectedIndex (LiveCycle Designer Scripting Reference; XFA 3.3 §6 only
// addresses items by index): the index of the item whose save value is the
// field's value, -1 when none is, as a choice list with nothing chosen
// reads in Acrobat. Assigning an index chooses that item; -1 clears.
Object.defineProperty(_xfa_tmp_, 'selectedIndex', {
    get: function() {
        if (!this._items) return -1;
        var current = (this.rawValue === null || this.rawValue === undefined) ? '' : String(this.rawValue);
        if (current === '') return -1;
        for (var k = 0; k < this._items.length; k++) {
            if (this._items[k].s === current) return k;
        }
        return -1;
    },
    set: function(i) {
        i = Number(i);
        if (this._items && this._items[i]) { this.rawValue = this._items[i].s; }
        else if (i === -1) { this.rawValue = ''; }
    },
    enumerable: false, configurable: true
});
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

        for field in [&mut self.current_field_path, &mut self.current_context_path] {
            if let Some(path) = field.as_ref()
                && let Some(new) = remap(path)
            {
                *field = new;
            }
        }
    }

    fn som_expression_of(&mut self, obj: &JsObject) -> Option<SomPath> {
        read_js_string_prop(obj, "somExpression", &mut self.context).map(SomPath::new)
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

    /// `source` inside the running script's scope (`with (_xfa_scope_(...))`,
    /// as `execute_javascript` runs a script), so unqualified names resolve
    /// by XFA 3.3 §3 here too; with no running script, only globals.
    fn in_script_scope(source: &str) -> String {
        format!(
            "with (_xfa_scope_(typeof _xfa_this_ === 'undefined' ? undefined : _xfa_this_)) {{ {source}\n }}"
        )
    }

    pub fn execute_variable_script(&mut self, source: &str) -> Result<(), String> {
        match self.context.eval(Source::from_bytes(&Self::in_script_scope(source))) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Variable script error: {}", e)),
        }
    }

    pub fn evaluate_expression(&mut self, source: &str) -> Result<String, String> {
        match self.context.eval(Source::from_bytes(&Self::in_script_scope(source))) {
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
        let activity_name = activity.activity_name();

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
        // A change made by this host replaces the whole value: `change` is
        // the text put in, from `selStart` 0 to `selEnd` at the end of the
        // previous text, and `newText` is what the field holds afterwards.
        // Every other event carries empty texts, never a previous event's.
        let is_change = matches!(activity, EventActivity::Change);
        let current_value = self.get_field_value(&target_som).unwrap_or_default();
        let prev = if is_change { prev_value.unwrap_or("") } else { "" };
        let after = if is_change { current_value.as_str() } else { "" };
        for (key, value) in [("change", after), ("prevText", prev), ("newText", after), ("fullText", after)] {
            event
                .define_property_or_throw(
                    PropertyKey::from(JsString::from(key)),
                    boa_engine::property::PropertyDescriptor::builder()
                        .value(JsValue::from(js_string!(value)))
                        .writable(key == "change")
                        .configurable(true)
                        .enumerable(true)
                        .build(),
                    &mut self.context,
                )
                .ok();
        }
        event
            .set(
                PropertyKey::from(js_string!("selEnd")),
                JsValue::from(prev.chars().count() as i32),
                false,
                &mut self.context,
            )
            .ok();
    }

    /// Make `path` the running script's container, with its children linked
    /// in (`children` as (name, id) pairs).
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

        // A child the Form DOM already holds is reached through its parent
        // link (`_xfa_set_child_`); one the tree does not have yet (a leaf
        // registered on its own) is linked in the same way, never set as a
        // plain property that an assignment could replace.
        for (child_name, child_id) in children {
            let child_path = format!("{}.{}", path, child_name);
            let child_som_path = SomPath::new(&child_path);
            self.child_name_to_id.insert(child_name.clone(), child_id.clone());
            if self.field_objects.contains_key(&child_som_path) {
                continue;
            }
            let child_obj = self.create_field_object(child_name, &child_path, "");
            self.field_objects.insert(child_som_path, child_obj.clone());
            self.index_by_path(&child_path, &child_obj);
            self.call_helper(
                "_xfa_link_child_",
                &[
                    JsValue::from(this_obj.clone()),
                    JsValue::from(JsString::from(child_name.as_str())),
                    JsValue::from(0),
                    JsValue::from(child_obj),
                    JsValue::from(false),
                ],
            );
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
            if let Some(value) = read_raw_value(child_obj, &mut self.context) {
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
        read_raw_value(obj, &mut self.context)
    }

    /// Every field's value keyed by its full SOM path (aliases included),
    /// and a draw's only once a script replaced it (`read_layout_value`).
    pub fn get_all_som_field_values_by_path(&mut self) -> HashMap<String, String> {
        let mut values = HashMap::new();

        for (path, obj) in &self.field_objects {
            if let Some(value) = read_layout_value(obj, &mut self.context) {
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
            if let Some(value) = read_raw_value(&obj, &mut self.context) {
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

    /// Mark the object registered at `path` as a draw holding its template
    /// content `value`, which [`read_layout_value`] then leaves to the
    /// template until a script replaces it. A path with no registered
    /// object is ignored, as for [`Self::init_access`].
    pub fn mark_draw(&mut self, path: &str, value: &str) {
        let Some(obj) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        self.call_helper(
            "_xfa_hidden_",
            &[
                JsValue::from(obj),
                JsValue::from(js_string!("_xfa_drawTemplateValue_")),
                JsValue::from(JsString::from(value)),
            ],
        );
    }

    /// Bind the object registered at `path` to the global data value named
    /// `name` (XFA 3.3 §6 "Global data", `<bind match="global"/>`): from
    /// here on it shares its value with every other object bound to that
    /// name. A path with no registered object is ignored, as for
    /// [`Self::init_access`].
    pub fn join_global_binding(&mut self, path: &str, name: &str) {
        let Some(obj) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        self.call_helper(
            "_xfa_join_global_binding_",
            &[JsValue::from(obj), JsValue::from(JsString::from(name))],
        );
    }

    /// Give the object registered at `path` its caption text, reachable as
    /// `caption.value.#text` (XFA 3.3 §3 class references) so a script can
    /// read and relabel it. `None` for a caption without a `<text>`, which
    /// then has no `#text` to resolve, as in Acrobat. A path with no
    /// registered object is ignored, as for [`Self::init_access`].
    pub fn init_caption(&mut self, path: &str, text: Option<&str>) {
        let Some(obj) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        let text = match text {
            Some(text) => JsValue::from(JsString::from(text)),
            None => JsValue::null(),
        };
        self.call_helper("_xfa_init_caption_", &[JsValue::from(obj), text]);
    }

    /// Every caption text a script has changed since the last call, under
    /// each object's canonical path, with the baseline moved to the new
    /// text, as [`Self::take_access_changes`] does for `access`.
    pub fn take_caption_changes(&mut self) -> Vec<(SomPath, String)> {
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
            let changed = self.call_helper("_xfa_take_caption_change_", &[JsValue::from(obj)]);
            if let Some(text) = changed.as_string() {
                changes.push((path, text.to_std_string_escaped()));
            }
        }
        changes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        changes
    }

    /// Every choice list a script has changed since the last call
    /// (`addItem`, `clearItems`, `deleteItem`, `setItems`; XFA 3.3 §6
    /// "Scripting Methods"), under each field's canonical path as (display,
    /// save) pairs, with the baseline moved to the new list, as
    /// [`Self::take_caption_changes`] does for captions.
    pub fn take_item_changes(&mut self) -> Vec<(SomPath, Vec<(String, String)>)> {
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
            let changed = self.call_helper("_xfa_take_items_change_", &[JsValue::from(obj)]);
            let Some(json) = changed.as_string() else {
                continue;
            };
            let items: Vec<(String, String)> =
                serde_json::from_str(&json.to_std_string_escaped()).unwrap_or_else(|e| {
                    panic!("the items key is JSON this engine wrote itself: {e}")
                });
            changes.push((path, items));
        }
        changes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        changes
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
    /// clearing and rebuilding the registration because
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

    /// Make the pageSet registered at `path` (as a container, by
    /// [`Self::register_xfa_node`]) reachable from its parent by class name
    /// as well: `form1.pageSet.MP_Last` for a pageSet named `MPs`, the way
    /// Designer writes it and Acrobat resolves it (XFA 3.3 §3 "Reference by
    /// Class" spells the class reference `#pageSet`).
    pub fn register_page_set(&mut self, path: &str, parent_path: Option<&str>) {
        self.alias_by_class(path, parent_path, "pageSet");
    }

    /// Make the pageArea registered at `path` (as a container, by
    /// [`Self::register_xfa_node`]) tell scripts which page is being laid
    /// out: its `index` is the page's (see [`Self::set_layout_context`]).
    /// Also reachable from its pageSet as `pageArea`, as
    /// [`Self::register_page_set`] does for the pageSet.
    pub fn register_page_area(&mut self, path: &str, page_set_path: Option<&str>) {
        self.alias_by_class(path, page_set_path, "pageArea");
        if let Some(object) = self.field_objects.get(&SomPath::new(path)).cloned() {
            self.page_area_objects.push(object);
        }
    }

    /// `parent.<class_name>` for the node registered at `path`, unless the
    /// parent already has a child or alias of that name.
    fn alias_by_class(&mut self, path: &str, parent_path: Option<&str>, class_name: &str) {
        let Some(object) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        let Some(parent) = parent_path.and_then(|p| self.field_objects.get(&SomPath::new(p)).cloned())
        else {
            return;
        };
        let taken = parent
            .get(PropertyKey::from(JsString::from(class_name)), &mut self.context)
            .map(|v| !v.is_undefined())
            .unwrap_or(false);
        if !taken {
            parent
                .set(
                    PropertyKey::from(JsString::from(class_name)),
                    object,
                    false,
                    &mut self.context,
                )
                .ok();
        }
    }

    /// Declare every named script object of `nodes` (see
    /// `js_helpers::XFA_SCRIPT_OBJECTS`): each is built on its first
    /// reference, in its owner subform's scope. Call once the SOM hierarchy
    /// is registered, so the owners exist.
    pub fn register_script_objects(&mut self, nodes: &[crate::xfa::XfaNode]) {
        let declared: Vec<(String, String, Option<String>)> =
            match crate::xfa::find_root_subform(nodes) {
                Some(root) => {
                    let root_name = root.name.clone().unwrap_or_default();
                    crate::xfa::collect_variable_script_owners(&root_name, &root.children)
                        .into_iter()
                        .map(|(name, content, owner)| (name, content, Some(owner)))
                        .collect()
                }
                None => crate::xfa::collect_variable_scripts(nodes)
                    .into_iter()
                    .map(|(name, content)| (name, content, None))
                    .collect(),
            };
        for (name, content, owner_path) in declared {
            let owner = owner_path
                .and_then(|p| self.field_objects.get(&SomPath::new(p)).cloned())
                .map_or(JsValue::null(), JsValue::from);
            let factory = super::script_object::script_object_factory(&content);
            self.call_helper(
                "_xfa_define_script_object_",
                &[
                    JsValue::from(JsString::from(name.as_str())),
                    owner,
                    JsValue::from(JsString::from(factory.as_str())),
                ],
            );
        }
        // Declared first, then instantiated in document order (XFA 3.3 §10
        // "Instantiation of Named Script Objects"): a body may refer to an
        // object declared after it, which is built on that reference.
        self.call_helper("_xfa_instantiate_script_objects_", &[]);
    }

    /// The `presence` the template declared for the object registered at
    /// `path` (its `_initialPresence`).
    pub fn get_template_presence(&mut self, path: &str) -> Option<String> {
        let obj = self.field_objects.get(&SomPath::new(path)).cloned()?;
        read_js_string_prop(&obj, "_initialPresence", &mut self.context)
    }

    /// The `presence` the object registered at `path` currently holds.
    pub fn get_js_presence(&mut self, path: &str) -> Option<String> {
        let obj = self.field_objects.get(&SomPath::new(path)).cloned()?;
        read_js_string_prop(&obj, "presence", &mut self.context)
    }

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

    /// Put the object at `path` back to the `access` and caption text its
    /// baselines hold, so a master-page script's per-page writes to them
    /// (which only that page's instance would carry) are not reported as
    /// the next interaction's changes. A path with no registered object is
    /// ignored.
    pub fn reset_to_baseline(&mut self, path: &str) {
        let Some(obj) = self.field_objects.get(&SomPath::new(path)).cloned() else {
            return;
        };
        self.call_helper("_xfa_reset_to_baseline_", &[JsValue::from(obj)]);
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

        // The saved Form DOM is what the form was merged with.
        self.set_merged_value(&obj, value);

        // Update form_state value.
        {
            let mut state = self.form_state.write().unwrap();
            state.set_value(path.clone(), XfaValue::String(value.to_string()));
        }
    }

    /// Make every node's current value its merged value, the one
    /// `getDeltas()` compares against: called once the load pass has run,
    /// so a form opens with no deltas however many values its own scripts
    /// filled in.
    pub fn rebase_merged_values(&mut self) {
        let objects: Vec<JsObject> = self.field_objects.values().cloned().collect();
        for obj in objects {
            if let Some(value) = read_raw_value(&obj, &mut self.context) {
                self.set_merged_value(&obj, &value);
            }
        }
    }

    /// Record `value` as the node's merged value, the one `getDeltas()`
    /// compares its current value with (a hidden `_xfa_mergedValue_`).
    fn set_merged_value(&mut self, obj: &JsObject, value: &str) {
        obj.define_property_or_throw(
            js_string!("_xfa_mergedValue_"),
            boa_engine::property::PropertyDescriptor::builder()
                .value(JsValue::from(JsString::from(value)))
                .writable(true)
                .enumerable(false)
                .configurable(true)
                .build(),
            &mut self.context,
        )
        .expect("a plain object accepts a new property");
    }

    /// Run one script with the current field as `this`. A failure is
    /// returned and also recorded for [`Self::take_script_errors`], under the
    /// script's owner and activity.
    /// Run `script` as its owner's event handler. The answer is the value
    /// the script gave its owner (see [`ScriptResult`]): for a calculate
    /// script its result, for any other event only a `this.rawValue` it
    /// changed.
    pub fn execute_script(&mut self, script: &XfaScript) -> Result<Option<String>, String> {
        let mode = if script.activity == EventActivity::Calculate {
            ScriptResult::Completion
        } else {
            ScriptResult::Owner
        };
        self.run_script(script, mode)
    }

    fn run_script(&mut self, script: &XfaScript, mode: ScriptResult) -> Result<Option<String>, String> {
        // A server-only script is not run by a client (see
        // `XfaScript::runs_on_client`): it did nothing here, as in Acrobat.
        if !script.runs_on_client() {
            return Ok(None);
        }
        let result = match script.content_type {
            ScriptContentType::JavaScript => self.execute_javascript(&script.source, mode),
            // A FormCalc script that translated is JavaScript by now (see
            // `events::executable`); this one did not, and says why.
            ScriptContentType::FormCalc => Err(match super::formcalc::transpile(&script.source) {
                Err(e) => e.to_string(),
                Ok(_) => "FormCalc script was not translated before it ran".to_string(),
            }),
        };
        if let Err(e) = &result {
            let owner = self
                .current_field_path
                .as_ref()
                .map(|p| p.as_str().to_string())
                .unwrap_or_default();
            self.record_script_error(ScriptFailure {
                activity: script.activity.activity_name().to_string(),
                owner,
                page: None,
                error: e.clone(),
            });
        }
        result
    }

    /// Run `script` for its value: its last expression's, as a validate
    /// script's verdict is (XFA 3.3 "The validate element": a script test
    /// fails when it evaluates to false). A direct `eval` inside the usual
    /// wrapper keeps `this` the script's owner.
    pub fn evaluate_script(&mut self, script: &XfaScript) -> Result<Option<String>, String> {
        let literal = serde_json::to_string(&script.source).expect("a string serialises");
        let mut wrapped = script.clone();
        wrapped.source = format!("return eval({literal});");
        self.run_script(&wrapped, ScriptResult::Return)
    }

    /// Record a script that could not run at all (a FormCalc script this
    /// engine does not translate), as if it had run and failed.
    pub fn record_unrunnable(&mut self, script: &XfaScript, owner: &str) {
        if !script.runs_on_client() {
            return;
        }
        if let Err(error) = match script.content_type {
            ScriptContentType::FormCalc => super::formcalc::transpile(&script.source).map(|_| ()),
            ScriptContentType::JavaScript => Ok(()),
        } {
            self.record_script_error(ScriptFailure {
                activity: script.activity.activity_name().to_string(),
                owner: owner.to_string(),
                page: None,
                error: error.to_string(),
            });
        }
    }

    fn record_script_error(&mut self, error: ScriptFailure) {
        if self.script_errors.len() < MAX_RECORDED_SCRIPT_ERRORS {
            self.script_errors.push(error);
        } else {
            self.dropped_script_errors += 1;
        }
    }

    /// Every script failure since the last call, the host's own included (a
    /// `resetData` naming nothing), in the order they happened, and how many
    /// more were not kept.
    pub fn take_script_errors(&mut self) -> (Vec<ScriptFailure>, usize) {
        let host_errors = self.call_helper("_xfa_drain_host_errors_", &[]);
        for error in self.js_string_list(&host_errors) {
            self.record_script_error(ScriptFailure {
                activity: String::new(),
                owner: String::new(),
                page: None,
                error,
            });
        }
        let dropped = std::mem::take(&mut self.dropped_script_errors);
        (std::mem::take(&mut self.script_errors), dropped)
    }

    /// Every script failure and dialog since the last call.
    pub fn take_report(&mut self) -> ScriptReport {
        let (errors, dropped_errors) = self.take_script_errors();
        ScriptReport {
            errors,
            dropped_errors,
            messages: self.drain_host_messages(),
        }
    }

    /// Every dialog the scripts raised since the last call, in order.
    pub fn drain_host_messages(&mut self) -> Vec<HostMessage> {
        let queued = self.call_helper("_xfa_drain_host_messages_", &[]);
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
                let answer = entry
                    .get(PropertyKey::from(js_string!("answer")), &mut self.context)
                    .ok()?
                    .to_number(&mut self.context)
                    .ok()? as i32;
                Some(HostMessage {
                    source: read_js_string_prop(&entry, "source", &mut self.context)?,
                    text: read_js_string_prop(&entry, "text", &mut self.context)?,
                    title: read_js_string_prop(&entry, "title", &mut self.context)
                        .unwrap_or_default(),
                    answer,
                })
            })
            .collect()
    }

    /// The strings of a JS array, in order.
    fn js_string_list(&mut self, value: &JsValue) -> Vec<String> {
        let Some(list) = value.as_object().cloned() else {
            return Vec::new();
        };
        let length = list
            .get(PropertyKey::from(js_string!("length")), &mut self.context)
            .ok()
            .and_then(|v| v.to_number(&mut self.context).ok())
            .unwrap_or(0.0) as u32;
        (0..length)
            .filter_map(|i| {
                let v = list.get(PropertyKey::from(i), &mut self.context).ok()?;
                v.to_string(&mut self.context)
                    .ok()
                    .map(|s| s.to_std_string_escaped())
            })
            .collect()
    }

    /// Give the field at `path` its `formattedValue` and `editValue`, through
    /// its format and edit pictures (either may be absent) in `locale`.
    pub fn install_value_format(
        &mut self,
        path: &SomPath,
        format: Option<&str>,
        edit: Option<&str>,
        locale: &crate::xfa::locale::Locale,
    ) {
        let Some(obj) = self.field_objects.get(path).cloned() else {
            return;
        };
        let locales = self
            .context
            .global_object()
            .get(PropertyKey::from(js_string!("_xfa_locales_")), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .expect("_xfa_locales_ is defined by the value-format helpers");
        let key = PropertyKey::from(JsString::from(locale.name.as_str()));
        if !locales.has_own_property(key.clone(), &mut self.context).unwrap_or(false) {
            let json = serde_json::to_string(locale).expect("a locale serialises");
            locales
                .set(key, JsValue::from(JsString::from(json.as_str())), false, &mut self.context)
                .expect("a plain object accepts any property");
        }
        let text = |s: Option<&str>| JsValue::from(JsString::from(s.unwrap_or("")));
        self.call_helper(
            "_xfa_install_value_format_",
            &[
                JsValue::from(obj),
                text(format),
                text(edit),
                JsValue::from(JsString::from(locale.name.as_str())),
            ],
        );
    }

    /// Give `xfa.host.resetData` each field's template default, keyed by its
    /// path without instance indices (`SomPath::index_free`).
    pub fn set_template_defaults(&mut self, defaults: &HashMap<SomPath, String>) {
        let target = self
            .context
            .global_object()
            .get(
                PropertyKey::from(js_string!("_xfa_template_defaults_")),
                &mut self.context,
            )
            .ok()
            .and_then(|v| v.as_object().cloned())
            .expect("_xfa_template_defaults_ is defined by the host extensions");
        for (path, value) in defaults {
            target
                .set(
                    PropertyKey::from(JsString::from(path.index_free().as_str())),
                    JsValue::from(JsString::from(value.as_str())),
                    false,
                    &mut self.context,
                )
                .expect("a plain object accepts any property");
        }
    }

    /// Run `source` as its owner's script; `mode` says what the answer is.
    fn execute_javascript(&mut self, source: &str, mode: ScriptResult) -> Result<Option<String>, String> {
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

        let initial_raw_value = this_obj
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|obj| read_raw_value(obj, &mut self.context));

        // The script runs as a function with its owner as `this`, inside
        // `with` its owner's scope (XFA 3.3 §3: an unqualified name is
        // searched among the container's children, then its siblings, its
        // parent's siblings, ... up to the root). The `with` is outside the
        // function so the script's own `var`s shadow form nodes.
        let (this_expr, scope_expr) = if has_this_context {
            ("_xfa_this_", "_xfa_this_")
        } else {
            ("undefined", "undefined")
        };
        let body = match mode {
            ScriptResult::Completion => {
                let literal = serde_json::to_string(source).expect("a string serialises");
                format!("return eval({literal});")
            }
            ScriptResult::Owner | ScriptResult::Return => format!("{source}\n"),
        };
        let wrapped_source = format!(
            "var _xfa_result_ = undefined; with (_xfa_scope_({scope_expr})) {{ \
             _xfa_result_ = (function() {{ {body} }}).call({this_expr}); }} _xfa_result_"
        );

        match self.context.eval(Source::from_bytes(&wrapped_source)) {
            Ok(result) => {
                if let Ok(this_val) = self.context.global_object().get(
                    PropertyKey::from(js_string!("_xfa_this_")),
                    &mut self.context,
                ) && let Some(this_obj) = this_val.as_object()
                    && let Some(value_str) = read_raw_value(this_obj, &mut self.context)
                {
                    let changed = initial_raw_value.as_ref() != Some(&value_str);

                    if changed {
                        if let Some(ref path) = self.current_field_path {
                            let mut state = self.form_state.write().unwrap();
                            state.set_value(path.clone(), XfaValue::String(value_str.clone()));
                        }
                        return Ok(Some(value_str));
                    }
                }

                if mode == ScriptResult::Owner || result.is_undefined() || result.is_null() {
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
            self.set_merged_value(&subform_obj, value);

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
                        return _xfa_content_value_(this._rawValue);
                    },
                    set: function(v) {
                        _xfa_set_raw_(this, v);
                        if (this._exclGroupParent) {
                            _xfa_set_raw_(this._exclGroupParent, v);
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
        self.index_by_path(path, &node_obj);

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
                    // XFA 3.3 §10 Rule 1: an inactive container, or one inside
                    // an inactive container, fires no events.
                    for (var holder = this; holder; holder = holder.parent) {
                        if (holder.presence === 'inactive') return;
                    }

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
                        // Execute with `this` bound to the target object, in
                        // its scope (see `execute_javascript`).
                        var fn = new Function('_xfa_scope_proxy_',
                            'with (_xfa_scope_proxy_) { return (function() { ' + scriptSrc + '\n }).call(this); }');
                        fn.call(this, _xfa_scope_(this));
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

    /// Read the form-state value for a path (test helper).
    pub fn get_form_state_value(&self, path: &SomPath) -> Option<String> {
        let state = self.form_state.read().unwrap();
        state.get_value(path).map(|v| v.as_string())
    }
}
