//! The script contract: `function check(output, ctx) -> { pass, violations
//! }`, and what it takes to trust a script's answer without trusting the
//! script.
//!
//! `RuleViolation` matches the shape `u2s_aem::model::validate::Violation`
//! already documents as "the same shape rule scripts emit" — so Output
//! Review can render a schema violation and a rule violation identically.
//!
//! A rule may also carry `function fix(output, ctx) -> [ops]`, an RFC 6902
//! patch over a failing check's own violations. The fix half of this
//! contract lives at the bottom of this file, mirroring the check half's
//! discipline: a fix is never trusted on its say-so either — see
//! [`BrokenFix`] and [`crate::fix::verify_fix`], which is where that
//! discipline is actually enforced (it needs to re-run `check`, which this
//! module does not depend on).

use serde::{Deserialize, Serialize};

/// One violation, anchored to the JSON Pointer of the offending value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleViolation {
    pub pointer: String,
    pub message: String,
}

/// What a passing or failing check looks like once it has been checked
/// against the contract below — never constructed directly from a script's
/// raw return value. See [`crate::check::run_check`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckOutcome {
    pub pass: bool,
    pub violations: Vec<RuleViolation>,
}

/// The raw shape a script's `check` function must return, before it has
/// been cross-checked against the output it ran over. Kept separate from
/// [`CheckOutcome`] because `serde`'s `Deserialize` only proves the JSON
/// *parsed* — it says nothing about whether `pass` agrees with
/// `violations`, or whether a pointer resolves.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawCheckOutcome {
    pub pass: bool,
    #[serde(default)]
    pub violations: Vec<RuleViolation>,
}

/// A script that could not be trusted to answer at all. This marks the
/// **rule** broken, never the run it was checked against — PLAN.md's
/// invariant that a bad script is the Rule Agent's problem to fix, not a
/// reason to fail every conversion using the corpus it runs over.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BrokenRule {
    /// The script itself, or the driver wrapping it, threw.
    #[error("script threw: {0}")]
    Threw(String),

    /// `check` was never defined, or was defined as something uncallable.
    #[error("check() must define a function named `check(output, ctx)`")]
    NoCheckFunction,

    /// The value `check` returned did not even parse as `{ pass, violations:
    /// [{ pointer, message }] }`.
    #[error("check() must return {{ pass: boolean, violations: [{{pointer, message}}] }}: {0}")]
    MalformedResult(String),

    /// `pass` must be the logical negation of "any violations" — a script
    /// disagreeing with its own evidence cannot be trusted on either count.
    #[error("check() returned pass={pass} with {violation_count} violation(s) — these must agree")]
    PassViolationsDisagree { pass: bool, violation_count: usize },

    /// A violation named a JSON Pointer that does not resolve against the
    /// output it was checked against. This is the single most likely way a
    /// generated script is subtly wrong, and it is caught here rather than
    /// surfacing as a broken link on the review page.
    #[error("violation pointer {pointer:?} does not resolve against the output: {reason}")]
    UnresolvablePointer { pointer: String, reason: String },

    /// The script ran past its wall-clock budget. See [`crate::budget`] for
    /// why this is detected rather than pre-empted.
    #[error("script exceeded its {0:?} wall-clock budget")]
    Timeout(std::time::Duration),

    /// The script's top-level `requires` is not an array of distinct fact
    /// names -- see [`crate::requires::read_requires`].
    #[error("invalid `requires` declaration: {0}")]
    MalformedRequires(String),

    /// An `ingest_script` fact's script defines no `extract(ingest)`.
    #[error("an ingest_script fact must define a function named `extract(ingest)`")]
    NoExtractFunction,
}

