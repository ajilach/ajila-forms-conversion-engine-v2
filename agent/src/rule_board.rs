//! Where every rule stands on the run's document, for a person watching the
//! run: the latest verdict each rule got, and the revision it got it on.
//!
//! The board only records what the existing checks found (the lint after
//! every edit, `rule_check`'s scripts and judges); it never checks anything
//! itself. A scripted rule is re-checked on every edit, so its verdict is
//! always current once it has one; a judged rule is only checked when a
//! `rule_check` dispatches its judge, so its verdict goes out of date with the
//! next edit, and [`RuleBoard::snapshot`] says so.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rules::{JudgedRule, RuleVerdict};

/// Which kind of check decides a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Script,
    Judge,
}

/// A rule's latest verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuleState {
    /// No check has reached the rule yet.
    NotChecked,
    Pass,
    Fail { violations: usize },
    /// A check ran but gave no verdict: the script broke or lacked its facts,
    /// or the judge failed.
    Unchecked { reason: String },
}

/// One rule on the board, as a person watching the run sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleView {
    pub rule_id: String,
    pub title: String,
    pub kind: RuleKind,
    #[serde(flatten)]
    pub state: RuleState,
    /// The document changed after the verdict was given.
    pub outdated: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    rule_id: String,
    title: String,
    kind: RuleKind,
    state: RuleState,
    /// The document revision the verdict describes.
    checked_at: Option<u64>,
}

/// Every rule of the run's target, in rule order: the scripted ones, then the
/// judged ones.
#[derive(Debug, Clone, Default)]
pub struct RuleBoard {
    entries: Vec<Entry>,
}

impl RuleBoard {
    /// A board on which no rule is checked yet.
    pub fn new<'a>(
        scripted: impl IntoIterator<Item = (String, String)>,
        judged: impl IntoIterator<Item = &'a JudgedRule>,
    ) -> Self {
        let entry = |rule_id, title, kind| Entry { rule_id, title, kind, state: RuleState::NotChecked, checked_at: None };
        let mut entries: Vec<Entry> = scripted
            .into_iter()
            .map(|(id, title)| entry(id, title, RuleKind::Script))
            .collect();
        entries.extend(judged.into_iter().map(|r| entry(r.id.clone(), r.title.clone(), RuleKind::Judge)));
        Self { entries }
    }

    /// Records the scripts' verdicts (`check_rules`' `verdicts` entries) on
    /// `revision`. A verdict for a rule the board does not hold is a broken
    /// invariant: the board is built from the same rules the scripts run.
    pub fn record_scripted(&mut self, verdicts: &[Value], revision: u64) {
        for verdict in verdicts {
            let id = verdict["rule_id"].as_str().unwrap_or_default();
            let state = match verdict["verdict"].as_str() {
                Some("positive") => RuleState::Pass,
                Some("negative") => RuleState::Fail {
                    violations: verdict["violations"].as_array().map_or(0, Vec::len),
                },
                Some("broken") => RuleState::Unchecked { reason: reason(verdict, "broken_reason", "the rule's script broke") },
                Some("indeterminate") => RuleState::Unchecked {
                    reason: reason(verdict, "indeterminate_reason", "the rule's facts are missing"),
                },
                other => RuleState::Unchecked { reason: format!("unknown verdict {other:?}") },
            };
            self.set(id, state, revision);
        }
    }

    /// Records the judges' outcomes of one `rule_check` on `revision`, the
    /// revision the judges were dispatched on.
    pub fn record_judged(&mut self, outcomes: &[(JudgedRule, Result<RuleVerdict, String>)], revision: u64) {
        for (rule, outcome) in outcomes {
            let state = match outcome {
                Ok(v) if v.pass => RuleState::Pass,
                Ok(v) => RuleState::Fail { violations: v.violations.len() },
                Err(reason) => RuleState::Unchecked { reason: reason.clone() },
            };
            self.set(&rule.id, state, revision);
        }
    }

    fn set(&mut self, rule_id: &str, state: RuleState, revision: u64) {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.rule_id == rule_id)
            .unwrap_or_else(|| panic!("a verdict for rule {rule_id}, which the board does not hold"));
        entry.state = state;
        entry.checked_at = Some(revision);
    }

    /// Whether every scripted rule has a verdict on `revision`.
    pub fn scripted_current(&self, revision: u64) -> bool {
        self.entries
            .iter()
            .filter(|e| e.kind == RuleKind::Script)
            .all(|e| e.checked_at == Some(revision))
    }

    /// The board as it reads on `current_revision`.
    pub fn snapshot(&self, current_revision: u64) -> Vec<RuleView> {
        self.entries
            .iter()
            .map(|e| RuleView {
                rule_id: e.rule_id.clone(),
                title: e.title.clone(),
                kind: e.kind,
                state: e.state.clone(),
                outdated: e.checked_at.is_some_and(|at| at < current_revision),
            })
            .collect()
    }
}

