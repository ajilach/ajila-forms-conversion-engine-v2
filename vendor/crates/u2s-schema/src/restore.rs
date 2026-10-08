//! Turning a strict-mode generation back into a document the original schema
//! describes: the inverse of [`super::adapt`].
//!
//! Driven by the **original** schema, walking it alongside the value, rather
//! than by positions recorded during adaptation. Schema pointers do not map
//! one-to-one onto instance pointers once arrays and `$ref`s are involved, so
//! recorded positions would be fragile; reading the original schema is exact.
//! It also means the two directions cannot fall out of step.
//!
//! Two things are undone:
//!
//! 1. **Pair arrays become maps again**, wherever the original says a position
//!    is a map and the value arrived as an array of `{key, value}`.
//! 2. **Nulls that stood in for absence are dropped**, wherever the original
//!    marks a property optional and does not itself accept `null`. A `null` the
//!    original schema genuinely allows is left alone.
//!
//! `$ref`s are followed, which terminates because the descent is driven by the
//! finite value, not by the schema.

use super::ref_resolve::resolve;
use super::{MAP_KEY, MAP_VALUE};
use serde_json::{Map, Value};

/// Rewrites `value` — a generation against [`super::adapt`]'s output — into a
/// document shaped by `original`.
///
/// Anything the original schema does not describe is passed through unchanged:
/// restoration is not validation, and [`super::validate`] is what judges the
/// result.
pub fn restore(original: &Value, value: Value) -> Value {
    restore_node(original, original, value)
}

fn restore_node(root: &Value, schema: &Value, value: Value) -> Value {
    let Some(schema) = resolve(root, schema) else {
        return value;
    };

    if is_map_schema(schema) {
        if let Value::Array(items) = value {
            return pairs_to_map(root, schema, items);
        }
        return value;
    }

    match value {
        Value::Object(fields) => {
            let Some(props) = schema.get("properties").and_then(Value::as_object) else {
                return Value::Object(fields);
            };
            let required = required_names(schema);
            let mut out = Map::with_capacity(fields.len());
            for (name, field) in fields {
                match props.get(&name) {
                    Some(sub) => {
                        if field.is_null()
                            && !required.iter().any(|r| r == &name)
                            && !accepts_null(root, sub)
                        {
                            // The null only existed because strict mode has no
                            // optional properties. Absent is what was meant.
                            continue;
                        }
                        out.insert(name, restore_node(root, sub, field));
                    }
                    None => {
                        out.insert(name, field);
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let Some(item_schema) = schema.get("items") else {
                return Value::Array(items);
            };
            Value::Array(
                items
                    .into_iter()
                    .map(|item| restore_node(root, item_schema, item))
                    .collect(),
            )
        }
        other => {
            // A union: restore under whichever branch describes this value's
            // shape. Branches are tried in order and the first structural match
            // wins, which is enough because `adapt` only ever adds a `null`
            // branch to an existing union.
            if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
                for branch in branches {
                    if let Some(resolved) = resolve(root, branch)
                        && shape_matches(resolved, &other)
                    {
                        return restore_node(root, branch, other);
                    }
                }
            }
            other
        }
    }
}

/// Restores a union-typed object or array, which `restore_node`'s `other` arm
/// cannot see because objects and arrays are matched earlier.
fn shape_matches(schema: &Value, value: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("integer") | Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => true,
    }
}

fn pairs_to_map(root: &Value, schema: &Value, items: Vec<Value>) -> Value {
    let value_schema = map_value_schema(schema);
    let mut out = Map::with_capacity(items.len());
    for item in items {
        let Value::Object(mut pair) = item else {
            // Not a pair: leave the whole thing alone rather than silently
            // dropping data. `validate` will report it against the original.
            return Value::Array(vec![Value::Object(Map::new())]);
        };
        let Some(Value::String(key)) = pair.remove(MAP_KEY) else {
            continue;
        };
        let value = pair.remove(MAP_VALUE).unwrap_or(Value::Null);
        let value = match &value_schema {
            Some(sub) => restore_node(root, sub, value),
            None => value,
        };
        out.insert(key, value);
    }
    Value::Object(out)
}

pub(super) fn required_names(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn accepts_null(root: &Value, schema: &Value) -> bool {
    let Some(schema) = resolve(root, schema) else {
        return false;
    };
    match schema.get("type") {
        Some(Value::String(t)) => t == "null",
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some("null")),
        _ => schema
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| branches.iter().any(|b| accepts_null(root, b))),
    }
}

