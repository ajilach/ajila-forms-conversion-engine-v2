//! Schemas generated from Rust types, never written by hand.
//!
//! Two kinds, each with one generator, so the settings that make a schema
//! acceptable to a provider are stated once:
//!
//! - [`tool_input_schema`]: the arguments a tool accepts. Tool schemas are
//!   offered to the model as they are, without [`crate::adapt`], so they must
//!   already be in the shape every provider accepts for function parameters
//!   (see [`tool_schema_violations`]). Descriptions are kept: they are what
//!   the model reads about each argument.
//! - [`answer_schema`]: a structured answer. These always go through
//!   [`crate::adapt`], which handles `$defs` and unions. Descriptions are
//!   removed: on an answer type they are maintainer notes, and the model's
//!   instructions live in the agent's prompt.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use serde_json::{Map, Value};

/// The input schema of a tool whose arguments deserialize into `T`.
///
/// Every subschema is inlined (no `$ref`), and an `Option` field is an
/// optional property that does not accept `null`: a model told it may send
/// `null` fills every optional with it.
///
/// Only field and variant doc comments become descriptions. A type's own
/// doc comment is a maintainer note (`RuleId`'s names its table) and is never
/// sent. schemars keeps a type's doc on its `$defs` entry and a field's doc
/// at the `$ref` site, which is why this generates with references and
/// inlines them itself instead of using `inline_subschemas`.
pub fn tool_input_schema<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::draft2020_12();
    settings.meta_schema = None;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let Value::Object(mut root) =
        serde_json::to_value(schema).expect("a generated schema serializes")
    else {
        panic!(
            "{}: a tool's arguments must be an object",
            std::any::type_name::<T>()
        );
    };
    let defs: Map<String, Value> = match root.remove("$defs") {
        Some(Value::Object(defs)) => defs
            .into_iter()
            .map(|(name, mut def)| {
                if let Value::Object(def) = &mut def {
                    def.remove("description");
                }
                (name, def)
            })
            .collect(),
        _ => Map::new(),
    };
    root.remove("title");
    root.remove("description");
    // A tool without arguments still declares an (empty) `properties`:
    // some providers refuse function parameters without one.
    root.entry("properties")
        .or_insert_with(|| Value::Object(Map::new()));
    let inlined = inline_refs(Value::Object(root), &defs, &mut Vec::new());
    unwrapped_descriptions(without_optional_null(inlined))
}

/// The `format`s schemars adds for a Rust number's width. They are not
/// JSON Schema formats, and a `minimum` already says what they mean.
const WIDTH_FORMATS: &[&str] = &[
    "int8", "int16", "int32", "int64", "int", "uint8", "uint16", "uint32", "uint64", "uint",
    "float", "double",
];

/// Every `description` with the line breaks of a wrapped doc comment
/// joined by a space (a blank line, a paragraph break, is kept), no
/// number-width `format`, and no `default: null`, which `#[serde(default)]`
/// on an `Option` produces and which only means "absent".
fn unwrapped_descriptions(node: Value) -> Value {
    match node {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .filter(|(key, sub)| {
                    let width_format =
                        key == "format" && sub.as_str().is_some_and(|f| WIDTH_FORMATS.contains(&f));
                    let null_default = key == "default" && sub.is_null();
                    !(width_format || null_default)
                })
                .map(|(key, sub)| match (key.as_str(), sub) {
                    ("description", Value::String(text)) => {
                        (key, Value::String(unwrap_lines(&text)))
                    }
                    (_, sub) => (key, unwrapped_descriptions(sub)),
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(unwrapped_descriptions).collect())
        }
        other => other,
    }
}

