//! A file-based rule loader: [`load_rules_dir`] reads one [`RuleForCheck`]
//! per subdirectory of a rules directory, for a host that has no store
//! behind it (see the crate's own doc for why `u2s-doc-tools` cannot depend
//! on `u2s-store` or `u2s-agent` at all).
//!
//! # Layout
//!
//! ```text
//! rules/
//!   no-empty-title/
//!     rule.toml
//!     check.js
//!     fix.js        (optional)
//!   amount-must-be-positive/
//!     rule.toml
//!     check.js
//! ```
//!
//! `rule.toml`:
//!
//! ```toml
//! id = "no-empty-title"
//! title = "Title must not be empty"
//! description = """
//! The document's `title` field must be a non-empty string. A blank title
//! reaches the review stage looking finished and is easy to miss.
//! """
//! output_formats = ["aem"]
//! ```
//!
//! `id` is a stable string the rule's author picks and does not change --
//! ordinarily the same text as the directory name, but read from the file
//! rather than the directory so renaming the directory never mints a new
//! rule. [`RuleForCheck::id`] is a [`u2s_core::RuleId`], not a string, so it
//! is derived from this field with UUIDv5 (a fixed, never-changing
//! namespace plus the `id` string) rather than asking the file to spell out
//! a raw UUID a person would have to invent and keep unique by hand: two
//! `rule.toml`s with the same `id` string collide by construction, which is
//! exactly the duplicate this loader must refuse.
//!
//! `output_formats` is read and validated (a list of strings) but not
//! filtered on here: `load_rules_dir` has no notion of "the run's output
//! format" to filter against, unlike the store-backed active-rules query
//! (`u2s-store`'s `active_rules`) that a real run goes through. A host that
//! cares reads it itself from the same file; nothing here drops it
//! silently, but nothing here acts on it either.
//!
//! # Facts
//!
//! A file-loaded rule has no facts system behind it, so every rule this
//! loader produces gets [`u2s_facts::FactsForCheck::Ready`] with an empty
//! map. A script that declares `const requires = [...]` with at least one
//! entry is refused at load time rather than silently run with no facts: a
//! check reading `ctx.facts.<key>` a file host can never resolve would
//! either throw (surfacing as `Broken`, which hides the real problem: the
//! rule cannot run *here*) or, if the sandbox let the read through, run on
//! a wrong assumption. Refusing to load is the honest answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use u2s_core::RuleId;
use u2s_rules::{BrokenRule, ScriptBudget, read_requires};
use uuid::Uuid;

use crate::native::RuleForCheck;

/// The name of a rule's check script within its subdirectory.
const CHECK_JS: &str = "check.js";
/// The name of a rule's optional fix script within its subdirectory.
const FIX_JS: &str = "fix.js";
/// The name of a rule's metadata file within its subdirectory.
const RULE_TOML: &str = "rule.toml";

/// A fixed, arbitrary namespace UUID for deriving a file-based rule's
/// [`RuleId`] from its `rule.toml` `id` string via UUIDv5
/// (`Uuid::new_v5(&RULE_ID_NAMESPACE, id.as_bytes())`). Generated once and
/// never to change: changing it would re-mint every rule loaded from a
/// directory that already exists, which a store-backed caller (autofix
/// results, run history keyed by `RuleId`) would see as a wholly different
/// rule.
fn rule_id_namespace() -> Uuid {
    Uuid::parse_str("2f6f7e9c-9a3b-5c1d-8e2a-4b6d7c8e9f0a").expect("a fixed, valid UUID literal")
}

