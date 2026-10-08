//! What a fact revision is made of besides its question and answer schema:
//! where its values come from ([`FactSource`]) and how retrieval indexes it
//! ([`IndexProjection`]).

use serde::{Deserialize, Serialize};

/// Where a fact revision's values come from. One source per revision, never
/// a fallback chain: values from two different sources look alike but are
/// not comparable, which would quietly corrupt both verdicts and the
/// stability gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactSource {
    /// A one-shot structured LLM answer over the input's text and page
    /// renders.
    Llm,
    /// `function extract(ingest)`, run in the rule sandbox over the stored
    /// ingest data. Deterministic, so no stability gate applies.
    IngestScript { script_js: String },
}

/// The source column and the script column of a `fact_revisions` row do not
/// form a valid combination. The table's CHECK constraints make this
/// unreachable from a real row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("fact source {kind:?} with script present = {has_script} is not a valid combination")]
pub struct FactSourceError {
    pub kind: String,
    pub has_script: bool,
}

impl FactSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::IngestScript { .. } => "ingest_script",
        }
    }

    pub fn script_js(&self) -> Option<&str> {
        match self {
            Self::Llm => None,
            Self::IngestScript { script_js } => Some(script_js),
        }
    }

    /// The one conversion from the `source` and `script_js` columns.
    pub fn from_columns(source: &str, script_js: Option<String>) -> Result<Self, FactSourceError> {
        match (source, script_js) {
            ("llm", None) => Ok(Self::Llm),
            ("ingest_script", Some(script_js)) => Ok(Self::IngestScript { script_js }),
            (kind, script) => Err(FactSourceError {
                kind: kind.to_owned(),
                has_script: script.is_some(),
            }),
        }
    }
}

/// How retrieval indexes a fact. Only `None` exists until facts become a
/// retrieval channel; the column exists already so that phase needs no
/// migration of saved revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexProjection {
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_columns_round_trip() {
        assert_eq!(FactSource::from_columns("llm", None), Ok(FactSource::Llm));
        let script = FactSource::from_columns("ingest_script", Some("x".into())).unwrap();
        assert_eq!(script.as_str(), "ingest_script");
        assert_eq!(script.script_js(), Some("x"));
    }

    #[test]
    fn mismatched_source_columns_are_refused() {
        assert!(FactSource::from_columns("llm", Some("x".into())).is_err());
        assert!(FactSource::from_columns("ingest_script", None).is_err());
        assert!(FactSource::from_columns("other", None).is_err());
    }

    #[test]
    fn index_projection_matches_the_column_default() {
        assert_eq!(serde_json::to_value(IndexProjection::None).unwrap(), json!({"kind": "none"}));
        assert_eq!(
            serde_json::from_value::<IndexProjection>(json!({"kind": "none"})).unwrap(),
            IndexProjection::None
        );
    }
}
