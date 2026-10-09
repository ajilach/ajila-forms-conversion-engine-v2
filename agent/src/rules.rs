//! The rules a run's document is held to, and the sandbox their scripts run in.
//!
//! The rules live under `rules/aem/` at the repository root, one directory
//! per rule, compiled in. A rule is one of two kinds, by what its directory
//! holds:
//!
//! - **scripted**: `rule.toml` and `check.js` (optionally `fix.js`). Its script
//!   decides it; it runs on every edit and in `rule_check`.
//! - **judged**: `rule.toml` alone. No script can decide it, so `rule_check`
//!   hands its description to a judge agent (the pipeline's, see
//!   `pipeline::judge`); without one it is reported unchecked.
//!
//! They live here rather than in the vendored UBS layer, which keeps the
//! templates, the writer and the normalize passes: `specs/feedback/rule-coverage.md`
//! says which of the feedback guard's problems each one stands for. Every script
//! runs in a worker process with a memory and time ceiling (see `u2s-rules-host`),
//! so a runaway script fails its own rule rather than the conversion. That
//! process is the running executable itself, started with the worker flag (see
//! [`runner`]).

use std::ffi::OsString;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use u2s_doc_tools::native::RuleForCheck;
use u2s_doc_tools::rules_dir::RuleFiles;
use u2s_rules_host::runner::RuleRunner;
use u2s_rules_host::worker::WORKER_ARG;

/// The worker binary's file name, which only test binaries use.
fn worker_name() -> String {
    format!("u2s-rules-worker{}", std::env::consts::EXE_SUFFIX)
}

/// What every rule runs in: this executable itself, started with
/// [`WORKER_ARG`], so the app and the CLI ship no second
/// program. Each of them hands control to [`serve_worker_if_invoked`] first
/// thing in `main`.
///
/// A test binary cannot act as the worker (the test harness owns its `main`),
/// so a test runs from `target/<profile>/deps` and uses the built
/// `u2s-rules-worker` one level up instead.
fn worker_command() -> Result<(PathBuf, Vec<OsString>), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    let dir = exe.parent().ok_or("this executable has no directory")?;
    if dir.ends_with("deps") {
        let parent = dir.parent().ok_or("the test binary's directory has no parent")?;
        return Ok((parent.join(worker_name()), Vec::new()));
    }
    Ok((exe, vec![WORKER_ARG.into()]))
}

/// Serves rule-worker requests and exits, when this process was started as
/// the worker; returns otherwise. Call it first thing in `main`, before a
/// runtime, a window or a stdio server exists.
pub fn serve_worker_if_invoked() {
    if u2s_rules_host::worker::invoked_as_worker() {
        u2s_rules_host::worker::run();
        std::process::exit(0);
    }
}

/// A runner over this executable as its own worker, or why there is none.
pub fn runner() -> Result<RuleRunner, String> {
    let (program, args) = worker_command()?;
    RuleRunner::with_args(program.clone(), args, RuleRunner::workers_from_env()).map_err(|e| {
        format!(
            "the rule sandbox cannot start: {e}. A test needs the worker built first: \
             `cargo build -p u2s-rules-host --bin u2s-rules-worker` (with `--release` for \
             release builds), so that it sits at {}",
            program.display()
        )
    })
}

static AEM_RULES: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../rules/aem");

/// A `rule.toml`, the same fields the vendored loader reads for a scripted
/// rule; parsed here too, for the judged rules it never sees.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleToml {
    id: String,
    title: String,
    description: String,
    #[serde(default)]
    #[allow(dead_code)] // validated, as the vendored loader does
    output_formats: Vec<String>,
}

/// A rule no script decides: a judge agent checks the document against its
/// description.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgedRule {
    /// The id `rule_list` and `rule_check` use, from the same scheme as a
    /// scripted rule's (`derive_rule_id` over the `rule.toml` id).
    pub id: String,
    /// The `rule.toml` id, e.g. `ubs-aem-option-dependent-fields`.
    pub name: String,
    pub title: String,
    pub description: String,
}

/// Every rule, by kind.
#[derive(Debug, Default)]
pub struct Rules {
    pub scripted: Vec<RuleForCheck>,
    pub judged: Vec<JudgedRule>,
}

