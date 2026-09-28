//! Constructing a boa [`Context`] that is sandboxed by construction, not by
//! policy — the crate's whole security argument rests on this file.
//!
//! Two things a caller might reasonably assume `boa_engine::Context::default()`
//! already does, and does not:
//!
//! - **It has no filesystem access.** False. With no module loader supplied,
//!   boa's own builder falls back to `SimpleModuleLoader::new(".")`, which
//!   resolves `import` specifiers against the process's *current working
//!   directory* and reads them off disk with `Source::from_filepath`. A
//!   script doing `import("./Cargo.toml")` against a bare `Context::default()`
//!   would read this workspace's own manifest. Replacing the loader with
//!   [`IdleModuleLoader`] — which answers every `import` with "no such
//!   module" — is the single most important line below.
//! - **A runaway loop cannot hang.** False. `RuntimeLimits::loop_iteration_limit`
//!   defaults to `u64::MAX`, i.e. no limit at all; recursion and stack limits
//!   have finite (generous) defaults but are set explicitly here anyway, so
//!   the sandbox's behaviour does not depend on boa's own defaults staying
//!   what they are today.
//!
//! What is deliberately **not** done: no native Rust function is registered
//! on the context (see [`crate::check`] for how data crosses the boundary
//! instead — as JSON text, via `JSON.parse`/`JSON.stringify`), so there is
//! no Rust call surface for a script to reach into at all. This is strictly
//! stronger than PLAN.md's "expose only pure JSON helpers": there is nothing
//! exposed to audit in the first place.

use std::rc::Rc;

use boa_engine::module::IdleModuleLoader;
use boa_engine::{Context, JsValue, property::Attribute};

use crate::budget::ScriptBudget;

/// A fresh, locked-down context with `budget`'s limits applied. Never
/// reused across scripts — boa's `Context` is `!Send` (it holds `Rc`s
/// internally), so one is built per evaluation on whatever thread is
/// running it; see `check::run_check`'s docs for the threading discipline
/// that follows from that.
pub(crate) fn build(budget: &ScriptBudget) -> Context {
    let mut context = Context::builder()
        .module_loader(Rc::new(IdleModuleLoader))
        .build()
        .expect("a context with no host hooks and the idle module loader always builds");

    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(budget.loop_iterations);
    limits.set_recursion_limit(budget.recursion_limit);
    limits.set_stack_size_limit(budget.stack_size_bytes);

    context
}

/// Registers `name` as a non-configurable, non-writable global string —
/// this is how the output JSON and the schema JSON cross into the script's
/// world. A global *value*, never a callable, so it carries data and
/// nothing else: the script parses it with `JSON.parse`, which is ordinary
/// language surface, not a host capability.
pub(crate) fn define_global_json(context: &mut Context, name: &str, json_text: &str) {
    context
        .register_global_property(
            boa_engine::JsString::from(name),
            JsValue::from(boa_engine::JsString::from(json_text)),
            Attribute::default(),
        )
        .expect("defining a fresh global property on a freshly built context cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::{JsError, Source};

    #[test]
    fn the_idle_loader_refuses_every_import() {
        use boa_engine::builtins::promise::PromiseState;
        use boa_engine::object::builtins::JsPromise;

        let mut context = build(&ScriptBudget::default());
        // Dynamic `import()` always evaluates to a Promise at the top
        // level — per spec it never throws synchronously, so the property
        // to check is not "eval returns an error" but "the promise the
        // idle loader hands back is already rejected, with no file ever
        // touched". If the default `SimpleModuleLoader` were still active,
        // this would instead read this crate's own Cargo.toml off disk.
        let result: JsValue = context
            .eval(Source::from_bytes("import('./Cargo.toml')"))
            .expect("dynamic import() itself never throws synchronously");
        let promise = JsPromise::from_object(
            result
                .as_object()
                .expect("import() returns an object")
                .clone(),
        )
        .expect("the returned object is a real Promise");
        assert!(
            matches!(promise.state(), PromiseState::Rejected(_)),
            "the idle loader must synchronously reject every import"
        );
    }

    #[test]
    fn an_infinite_loop_is_killed_by_the_iteration_limit() {
        let budget = ScriptBudget {
            loop_iterations: 10,
            ..ScriptBudget::default()
        };
        let mut context = build(&budget);
        let result: Result<JsValue, JsError> = context.eval(Source::from_bytes("while (true) {}"));
        assert!(
            result.is_err(),
            "the loop must be killed, not hang the test"
        );
    }

    #[test]
    fn unbounded_recursion_is_killed_by_the_recursion_limit() {
        let budget = ScriptBudget {
            recursion_limit: 10,
            ..ScriptBudget::default()
        };
        let mut context = build(&budget);
        let result: Result<JsValue, JsError> = context.eval(Source::from_bytes(
            "function f(n) { return f(n + 1); } f(0)",
        ));
        assert!(result.is_err(), "unbounded recursion must be killed");
    }

    #[test]
    fn no_filesystem_or_process_globals_are_reachable() {
        let mut context = build(&ScriptBudget::default());
        for probe in ["typeof require", "typeof process", "typeof Deno"] {
            let value = context
                .eval(Source::from_bytes(probe))
                .expect("typeof never throws");
            assert_eq!(
                value.as_string().map(|s| s.to_std_string_escaped()),
                Some("undefined".to_owned()),
                "{probe} must be undefined in the sandbox"
            );
        }
    }
}
