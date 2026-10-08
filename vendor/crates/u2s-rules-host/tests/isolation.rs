//! The properties that justify running scripts out of process at all.
//!
//! Each of these fails, or takes the whole test binary down with it, if the
//! evaluation happens in-process. That is the point: they are not testing
//! the sandbox (`u2s-rules` does that), they are testing that a sandbox
//! failure stays survivable.

use std::path::PathBuf;

use serde_json::json;
use u2s_rules::{CheckVerdict, ScriptBudget};
use u2s_rules_host::protocol::CheckRequest;
use u2s_rules_host::runner::{RuleRunner, WORKER_BIN, into_map};
// The walk-up-from-`current_exe` lookup lives in the crate's own
// `test_support`, so this file, `u2s-agent`'s tests and `u2s-server`'s all
// share one copy of it.
use u2s_rules_host::runner::test_support::runner_with as runner;

fn request(script_js: &str, budget: ScriptBudget) -> CheckRequest {
    CheckRequest::new(
        script_js.to_owned(),
        None,
        json!({ "a": 1 }),
        json!({ "type": "object" }),
        serde_json::Map::new(),
        &budget,
    )
}

const PASSING: &str = "function check(output, ctx) { return { pass: true, violations: [] }; }";

/// **The reason this crate exists.**
///
/// `String.prototype.repeat` to near the maximum string length spends zero
/// loop iterations and zero recursion, so boa's own limits never fire. It
/// asks for tens of gigabytes, and Rust answers an allocation failure with
/// `abort()` -- not an unwind. In-process this kills the server outright and
/// no `catch_unwind` or `spawn_blocking` can stop it.
///
/// Out of process it is one dead child. The assertion that matters as much
/// as the verdict is the implicit one: this test function returns, so the
/// test process is still alive.
#[tokio::test]
async fn a_script_that_exhausts_memory_breaks_its_own_rule_and_nothing_else() {
    let runner = runner(2);
    let hungry = r#"function check(output, ctx) {
        var huge = "x".repeat(4294967294);
        return { pass: huge.length === 0, violations: [] };
    }"#;

    let results = runner
        .run_batch(vec![
            (1u32, request(hungry, ScriptBudget::default())),
            (2u32, request(PASSING, ScriptBudget::default())),
        ])
        .await;

    let by_key = u2s_rules_host::runner::into_map(results);
    assert_eq!(
        by_key[&1].verdict,
        CheckVerdict::Broken,
        "a script that cannot be given the memory it asks for is a broken rule"
    );
    assert!(
        by_key[&1].broken_reason.is_some(),
        "and the reason must say something an operator can act on"
    );
    assert_eq!(
        by_key[&2].verdict,
        CheckVerdict::Positive,
        "its neighbour in the same batch is unaffected"
    );
}

/// The deadline the in-process design could never enforce: `wall_clock` was
/// compared only *after* a script returned, so a script that never returned
/// was never stopped.
///
/// `while (true) {}` does trip boa's loop-iteration limit eventually, so the
/// script here spins without looping -- recursion-free, allocation-free,
/// and invisible to every limit boa offers.
#[tokio::test]
async fn a_script_that_never_finishes_is_stopped_at_its_deadline() {
    let runner = runner(2);
    let budget = ScriptBudget {
        wall_clock: std::time::Duration::from_secs(1),
        // Effectively unbounded, so the loop limit cannot be what stops it.
        loop_iterations: u64::MAX,
        ..ScriptBudget::default()
    };
    let spinner = "function check(output, ctx) { while (true) {} }";

    let started = std::time::Instant::now();
    let results = runner
        .run_batch(vec![
            (1u32, request(spinner, budget)),
            (2u32, request(PASSING, budget)),
        ])
        .await;
    let elapsed = started.elapsed();

    let by_key = u2s_rules_host::runner::into_map(results);
    assert_eq!(by_key[&1].verdict, CheckVerdict::Broken);
    assert_eq!(
        by_key[&2].verdict,
        CheckVerdict::Positive,
        "a hung sibling must not take the batch with it"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the batch must end on the deadline, not run forever: took {elapsed:?}"
    );
}

