//! Rewriting an arbitrary JSON Schema into the strict subset. See the
//! module docs in `mod.rs` for why, and for the two rules this follows.

use super::{MAP_KEY, MAP_VALUE};
use serde_json::{Map, Value, json};

/// Keywords the strict subset cannot express. Dropping any of these widens the
/// set of accepted documents, which is safe because [`validate`] re-checks
/// against the original schema.
const STRIPPED: &[&str] = &[
    // strings
    "minLength",
    "maxLength",
    "pattern",
    "format",
    // numbers
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    // arrays
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
    "minContains",
    "maxContains",
    "unevaluatedItems",
    // objects
    "minProperties",
    "maxProperties",
    "propertyNames",
    "dependentRequired",
    "dependentSchemas",
    "unevaluatedProperties",
    // annotations the provider ignores and that only cost prompt tokens
    "default",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
];

/// Keywords whose removal would change meaning rather than widen it.
const REFUSED: &[&str] = &["allOf", "not", "if", "then", "else"];

/// Something [`adapt`] had to give up, recorded so it can be reported rather
/// than discovered later. `pointer` is a JSON Pointer into the *schema*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relaxation {
    /// A constraint keyword the strict subset cannot express.
    KeywordStripped {
        pointer: String,
        keyword: &'static str,
    },
    /// `oneOf` rewritten to `anyOf`: the provider has no exclusive union, so
    /// "exactly one branch" becomes "at least one".
    OneOfWidened { pointer: String },
    /// An originally-optional property made required-and-nullable.
    OptionalWidened { pointer: String, property: String },
    /// A map re-encoded as an array of `{key, value}` pairs.
    MapAsPairs { pointer: String },
}

/// A schema [`adapt`] cannot make strict-safe without changing its meaning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdaptError {
    #[error(
        "{pointer}: `{keyword}` cannot be expressed in the strict subset, and dropping it would change the schema's meaning"
    )]
    Unsupported {
        pointer: String,
        keyword: &'static str,
    },
    #[error(
        "{pointer}: an object with both `properties` and a schema-valued `additionalProperties` is ambiguous under strict mode"
    )]
    OpenObject { pointer: String },
    #[error("{pointer}: a map must declare a value schema")]
    MapWithoutValueSchema { pointer: String },
}

/// A strict-safe schema plus everything given up to get there.
#[derive(Debug, Clone, PartialEq)]
pub struct Adapted {
    pub schema: Value,
    pub relaxations: Vec<Relaxation>,
}

impl Adapted {
    /// The relaxations that weaken validation, i.e. the ones whose violations
    /// [`validate`] has to catch. Excludes the re-encodings, which lose nothing.
    pub fn weakening(&self) -> impl Iterator<Item = &Relaxation> {
        self.relaxations.iter().filter(|r| {
            matches!(
                r,
                Relaxation::KeywordStripped { .. } | Relaxation::OneOfWidened { .. }
            )
        })
    }
}

/// Rewrites `original` into the strict subset.
///
/// Does not follow `$ref`: `$defs` are walked in place, so a recursive schema
/// terminates without cycle tracking.
pub fn adapt(original: &Value) -> Result<Adapted, AdaptError> {
    let mut relaxations = Vec::new();
    let schema = adapt_node(original, "", &mut relaxations)?;
    Ok(Adapted {
        schema,
        relaxations,
    })
}

