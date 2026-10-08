//! XFA Event Types and Script Definitions
//!
//! This module defines the event and script types per XFA 3.3 specification.
//!
//! ## XFA 3.3 Spec References:
//! - Chapter 10: Events (page 378-408)
//! - Chapter 11: Scripting (page 410-416)

use std::str::FromStr;

/// Script content type as per XFA spec
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptContentType {
    JavaScript,
    FormCalc,
}

impl ScriptContentType {
    pub fn from_content_type(s: &str) -> Option<Self> {
        match s {
            "application/x-javascript" => Some(ScriptContentType::JavaScript),
            "application/x-formcalc" => Some(ScriptContentType::FormCalc),
            _ => None,
        }
    }
}

/// XFA Event activity types
/// Per XFA spec section 10, "Events"
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventActivity {
    Ready,
    Initialize,
    Enter,
    Exit,
    Change,
    Click,
    Calculate,
    Validate,
    PreSubmit,
    PostSubmit,
    DocReady,
    IndexChange,
    Other(String),
}

impl FromStr for EventActivity {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "ready" => EventActivity::Ready,
            "initialize" => EventActivity::Initialize,
            "enter" => EventActivity::Enter,
            "exit" => EventActivity::Exit,
            "change" => EventActivity::Change,
            "click" => EventActivity::Click,
            "calculate" => EventActivity::Calculate,
            "validate" => EventActivity::Validate,
            "preSubmit" => EventActivity::PreSubmit,
            "postSubmit" => EventActivity::PostSubmit,
            "docReady" => EventActivity::DocReady,
            "indexChange" => EventActivity::IndexChange,
            _ => EventActivity::Other(s.to_string()),
        })
    }
}

impl EventActivity {
    /// Return the canonical string name of this activity, matching the XFA
    /// attribute values used in `<event activity="...">`.
    pub fn activity_name(&self) -> &str {
        match self {
            EventActivity::Ready => "ready",
            EventActivity::Initialize => "initialize",
            EventActivity::Enter => "enter",
            EventActivity::Exit => "exit",
            EventActivity::Change => "change",
            EventActivity::Click => "click",
            EventActivity::Calculate => "calculate",
            EventActivity::Validate => "validate",
            EventActivity::PreSubmit => "preSubmit",
            EventActivity::PostSubmit => "postSubmit",
            EventActivity::DocReady => "docReady",
            EventActivity::IndexChange => "indexChange",
            EventActivity::Other(s) => s.as_str(),
        }
    }
}

/// XFA Event reference target
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventRef {
    Form,
    Layout,
    Data,
    Current,
    Named(String),
}

impl FromStr for EventRef {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "$form" | "xfa.form" => EventRef::Form,
            "$layout" | "xfa.layout" => EventRef::Layout,
            "$data" | "xfa.data" => EventRef::Data,
            "$" => EventRef::Current,
            _ => EventRef::Named(s.to_string()),
        })
    }
}

/// Controls whether events propagate from descendant containers.
/// Per XFA 3.3 §10 p.387 and §17 p.707.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenScope {
    /// Event only fires on the target container (default, backward compat)
    #[default]
    RefOnly,
    /// Event also fires when a descendant container triggers the same activity
    RefAndDescendents,
}

impl FromStr for ListenScope {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "refAndDescendents" => ListenScope::RefAndDescendents,
            _ => ListenScope::RefOnly,
        })
    }
}

/// Where the script should be executed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunAt {
    #[default]
    Client,
    Server,
    Both,
}

impl FromStr for RunAt {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "server" => RunAt::Server,
            "both" => RunAt::Both,
            _ => RunAt::Client,
        })
    }
}

/// Represents a script attached to an event
#[derive(Debug, Clone)]
pub struct XfaScript {
    pub source: String,
    pub content_type: ScriptContentType,
    pub activity: EventActivity,
    pub event_ref: EventRef,
    pub name: Option<String>,
    pub run_at: RunAt,
    /// Per XFA 3.3 §10 p.387: controls upward event propagation.
    pub listen: ListenScope,
}

