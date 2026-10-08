//! The wire between the host and a worker process.
//!
//! Newline-delimited JSON, one request per line and one response per line.
//! Framing on `\n` rather than a length prefix is deliberate: a worker that
//! dies mid-write leaves a partial line, and a partial line never parses, so
//! a truncated answer can never be mistaken for a real verdict. That
//! property is load-bearing here, because "the worker died" is an expected
//! outcome rather than an exceptional one -- it is what a script hitting the
//! memory cap looks like.
//!
//! `u2s-rules`' own types stay off the wire. [`u2s_rules::ScriptBudget`] has
//! no serde derives and should not grow them for a transport's benefit, and
//! [`u2s_rules::BrokenRule`]/[`u2s_rules::BrokenFix`]'s variants never
//! cross: nothing outside that crate matches on them (their only consumers
//! stringify), so a reason travels as text and the typed errors stay
//! worker-side.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use u2s_rules::{CheckVerdict, CheckedOutcome, FixOutcome, ScriptBudget};

/// One script against one document, with an optional fix to verify if the
/// check fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRequest {
    pub script_js: String,
    /// The rule's `fix_js`, if it has one. `None` is the common case and
    /// reproduces today's check-only behaviour exactly -- see
    /// [`CheckRequest::eval_multiplier`] for why its presence changes how
    /// long this job is allowed to take.
    #[serde(default)]
    pub fix_js: Option<String>,
    pub output: Value,
    pub schema: Value,
    /// `ctx.facts`: the rule's declared facts, resolved for the input this
    /// output came from. Empty for an intrinsic rule, which is also what an
    /// older host that never sends the field means.
    #[serde(default)]
    pub facts: Map<String, Value>,
    pub budget: WireBudget,
}

impl CheckRequest {
    /// The one constructor every host-side caller uses, so adding a field
    /// here is one change rather than one per call site.
    pub fn new(
        script_js: String,
        fix_js: Option<String>,
        output: Value,
        schema: Value,
        facts: Map<String, Value>,
        budget: &ScriptBudget,
    ) -> Self {
        Self {
            script_js,
            fix_js,
            output,
            schema,
            facts,
            budget: budget.into(),
        }
    }

    /// How many script evaluations this job may need: one for a check
    /// alone, or up to three once a fix is attached -- the check, the fix
    /// script itself, and the re-check that [`u2s_rules::classify_check_and_fix`]
    /// runs to verify it (`crate::fix::verify_fix` inside `u2s-rules`).
    ///
    /// Both the host's deadline ([`crate::runner`]) and the worker's own
    /// CPU ceiling (`u2s-rules-worker`) derive their multiplier from this
    /// one place. If they derived it separately, an honest check-fix-recheck
    /// run near its wall-clock budget could pass the host's deadline check
    /// and still be killed by the worker's tighter CPU limit -- the failure
    /// would look exactly like a runaway script, silently, on every rule
    /// that ever attaches a fix.
    pub fn eval_multiplier(&self) -> u32 {
        if self.fix_js.is_some() { 3 } else { 1 }
    }
}

/// [`ScriptBudget`] as four scalars. `wall_clock` becomes milliseconds
/// because `Duration`'s serde representation is a struct, and a budget that
/// crosses a process boundary should be readable in a log line.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WireBudget {
    pub loop_iterations: u64,
    pub recursion_limit: usize,
    pub stack_size_bytes: usize,
    pub wall_clock_ms: u64,
}

impl From<&ScriptBudget> for WireBudget {
    fn from(budget: &ScriptBudget) -> Self {
        Self {
            loop_iterations: budget.loop_iterations,
            recursion_limit: budget.recursion_limit,
            stack_size_bytes: budget.stack_size_bytes,
            wall_clock_ms: budget.wall_clock.as_millis().min(u128::from(u64::MAX)) as u64,
        }
    }
}

impl From<WireBudget> for ScriptBudget {
    fn from(wire: WireBudget) -> Self {
        Self {
            loop_iterations: wire.loop_iterations,
            recursion_limit: wire.recursion_limit,
            stack_size_bytes: wire.stack_size_bytes,
            wall_clock: Duration::from_millis(wire.wall_clock_ms),
        }
    }
}