fn adapt_node(node: &Value, pointer: &str, out: &mut Vec<Relaxation>) -> Result<Value, AdaptError> {
    let Value::Object(obj) = node else {
        // A boolean schema (`true`/`false`) or a non-schema leaf inside `enum`
        // or `const` passes through untouched.
        return Ok(node.clone());
    };

    for keyword in REFUSED {
        if obj.contains_key(*keyword) {
            return Err(AdaptError::Unsupported {
                pointer: pointer.to_owned(),
                keyword,
            });
        }
    }

    if is_map(obj) {
        return adapt_map(obj, pointer, out);
    }

    let mut result = Map::new();
    for (key, value) in obj {
        if STRIPPED.contains(&key.as_str()) {
            out.push(Relaxation::KeywordStripped {
                pointer: pointer.to_owned(),
                // The `&str`s in STRIPPED are `'static`, so this recovers the
                // static lifetime the variant wants.
                keyword: STRIPPED[STRIPPED
                    .iter()
                    .position(|k| k == key)
                    .expect("just matched")],
            });
            continue;
        }

        let child_ptr = |suffix: &str| format!("{pointer}/{suffix}");
        match key.as_str() {
            "properties" => {
                let props = value.as_object().cloned().unwrap_or_default();
                let mut adapted = Map::new();
                for (name, sub) in &props {
                    adapted.insert(
                        name.clone(),
                        adapt_node(sub, &child_ptr(&format!("properties/{name}")), out)?,
                    );
                }
                result.insert("properties".to_owned(), Value::Object(adapted));
            }
            "items" | "additionalProperties" | "propertyNames" => {
                result.insert(key.clone(), adapt_node(value, &child_ptr(key), out)?);
            }
            "anyOf" | "oneOf" => {
                let branches = value.as_array().cloned().unwrap_or_default();
                let mut adapted = Vec::with_capacity(branches.len());
                for (i, branch) in branches.iter().enumerate() {
                    adapted.push(adapt_node(branch, &child_ptr(&format!("{key}/{i}")), out)?);
                }
                if key == "oneOf" {
                    out.push(Relaxation::OneOfWidened {
                        pointer: pointer.to_owned(),
                    });
                }
                result.insert("anyOf".to_owned(), Value::Array(adapted));
            }
            "$defs" | "definitions" => {
                let defs = value.as_object().cloned().unwrap_or_default();
                let mut adapted = Map::new();
                for (name, sub) in &defs {
                    adapted.insert(
                        name.clone(),
                        adapt_node(sub, &child_ptr(&format!("{key}/{name}")), out)?,
                    );
                }
                result.insert(key.clone(), Value::Object(adapted));
            }
            _ => {
                result.insert(key.clone(), value.clone());
            }
        }
    }

    if result.contains_key("properties") {
        close_object(&mut result, obj, pointer, out);
    }

    Ok(Value::Object(result))
}

/// Applies strict mode's two object rules: `additionalProperties: false`, and
/// every property listed in `required` — with the originally-optional ones
/// widened to accept `null` so "absent" stays expressible.
fn close_object(
    result: &mut Map<String, Value>,
    original: &Map<String, Value>,
    pointer: &str,
    out: &mut Vec<Relaxation>,
) {
    let was_required: Vec<String> = original
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let names: Vec<String> = result["properties"]
        .as_object()
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default();

    for name in &names {
        if was_required.contains(name) {
            continue;
        }
        out.push(Relaxation::OptionalWidened {
            pointer: pointer.to_owned(),
            property: name.clone(),
        });
        let props = result
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .expect("checked above");
        let sub = props.get_mut(name).expect("name came from this map");
        *sub = allow_null(sub.take());
    }

    result.insert(
        "required".to_owned(),
        Value::Array(names.into_iter().map(Value::String).collect()),
    );
    result.insert("additionalProperties".to_owned(), Value::Bool(false));
}

/// Widens a subschema to accept `null`, cheaply where the node carries a plain
/// `type`, by wrapping otherwise. Already-nullable nodes are left alone so
/// schemars' own `Option<T>` rendering does not get double-wrapped.
fn allow_null(node: Value) -> Value {
    if accepts_null(&node) {
        return node;
    }
    if let Value::Object(mut obj) = node {
        match obj.get("type") {
            Some(Value::String(_)) if plain_typed(&obj) => {
                let existing = obj.remove("type").expect("matched");
                obj.insert("type".to_owned(), json!([existing, "null"]));
                return Value::Object(obj);
            }
            Some(Value::Array(types)) => {
                let mut types = types.clone();
                types.push(Value::String("null".to_owned()));
                obj.insert("type".to_owned(), Value::Array(types));
                return Value::Object(obj);
            }
            _ => return json!({ "anyOf": [Value::Object(obj), { "type": "null" }] }),
        }
    }
    json!({ "anyOf": [node, { "type": "null" }] })
}

