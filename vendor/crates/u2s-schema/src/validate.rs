//! Validation against a schema: every value that breaks it shows up as a
//! [`Violation`] with the JSON Pointer of the offending value.
//!
//! Violations are anchored by JSON Pointer because that is the anchor the rest
//! of the system already uses: rule-script violations, the review page's
//! highlight-in-place, and `json_patch` addressing all speak pointers.

use serde::Serialize;
use serde_json::Value;

/// One way a value fails its schema, anchored where the review page can point
/// at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// JSON Pointer into the *value*.
    pub pointer: String,
    /// JSON Pointer into the schema keyword that rejected it, when the
    /// validator reports one — the difference between "this is wrong" and
    /// "this is wrong because of that rule".
    pub schema_pointer: String,
    pub message: String,
}

/// A schema that will not compile. Its own error rather than a `jsonschema`
/// type, so the validator stays an implementation detail — nothing above this
/// crate should have to name it — and so the `Err` variant stays small.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the schema itself is invalid: {message}")]
pub struct SchemaError {
    pub message: String,
}

/// Compiles `schema` and checks `value` against it.
///
/// A schema that will not compile is an error in its own right, not an empty
/// violation list: an output format whose schema is malformed must fail loudly
/// at the point of use rather than silently accept everything.
pub fn validate(schema: &Value, value: &Value) -> Result<Vec<Violation>, SchemaError> {
    let validator = jsonschema::validator_for(schema).map_err(|e| SchemaError {
        message: e.to_string(),
    })?;

    let mut violations: Vec<Violation> = validator
        .iter_errors(value)
        .map(|error| Violation {
            pointer: error.instance_path.to_string(),
            schema_pointer: error.schema_path.to_string(),
            message: error.to_string(),
        })
        .collect();

    // `iter_errors` order follows the validator's internal keyword order, which
    // is stable but arbitrary. Sorting by pointer makes the list reviewable and
    // makes assertions in tests independent of that order.
    violations
        .sort_by(|a, b| (&a.pointer, &a.schema_pointer).cmp(&(&b.pointer, &b.schema_pointer)));
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_valid_value_has_no_violations() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "required": ["a"]
        });
        assert!(
            validate(&schema, &json!({ "a": "x" }))
                .expect("compiles")
                .is_empty()
        );
    }

    #[test]
    fn a_violation_carries_the_pointer_of_the_offending_value() {
        let schema = json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": { "type": "object", "properties": { "n": { "type": "integer" } } }
                }
            }
        });
        let violations =
            validate(&schema, &json!({ "items": [{ "n": 1 }, { "n": "no" }] })).expect("compiles");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].pointer, "/items/1/n");
    }

    #[test]
    fn a_malformed_schema_is_an_error_not_an_empty_verdict() {
        let broken = json!({ "type": "nonsense" });
        assert!(validate(&broken, &json!({})).is_err());
    }
}
