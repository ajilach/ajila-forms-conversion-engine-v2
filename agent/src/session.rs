//! Restoring a previously recorded session from the edit-history store.
//!
//! A run records its output document, after every edit, under the
//! `<session>#document` sibling of its session (see [`document_session`]).
//! Restoring reads the latest of those snapshots back.
//!
//! Sessions recorded before runs authored one document in the UBS formats hold
//! only the old tree shapes (`<session>` structured snapshots, a `#aem` tree,
//! `#headers`). They stay listed, but cannot be resumed: [`restore`] says so
//! rather than guessing a document out of them.

use serde_json::Value;

use crate::OutputTarget;

/// The session id a run's document snapshots are recorded under.
pub fn document_session(session_id: &str) -> String {
    format!("{session_id}#document")
}

/// What a session's history holds for a resumed run.
#[derive(Debug)]
pub enum Restored {
    /// The document the session last recorded.
    Document(Value),
    /// Nothing was ever recorded under the session.
    Nothing,
}

/// The document `session_id` last recorded, for a run aimed at `target`.
///
/// An error when the session holds something that cannot be resumed: only the
/// tree shapes recorded before the move to one document per run, or a
/// document of another format.
pub fn restore(session_id: &str, target: OutputTarget) -> Result<Restored, String> {
    match latest(&document_session(session_id)) {
        Some(json) => parse(&json, target).map(Restored::Document),
        None if predates_documents(session_id) => Err(format!(
            "session {session_id} was recorded before runs authored one document in the UBS \
             formats; it can be viewed in the history but not resumed. Start a new conversion \
             instead."
        )),
        None => Ok(Restored::Nothing),
    }
}

/// Parse a recorded snapshot as a document of `target`'s format.
pub fn parse(json: &str, target: OutputTarget) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("the recorded document is not JSON: {e}"))?;
    let valid = match target {
        OutputTarget::Aem => u2s_aem_ubs_mcp::UbsAemDocument::from_json(&value)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        OutputTarget::Redacto => {
            serde_json::from_value::<u2s_redacto_ubs_mcp::UbsRedactoDocument>(value.clone())
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    };
    valid
        .map(|()| value)
        .map_err(|e| format!("the recorded document is not a {} document: {e}", target.label()))
}

fn latest(session_id: &str) -> Option<String> {
    let seq = crate::db::latest_seq(session_id)?;
    crate::db::snapshot_at(session_id, seq)
}

/// Whether the session recorded the tree shapes older runs wrote.
fn predates_documents(session_id: &str) -> bool {
    [session_id.to_string(), format!("{session_id}#aem")]
        .iter()
        .any(|id| crate::db::latest_seq(id).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn aem_document() -> Value {
        json!({
            "variables": {"formrange_code": "AAEV", "formrange_entity": "019"},
            "languages": ["en"],
            "form": {"type": "Root", "title": {"en": "Form"}, "children": []}
        })
    }

    #[test]
    fn a_recorded_document_comes_back() {
        crate::db::claim_scratch_db_for_test();
        let session = format!("restore-{}", uuid::Uuid::new_v4());
        crate::db::insert_edit(&document_session(&session), "AI: json_patch", &aem_document().to_string());
        match restore(&session, OutputTarget::Aem).unwrap() {
            Restored::Document(doc) => assert_eq!(doc, aem_document()),
            other => panic!("expected the document, got {other:?}"),
        }
    }

    #[test]
    fn a_document_of_another_format_is_refused() {
        let err = parse(&aem_document().to_string(), OutputTarget::Redacto).unwrap_err();
        assert!(err.contains("Redacto"), "{err}");
    }

    /// A session from before the move keeps its history but is not resumed
    /// from a guess.
    #[test]
    fn a_session_with_only_the_old_trees_cannot_be_resumed() {
        crate::db::claim_scratch_db_for_test();
        let session = format!("old-{}", uuid::Uuid::new_v4());
        crate::db::insert_edit(&format!("{session}#aem"), "AI: set_aem_translated", "{}");
        let err = restore(&session, OutputTarget::Aem).unwrap_err();
        assert!(err.contains("cannot") || err.contains("not resumed"), "{err}");
    }

    #[test]
    fn an_unknown_session_holds_nothing() {
        crate::db::claim_scratch_db_for_test();
        let session = format!("none-{}", uuid::Uuid::new_v4());
        assert!(matches!(restore(&session, OutputTarget::Aem).unwrap(), Restored::Nothing));
    }
}
