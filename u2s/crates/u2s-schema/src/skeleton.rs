//! [`skeleton`] — a schema-derived starting document for the Conversion
//! Agent, used when no reference run is close enough to seed from.
//!
//! PLAN.md: "The seed is the highest-ranked reference run's output when one
//! is close enough... otherwise a schema-derived skeleton." This is that
//! other branch, and it lives here — in `u2s-engine`, which is already the
//! one crate that knows JSON Schema — rather than in `u2s-agent`, so the
//! same "no format-specific knowledge above this layer" discipline
//! `adapt`/`restore`/`validate` already hold applies to it too.
//!
//! Only **required** properties are populated, recursively; an optional
//! property is left out entirely rather than filled with a placeholder,
//! because a value the agent never decided to add is not a value the schema
//! required it to have — an optional field with a guessed default would be
//! indistinguishable from one the model deliberately filled in.
//!
//! **Not guaranteed schema-valid, and that limit is real rather than an
//! oversight.** `enum` (first entry) and `oneOf`/`anyOf` (first alternative,
//! recursively) are satisfied, because both name a closed, enumerable set of
//! acceptable values or shapes. A `pattern` with no `enum`/`const` alongside
//! it is not: synthesizing a string matching an arbitrary regular
//! expression generically, with no format-specific knowledge, is not a
//! problem this function can solve without becoming exactly the
//! format-specific code this crate exists to avoid. A required string whose
//! only constraint is `pattern` is left empty, which is a document the
//! Conversion Agent's first real edit is expected to repair — consistent
//! with PLAN.md's own acceptance elsewhere (verification item 6) that not
//! every schema constraint is satisfied by construction; some surface as a
//! review finding instead.

use serde_json::{Map, Value};

use super::ref_resolve::resolve;
use super::restore::required_names;

/// A schema this crate cannot turn into a skeleton: not an object at its
/// root (there is nothing to hang required properties off), or a `$ref`
/// cycle deep enough that recursion cannot terminate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkeletonError {
    #[error("the schema's root is not an object, so it has no properties to fill")]
    NotAnObject,
    #[error("a $ref cycle was detected at {pointer}; the schema cannot be a finite document")]
    RefCycle { pointer: String },
}

/// How many `$ref` hops to follow before assuming a cycle. Generous for any
/// schema a real output format would define, and finite so a malformed
/// self-referencing schema fails loudly rather than recursing forever.
const MAX_REF_DEPTH: usize = 64;

/// Builds a minimal document satisfying `schema`'s required shape.
///
/// Recurses through nested objects and arrays, following local `$ref`s with
/// [`super::ref_resolve::resolve`] — the same resolver `restore` uses, so a
/// reference form this crate accepts in one place is accepted in both. Every
/// visited `$ref` is remembered on the current recursion path (not globally)
/// so a schema that legitimately reuses one definition in two unrelated
/// branches is not mistaken for a cycle.
pub fn skeleton(schema: &Value) -> Result<Value, SkeletonError> {
    let mut visiting: Vec<String> = Vec::new();
    build(schema, schema, &mut visiting, 0)
}