/// One line from the host to a worker. A check is by far the common job;
/// the other two exist because a script's top-level `requires` and an
/// `ingest_script` fact's `extract(ingest)` are generated code too, and
/// must run under the same isolation as a check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "job", rename_all = "snake_case")]
pub enum WorkerRequest {
    Check(CheckRequest),
    Requires {
        script_js: String,
        budget: WireBudget,
    },
    Extract {
        script_js: String,
        ingest: Value,
        budget: WireBudget,
    },
}

impl WorkerRequest {
    pub fn budget(&self) -> ScriptBudget {
        match self {
            Self::Check(request) => request.budget.into(),
            Self::Requires { budget, .. } | Self::Extract { budget, .. } => (*budget).into(),
        }
    }

    /// How many script evaluations this job may need -- see
    /// [`CheckRequest::eval_multiplier`]. Declarations and extractions are
    /// one evaluation each.
    pub fn eval_multiplier(&self) -> u32 {
        match self {
            Self::Check(request) => request.eval_multiplier(),
            Self::Requires { .. } | Self::Extract { .. } => 1,
        }
    }
}

/// One line from a worker back to the host, always of the same kind as the
/// request it answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "job", rename_all = "snake_case")]
pub enum WorkerResponse {
    Check(CheckResponse),
    Requires { result: Result<Vec<String>, String> },
    Extract { result: Result<Value, String> },
}

impl WorkerResponse {
    /// An answer the host produces itself when the worker could not: it
    /// died, ran past its deadline, or could not be started. Shaped after
    /// `request` so the caller always receives the kind it asked for.
    pub fn failed(request: &WorkerRequest, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        match request {
            WorkerRequest::Check(_) => Self::Check(CheckResponse::broken(reason)),
            WorkerRequest::Requires { .. } => Self::Requires {
                result: Err(reason),
            },
            WorkerRequest::Extract { .. } => Self::Extract {
                result: Err(reason),
            },
        }
    }
}

/// A fix's outcome, crossing the wire. Mirrors [`FixOutcome`] exactly --
/// both already carry a stringified reason rather than the typed
/// `BrokenFix`, so nothing is lost in the crossing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum WireFix {
    /// Verified RFC 6902 operations, as plain JSON.
    Ops(Value),
    /// Why the fix could not be trusted, as text.
    Broken(String),
}

impl From<FixOutcome> for WireFix {
    fn from(fix: FixOutcome) -> Self {
        match fix {
            FixOutcome::Ops(ops) => WireFix::Ops(ops),
            FixOutcome::Broken(reason) => WireFix::Broken(reason),
        }
    }
}

impl From<WireFix> for FixOutcome {
    fn from(wire: WireFix) -> Self {
        match wire {
            WireFix::Ops(ops) => FixOutcome::Ops(ops),
            WireFix::Broken(reason) => FixOutcome::Broken(reason),
        }
    }
}

/// What the worker answers with. Shaped as [`CheckedOutcome`] rather than a
/// `Result`, for the same reason that type exists: an overrunning or
/// throwing script is a verdict about the *rule*, not an error about the
/// run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResponse {
    pub verdict: WireVerdict,
    pub violations: Value,
    pub broken_reason: Option<String>,
    /// Set exactly when [`CheckedOutcome::fix`] is -- `Negative` with a
    /// `fix_js` given. `None` for every other verdict and for a rule with
    /// no fix script.
    #[serde(default)]
    pub fix: Option<WireFix>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireVerdict {
    Positive,
    Negative,
    Broken,
}

impl CheckResponse {
    /// A verdict the host produced itself, without the worker answering --
    /// the worker died, or ran past its deadline. Kept here beside the wire
    /// type so every "broken" reason a caller can observe has one shape.
    pub fn broken(reason: impl Into<String>) -> Self {
        Self {
            verdict: WireVerdict::Broken,
            violations: Value::Array(Vec::new()),
            broken_reason: Some(reason.into()),
            fix: None,
        }
    }
}

