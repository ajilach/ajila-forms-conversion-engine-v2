//! [`AgentRole`] — *which agent* may call a tool, as distinct from
//! `mcp_tools.role` (`normalize`/`decode`/`encode`/`query`), which says what
//! a tool *does*.
//!
//! It lives here, beside [`crate::FormatScope`], for exactly the same reason:
//! two crates must agree on the vocabulary and neither may depend on the
//! other. `u2s-store` stores it as the Postgres `agent_role` enum on
//! `dataset_tools.enabled_for` and on `agent_turns.agent_role`; `u2s-agent`
//! puts it on a `ToolDecl`'s `roles_allowed`. Because both sides name this
//! one type, "only the Conversion Agent may call a side-effecting tool" is a
//! single comparison rather than two conventions that can drift.
//!
//! The Rule Agent is deliberately **not** a variant of the enablement half of
//! this vocabulary — see [`AgentRole::TOOL_USING`]. It calls one native
//! tool, `rule_try` (`u2s_agent::tools::NativeJsonTool::TryRule`), to try a
//! candidate script against the reference corpus before answering, but no
//! *enableable* (MCP) tool: a per-dataset tool enablement naming it could
//! still never mean anything.

use serde::{Deserialize, Serialize};

/// One of the agents: the Conversion Agent, the Rule Agent and the Judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Conversion,
    /// Off the conversion path and interactive (PLAN.md). Calls one native
    /// tool, `rule_try`, and no MCP tool.
    Rule,
    /// Decides one judged rule (a rule without a check script) against one
    /// output and its source. Calls only reading tools: the native JSON
    /// readers over the output, and the Conversion Agent's enabled MCP tools
    /// that neither change anything nor differ between calls.
    Judge,
}

/// A role name that is not one of the agents.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown agent role {value:?}: expected one of conversion, rule, judge")]
pub struct AgentRoleError {
    pub value: String,
}

impl AgentRole {
    /// Every role, including the ones no tool enablement may name. This is
    /// the vocabulary `agent_turns.agent_role` records, because a call by any
    /// of them is still a model call worth auditing and costing.
    pub const ALL: &'static [AgentRole] =
        &[AgentRole::Conversion, AgentRole::Rule, AgentRole::Judge];

    /// The roles a tool enablement may name. Excludes [`AgentRole::Rule`],
    /// which calls no *enableable* (MCP) tool: enabling a tool for it would
    /// be a row that can never be read, which is worse than a row that cannot
    /// be written. Excludes [`AgentRole::Judge`] too: its MCP tools are the
    /// Conversion Agent's reading ones, derived rather than enabled, so a
    /// judge always reads the source through what the conversion read.
    pub const TOOL_USING: &'static [AgentRole] = &[AgentRole::Conversion];

    /// The wire and database spelling. Must stay in step with the Postgres
    /// `agent_role` enum's labels, which the enablement migration defines.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Conversion => "conversion",
            AgentRole::Rule => "rule",
            AgentRole::Judge => "judge",
        }
    }

    /// The one conversion point: untrusted text — a request body, a manifest,
    /// a database label — becomes a valid role or an error. Unlike the
    /// `RawXRow` enum parsers in `u2s-store`, this does not panic on an
    /// unknown value, because its inputs include request bodies.
    pub fn parse(value: &str) -> Result<Self, AgentRoleError> {
        AgentRole::ALL
            .iter()
            .copied()
            .find(|role| role.as_str() == value)
            .ok_or_else(|| AgentRoleError {
                value: value.to_owned(),
            })
    }

    /// Whether a tool enablement may name this role.
    pub fn is_tool_using(self) -> bool {
        AgentRole::TOOL_USING.contains(&self)
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which of PLAN.md's two phases a model call belongs to.
///
/// Here rather than in `u2s-store` for the same reason as [`AgentRole`]:
/// `u2s-store` persists it on `agent_turns.phase` and `u2s-agent` produces
/// it, and neither crate may depend on the other.
///
/// The split is a policy, not an API constraint — rig permits tools and an
/// output schema together. It buys two things: the large gather transcript
/// stays out of the schema-constrained call, and `answer_structured`'s
/// mandatory adapt/restore/validate pipeline remains the single place a
/// structured answer is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPhase {
    /// Tools on, no output schema, bounded by the call budget.
    Gather,
    /// One structured completion over the transcript, no tools.
    Answer,
    /// A summary of the turns the context window evicted, written by the
    /// model so a long gather keeps what it learnt. No tools, no schema.
    Compaction,
}

impl TurnPhase {
    pub const ALL: &'static [TurnPhase] =
        &[TurnPhase::Gather, TurnPhase::Answer, TurnPhase::Compaction];

    /// Must stay in step with the `agent_turns.phase` CHECK constraint.
    pub fn as_str(self) -> &'static str {
        match self {
            TurnPhase::Gather => "gather",
            TurnPhase::Answer => "answer",
            TurnPhase::Compaction => "compaction",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AgentRoleError> {
        TurnPhase::ALL
            .iter()
            .copied()
            .find(|phase| phase.as_str() == value)
            .ok_or_else(|| AgentRoleError {
                value: value.to_owned(),
            })
    }
}

impl std::fmt::Display for TurnPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_round_trips_through_its_wire_spelling() {
        for role in AgentRole::ALL {
            assert_eq!(
                AgentRole::parse(role.as_str()).expect("a role's own spelling parses"),
                *role
            );
        }
    }

    #[test]
    fn spellings_are_distinct_so_a_stored_label_is_unambiguous() {
        let mut seen: Vec<&str> = AgentRole::ALL.iter().map(|r| r.as_str()).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total);
    }

    #[test]
    fn an_unknown_role_is_an_error_and_never_a_panic() {
        let err = AgentRole::parse("admin").expect_err("not a role");
        assert_eq!(err.value, "admin");
        assert!(err.to_string().contains("conversion"), "{err}");
    }

    /// The Rule Agent calls `rule_try`, a native tool, but no *enableable*
    /// (MCP) tool, so it must not be enableable. Asserted rather than left
    /// to a comment, because `TOOL_USING` is what the enablement API
    /// validates against.
    #[test]
    fn only_the_conversion_agent_is_tool_using() {
        assert!(!AgentRole::Rule.is_tool_using());
        assert!(!AgentRole::Judge.is_tool_using());
        assert!(AgentRole::Conversion.is_tool_using());
        assert_eq!(AgentRole::TOOL_USING, &[AgentRole::Conversion]);
    }

    #[test]
    fn every_phase_round_trips_through_its_wire_spelling() {
        for phase in TurnPhase::ALL {
            assert_eq!(TurnPhase::parse(phase.as_str()).expect("parses"), *phase);
        }
        assert!(TurnPhase::parse("reflecting").is_err());
    }

    #[test]
    fn serde_uses_the_same_spelling_as_as_str() {
        for role in AgentRole::ALL {
            let json = serde_json::to_string(role).expect("serialize");
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            let back: AgentRole = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, *role);
        }
    }
}
