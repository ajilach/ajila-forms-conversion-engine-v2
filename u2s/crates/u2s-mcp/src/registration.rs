//! **Registering a stdio server is arbitrary command execution on the u2s
//! host — treat it as such.** [`validate_stdio_command`] is the one control
//! that matters for that (PLAN.md): the command must resolve inside an
//! operator-owned directory, so the registration API can start vetted
//! servers but cannot be used to introduce a new executable.
//!
//! Registration-as-an-API-call being platform-admin-only is an authorization
//! check that belongs in `u2s-server` (it needs `Authorized<PlatformAdmin>`,
//! which this crate cannot depend on without pulling `u2s-auth`'s Keycloak
//! stack into every MCP client). This module is the other half: the checks
//! that hold even if the authorization check is ever satisfied by an actor
//! who should not have been platform admin.
//!
//! There are two of them, because confining the *command* is not enough on
//! its own. [`validate_stdio_env`] confines the environment: `LD_PRELOAD`
//! and its relatives are read by the dynamic loader **before the vetted
//! binary's own `main` runs**, so a registration free to set them can
//! execute arbitrary code through a command that passed
//! [`validate_stdio_command`] cleanly. Confining one without the other
//! would have left the module doc above technically true and practically
//! worthless.
//!
//! `args` are deliberately *not* restricted: they reach a vetted,
//! operator-owned binary that decides what to do with them, which is a
//! different trust question from the loader acting before that binary has
//! any say.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("{command}: does not resolve to a file ({source})")]
    NotFound {
        command: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{bin_dir}: does not exist or is not a directory ({source})")]
    BinDirUnusable {
        bin_dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{command}: resolves to {resolved}, which is outside {bin_dir} — refused")]
    OutsideBinDir {
        command: PathBuf,
        resolved: PathBuf,
        bin_dir: PathBuf,
    },
    #[error(
        "environment variable {name:?} is not allowed for a registered server: only \
         U2S_*, RUST_LOG, RUST_BACKTRACE and the renderers' own cache/iteration \
         settings may be set. Loader variables such as LD_PRELOAD would run code \
         before the vetted binary does, which is exactly what the bin-dir check exists \
         to prevent."
    )]
    EnvNotAllowed { name: String },
}

/// Environment variables a registered stdio server may be given.
///
/// An allowlist, not a denylist of `LD_*`/`DYLD_*`: the loader-variable
/// family differs per platform and grows, so enumerating what is *safe*
/// fails closed when it is incomplete, while enumerating what is dangerous
/// fails open. These are the names this workspace's own servers actually
/// read (`U2S_FONT_DIR`, `U2S_FONT_FALLBACK`, the two cache sizes,
/// `XFA_MAX_CALC_ITERATIONS`) plus the usual tracing controls.
const ALLOWED_ENV_EXACT: &[&str] = &[
    "RUST_LOG",
    "RUST_BACKTRACE",
    "DOC_CACHE_SIZE",
    "RASTER_CACHE_SIZE",
    "XFA_MAX_CALC_ITERATIONS",
];

/// Confirms every variable a registration wants to set is one a server is
/// allowed to be given. See this module's own docs for why this exists
/// beside [`validate_stdio_command`] rather than being left to the
/// authorization check.
pub fn validate_stdio_env(env: &[(String, String)]) -> Result<(), RegistrationError> {
    for (name, _) in env {
        let allowed =
            name.starts_with("U2S_") || ALLOWED_ENV_EXACT.contains(&name.as_str());
        if !allowed {
            return Err(RegistrationError::EnvNotAllowed { name: name.clone() });
        }
    }
    Ok(())
}