fn is_map_schema(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    if obj.contains_key("properties") {
        return false;
    }
    obj.contains_key("patternProperties")
        || matches!(obj.get("additionalProperties"), Some(Value::Object(_)))
}

fn map_value_schema(schema: &Value) -> Option<Value> {
    if let Some(Value::Object(patterns)) = schema.get("patternProperties") {
        let mut branches: Vec<Value> = patterns.values().cloned().collect();
        return match branches.len() {
            0 => None,
            1 => Some(branches.remove(0)),
            _ => Some(serde_json::json!({ "anyOf": branches })),
        };
    }
    match schema.get("additionalProperties") {
        Some(Value::Object(schema)) => Some(Value::Object(schema.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::adapt;
    use super::*;
    use serde_json::json;

    #[test]
    fn a_null_standing_in_for_absence_is_dropped() {
        let original = json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "integer" } },
            "required": ["a"]
        });
        let restored = restore(&original, json!({ "a": "x", "b": null }));
        assert_eq!(restored, json!({ "a": "x" }));
    }

    #[test]
    fn a_null_the_original_allows_is_kept() {
        let original = json!({
            "type": "object",
            "properties": { "b": { "type": ["integer", "null"] } },
            "required": []
        });
        let restored = restore(&original, json!({ "b": null }));
        assert_eq!(
            restored,
            json!({ "b": null }),
            "explicit null is meaningful when the schema admits it"
        );
    }

    #[test]
    fn a_required_null_is_kept_so_validate_can_report_it() {
        let original = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "required": ["a"]
        });
        let restored = restore(&original, json!({ "a": null }));
        assert_eq!(
            restored,
            json!({ "a": null }),
            "dropping it would hide the violation"
        );
    }

    #[test]
    fn pairs_become_a_map_again() {
        let original = json!({
            "type": "object",
            "patternProperties": { "^[a-z]{2}$": { "type": "string" } },
            "additionalProperties": false
        });
        let restored = restore(
            &original,
            json!([{ "key": "de", "value": "Hallo" }, { "key": "fr", "value": "Bonjour" }]),
        );
        assert_eq!(restored, json!({ "de": "Hallo", "fr": "Bonjour" }));
    }

    #[test]
    fn a_nested_map_inside_a_property_is_restored() {
        let original = json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "object",
                    "patternProperties": { "^[a-z]{2}$": { "type": "string" } }
                }
            },
            "required": ["label"]
        });
        let restored = restore(
            &original,
            json!({ "label": [{ "key": "de", "value": "Hallo" }] }),
        );
        assert_eq!(restored, json!({ "label": { "de": "Hallo" } }));
    }

    #[test]
    fn refs_are_followed() {
        let original = json!({
            "$ref": "#/$defs/Node",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "children": { "type": "array", "items": { "$ref": "#/$defs/Node" } }
                    },
                    "required": ["children"]
                }
            }
        });
        let restored = restore(
            &original,
            json!({ "title": null, "children": [{ "title": "x", "children": [] }] }),
        );
        assert_eq!(
            restored,
            json!({ "children": [{ "title": "x", "children": [] }] }),
            "the optional null is dropped at every level of the recursion"
        );
    }

    #[test]
    fn adapt_then_restore_round_trips_the_shape() {
        let original = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "pattern": "^[A-Z]" },
                "count": { "type": "integer", "minimum": 1 },
                "labels": {
                    "type": "object",
                    "patternProperties": { "^[a-z]{2}$": { "type": "string" } }
                }
            },
            "required": ["name"]
        });

        let adapted = adapt(&original).expect("adapts");
        // What a strict-mode generation against `adapted` looks like: every
        // property present, absence spelled `null`, the map as pairs.
        let generated = json!({
            "name": "Alpha",
            "count": null,
            "labels": [{ "key": "de", "value": "Etikett" }]
        });
        assert_eq!(
            adapted.schema["required"],
            json!(["count", "labels", "name"])
        );

        let restored = restore(&original, generated);
        assert_eq!(
            restored,
            json!({ "name": "Alpha", "labels": { "de": "Etikett" } })
        );
    }
}