/// Parallelism, stated as the property the old single-worker design gave up:
/// N scripts that each take about `d` must not take about `N*d`.
#[tokio::test]
async fn a_batch_runs_in_parallel_across_workers() {
    // Busy work rather than a sleep: there is no clock in the sandbox, so
    // the only way to spend time is to spend it. The loop limit is raised
    // to match -- the point here is elapsed time, and a script stopped by
    // boa's own limit would measure nothing.
    let slow = "function check(output, ctx) { \
         var n = 0; for (var i = 0; i < 3000000; i++) { n += i; } \
         return { pass: true, violations: [] }; }";
    let budget = ScriptBudget {
        loop_iterations: u64::MAX,
        wall_clock: std::time::Duration::from_secs(30),
        ..ScriptBudget::default()
    };

    let jobs = |count: u32| -> Vec<(u32, CheckRequest)> {
        (0..count).map(|k| (k, request(slow, budget))).collect()
    };
    let warmup = |count: u32| -> Vec<(u32, CheckRequest)> {
        (0..count).map(|k| (k, request(PASSING, budget))).collect()
    };

    let serial = runner(1);
    let parallel = runner(4);

    // Warm both pools first, so the measurement is of scripts running
    // concurrently rather than of how many processes each pool had to
    // start.
    serial.run_batch(warmup(1)).await;
    parallel.run_batch(warmup(4)).await;

    let started = std::time::Instant::now();
    let one_at_a_time = serial.run_batch(jobs(4)).await;
    let serial_elapsed = started.elapsed();

    let started = std::time::Instant::now();
    let all_at_once = parallel.run_batch(jobs(4)).await;
    let parallel_elapsed = started.elapsed();

    assert_eq!(one_at_a_time.len(), 4);
    assert_eq!(all_at_once.len(), 4);
    assert!(
        all_at_once
            .iter()
            .all(|(_, outcome)| outcome.verdict == CheckVerdict::Positive),
        "every job still produces a real verdict"
    );
    assert!(
        parallel_elapsed < serial_elapsed,
        "four workers must beat one: {parallel_elapsed:?} vs {serial_elapsed:?}"
    );
}

/// A batch is not all-or-nothing. This is the invariant the previous
/// implementation asserted for a *throwing* script; isolation is what
/// extends it to a script that takes its interpreter down.
#[tokio::test]
async fn a_throwing_script_still_only_breaks_itself() {
    let runner = runner(2);
    let results = runner
        .run_batch(vec![
            (
                1u32,
                request(
                    "function check(output, ctx) { throw new Error('boom'); }",
                    ScriptBudget::default(),
                ),
            ),
            (2u32, request(PASSING, ScriptBudget::default())),
        ])
        .await;

    let by_key = u2s_rules_host::runner::into_map(results);
    assert_eq!(by_key[&1].verdict, CheckVerdict::Broken);
    assert!(
        by_key[&1]
            .broken_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("boom")),
        "a thrown error keeps its own message rather than becoming a generic failure"
    );
    assert_eq!(by_key[&2].verdict, CheckVerdict::Positive);
}

/// Ordinary verdicts must survive the round trip unchanged -- violations
/// included, since those are what the review page renders.
#[tokio::test]
async fn a_failing_script_returns_its_violations_through_the_worker() {
    let runner = runner(1);
    let failing = r#"function check(output, ctx) {
        return { pass: false, violations: [{ pointer: "/a", message: "must not be 1" }] };
    }"#;

    let results = runner
        .run_batch(vec![(7u32, request(failing, ScriptBudget::default()))])
        .await;

    let (_, outcome) = &results[0];
    assert_eq!(outcome.verdict, CheckVerdict::Negative);
    assert_eq!(outcome.violations[0]["pointer"], "/a");
    assert_eq!(outcome.violations[0]["message"], "must not be 1");
    assert!(outcome.broken_reason.is_none());
}

/// A worker is reused between jobs, so a script must not be able to leave
/// anything behind for the next one -- the sandbox builds a fresh boa
/// context per evaluation, and this asserts the pooling did not undermine
/// that.
#[tokio::test]
async fn one_script_cannot_leave_state_for_the_next() {
    let runner = runner(1);
    let planter = "globalThis.__planted = 'here'; \
                   function check(output, ctx) { return { pass: true, violations: [] }; }";
    let reader = r#"function check(output, ctx) {
        var leaked = typeof globalThis.__planted !== "undefined";
        return leaked
            ? { pass: false, violations: [{ pointer: "", message: "state leaked" }] }
            : { pass: true, violations: [] };
    }"#;

    // Sequential batches so both certainly land on the same worker.
    let first = runner
        .run_batch(vec![(1u32, request(planter, ScriptBudget::default()))])
        .await;
    assert_eq!(first[0].1.verdict, CheckVerdict::Positive);

    let second = runner
        .run_batch(vec![(2u32, request(reader, ScriptBudget::default()))])
        .await;
    assert_eq!(
        second[0].1.verdict,
        CheckVerdict::Positive,
        "a reused worker must still give each script a fresh context"
    );
}

/// Refusing to start without the binary is the design: a fallback to
/// in-process evaluation would quietly reinstate the abort risk.
#[test]
fn a_missing_worker_binary_is_refused_rather_than_worked_around() {
    let err = RuleRunner::new(PathBuf::from("/nonexistent/u2s-rules-worker"), 1)
        .expect_err("must refuse");
    assert!(format!("{err}").contains(WORKER_BIN));
}

