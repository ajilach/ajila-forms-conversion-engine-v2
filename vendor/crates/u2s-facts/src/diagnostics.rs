//! Advisory signals over a fact's stored values, the fact counterpart of the
//! rule diagnostics that come out of `rule_references`. None of them blocks
//! a save; the one gating signal, instability, is [`crate::extractions_agree`]
//! applied by the save job.

use std::collections::BTreeMap;

use u2s_core::InputId;

use crate::value::FactValue;

/// The same effective value on every input that has one, over at least two
/// inputs. Harmless for a rule; a sign the question does not tell forms
/// apart.
pub fn is_constant(values: &BTreeMap<InputId, FactValue>) -> bool {
    let mut effective = values.values().filter_map(FactValue::effective);
    let Some(first) = effective.next() else {
        return false;
    };
    let mut count = 1;
    for value in effective {
        if value != first {
            return false;
        }
        count += 1;
    }
    count >= 2
}

/// Inputs where a person confirmed a value that differs from the extracted
/// one: the question or schema likely needs revising.
pub fn disputed_inputs(values: &BTreeMap<InputId, FactValue>) -> Vec<InputId> {
    values
        .iter()
        .filter(|(_, value)| value.is_disputed())
        .map(|(input, _)| *input)
        .collect()
}

/// How many inputs two facts share an effective value on, when they agree on
/// every one of them; `None` when they disagree on any shared input or share
/// none. Two facts that always agree carry the same information, so one of
/// them is redundant, with the shared count as the weight of that claim.
pub fn duplicate_overlap(
    a: &BTreeMap<InputId, FactValue>,
    b: &BTreeMap<InputId, FactValue>,
) -> Option<usize> {
    let mut shared = 0;
    for (input, value_a) in a {
        let (Some(va), Some(vb)) = (
            value_a.effective(),
            b.get(input).and_then(FactValue::effective),
        ) else {
            continue;
        };
        if va != vb {
            return None;
        }
        shared += 1;
    }
    (shared > 0).then_some(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Extraction;
    use serde_json::{Value, json};

    fn extracted(v: Value) -> FactValue {
        FactValue::Unconfirmed(Extraction::Extracted(v))
    }

    #[test]
    fn constant_needs_two_equal_values() {
        let i = InputId::generate();
        let j = InputId::generate();
        assert!(!is_constant(&BTreeMap::from([(i, extracted(json!(1)))])));
        assert!(is_constant(&BTreeMap::from([(i, extracted(json!(1))), (j, extracted(json!(1)))])));
        assert!(!is_constant(&BTreeMap::from([(i, extracted(json!(1))), (j, extracted(json!(2)))])));
    }

    #[test]
    fn constant_ignores_failed_inputs() {
        let failed = FactValue::Unconfirmed(Extraction::Failed("x".into()));
        let values = BTreeMap::from([
            (InputId::generate(), extracted(json!(1))),
            (InputId::generate(), failed),
            (InputId::generate(), extracted(json!(1))),
        ]);
        assert!(is_constant(&values));
    }

    #[test]
    fn disputed_lists_only_confirmed_values_that_differ() {
        let disputed = InputId::generate();
        let values = BTreeMap::from([
            (
                disputed,
                FactValue::Confirmed {
                    value: json!(2),
                    extraction: Some(Extraction::Extracted(json!(1))),
                },
            ),
            (InputId::generate(), extracted(json!(1))),
        ]);
        assert_eq!(disputed_inputs(&values), vec![disputed]);
    }

    #[test]
    fn duplicates_agree_on_every_shared_input() {
        let i = InputId::generate();
        let j = InputId::generate();
        let a = BTreeMap::from([(i, extracted(json!(1))), (j, extracted(json!(2)))]);
        let b = BTreeMap::from([(i, extracted(json!(1))), (j, extracted(json!(2)))]);
        let c = BTreeMap::from([(i, extracted(json!(1))), (j, extracted(json!(3)))]);
        assert_eq!(duplicate_overlap(&a, &b), Some(2));
        assert_eq!(duplicate_overlap(&a, &c), None);
        assert_eq!(duplicate_overlap(&a, &BTreeMap::new()), None);
    }
}