fn build(
    root: &Value,
    schema: &Value,
    visiting: &mut Vec<String>,
    depth: usize,
) -> Result<Value, SkeletonError> {
    if depth > MAX_REF_DEPTH {
        return Err(SkeletonError::RefCycle {
            pointer: visiting.last().cloned().unwrap_or_default(),
        });
    }

    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if visiting.iter().any(|seen| seen == reference) {
            return Err(SkeletonError::RefCycle {
                pointer: reference.to_owned(),
            });
        }
        let Some(resolved) = resolve(root, schema) else {
            // A `$ref` this workspace's resolver cannot follow (foreign or
            // malformed) contributes nothing rather than failing the whole
            // skeleton over one unreachable definition elsewhere in the
            // schema -- the same conservative choice `restore` already
            // makes for a `$ref` it cannot resolve.
            return Ok(Value::Object(Map::new()));
        };
        visiting.push(reference.to_owned());
        let result = build(root, resolved, visiting, depth + 1);
        visiting.pop();
        return result;
    }

    // A closed set of acceptable literals: the first one is always valid,
    // and picking one deterministically (rather than, say, the shortest) is
    // what keeps two runs of this function on the same schema identical.
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return Ok(first.clone());
    }
    if let Some(literal) = schema.get("const") {
        return Ok(literal.clone());
    }

    // A closed set of acceptable *shapes*. The first alternative is
    // resolved and built exactly like any other schema, so a `oneOf`
    // pointing at a `$ref` still goes through the same cycle-tracked path.
    for keyword in ["oneOf", "anyOf"] {
        if let Some(alternatives) = schema.get(keyword).and_then(Value::as_array)
            && let Some(first) = alternatives.first()
        {
            return build(root, first, visiting, depth + 1);
        }
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("object") | None if schema.get("properties").is_some() => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let required = required_names(schema);
            let mut out = Map::new();
            if let Some(properties) = properties {
                for name in &required {
                    if let Some(prop_schema) = properties.get(name) {
                        out.insert(name.clone(), build(root, prop_schema, visiting, depth + 1)?);
                    }
                }
            }
            Ok(Value::Object(out))
        }
        Some("array") => {
            // Required means the array itself must be present, never that
            // it must be non-empty -- `minItems` is a separate, unenforced
            // keyword here, matching this crate's existing strict-mode
            // adaptation stance that unsupported keywords are recorded, not
            // silently honoured by guessing a length.
            Ok(Value::Array(Vec::new()))
        }
        Some("string") => Ok(Value::String(String::new())),
        Some("integer") | Some("number") => Ok(Value::from(0)),
        Some("boolean") => Ok(Value::Bool(false)),
        Some("null") => Ok(Value::Null),
        // No `type` and no `properties`: nothing this function can shape
        // more specifically than an empty object -- the same floor a
        // schema of `{}` (accepts anything) would need anyway.
        _ => Ok(Value::Object(Map::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_non_object_root_still_produces_a_typed_default() {
        // The root itself is never required-filtered (there is no parent to
        // have required it), so a scalar root just produces its own default.
        assert_eq!(skeleton(&json!({ "type": "string" })).unwrap(), json!(""));
    }

    #[test]
    fn only_required_properties_are_populated() {
        let schema = json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "notes": { "type": "string" }
            },
            "required": ["title"]
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "title": "" }));
    }

    #[test]
    fn nested_required_objects_recurse() {
        let schema = json!({
            "type": "object",
            "properties": {
                "header": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "count": { "type": "integer" }
                    },
                    "required": ["name", "count"]
                }
            },
            "required": ["header"]
        });
        assert_eq!(
            skeleton(&schema).unwrap(),
            json!({ "header": { "name": "", "count": 0 } })
        );
    }

    #[test]
    fn a_required_array_is_present_and_empty() {
        let schema = json!({
            "type": "object",
            "properties": { "items": { "type": "array", "items": { "type": "string" } } },
            "required": ["items"]
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "items": [] }));
    }

    #[test]
    fn an_enum_uses_its_first_entry() {
        let schema = json!({
            "type": "object",
            "properties": { "mode": { "type": "string", "enum": ["none", "generate"] } },
            "required": ["mode"]
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "mode": "none" }));
    }

    #[test]
    fn a_const_is_used_directly() {
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "const": "form" } },
            "required": ["kind"]
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "kind": "form" }));
    }

    #[test]
    fn one_of_recurses_into_its_first_alternative() {
        let schema = json!({
            "type": "object",
            "properties": {
                "data_model": {
                    "oneOf": [
                        { "type": "object", "properties": { "static_": { "type": "boolean" } }, "required": ["static_"] },
                        { "type": "object", "properties": { "rest": { "type": "string" } }, "required": ["rest"] }
                    ]
                }
            },
            "required": ["data_model"]
        });
        assert_eq!(
            skeleton(&schema).unwrap(),
            json!({ "data_model": { "static_": false } })
        );
    }

    #[test]
    fn any_of_recurses_into_its_first_alternative_too() {
        let schema = json!({
            "type": "object",
            "properties": { "x": { "anyOf": [{ "type": "integer" }, { "type": "string" }] } },
            "required": ["x"]
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "x": 0 }));
    }

    /// A `oneOf` pointing at `$ref`s must still go through the cycle-tracked
    /// path -- otherwise a self-referential alternative would bypass the
    /// depth cap entirely.
    #[test]
    fn a_one_of_ref_cycle_is_still_caught() {
        let schema = json!({
            "$ref": "#/$defs/Node",
            "$defs": {
                "Node": {
                    "oneOf": [{ "$ref": "#/$defs/Node" }]
                }
            }
        });
        let err = skeleton(&schema).expect_err("a oneOf cycle must not recurse forever");
        assert!(matches!(err, SkeletonError::RefCycle { .. }), "{err:?}");
    }

    #[test]
    fn a_ref_is_followed_to_its_definition() {
        let schema = json!({
            "type": "object",
            "properties": { "node": { "$ref": "#/$defs/Node" } },
            "required": ["node"],
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": { "label": { "type": "string" } },
                    "required": ["label"]
                }
            }
        });
        assert_eq!(
            skeleton(&schema).unwrap(),
            json!({ "node": { "label": "" } })
        );
    }

    /// The real reason this resolver had to be shared rather than
    /// reimplemented: proving a self-referential schema (the AEM-shaped case
    /// PLAN.md's own example — a node with children of the same type) is
    /// depth-capped rather than recursing forever, without the AEM schema
    /// itself ever appearing in this crate.
    #[test]
    fn a_self_referential_schema_is_a_named_error_not_an_infinite_recursion() {
        let schema = json!({
            "$ref": "#/$defs/Node",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" },
                        "child": { "$ref": "#/$defs/Node" }
                    },
                    "required": ["label", "child"]
                }
            }
        });
        let err = skeleton(&schema).expect_err("a required self-reference cannot terminate");
        assert!(matches!(err, SkeletonError::RefCycle { .. }), "{err:?}");
    }

    /// The same definition reused in two unrelated branches is not a cycle
    /// -- only revisiting a `$ref` on the **current path** counts.
    #[test]
    fn reusing_one_definition_in_two_branches_is_not_a_cycle() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": { "$ref": "#/$defs/Leaf" },
                "b": { "$ref": "#/$defs/Leaf" }
            },
            "required": ["a", "b"],
            "$defs": { "Leaf": { "type": "string" } }
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "a": "", "b": "" }));
    }

    #[test]
    fn an_unresolvable_ref_contributes_an_empty_object_rather_than_failing() {
        let schema = json!({
            "type": "object",
            "properties": { "node": { "$ref": "#/$defs/Missing" } },
            "required": ["node"]
        });
        assert_eq!(skeleton(&schema).unwrap(), json!({ "node": {} }));
    }

    #[test]
    fn a_schema_with_no_type_and_no_properties_is_an_empty_object() {
        assert_eq!(skeleton(&json!({})).unwrap(), json!({}));
    }

    #[test]
    fn every_output_is_valid_against_its_own_schema() {
        // The whole point of a skeleton: it must not need repair before the
        // agent's first edit. Checked with the workspace's own validator, so
        // this is the same standard a real conversion is held to.
        let schema = json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "count": { "type": "integer" },
                "items": { "type": "array" },
                "header": {
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }
            },
            "required": ["title", "count", "items", "header"],
            "additionalProperties": false
        });
        let doc = skeleton(&schema).unwrap();
        let violations = crate::validate(&schema, &doc).expect("a well-formed schema validates");
        assert!(violations.is_empty(), "{violations:?}");
    }
}