fn text(dir: &include_dir::Dir<'_>, name: &str) -> Option<String> {
    dir.get_file(dir.path().join(name))
        .map(|f| f.contents_utf8().expect("a rule file is UTF-8").to_string())
}

/// The scripted rules, compiled in: the rule directories with a
/// `check.js`.
pub fn rule_files() -> Vec<RuleFiles> {
    AEM_RULES
        .dirs()
        .filter_map(|dir| {
            let slug = dir.path().to_string_lossy().into_owned();
            Some(RuleFiles {
                check_js: text(dir, "check.js")?,
                rule_toml: text(dir, "rule.toml").unwrap_or_else(|| panic!("rule {slug} has a rule.toml")),
                fix_js: text(dir, "fix.js"),
                slug,
            })
        })
        .collect()
}

/// The rules a document is held to. A rule directory without a
/// `rule.toml`, a judged rule with a `fix.js`, and two rules with one id are
/// errors: a rule that silently drops out is a rule nobody decided to stop.
pub fn rules() -> Result<Rules, String> {
    let fail = |e: String| format!("the rules do not load: {e}");
    let mut names = std::collections::BTreeSet::new();
    let mut judged = Vec::new();
    for dir in AEM_RULES.dirs() {
        let slug = dir.path().to_string_lossy().into_owned();
        let toml_text = text(dir, "rule.toml").ok_or_else(|| fail(format!("{slug} has no rule.toml")))?;
        let toml: RuleToml = toml::from_str(&toml_text).map_err(|e| fail(format!("{slug}/rule.toml: {e}")))?;
        if !names.insert(toml.id.clone()) {
            return Err(fail(format!("two rules have the id {}", toml.id)));
        }
        if text(dir, "check.js").is_none() {
            if text(dir, "fix.js").is_some() {
                return Err(fail(format!("{slug} has a fix.js but no check.js")));
            }
            judged.push(JudgedRule {
                id: u2s_doc_tools::rules_dir::derive_rule_id(&toml.id).to_string(),
                name: toml.id,
                title: toml.title,
                description: toml.description,
            });
        }
    }
    let scripted = u2s_doc_tools::rules_dir::load_rules(rule_files()).map_err(|e| fail(e.to_string()))?;
    Ok(Rules { scripted, judged })
}

/// What `rule_list` adds for the judged rules, and the `check` it marks every
/// rule with: `script` for a rule its script decides, `agent` for one a judge
/// does.
pub fn list_with_judged(mut listed: Value, judged: &[JudgedRule]) -> Value {
    if let Some(rules) = listed.get_mut("rules").and_then(Value::as_array_mut) {
        for rule in rules.iter_mut() {
            rule["check"] = json!("script");
        }
        rules.extend(judged.iter().map(|r| {
            json!({ "id": r.id, "title": r.title, "description": r.description, "check": "agent" })
        }));
    }
    listed
}

/// A judge's verdict on one rule, as `submit_rule_verdict` takes it (its
/// `judgement` is the key it is stored under, not part of the verdict).
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
pub struct RuleVerdict {
    pub pass: bool,
    #[serde(default)]
    pub violations: Vec<Violation>,
}

/// One place a rule is broken.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Violation {
    pub pointer: String,
    pub message: String,
}

impl RuleVerdict {
    /// A verdict that says what it found: a kept rule has no violations, and a
    /// broken one names at least one place to fix.
    pub fn validate(&self) -> Result<(), String> {
        match (self.pass, self.violations.is_empty()) {
            (true, false) => Err("pass=true lists violations: a kept rule has none".into()),
            (false, true) => Err("pass=false needs at least one violation saying where to fix it".into()),
            _ => Ok(()),
        }
    }
}

