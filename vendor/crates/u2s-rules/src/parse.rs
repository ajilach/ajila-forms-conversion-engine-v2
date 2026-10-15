//! [`parse_script`]: whether a check script loads at all, without running
//! `check`.
//!
//! A host that takes scripts from outside a store (a rules directory, an
//! import) refuses a script that cannot even be evaluated when it is loaded,
//! rather than recording it as `broken` on every run it is checked against.

use boa_engine::Source;

use crate::budget::ScriptBudget;
use crate::contract::BrokenRule;
use crate::sandbox;

const PRELUDE: &str = include_str!("prelude.js");

/// Evaluates only the script's top level (never `check`) and confirms it
/// defines a `check` function.
pub fn parse_script(script_js: &str, budget: &ScriptBudget) -> Result<(), BrokenRule> {
    let mut context = sandbox::build(budget);
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> ScriptBudget {
        ScriptBudget::default()
    }

    #[test]
    fn a_script_defining_check_parses() {
        assert!(parse_script("function check(o, c) {}", &budget()).is_ok());
    }

    #[test]
    fn check_itself_is_never_run() {
        let script = "function check(o, c) { throw new Error('must not run'); }";
        assert!(parse_script(script, &budget()).is_ok());
    }

    #[test]
    fn a_script_without_check_is_refused() {
        assert!(matches!(
            parse_script("const x = 1;", &budget()),
            Err(BrokenRule::NoCheckFunction)
        ));
    }

    #[test]
    fn a_throwing_or_unparsable_top_level_is_broken() {
        for script in ["throw new Error('boom');", "this is not javascript {{{"] {
            assert!(
                matches!(parse_script(script, &budget()), Err(BrokenRule::Threw(_))),
                "{script}"
            );
        }
    }
}
