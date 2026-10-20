//! Finding the optional properties of a JSON Schema that accept an empty
//! value. Shared by the MCP conformance check (`u2s_mcp::conformance`) and
//! the native tools' own tests, so every tool a model can call is held to
//! the same rule.

use serde_json::Value;

/// The optional properties of `schema` (at any depth: nested objects, array
/// items and `oneOf`/`anyOf`/`allOf` branches) that accept an empty value,
/// as JSON-Pointer-like paths.
///
/// Why this is a conformance failure: models fill in every optional field,
/// and an empty value (`""`, `[]`, `{}`, or `0` where zero is no real
/// choice) then reaches the server. Servers have read those as "given" in
/// different, silent ways: `kinds: []` filtered out every control, `pages:
/// []` rendered nothing, `max_edge_px: 0` gave a 16 px image. Declaring the
/// minimum makes the schema itself refuse them, so a caller is told to leave
/// the argument out instead.
///
/// - a string needs `minLength` >= 1, or an `enum`/`const`;
/// - an array needs `minItems` >= 1;
/// - an object needs `minProperties` >= 1 or a non-empty `required`;
/// - an integer or number needs a declared `minimum`.
///
/// Required properties are not checked: an empty required value is one the
/// server must handle itself (e.g. setting a field to `""` to clear it).
pub fn empty_optionals(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_empty_optionals(schema, "", &mut found);
    found
}

fn collect_empty_optionals(schema: &Value, path: &str, found: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|keys| keys.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for (key, property) in properties {
            let property_path = format!("{path}/{key}");
            if !required.contains(&key.as_str()) && accepts_empty(property) {
                found.push(property_path.clone());
            }
            collect_empty_optionals(property, &property_path, found);
        }
    }
    if let Some(items) = object.get("items") {
        collect_empty_optionals(items, &format!("{path}/items"), found);
    }
    for combinator in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = object.get(combinator).and_then(Value::as_array) {
            for (index, branch) in branches.iter().enumerate() {
                collect_empty_optionals(branch, &format!("{path}/{combinator}/{index}"), found);
            }
        }
    }
}

/// Whether one property's own schema admits an empty value. A property
/// with several types (`["string", "null"]`) is judged on each.
fn accepts_empty(property: &Value) -> bool {
    let Some(object) = property.as_object() else {
        return false;
    };
    if object.contains_key("enum") || object.contains_key("const") {
        return false;
    }
    let at_least_one = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 1)
    };
    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    types.iter().any(|kind| match *kind {
        "string" => !at_least_one("minLength"),
        "array" => !at_least_one("minItems"),
        "object" => !at_least_one("minProperties") && !requires_a_key(property),
        "integer" | "number" => {
            !object.contains_key("minimum") && !object.contains_key("exclusiveMinimum")
        }
        "null" => true,
        _ => false,
    })
}

/// Whether an object schema can only be satisfied with at least one key:
/// it has a non-empty `required`, or every branch of its `oneOf`/`anyOf`
/// does.
fn requires_a_key(schema: &Value) -> bool {
    let own = schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|keys| !keys.is_empty());
    own || ["oneOf", "anyOf"].iter().any(|combinator| {
        schema
            .get(*combinator)
            .and_then(Value::as_array)
            .is_some_and(|branches| !branches.is_empty() && branches.iter().all(requires_a_key))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_optional_argument_that_can_be_empty_is_found_at_any_depth() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "doc_path": { "type": "string" },
                "session": { "type": "string", "minLength": 1 },
                "format": { "type": "string", "enum": ["png", "jpeg"] },
                "kinds": { "type": "array", "items": { "type": "string", "enum": ["a"] } },
                "pages": { "type": "array", "minItems": 1, "items": { "type": "integer", "minimum": 1 } },
                "dpi": { "type": "number" },
                "page": { "type": "integer", "minimum": 1 },
                "field": { "type": "string" },
                "state": {
                    "type": "object",
                    "minProperties": 1,
                    "properties": {
                        "steps": { "type": "array" },
                        "note": { "type": "string" }
                    },
                    "required": ["note"]
                },
                "rect": {
                    "type": "object",
                    "properties": { "x": { "type": "number" } },
                    "required": ["x"]
                },
                "nothing": { "type": ["string", "null"], "minLength": 1 }
            },
            "required": ["field"]
        });
        assert_eq!(
            super::empty_optionals(&schema),
            vec![
                "/doc_path".to_owned(),
                "/dpi".to_owned(),
                "/kinds".to_owned(),
                "/nothing".to_owned(),
                "/state/steps".to_owned(),
            ]
        );
    }

    #[test]
    fn a_schema_that_declares_every_minimum_has_no_findings() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "doc_path": { "type": "string", "minLength": 1 },
                "state": {
                    "type": "object",
                    "oneOf": [
                        { "properties": { "steps": { "type": "array", "minItems": 1 } }, "required": ["steps"] }
                    ]
                },
                "limit": { "type": "integer", "minimum": 1 }
            }
        });
        assert!(super::empty_optionals(&schema).is_empty());
    }
}
