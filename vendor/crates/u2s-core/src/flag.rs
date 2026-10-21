//! [`FlagReason`]: why the Reviewer flagged a conversion for a person to
//! look at. The one place the model's free text becomes a value that cannot
//! be blank or over-long.
//!
//! The bounds match the `runs_flag_reason_bounded` `CHECK` constraint in
//! `migrations/20261017090000_run_flagged.sql` exactly. Keep the two in sync
//! if either changes.

use std::fmt;

/// The longest reason accepted, in characters (Postgres `char_length`,
/// Rust `chars().count()`).
pub const MAX_FLAG_REASON_CHARS: usize = 2_000;

/// A reason that failed the bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlagReasonError {
    #[error("the reason must not be blank")]
    Blank,
    #[error("the reason must be at most {MAX_FLAG_REASON_CHARS} characters, not {0}")]
    TooLong(usize),
}

/// Non-blank, trimmed text of at most [`MAX_FLAG_REASON_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagReason(String);

impl FlagReason {
    pub fn parse(raw: &str) -> Result<Self, FlagReasonError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(FlagReasonError::Blank);
        }
        let chars = trimmed.chars().count();
        if chars > MAX_FLAG_REASON_CHARS {
            return Err(FlagReasonError::TooLong(chars));
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FlagReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_a_reason() {
        assert_eq!(
            FlagReason::parse("  expected a letter, found an invoice \n")
                .unwrap()
                .as_str(),
            "expected a letter, found an invoice"
        );
    }

    #[test]
    fn refuses_blank() {
        assert_eq!(FlagReason::parse(" \n\t "), Err(FlagReasonError::Blank));
    }

    #[test]
    fn bounds_are_in_characters() {
        assert!(FlagReason::parse(&"ä".repeat(MAX_FLAG_REASON_CHARS)).is_ok());
        assert_eq!(
            FlagReason::parse(&"ä".repeat(MAX_FLAG_REASON_CHARS + 1)),
            Err(FlagReasonError::TooLong(MAX_FLAG_REASON_CHARS + 1))
        );
    }
}
