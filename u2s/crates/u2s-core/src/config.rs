//! Config-parsing primitives shared by `u2s-auth` and `u2s-server`, so
//! neither duplicates the other's error accumulation or environment
//! abstraction (CLAUDE.md: do not duplicate logic).

use std::collections::BTreeMap;

/// Where config values come from. Implemented for the real process
/// environment and, for tests, a plain map — so a `from_env` parser never
/// touches `std::env` directly and stays a pure, unit-testable function.
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl EnvSource for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        // Inherent `BTreeMap::get` (Q: Borrow<K>, so &str works against a
        // String key) takes priority over this trait method by Rust's
        // method-resolution rules, so this is not infinite recursion.
        self.get(key).cloned()
    }
}

/// Which Cargo build profile produced this binary. A parameter to `from_env`
/// rather than a `cfg!` read inside it, so profile-dependent rules (e.g.
/// "dev auth mode refuses to start under a release profile") are testable
/// from a debug-built test — `cfg!(debug_assertions)` is only a proxy for
/// the profile and does not belong inside the parser itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildProfile {
    Debug,
    Release,
}

impl BuildProfile {
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            BuildProfile::Debug
        } else {
            BuildProfile::Release
        }
    }
}

/// One thing wrong with the configuration. `from_env` implementations
/// accumulate every problem in one pass rather than stopping at the first,
/// so a fresh checkout reports everything wrong at once instead of one
/// missing variable per restart cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigProblem {
    Missing {
        key: &'static str,
    },
    Invalid {
        key: &'static str,
        value_hint: String,
        reason: String,
    },
    /// A value that parses fine but is not allowed in this context — e.g.
    /// `U2S_AUTH_MODE=dev` under a release build.
    Forbidden {
        key: &'static str,
        reason: &'static str,
    },
}

impl std::fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigProblem::Missing { key } => write!(f, "{key}: missing"),
            ConfigProblem::Invalid {
                key,
                value_hint,
                reason,
            } => write!(f, "{key}={value_hint:?}: {reason}"),
            ConfigProblem::Forbidden { key, reason } => {
                write!(f, "{key}: forbidden here: {reason}")
            }
        }
    }
}

/// One or more [`ConfigProblem`]s found while parsing. Never constructed
/// with an empty `problems` list — see [`ConfigError::from_problems`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub problems: Vec<ConfigProblem>,
}

impl ConfigError {
    /// Returns `None` for an empty list, so callers can write
    /// `ConfigError::from_problems(problems).map(Err).unwrap_or(Ok(value))`-
    /// style accumulation without a separate emptiness check.
    pub fn from_problems(problems: Vec<ConfigProblem>) -> Option<Self> {
        if problems.is_empty() {
            None
        } else {
            Some(Self { problems })
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{} configuration problem(s):", self.problems.len())?;
        for problem in &self.problems {
            writeln!(f, "  - {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_env_reads_a_real_variable() {
        // SAFETY: test-only, single-threaded within this test's assertions.
        unsafe {
            std::env::set_var("U2S_CORE_CONFIG_TEST_VAR", "hello");
        }
        assert_eq!(
            ProcessEnv.get("U2S_CORE_CONFIG_TEST_VAR"),
            Some("hello".to_owned())
        );
        unsafe {
            std::env::remove_var("U2S_CORE_CONFIG_TEST_VAR");
        }
    }

    #[test]
    fn map_env_source_looks_up_by_key() {
        let mut env = BTreeMap::new();
        env.insert("FOO".to_owned(), "bar".to_owned());
        assert_eq!(EnvSource::get(&env, "FOO"), Some("bar".to_owned()));
        assert_eq!(EnvSource::get(&env, "MISSING"), None);
    }

    #[test]
    fn from_problems_empty_is_none() {
        assert!(ConfigError::from_problems(vec![]).is_none());
    }

    #[test]
    fn from_problems_nonempty_is_some() {
        let err = ConfigError::from_problems(vec![ConfigProblem::Missing { key: "X" }]);
        assert!(err.is_some());
        assert_eq!(err.unwrap().problems.len(), 1);
    }

    #[test]
    fn display_lists_every_problem() {
        let err = ConfigError {
            problems: vec![
                ConfigProblem::Missing { key: "A" },
                ConfigProblem::Forbidden {
                    key: "B",
                    reason: "not here",
                },
            ],
        };
        let s = err.to_string();
        assert!(s.contains("A: missing"));
        assert!(s.contains("B: forbidden here: not here"));
        assert!(s.contains("2 configuration problem"));
    }
}
