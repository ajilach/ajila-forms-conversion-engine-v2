//! [`RuleCheck`]: how one rule revision is checked.
//!
//! Here rather than in `u2s-store` or `u2s-doc-tools` because both name it:
//! the store reads it off `rule_revisions`, and a rules directory loads it
//! from files, and neither crate may depend on the other.

use serde::Serialize;

/// How a rule revision is checked. A judged revision carries no script, so
/// it can carry no fix either: the type cannot hold a fix without a check,
/// matching `rule_revisions_fix_needs_script`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleCheck {
    /// Deterministic: `check(output, ctx)` runs in the sandbox, and
    /// `fix_js`, when given, is the rule's mechanical repair.
    Script {
        script_js: String,
        fix_js: Option<String>,
    },
    /// No script: the Judge agent reads the output and its source and
    /// decides against the rule's description.
    Judged,
}

/// [`RuleCheck`] without its scripts, for a view or a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleCheckKind {
    Script,
    Judged,
}

impl RuleCheckKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Judged => "judged",
        }
    }
}

/// A fix script stored for a revision without a check script.
#[derive(Debug, Clone, thiserror::Error)]
#[error("a rule revision with a fix script must have a check script")]
pub struct FixWithoutScript;

impl RuleCheck {
    /// A scripted check without a fix.
    pub fn script(script_js: impl Into<String>) -> Self {
        Self::Script {
            script_js: script_js.into(),
            fix_js: None,
        }
    }

    /// From the two nullable `rule_revisions` columns.
    pub fn from_columns(
        script_js: Option<String>,
        fix_js: Option<String>,
    ) -> Result<Self, FixWithoutScript> {
        match (script_js, fix_js) {
            (Some(script_js), fix_js) => Ok(Self::Script { script_js, fix_js }),
            (None, None) => Ok(Self::Judged),
            (None, Some(_)) => Err(FixWithoutScript),
        }
    }

    pub fn kind(&self) -> RuleCheckKind {
        match self {
            Self::Script { .. } => RuleCheckKind::Script,
            Self::Judged => RuleCheckKind::Judged,
        }
    }

    pub fn script_js(&self) -> Option<&str> {
        match self {
            Self::Script { script_js, .. } => Some(script_js),
            Self::Judged => None,
        }
    }

    pub fn fix_js(&self) -> Option<&str> {
        match self {
            Self::Script { fix_js, .. } => fix_js.as_deref(),
            Self::Judged => None,
        }
    }

    pub fn is_judged(&self) -> bool {
        matches!(self, Self::Judged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_columns_round_trip() {
        for check in [
            RuleCheck::Judged,
            RuleCheck::script("function check() {}"),
            RuleCheck::Script {
                script_js: "function check() {}".to_owned(),
                fix_js: Some("function fix() {}".to_owned()),
            },
        ] {
            let back = RuleCheck::from_columns(
                check.script_js().map(str::to_owned),
                check.fix_js().map(str::to_owned),
            )
            .expect("valid columns");
            assert_eq!(back, check);
        }
    }

    #[test]
    fn a_fix_without_a_script_is_refused() {
        assert!(RuleCheck::from_columns(None, Some("function fix() {}".to_owned())).is_err());
    }
}
