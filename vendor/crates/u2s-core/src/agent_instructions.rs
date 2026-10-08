//! [`AgentInstructions`]: a dataset's own text, appended to one agent's
//! built-in system prompt. The one place untrusted text from the settings
//! page or API becomes a value that cannot be blank or over-long.
//!
//! The bounds match the `dataset_settings_instructions_bounded` `CHECK`
//! constraint in `migrations/20261006120000_dataset_agent_instructions.sql`
//! exactly. Keep the two in sync if either changes.

use std::fmt;

/// The longest instructions text accepted, in characters (Postgres
/// `char_length`, Rust `chars().count()`).
pub const MAX_AGENT_INSTRUCTIONS_CHARS: usize = 20_000;

/// Text that failed the bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentInstructionsError {
    #[error("must not be blank")]
    Blank,
    #[error("must be at most {MAX_AGENT_INSTRUCTIONS_CHARS} characters, not {0}")]
    TooLong(usize),
}

/// Non-blank, trimmed, LF-only text of at most
/// [`MAX_AGENT_INSTRUCTIONS_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInstructions(String);

impl AgentInstructions {
    /// Normalises CRLF (what a browser textarea submits) to LF, trims, then
    /// checks the bounds.
    pub fn parse(raw: &str) -> Result<Self, AgentInstructionsError> {
        let normalized = raw.replace("\r\n", "\n");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            return Err(AgentInstructionsError::Blank);
        }
        let chars = trimmed.chars().count();
        if chars > MAX_AGENT_INSTRUCTIONS_CHARS {
            return Err(AgentInstructionsError::TooLong(chars));
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentInstructions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_normalises_line_endings() {
        let parsed =
            AgentInstructions::parse("  \r\nUse German labels.\r\nKeep ids.\r\n ").expect("valid");
        assert_eq!(parsed.as_str(), "Use German labels.\nKeep ids.");
    }

    #[test]
    fn refuses_blank() {
        assert_eq!(
            AgentInstructions::parse(" \r\n\t "),
            Err(AgentInstructionsError::Blank)
        );
    }

    #[test]
    fn bounds_are_in_characters() {
        let at_limit = "ä".repeat(MAX_AGENT_INSTRUCTIONS_CHARS);
        assert!(AgentInstructions::parse(&at_limit).is_ok());
        let over = "ä".repeat(MAX_AGENT_INSTRUCTIONS_CHARS + 1);
        assert_eq!(
            AgentInstructions::parse(&over),
            Err(AgentInstructionsError::TooLong(
                MAX_AGENT_INSTRUCTIONS_CHARS + 1
            ))
        );
    }
}