/// One `rule_check` report from the scripts' verdicts and the judged rules':
/// a judged rule with a verdict reads like a scripted one (`positive` or
/// `negative` with its violations), one without is `unchecked` with the
/// reason. Every verdict says which kind of check made it.
pub fn merge_rule_report(mut scripted: Value, judged: &[(JudgedRule, Result<RuleVerdict, String>)]) -> Value {
    // A script report of another shape keeps its place, but never swallows
    // the judges' verdicts.
    if !scripted.get("verdicts").is_some_and(Value::is_array) {
        scripted = match scripted {
            Value::Object(mut object) => {
                object.insert("verdicts".into(), json!([]));
                Value::Object(object)
            }
            other => json!({ "verdicts": [], "scripted_report": other }),
        };
    }
    let verdicts = scripted["verdicts"].as_array_mut().expect("made an array above");
    for verdict in verdicts.iter_mut() {
        verdict["check"] = json!("script");
    }
    for (rule, outcome) in judged {
        verdicts.push(match outcome {
            Ok(v) => json!({
                "rule_id": rule.id,
                "title": rule.title,
                "check": "agent",
                "verdict": if v.pass { "positive" } else { "negative" },
                "violations": v.violations,
            }),
            Err(reason) => json!({
                "rule_id": rule.id,
                "title": rule.title,
                "check": "agent",
                "verdict": "unchecked",
                "violations": [],
                "unchecked_reason": reason,
            }),
        });
    }
    scripted
}

/// Check that the rules load and their sandbox starts, before a run
/// spends a token: a document that cannot be held to its rules is not a
/// conversion to start. Reports what it checked.
pub fn readiness() -> Result<String, String> {
    let rules = rules()?;
    if rules.scripted.is_empty() && rules.judged.is_empty() {
        return Ok("no rules".into());
    }
    if !rules.scripted.is_empty() {
        runner()?;
    }
    Ok(format!(
        "{} scripted rules, each run in a sandboxed worker process, and {} judged by an agent",
        rules.scripted.len(),
        rules.judged.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_load() {
        let rules = rules().unwrap();
        assert!(!rules.scripted.is_empty() && !rules.judged.is_empty());
    }

    fn judged(name: &str) -> JudgedRule {
        JudgedRule {
            id: format!("id-{name}"),
            name: name.into(),
            title: format!("Title {name}"),
            description: format!("Description {name}"),
        }
    }

    #[test]
    fn rule_list_holds_both_kinds_and_says_which() {
        let listed = list_with_judged(
            json!({"rules": [{"id": "s", "title": "Scripted", "description": "d"}]}),
            &[judged("a")],
        );
        let rules = listed["rules"].as_array().unwrap();
        assert_eq!(rules[0]["check"], "script");
        assert_eq!(rules[1], json!({"id": "id-a", "title": "Title a", "description": "Description a", "check": "agent"}));
    }

    #[test]
    fn a_judged_rule_reads_like_a_scripted_one_or_says_why_it_is_unchecked() {
        let merged = merge_rule_report(
            json!({"verdicts": [{"rule_id": "s", "verdict": "positive", "violations": []}], "package_findings": []}),
            &[
                (judged("a"), Ok(RuleVerdict { pass: false, violations: vec![Violation { pointer: "/form".into(), message: "m".into() }] })),
                (judged("b"), Ok(RuleVerdict { pass: true, violations: vec![] })),
                (judged("c"), Err("no judge in this run".into())),
            ],
        );
        let v = merged["verdicts"].as_array().unwrap();
        assert_eq!(v[0]["check"], "script");
        assert_eq!((v[1]["verdict"].as_str(), v[1]["check"].as_str()), (Some("negative"), Some("agent")));
        assert_eq!(v[1]["violations"][0]["pointer"], "/form");
        assert_eq!(v[2]["verdict"], "positive");
        assert_eq!((v[3]["verdict"].as_str(), v[3]["unchecked_reason"].as_str()), (Some("unchecked"), Some("no judge in this run")));
        assert_eq!(merged["package_findings"], json!([]));

        // A script report of another shape does not swallow the judges' verdicts.
        let merged = merge_rule_report(json!("broken"), &[(judged("a"), Err("x".into()))]);
        assert_eq!(merged["verdicts"][0]["rule_id"], "id-a");
        assert_eq!(merged["scripted_report"], "broken");
    }

    #[test]
    fn a_verdict_says_what_it_found() {
        let violation = || Violation { pointer: "/form".into(), message: "m".into() };
        assert!(RuleVerdict { pass: true, violations: vec![] }.validate().is_ok());
        assert!(RuleVerdict { pass: false, violations: vec![violation()] }.validate().is_ok());
        assert!(RuleVerdict { pass: true, violations: vec![violation()] }.validate().is_err());
        assert!(RuleVerdict { pass: false, violations: vec![] }.validate().is_err());
    }
}