/// Whether a node's `type` is the whole story, so extending the type array is
/// equivalent to a union. `enum` and `const` restrict values independently of
/// `type`, so those must be wrapped instead.
fn plain_typed(obj: &Map<String, Value>) -> bool {
    !obj.contains_key("enum") && !obj.contains_key("const") && !obj.contains_key("$ref")
}

fn accepts_null(node: &Value) -> bool {
    let Value::Object(obj) = node else {
        return false;
    };
    match obj.get("type") {
        Some(Value::String(t)) => t == "null",
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some("null")),
        _ => obj
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| branches.iter().any(accepts_null)),
    }
}

/// A map is an object whose keys are open: `patternProperties`, or a
/// schema-valued `additionalProperties`, with no fixed `properties`.
fn is_map(obj: &Map<String, Value>) -> bool {
    if obj.contains_key("properties") {
        return false;
    }
    obj.contains_key("patternProperties")
        || matches!(obj.get("additionalProperties"), Some(Value::Object(_)))
}

fn adapt_map(
    obj: &Map<String, Value>,
    pointer: &str,
    out: &mut Vec<Relaxation>,
) -> Result<Value, AdaptError> {
    let value_schema = map_value_schema(obj).ok_or_else(|| AdaptError::MapWithoutValueSchema {
        pointer: pointer.to_owned(),
    })?;
    let adapted_value = adapt_node(&value_schema, &format!("{pointer}/{MAP_VALUE}"), out)?;

    out.push(Relaxation::MapAsPairs {
        pointer: pointer.to_owned(),
    });

    let mut pairs = json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                MAP_KEY: { "type": "string" },
                MAP_VALUE: adapted_value,
            },
            "required": [MAP_KEY, MAP_VALUE],
            "additionalProperties": false,
        }
    });
    if let Some(description) = obj.get("description") {
        pairs["description"] = description.clone();
    }
    Ok(pairs)
}

