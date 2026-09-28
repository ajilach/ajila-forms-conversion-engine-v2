//! The UBS AEM output format: the authoring model the Conversion Agent edits
//! (`aem::AemNodeTranslated`), its lowering and the UBS templates, the
//! FileVault package writer, the XSD, and the reader for real UBS packages.
//!
//! Moved from `ajilach/ajila-forms-conversion-engine`'s deterministic engine,
//! where these templates were refined against the deployed UBS corpus. The
//! generic `u2s-mapper-aem` stays free of UBS behaviour; converging this
//! writer onto it is future work, held to `tests/fixtures/golden`.

pub mod aem;
pub mod context;
pub mod document;
pub mod profiles;
pub mod template;
mod util;
pub mod value;
pub mod xsd;

pub use document::{UbsAemBuild, UbsAemDocument, decode, encode};

static RULES_DIR: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/rules");

/// The UBS check rules under `rules/`, compiled in, one per rule directory, for
/// [`u2s_doc_tools::rules_dir::load_rules`].
pub fn rule_files() -> Vec<u2s_doc_tools::rules_dir::RuleFiles> {
    RULES_DIR
        .dirs()
        .map(|dir| {
            let slug = dir.path().to_string_lossy().into_owned();
            let text = |name: &str| {
                dir.get_file(dir.path().join(name))
                    .map(|f| f.contents_utf8().expect("a rule file is UTF-8").to_string())
            };
            u2s_doc_tools::rules_dir::RuleFiles {
                rule_toml: text("rule.toml")
                    .unwrap_or_else(|| panic!("rule {slug} has a rule.toml")),
                check_js: text("check.js").unwrap_or_else(|| panic!("rule {slug} has a check.js")),
                fix_js: text("fix.js"),
                slug,
            }
        })
        .collect()
}

/// Errors from building a UBS AEM configuration or rendering its templates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The AEM configuration could not be built (for example a missing form variable).
    #[error("AEM config error: {0}")]
    AemConfig(String),
    /// A profile template string could not be rendered.
    #[error("Template error: {0}")]
    Template(String),
    /// The profile could not be loaded.
    #[error("Profile error: {0}")]
    Profile(String),
    /// A document breaks an invariant the writer relies on.
    #[error("Invalid document: {0}")]
    InvalidDocument(String),
    /// A package could not be read.
    #[error("Package error: {0}")]
    Package(String),
}

/// The JSON Schema of [`UbsAemDocument`], which `json_validate` checks a document
/// against.
pub fn document_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(UbsAemDocument)).expect("the schema serializes")
}