/// Derives a file-based rule's [`RuleId`] from its stable `rule.toml` `id`
/// string. See the module doc for why this is UUIDv5 over a fixed
/// namespace rather than a raw UUID the file must spell out. Public so a host
/// that keeps rules of its own beside these (one judged by an agent rather
/// than a script, say) gives them ids from the same scheme.
pub fn derive_rule_id(id: &str) -> RuleId {
    RuleId::from(Uuid::new_v5(&rule_id_namespace(), id.as_bytes()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleToml {
    id: String,
    title: String,
    description: String,
    #[serde(default)]
    #[allow(dead_code)] // read and validated, not yet consumed -- see the module doc
    output_formats: Vec<String>,
}

/// Everything that stops [`load_rules_dir`] from returning a rule, always a
/// hard error -- there is no partial or best-effort result, since a rule
/// silently dropped from the set an agent checks against is a rule that
/// stops being enforced without anyone deciding that.
#[derive(Debug, thiserror::Error)]
pub enum RulesDirError {
    /// A rule directory whose name is not UTF-8, so it has no slug to report or sort by.
    #[error("the rule directory {path} is not named in UTF-8")]
    SlugNotUtf8 { path: PathBuf },
    #[error("could not read the rules directory {path}: {source}")]
    RulesDirUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{slug}/{RULE_TOML} could not be read: {source}")]
    RuleTomlUnreadable {
        slug: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{slug}/{RULE_TOML} is not valid: {source}")]
    RuleTomlInvalid {
        slug: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("{slug}/{CHECK_JS} could not be read: {source}")]
    CheckJsMissing {
        slug: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{slug}/{FIX_JS} could not be read: {source}")]
    FixJsUnreadable {
        slug: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{slug}/{CHECK_JS} does not parse: {source}")]
    CheckJsInvalid {
        slug: String,
        #[source]
        source: BrokenRule,
    },

    #[error(
        "{slug}/{CHECK_JS} declares `requires = {requires:?}`, but a file-loaded rule has no \
         facts system to resolve them against -- an extrinsic rule cannot be loaded from a \
         directory"
    )]
    RuleRequiresFacts { slug: String, requires: Vec<String> },

    #[error(
        "{slug}/{RULE_TOML} and {first_slug}/{RULE_TOML} both declare id {id:?}; \
         every rule's id must be unique"
    )]
    DuplicateId {
        id: String,
        first_slug: String,
        slug: String,
    },
}

/// One rule's files, read from wherever the host keeps them: a rules
/// directory ([`load_rules_dir`]), or data compiled into the binary.
#[derive(Debug, Clone)]
pub struct RuleFiles {
    /// The rule's directory name, which orders the rules and names them in errors.
    pub slug: String,
    /// The text of `rule.toml`.
    pub rule_toml: String,
    /// The text of `check.js`.
    pub check_js: String,
    /// The text of `fix.js`, if the rule has one.
    pub fix_js: Option<String>,
}

/// Reads one [`RuleForCheck`] per subdirectory of `dir`, sorted by
/// directory name (the "slug") for a deterministic, diffable order.
///
/// A non-directory entry directly under `dir` (a stray `README.md`, a
/// `.DS_Store`) is not a rule and is skipped without ceremony; everything
/// under a subdirectory that *is* treated as a rule is checked in full, and
/// any problem there is a hard error -- see [`RulesDirError`].
pub fn load_rules_dir(dir: &Path) -> Result<Vec<RuleForCheck>, RulesDirError> {
    let entries = std::fs::read_dir(dir).map_err(|source| RulesDirError::RulesDirUnreadable {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| RulesDirError::RulesDirUnreadable {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let slug = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| RulesDirError::SlugNotUtf8 { path: path.clone() })?
            .to_owned();
        files.push(read_rule_files(slug, &path)?);
    }
    load_rules(files)
}

/// The rules in `files`, sorted by slug, each checked in full: see
/// [`RulesDirError`] for everything that is refused.
pub fn load_rules(mut files: Vec<RuleFiles>) -> Result<Vec<RuleForCheck>, RulesDirError> {
    files.sort_by(|a, b| a.slug.cmp(&b.slug));

    let budget = ScriptBudget::default();
    let mut seen_ids: HashMap<RuleId, String> = HashMap::new();
    let mut rules = Vec::with_capacity(files.len());

    for files in files {
        let (id, rule) = parse_rule(&files, &budget)?;
        if let Some(first_slug) = seen_ids.get(&rule.id) {
            return Err(RulesDirError::DuplicateId {
                id,
                first_slug: first_slug.clone(),
                slug: files.slug,
            });
        }
        seen_ids.insert(rule.id, files.slug);
        rules.push(rule);
    }

    Ok(rules)
}

fn read_rule_files(slug: String, dir: &Path) -> Result<RuleFiles, RulesDirError> {
    let rule_toml = std::fs::read_to_string(dir.join(RULE_TOML)).map_err(|source| {
        RulesDirError::RuleTomlUnreadable {
            slug: slug.clone(),
            source,
        }
    })?;
    let check_js = std::fs::read_to_string(dir.join(CHECK_JS)).map_err(|source| {
        RulesDirError::CheckJsMissing {
            slug: slug.clone(),
            source,
        }
    })?;
    let fix_js = match std::fs::read_to_string(dir.join(FIX_JS)) {
        Ok(text) => Some(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(RulesDirError::FixJsUnreadable { slug, source }),
    };
    Ok(RuleFiles {
        slug,
        rule_toml,
        check_js,
        fix_js,
    })
}

/// One rule, with the `id` string it was derived from (for a duplicate's error).
fn parse_rule(
    files: &RuleFiles,
    budget: &ScriptBudget,
) -> Result<(String, RuleForCheck), RulesDirError> {
    let slug = &files.slug;
    let rule_toml: RuleToml =
        toml::from_str(&files.rule_toml).map_err(|source| RulesDirError::RuleTomlInvalid {
            slug: slug.clone(),
            source,
        })?;

    let requires = read_requires(&files.check_js, budget).map_err(|source| {
        RulesDirError::CheckJsInvalid {
            slug: slug.clone(),
            source,
        }
    })?;
    if !requires.is_empty() {
        return Err(RulesDirError::RuleRequiresFacts {
            slug: slug.clone(),
            requires,
        });
    }

    let rule = RuleForCheck {
        id: derive_rule_id(&rule_toml.id),
        title: rule_toml.title,
        description_md: rule_toml.description,
        script_js: files.check_js.clone(),
        fix_js: files.fix_js.clone(),
        facts: u2s_facts::FactsForCheck::Ready(serde_json::Map::new()),
    };
    Ok((rule_toml.id, rule))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_rule(
        root: &Path,
        slug: &str,
        id: &str,
        title: &str,
        description: &str,
        check_js: &str,
        fix_js: Option<&str>,
    ) {
        let dir = root.join(slug);
        fs::create_dir_all(&dir).expect("create rule dir");
        fs::write(
            dir.join(RULE_TOML),
            format!(
                "id = {id:?}\ntitle = {title:?}\ndescription = {description:?}\n\
                 output_formats = [\"aem\"]\n"
            ),
        )
        .expect("write rule.toml");
        fs::write(dir.join(CHECK_JS), check_js).expect("write check.js");
        if let Some(fix) = fix_js {
            fs::write(dir.join(FIX_JS), fix).expect("write fix.js");
        }
    }

    const ALWAYS_POSITIVE: &str =
        "function check(output, ctx) { return { pass: true, violations: [] }; }";

    #[test]
    fn loads_every_rule_sorted_by_slug() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "zzz-last",
            "zzz-last",
            "Last",
            "last rule",
            ALWAYS_POSITIVE,
            None,
        );
        write_rule(
            tmp.path(),
            "aaa-first",
            "aaa-first",
            "First",
            "first rule",
            ALWAYS_POSITIVE,
            None,
        );

        let rules = load_rules_dir(tmp.path()).expect("load");
        let titles: Vec<&str> = rules.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["First", "Last"]);
    }

    #[test]
    fn a_loaded_rule_carries_no_script_and_ready_empty_facts() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "no-title",
            "no-title",
            "No title",
            "d",
            ALWAYS_POSITIVE,
            Some("function fix(output, ctx) { return []; }"),
        );

        let rules = load_rules_dir(tmp.path()).expect("load");
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!(rule.title, "No title");
        assert_eq!(rule.description_md, "d");
        assert!(rule.fix_js.is_some());
        assert_eq!(
            rule.facts,
            u2s_facts::FactsForCheck::Ready(serde_json::Map::new())
        );
    }

    #[test]
    fn the_same_id_string_always_derives_the_same_rule_id() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "some-slug",
            "stable-id",
            "T",
            "d",
            ALWAYS_POSITIVE,
            None,
        );
        let first = load_rules_dir(tmp.path()).expect("load")[0].id;

        // Renaming the directory must not change the derived id, since it is
        // read from `rule.toml`'s own `id` field.
        fs::rename(tmp.path().join("some-slug"), tmp.path().join("renamed")).expect("rename");
        let second = load_rules_dir(tmp.path()).expect("load")[0].id;

        assert_eq!(first, second);
    }

    #[test]
    fn missing_check_js_is_a_hard_error() {
        let tmp = tempdir();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(
            dir.join(RULE_TOML),
            "id = \"broken\"\ntitle = \"T\"\ndescription = \"d\"\n",
        )
        .expect("write rule.toml");

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(matches!(err, RulesDirError::CheckJsMissing { .. }), "{err}");
    }

    #[test]
    fn an_unreadable_rule_toml_is_a_hard_error() {
        let tmp = tempdir();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join(CHECK_JS), ALWAYS_POSITIVE).expect("write check.js");
        // no rule.toml at all

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(
            matches!(err, RulesDirError::RuleTomlUnreadable { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_invalid_rule_toml_is_a_hard_error() {
        let tmp = tempdir();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join(RULE_TOML), "this is not valid toml {{{").expect("write rule.toml");
        fs::write(dir.join(CHECK_JS), ALWAYS_POSITIVE).expect("write check.js");

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(
            matches!(err, RulesDirError::RuleTomlInvalid { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_rule_toml_missing_a_required_field_is_a_hard_error() {
        let tmp = tempdir();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).expect("create dir");
        // no `description`
        fs::write(dir.join(RULE_TOML), "id = \"broken\"\ntitle = \"T\"\n")
            .expect("write rule.toml");
        fs::write(dir.join(CHECK_JS), ALWAYS_POSITIVE).expect("write check.js");

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(
            matches!(err, RulesDirError::RuleTomlInvalid { .. }),
            "{err}"
        );
    }

    /// A misspelled key would otherwise be ignored: `output_format` for
    /// `output_formats` quietly leaves the rule unscoped.
    #[test]
    fn an_unknown_rule_toml_key_is_a_hard_error() {
        let tmp = tempdir();
        let dir = tmp.path().join("typo");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(
            dir.join(RULE_TOML),
            "id = \"typo\"\ntitle = \"T\"\ndescription = \"D\"\noutput_format = [\"aem\"]\n",
        )
        .expect("write rule.toml");
        fs::write(dir.join(CHECK_JS), ALWAYS_POSITIVE).expect("write check.js");

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(
            matches!(err, RulesDirError::RuleTomlInvalid { .. }),
            "{err}"
        );
    }

    #[test]
    fn duplicate_ids_are_a_hard_error() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "first-dir",
            "same-id",
            "First",
            "d",
            ALWAYS_POSITIVE,
            None,
        );
        write_rule(
            tmp.path(),
            "second-dir",
            "same-id",
            "Second",
            "d",
            ALWAYS_POSITIVE,
            None,
        );

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(matches!(err, RulesDirError::DuplicateId { .. }), "{err}");
    }

    #[test]
    fn a_script_that_fails_to_parse_is_a_hard_error() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "broken-script",
            "broken-script",
            "T",
            "d",
            "this is not valid javascript {{{",
            None,
        );

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        assert!(matches!(err, RulesDirError::CheckJsInvalid { .. }), "{err}");
    }

    #[test]
    fn a_script_that_declares_requires_is_refused() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "extrinsic",
            "extrinsic",
            "T",
            "d",
            r#"
                const requires = ["source_fields"];
                function check(output, ctx) { return { pass: true, violations: [] }; }
            "#,
            None,
        );

        let err = load_rules_dir(tmp.path()).expect_err("must fail");
        match err {
            RulesDirError::RuleRequiresFacts { requires, .. } => {
                assert_eq!(requires, vec!["source_fields".to_owned()]);
            }
            other => panic!("expected RuleRequiresFacts, got {other}"),
        }
    }

    #[test]
    fn an_empty_directory_loads_no_rules() {
        let tmp = tempdir();
        let rules = load_rules_dir(tmp.path()).expect("load");
        assert!(rules.is_empty());
    }

    #[test]
    fn a_stray_non_directory_entry_is_not_a_rule() {
        let tmp = tempdir();
        fs::write(tmp.path().join("README.md"), "not a rule").expect("write");
        write_rule(
            tmp.path(),
            "actual-rule",
            "actual-rule",
            "T",
            "d",
            ALWAYS_POSITIVE,
            None,
        );

        let rules = load_rules_dir(tmp.path()).expect("load");
        assert_eq!(rules.len(), 1);
    }

    /// A loaded rule is checked through the exact path a real conversion
    /// uses -- `crate::native::check_rules` over a real
    /// [`u2s_rules_host::runner::RuleRunner`] -- to prove the loader
    /// produces a [`RuleForCheck`] that is not merely well-formed data but
    /// actually runnable.
    #[tokio::test]
    async fn a_loaded_rules_check_runs_through_check_rules_with_a_real_runner() {
        let tmp = tempdir();
        write_rule(
            tmp.path(),
            "reject-empty-title",
            "reject-empty-title",
            "Title required",
            "d",
            r#"
                function check(output, ctx) {
                    if (!output.title) {
                        return { pass: false, violations: [{ pointer: "", message: "title required" }] };
                    }
                    return { pass: true, violations: [] };
                }
            "#,
            None,
        );
        let rules = load_rules_dir(tmp.path()).expect("load");

        let runner = u2s_rules_host::runner::test_support::runner();
        let outcome = crate::native::check_rules(
            &serde_json::json!({}),
            &serde_json::json!({}),
            Some(&serde_json::json!({})),
            &rules,
            &runner,
        )
        .await
        .expect("check_rules");

        let verdicts = outcome.value["verdicts"].as_array().expect("an array");
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0]["verdict"], "negative");
    }

    /// A tiny stand-in for `tempfile::TempDir`: this crate has no dependency
    /// on it, and one directory removed on drop is all these tests need.
    struct TempDir(PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "u2s-doc-tools-rules-dir-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
}
