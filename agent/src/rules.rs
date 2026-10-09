//! The rules a run's document is held to, and the sandbox their scripts run in.
//!
//! Each target's rules live under `rules/<target>/` at the repository root
//! (`rules/aem/`, `rules/redacto/`), one directory per rule, compiled in. A rule
//! is one of two kinds, by what its directory holds:
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

use crate::OutputTarget;

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
static REDACTO_RULES: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../rules/redacto");

fn rule_dir(target: OutputTarget) -> &'static include_dir::Dir<'static> {
    match target {
        OutputTarget::Aem => &AEM_RULES,
        OutputTarget::Redacto => &REDACTO_RULES,
    }
}

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
    /// What a judged rule's judge reads (see [`JudgeScope`]). A scripted rule
    /// has no judge, so it may not carry one.
    #[serde(default)]
    judge: Option<JudgeScope>,
}

/// What a judged rule's judge reads, from the `[judge]` block of its
/// `rule.toml`. A rule without one reads the whole document and the source
/// with its tools, as every judge did before the block existed.
///
/// The part of the document the scope names is put into the judge's prompt,
/// so it does not spend turns reading it, and it is what the judge's verdict
/// is kept for: an edit elsewhere leaves the verdict standing.
///
/// ```toml
/// [judge]
/// pointers = ["/header"]          # parts of the document, by JSON Pointer
/// node_types = ["Repeatable"]     # every node of these `type`s, wherever it sits
/// source = "first_page"           # "tools" (default), "first_page" or "none"
/// max_turns = 12                  # the judge's turn budget, when not the default
/// ```
///
/// A scope is a promise that the rule reads nothing else of the document:
/// name too little and a verdict outlives the edit that broke it. A rule
/// that walks the whole document names nothing, and keeps the whole document
/// as its key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeScope {
    /// JSON Pointers of the parts of the document the rule reads.
    #[serde(default)]
    pub pointers: Vec<String>,
    /// Node `type`s the rule reads, wherever such a node sits.
    #[serde(default)]
    pub node_types: Vec<String>,
    /// What the judge needs of the source.
    #[serde(default)]
    pub source: SourceNeed,
    /// The judge's turn budget, when the rule needs another than the
    /// default (a rule that walks every page needs more).
    #[serde(default)]
    pub max_turns: Option<usize>,
}

impl JudgeScope {
    /// Whether the scope names a part of the document rather than all of it.
    pub fn is_partial(&self) -> bool {
        !self.pointers.is_empty() || !self.node_types.is_empty()
    }
}

/// What a judge needs of the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceNeed {
    /// It reads the source itself, with the xfa_* tools.
    #[default]
    Tools,
    /// The text of the first page of every source PDF, put into its prompt
    /// (a master-page header, a title). It can still read more with the tools.
    FirstPage,
    /// Nothing: the rule is about the document alone.
    None,
}

/// A rule no script decides: a judge agent checks the document against its
/// description.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JudgedRule {
    /// The id `rule_list` and `rule_check` use, from the same scheme as a
    /// scripted rule's (`derive_rule_id` over the `rule.toml` id).
    pub id: String,
    /// The `rule.toml` id, e.g. `ubs-aem-option-dependent-fields`.
    pub name: String,
    pub title: String,
    pub description: String,
    /// What its judge reads (see [`JudgeScope`]).
    pub scope: JudgeScope,
}

/// Every rule of a target, by kind.
#[derive(Debug, Default)]
pub struct Rules {
    pub scripted: Vec<RuleForCheck>,
    pub judged: Vec<JudgedRule>,
}

fn text(dir: &include_dir::Dir<'_>, name: &str) -> Option<String> {
    dir.get_file(dir.path().join(name))
        .map(|f| f.contents_utf8().expect("a rule file is UTF-8").to_string())
}

