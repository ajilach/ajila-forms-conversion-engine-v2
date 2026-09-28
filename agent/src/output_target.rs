//! The output target a conversion produces.
//!
//! A profile can be configured for several outputs at once (`profiles/{name}/`
//! holds an `aem/`, `redacto/`, `xsd/` and `html/` section), but a *run* aims at
//! one of them, and that choice changes what the conversion agent authors: an
//! AEM adaptive-form tree, or the structured document a Redacto dump is built
//! from. Selecting it up front is what keeps the artefact that ships and the
//! artefact the agent worked on the same thing.

use serde::{Deserialize, Serialize};

/// What a conversion run produces.
///
/// An enum rather than a string so the places that branch on it — role/prompt
/// selection and output assembly — are exhaustive `match`es. A mistyped target
/// silently falling through to the AEM path is the failure class this type
/// exists to rule out.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputTarget {
    /// An AEM adaptive form, delivered as a FileVault content package.
    #[default]
    Aem,
    /// A Redacto text document, delivered as a PostgreSQL dump.
    Redacto,
}

impl OutputTarget {
    /// Every target, in the order they should be offered.
    pub const ALL: [OutputTarget; 2] = [OutputTarget::Aem, OutputTarget::Redacto];

    /// The stable machine-readable form, used for persistence and UI values.
    pub fn as_str(self) -> &'static str {
        match self {
            OutputTarget::Aem => "aem",
            OutputTarget::Redacto => "redacto",
        }
    }

    /// The human-readable form, for pickers and status lines.
    pub fn label(self) -> &'static str {
        match self {
            OutputTarget::Aem => "AEM Adaptive Form",
            OutputTarget::Redacto => "Redacto Document",
        }
    }

    /// Parse [`as_str`](Self::as_str) (case-insensitive).
    pub fn parse(value: &str) -> Option<OutputTarget> {
        match value.trim().to_ascii_lowercase().as_str() {
            "aem" => Some(OutputTarget::Aem),
            "redacto" => Some(OutputTarget::Redacto),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_every_target() {
        for target in OutputTarget::ALL {
            assert_eq!(OutputTarget::parse(target.as_str()), Some(target));
        }
    }

    #[test]
    fn parse_is_lenient_about_case_and_padding_and_rejects_the_unknown() {
        assert_eq!(OutputTarget::parse("  ReDaCtO "), Some(OutputTarget::Redacto));
        assert_eq!(OutputTarget::parse("xsd"), None);
        assert_eq!(OutputTarget::parse(""), None);
    }

    /// Every pre-existing run and every caller that has not been taught about
    /// targets yet must keep producing an AEM form.
    #[test]
    fn the_default_is_aem() {
        assert_eq!(OutputTarget::default(), OutputTarget::Aem);
    }
}
