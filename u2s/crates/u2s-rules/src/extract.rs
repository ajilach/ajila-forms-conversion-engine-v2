//! [`run_extract`]: an `ingest_script` fact's `extract(ingest)`.
//!
//! Such a fact computes its value mechanically from the input's stored
//! ingest data (the outline nodes and text of each file) instead of asking
//! an LLM. It is generated code like a rule's check, so it runs in the same
//! sandbox under the same budget. Validating the returned value against the
//! fact revision's answer schema is the host's job: this crate returns the
//! value as the script produced it.

use std::time::Instant;

use boa_engine::{JsValue, Source};
use serde_json::Value;

use crate::budget::ScriptBudget;
use crate::contract::BrokenRule;
use crate::sandbox;

const PRELUDE: &str = include_str!("prelude.js");
const INGEST_GLOBAL: &str = "__U2S_INGEST_JSON__";

/// Runs `extract(ingest)` in a fresh sandbox and returns its value.
pub fn run_extract(
    script_js: &str,
    ingest: &Value,
    budget: &ScriptBudget,
) -> Result<Value, BrokenRule> {
    let started = Instant::now();
    let mut context = sandbox::build(budget);

    let ingest_json = serde_json::to_string(ingest).expect("serde_json::Value always serializes");
    sandbox::define_global_json(&mut context, INGEST_GLOBAL, &ingest_json);

    context
        .eval(Source::from_bytes(PRELUDE))
        .expect("the built-in prelude is trusted code and must always evaluate");
    context
        .eval(Source::from_bytes(script_js))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?;

    let is_function = context
        .eval(Source::from_bytes("typeof extract"))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?
        .as_string()
        .map(|s| s.to_std_string_escaped())
        == Some("function".to_owned());
    if !is_function {
        return Err(BrokenRule::NoExtractFunction);
    }

    let driver = format!(
        "(function () {{ \
           var __result = extract(JSON.parse(globalThis.{INGEST_GLOBAL})); \
           return __result === undefined ? undefined : JSON.stringify(__result); \
         }})()"
    );
    let result: JsValue = context
        .eval(Source::from_bytes(&driver))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?;

    let text = result
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| {
            BrokenRule::MalformedResult(
                "extract() must return a JSON-serializable value".to_owned(),
            )
        })?;
    let value: Value =
        serde_json::from_str(&text).map_err(|err| BrokenRule::MalformedResult(err.to_string()))?;

    let elapsed = started.elapsed();
    if elapsed > budget.wall_clock {
        return Err(BrokenRule::Timeout(elapsed));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn budget() -> ScriptBudget {
        ScriptBudget::default()
    }

    #[test]
    fn extract_reads_the_ingest_view() {
        let script = r#"
            function extract(ingest) {
                return ingest.files.flatMap(f => (f.outline || [])
                    .filter(n => n.kind === "field")
                    .map(n => n.path));
            }
        "#;
        let ingest = json!({ "files": [{
            "filename": "form.pdf",
            "outline": [
                { "path": "form.page1", "kind": "subform" },
                { "path": "form.page1.name", "kind": "field" }
            ]
        }]});
        assert_eq!(
            run_extract(script, &ingest, &budget()).unwrap(),
            json!(["form.page1.name"])
        );
    }

    #[test]
    fn a_script_without_extract_is_broken() {
        assert!(matches!(
            run_extract("function check() {}", &json!({}), &budget()),
            Err(BrokenRule::NoExtractFunction)
        ));
    }

    #[test]
    fn returning_nothing_is_malformed() {
        assert!(matches!(
            run_extract("function extract(i) {}", &json!({}), &budget()),
            Err(BrokenRule::MalformedResult(_))
        ));
    }

    #[test]
    fn a_throwing_extract_is_broken() {
        assert!(matches!(
            run_extract(
                "function extract(i) { throw new Error('x'); }",
                &json!({}),
                &budget()
            ),
            Err(BrokenRule::Threw(_))
        ));
    }

    #[test]
    fn a_runaway_extract_is_stopped_by_the_loop_budget() {
        assert!(
            run_extract(
                "function extract(i) { while (true) {} }",
                &json!({}),
                &budget()
            )
            .is_err()
        );
    }
}