/// Cross-checks a freshly parsed [`RawCheckOutcome`] against the output it
/// was computed over, producing a trustworthy [`CheckOutcome`] or naming
/// exactly which guarantee the script broke.
pub(crate) fn validate_outcome(
    raw: RawCheckOutcome,
    output: &serde_json::Value,
) -> Result<CheckOutcome, BrokenRule> {
    if raw.pass != raw.violations.is_empty() {
        return Err(BrokenRule::PassViolationsDisagree {
            pass: raw.pass,
            violation_count: raw.violations.len(),
        });
    }

    for violation in &raw.violations {
        let pointer: jsonptr::PointerBuf =
            violation
                .pointer
                .parse()
                .map_err(|err: jsonptr::ParseError| BrokenRule::UnresolvablePointer {
                    pointer: violation.pointer.clone(),
                    reason: err.to_string(),
                })?;
        pointer
            .resolve(output)
            .map_err(|err| BrokenRule::UnresolvablePointer {
                pointer: violation.pointer.clone(),
                reason: err.to_string(),
            })?;
    }

    Ok(CheckOutcome {
        pass: raw.pass,
        violations: raw.violations,
    })
}

/// A fix script that could not be trusted to answer, or whose operations
/// were rejected before or after being proven against the check they claim
/// to repair. Mirrors [`BrokenRule`]'s discipline — see this module's own
/// doc comment for why the discipline is split across two files.
///
/// Never surfaces as the *rule's* verdict: a broken fix leaves the check's
/// own [`crate::check::CheckVerdict::Negative`] verdict and violations
/// exactly as they were. A rule whose check is right and whose fix is wrong
/// is half-broken, and saying so plainly is more useful than declaring the
/// whole rule broken over a repair nobody asked the check to attempt.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BrokenFix {
    /// The script itself, or the driver wrapping it, threw.
    #[error("fix script threw: {0}")]
    Threw(String),

    /// `fix` was never defined, or was defined as something uncallable.
    #[error("fix() must define a function named `fix(output, ctx)`")]
    NoFixFunction,

    /// The value `fix` returned did not even parse as a JSON array, or the
    /// array did not parse as RFC 6902 operations.
    #[error("fix() must return an array of RFC 6902 operations: {0}")]
    MalformedOps(String),

    /// An empty patch for a check that is currently failing — the fix
    /// claims a repair and performs none, which is exactly as untrustworthy
    /// as a script that never answered.
    #[error("fix() returned no operations for a failing check")]
    NoOps,

    /// `json_patch::patch` refused the operations against a clone of the
    /// output — a malformed path, a failed `test` op, an out-of-bounds
    /// index.
    #[error("fix()'s operations were rejected: {0}")]
    OpsRejected(String),

    /// The operations applied cleanly, but re-running `check(output, ctx)`
    /// against the fixed clone still fails. This is the load-bearing
    /// variant: it is what makes a fix's operations trustworthy without
    /// trusting the fix script itself, the same role
    /// [`BrokenRule::UnresolvablePointer`] plays for a check's own
    /// pointers.
    #[error("fix()'s operations did not resolve the violation(s) they were computed for")]
    DidNotFix { remaining: Vec<RuleViolation> },

    /// The fix script ran past its wall-clock budget.
    #[error("fix script exceeded its {0:?} wall-clock budget")]
    Timeout(std::time::Duration),
}

/// Parses `raw` as the RFC 6902 patch `fix(output, ctx)` must return, or
/// names exactly why it does not qualify. A separate step from applying it
/// (see [`apply_fix_ops`]) so a malformed shape and a rejected application
/// are two distinct, distinguishable [`BrokenFix`] variants rather than one
/// catch-all.
pub(crate) fn parse_fix_ops(
    raw: &serde_json::Value,
) -> Result<Vec<json_patch::PatchOperation>, BrokenFix> {
    if !raw.is_array() {
        return Err(BrokenFix::MalformedOps(
            "fix() must return an array".to_owned(),
        ));
    }
    let ops: Vec<json_patch::PatchOperation> = serde_json::from_value(raw.clone())
        .map_err(|err| BrokenFix::MalformedOps(err.to_string()))?;
    if ops.is_empty() {
        return Err(BrokenFix::NoOps);
    }
    Ok(ops)
}

