//! Ties the sandbox, the host prelude, and the contract together: this is
//! the one function the rest of the workspace calls.

use std::time::Instant;

use boa_engine::{JsValue, Source};
use serde_json::{Map, Value};

use crate::budget::ScriptBudget;
use crate::contract::{self, BrokenRule, CheckOutcome, RawCheckOutcome};
use crate::sandbox;

const PRELUDE: &str = include_str!("prelude.js");

const OUTPUT_GLOBAL: &str = "__U2S_OUTPUT_JSON__";
const SCHEMA_GLOBAL: &str = "__U2S_SCHEMA_JSON__";
/// Shared with `fix.rs`, whose driver builds the same `ctx.facts`.
pub(crate) const FACTS_GLOBAL: &str = "__U2S_FACTS_JSON__";

/// Runs a generated script's `check(output, ctx)` against one output
/// document, in a fresh sandbox built for exactly this evaluation.
///
/// A malformed, throwing, or budget-exceeding script never panics and never
/// reaches the caller as a Rust error about *this run* — it comes back as
/// [`BrokenRule`], because a bad rule is the Rule Agent's problem, not a
/// reason to fail whatever conversion happens to be checked against it.
///
/// `facts` becomes `ctx.facts`: the rule's declared facts, resolved for the
/// input this output was converted from, and nothing else (an empty map for
/// an intrinsic rule). The host decides beforehand whether the check can run
/// at all; a missing fact never reaches this function.
pub fn run_check(
    script_js: &str,
    output: &Value,
    schema: &Value,
    facts: &Map<String, Value>,
    budget: &ScriptBudget,
) -> Result<CheckOutcome, BrokenRule> {
    let started = Instant::now();

    let mut context = sandbox::build(budget);

    let output_json = serde_json::to_string(output).expect("serde_json::Value always serializes");
    let schema_json = serde_json::to_string(schema).expect("serde_json::Value always serializes");
    sandbox::define_global_json(&mut context, OUTPUT_GLOBAL, &output_json);
    sandbox::define_global_json(&mut context, SCHEMA_GLOBAL, &schema_json);
    let facts_json = serde_json::to_string(facts).expect("serde_json::Map always serializes");
    sandbox::define_global_json(&mut context, FACTS_GLOBAL, &facts_json);

    context
        .eval(Source::from_bytes(PRELUDE))
        .expect("the built-in prelude is trusted code and must always evaluate");

    context
        .eval(Source::from_bytes(script_js))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?;

    let is_function = context
        .eval(Source::from_bytes("typeof check"))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?
        .as_string()
        .map(|s| s.to_std_string_escaped())
        == Some("function".to_owned());
    if !is_function {
        return Err(BrokenRule::NoCheckFunction);
    }

    let driver = format!(
        "(function () {{ \
           var __output = JSON.parse(globalThis.{OUTPUT_GLOBAL}); \
           var __ctx = {{ \
             schema: JSON.parse(globalThis.{SCHEMA_GLOBAL}), \
             walk: walk, \
             facts: __u2s_factsProxy(JSON.parse(globalThis.{FACTS_GLOBAL})) \
           }}; \
           var __result = check(__output, __ctx); \
           return JSON.stringify(__result); \
         }})()"
    );
    let result: JsValue = context
        .eval(Source::from_bytes(&driver))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?;

    let result_text = result
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| {
            BrokenRule::MalformedResult(
                "check() must return a JSON-serializable object; got no value".to_owned(),
            )
        })?;

    let raw: RawCheckOutcome = serde_json::from_str(&result_text)
        .map_err(|err| BrokenRule::MalformedResult(err.to_string()))?;

    let outcome = contract::validate_outcome(raw, output)?;

    let elapsed = started.elapsed();
    if elapsed > budget.wall_clock {
        return Err(BrokenRule::Timeout(elapsed));
    }

    Ok(outcome)
}

/// A verdict shape this crate owns, independent of any store schema --
/// `u2s-rules` has no `u2s-store` dependency (a pure, boa-based crate, by
/// design), so it names its own three states rather than reusing the
/// database's `rule_references.verdict` enum. A caller with that enum maps
/// onto this one-to-one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckVerdict {
    Positive,
    Negative,
    Broken,
}

/// What a rule's optional `fix_js` did, once it has been proven or refused
/// against the check it claims to repair -- see [`crate::fix::verify_fix`].
/// Never constructed for a `Positive` or `Broken` check: see
/// [`classify_check_and_fix`].
///
/// `Broken` carries a string, not the typed [`crate::contract::BrokenFix`] -- the same
/// choice [`CheckedOutcome::broken_reason`] already makes for a broken
/// check, and for the same reason: this type crosses into
/// `u2s-rules-host`'s wire protocol, where only this crate's own tests ever
/// match on the typed error, never a caller outside it.
#[derive(Debug, Clone, PartialEq)]
pub enum FixOutcome {
    /// Verified RFC 6902 operations, as plain JSON -- not yet applied to
    /// anything. Applying them for real is the caller's decision.
    Ops(Value),
    /// The fix script itself, never the check -- see [`crate::contract::BrokenFix`]'s
    /// `Display` for what this reads.
    Broken(String),
}

