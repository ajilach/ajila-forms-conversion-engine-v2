//! [`read_requires`]: a script's declaration of the facts it reads.
//!
//! An extrinsic rule declares its facts at the top level of its script,
//!
//! ```js
//! const requires = ["source_fields"];
//! function check(output, ctx) { const fields = ctx.facts.source_fields; ... }
//! ```
//!
//! and the host reads that declaration when a revision is drafted, before
//! any check runs, to decide which fact revisions the revision pins. The
//! declaration lives in the script so it cannot drift from what the script
//! reads: `ctx.facts` throws on any key the host did not hand over, and the
//! host hands over exactly the declared ones.

use boa_engine::Source;

use crate::budget::ScriptBudget;
use crate::contract::BrokenRule;
use crate::sandbox;

const PRELUDE: &str = include_str!("prelude.js");

/// Evaluates only the script's top level (never `check`) and returns its
/// `requires` declaration: an empty list when the script declares none.
///
/// Refused as [`BrokenRule::MalformedRequires`] unless `requires` is an
/// array of distinct strings. Whether each string is a valid, existing fact
/// key is the host's question, not this crate's.
pub fn read_requires(script_js: &str, budget: &ScriptBudget) -> Result<Vec<String>, BrokenRule> {
    let mut context = sandbox::build(budget);
    context
        .eval(Source::from_bytes(PRELUDE))
        .expect("the built-in prelude is trusted code and must always evaluate");
    context
        .eval(Source::from_bytes(script_js))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?;

    // `typeof` on an undeclared global is the one read that cannot throw,
    // and a top-level `const` is visible to a later script in the same
    // realm, so this sees the declaration without running anything else.
    let declared = context
        .eval(Source::from_bytes(
            "typeof requires === 'undefined' ? null : JSON.stringify(requires)",
        ))
        .map_err(|err| BrokenRule::Threw(err.to_string()))?;

    if declared.is_null() {
        return Ok(Vec::new());
    }
    let text = declared
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| {
            BrokenRule::MalformedRequires("`requires` must be JSON-serializable".to_owned())
        })?;
    let keys: Vec<String> = serde_json::from_str(&text).map_err(|_| {
        BrokenRule::MalformedRequires(format!(
            "`requires` must be an array of fact names; got {text}"
        ))
    })?;

    let mut seen = std::collections::BTreeSet::new();
    for key in &keys {
        if !seen.insert(key) {
            return Err(BrokenRule::MalformedRequires(format!(
                "`requires` lists {key:?} more than once"
            )));
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> ScriptBudget {
        ScriptBudget::default()
    }

    #[test]
    fn a_script_without_a_declaration_requires_nothing() {
        let keys = read_requires("function check(o, c) {}", &budget()).unwrap();
        assert!(keys.is_empty());
    }

    #[test]
    fn a_const_declaration_is_read_in_order() {
        let script = r#"
            const requires = ["source_fields", "page_count"];
            function check(o, c) {}
        "#;
        assert_eq!(
            read_requires(script, &budget()).unwrap(),
            vec!["source_fields".to_owned(), "page_count".to_owned()]
        );
    }

    #[test]
    fn check_itself_is_never_run() {
        let script = r#"
            const requires = ["a"];
            function check(o, c) { throw new Error("must not run"); }
        "#;
        assert_eq!(
            read_requires(script, &budget()).unwrap(),
            vec!["a".to_owned()]
        );
    }

    #[test]
    fn a_duplicate_or_non_string_declaration_is_refused() {
        for script in [
            r#"const requires = ["a", "a"];"#,
            r#"const requires = "a";"#,
            r#"const requires = [1];"#,
            r#"const requires = { a: true };"#,
        ] {
            assert!(
                matches!(
                    read_requires(script, &budget()),
                    Err(BrokenRule::MalformedRequires(_))
                ),
                "{script}"
            );
        }
    }

    #[test]
    fn a_throwing_top_level_is_broken() {
        assert!(matches!(
            read_requires("throw new Error('boom');", &budget()),
            Err(BrokenRule::Threw(_))
        ));
    }
}