impl From<CheckedOutcome> for CheckResponse {
    fn from(outcome: CheckedOutcome) -> Self {
        Self {
            verdict: match outcome.verdict {
                CheckVerdict::Positive => WireVerdict::Positive,
                CheckVerdict::Negative => WireVerdict::Negative,
                CheckVerdict::Broken => WireVerdict::Broken,
            },
            violations: outcome.violations,
            broken_reason: outcome.broken_reason,
            fix: outcome.fix.map(WireFix::from),
        }
    }
}

impl From<CheckResponse> for CheckedOutcome {
    fn from(response: CheckResponse) -> Self {
        Self {
            verdict: match response.verdict {
                WireVerdict::Positive => CheckVerdict::Positive,
                WireVerdict::Negative => CheckVerdict::Negative,
                WireVerdict::Broken => CheckVerdict::Broken,
            },
            violations: response.violations,
            broken_reason: response.broken_reason,
            fix: response.fix.map(FixOutcome::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_budget_survives_the_round_trip() {
        let budget = ScriptBudget::default();
        let back: ScriptBudget = WireBudget::from(&budget).into();
        assert_eq!(back, budget);
    }

    #[test]
    fn an_outcome_survives_the_round_trip() {
        let outcome = CheckedOutcome {
            verdict: CheckVerdict::Negative,
            violations: json!([{ "pointer": "/a", "message": "bad" }]),
            broken_reason: None,
            fix: None,
        };
        let back: CheckedOutcome = CheckResponse::from(outcome.clone()).into();
        assert_eq!(back, outcome);
    }

    #[test]
    fn a_fix_outcome_survives_the_round_trip_both_ways() {
        for fix in [
            FixOutcome::Ops(json!([{ "op": "remove", "path": "/a" }])),
            FixOutcome::Broken("did not fix it".to_owned()),
        ] {
            let outcome = CheckedOutcome {
                verdict: CheckVerdict::Negative,
                violations: json!([{ "pointer": "/a", "message": "bad" }]),
                broken_reason: None,
                fix: Some(fix.clone()),
            };
            let back: CheckedOutcome = CheckResponse::from(outcome.clone()).into();
            assert_eq!(back, outcome, "fix outcome {fix:?} did not round-trip");
        }
    }

    /// The framing property the whole protocol leans on: a line the worker
    /// only half-wrote must fail to parse rather than deserialize into
    /// something plausible.
    #[test]
    fn a_truncated_response_line_does_not_parse() {
        let whole = serde_json::to_string(&CheckResponse::from(CheckedOutcome {
            verdict: CheckVerdict::Positive,
            violations: json!([]),
            broken_reason: None,
            fix: None,
        }))
        .expect("serialize");
        let truncated = &whole[..whole.len() - 3];
        assert!(serde_json::from_str::<CheckResponse>(truncated).is_err());
    }

    #[test]
    fn the_verdict_tags_are_the_stable_lowercase_names() {
        let json = serde_json::to_string(&WireVerdict::Broken).expect("serialize");
        assert_eq!(json, r#""broken""#);
    }

    /// A request with no `fix_js` uses a plain single-evaluation budget --
    /// this is what keeps a lint pass (which never sends `fix_js`) as cheap
    /// as `rule_check` was before fixes existed.
    #[test]
    fn a_request_with_no_fix_has_a_multiplier_of_one() {
        let request = CheckRequest {
            script_js: "function check(o,c){return{pass:true,violations:[]}}".to_owned(),
            fix_js: None,
            output: json!({}),
            schema: json!({}),
            facts: Map::new(),
            budget: WireBudget::from(&ScriptBudget::default()),
        };
        assert_eq!(request.eval_multiplier(), 1);
    }

    #[test]
    fn a_request_with_a_fix_has_a_wider_multiplier() {
        let request = CheckRequest {
            script_js: "function check(o,c){return{pass:false,violations:[]}}".to_owned(),
            fix_js: Some("function fix(o,c){return []}".to_owned()),
            output: json!({}),
            schema: json!({}),
            facts: Map::new(),
            budget: WireBudget::from(&ScriptBudget::default()),
        };
        assert!(
            request.eval_multiplier() > 1,
            "a fixed job may run check, fix, and a re-check"
        );
    }
}
