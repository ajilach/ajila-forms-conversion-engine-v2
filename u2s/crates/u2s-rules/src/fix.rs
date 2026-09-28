//! Ties the sandbox, the host prelude, and the fix contract together —
//! `fix(output, ctx)`'s counterpart to [`crate::check::run_check`].
//!
//! A fix script is only ever invoked after a failing check, and only ever
//! against the exact violations that check reported (`ctx.violations`).
//! Its operations are never trusted on their own say-so: [`verify_fix`]
//! applies them to a clone of the output and re-runs the *same* `check`
//! against that clone. Only a clone that then passes counts as a real fix
//! — see [`crate::contract::BrokenFix::DidNotFix`], which is what this
//! module exists to be able to name.

use std::time::Instant;

use boa_engine::{JsValue, Source};
use serde_json::{Map, Value};

use crate::budget::ScriptBudget;
use crate::check::FACTS_GLOBAL;
use crate::contract::{self, BrokenFix, RuleViolation};
use crate::sandbox;

const PRELUDE: &str = include_str!("prelude.js");

const OUTPUT_GLOBAL: &str = "__U2S_OUTPUT_JSON__";
const SCHEMA_GLOBAL: &str = "__U2S_SCHEMA_JSON__";
const VIOLATIONS_GLOBAL: &str = "__U2S_VIOLATIONS_JSON__";

/// Runs `fix_js`'s `fix(output, ctx)` in a fresh sandbox, built the same
/// way [`crate::check::run_check`]'s is, and returns whatever it returned
/// as a plain [`Value`] — **not yet parsed as ops, not yet applied to
/// anything**. Splitting this out of [`verify_fix`] is what lets a
/// malformed return value (this function's problem) and operations that
/// apply but do not repair (`verify_fix`'s problem) surface as two
/// different [`BrokenFix`] variants rather than one.
fn run_fix_script(
    fix_js: &str,
    output: &Value,
    schema: &Value,
    facts: &Map<String, Value>,
    violations: &[RuleViolation],
    budget: &ScriptBudget,
) -> Result<Value, BrokenFix> {
    let started = Instant::now();

    let mut context = sandbox::build(budget);

    let output_json = serde_json::to_string(output).expect("serde_json::Value always serializes");
    let schema_json = serde_json::to_string(schema).expect("serde_json::Value always serializes");
    let violations_json = serde_json::to_string(violations).expect("violations always serialize");
    sandbox::define_global_json(&mut context, OUTPUT_GLOBAL, &output_json);
    sandbox::define_global_json(&mut context, SCHEMA_GLOBAL, &schema_json);
    sandbox::define_global_json(&mut context, VIOLATIONS_GLOBAL, &violations_json);
    let facts_json = serde_json::to_string(facts).expect("serde_json::Map always serializes");
    sandbox::define_global_json(&mut context, FACTS_GLOBAL, &facts_json);

    context
        .eval(Source::from_bytes(PRELUDE))
        .expect("the built-in prelude is trusted code and must always evaluate");

    context
        .eval(Source::from_bytes(fix_js))
        .map_err(|err| BrokenFix::Threw(err.to_string()))?;

    let is_function = context
        .eval(Source::from_bytes("typeof fix"))
        .map_err(|err| BrokenFix::Threw(err.to_string()))?
        .as_string()
        .map(|s| s.to_std_string_escaped())
        == Some("function".to_owned());
    if !is_function {
        return Err(BrokenFix::NoFixFunction);
    }

    let driver = format!(
        "(function () {{ \
           var __output = JSON.parse(globalThis.{OUTPUT_GLOBAL}); \
           var __ctx = {{ \
             schema: JSON.parse(globalThis.{SCHEMA_GLOBAL}), \
             violations: JSON.parse(globalThis.{VIOLATIONS_GLOBAL}), \
             walk: walk, \
             facts: __u2s_factsProxy(JSON.parse(globalThis.{FACTS_GLOBAL})) \
           }}; \
           var __result = fix(__output, __ctx); \
           return JSON.stringify(__result); \
         }})()"
    );
    let result: JsValue = context
        .eval(Source::from_bytes(&driver))
        .map_err(|err| BrokenFix::Threw(err.to_string()))?;

    let result_text = result
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| {
            BrokenFix::MalformedOps(
                "fix() must return a JSON-serializable value; got no value".to_owned(),
            )
        })?;

    let raw: Value = serde_json::from_str(&result_text)
        .map_err(|err| BrokenFix::MalformedOps(err.to_string()))?;

    // Checked here, not only by the caller: a fix that parses fine but ran
    // long must not go on to be applied and re-checked, which would only
    // spend more of the budget on a script already over it.
    let elapsed = started.elapsed();
    if elapsed > budget.wall_clock {
        return Err(BrokenFix::Timeout(elapsed));
    }

    Ok(raw)
}

