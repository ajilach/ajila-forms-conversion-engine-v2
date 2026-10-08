//! AEM's rule storage, one layer at a time.
//!
//! A rule on an Adaptive Form component lives in an attribute of the
//! component's `fd:scripts` (code editor) or `fd:rules` (visual editor)
//! child, named after the event (`fd:click`, `fd:init`, ...). Its value is
//! three encodings deep:
//!
//! 1. an XML attribute value (`&quot;`, `&amp;`, `&#xa;`, ...), which
//!    [`crate::xml_writer`] writes and any XML reader undoes;
//! 2. a JCR multi-value, `[item,item]`, in which a `,` or `\` that belongs to
//!    an item is escaped as `\,` or `\\` ([`crate::jcr::multi_value`]);
//! 3. one JSON object per item. The code editor's is a SCRIPTMODEL:
//!    `{"script":{"field":..,"event":..,"model":..,"content":..},
//!    "nodeName":"SCRIPTMODEL","version":1,"enabled":true}`, whose `content`
//!    is the JavaScript as a JSON string.
//!
//! This module is layers 2 and 3. It decodes exactly, keeping every item as
//! the bytes it was written as, because the deployed forms spell the same
//! object several ways (key orders, pretty-printed JSON) and a re-encoding
//! must not rewrite a rule nobody edited. [`EventScript`] is the typed view
//! of the one spelling this encoder writes; an item is recognised as one only
//! when writing it back reproduces it byte for byte.

use serde::Serialize;
use std::fmt;

/// Why an attribute value is not a list of rule objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptListError {
    /// Not a JCR multi-value: no surrounding `[` `]`.
    NotAList,
    /// A backslash in the item at `index` escapes neither `,` nor `\`: no
    /// encoder writes one, so the value was edited by hand or corrupted, and
    /// reading it would change it.
    StrayEscape { index: usize },
    /// The item at `index` is empty, as in `[a,,b]`.
    EmptyItem { index: usize },
    /// Two objects ran together in one item because the comma between them
    /// was escaped (`{..}\,{..}`): AEM reads them as one malformed item. The
    /// deployed corpus carries this defect, so it is named rather than
    /// reported as generic bad JSON.
    EscapedSeparator { index: usize },
    /// The item at `index` is not a JSON object.
    NotAnObject { index: usize, message: String },
}

impl fmt::Display for ScriptListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAList => write!(f, "not a JCR multi-value `[...]`"),
            Self::StrayEscape { index } => {
                write!(f, "item {index} has a backslash that escapes neither `,` nor `\\`")
            }
            Self::EmptyItem { index } => write!(f, "item {index} is empty"),
            Self::EscapedSeparator { index } => write!(
                f,
                "item {index} holds two rule objects joined by an escaped comma `\\,`"
            ),
            Self::NotAnObject { index, message } => {
                write!(f, "item {index} is not a JSON object: {message}")
            }
        }
    }
}

impl std::error::Error for ScriptListError {}

/// The items of a JCR multi-value, unescaped, exactly: the inverse of
/// [`crate::jcr::multi_value`]. Unlike [`crate::jcr::value::parse_jcr_array`]
/// it neither trims nor drops blank items, and it refuses a backslash that
/// escapes nothing, so `multi_value(split(v)) == v` for every `v` it accepts.
/// `[]` is the empty list.
pub fn split_multi_value(value: &str) -> Result<Vec<String>, ScriptListError> {
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or(ScriptListError::NotAList)?;
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(next @ (',' | '\\')) => current.push(next),
                _ => return Err(ScriptListError::StrayEscape { index: items.len() }),
            },
            ',' => items.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    items.push(current);
    Ok(items)
}

