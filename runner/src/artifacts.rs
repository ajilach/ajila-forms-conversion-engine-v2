//! What a finished run hands over, and what each piece is called.
//!
//! One table, so the file the app drops in Downloads and the file the CLI writes
//! to its output directory cannot end up under different names.

use agent::OutputTarget;
use pipeline::RunOutcome;

/// Build an artefact filename like `forms-package-<code>.zip`, falling back to
/// `forms-package.zip` when the form code is unknown.
pub fn artifact_filename(prefix: &str, form_code: Option<&str>, ext: &str) -> String {
    match form_code {
        Some(code) => format!("{prefix}-{code}.{ext}"),
        None => format!("{prefix}.{ext}"),
    }
}

/// One artefact of a finished run.
///
/// Only the run's own outputs: a consumer's own by-products (a UI transcript,
/// say) are its business and are named on its side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Artifact {
    Package,
    PackageBound,
    RedactoSql,
    Xsd,
}

impl Artifact {
    /// Every artefact, in the order a consumer should offer them.
    pub const ALL: &'static [Artifact] = &[
        Artifact::Package,
        Artifact::PackageBound,
        Artifact::Xsd,
        Artifact::RedactoSql,
    ];

    /// `(filename prefix, extension)`, paired here so a payload cannot end up
    /// under another artefact's name.
    pub fn naming(self) -> (&'static str, &'static str) {
        match self {
            Self::Package => ("forms-package", "zip"),
            Self::PackageBound => ("forms-package-bindrefs", "zip"),
            Self::RedactoSql => ("redacto", "sql"),
            Self::Xsd => ("schema", "xsd"),
        }
    }

    pub fn filename(self, form_code: Option<&str>) -> String {
        let (prefix, ext) = self.naming();
        artifact_filename(prefix, form_code, ext)
    }

    /// Whether this artefact belongs to `target`'s result.
    ///
    /// Presence alone is not the rule: an artefact that only makes sense for the
    /// other target stays hidden even if the run happens to have produced it.
    pub fn belongs_to(self, target: OutputTarget) -> bool {
        match self {
            Self::Package | Self::PackageBound | Self::Xsd => target == OutputTarget::Aem,
            Self::RedactoSql => target == OutputTarget::Redacto,
        }
    }

    /// This artefact's bytes, or `None` if the run did not produce it.
    pub fn bytes_from(self, outcome: &RunOutcome) -> Option<Vec<u8>> {
        match self {
            Self::Package => outcome.aem_package.clone(),
            Self::PackageBound => outcome.aem_package_bound.clone(),
            Self::RedactoSql => outcome.redacto_sql.as_ref().map(|s| s.clone().into_bytes()),
            Self::Xsd => outcome.xsd_schema.as_ref().map(|s| s.clone().into_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_falls_back_when_the_form_code_is_unknown() {
        assert_eq!(
            Artifact::Package.filename(Some("AAEV")),
            "forms-package-AAEV.zip"
        );
        assert_eq!(Artifact::RedactoSql.filename(None), "redacto.sql");
    }

    /// A Redacto run must not offer AEM artefacts, and vice versa — the check
    /// every consumer relies on instead of testing for presence.
    #[test]
    fn each_target_offers_only_its_own_artifacts() {
        let aem: Vec<_> = Artifact::ALL
            .iter()
            .filter(|a| a.belongs_to(OutputTarget::Aem))
            .collect();
        assert_eq!(aem.len(), 3);
        let redacto: Vec<_> = Artifact::ALL
            .iter()
            .filter(|a| a.belongs_to(OutputTarget::Redacto))
            .collect();
        assert_eq!(redacto, vec![&Artifact::RedactoSql]);
    }
}
