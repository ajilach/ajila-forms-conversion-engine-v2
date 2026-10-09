//! Script Registry - Categorized storage for XFA scripts
//!
//! Per XFA 3.3 spec Chapter 10:
//! - Initialize: Run once when form loads, in depth-first order
//! - Calculate: Re-run when dependent values change
//! - Event: Run on specific user/system events (click, change, enter, exit)
//! - Validate: Run to validate field values

use super::events::{EventActivity, XfaScript};
use super::som::SomPath;
use std::collections::HashMap;

/// Categorizes scripts by their XFA lifecycle type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptType {
    /// Initialize scripts run once at form load
    Initialize,
    /// Calculate scripts re-run when dependencies change
    Calculate,
    /// Event scripts run on specific activities (click, change, etc.)
    Event,
    /// Validate scripts check field values
    Validate,
}

impl ScriptType {
    /// Determine the script type from the event activity
    pub fn from_activity(activity: &EventActivity) -> Self {
        match activity {
            EventActivity::Initialize => ScriptType::Initialize,
            EventActivity::Calculate => ScriptType::Calculate,
            EventActivity::Validate => ScriptType::Validate,
            // All other activities are event-driven
            _ => ScriptType::Event,
        }
    }
}

/// A registered script with its metadata
#[derive(Debug, Clone)]
pub struct RegisteredScript {
    /// The script source and configuration
    pub script: XfaScript,
    /// The template path of the field/subform this script is attached to:
    /// index-free, since every instance of a repeated section carries the
    /// same declaration (XFA 3.3 §9). Expand it to concrete instances with
    /// `SomResolver::expand_template`.
    pub owner_path: SomPath,
    /// The field/subform name (last part of path)
    pub owner_name: String,
    /// Child fields accessible via `this.childName`
    pub child_fields: Vec<(String, String)>, // (name, id)
    /// Script type for categorization
    pub script_type: ScriptType,
}

/// Registry holding all scripts in the form, categorized by type.
///
/// Keyed by index-free template path on both insert and lookup, so a query
/// with any instance's path (`A.Row[2].F`) finds the declaration all
/// instances share, and N instances of a repeated section register each
/// script once rather than N times.
/// This enables selective execution: only initialize scripts at load,
/// only change events on user interaction, etc.
#[derive(Debug, Default, Clone)]
pub struct ScriptRegistry {
    /// All scripts by owner path
    scripts_by_owner: HashMap<SomPath, Vec<RegisteredScript>>,
    /// Scripts by type for quick lookup
    scripts_by_type: HashMap<ScriptType, Vec<SomPath>>, // ScriptType -> owner paths
    /// Scripts by event activity for event-driven lookup
    scripts_by_activity: HashMap<EventActivity, Vec<SomPath>>, // activity -> owner paths
}

impl ScriptRegistry {
    pub fn new() -> Self {
        ScriptRegistry {
            scripts_by_owner: HashMap::new(),
            scripts_by_type: HashMap::new(),
            scripts_by_activity: HashMap::new(),
        }
    }

    /// Register a script
    pub fn register(&mut self, mut script: RegisteredScript) {
        let owner_path = script.owner_path.index_free();
        script.owner_path = owner_path.clone();
        let script_type = script.script_type;
        let activity = script.script.activity.clone();

        // Add to by-owner index, once per declaration: every instance of a
        // repeated section is walked, but declares the same scripts.
        let owned = self.scripts_by_owner.entry(owner_path.clone()).or_default();
        if owned.iter().any(|s| {
            s.script.activity == script.script.activity
                && s.script.name == script.script.name
                && s.script.source == script.script.source
        }) {
            return;
        }
        owned.push(script);

        Self::push_unique(
            self.scripts_by_type.entry(script_type).or_default(),
            &owner_path,
        );
        Self::push_unique(
            self.scripts_by_activity.entry(activity).or_default(),
            &owner_path,
        );
    }

    fn push_unique(owners: &mut Vec<SomPath>, owner: &SomPath) {
        if !owners.contains(owner) {
            owners.push(owner.clone());
        }
    }

    /// Get all scripts for a specific owner
    pub fn get_scripts_for_owner(&self, owner_path: &SomPath) -> Vec<&RegisteredScript> {
        self.scripts_by_owner
            .get(&owner_path.index_free())
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    /// Get all scripts of a specific type
    pub fn get_scripts_of_type(&self, script_type: ScriptType) -> Vec<&RegisteredScript> {
        self.scripts_by_type
            .get(&script_type)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| self.scripts_by_owner.get(path))
                    .flatten()
                    .filter(|s| s.script_type == script_type)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all scripts for a specific event activity on a specific owner
    pub fn get_event_scripts(
        &self,
        owner_path: &SomPath,
        activity: &EventActivity,
    ) -> Vec<&RegisteredScript> {
        self.scripts_by_owner
            .get(&owner_path.index_free())
            .map(|scripts| {
                scripts
                    .iter()
                    .filter(|s| &s.script.activity == activity)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the template path of every owner with scripts for an activity.
    pub fn get_owners_with_activity(&self, activity: &EventActivity) -> Vec<&SomPath> {
        self.scripts_by_activity
            .get(activity)
            .map(|paths| paths.iter().collect::<Vec<&SomPath>>())
            .unwrap_or_default()
    }

    /// Check if any scripts exist for a given activity
    pub fn has_scripts_for_activity(&self, activity: &EventActivity) -> bool {
        self.scripts_by_activity
            .get(activity)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Check if the owner has any interactive scripts (change, click, or calculate).
    /// These are the scripts that can affect form layout when a field value changes.
    /// Scripts whose source is entirely comments are ignored.
    pub fn has_interactive_scripts(&self, owner_path: &SomPath) -> bool {
        self.scripts_by_owner
            .get(&owner_path.index_free())
            .is_some_and(|scripts| {
                scripts.iter().any(|s| {
                    matches!(
                        s.script.activity,
                        EventActivity::Change | EventActivity::Click | EventActivity::Calculate
                    ) && !is_comment_only(&s.script.source)
                })
            })
    }
}

/// Returns true if the script source contains no executable code — only
/// whitespace, line comments (`//`), and block comments (`/* ... */`).
pub fn is_comment_only(source: &str) -> bool {
    let mut chars = source.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' | '\r' | '\n' => {
                chars.next();
            }
            '/' => {
                chars.next();
                match chars.peek() {
                    Some('/') => {
                        // Line comment — skip to end of line
                        for c in chars.by_ref() {
                            if c == '\n' {
                                break;
                            }
                        }
                    }
                    Some('*') => {
                        // Block comment — skip to */
                        chars.next(); // consume '*'
                        let mut prev = ' ';
                        for c in chars.by_ref() {
                            if prev == '*' && c == '/' {
                                break;
                            }
                            prev = c;
                        }
                    }
                    _ => return false, // '/' not followed by '/' or '*' → executable
                }
            }
            _ => return false,
        }
    }
    true
}