/// The rule objects of an `fd:scripts`/`fd:rules` attribute value (already
/// XML-unescaped), each as the JSON text it was written as.
pub fn decode_script_list(value: &str) -> Result<Vec<String>, ScriptListError> {
    let items = split_multi_value(value)?;
    for (index, item) in items.iter().enumerate() {
        if item.is_empty() {
            return Err(ScriptListError::EmptyItem { index });
        }
        if let Err(error) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(item)
        {
            return Err(if runs_into_another_object(item) {
                ScriptListError::EscapedSeparator { index }
            } else {
                ScriptListError::NotAnObject {
                    index,
                    message: error.to_string(),
                }
            });
        }
    }
    Ok(items)
}

/// Does `item` start with one complete JSON object followed by `,{`?
fn runs_into_another_object(item: &str) -> bool {
    let mut stream = serde_json::Deserializer::from_str(item).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(serde_json::Value::Object(_))) => {
            item[stream.byte_offset()..].trim_start().starts_with(",{")
        }
        _ => false,
    }
}

/// The attribute value (before XML escaping) holding `items`, each a JSON
/// rule object.
pub fn encode_script_list<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    crate::jcr::multi_value(items)
}

/// The events a code-editor rule runs on, each stored in its own attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptEvent {
    Initialize,
    Click,
    Visibility,
    ValueCommit,
    Calculate,
    Navigation,
    Enabled,
    Validate,
    Change,
}

impl ScriptEvent {
    pub const ALL: [ScriptEvent; 9] = [
        Self::Initialize,
        Self::Click,
        Self::Visibility,
        Self::ValueCommit,
        Self::Calculate,
        Self::Navigation,
        Self::Enabled,
        Self::Validate,
        Self::Change,
    ];

    /// The attribute of `fd:scripts` / `fd:rules` that holds this event's rules.
    pub fn attribute(self) -> &'static str {
        match self {
            Self::Initialize => "fd:init",
            Self::Click => "fd:click",
            Self::Visibility => "fd:visible",
            Self::ValueCommit => "fd:valueCommit",
            Self::Calculate => "fd:calc",
            Self::Navigation => "fd:navigationChange",
            Self::Enabled => "fd:enabled",
            Self::Validate => "fd:validate",
            Self::Change => "fd:change",
        }
    }

    /// The `event` a SCRIPTMODEL names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Initialize => "Initialize",
            Self::Click => "Click",
            Self::Visibility => "Visibility",
            Self::ValueCommit => "Value Commit",
            Self::Calculate => "Calculate",
            Self::Navigation => "Navigation",
            Self::Enabled => "Enabled",
            Self::Validate => "Validate",
            Self::Change => "Change",
        }
    }

    pub fn from_attribute(attribute: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.attribute() == attribute)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == name)
    }
}

/// The two key orders a code-editor SCRIPTMODEL's `script` is written in.
/// Both occur in the deployed forms, and the order is part of the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyOrder {
    /// `field, event, model, content`: the code editor's own order.
    FieldFirst,
    /// `content, event, field`: no `model`.
    ContentFirst,
}

/// One code-editor rule: `content` (JavaScript) run on `event` for the
/// component at `field` (its guide path, or `this`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventScript {
    pub field: String,
    pub event: ScriptEvent,
    pub content: String,
    pub order: BodyOrder,
    /// An `_archetype` marker after `enabled`, naming the generator of a
    /// rule that is regenerated rather than edited.
    pub archetype: Option<String>,
}

#[derive(Serialize)]
struct Model {
    #[serde(rename = "nodeName")]
    node_name: &'static str,
}

#[derive(Serialize)]
struct FieldFirstBody<'a> {
    field: &'a str,
    event: &'static str,
    model: Model,
    content: &'a str,
}

#[derive(Serialize)]
struct ContentFirstBody<'a> {
    content: &'a str,
    event: &'static str,
    field: &'a str,
}

#[derive(Serialize)]
struct ScriptModel<B> {
    script: B,
    #[serde(rename = "nodeName")]
    node_name: &'static str,
    version: u8,
    enabled: bool,
    #[serde(rename = "_archetype", skip_serializing_if = "Option::is_none")]
    archetype: Option<String>,
}

