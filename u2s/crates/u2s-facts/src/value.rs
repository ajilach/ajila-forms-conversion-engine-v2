//! [`FactValue`]: one stored `fact_values` row, and [`extractions_agree`],
//! the stability gate's comparison.

use serde_json::Value;

/// What one extraction attempt produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Extraction {
    /// A value that already conforms to the fact revision's answer schema.
    Extracted(Value),
    /// Why no value could be produced (a stable error code or a message).
    Failed(String),
}

/// A `fact_values` row whose columns do not form a valid combination.
///
/// The table's CHECK constraints make every one of these unreachable from a
/// real row, so hitting one means schema and code have drifted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactValueError {
    #[error("fact value carries both an extracted value and a failure reason")]
    ExtractedAndFailed,
    #[error("fact value carries neither an extraction nor a confirmed value")]
    Empty,
}

/// The value of one fact revision for one input, as stored.
///
/// A missing row (not extracted yet) is not a `FactValue`: callers hold an
/// `Option<FactValue>`, so "never attempted" and "attempted and failed" stay
/// distinct.
#[derive(Debug, Clone, PartialEq)]
pub enum FactValue {
    /// Extracted or failed, with no human confirmation.
    Unconfirmed(Extraction),
    /// A value a person set. It wins over whatever extraction produced, and
    /// re-extraction never overwrites it. `extraction` is kept alongside so
    /// a disagreement between the two stays visible.
    Confirmed {
        value: Value,
        extraction: Option<Extraction>,
    },
}

impl FactValue {
    /// The one conversion from the three nullable columns of a
    /// `fact_values` row.
    pub fn from_columns(
        extracted_value: Option<Value>,
        failure_reason: Option<String>,
        confirmed_value: Option<Value>,
    ) -> Result<Self, FactValueError> {
        let extraction = match (extracted_value, failure_reason) {
            (Some(_), Some(_)) => return Err(FactValueError::ExtractedAndFailed),
            (Some(value), None) => Some(Extraction::Extracted(value)),
            (None, Some(reason)) => Some(Extraction::Failed(reason)),
            (None, None) => None,
        };
        match (confirmed_value, extraction) {
            (Some(value), extraction) => Ok(Self::Confirmed { value, extraction }),
            (None, Some(extraction)) => Ok(Self::Unconfirmed(extraction)),
            (None, None) => Err(FactValueError::Empty),
        }
    }

    /// The value a check sees: the confirmed one if a person set it,
    /// otherwise the extracted one. `None` when the only thing on record is
    /// a failed extraction.
    pub fn effective(&self) -> Option<&Value> {
        match self {
            Self::Confirmed { value, .. } => Some(value),
            Self::Unconfirmed(Extraction::Extracted(value)) => Some(value),
            Self::Unconfirmed(Extraction::Failed(_)) => None,
        }
    }

    /// Why there is no effective value, when there is none.
    pub fn failure_reason(&self) -> Option<&str> {
        match self {
            Self::Unconfirmed(Extraction::Failed(reason)) => Some(reason),
            _ => None,
        }
    }

    /// A person confirmed a value that differs from what extraction said.
    /// A failed extraction under a confirmed value is not a dispute: there
    /// is no competing value.
    pub fn is_disputed(&self) -> bool {
        matches!(
            self,
            Self::Confirmed { value, extraction: Some(Extraction::Extracted(extracted)) }
                if value != extracted
        )
    }
}

/// Whether two extractions of the same fact revision on the same input
/// agree: the stability gate refuses to save an LLM fact when any sample
/// input disagrees.
///
/// Two values agree when they are equal as JSON. Object key order is
/// irrelevant (`serde_json` compares objects as maps); array order is not,
/// which is why the extraction prompt asks for lists in document order. Two
/// failures agree: a fact that fails the same way twice is failing, not
/// unstable, and that shows up as `indeterminate` verdicts instead. A value
/// on one attempt and a failure on the other disagree.
pub fn extractions_agree(first: &Extraction, second: &Extraction) -> bool {
    match (first, second) {
        (Extraction::Extracted(a), Extraction::Extracted(b)) => a == b,
        (Extraction::Failed(_), Extraction::Failed(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn columns_convert_to_the_matching_variant() {
        assert_eq!(
            FactValue::from_columns(Some(json!(3)), None, None),
            Ok(FactValue::Unconfirmed(Extraction::Extracted(json!(3))))
        );
        assert_eq!(
            FactValue::from_columns(None, Some("timeout".into()), None),
            Ok(FactValue::Unconfirmed(Extraction::Failed("timeout".into())))
        );
        assert_eq!(
            FactValue::from_columns(None, None, Some(json!(4))),
            Ok(FactValue::Confirmed { value: json!(4), extraction: None })
        );
    }

    #[test]
    fn inconsistent_columns_are_refused() {
        assert_eq!(
            FactValue::from_columns(Some(json!(1)), Some("x".into()), None),
            Err(FactValueError::ExtractedAndFailed)
        );
        assert_eq!(FactValue::from_columns(None, None, None), Err(FactValueError::Empty));
    }

    #[test]
    fn a_confirmed_value_wins_over_extraction_and_over_failure() {
        let over_value = FactValue::from_columns(Some(json!(1)), None, Some(json!(2))).unwrap();
        assert_eq!(over_value.effective(), Some(&json!(2)));
        assert!(over_value.is_disputed());

        let over_failure =
            FactValue::from_columns(None, Some("timeout".into()), Some(json!(2))).unwrap();
        assert_eq!(over_failure.effective(), Some(&json!(2)));
        assert_eq!(over_failure.failure_reason(), None);
        assert!(!over_failure.is_disputed());
    }

    #[test]
    fn a_confirmed_value_equal_to_extraction_is_not_disputed() {
        let v = FactValue::from_columns(Some(json!({"a": 1})), None, Some(json!({"a": 1}))).unwrap();
        assert!(!v.is_disputed());
    }

    #[test]
    fn a_failed_extraction_has_no_effective_value() {
        let v = FactValue::from_columns(None, Some("timeout".into()), None).unwrap();
        assert_eq!(v.effective(), None);
        assert_eq!(v.failure_reason(), Some("timeout"));
    }

    #[test]
    fn agreement_ignores_key_order_but_not_array_order() {
        let a = Extraction::Extracted(json!({"a": 1, "b": [1, 2]}));
        let same = Extraction::Extracted(json!({"b": [1, 2], "a": 1}));
        let reordered = Extraction::Extracted(json!({"a": 1, "b": [2, 1]}));
        assert!(extractions_agree(&a, &same));
        assert!(!extractions_agree(&a, &reordered));
    }

    #[test]
    fn two_failures_agree_and_a_value_against_a_failure_does_not() {
        let failed = Extraction::Failed("x".into());
        let failed_other = Extraction::Failed("y".into());
        let value = Extraction::Extracted(json!(1));
        assert!(extractions_agree(&failed, &failed_other));
        assert!(!extractions_agree(&failed, &value));
        assert!(!extractions_agree(&value, &failed));
    }
}
