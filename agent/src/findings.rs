//! What an inspector reports: the read-only sub-agent a stage's `inspect`
//! hands one brief to (`pipeline::inspect`). It examines what the brief names,
//! changes nothing, and ends with `submit_findings`, whose input is parsed here
//! once into a [`Findings`] that cannot hold a report saying nothing.

use serde::{Deserialize, Serialize};

/// An inspector's report, as `submit_findings` takes it (its `inspection` is
/// the key it is stored under, not part of the report).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Findings {
    /// What the inspector actually examined: pages, variants, languages.
    pub checked: Vec<String>,
    /// What the brief asked for and the inspector left out, each with why.
    #[serde(default)]
    pub not_checked: Vec<String>,
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// One thing an inspector found.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    /// The JSON Pointer of the document node it is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Where in the source it is: a page, an XFA path, a variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The output is wrong and has to change.
    Defect,
    /// Something the inspector could not decide, for the stage to look at.
    Question,
    /// An observation that needs no change.
    Note,
}

impl Findings {
    /// Parses `submit_findings`' input, its `inspection` aside, into a report
    /// that says what it examined.
    pub fn from_input(input: &serde_json::Value) -> Result<Self, String> {
        let mut report = input.clone();
        if let Some(object) = report.as_object_mut() {
            object.remove("inspection");
        }
        let findings: Self = serde_json::from_value(report).map_err(|e| e.to_string())?;
        if findings.checked.iter().all(|c| c.trim().is_empty()) {
            return Err("checked must say what you examined".into());
        }
        if findings.findings.iter().any(|f| f.message.trim().is_empty()) {
            return Err("every finding needs a message".into());
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_report_parses_without_its_inspection_id() {
        let findings = Findings::from_input(&json!({
            "inspection": "inspection-1",
            "checked": ["pages 1-3 in de and it"],
            "findings": [{"severity": "defect", "message": "the IBAN field is missing", "source": "page 2"}],
        }))
        .unwrap();
        assert_eq!(findings.checked, ["pages 1-3 in de and it"]);
        assert!(findings.not_checked.is_empty());
        assert_eq!(findings.findings[0].severity, Severity::Defect);
        assert_eq!(findings.findings[0].source.as_deref(), Some("page 2"));
    }

    #[test]
    fn a_report_that_examined_nothing_is_refused() {
        assert!(Findings::from_input(&json!({"checked": []})).is_err());
        assert!(Findings::from_input(&json!({"checked": [" "]})).is_err());
        assert!(Findings::from_input(&json!({})).is_err());
    }

    #[test]
    fn unknown_fields_and_severities_are_refused() {
        assert!(Findings::from_input(&json!({"checked": ["p1"], "verdict": "ok"})).is_err());
        assert!(
            Findings::from_input(&json!({"checked": ["p1"], "findings": [{"severity": "major", "message": "x"}]}))
                .is_err()
        );
        assert!(
            Findings::from_input(&json!({"checked": ["p1"], "findings": [{"severity": "note", "message": ""}]}))
                .is_err()
        );
    }
}
