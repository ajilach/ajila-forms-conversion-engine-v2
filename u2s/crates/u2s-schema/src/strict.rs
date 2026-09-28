//! What "strict-safe" means, stated once as an executable check.
//!
//! [`adapt`](super::adapt) produces schemas for a provider that rejects
//! anything outside its subset with an opaque 400. Asserting the subset here
//! turns that into a local test failure naming the pointer, and lets the format
//! crates check their own schemas without restating the rules — the day-one AEM
//! schema does exactly that.
//!
//! This is a checker, not a validator: it inspects a *schema*, not a document.

use serde_json::Value;

/// Every keyword the strict subset rejects outright. A superset of what
/// [`super::adapt`] strips, because a hand-written or third-party schema may
/// contain keywords the adapter refuses rather than removes.
const FORBIDDEN: &[&str] = &[
    "allOf",
    "not",
    "if",
    "then",
    "else",
    "oneOf",
    "patternProperties",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
    "minContains",
    "maxContains",
    "unevaluatedItems",
    "minProperties",
    "maxProperties",
    "propertyNames",
    "dependentRequired",
    "dependentSchemas",
    "unevaluatedProperties",
    "default",
];

/// Reasons `schema` would be rejected in strict mode, each naming a JSON
/// Pointer into the schema. Empty means strict-safe.
pub fn strict_violations(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(schema, "", &mut out);
    out
}

fn walk(node: &Value, pointer: &str, out: &mut Vec<String>) {
    let Value::Object(obj) = node else {
        return;
    };

    for keyword in FORBIDDEN {
        if obj.contains_key(*keyword) {
            out.push(format!(
                "{pointer}: `{keyword}` is not in the strict subset"
            ));
        }
    }

    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        match obj.get("additionalProperties") {
            Some(Value::Bool(false)) => {}
            _ => out.push(format!(
                "{pointer}: an object needs `additionalProperties: false`"
            )),
        }

        let required: Vec<&str> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for name in props.keys() {
            if !required.contains(&name.as_str()) {
                out.push(format!("{pointer}: `{name}` must be listed in `required`"));
            }
        }
        for (name, sub) in props {
            walk(sub, &format!("{pointer}/properties/{name}"), out);
        }
    }

    for key in ["items", "additionalProperties"] {
        if let Some(sub) = obj.get(key) {
            walk(sub, &format!("{pointer}/{key}"), out);
        }
    }
    for key in ["anyOf", "$defs", "definitions"] {
        match obj.get(key) {
            Some(Value::Array(branches)) => {
                for (i, branch) in branches.iter().enumerate() {
                    walk(branch, &format!("{pointer}/{key}/{i}"), out);
                }
            }
            Some(Value::Object(entries)) => {
                for (name, sub) in entries {
                    walk(sub, &format!("{pointer}/{key}/{name}"), out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::adapt;
    use super::*;
    use serde_json::json;

    #[test]
    fn an_unadapted_schema_is_reported() {
        let violations = strict_violations(&json!({
            "type": "object",
            "properties": { "a": { "type": "string", "pattern": "^x$" } },
            "required": []
        }));
        assert!(violations.iter().any(|v| v.contains("`pattern`")));
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`additionalProperties: false`"))
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`a` must be listed in `required`"))
        );
    }

    #[test]
    fn adaptation_makes_a_schema_strict_safe() {
        let original = json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "pattern": "^x$", "format": "email" },
                "m": { "type": "object", "patternProperties": { "^k$": { "type": "string" } } },
                "u": { "oneOf": [{ "type": "string" }, { "type": "integer" }] }
            },
            "required": ["a"]
        });
        let adapted = adapt(&original).expect("adapts");
        assert_eq!(
            strict_violations(&adapted.schema),
            Vec::<String>::new(),
            "adapt must produce a strict-safe schema"
        );
    }
}