/// The schema every value in a map must satisfy. Several `patternProperties`
/// patterns become an `anyOf`, since a generated key may match any of them.
fn map_value_schema(obj: &Map<String, Value>) -> Option<Value> {
    if let Some(Value::Object(patterns)) = obj.get("patternProperties") {
        let mut branches: Vec<Value> = patterns.values().cloned().collect();
        return match branches.len() {
            0 => None,
            1 => Some(branches.remove(0)),
            _ => Some(json!({ "anyOf": branches })),
        };
    }
    match obj.get("additionalProperties") {
        Some(Value::Object(schema)) => Some(Value::Object(schema.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapted(schema: Value) -> Adapted {
        adapt(&schema).expect("adapts")
    }

    #[test]
    fn every_property_becomes_required_and_optionals_accept_null() {
        let out = adapted(json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "integer" } },
            "required": ["a"]
        }));

        assert_eq!(out.schema["required"], json!(["a", "b"]));
        assert_eq!(out.schema["additionalProperties"], json!(false));
        assert_eq!(out.schema["properties"]["a"]["type"], json!("string"));
        assert_eq!(
            out.schema["properties"]["b"]["type"],
            json!(["integer", "null"]),
            "an originally-optional property must stay expressible as absent"
        );
        assert!(out.relaxations.contains(&Relaxation::OptionalWidened {
            pointer: String::new(),
            property: "b".to_owned()
        }));
    }

    #[test]
    fn an_already_nullable_optional_is_not_double_wrapped() {
        let out = adapted(json!({
            "type": "object",
            "properties": { "a": { "type": ["string", "null"] } },
            "required": []
        }));
        assert_eq!(
            out.schema["properties"]["a"]["type"],
            json!(["string", "null"])
        );
    }

    #[test]
    fn an_optional_ref_is_wrapped_rather_than_type_extended() {
        let out = adapted(json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/T" } }
        }));
        assert_eq!(
            out.schema["properties"]["a"],
            json!({ "anyOf": [{ "$ref": "#/$defs/T" }, { "type": "null" }] }),
            "a $ref carries no `type` to extend"
        );
    }

    #[test]
    fn an_optional_enum_is_wrapped_rather_than_type_extended() {
        let out = adapted(json!({
            "type": "object",
            "properties": { "a": { "type": "string", "enum": ["x", "y"] } }
        }));
        assert_eq!(
            out.schema["properties"]["a"]["anyOf"][0]["enum"],
            json!(["x", "y"]),
            "extending `type` would not add null to the enum's value set"
        );
    }

    #[test]
    fn constraint_keywords_are_stripped_and_recorded() {
        let out = adapted(json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "pattern": "^x$", "minLength": 1 },
                "n": { "type": "integer", "minimum": 3 }
            },
            "required": ["a", "n"]
        }));

        let a = &out.schema["properties"]["a"];
        assert!(a.get("pattern").is_none() && a.get("minLength").is_none());
        assert!(out.schema["properties"]["n"].get("minimum").is_none());

        let stripped: Vec<_> = out
            .relaxations
            .iter()
            .filter_map(|r| match r {
                Relaxation::KeywordStripped { pointer, keyword } => {
                    Some((pointer.as_str(), *keyword))
                }
                _ => None,
            })
            .collect();
        assert!(stripped.contains(&("/properties/a", "pattern")));
        assert!(stripped.contains(&("/properties/a", "minLength")));
        assert!(stripped.contains(&("/properties/n", "minimum")));
    }

    #[test]
    fn one_of_becomes_any_of() {
        let out = adapted(json!({
            "oneOf": [
                { "type": "object", "properties": { "k": { "const": "a" } }, "required": ["k"] },
                { "type": "object", "properties": { "k": { "const": "b" } }, "required": ["k"] }
            ]
        }));
        assert!(out.schema.get("oneOf").is_none());
        assert_eq!(out.schema["anyOf"].as_array().expect("array").len(), 2);
        assert!(out.relaxations.contains(&Relaxation::OneOfWidened {
            pointer: String::new()
        }));
    }

    #[test]
    fn a_pattern_properties_map_becomes_pairs() {
        let out = adapted(json!({
            "type": "object",
            "additionalProperties": false,
            "patternProperties": {
                "^[a-z]{2}$": { "type": "string", "minLength": 1 }
            }
        }));

        assert_eq!(out.schema["type"], json!("array"));
        let item = &out.schema["items"];
        assert_eq!(item["properties"][MAP_KEY]["type"], json!("string"));
        assert_eq!(item["properties"][MAP_VALUE]["type"], json!("string"));
        assert_eq!(item["required"], json!([MAP_KEY, MAP_VALUE]));
        assert!(
            item["properties"][MAP_VALUE].get("minLength").is_none(),
            "the value schema is adapted too"
        );
        assert!(out.relaxations.contains(&Relaxation::MapAsPairs {
            pointer: String::new()
        }));
    }

    #[test]
    fn an_additional_properties_map_becomes_pairs() {
        let out = adapted(json!({
            "type": "object",
            "additionalProperties": { "type": "integer" }
        }));
        assert_eq!(out.schema["type"], json!("array"));
        assert_eq!(
            out.schema["items"]["properties"][MAP_VALUE]["type"],
            json!("integer")
        );
    }

    #[test]
    fn defs_are_adapted_in_place_so_recursion_terminates() {
        let out = adapted(json!({
            "$ref": "#/$defs/Node",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "children": { "type": "array", "items": { "$ref": "#/$defs/Node" } }
                    }
                }
            }
        }));
        assert_eq!(out.schema["$defs"]["Node"]["required"], json!(["children"]));
        assert_eq!(
            out.schema["$defs"]["Node"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn all_of_is_refused_by_pointer() {
        let err = adapt(&json!({
            "type": "object",
            "properties": { "a": { "allOf": [{ "type": "string" }] } }
        }))
        .expect_err("allOf cannot be loosened");
        assert_eq!(
            err,
            AdaptError::Unsupported {
                pointer: "/properties/a".to_owned(),
                keyword: "allOf"
            }
        );
    }

    #[test]
    fn weakening_excludes_the_re_encodings() {
        let out = adapted(json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "pattern": "^x$" },
                "m": { "type": "object", "additionalProperties": { "type": "string" } }
            },
            "required": ["a", "m"]
        }));
        let weakening: Vec<_> = out.weakening().collect();
        assert_eq!(
            weakening.len(),
            1,
            "the map re-encoding loses nothing: {:?}",
            out.relaxations
        );
    }
}
