//! [`resolve_facts`]: the single place a rule revision's verdict on an input
//! becomes `indeterminate`.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::key::FactKey;
use crate::value::FactValue;

/// What a check against one input gets as `ctx.facts`, or why it cannot be
/// checked at all.
#[derive(Debug, Clone, PartialEq)]
pub enum FactsForCheck {
    /// Every pinned fact has an effective value. The map holds exactly the
    /// pinned keys, which is what the sandbox's `ctx.facts` proxy allows; an
    /// intrinsic rule gets an empty map.
    Ready(Map<String, Value>),
    /// At least one pinned fact has no effective value for this input. The
    /// script is not run: a verdict computed without the facts it declared
    /// would be a guess. Carries a reason naming every such fact.
    Indeterminate(String),
}

impl FactsForCheck {
    /// The facts a sandbox receives, or `None` when the check must not run.
    pub fn ready(&self) -> Option<&Map<String, Value>> {
        match self {
            Self::Ready(map) => Some(map),
            Self::Indeterminate(_) => None,
        }
    }
}

/// Resolves a rule revision's pinned facts against one input.
///
/// `pinned` holds one entry per fact the revision pins, keyed by the name
/// the script reads it under, with the stored value for this input or
/// `None` when nothing was extracted yet. The pins are written from the
/// script's own `requires` declaration, so this map *is* the declaration;
/// an empty map is an intrinsic rule and always resolves to an empty
/// `Ready`.
pub fn resolve_facts(pinned: &BTreeMap<FactKey, Option<FactValue>>) -> FactsForCheck {
    let mut ready = Map::new();
    let mut missing = Vec::new();
    for (key, value) in pinned {
        match value {
            None => missing.push(format!("fact `{key}` not extracted for this input")),
            Some(value) => match value.effective() {
                Some(effective) => {
                    ready.insert(key.as_str().to_owned(), effective.clone());
                }
                None => missing.push(format!(
                    "fact `{key}` failed: {}",
                    value.failure_reason().unwrap_or("no value")
                )),
            },
        }
    }
    if missing.is_empty() {
        FactsForCheck::Ready(ready)
    } else {
        FactsForCheck::Indeterminate(missing.join("; "))
    }
}

/// Whether a rule reads the input at all, derived from its declared facts.
/// Never stored: the pins are the source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleGrounding {
    /// Checks the output alone.
    Intrinsic,
    /// Checks the output against these facts about the input. Never empty:
    /// only [`RuleGrounding::from_keys`] builds one.
    Extrinsic(Vec<FactKey>),
}

impl RuleGrounding {
    pub fn from_keys(mut keys: Vec<FactKey>) -> Self {
        if keys.is_empty() {
            return Self::Intrinsic;
        }
        keys.sort();
        keys.dedup();
        Self::Extrinsic(keys)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Intrinsic => "intrinsic",
            Self::Extrinsic(_) => "extrinsic",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Extraction;
    use serde_json::json;

    fn key(s: &str) -> FactKey {
        FactKey::parse(s).unwrap()
    }

    #[test]
    fn an_intrinsic_rule_resolves_to_no_facts() {
        assert_eq!(resolve_facts(&BTreeMap::new()), FactsForCheck::Ready(Map::new()));
    }

    #[test]
    fn every_present_fact_is_handed_over_under_its_key() {
        let pinned = BTreeMap::from([
            (key("a"), Some(FactValue::Unconfirmed(Extraction::Extracted(json!([1]))))),
            (
                key("b"),
                Some(FactValue::Confirmed { value: json!("x"), extraction: None }),
            ),
        ]);
        let FactsForCheck::Ready(map) = resolve_facts(&pinned) else {
            panic!("expected ready");
        };
        assert_eq!(map.get("a"), Some(&json!([1])));
        assert_eq!(map.get("b"), Some(&json!("x")));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn a_missing_or_failed_fact_makes_the_check_indeterminate_and_names_each() {
        let pinned = BTreeMap::from([
            (key("a"), None),
            (key("b"), Some(FactValue::Unconfirmed(Extraction::Failed("timeout".into())))),
            (key("c"), Some(FactValue::Unconfirmed(Extraction::Extracted(json!(1))))),
        ]);
        assert_eq!(
            resolve_facts(&pinned),
            FactsForCheck::Indeterminate(
                "fact `a` not extracted for this input; fact `b` failed: timeout".into()
            )
        );
    }

    #[test]
    fn a_confirmed_value_rescues_a_failed_extraction() {
        let pinned = BTreeMap::from([(
            key("a"),
            Some(FactValue::Confirmed {
                value: json!(2),
                extraction: Some(Extraction::Failed("timeout".into())),
            }),
        )]);
        assert!(resolve_facts(&pinned).ready().is_some());
    }

    #[test]
    fn grounding_is_derived_from_the_declared_keys() {
        assert_eq!(RuleGrounding::from_keys(vec![]), RuleGrounding::Intrinsic);
        assert_eq!(
            RuleGrounding::from_keys(vec![key("b"), key("a"), key("b")]),
            RuleGrounding::Extrinsic(vec![key("a"), key("b")])
        );
    }
}