impl EventScript {
    /// The rule as one JSON object, compact, keys in [`Self::order`].
    pub fn to_json(&self) -> String {
        fn model<B: Serialize>(script: &EventScript, body: B) -> String {
            serde_json::to_string(&ScriptModel {
                script: body,
                node_name: "SCRIPTMODEL",
                version: 1,
                enabled: true,
                archetype: script.archetype.clone(),
            })
            .expect("a SCRIPTMODEL serialises")
        }
        match self.order {
            BodyOrder::FieldFirst => model(
                self,
                FieldFirstBody {
                    field: &self.field,
                    event: self.event.name(),
                    model: Model {
                        node_name: "EVENT_SCRIPTS",
                    },
                    content: &self.content,
                },
            ),
            BodyOrder::ContentFirst => model(
                self,
                ContentFirstBody {
                    content: &self.content,
                    event: self.event.name(),
                    field: &self.field,
                },
            ),
        }
    }

    /// The rule `json` spells, when it is exactly what [`Self::to_json`]
    /// writes for it; `None` for any other spelling, which a re-encoding must
    /// then carry as it is.
    pub fn from_json(json: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        let object = value.as_object()?;
        let script = object.get("script")?.as_object()?;
        let text = |key: &str| script.get(key)?.as_str().map(str::to_owned);
        let archetype = match object.get("_archetype") {
            None => None,
            Some(value) => Some(value.as_str()?.to_owned()),
        };
        let event = ScriptEvent::from_name(&text("event")?)?;
        let (field, content) = (text("field")?, text("content")?);
        [BodyOrder::FieldFirst, BodyOrder::ContentFirst]
            .into_iter()
            .map(|order| EventScript {
                field: field.clone(),
                event,
                content: content.clone(),
                order,
                archetype: archetype.clone(),
            })
            .find(|candidate| candidate.to_json() == json)
    }
}

/// How a rule object is spelled, apart from its JavaScript and the component
/// it runs on: its keys in the order written, its `script`'s keys, the event,
/// the model's `nodeName` and the `_archetype`. Two rules of one shape differ
/// only in what they do, not in how AEM stores them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleShape {
    pub keys: Vec<String>,
    pub script_keys: Vec<String>,
    pub event: Option<String>,
    pub model: Option<String>,
    pub archetype: Option<String>,
    /// Written without whitespace outside its strings.
    pub compact: bool,
}

impl RuleShape {
    /// The shape of one rule object, or `None` when `json` is not one.
    pub fn of(json: &str) -> Option<Self> {
        let ordered: ordered::Value = serde_json::from_str(json).ok()?;
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        let text = |pointer: &str| value.pointer(pointer).and_then(|v| v.as_str()).map(str::to_owned);
        Some(RuleShape {
            keys: ordered.keys(),
            script_keys: ordered.get("script").map(ordered::Value::keys).unwrap_or_default(),
            event: text("/script/event"),
            model: text("/script/model/nodeName"),
            archetype: text("/_archetype"),
            compact: is_compact(json),
        })
    }
}

/// Does `json` hold no whitespace outside its strings?
fn is_compact(json: &str) -> bool {
    let (mut in_string, mut escaped) = (false, false);
    for c in json.chars() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if c.is_whitespace() {
            return false;
        }
    }
    true
}