// =============================================================================
// Parsing functions
// =============================================================================

use crate::xfa::{XfaNode, XfaNodeKind};

/// Parse all event scripts from node children
pub fn parse_events_from_node(children: &[XfaNode]) -> Vec<XfaScript> {
    let mut scripts = Vec::new();

    for child in children {
        if let XfaNodeKind::Element { tag_name, .. } = &child.kind {
            match tag_name.as_str() {
                "event" => {
                    if let Some(script) = parse_event_element(child) {
                        scripts.push(script);
                    }
                }
                // XFA 3.3 §D.4: <calculate> and <validate> can appear as direct
                // children of field/subform/exclGroup, equivalent to
                // <event activity="calculate/validate"><script>...</script></event>.
                "calculate" | "validate" => {
                    if let Some(script) = parse_shorthand_element(child, tag_name) {
                        scripts.push(script);
                    }
                }
                _ => {}
            }
        }
    }

    scripts
}

fn parse_event_element(event_node: &XfaNode) -> Option<XfaScript> {
    let activity = event_node
        .attributes
        .get("activity")
        .and_then(|s| s.parse().ok())
        .unwrap_or(EventActivity::Other("unknown".to_string()));

    let event_ref = event_node
        .attributes
        .get("ref")
        .and_then(|s| s.parse().ok())
        .unwrap_or(EventRef::Current);

    let name = event_node.attributes.get("name").cloned();

    let listen = event_node
        .attributes
        .get("listen")
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();

    for child in &event_node.children {
        if let XfaNodeKind::Element {
            tag_name,
            text_content,
        } = &child.kind
            && tag_name == "script"
        {
            let content_type = child
                .attributes
                .get("contentType")
                .and_then(|s| ScriptContentType::from_content_type(s))
                .unwrap_or(ScriptContentType::FormCalc);

            let run_at = child
                .attributes
                .get("runAt")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();

            let source = text_content.clone().unwrap_or_default();

            if !source.trim().is_empty() {
                return Some(XfaScript {
                    source,
                    content_type,
                    activity,
                    event_ref,
                    name,
                    run_at,
                    listen,
                });
            }
        }
    }

    None
}

/// Parse a shorthand `<calculate>` or `<validate>` element into an [`XfaScript`].
///
/// XFA allows `<calculate><script>...</script></calculate>` directly on
/// field/subform/exclGroup nodes as shorthand for
/// `<event activity="calculate"><script>...</script></event>`.
fn parse_shorthand_element(node: &XfaNode, tag_name: &str) -> Option<XfaScript> {
    let activity = match tag_name {
        "calculate" => EventActivity::Calculate,
        "validate" => EventActivity::Validate,
        _ => return None,
    };

    for child in &node.children {
        if let XfaNodeKind::Element {
            tag_name: child_tag,
            text_content,
        } = &child.kind
            && child_tag == "script"
        {
            let content_type = child
                .attributes
                .get("contentType")
                .and_then(|s| ScriptContentType::from_content_type(s))
                .unwrap_or(ScriptContentType::FormCalc);

            let run_at = child
                .attributes
                .get("runAt")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();

            let source = text_content.clone().unwrap_or_default();

            if !source.trim().is_empty() {
                return Some(XfaScript {
                    source,
                    content_type,
                    activity,
                    event_ref: EventRef::Current,
                    name: None,
                    run_at,
                    listen: ListenScope::default(),
                });
            }
        }
    }

    None
}

/// Parse variables from node children (for script objects)
pub fn parse_variables_from_node(
    children: &[XfaNode],
) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    let mut variables = std::collections::HashMap::new();

    for child in children {
        if let XfaNodeKind::Element { tag_name, .. } = &child.kind
            && tag_name == "variables"
        {
            for var_child in &child.children {
                if let XfaNodeKind::Element {
                    tag_name: var_tag, ..
                } = &var_child.kind
                    && var_tag == "script"
                    && let Some(name) = var_child.attributes.get("name")
                {
                    variables.insert(name.clone(), std::collections::HashMap::new());
                }
            }
        }
    }

    variables
}
