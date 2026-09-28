//! The vendored UBS XFA test corpus.
//!
//! `#[doc(hidden)]` for the same reason as [`crate::fonts::test_support`]: this
//! is test infrastructure other crates' integration tests need, not part of
//! the library's public surface.
#[doc(hidden)]
pub mod test_support {
    use std::path::PathBuf;

    /// `corpus/ubs/` at the workspace root — resolved against
    /// `CARGO_MANIFEST_DIR`, since a test's working directory is the crate
    /// root, not the workspace root.
    pub fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ubs")
    }

    /// One vendored form, e.g. `form("AAAA_019_DE.pdf")`.
    ///
    /// Panics naming the file: the corpus is committed, so a missing entry
    /// means a broken checkout, not a reason to skip.
    pub fn form(name: &str) -> PathBuf {
        let path = dir().join(name);
        assert!(
            path.is_file(),
            "vendored UBS corpus form {name} not found at {} — broken checkout? \
             see corpus/ubs/README.md",
            path.display()
        );
        path
    }
}