/// JSON with every object's keys in the order they were written, which
/// `serde_json::Value` does not keep.
mod ordered {
    use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};

    pub enum Value {
        Object(Vec<(String, Value)>),
        Other,
    }

    impl Value {
        pub fn keys(&self) -> Vec<String> {
            match self {
                Value::Object(entries) => entries.iter().map(|(k, _)| k.clone()).collect(),
                Value::Other => Vec::new(),
            }
        }

        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                Value::Other => None,
            }
        }
    }

    impl<'de> serde::Deserialize<'de> for Value {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Any;
            impl<'de> Visitor<'de> for Any {
                type Value = Value;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("any JSON value")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                    let mut entries = Vec::new();
                    while let Some(entry) = map.next_entry::<String, Value>()? {
                        entries.push(entry);
                    }
                    Ok(Value::Object(entries))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
                    while seq.next_element::<Value>()?.is_some() {}
                    Ok(Value::Other)
                }
                fn visit_str<E>(self, _: &str) -> Result<Value, E> {
                    Ok(Value::Other)
                }
                fn visit_bool<E>(self, _: bool) -> Result<Value, E> {
                    Ok(Value::Other)
                }
                fn visit_i64<E>(self, _: i64) -> Result<Value, E> {
                    Ok(Value::Other)
                }
                fn visit_u64<E>(self, _: u64) -> Result<Value, E> {
                    Ok(Value::Other)
                }
                fn visit_f64<E>(self, _: f64) -> Result<Value, E> {
                    Ok(Value::Other)
                }
                fn visit_unit<E>(self) -> Result<Value, E> {
                    Ok(Value::Other)
                }
            }
            d.deserialize_any(Any)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_is_the_exact_inverse_of_multi_value() {
        for items in [
            vec![],
            vec![""],
            vec!["a"],
            vec![" a "],
            vec!["a,b", "c\\d"],
            vec!["", "x", ""],
            vec!["\\,", "\\\\,"],
        ] {
            let joined = crate::jcr::multi_value(&items);
            if items.as_slice() == [""] {
                // `[]` is the empty list: a single empty item has no spelling
                // of its own, which is why an empty rule item is an error.
                assert_eq!(split_multi_value(&joined).unwrap(), Vec::<String>::new());
                continue;
            }
            assert_eq!(split_multi_value(&joined).unwrap(), items, "{joined}");
        }
    }

    #[test]
    fn a_shape_keeps_the_key_order() {
        let shape = RuleShape::of(
            r#"{"script":{"content":"x","event":"Click","field":"f"},"nodeName":"SCRIPTMODEL","version":1,"enabled":true}"#,
        )
        .unwrap();
        assert_eq!(shape.keys, ["script", "nodeName", "version", "enabled"]);
        assert_eq!(shape.script_keys, ["content", "event", "field"]);
        assert_eq!(shape.event.as_deref(), Some("Click"));
        assert!(shape.compact && shape.model.is_none() && shape.archetype.is_none());
    }

    #[test]
    fn a_backslash_that_escapes_nothing_is_refused() {
        assert_eq!(split_multi_value(r"[a,b\c]"), Err(ScriptListError::StrayEscape { index: 1 }));
        assert_eq!(split_multi_value(r"[a\]"), Err(ScriptListError::StrayEscape { index: 0 }));
    }

    #[test]
    fn compactness_is_read_outside_strings_only() {
        assert!(is_compact(r#"{"a":"x y","b":"\" ","c":1}"#));
        assert!(!is_compact(r#"{"a": 1}"#));
        assert!(is_compact(r#"{"a":"é\/"}"#));
    }

    #[test]
    fn an_escaped_separator_is_named() {
        let a = r#"{"a":1}"#;
        let value = format!("[{a}\\,{a}]");
        assert_eq!(
            decode_script_list(&value),
            Err(ScriptListError::EscapedSeparator { index: 0 })
        );
    }

    #[test]
    fn a_script_round_trips_through_its_typed_view_in_both_orders() {
        for order in [BodyOrder::FieldFirst, BodyOrder::ContentFirst] {
            let script = EventScript {
                field: "guide.guideRootPanel.p".into(),
                event: ScriptEvent::ValueCommit,
                content: "a(\"x, y\");\nb('\\\\');".into(),
                order,
                archetype: Some("gen".into()),
            };
            assert_eq!(EventScript::from_json(&script.to_json()), Some(script));
        }
    }
}