/// The scripted rules of `target`, compiled in: the rule directories with a
/// `check.js`.
pub fn rule_files(target: OutputTarget) -> Vec<RuleFiles> {
    rule_dir(target)
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

/// The rules `target`'s documents are held to. A rule directory without a
/// `rule.toml`, a judged rule with a `fix.js`, and two rules with one id are
/// errors: a rule that silently drops out is a rule nobody decided to stop.
pub fn rules_for(target: OutputTarget) -> Result<Rules, String> {
    let fail = |e: String| format!("the {} rules do not load: {e}", target.label());
    let mut names = std::collections::BTreeSet::new();
    let mut judged = Vec::new();
    for dir in rule_dir(target).dirs() {
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
            let scope = toml.judge.unwrap_or_default();
            if let Some(bad) = scope.pointers.iter().find(|p| !p.is_empty() && !p.starts_with('/')) {
                return Err(fail(format!("{slug}/rule.toml: [judge] pointer {bad:?} is not a JSON Pointer")));
            }
            if scope.max_turns == Some(0) {
                return Err(fail(format!("{slug}/rule.toml: [judge] max_turns is 0")));
            }
            judged.push(JudgedRule {
                id: u2s_doc_tools::rules_dir::derive_rule_id(&toml.id).to_string(),
                name: toml.id,
                title: toml.title,
                description: toml.description,
                scope,
            });
        } else if toml.judge.is_some() {
            return Err(fail(format!("{slug} has a check.js and a [judge] block: a script decides it, no judge")));
        }
    }
    let scripted = u2s_doc_tools::rules_dir::load_rules(rule_files(target)).map_err(|e| fail(e.to_string()))?;
    Ok(Rules { scripted, judged })
}

/// `rule_list`: an index of every rule by id and title, in two lists by how
/// a rule is checked: `scripted` (its script decides it, on every edit too)
/// and `judged` (a judge agent does). The descriptions are what made the full
/// list long (75'000 characters for the AEM rules), so they come from
/// [`rule_get`] for the rules a stage asks for.
pub fn list_with_judged(listed: Value, judged: &[JudgedRule]) -> Value {
    let scripted: Vec<Value> = listed
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| rules.iter().map(|r| json!({ "id": r["id"], "title": r["title"] })).collect())
        .unwrap_or_default();
    let judged: Vec<Value> = judged.iter().map(|r| json!({ "id": r.id, "title": r.title })).collect();
    json!({
        "scripted": scripted,
        "judged": judged,
        "descriptions": "rule_get with the ids you need returns each rule's description: what is required, \
                         how it is judged and how to fix a break.",
    })
}

/// `rule_get`: the full text of the rules `input.ids` names (every rule when
/// it names none), from the scripted rules' and the judged rules' own
/// `rule.toml`. An id no rule has is an error, not a silently shorter list.
pub fn rule_get(scripted: &[RuleForCheck], judged: &[JudgedRule], input: &Value) -> Result<Value, String> {
    let ids: Vec<&str> = match input.get("ids") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| id.as_str().ok_or("ids holds only strings"))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("ids is a list of rule ids from rule_list".into()),
    };
    let wanted = |id: &str| ids.is_empty() || ids.contains(&id);
    let mut rules: Vec<Value> = scripted
        .iter()
        .filter(|r| wanted(&r.id.to_string()))
        .map(|r| json!({ "id": r.id.to_string(), "title": r.title, "check": "script", "description": r.description_md }))
        .collect();
    rules.extend(
        judged
            .iter()
            .filter(|r| wanted(&r.id))
            .map(|r| json!({ "id": r.id, "title": r.title, "check": "agent", "description": r.description })),
    );
    let unknown: Vec<&&str> = ids.iter().filter(|id| !rules.iter().any(|r| r["id"] == **id)).collect();
    if !unknown.is_empty() {
        return Err(format!("no rule has the id {unknown:?}; rule_list lists them"));
    }
    Ok(json!({ "rules": rules }))
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
///
/// The report is compact: a positive verdict says nothing to act on, so the
/// positive rules are only counted and named in `positive`; `verdicts` holds
/// the others in full. `summary` counts every kind.
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
    compact(scripted)
}