fn reason(verdict: &Value, field: &str, fallback: &str) -> String {
    verdict[field].as_str().unwrap_or(fallback).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Violation;
    use serde_json::json;

    fn judged(id: &str) -> JudgedRule {
        JudgedRule { id: id.into(), name: id.into(), title: format!("title {id}"), description: String::new() }
    }

    fn board() -> RuleBoard {
        RuleBoard::new([("s1".to_string(), "S1".to_string()), ("s2".to_string(), "S2".to_string())], &[judged("j1")])
    }

    fn state(board: &RuleBoard, rev: u64, id: &str) -> (RuleState, bool) {
        let view = board.snapshot(rev).into_iter().find(|v| v.rule_id == id).unwrap();
        (view.state, view.outdated)
    }

    #[test]
    fn a_new_board_has_every_rule_unchecked_in_order() {
        let views = board().snapshot(0);
        let ids: Vec<_> = views.iter().map(|v| (v.rule_id.as_str(), v.kind)).collect();
        assert_eq!(ids, [("s1", RuleKind::Script), ("s2", RuleKind::Script), ("j1", RuleKind::Judge)]);
        assert!(views.iter().all(|v| v.state == RuleState::NotChecked && !v.outdated));
    }

    #[test]
    fn scripted_verdicts_map_to_states() {
        let mut b = RuleBoard::new(
            ["p", "n", "b", "i"].map(|id| (id.to_string(), id.to_string())),
            std::iter::empty(),
        );
        b.record_scripted(
            &[
                json!({"rule_id": "p", "verdict": "positive", "violations": []}),
                json!({"rule_id": "n", "verdict": "negative", "violations": [{}, {}]}),
                json!({"rule_id": "b", "verdict": "broken", "broken_reason": "threw"}),
                json!({"rule_id": "i", "verdict": "indeterminate", "indeterminate_reason": "no facts"}),
            ],
            3,
        );
        assert_eq!(state(&b, 3, "p").0, RuleState::Pass);
        assert_eq!(state(&b, 3, "n").0, RuleState::Fail { violations: 2 });
        assert_eq!(state(&b, 3, "b").0, RuleState::Unchecked { reason: "threw".into() });
        assert_eq!(state(&b, 3, "i").0, RuleState::Unchecked { reason: "no facts".into() });
        assert!(b.scripted_current(3));
        assert!(!b.scripted_current(4));
    }

    #[test]
    fn judged_outcomes_map_to_states() {
        let mut b = board();
        let fail = RuleVerdict { pass: false, violations: vec![Violation { pointer: "/a".into(), message: "m".into() }] };
        b.record_judged(&[(judged("j1"), Ok(fail))], 2);
        assert_eq!(state(&b, 2, "j1").0, RuleState::Fail { violations: 1 });
        b.record_judged(&[(judged("j1"), Err("gave up".into()))], 2);
        assert_eq!(state(&b, 2, "j1").0, RuleState::Unchecked { reason: "gave up".into() });
        b.record_judged(&[(judged("j1"), Ok(RuleVerdict { pass: true, violations: vec![] }))], 2);
        assert_eq!(state(&b, 2, "j1"), (RuleState::Pass, false));
    }

    #[test]
    fn a_later_edit_dates_a_judged_verdict_but_not_a_rechecked_script() {
        let mut b = board();
        b.record_scripted(&[json!({"rule_id": "s1", "verdict": "positive"})], 1);
        b.record_judged(&[(judged("j1"), Ok(RuleVerdict { pass: true, violations: vec![] }))], 1);
        // The edit to revision 2 re-checks the scripts, not the judge.
        b.record_scripted(&[json!({"rule_id": "s1", "verdict": "negative", "violations": [{}]})], 2);
        assert_eq!(state(&b, 2, "s1"), (RuleState::Fail { violations: 1 }, false));
        assert_eq!(state(&b, 2, "j1"), (RuleState::Pass, true));
        // A rule never checked is not outdated, only not checked.
        assert_eq!(state(&b, 2, "s2"), (RuleState::NotChecked, false));
    }

    #[test]
    fn a_view_round_trips_through_json() {
        let view = RuleView {
            rule_id: "r".into(),
            title: "t".into(),
            kind: RuleKind::Judge,
            state: RuleState::Fail { violations: 2 },
            outdated: true,
        };
        let back: RuleView = serde_json::from_value(serde_json::to_value(&view).unwrap()).unwrap();
        assert_eq!(back, view);
    }
}
