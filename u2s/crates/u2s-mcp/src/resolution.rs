//! For a `(role, FormatScope)`, which tool wins and why — pure, no I/O, so
//! `GET /v1/datasets/{id}/adapters` can compute and explain this without a
//! network round trip once the candidates are in hand.
//!
//! Deliberately decoupled from persistence: a [`ToolCandidate`] identifies a
//! server by whatever `String` id the caller uses (an `mcp_servers.id` once
//! that table exists — step 4 of `plan-the-agent-engine.md`), so this module
//! has no opinion on where candidates come from. `u2s-mcp` stays usable
//! without a database.

use u2s_core::{FormatId, FormatScope, format_matches};

use crate::manifest::{IngestCapability, ToolRole, VerifyCapability};

/// One tool a server has offered, in the shape resolution needs to compare
/// it against others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCandidate {
    pub server_id: String,
    pub tool: String,
    pub role: ToolRole,
    /// `Some` when this candidate opts into serving one of the normalizer's
    /// ingest steps — see [`IngestCapability`] and [`ingest_candidates`].
    pub ingest: Option<IngestCapability>,
    /// `Some` when this candidate opts into serving the platform's
    /// deterministic verification pass — see [`VerifyCapability`] and
    /// [`verify_candidate`].
    pub verify: Option<VerifyCapability>,
    pub scope: FormatScope,
}

/// Why a candidate won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionReason {
    /// A dataset explicitly pinned this server for this role.
    Pinned,
    /// Exactly one registered candidate matches; nothing to choose between.
    OnlyMatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionOutcome {
    Resolved {
        server_id: String,
        tool: String,
        reason: ResolutionReason,
    },
    /// No registered candidate covers this `(role, output)`.
    NoMatch,
    /// More than one candidate matches and nothing breaks the tie. Refused
    /// rather than resolved arbitrarily (PLAN.md: a genuine tie is an
    /// operator decision, via a pin, not a coin flip this function makes for
    /// them).
    Ambiguous { candidates: Vec<ToolCandidate> },
}

/// Resolves which tool serves `role` for `output`.
///
/// `pin`, when given, names a server id that must be preferred: if it has a
/// matching candidate, that candidate wins regardless of how many others also
/// match. A pin naming a server with no matching candidate is not an error
/// here — resolution falls through to the normal rule, since "pinned to a
/// server that cannot serve this format" is a configuration problem the
/// caller surfaces, not something this function should paper over by picking
/// a different winner silently.
///
/// Not used for ingest resolution — see [`ingest_candidates`], which
/// deliberately does not refuse a tie: a tie between two servers both
/// serving `info`, say, is the ordinary case once a second document format
/// is registered, with no input side left to break it.
pub fn resolve(
    candidates: &[ToolCandidate],
    role: ToolRole,
    output: &FormatId,
    pin: Option<&str>,
) -> ResolutionOutcome {
    resolve_matching(
        candidates,
        |c| c.role == role && format_matches(&c.scope, output),
        pin,
    )
}

/// The tie-breaking rule shared by every "exactly one tool may claim this"
/// resolution: a pin wins outright, no match is `NoMatch`, one match is
/// `OnlyMatch`, and more than one is `Ambiguous` — refused rather than
/// resolved arbitrarily, because a genuine tie is an operator decision via a
/// pin, not a coin flip this function makes for them. [`resolve`] and
/// [`verify_candidate`] both reduce to this with a different predicate; only
/// [`ingest_candidates`] genuinely differs, because a tie there is the
/// normal case rather than a conflict.
fn resolve_matching(
    candidates: &[ToolCandidate],
    matches: impl Fn(&ToolCandidate) -> bool,
    pin: Option<&str>,
) -> ResolutionOutcome {
    let matching: Vec<&ToolCandidate> = candidates.iter().filter(|c| matches(c)).collect();

    if let Some(pinned_id) = pin
        && let Some(candidate) = matching.iter().find(|c| c.server_id == pinned_id)
    {
        return ResolutionOutcome::Resolved {
            server_id: candidate.server_id.clone(),
            tool: candidate.tool.clone(),
            reason: ResolutionReason::Pinned,
        };
    }

    match matching.len() {
        0 => ResolutionOutcome::NoMatch,
        1 => ResolutionOutcome::Resolved {
            server_id: matching[0].server_id.clone(),
            tool: matching[0].tool.clone(),
            reason: ResolutionReason::OnlyMatch,
        },
        _ => ResolutionOutcome::Ambiguous {
            candidates: matching.into_iter().cloned().collect(),
        },
    }
}