/// Applies `ops` to a *clone* of `output`, returning the fixed document
/// without ever mutating the caller's own copy — the caller still needs the
/// original to build the driver for the re-check that follows.
pub(crate) fn apply_fix_ops(
    ops: &[json_patch::PatchOperation],
    output: &serde_json::Value,
) -> Result<serde_json::Value, BrokenFix> {
    let mut fixed = output.clone();
    json_patch::patch(&mut fixed, ops).map_err(|err| BrokenFix::OpsRejected(err.to_string()))?;
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agreeing_pass_and_empty_violations_is_accepted() {
        let raw = RawCheckOutcome {
            pass: true,
            violations: vec![],
        };
        let outcome = validate_outcome(raw, &json!({})).expect("must be accepted");
        assert!(outcome.pass);
        assert!(outcome.violations.is_empty());
    }

    #[test]
    fn pass_true_with_violations_is_broken() {
        let raw = RawCheckOutcome {
            pass: true,
            violations: vec![RuleViolation {
                pointer: "/a".to_owned(),
                message: "x".to_owned(),
            }],
        };
        let err = validate_outcome(raw, &json!({ "a": 1 })).unwrap_err();
        assert!(matches!(err, BrokenRule::PassViolationsDisagree { .. }));
    }

    #[test]
    fn pass_false_with_no_violations_is_broken() {
        let raw = RawCheckOutcome {
            pass: false,
            violations: vec![],
        };
        let err = validate_outcome(raw, &json!({})).unwrap_err();
        assert!(matches!(err, BrokenRule::PassViolationsDisagree { .. }));
    }

    #[test]
    fn a_pointer_that_does_not_resolve_is_broken() {
        let raw = RawCheckOutcome {
            pass: false,
            violations: vec![RuleViolation {
                pointer: "/nowhere".to_owned(),
                message: "x".to_owned(),
            }],
        };
        let err = validate_outcome(raw, &json!({ "a": 1 })).unwrap_err();
        assert!(matches!(err, BrokenRule::UnresolvablePointer { .. }));
    }

    #[test]
    fn an_unparseable_pointer_is_broken() {
        let raw = RawCheckOutcome {
            pass: false,
            violations: vec![RuleViolation {
                pointer: "not-a-pointer".to_owned(),
                message: "x".to_owned(),
            }],
        };
        let err = validate_outcome(raw, &json!({ "a": 1 })).unwrap_err();
        assert!(matches!(err, BrokenRule::UnresolvablePointer { .. }));
    }

    #[test]
    fn fix_ops_must_be_an_array() {
        let err = parse_fix_ops(&json!({ "op": "remove", "path": "/a" })).unwrap_err();
        assert!(matches!(err, BrokenFix::MalformedOps(_)));
    }

    #[test]
    fn fix_ops_must_parse_as_rfc6902() {
        let err = parse_fix_ops(&json!([{ "op": "not-a-real-op" }])).unwrap_err();
        assert!(matches!(err, BrokenFix::MalformedOps(_)));
    }

    #[test]
    fn an_empty_patch_is_no_ops_not_a_no_op_success() {
        let err = parse_fix_ops(&json!([])).unwrap_err();
        assert!(matches!(err, BrokenFix::NoOps));
    }

    #[test]
    fn valid_ops_parse_and_apply() {
        let ops = parse_fix_ops(&json!([{ "op": "remove", "path": "/a" }])).unwrap();
        let fixed = apply_fix_ops(&ops, &json!({ "a": 1, "b": 2 })).unwrap();
        assert_eq!(fixed, json!({ "b": 2 }));
    }

    #[test]
    fn ops_applying_to_a_clone_never_mutate_the_original() {
        let ops = parse_fix_ops(&json!([{ "op": "remove", "path": "/a" }])).unwrap();
        let original = json!({ "a": 1 });
        let _fixed = apply_fix_ops(&ops, &original).unwrap();
        assert_eq!(
            original,
            json!({ "a": 1 }),
            "the caller's copy must be untouched"
        );
    }

    #[test]
    fn ops_rejected_by_json_patch_are_named_distinctly() {
        let ops = parse_fix_ops(&json!([{ "op": "remove", "path": "/nowhere" }])).unwrap();
        let err = apply_fix_ops(&ops, &json!({ "a": 1 })).unwrap_err();
        assert!(matches!(err, BrokenFix::OpsRejected(_)));
    }
}
