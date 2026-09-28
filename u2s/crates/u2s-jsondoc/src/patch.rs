//! [`apply`] — RFC 6902, atomic, revision-checked.
//!
//! Two guarantees, neither of them ours to get subtly wrong, so both come
//! from the `json-patch` crate rather than a hand-rolled walker:
//! **atomicity** (a failing operation partway through a patch leaves the
//! document exactly as it was — `json-patch::patch` undoes what it already
//! applied) and **`test` ops** (a conditional write: "patch only if this
//! sub-value still looks like X").
//!
//! What is ours: the **revision check**. `json-patch` has no concept of
//! optimistic concurrency — PLAN.md's "no edit lands on a version the agent
//! has not seen" is `expected_revision` compared against
//! [`Document::revision`] *before* `json-patch` ever sees the operations.

use json_patch::PatchOperation;
use serde_json::Value;

use crate::document::{Document, Revision};

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error(
        "expected revision {expected}, document is at {actual} — it changed since this was read"
    )]
    RevisionMismatch {
        expected: Revision,
        actual: Revision,
    },
    #[error("ops is not a valid RFC 6902 patch: {0}")]
    InvalidOps(#[source] serde_json::Error),
    #[error(transparent)]
    Apply(#[from] json_patch::PatchError),
}

/// Applies `ops` (a JSON array in RFC 6902 shape) to `doc`, but only if
/// `doc` is still at `expected_revision`. On success `doc`'s revision
/// advances by exactly one, regardless of how many operations were in the
/// patch — one call is one edit, whether it touched one field or ten.
///
/// On any failure — stale revision, a malformed op, a failed `test`, an
/// out-of-bounds path — `doc` is left completely unchanged. A caller never
/// has to distinguish "failed before touching anything" from "failed after
/// partially applying"; there is no partial case.
pub fn apply(
    doc: &mut Document,
    ops: &Value,
    expected_revision: Revision,
) -> Result<(), PatchError> {
    if doc.revision() != expected_revision {
        return Err(PatchError::RevisionMismatch {
            expected: expected_revision,
            actual: doc.revision(),
        });
    }

    let operations: Vec<PatchOperation> =
        serde_json::from_value(ops.clone()).map_err(PatchError::InvalidOps)?;

    json_patch::patch(doc.value_mut(), &operations)?;
    doc.advance();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_successful_patch_applies_and_advances_the_revision() {
        let mut doc = Document::new(json!({ "a": 1 }));
        let rev = doc.revision();

        apply(
            &mut doc,
            &json!([{ "op": "replace", "path": "/a", "value": 2 }]),
            rev,
        )
        .unwrap();

        assert_eq!(doc.value(), &json!({ "a": 2 }));
        assert_eq!(doc.revision().get(), rev.get() + 1);
    }

    #[test]
    fn a_stale_revision_is_refused_and_nothing_changes() {
        let mut doc = Document::new(json!({ "a": 1 }));
        let stale = doc.revision();
        apply(
            &mut doc,
            &json!([{ "op": "replace", "path": "/a", "value": 2 }]),
            stale,
        )
        .unwrap();

        // doc is now at revision 1; try again with the now-stale revision 0.
        let err = apply(
            &mut doc,
            &json!([{ "op": "replace", "path": "/a", "value": 3 }]),
            stale,
        )
        .expect_err("must refuse a stale revision");
        assert!(matches!(err, PatchError::RevisionMismatch { .. }));
        assert_eq!(
            doc.value(),
            &json!({ "a": 2 }),
            "the refused patch must not have applied"
        );
    }

    #[test]
    fn a_failed_test_op_leaves_the_document_untouched_atomicity() {
        let mut doc = Document::new(json!({ "a": 1, "b": 1 }));
        let rev = doc.revision();

        let ops = json!([
            { "op": "replace", "path": "/a", "value": 99 },
            { "op": "test", "path": "/b", "value": 2 }
        ]);
        let err = apply(&mut doc, &ops, rev).expect_err("the test op must fail");
        assert!(matches!(err, PatchError::Apply(_)));

        // The first op (replace /a) must have been undone — atomicity, not
        // just "an error was reported".
        assert_eq!(doc.value(), &json!({ "a": 1, "b": 1 }));
        assert_eq!(
            doc.revision(),
            rev,
            "a failed patch must not advance the revision"
        );
    }

    #[test]
    fn an_out_of_bounds_path_fails_without_partial_mutation() {
        let mut doc = Document::new(json!({ "items": [1, 2] }));
        let rev = doc.revision();

        let ops = json!([
            { "op": "add", "path": "/marker", "value": true },
            { "op": "replace", "path": "/items/99", "value": 0 }
        ]);
        assert!(apply(&mut doc, &ops, rev).is_err());
        assert_eq!(
            doc.value(),
            &json!({ "items": [1, 2] }),
            "the marker add must have been undone too"
        );
    }

    #[test]
    fn malformed_ops_are_reported_distinctly_and_change_nothing() {
        let mut doc = Document::new(json!({ "a": 1 }));
        let rev = doc.revision();
        let err = apply(&mut doc, &json!([{ "op": "not-a-real-op" }]), rev).expect_err("must fail");
        assert!(matches!(err, PatchError::InvalidOps(_)));
        assert_eq!(doc.value(), &json!({ "a": 1 }));
    }

    #[test]
    fn a_conditional_write_via_test_then_edit_only_takes_effect_when_the_condition_holds() {
        let mut doc = Document::new(json!({ "status": "draft" }));
        let rev = doc.revision();
        let ops = json!([
            { "op": "test", "path": "/status", "value": "draft" },
            { "op": "replace", "path": "/status", "value": "active" }
        ]);
        apply(&mut doc, &ops, rev).unwrap();
        assert_eq!(doc.value(), &json!({ "status": "active" }));
    }

    #[test]
    fn one_call_is_one_revision_step_regardless_of_op_count() {
        let mut doc = Document::new(json!({ "a": 1, "b": 1, "c": 1 }));
        let rev = doc.revision();
        let ops = json!([
            { "op": "replace", "path": "/a", "value": 2 },
            { "op": "replace", "path": "/b", "value": 2 },
            { "op": "replace", "path": "/c", "value": 2 }
        ]);
        apply(&mut doc, &ops, rev).unwrap();
        assert_eq!(doc.revision().get(), rev.get() + 1);
    }
}