/// A CPU budget must bound each job, not the worker.
///
/// `RLIMIT_CPU` counts CPU for the life of a process, and a worker is
/// reused across jobs. Set once to one job's budget it bounds the *worker*:
/// several honest scripts in a row exhaust it between them and the kernel
/// kills whichever one happens to be running, which the host then reports
/// as a broken rule. This is that regression -- every one of these scripts
/// is comfortably inside its own budget, and their sum is not.
#[tokio::test]
async fn honest_scripts_do_not_inherit_each_others_cpu_time() {
    // One worker, so every job in the batch really does run on the same
    // process. With a pool the sharing would still happen, just less
    // reliably from a test's point of view.
    let runner = runner(1);

    // ~0.4s of CPU each, well inside the two-second default, and six of
    // them is over twice the ceiling a single-shot limit would have set.
    let burner = r#"
        function check(output, ctx) {
            var n = 0;
            for (var i = 0; i < 300000; i++) { n = (n + i) % 7919; }
            return { pass: n >= 0, violations: [] };
        }
    "#;

    let jobs: Vec<(usize, CheckRequest)> = (0..6)
        .map(|i| {
            (
                i,
                CheckRequest {
                    script_js: burner.to_owned(),
                    fix_js: None,
                    output: json!({ "a": 1 }),
                    schema: json!({}),
                    facts: serde_json::Map::new(),
                    budget: (&ScriptBudget::default()).into(),
                },
            )
        })
        .collect();

    let results = into_map(runner.run_batch(jobs).await);
    assert_eq!(results.len(), 6);
    for i in 0..6 {
        let outcome = &results[&i];
        assert_eq!(
            outcome.verdict,
            CheckVerdict::Positive,
            "script {i} ran inside its own budget and must get its own verdict, \
             not one inherited from its predecessors: {:?}",
            outcome.broken_reason
        );
    }
}

#[tokio::test]
async fn facts_reach_ctx_through_the_worker() {
    let script = r#"
        const requires = ["expected"];
        function check(output, ctx) {
            const ok = output.a === ctx.facts.expected;
            return { pass: ok, violations: ok ? [] : [{ pointer: "/a", message: "differs" }] };
        }
    "#;
    let mut facts = serde_json::Map::new();
    facts.insert("expected".to_owned(), json!(2));
    let checked = CheckRequest::new(
        script.to_owned(),
        None,
        json!({ "a": 1 }),
        json!({}),
        facts,
        &ScriptBudget::default(),
    );
    let outcome = into_map(runner(1).run_batch(vec![((), checked)]).await)
        .remove(&())
        .unwrap();
    assert_eq!(outcome.verdict, CheckVerdict::Negative, "{outcome:?}");
}

#[tokio::test]
async fn a_requires_declaration_is_read_through_the_worker() {
    let runner = runner(1);
    let keys = runner
        .read_requires(
            "const requires = ['a', 'b']; function check() {}",
            &ScriptBudget::default(),
        )
        .await
        .unwrap();
    assert_eq!(keys, vec!["a".to_owned(), "b".to_owned()]);

    let err = runner
        .read_requires("const requires = 'a';", &ScriptBudget::default())
        .await
        .unwrap_err();
    assert!(err.contains("requires"), "{err}");
}

#[tokio::test]
async fn an_extract_runs_through_the_worker_and_a_runaway_one_is_stopped() {
    let runner = runner(1);
    let value = runner
        .run_extract(
            "function extract(ingest) { return ingest.files.length; }",
            &json!({ "files": [{}, {}] }),
            &ScriptBudget::default(),
        )
        .await
        .unwrap();
    assert_eq!(value, json!(2));

    let err = runner
        .run_extract(
            "function extract(ingest) { while (true) {} }",
            &json!({}),
            &ScriptBudget::default(),
        )
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}

/// A host that is its own worker starts it with arguments
/// (`RuleRunner::with_args`). The program here only becomes a worker when its
/// arguments arrive: `sh` without them would read the requests as a script.
#[cfg(unix)]
#[tokio::test]
async fn a_worker_started_with_arguments_answers_like_any_other() {
    let worker = u2s_rules_host::runner::test_support::worker_bin();
    let runner = RuleRunner::with_args(
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), format!("exec '{}'", worker.display()).into()],
        1,
    )
    .expect("sh is present");

    let results = runner
        .run_batch(vec![(1u32, request(PASSING, ScriptBudget::default()))])
        .await;

    assert_eq!(results[0].1.verdict, CheckVerdict::Positive, "{:?}", results[0].1);
}

/// Only the first argument decides whether a process is a worker.
#[test]
fn the_worker_argument_is_recognised_only_first() {
    use u2s_rules_host::worker::{WORKER_ARG, is_worker_invocation};
    let args = |list: &[&str]| list.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
    assert!(is_worker_invocation(args(&["host", WORKER_ARG])));
    assert!(!is_worker_invocation(args(&["host"])));
    assert!(!is_worker_invocation(args(&["host", "convert", WORKER_ARG])));
}
