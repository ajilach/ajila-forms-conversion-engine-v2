//! Opaque store identities. Each wraps a [`Uuid`] behind its own type so a
//! `DatasetId` and (in a later milestone) a `RuleId` cannot be swapped for
//! one another at a call site by accident — the store API's discipline of
//! taking a compound key (e.g. `rule_in_dataset(DatasetId, RuleId)` rather
//! than a bare `rule(RuleId)`) depends on the types genuinely being distinct.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Declares a `Uuid` newtype with no cross-type conversions: turning a
/// `DatasetId` into a `RuleId` must go through `Uuid` explicitly, never
/// silently.
macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new(id: Uuid) -> Self {
                Self(id)
            }

            /// A fresh random identity. For inserts the store performs;
            /// never derived from anything an untrusted caller sent.
            pub fn generate() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::from_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

uuid_id!(
    /// A dataset's store identity — `datasets.id`. The Keycloak-facing name
    /// is [`crate::slug::DatasetSlug`]; the two are looked up together and
    /// neither substitutes for the other.
    DatasetId
);

uuid_id!(
    /// A registered MCP server's store identity — `mcp_servers.id`. This is
    /// the identity `u2s-mcp`'s [`crate::FormatScope`]-agnostic resolution
    /// deliberately does *not* know about (its `ToolCandidate::server_id` is
    /// a plain `String`): the app converts between the two at the service
    /// layer, which is where persistence and resolution meet.
    McpServerId
);

uuid_id!(
    /// A discovered tool's store identity — `mcp_tools.id`.
    McpToolId
);

uuid_id!(
    /// A stored input's identity — `inputs.id`. One row per distinct
    /// document, deduped by content hash within a dataset; this identity,
    /// not the hash, is what `runs.input_id` keys on.
    InputId
);

uuid_id!(
    /// One uploaded file within an input — `input_files.id`. Kept distinct
    /// from [`InputId`]: an input is the logical submission a run converts
    /// (and the corpus unit), a file is one of the bytes blobs
    /// that make it up, and `input_pages` keys on this identity rather than
    /// on `InputId` because two files can each have a page 1.
    InputFileId
);

uuid_id!(
    /// A conversion's identity — `runs.id`.
    RunId
);

uuid_id!(
    /// One long-running piece of agent work — `agent_jobs.id`.
    ///
    /// Kept distinct from [`RunId`] even though a conversion job has
    /// exactly one run: they are different lifetimes. A run is the domain
    /// object a reviewer looks at forever; a job is the execution that
    /// produced it, and a resumed job is a second attempt at the same run.
    /// Collapsing the two would make "which attempt failed" unaskable.
    JobId
);

uuid_id!(
    /// A rule's identity — `rules.id`. Revisions and reference verdicts key
    /// on this plus their own `(revision)` / `(run_id, revision)`, never on a
    /// bare revision id, so "which rule does this belong to" is never a
    /// second lookup.
    RuleId
);

uuid_id!(
    /// One generated-or-refined version of a rule's check script —
    /// `rule_revisions.id`. Kept distinct from [`RuleId`] on purpose: a
    /// rule's `current_revision_id` and `draft_revision_id` are two
    /// different revisions of the *same* rule, and mixing up which one a
    /// verdict was checked against is exactly the bug PLAN.md's side-by-side
    /// reference table exists to make visible instead of silent.
    RuleRevisionId
);

uuid_id!(
    /// A fact's identity -- `facts.id`. A fact is a named question about an
    /// input that extrinsic rules read through `ctx.facts`; its question and
    /// answer schema live on its revisions.
    FactId
);

uuid_id!(
    /// One version of a fact's question, answer schema and source --
    /// `fact_revisions.id`. Rule revisions pin this identity, never a bare
    /// [`FactId`], so a later revision of a fact cannot change the verdicts
    /// of a rule revision that was saved against an earlier one.
    FactRevisionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string() {
        let id = DatasetId::generate();
        let s = id.to_string();
        assert_eq!(DatasetId::from_str(&s).unwrap(), id);
    }

    #[test]
    fn rejects_a_non_uuid_string() {
        assert!(DatasetId::from_str("not-a-uuid").is_err());
    }

    #[test]
    fn distinct_generated_ids_differ() {
        assert_ne!(DatasetId::generate(), DatasetId::generate());
    }

    #[test]
    fn serializes_as_a_plain_uuid_string() {
        let id = DatasetId::new(Uuid::nil());
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"00000000-0000-0000-0000-000000000000\""
        );
    }
}