fn unwrap_lines(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| {
            paragraph
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `node` with every `#/$defs/..` reference replaced by its definition.
/// Keys next to the `$ref` (a field's description) win over the
/// definition's. `stack` names the definitions being inlined, so a
/// recursive argument type is a loud failure rather than endless recursion:
/// tool arguments are never recursive, and a schema that needed `$ref` could
/// not be offered as one anyway.
fn inline_refs(node: Value, defs: &Map<String, Value>, stack: &mut Vec<String>) -> Value {
    match node {
        Value::Object(mut obj) => {
            if let Some(Value::String(reference)) = obj.remove("$ref") {
                let name = reference
                    .strip_prefix("#/$defs/")
                    .unwrap_or_else(|| panic!("unexpected reference {reference}"))
                    .to_owned();
                assert!(
                    !stack.contains(&name),
                    "tool arguments must not be recursive: {name}"
                );
                let def = defs
                    .get(&name)
                    .unwrap_or_else(|| panic!("no definition for {reference}"))
                    .clone();
                stack.push(name);
                let inlined = inline_refs(def, defs, stack);
                stack.pop();
                if let Value::Object(def) = inlined {
                    for (key, sub) in def {
                        obj.entry(key).or_insert(sub);
                    }
                }
            }
            Value::Object(
                obj.into_iter()
                    .map(|(key, sub)| (key, inline_refs(sub, defs, stack)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|sub| inline_refs(sub, defs, stack))
                .collect(),
        ),
        other => other,
    }
}

/// The schema of a structured answer that deserializes into `T`.
pub fn answer_schema<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .with_transform(RecursiveTransform(|schema: &mut schemars::Schema| {
            schema.remove("description");
        }))
        .into_generator()
        .into_root_schema_for::<T>();
    serde_json::to_value(schema).expect("a generated schema serializes")
}

/// A tool call's arguments read into the tool's argument type `T`, the type
/// its [`tool_input_schema`] was generated from. The error is serde's text,
/// which names the argument; each server wraps it in its own error type.
///
/// Bounds and other schema-only constraints are not checked here: an agent
/// checks every call against the schema before it reaches a tool.
pub fn parse_tool_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, String> {
    T::deserialize(args).map_err(|err| err.to_string())
}

/// The arguments of a tool that takes none.
#[derive(Debug, Default, serde::Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoArgs {}

/// Reasons a tool input schema would be refused as function parameters,
/// each naming a JSON Pointer into it. Empty means acceptable.
///
/// The top level must be a plain object: OpenAI refuses `anyOf`, `oneOf`,
/// `allOf`, `enum` and `not` there. A `$ref` anywhere is refused too, since
/// tool schemas are sent without the `$defs` handling answers get.
pub fn tool_schema_violations(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(root) = schema.as_object() else {
        return vec!["the schema must be an object".to_owned()];
    };
    if root.get("type").and_then(Value::as_str) != Some("object") {
        out.push("the top level must be `type: object`".to_owned());
    }
    for keyword in ["anyOf", "oneOf", "allOf", "enum", "not"] {
        if root.contains_key(keyword) {
            out.push(format!("`{keyword}` is not allowed at the top level"));
        }
    }
    collect_refs(schema, "", &mut out);
    out
}

fn collect_refs(node: &Value, pointer: &str, out: &mut Vec<String>) {
    match node {
        Value::Object(obj) => {
            if obj.contains_key("$ref") {
                out.push(format!("{pointer}: `$ref` is not allowed in a tool schema"));
            }
            for (key, sub) in obj {
                collect_refs(sub, &format!("{pointer}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (i, sub) in items.iter().enumerate() {
                collect_refs(sub, &format!("{pointer}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// Removes the `null` schemars adds to each optional property's type, at
/// every depth. A required property keeps it: there `null` is a value the
/// type genuinely accepts.
fn without_optional_null(node: Value) -> Value {
    match node {
        Value::Object(mut obj) => {
            let required: Vec<String> = obj
                .get("required")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if let Some(Value::Object(props)) = obj.get_mut("properties") {
                for (name, sub) in props.iter_mut() {
                    if !required.contains(name) {
                        *sub = drop_null(sub.take());
                    }
                }
            }
            Value::Object(
                obj.into_iter()
                    .map(|(key, sub)| (key, without_optional_null(sub)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(without_optional_null).collect()),
        other => other,
    }
}

/// `schema` without `null` among the types it accepts. schemars spells an
/// `Option<T>` as `type: [T, "null"]` or as `anyOf: [T, {type: null}]`.
fn drop_null(schema: Value) -> Value {
    let Value::Object(mut obj) = schema else {
        return schema;
    };
    if let Some(Value::Array(types)) = obj.get("type") {
        let kept: Vec<Value> = types
            .iter()
            .filter(|t| t.as_str() != Some("null"))
            .cloned()
            .collect();
        let collapsed = match kept.as_slice() {
            [only] => only.clone(),
            _ => Value::Array(kept),
        };
        obj.insert("type".to_owned(), collapsed);
    }
    if let Some(Value::Array(branches)) = obj.get("anyOf") {
        let kept: Vec<Value> = branches
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) != Some("null"))
            .cloned()
            .collect();
        if let [only] = kept.as_slice() {
            obj.remove("anyOf");
            // The one remaining branch becomes the property's schema; keys
            // the wrapper carried (its description) win over the branch's.
            if let Value::Object(branch) = only {
                for (key, sub) in branch {
                    obj.entry(key.clone()).or_insert_with(|| sub.clone());
                }
            }
        } else {
            obj.insert("anyOf".to_owned(), Value::Array(kept));
        }
    }
    if let Some(Value::Array(values)) = obj.get("enum") {
        let kept: Vec<Value> = values.iter().filter(|v| !v.is_null()).cloned().collect();
        obj.insert("enum".to_owned(), Value::Array(kept));
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(dead_code)]
    #[derive(serde::Deserialize, JsonSchema)]
    #[schemars(deny_unknown_fields)]
    struct Args {
        /// The page to read.
        page: u32,
        /// How many characters.
        #[schemars(range(min = 1))]
        limit: Option<u32>,
        rect: Option<Rect>,
        format: Option<Format>,
        #[serde(default)]
        tags: Option<Vec<String>>,
    }

    /// A maintainer note on the type, never sent.
    #[allow(dead_code)]
    #[derive(serde::Deserialize, JsonSchema)]
    struct Rect {
        x: f32,
        y: Option<f32>,
    }

    /// A maintainer note on the type, never sent.
    #[allow(dead_code)]
    #[derive(serde::Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    enum Format {
        Jpeg,
        Png,
    }

    /// A maintainer note on the type, never sent.
    #[allow(dead_code)]
    #[derive(serde::Deserialize, JsonSchema)]
    #[serde(tag = "mode", rename_all = "snake_case")]
    enum Selection {
        /// Every rule.
        All,
        /// Only these.
        Only { ids: Vec<String> },
    }

    #[allow(dead_code)]
    #[derive(serde::Deserialize, JsonSchema)]
    struct Nested {
        /// Which rules apply.
        rules: Selection,
        plain: Rect,
    }

    #[allow(dead_code)]
    #[derive(serde::Deserialize, JsonSchema)]
    struct Wrapped {
        /// A description long enough
        /// to wrap.
        ///
        /// A second paragraph.
        a: String,
    }

    #[test]
    fn a_wrapped_doc_comment_is_one_line_per_paragraph() {
        let schema = tool_input_schema::<Wrapped>();
        assert_eq!(
            schema["properties"]["a"]["description"],
            json!("A description long enough to wrap.\n\nA second paragraph.")
        );
    }

    #[test]
    fn only_field_and_variant_docs_become_descriptions() {
        let schema = tool_input_schema::<Nested>();
        assert_eq!(tool_schema_violations(&schema), Vec::<String>::new());
        assert!(!schema.to_string().contains("maintainer note"), "{schema}");
        assert_eq!(
            schema["properties"]["rules"]["description"],
            json!("Which rules apply.")
        );
        assert!(schema.to_string().contains("Every rule."), "{schema}");
        assert!(schema["properties"]["plain"].get("description").is_none());
        assert_eq!(
            schema["properties"]["plain"]["properties"]["x"]["type"],
            json!("number")
        );
    }

    #[test]
    fn a_tool_schema_is_flat_and_never_offers_null() {
        let schema = tool_input_schema::<Args>();
        assert_eq!(tool_schema_violations(&schema), Vec::<String>::new());
        assert!(schema.get("title").is_none() && schema.get("$schema").is_none());
        assert_eq!(schema["required"], json!(["page"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["properties"]["page"]["description"],
            json!("The page to read.")
        );
        assert_eq!(schema["properties"]["limit"]["type"], json!("integer"));
        assert!(
            schema["properties"]["limit"].get("format").is_none(),
            "{schema}"
        );
        assert_eq!(schema["properties"]["limit"]["minimum"], json!(1));
        assert_eq!(schema["properties"]["rect"]["type"], json!("object"));
        assert_eq!(
            schema["properties"]["rect"]["properties"]["y"]["type"],
            json!("number")
        );
        assert_eq!(
            schema["properties"]["format"]["enum"],
            json!(["jpeg", "png"])
        );
        assert!(!schema.to_string().contains("null"), "{schema}");
    }

    #[test]
    fn a_tool_with_no_arguments_is_an_empty_object() {
        let schema = tool_input_schema::<NoArgs>();
        assert_eq!(tool_schema_violations(&schema), Vec::<String>::new());
        assert_eq!(schema["properties"], json!({}));
    }

    #[test]
    fn a_top_level_union_or_a_ref_is_refused() {
        let violations = tool_schema_violations(&json!({
            "anyOf": [{ "type": "object" }],
            "properties": { "a": { "$ref": "#/$defs/A" } }
        }));
        assert!(
            violations.iter().any(|v| v.contains("`type: object`")),
            "{violations:?}"
        );
        assert!(
            violations.iter().any(|v| v.contains("`anyOf`")),
            "{violations:?}"
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contains("/properties/a: `$ref`")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_answer_schema_has_no_descriptions() {
        let schema = answer_schema::<Args>();
        assert!(
            !schema.to_string().contains("The page to read."),
            "{schema}"
        );
    }
}
