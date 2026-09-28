//! The value a condition compares a field against.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A field value in a [`ConditionRule`](crate::aem::ConditionRule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum InputValue {
    Text(String),
    Number(#[schemars(with = "String")] Decimal),
    Bool(bool),
}