/// Runs `fix_js` against `check_js`'s own `violations`, and proves the
/// result actually repairs them before returning it.
///
/// Never called on a passing or already-broken check — see
/// [`crate::check::classify_check_and_fix`], the one caller. `Positive`
/// means there is nothing to repair; `Broken` means there is no
/// trustworthy violation list to repair against, and no working `check` to
/// re-run for verification either.
///
/// On success, returns the fix's own operations as a plain [`Value`] — RFC
/// 6902 ops, unapplied. Applying them for real is the caller's decision to
/// make (see `rule_autofix`, which applies through
/// `u2s_jsondoc::patch_apply` so the revision check and atomicity guarantees
/// stay in one place); this function's job ends at proving the ops work
/// against a disposable clone.
///
/// `facts` is the same `ctx.facts` the check saw, handed to both the fix and
/// the re-check: a fix proven against different facts than the check that
/// asked for it would prove nothing.
pub(crate) fn verify_fix(
    check_js: &str,
    fix_js: &str,
    output: &Value,
    schema: &Value,
    facts: &Map<String, Value>,
    violations: &[RuleViolation],
    budget: &ScriptBudget,
) -> Result<Value, BrokenFix> {
    let raw_ops = run_fix_script(fix_js, output, schema, facts, violations, budget)?;
    let ops = contract::parse_fix_ops(&raw_ops)?;
    let fixed = contract::apply_fix_ops(&ops, output)?;

    match crate::check::run_check(check_js, &fixed, schema, facts, budget) {
        Ok(outcome) if outcome.pass => Ok(raw_ops),
        Ok(outcome) => Err(BrokenFix::DidNotFix {
            remaining: outcome.violations,
        }),
        // The check itself came back broken on the *fixed* document. That
        // is not proof the fix repaired anything, so it does not count as
        // a fix either -- but it is also not the original check's own
        // `BrokenRule`, since the original check answered fine on the
        // original document. Naming it as an unresolved violation keeps
        // the caller from having to handle a third error type here.
        Err(broken) => Err(BrokenFix::DidNotFix {
            remaining: vec![RuleViolation {
                pointer: String::new(),
                message: format!("check() broke on the fixed document: {broken}"),
            }],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn budget() -> ScriptBudget {
        ScriptBudget::default()
    }

    const REMOVE_TITLE_CHECK: &str = r#"
        function check(output, ctx) {
            if (output.title) {
                return { pass: false, violations: [{ pointer: "/title", message: "no title allowed" }] };
            }
            return { pass: true, violations: [] };
        }
    "#;

    #[test]
    fn a_fix_that_repairs_every_violation_is_accepted() {
        let fix_js = r#"
            function fix(output, ctx) {
                return ctx.violations.map(v => ({ op: "remove", path: v.pointer }));
            }
        "#;
        let output = json!({ "title": "nope" });
        let violations = vec![RuleViolation {
            pointer: "/title".to_owned(),
            message: "no title allowed".to_owned(),
        }];
        let ops = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &output,
            &json!({}),
            &Map::new(),
            &violations,
            &budget(),
        )
        .expect("the fix repairs the violation");
        assert_eq!(ops, json!([{ "op": "remove", "path": "/title" }]));
    }

    #[test]
    fn a_fix_that_does_not_repair_the_violation_is_did_not_fix_and_names_what_remains() {
        // Removes the wrong field -- `/title` is still present afterward.
        let fix_js = r#"
            function fix(output, ctx) {
                return [{ op: "add", path: "/unrelated", value: 1 }];
            }
        "#;
        let output = json!({ "title": "nope" });
        let violations = vec![RuleViolation {
            pointer: "/title".to_owned(),
            message: "no title allowed".to_owned(),
        }];
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &output,
            &json!({}),
            &Map::new(),
            &violations,
            &budget(),
        )
        .unwrap_err();
        match err {
            BrokenFix::DidNotFix { remaining } => {
                assert_eq!(remaining[0].pointer, "/title");
            }
            other => panic!("expected DidNotFix, got {other:?}"),
        }
    }

    #[test]
    fn a_fix_returning_a_non_array_is_malformed_ops() {
        let fix_js = "function fix(output, ctx) { return { not: \"an array\" }; }";
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &[],
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenFix::MalformedOps(_)));
    }

    #[test]
    fn a_fix_returning_no_ops_is_no_ops() {
        let fix_js = "function fix(output, ctx) { return []; }";
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &[],
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenFix::NoOps));
    }

    #[test]
    fn ops_json_patch_rejects_are_ops_rejected() {
        let fix_js = "function fix(output, ctx) { return [{ op: 'remove', path: '/nowhere' }]; }";
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &[],
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenFix::OpsRejected(_)));
    }

    #[test]
    fn a_throwing_fix_is_threw() {
        let fix_js = "function fix(output, ctx) { throw new Error('boom'); }";
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &[],
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenFix::Threw(_)));
    }

    #[test]
    fn a_missing_fix_function_is_no_fix_function() {
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            "const x = 1;",
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &[],
            &budget(),
        )
        .unwrap_err();
        assert!(matches!(err, BrokenFix::NoFixFunction));
    }

    #[test]
    fn an_infinite_loop_inside_fix_is_a_broken_fix_not_a_hang() {
        let b = ScriptBudget {
            loop_iterations: 1000,
            ..budget()
        };
        let fix_js = "function fix(output, ctx) { while (true) {} }";
        let err = verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &json!({ "title": "x" }),
            &json!({}),
            &Map::new(),
            &[],
            &b,
        )
        .unwrap_err();
        assert!(matches!(err, BrokenFix::Threw(_)));
    }

    #[test]
    fn the_violations_the_check_reported_are_visible_on_ctx() {
        let fix_js = r#"
            function fix(output, ctx) {
                if (ctx.violations.length !== 1 || ctx.violations[0].pointer !== "/title") {
                    throw new Error("violations not visible as expected");
                }
                return [{ op: "remove", path: ctx.violations[0].pointer }];
            }
        "#;
        let output = json!({ "title": "nope" });
        let violations = vec![RuleViolation {
            pointer: "/title".to_owned(),
            message: "no title allowed".to_owned(),
        }];
        verify_fix(
            REMOVE_TITLE_CHECK,
            fix_js,
            &output,
            &json!({}),
            &Map::new(),
            &violations,
            &budget(),
        )
        .expect("ctx.violations must carry the check's own violations");
    }
}