/// What one script said about one output, as a total value rather than a
/// `Result` -- every caller (the deterministic pass, a native tool the
/// Conversion Agent calls mid-round) wants the same three fields whether
/// or not the script threw, which is exactly [`run_check`]'s `Result`
/// flattened one level.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedOutcome {
    pub verdict: CheckVerdict,
    /// The rule's own violations, already JSON. Empty for `Positive` and
    /// `Broken`.
    pub violations: Value,
    /// Set exactly when the script could not be trusted to answer, never
    /// when it answered `Negative`.
    pub broken_reason: Option<String>,
    /// The rule's `fix_js`, evaluated and verified -- only ever `Some` when
    /// `verdict` is `Negative` and a fix script was given. `None` for
    /// `Positive` (nothing to repair), `Broken` (no trustworthy violations
    /// to repair against), and a rule with no `fix_js` at all.
    pub fix: Option<FixOutcome>,
}

/// [`run_check`], classified into a total [`CheckedOutcome`], and — when the
/// check fails and `fix_js` is given — that fix verified against it. Pure
/// and synchronous, and total: an overrunning or throwing script becomes
/// `Broken` rather than reaching the caller as a Rust error, since a bad
/// rule is the Rule Agent's problem, never a reason to fail whatever it is
/// checked against (PLAN.md verification 8). `fix_js: None` reproduces the
/// crate's pre-fix behaviour exactly -- this is the one entry point rather
/// than a check-only function sitting beside a check-and-fix one, so a
/// caller never has to choose between two ways into the sandbox.
pub fn classify_check_and_fix(
    script_js: &str,
    fix_js: Option<&str>,
    output: &Value,
    schema: &Value,
    facts: &Map<String, Value>,
    budget: &ScriptBudget,
) -> CheckedOutcome {
    match run_check(script_js, output, schema, facts, budget) {
        Ok(outcome) if outcome.pass => CheckedOutcome {
            verdict: CheckVerdict::Positive,
            violations: serde_json::to_value(&outcome.violations)
                .unwrap_or_else(|_| serde_json::json!([])),
            broken_reason: None,
            fix: None,
        },
        Ok(outcome) => {
            let fix = fix_js.map(|fix_js| {
                match crate::fix::verify_fix(
                    script_js,
                    fix_js,
                    output,
                    schema,
                    facts,
                    &outcome.violations,
                    budget,
                ) {
                    Ok(ops) => FixOutcome::Ops(ops),
                    Err(broken) => FixOutcome::Broken(broken.to_string()),
                }
            });
            CheckedOutcome {
                verdict: CheckVerdict::Negative,
                violations: serde_json::to_value(&outcome.violations)
                    .unwrap_or_else(|_| serde_json::json!([])),
                broken_reason: None,
                fix,
            }
        }
        Err(broken) => CheckedOutcome {
            verdict: CheckVerdict::Broken,
            // Empty, not the violations of a check that never completed --
            // a broken script produced no findings to report.
            violations: serde_json::json!([]),
            broken_reason: Some(broken.to_string()),
            fix: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn budget() -> ScriptBudget {
        ScriptBudget::default()
    }

    #[test]
    fn a_passing_script_reports_no_violations() {
        let script = "function check(output, ctx) { return { pass: true, violations: [] }; }";
        let outcome = run_check(
            script,
            &json!({ "a": 1 }),
            &json!({}),
            &Map::new(),
            &budget(),
        )
        .unwrap();
        assert!(outcome.pass);
        assert!(outcome.violations.is_empty());
    }

    #[test]
    fn a_failing_script_reports_its_violation() {
        let script = r#"
            function check(output, ctx) {
                return { pass: false, violations: [{ pointer: "/a", message: "bad" }] };
            }
        "#;
        let outcome = run_check(
            script,
            &json!({ "a": 1 }),
            &json!({}),
            &Map::new(),
            &budget(),
        )
        .unwrap();
        assert!(!outcome.pass);
        assert_eq!(outcome.violations[0].pointer, "/a");
    }

    #[test]
    fn walk_visits_every_node_with_correctly_escaped_pointers() {
        let script = r#"
            function check(output, ctx) {
                const seen = [];
                ctx.walk(output, (node, pointer) => seen.push(pointer));
                const violations = seen
                    .filter(p => p !== "")
                    .map(p => ({ pointer: p, message: "seen" }));
                return { pass: violations.length === 0, violations };
            }
        "#;
        // A key containing both "~" and "/" exercises RFC 6901 escaping.
        let output = json!({ "a~b/c": 1, "list": [1, 2] });
        let outcome = run_check(script, &output, &json!({}), &Map::new(), &budget()).unwrap();
        let pointers: Vec<&str> = outcome
            .violations
            .iter()
            .map(|v| v.pointer.as_str())
            .collect();
        assert!(pointers.contains(&"/a~0b~1c"));
        assert!(pointers.contains(&"/list"));
        assert!(pointers.contains(&"/list/0"));
        assert!(pointers.contains(&"/list/1"));
    }

    #[test]
    fn a_throwing_script_is_a_broken_rule() {
        let script = "function check(output, ctx) { throw new Error('boom'); }";
        let err = run_check(script, &json!({}), &json!({}), &Map::new(), &budget()).unwrap_err();
        assert!(matches!(err, BrokenRule::Threw(_)));
    }

    #[test]
    fn a_missing_check_function_is_a_broken_rule() {
        let err = run_check(
            "const x = 1;",
            &json!({}),
            &json!({}),
            &Map::new(),
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenRule::NoCheckFunction));
    }

    #[test]
    fn a_non_function_check_binding_is_a_broken_rule() {
        let err = run_check(
            "const check = 42;",
            &json!({}),
            &json!({}),
            &Map::new(),
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenRule::NoCheckFunction));
    }

    #[test]
    fn a_malformed_return_shape_is_a_broken_rule() {
        let script = "function check(output, ctx) { return 42; }";
        let err = run_check(script, &json!({}), &json!({}), &Map::new(), &budget()).unwrap_err();
        assert!(matches!(err, BrokenRule::MalformedResult(_)));
    }

    #[test]
    fn a_script_returning_nothing_is_a_broken_rule() {
        let script = "function check(output, ctx) {}";
        let err = run_check(script, &json!({}), &json!({}), &Map::new(), &budget()).unwrap_err();
        assert!(matches!(err, BrokenRule::MalformedResult(_)));
    }

    #[test]
    fn an_unresolvable_pointer_is_a_broken_rule() {
        let script = r#"
            function check(output, ctx) {
                return { pass: false, violations: [{ pointer: "/nowhere", message: "x" }] };
            }
        "#;
        let err = run_check(
            script,
            &json!({ "a": 1 }),
            &json!({}),
            &Map::new(),
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenRule::UnresolvablePointer { .. }));
    }

    #[test]
    fn an_infinite_loop_inside_check_is_a_broken_rule_not_a_hang() {
        let b = ScriptBudget {
            loop_iterations: 1000,
            ..budget()
        };
        let script = "function check(output, ctx) { while (true) {} }";
        let err = run_check(script, &json!({}), &json!({}), &Map::new(), &b).unwrap_err();
        assert!(matches!(err, BrokenRule::Threw(_)));
    }

    #[test]
    fn the_schema_is_visible_on_ctx() {
        let script = r#"
            function check(output, ctx) {
                const ok = ctx.schema.title === "Synthetic";
                return { pass: ok, violations: ok ? [] : [{ pointer: "", message: "schema missing" }] };
            }
        "#;
        let outcome = run_check(
            script,
            &json!({}),
            &json!({ "title": "Synthetic" }),
            &Map::new(),
            &budget(),
        )
        .unwrap();
        assert!(outcome.pass);
    }

    #[test]
    fn classify_check_reports_positive_and_negative_without_a_broken_reason() {
        let passing = classify_check_and_fix(
            "function check(output, ctx) { return { pass: true, violations: [] }; }",
            None,
            &json!({}),
            &json!({}),
            &Map::new(),
            &budget(),
        );
        assert_eq!(passing.verdict, CheckVerdict::Positive);
        assert!(passing.broken_reason.is_none());
        assert!(passing.fix.is_none(), "nothing to repair on a pass");

        let failing = classify_check_and_fix(
            r#"function check(output, ctx) {
                return { pass: false, violations: [{ pointer: "/a", message: "bad" }] };
            }"#,
            None,
            &json!({ "a": 1 }),
            &json!({}),
            &Map::new(),
            &budget(),
        );
        assert_eq!(failing.verdict, CheckVerdict::Negative);
        assert_eq!(failing.violations[0]["pointer"], "/a");
        assert!(failing.broken_reason.is_none());
        assert!(failing.fix.is_none(), "no fix_js was given");
    }

    #[test]
    fn classify_check_turns_a_throwing_script_into_broken_with_no_violations() {
        let broken = classify_check_and_fix(
            "function check(output, ctx) { throw new Error('boom'); }",
            None,
            &json!({}),
            &json!({}),
            &Map::new(),
            &budget(),
        );
        assert_eq!(broken.verdict, CheckVerdict::Broken);
        assert!(broken.violations.as_array().is_some_and(|v| v.is_empty()));
        assert!(broken.broken_reason.is_some());
        assert!(
            broken.fix.is_none(),
            "no trustworthy violations to repair against"
        );
    }

    #[test]
    fn classify_check_and_fix_verifies_a_working_fix() {
        let outcome = classify_check_and_fix(
            r#"function check(output, ctx) {
                if (output.title) {
                    return { pass: false, violations: [{ pointer: "/title", message: "bad" }] };
                }
                return { pass: true, violations: [] };
            }"#,
            Some(
                r#"function fix(output, ctx) {
                return ctx.violations.map(v => ({ op: "remove", path: v.pointer }));
            }"#,
            ),
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &budget(),
        );
        assert_eq!(outcome.verdict, CheckVerdict::Negative);
        match outcome.fix {
            Some(FixOutcome::Ops(ops)) => {
                assert_eq!(ops, json!([{ "op": "remove", "path": "/title" }]));
            }
            other => panic!("expected a verified fix, got {other:?}"),
        }
    }

    #[test]
    fn classify_check_and_fix_reports_a_broken_fix_without_disturbing_the_verdict() {
        let outcome = classify_check_and_fix(
            r#"function check(output, ctx) {
                if (output.title) {
                    return { pass: false, violations: [{ pointer: "/title", message: "bad" }] };
                }
                return { pass: true, violations: [] };
            }"#,
            Some("function fix(output, ctx) { return []; }"),
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &budget(),
        );
        assert_eq!(
            outcome.verdict,
            CheckVerdict::Negative,
            "a broken fix must not change the check's own verdict"
        );
        assert_eq!(outcome.violations[0]["pointer"], "/title");
        match outcome.fix {
            Some(FixOutcome::Broken(reason)) => {
                assert!(
                    reason.contains("no operations"),
                    "expected the NoOps reason, got {reason:?}"
                );
            }
            other => panic!("expected a broken fix (NoOps), got {other:?}"),
        }
    }

    fn facts(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn a_declared_fact_is_readable_on_ctx() {
        let script = r#"
            const requires = ["source_fields"];
            function check(output, ctx) {
                const missing = ctx.facts.source_fields.filter(f => !(f in output));
                return {
                    pass: missing.length === 0,
                    violations: missing.map(f => ({ pointer: "", message: "missing " + f })),
                };
            }
        "#;
        let outcome = run_check(
            script,
            &json!({ "name": 1 }),
            &json!({}),
            &facts(&[("source_fields", json!(["name", "iban"]))]),
            &budget(),
        )
        .unwrap();
        assert!(!outcome.pass);
        assert_eq!(outcome.violations[0].message, "missing iban");
    }

    #[test]
    fn reading_an_undeclared_fact_breaks_the_rule() {
        let script = r#"
            function check(output, ctx) {
                const x = ctx.facts.never_declared;
                return { pass: true, violations: [] };
            }
        "#;
        let err = run_check(script, &json!({}), &json!({}), &Map::new(), &budget()).unwrap_err();
        match err {
            BrokenRule::Threw(message) => {
                assert!(
                    message.contains("undeclared fact \"never_declared\""),
                    "{message}"
                )
            }
            other => panic!("expected Threw, got {other:?}"),
        }
    }

    #[test]
    fn serializing_the_facts_does_not_trip_the_undeclared_key_guard() {
        let script = r#"
            function check(output, ctx) {
                const text = JSON.stringify(ctx.facts);
                return { pass: text === '{"a":1}', violations: text === '{"a":1}' ? [] : [{ pointer: "", message: text }] };
            }
        "#;
        let outcome = run_check(
            script,
            &json!({}),
            &json!({}),
            &facts(&[("a", json!(1))]),
            &budget(),
        )
        .unwrap();
        assert!(outcome.pass, "{:?}", outcome.violations);
    }

    #[test]
    fn a_fix_sees_the_same_facts_as_its_check() {
        let check = r#"
            function check(output, ctx) {
                const missing = ctx.facts.required.filter(k => !(k in output));
                return {
                    pass: missing.length === 0,
                    violations: missing.map(k => ({ pointer: "", message: k })),
                };
            }
        "#;
        let fix = r#"
            function fix(output, ctx) {
                return ctx.facts.required
                    .filter(k => !(k in output))
                    .map(k => ({ op: "add", path: "/" + k, value: null }));
            }
        "#;
        let outcome = classify_check_and_fix(
            check,
            Some(fix),
            &json!({ "a": 1 }),
            &json!({}),
            &facts(&[("required", json!(["a", "b"]))]),
            &budget(),
        );
        assert_eq!(outcome.verdict, CheckVerdict::Negative);
        assert!(
            matches!(outcome.fix, Some(FixOutcome::Ops(_))),
            "{:?}",
            outcome.fix
        );
    }
}