/// Canonicalizes `command` and `bin_dir` and confirms the former resolves
/// inside the latter, returning the canonical path to actually spawn.
///
/// Canonicalizing before comparing is what makes this a real control rather
/// than a string check: it resolves `..`, symlinks and relative components on
/// both sides, so `U2S_MCP_BIN_DIR/../../etc/evil` or a symlink planted
/// inside the bin dir that points outside it cannot slip through a prefix
/// check done on the raw strings.
pub fn validate_stdio_command(
    command: &Path,
    bin_dir: &Path,
) -> Result<PathBuf, RegistrationError> {
    let bin_dir = bin_dir
        .canonicalize()
        .map_err(|source| RegistrationError::BinDirUnusable {
            bin_dir: bin_dir.to_path_buf(),
            source,
        })?;
    let resolved = command
        .canonicalize()
        .map_err(|source| RegistrationError::NotFound {
            command: command.to_path_buf(),
            source,
        })?;

    if resolved.starts_with(&bin_dir) {
        Ok(resolved)
    } else {
        Err(RegistrationError::OutsideBinDir {
            command: command.to_path_buf(),
            resolved,
            bin_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch layout: `<root>/bin/` (the vetted directory) and
    /// `<root>/outside/` (anywhere else), each with one executable-shaped
    /// file. Execute bits are irrelevant here — this function only checks
    /// paths, never spawns anything.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "u2s-mcp-registration-test-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("bin")).expect("create bin");
            std::fs::create_dir_all(root.join("outside")).expect("create outside");
            std::fs::write(root.join("bin/good-server"), b"").expect("write");
            std::fs::write(root.join("outside/evil-server"), b"").expect("write");
            Self { root }
        }

        fn bin_dir(&self) -> PathBuf {
            self.root.join("bin")
        }
    }

    #[test]
    fn a_command_inside_the_bin_dir_is_accepted() {
        let s = Scratch::new("inside");
        let resolved = validate_stdio_command(&s.bin_dir().join("good-server"), &s.bin_dir())
            .expect("must be accepted");
        assert_eq!(
            resolved,
            s.bin_dir().join("good-server").canonicalize().unwrap()
        );
    }

    #[test]
    fn a_command_outside_the_bin_dir_is_refused() {
        let s = Scratch::new("outside");
        let outside = s.root.join("outside/evil-server");
        let err = validate_stdio_command(&outside, &s.bin_dir()).expect_err("must be refused");
        assert!(matches!(err, RegistrationError::OutsideBinDir { .. }));
    }

    #[test]
    fn a_traversal_out_of_the_bin_dir_is_refused() {
        let s = Scratch::new("traversal");
        let traversal = s.bin_dir().join("../outside/evil-server");
        let err = validate_stdio_command(&traversal, &s.bin_dir()).expect_err("must be refused");
        assert!(matches!(err, RegistrationError::OutsideBinDir { .. }));
    }

    #[test]
    fn a_symlink_inside_the_bin_dir_pointing_outside_is_refused() {
        let s = Scratch::new("symlink");
        let link = s.bin_dir().join("sneaky-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(s.root.join("outside/evil-server"), &link).expect("symlink");
        #[cfg(unix)]
        {
            let err = validate_stdio_command(&link, &s.bin_dir()).expect_err("must be refused");
            assert!(matches!(err, RegistrationError::OutsideBinDir { .. }));
        }
    }

    #[test]
    fn a_nonexistent_command_is_not_found_rather_than_silently_outside() {
        let s = Scratch::new("missing");
        let err = validate_stdio_command(&s.bin_dir().join("no-such-file"), &s.bin_dir())
            .expect_err("must fail");
        assert!(matches!(err, RegistrationError::NotFound { .. }));
    }

    #[test]
    fn an_unusable_bin_dir_is_its_own_error() {
        let s = Scratch::new("badbindir");
        let err = validate_stdio_command(
            &s.bin_dir().join("good-server"),
            &s.root.join("does-not-exist"),
        )
        .expect_err("must fail");
        assert!(matches!(err, RegistrationError::BinDirUnusable { .. }));
    }

    #[test]
    fn the_servers_own_settings_are_allowed() {
        validate_stdio_env(&[
            ("U2S_FONT_DIR".to_owned(), "/opt/fonts".to_owned()),
            ("U2S_FONT_FALLBACK".to_owned(), "DejaVuSans".to_owned()),
            ("RUST_LOG".to_owned(), "info".to_owned()),
            ("DOC_CACHE_SIZE".to_owned(), "8".to_owned()),
            ("XFA_MAX_CALC_ITERATIONS".to_owned(), "100".to_owned()),
        ])
        .expect("the settings the workspace's own servers read must be settable");
    }

    /// The hole this check closes: `validate_stdio_command` confines the
    /// binary, and the loader would then run someone else's code inside it
    /// before that binary's `main` is ever reached.
    #[test]
    fn loader_variables_are_refused() {
        for name in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
            "PATH",
        ] {
            let err = validate_stdio_env(&[(name.to_owned(), "/tmp/evil".to_owned())])
                .expect_err("must be refused");
            match err {
                RegistrationError::EnvNotAllowed { name: refused } => {
                    assert_eq!(refused, name)
                }
                other => panic!("expected EnvNotAllowed for {name}, got {other:?}"),
            }
        }
    }

    /// Failing closed is the point of the allowlist: something nobody has
    /// considered yet is refused rather than waved through.
    #[test]
    fn an_unrecognised_variable_is_refused_rather_than_assumed_harmless() {
        assert!(
            validate_stdio_env(&[("SOMETHING_NEW".to_owned(), "x".to_owned())]).is_err()
        );
    }

    #[test]
    fn an_empty_environment_is_fine() {
        validate_stdio_env(&[]).expect("setting nothing is always allowed");
    }
}