/// Resolves which tool runs the platform's deterministic verification pass
/// for `output` — the `query` tool, if any, declaring
/// [`VerifyCapability::Run`] whose scope matches.
///
/// Shares `resolve`'s tie-breaking rule rather than `ingest_candidates`'
/// ordered-probe one: unlike ingest, which happens before any output
/// opinion exists and so has no format left to disambiguate on, a
/// verifier's whole point is that it is scoped to one output format. Two
/// verifiers both claiming that format is a genuine configuration conflict
/// — refused, exactly like two encoders for the same format would be — not
/// a list to probe in order.
pub fn verify_candidate(
    candidates: &[ToolCandidate],
    output: &FormatId,
    pin: Option<&str>,
) -> ResolutionOutcome {
    resolve_matching(
        candidates,
        |c| c.verify == Some(VerifyCapability::Run) && format_matches(&c.scope, output),
        pin,
    )
}

/// Every registered tool serving `capability`, in a total order that does
/// not depend on registry insertion order — `(server_id, tool)` — with a
/// `pin`'s server moved to the front rather than filtered, since a pinned
/// server that cannot handle a particular document must still let the probe
/// fall through to the next candidate.
///
/// Deliberately not `resolve`: `resolve` refuses a genuine tie, and under
/// capability addressing a tie is the *normal* case the moment a second
/// server serves the same capability, with no input format left to
/// disambiguate. An ordered probe list is what lets the caller try one
/// candidate, accept "not applicable" or a tool error as a normal answer,
/// and move to the next — see `service::normalize`'s election. Scope is not
/// consulted: ingest happens before any output opinion exists.
pub fn ingest_candidates<'a>(
    candidates: &'a [ToolCandidate],
    capability: IngestCapability,
    pin: Option<&str>,
) -> Vec<&'a ToolCandidate> {
    let mut matching: Vec<&ToolCandidate> = candidates
        .iter()
        .filter(|c| c.ingest == Some(capability))
        .collect();
    matching.sort_by(|a, b| (&a.server_id, &a.tool).cmp(&(&b.server_id, &b.tool)));

    if let Some(pinned_id) = pin
        && let Some(pos) = matching.iter().position(|c| c.server_id == pinned_id)
    {
        let pinned = matching.remove(pos);
        matching.insert(0, pinned);
    }

    matching
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> FormatId {
        FormatId::parse(s).expect("valid in tests")
    }

    fn candidate(server: &str, tool: &str, role: ToolRole, scope: FormatScope) -> ToolCandidate {
        ToolCandidate {
            server_id: server.to_owned(),
            tool: tool.to_owned(),
            role,
            ingest: None,
            verify: None,
            scope,
        }
    }

    fn ingest_candidate(server: &str, tool: &str, capability: IngestCapability) -> ToolCandidate {
        ToolCandidate {
            server_id: server.to_owned(),
            tool: tool.to_owned(),
            role: ToolRole::Query,
            ingest: Some(capability),
            verify: None,
            scope: FormatScope::any(),
        }
    }

    fn verify_candidate_row(server: &str, tool: &str, scope: FormatScope) -> ToolCandidate {
        ToolCandidate {
            server_id: server.to_owned(),
            tool: tool.to_owned(),
            role: ToolRole::Query,
            ingest: None,
            verify: Some(VerifyCapability::Run),
            scope,
        }
    }

    #[test]
    fn the_only_matching_candidate_wins() {
        let candidates = vec![candidate(
            "srv-1",
            "xfa_packets",
            ToolRole::Query,
            FormatScope::only(fmt("aem")),
        )];
        let outcome = resolve(&candidates, ToolRole::Query, &fmt("aem"), None);
        assert_eq!(
            outcome,
            ResolutionOutcome::Resolved {
                server_id: "srv-1".into(),
                tool: "xfa_packets".into(),
                reason: ResolutionReason::OnlyMatch,
            }
        );
    }

    #[test]
    fn no_candidate_covering_the_output_format_is_no_match() {
        let candidates = vec![candidate(
            "srv-1",
            "xfa_packets",
            ToolRole::Query,
            FormatScope::only(fmt("aem")),
        )];
        let outcome = resolve(&candidates, ToolRole::Query, &fmt("other"), None);
        assert_eq!(outcome, ResolutionOutcome::NoMatch);
    }

    #[test]
    fn two_matching_candidates_are_ambiguous_without_a_pin() {
        let candidates = vec![
            candidate("srv-1", "render_a", ToolRole::Query, FormatScope::any()),
            candidate("srv-2", "render_b", ToolRole::Query, FormatScope::any()),
        ];
        let outcome = resolve(&candidates, ToolRole::Query, &fmt("aem"), None);
        assert!(
            matches!(outcome, ResolutionOutcome::Ambiguous { candidates } if candidates.len() == 2)
        );
    }

    #[test]
    fn a_pin_breaks_the_tie() {
        let candidates = vec![
            candidate("srv-1", "render_a", ToolRole::Query, FormatScope::any()),
            candidate("srv-2", "render_b", ToolRole::Query, FormatScope::any()),
        ];
        let outcome = resolve(&candidates, ToolRole::Query, &fmt("aem"), Some("srv-2"));
        assert_eq!(
            outcome,
            ResolutionOutcome::Resolved {
                server_id: "srv-2".into(),
                tool: "render_b".into(),
                reason: ResolutionReason::Pinned,
            }
        );
    }

    #[test]
    fn a_pin_naming_a_server_without_a_matching_candidate_falls_through_rather_than_erroring() {
        let candidates = vec![candidate(
            "srv-1",
            "xfa_packets",
            ToolRole::Query,
            FormatScope::any(),
        )];
        let outcome = resolve(
            &candidates,
            ToolRole::Query,
            &fmt("aem"),
            Some("srv-does-not-exist"),
        );
        assert_eq!(
            outcome,
            ResolutionOutcome::Resolved {
                server_id: "srv-1".into(),
                tool: "xfa_packets".into(),
                reason: ResolutionReason::OnlyMatch,
            }
        );
    }

    #[test]
    fn role_mismatch_is_not_a_candidate() {
        let candidates = vec![candidate(
            "srv-1",
            "aem_encode",
            ToolRole::Encode,
            FormatScope::any(),
        )];
        let outcome = resolve(&candidates, ToolRole::Query, &fmt("aem"), None);
        assert_eq!(outcome, ResolutionOutcome::NoMatch);
    }

    #[test]
    fn candidates_are_ordered_independently_of_registry_order() {
        let candidates = vec![
            ingest_candidate("srv-z", "z_info", IngestCapability::Info),
            ingest_candidate("srv-a", "a_info", IngestCapability::Info),
        ];
        let ordered = ingest_candidates(&candidates, IngestCapability::Info, None);
        assert_eq!(
            ordered.iter().map(|c| c.server_id.as_str()).collect::<Vec<_>>(),
            vec!["srv-a", "srv-z"],
            "order must not depend on the order candidates were passed in"
        );
    }

    #[test]
    fn a_pin_moves_a_server_to_the_front_without_removing_the_others() {
        let candidates = vec![
            ingest_candidate("srv-a", "a_info", IngestCapability::Info),
            ingest_candidate("srv-b", "b_info", IngestCapability::Info),
        ];
        let ordered = ingest_candidates(&candidates, IngestCapability::Info, Some("srv-b"));
        assert_eq!(
            ordered.iter().map(|c| c.server_id.as_str()).collect::<Vec<_>>(),
            vec!["srv-b", "srv-a"],
            "the pinned server leads, but the other candidate must still be reachable if it declines"
        );
    }

    #[test]
    fn a_capability_nobody_serves_is_an_empty_list_not_an_error() {
        let candidates = vec![ingest_candidate("srv-a", "a_info", IngestCapability::Info)];
        let ordered = ingest_candidates(&candidates, IngestCapability::Render, None);
        assert!(ordered.is_empty());
    }

    #[test]
    fn two_servers_serving_one_capability_is_not_ambiguous() {
        // Unlike `resolve`, a tie among ingest candidates is not refused --
        // it is the normal shape once a second document-format server is
        // registered, and there is no input side left to break it.
        let candidates = vec![
            ingest_candidate("srv-a", "a_info", IngestCapability::Info),
            ingest_candidate("srv-b", "b_info", IngestCapability::Info),
        ];
        let ordered = ingest_candidates(&candidates, IngestCapability::Info, None);
        assert_eq!(ordered.len(), 2);
    }

    #[test]
    fn the_only_matching_verifier_wins() {
        let candidates = vec![verify_candidate_row(
            "srv-1",
            "verify_run",
            FormatScope::only(fmt("aem-ubs")),
        )];
        let outcome = verify_candidate(&candidates, &fmt("aem-ubs"), None);
        assert_eq!(
            outcome,
            ResolutionOutcome::Resolved {
                server_id: "srv-1".into(),
                tool: "verify_run".into(),
                reason: ResolutionReason::OnlyMatch,
            }
        );
    }

    #[test]
    fn a_verifier_scoped_to_a_different_format_is_no_match() {
        let candidates = vec![verify_candidate_row(
            "srv-1",
            "verify_run",
            FormatScope::only(fmt("aem-ubs")),
        )];
        let outcome = verify_candidate(&candidates, &fmt("aem"), None);
        assert_eq!(outcome, ResolutionOutcome::NoMatch);
    }

    #[test]
    fn a_query_tool_without_verify_is_not_a_verify_candidate() {
        let candidates = vec![candidate(
            "srv-1",
            "fragment_search",
            ToolRole::Query,
            FormatScope::only(fmt("aem-ubs")),
        )];
        let outcome = verify_candidate(&candidates, &fmt("aem-ubs"), None);
        assert_eq!(outcome, ResolutionOutcome::NoMatch);
    }

    #[test]
    fn two_verifiers_claiming_the_same_format_are_ambiguous_without_a_pin() {
        let candidates = vec![
            verify_candidate_row("srv-1", "verify_run_a", FormatScope::only(fmt("aem-ubs"))),
            verify_candidate_row("srv-2", "verify_run_b", FormatScope::only(fmt("aem-ubs"))),
        ];
        let outcome = verify_candidate(&candidates, &fmt("aem-ubs"), None);
        assert!(
            matches!(outcome, ResolutionOutcome::Ambiguous { candidates } if candidates.len() == 2)
        );
    }

    #[test]
    fn a_pin_breaks_a_tie_between_verifiers() {
        let candidates = vec![
            verify_candidate_row("srv-1", "verify_run_a", FormatScope::only(fmt("aem-ubs"))),
            verify_candidate_row("srv-2", "verify_run_b", FormatScope::only(fmt("aem-ubs"))),
        ];
        let outcome = verify_candidate(&candidates, &fmt("aem-ubs"), Some("srv-2"));
        assert_eq!(
            outcome,
            ResolutionOutcome::Resolved {
                server_id: "srv-2".into(),
                tool: "verify_run_b".into(),
                reason: ResolutionReason::Pinned,
            }
        );
    }
}
