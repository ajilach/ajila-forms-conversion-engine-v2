//! What a form's scripts did that the person filling it would have seen or
//! that went wrong: the dialogs they raised and the scripts that failed.
//!
//! A failing script aborts only its own handler (XFA 3.3 §10 leaves error
//! handling to the host; Acrobat logs to its console and carries on), so the
//! form keeps working, but whatever the rest of that handler would have done
//! is missing. A host that renders the form must say so rather than present
//! the result as complete.

use serde::Serialize;

/// How many distinct script failures a warning names before summarising the
/// rest as a count.
pub const WARNING_ERROR_LIMIT: usize = 5;

/// A dialog a script raised (`app.alert`, `xfa.host.messageBox`), and the
/// button it was answered with: nobody is there to press one, so a dialog
/// takes its default (see [`super::acrobat::dialog_answer`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostMessage {
    /// The call that raised it: `app.alert` or `xfa.host.messageBox`.
    pub source: String,
    pub text: String,
    /// The dialog's title; empty when the script gave none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Acrobat's return code for the button taken: 1 OK, 4 Yes.
    pub answer: i32,
}

/// One script run that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFailure {
    /// The event's activity (`initialize`, `click`, ...); empty for a failure
    /// of the host itself rather than of a script (a `resetData` naming
    /// nothing).
    pub activity: String,
    /// The SOM path of the script's owner; empty when the script ran with
    /// no owner (a script object's body).
    pub owner: String,
    /// The page being laid out, for a master-page script re-run per page.
    pub page: Option<usize>,
    pub error: String,
}

impl std::fmt::Display for ScriptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.activity.is_empty() {
            write!(f, "{} script on ", self.activity)?;
            if self.owner.is_empty() {
                write!(f, "the form")?;
            } else {
                write!(f, "{}", self.owner)?;
            }
            if let Some(page) = self.page {
                write!(f, " (page {})", page + 1)?;
            }
            write!(f, ": ")?;
        }
        write!(f, "{}", self.error)
    }
}

/// The scripts' dialogs and failures over some span of work: a load, or one
/// interaction and the recalculation after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptReport {
    /// Each failure, in order.
    pub errors: Vec<ScriptFailure>,
    /// Failures beyond what the engine keeps, counted only.
    pub dropped_errors: usize,
    pub messages: Vec<HostMessage>,
}

impl ScriptReport {
    /// Fold a later span's report into this one.
    pub fn absorb(&mut self, other: ScriptReport) {
        self.errors.extend(other.errors);
        self.dropped_errors += other.dropped_errors;
        self.messages.extend(other.messages);
    }

    /// The failures as one warning: how many scripts failed, the first
    /// [`WARNING_ERROR_LIMIT`] distinct ones verbatim, and how many more.
    /// `None` when nothing failed.
    pub fn warning(&self) -> Option<String> {
        let total = self.errors.len() + self.dropped_errors;
        if total == 0 {
            return None;
        }
        let mut distinct: Vec<String> = Vec::new();
        for error in &self.errors {
            let error = error.to_string();
            if !distinct.contains(&error) {
                distinct.push(error);
            }
        }
        let shown: Vec<&str> = distinct
            .iter()
            .take(WARNING_ERROR_LIMIT)
            .map(String::as_str)
            .collect();
        let hidden = total - shown.len();
        let noun = if total == 1 { "run" } else { "runs" };
        let mut warning = format!(
            "{total} script {noun} failed, so what the rest of each handler would have \
             done is missing: {}",
            shown.join("; ")
        );
        if hidden > 0 {
            warning.push_str(&format!("; and {hidden} more"));
        }
        Some(warning)
    }
}

/// `a` and `b` joined by "; ", whichever are present.
pub fn join_warnings(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(error: &str) -> ScriptFailure {
        ScriptFailure {
            activity: String::new(),
            owner: String::new(),
            page: None,
            error: error.to_string(),
        }
    }

    fn report(errors: &[&str], dropped: usize) -> ScriptReport {
        ScriptReport {
            errors: errors.iter().map(|e| failure(e)).collect(),
            dropped_errors: dropped,
            messages: Vec::new(),
        }
    }

    #[test]
    fn no_failure_is_no_warning() {
        assert_eq!(report(&[], 0).warning(), None);
    }

    #[test]
    fn a_failure_names_its_activity_owner_and_page() {
        let mut f = ScriptFailure {
            activity: "initialize".to_string(),
            owner: "F.Footer".to_string(),
            page: Some(1),
            error: "boom".to_string(),
        };
        assert_eq!(f.to_string(), "initialize script on F.Footer (page 2): boom");
        f.page = None;
        f.owner = String::new();
        assert_eq!(f.to_string(), "initialize script on the form: boom");
        assert_eq!(failure("host said no").to_string(), "host said no");
    }

    #[test]
    fn one_failure_is_named() {
        let one = ScriptReport {
            errors: vec![ScriptFailure {
                activity: "click".to_string(),
                owner: "A.B".to_string(),
                page: None,
                error: "JavaScript error: ReferenceError: x".to_string(),
            }],
            ..ScriptReport::default()
        };
        assert_eq!(
            one.warning().as_deref(),
            Some(
                "1 script run failed, so what the rest of each handler would have done is \
                 missing: click script on A.B: JavaScript error: ReferenceError: x"
            )
        );
    }

    #[test]
    fn repeats_are_named_once_and_the_rest_counted() {
        let errors: Vec<String> = (0..7).map(|i| format!("e{i}")).collect();
        let mut with_repeat: Vec<&str> = vec!["e0", "e0"];
        with_repeat.extend(errors.iter().skip(1).map(String::as_str));
        let warning = report(&with_repeat, 2).warning().unwrap();
        assert!(warning.starts_with("10 script runs failed"), "{warning}");
        assert!(warning.contains(": e0; e1; e2; e3; e4; and 5 more"), "{warning}");
    }

    #[test]
    fn absorbing_concatenates_in_order() {
        let mut a = report(&["a"], 1);
        a.absorb(report(&["b"], 2));
        assert_eq!(a, report(&["a", "b"], 3));
    }

    #[test]
    fn warnings_join_whichever_are_present() {
        let w = |s: &str| Some(s.to_string());
        assert_eq!(join_warnings(w("a"), w("b")), w("a; b"));
        assert_eq!(join_warnings(None, w("b")), w("b"));
        assert_eq!(join_warnings(w("a"), None), w("a"));
        assert_eq!(join_warnings(None, None), None);
    }
}