/// Folds the positive verdicts of a full report into `positive` and a count,
/// and drops what a verdict says by default (no fix script, no reason).
fn compact(mut report: Value) -> Value {
    let all = match report.get_mut("verdicts").map(Value::take) {
        Some(Value::Array(all)) => all,
        _ => Vec::new(),
    };
    let mut counts = serde_json::Map::new();
    let mut positive = Vec::new();
    let mut rest = Vec::new();
    for mut verdict in all {
        let kind = verdict["verdict"].as_str().unwrap_or("unknown").to_string();
        let count = counts.entry(kind.clone()).or_insert(json!(0));
        *count = json!(count.as_u64().unwrap_or(0) + 1);
        if kind == "positive" {
            positive.push(verdict["rule_id"].take());
            continue;
        }
        if let Some(object) = verdict.as_object_mut() {
            object.retain(|key, value| match key.as_str() {
                "autofix_available" => value != &json!(false),
                "broken_reason" | "indeterminate_reason" | "unchecked_reason" => !value.is_null(),
                _ => true,
            });
        }
        rest.push(verdict);
    }
    let object = report.as_object_mut().expect("a report is an object");
    object.insert("summary".into(), Value::Object(counts));
    object.insert("positive".into(), Value::Array(positive));
    object.insert("verdicts".into(), Value::Array(rest));
    report
}

