//! How the run box lists the rules: which mark each rule gets, in which order
//! they are shown, and the count in the list's header.

use std::collections::BTreeSet;

use agent::{RuleKind, RuleState, RuleView};

/// The mark a rule's row shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mark {
    /// A judge is judging it right now.
    Judging,
    Pass,
    Fail,
    /// A check ran but gave no verdict.
    Unchecked,
    NotChecked,
}

impl Mark {
    /// Modifier class and glyph; `None` glyph means a spinner.
    pub fn badge(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Judging => ("run", None),
            Self::Pass => ("ok", Some("✓")),
            Self::Fail => ("err", Some("✗")),
            Self::Unchecked => ("warn", Some("–")),
            Self::NotChecked => ("idle", Some("○")),
        }
    }
}

/// One row of the rule list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleRow {
    pub rule_id: String,
    pub title: String,
    pub kind: &'static str,
    pub mark: Mark,
    /// What the row says next to the title: the violation count, or why the
    /// rule has no verdict.
    pub note: Option<String>,
    pub outdated: bool,
}

/// The rows in display order: what needs attention first (failing rules),
/// then what has no current verdict, then the passing rules; the rules' own
/// order within each group.
pub fn rule_rows(rules: &[RuleView], judging: &BTreeSet<String>) -> Vec<RuleRow> {
    // Ordered by the verdict, not the mark: a rule must not jump around the
    // list while its judge runs.
    let mut ordered: Vec<&RuleView> = rules.iter().collect();
    ordered.sort_by_key(|rule| match rule.state {
        RuleState::Fail { .. } if !rule.outdated => 0,
        RuleState::Pass if !rule.outdated => 2,
        _ => 1,
    });
    ordered
        .into_iter()
        .map(|rule| {
            let (mark, note) = match &rule.state {
                _ if judging.contains(&rule.rule_id) => (Mark::Judging, Some("judging".to_string())),
                RuleState::Pass => (Mark::Pass, None),
                RuleState::Fail { violations } => (
                    Mark::Fail,
                    Some(match violations {
                        1 => "1 violation".to_string(),
                        n => format!("{n} violations"),
                    }),
                ),
                RuleState::Unchecked { reason } => (Mark::Unchecked, Some(reason.clone())),
                RuleState::NotChecked => (Mark::NotChecked, Some("not checked yet".to_string())),
            };
            RuleRow {
                rule_id: rule.rule_id.clone(),
                title: rule.title.clone(),
                kind: match rule.kind {
                    RuleKind::Script => "script",
                    RuleKind::Judge => "judge",
                },
                mark,
                note,
                outdated: rule.outdated,
            }
        })
        .collect()
}

/// The list header's count: how many rules pass, of all of them.
pub fn passing(rules: &[RuleView]) -> (usize, usize) {
    let pass = rules.iter().filter(|r| r.state == RuleState::Pass && !r.outdated).count();
    (pass, rules.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, kind: RuleKind, state: RuleState, outdated: bool) -> RuleView {
        RuleView { rule_id: id.into(), title: id.into(), kind, state, outdated }
    }

    fn rules() -> Vec<RuleView> {
        vec![
            rule("pass", RuleKind::Script, RuleState::Pass, false),
            rule("not", RuleKind::Judge, RuleState::NotChecked, false),
            rule("fail", RuleKind::Script, RuleState::Fail { violations: 2 }, false),
            rule("stale", RuleKind::Judge, RuleState::Pass, true),
            rule("broke", RuleKind::Script, RuleState::Unchecked { reason: "threw".into() }, false),
            rule("fail1", RuleKind::Judge, RuleState::Fail { violations: 1 }, false),
        ]
    }

    #[test]
    fn failing_rules_come_first_and_passing_last() {
        let rows = rule_rows(&rules(), &BTreeSet::new());
        let order: Vec<_> = rows.iter().map(|r| r.rule_id.as_str()).collect();
        assert_eq!(order, ["fail", "fail1", "not", "stale", "broke", "pass"]);
        assert_eq!(rows[0].note.as_deref(), Some("2 violations"));
        assert_eq!(rows[1].note.as_deref(), Some("1 violation"));
        assert_eq!(rows[4].note.as_deref(), Some("threw"));
        assert!(rows[3].outdated && rows[3].mark == Mark::Pass);
    }

    #[test]
    fn a_rule_being_judged_shows_the_spinner_whatever_it_had() {
        let judging = BTreeSet::from(["fail1".to_string()]);
        let rows = rule_rows(&rules(), &judging);
        let row = rows.iter().find(|r| r.rule_id == "fail1").unwrap();
        assert_eq!(row.mark, Mark::Judging);
        assert_eq!(row.mark.badge().1, None);
        // It keeps its place among the failing rules.
        assert_eq!(rows[1].rule_id, "fail1");
    }

    #[test]
    fn only_current_passes_count() {
        assert_eq!(passing(&rules()), (1, 6));
    }
}
