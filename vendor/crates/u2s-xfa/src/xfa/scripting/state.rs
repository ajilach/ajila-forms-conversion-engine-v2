//! Form State and Value Types
//!
//! This module provides shared form state that scripts can read/write,
//! plus value wrapper types for XFA field values.

use super::som::SomPath;
use boa_engine::{Context, JsValue, js_string};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

// Re-export Presence from xfa module - single source of truth
pub use crate::xfa::Presence;

/// Value wrapper for XFA field values
#[derive(Debug, Clone)]
pub enum XfaValue {
    Null,
    String(String),
    Number(Decimal),
    Boolean(bool),
}

impl XfaValue {
    pub fn to_js_value(&self, _context: &mut Context) -> JsValue {
        match self {
            XfaValue::Null => JsValue::null(),
            XfaValue::String(s) => JsValue::from(js_string!(s.as_str())),
            XfaValue::Number(n) => {
                let f: f64 = n.to_f64().unwrap_or(0.0);
                JsValue::from(f)
            }
            XfaValue::Boolean(b) => JsValue::from(*b),
        }
    }

    pub fn from_js_value(value: &JsValue, context: &mut Context) -> Self {
        if value.is_null_or_undefined() {
            XfaValue::Null
        } else if let Some(b) = value.as_boolean() {
            XfaValue::Boolean(b)
        } else if let Some(n) = value.as_number() {
            XfaValue::Number(Decimal::from_str(&n.to_string()).unwrap_or(Decimal::ZERO))
        } else if let Ok(s) = value.to_string(context) {
            XfaValue::String(s.to_std_string_escaped())
        } else {
            XfaValue::Null
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            XfaValue::Null => String::new(),
            XfaValue::String(s) => s.clone(),
            XfaValue::Number(n) => n.to_string(),
            XfaValue::Boolean(b) => b.to_string(),
        }
    }
}

/// Shared form data state that scripts can read/write
#[derive(Debug, Clone)]
pub struct FormState {
    /// Field values indexed by SOM path
    pub values: HashMap<SomPath, XfaValue>,
    /// Field presence (visible/invisible/hidden/inactive)
    pub presence: HashMap<SomPath, Presence>,
    /// Scripts can declare global variables
    pub global_variables: HashMap<String, XfaValue>,
}

impl FormState {
    pub fn new() -> Self {
        FormState {
            values: HashMap::new(),
            presence: HashMap::new(),
            global_variables: HashMap::new(),
        }
    }

    pub fn get_value(&self, path: &SomPath) -> Option<&XfaValue> {
        if let Some(v) = self.values.get(path) {
            return Some(v);
        }
        let field_name = path.name();
        for (key, value) in &self.values {
            if key.ends_with(&format!(".{}", field_name)) || key.as_str() == field_name {
                return Some(value);
            }
        }
        self.global_variables.get(path.as_str())
    }

    pub fn set_value(&mut self, path: SomPath, value: XfaValue) {
        self.values.insert(path, value);
    }

    pub fn get_presence(&self, path: &SomPath) -> Presence {
        self.presence.get(path).copied().unwrap_or_default()
    }

    pub fn set_presence(&mut self, path: SomPath, presence: Presence) {
        self.presence.insert(path, presence);
    }
}

impl Default for FormState {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedFormState = Arc<RwLock<FormState>>;