/// Check that `target`'s rules load and their sandbox starts, before a run
/// spends a token: a document that cannot be held to its rules is not a
/// conversion to start. Reports what it checked.
pub fn readiness(target: OutputTarget) -> Result<String, String> {
    let rules = rules_for(target)?;
    if rules.scripted.is_empty() && rules.judged.is_empty() {
        return Ok("no rules for this format".into());
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
    fn every_targets_rules_load() {
        assert!(!rules_for(OutputTarget::Aem).unwrap().scripted.is_empty());
        rules_for(OutputTarget::Redacto).unwrap();
    }

    fn judged(name: &str) -> JudgedRule {
        JudgedRule {
            id: format!("id-{name}"),
            name: name.into(),
            title: format!("Title {name}"),
            description: format!("Description {name}"),
            scope: JudgeScope::default(),
        }
    }

    #[test]
    fn rule_list_is_an_index_of_both_kinds() {
        let listed = list_with_judged(
            json!({"rules": [{"id": "s", "title": "Scripted", "description": "d"}]}),
            &[judged("a")],
        );
        assert_eq!(listed["scripted"], json!([{"id": "s", "title": "Scripted"}]));
        assert_eq!(listed["judged"], json!([{"id": "id-a", "title": "Title a"}]));
        assert!(listed["descriptions"].as_str().unwrap().contains("rule_get"));
    }

    /// The real AEM index stays small: the descriptions are what made the old
    /// list 75'000 characters long.
    #[test]
    fn the_aem_rule_index_is_small() {
        let rules = rules_for(OutputTarget::Aem).unwrap();
        let listed: Vec<Value> =
            rules.scripted.iter().map(|r| json!({"id": r.id.to_string(), "title": r.title})).collect();
        let index = list_with_judged(json!({ "rules": listed }), &rules.judged).to_string();
        assert!(index.len() < 6_000, "rule_list is {} characters", index.len());
    }

    #[test]
    fn rule_get_returns_the_named_rules_in_full_and_refuses_an_unknown_id() {
        let rules = rules_for(OutputTarget::Aem).unwrap();
        let scripted_id = rules.scripted[0].id.to_string();
        let judged_id = rules.judged[0].id.clone();
        let got = rule_get(&rules.scripted, &rules.judged, &json!({"ids": [scripted_id, judged_id]})).unwrap();
        let got = got["rules"].as_array().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0]["check"].as_str(), got[1]["check"].as_str()), (Some("script"), Some("agent")));
        assert!(got.iter().all(|r| !r["description"].as_str().unwrap().is_empty()));
        let all = rule_get(&rules.scripted, &rules.judged, &json!({})).unwrap();
        assert_eq!(all["rules"].as_array().unwrap().len(), rules.scripted.len() + rules.judged.len());
        let err = rule_get(&rules.scripted, &rules.judged, &json!({"ids": ["nope"]})).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn a_report_names_the_positive_rules_and_spells_out_the_rest() {
        let merged = merge_rule_report(
            json!({"verdicts": [
                {"rule_id": "s", "title": "S", "verdict": "positive", "violations": [], "broken_reason": null, "autofix_available": false},
                {"rule_id": "t", "title": "T", "verdict": "negative", "violations": [{"pointer": "/x", "message": "m"}],
                 "broken_reason": null, "autofix_available": true}
            ], "package_findings": []}),
            &[
                (judged("a"), Ok(RuleVerdict { pass: false, violations: vec![Violation { pointer: "/form".into(), message: "m".into() }] })),
                (judged("b"), Ok(RuleVerdict { pass: true, violations: vec![] })),
                (judged("c"), Err("no judge in this run".into())),
            ],
        );
        assert_eq!(merged["positive"], json!(["s", "id-b"]));
        assert_eq!(merged["summary"], json!({"positive": 2, "negative": 2, "unchecked": 1}));
        let v = merged["verdicts"].as_array().unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], json!({"rule_id": "t", "title": "T", "verdict": "negative", "check": "script",
            "violations": [{"pointer": "/x", "message": "m"}], "autofix_available": true}));
        assert_eq!((v[1]["verdict"].as_str(), v[1]["check"].as_str()), (Some("negative"), Some("agent")));
        assert_eq!(v[1]["violations"][0]["pointer"], "/form");
        assert_eq!((v[2]["verdict"].as_str(), v[2]["unchecked_reason"].as_str()), (Some("unchecked"), Some("no judge in this run")));
        assert_eq!(merged["package_findings"], json!([]));

        // A script report of another shape does not swallow the judges' verdicts.
        let merged = merge_rule_report(json!("broken"), &[(judged("a"), Err("x".into()))]);
        assert_eq!(merged["verdicts"][0]["rule_id"], "id-a");
        assert_eq!(merged["scripted_report"], "broken");
    }

    /// A report where every rule holds fits in a couple of thousand
    /// characters, whatever the rules are.
    #[test]
    fn an_all_positive_report_is_small() {
        let rules = rules_for(OutputTarget::Aem).unwrap();
        let scripted: Vec<Value> = rules
            .scripted
            .iter()
            .map(|r| json!({"rule_id": r.id.to_string(), "title": r.title, "verdict": "positive", "violations": [],
                "broken_reason": null, "autofix_available": r.fix_js.is_some()}))
            .collect();
        let judged: Vec<_> =
            rules.judged.iter().map(|r| (r.clone(), Ok(RuleVerdict { pass: true, violations: vec![] }))).collect();
        let report = merge_rule_report(json!({ "verdicts": scripted, "package_findings": [] }), &judged).to_string();
        assert!(report.len() < 2_000, "an all-positive rule_check is {} characters", report.len());
    }

    #[test]
    fn a_judge_scope_is_read_from_the_rule_toml() {
        let toml: RuleToml = toml::from_str(
            "id = \"x\"\ntitle = \"t\"\ndescription = \"d\"\n[judge]\npointers = [\"/header\"]\nnode_types = [\"Repeatable\"]\nsource = \"first_page\"\nmax_turns = 12\n",
        )
        .unwrap();
        let scope = toml.judge.unwrap();
        assert_eq!(scope.pointers, ["/header"]);
        assert_eq!(scope.node_types, ["Repeatable"]);
        assert_eq!((scope.source, scope.max_turns), (SourceNeed::FirstPage, Some(12)));
        assert!(scope.is_partial());
        assert!(!JudgeScope::default().is_partial());
        assert!(toml::from_str::<RuleToml>("id = \"x\"\ntitle = \"t\"\ndescription = \"d\"\n[judge]\nwhat = 1\n").is_err());
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
