//! Assembling a finished run's artefacts from the agent's working state.
//!
//! Split from the UI so the target-dependent rule — *the artefact that ships is
//! the one the agent actually worked on* — is a pure function with tests, and so
//! the MCP server can reach the same exports the desktop app offers.

use blueprint::{DocumentEnvelope, OutputTarget};

use crate::ConversionAgent;

/// The artefacts a finished run produces.
pub struct Outputs {
    pub envelope: DocumentEnvelope,
    pub redacto_sql: Option<String>,
    /// Human-readable notes about anything the run could not produce.
    pub warnings: Vec<String>,
}

/// Assemble the run's artefacts from the agent's working trees.
pub fn build(agent: &mut ConversionAgent, profile: Option<&str>) -> Outputs {
    let mut warnings: Vec<String> = Vec::new();

    match agent.target() {
        OutputTarget::Redacto => {
            // The authored structured tree IS the document. Never fall back to
            // `structured_from_aem_tree` here: that conversion drops every
            // non-master language onto a `default` pseudo-language and strips
            // the inline markup, which would turn a loud failure into a dump
            // that imports a multilingual document as one fake locale.
            let content = agent.structured().to_vec();
            let master = redacto_master_language(agent, profile);
            let context = agent.source_context(&master);

            let envelope = DocumentEnvelope {
                context,
                content,
                state_count: 1,
            };

            // Prefer the dump the Author last built and validated, so the SQL
            // that ships is the SQL that was reviewed.
            let redacto_sql = agent
                .redacto_dump()
                .filter(|dump| !dump.assets.is_empty())
                .map(|dump| dump.to_sql())
                .or_else(|| redacto_sql_for(&envelope, profile));

            if redacto_sql.is_none() {
                warnings.push(if envelope.content.is_empty() {
                    "No Redacto dump: the agent did not author any content.".to_string()
                } else {
                    "No Redacto dump: the authored document produced no text assets.".to_string()
                });
            }

            Outputs {
                envelope,
                redacto_sql,
                warnings,
            }
        }
        OutputTarget::Aem => {
            // The agent authors the AEM tree directly and leaves its structured
            // tree empty, so lift the authored tree back into structured content
            // — otherwise both editors open on an empty document.
            let aem_translated = agent.aem_translated().cloned();
            let mut content = agent.structured().to_vec();
            if content.is_empty()
                && let Some(tree) = &aem_translated
            {
                content = crate::session::structured_from_aem_tree(tree, profile);
            }

            // No Redacto dump here. An AEM run's result panel does not offer one,
            // and deriving it from the source ran a full extraction and dump
            // generation for a file nobody could reach. Convert with the Redacto
            // target to get one.
            Outputs {
                envelope: DocumentEnvelope {
                    context: agent.context().clone(),
                    content,
                    state_count: 1,
                },
                redacto_sql: None,
                warnings,
            }
        }
    }
}

/// The language a Redacto document is written in.
///
/// Each language variant carries its own page header and footer variables, so
/// the output asks for the master language's context rather than taking
/// whichever variant happened to be uploaded first.
pub fn redacto_master_language(agent: &ConversionAgent, profile: Option<&str>) -> String {
    profile
        .and_then(blueprint::redacto_master_language)
        .unwrap_or_else(|| agent.context().language().to_string())
}

/// Generate the XSD schema for `envelope`, if the profile has an `xsd/` section
/// and the document has content.
///
/// `form_code` is the code the AEM package was built with, so the schema names
/// the same form; pass `None` to leave the profile default in place.
pub fn xsd_schema_for(
    envelope: &DocumentEnvelope,
    profile: Option<&str>,
    form_code: Option<&str>,
) -> Option<String> {
    let profile_name = profile?;
    if envelope.content.is_empty() || !blueprint::has_xsd_config(profile_name) {
        return None;
    }
    let mut config = blueprint::load_xsd_config(profile_name).ok()?;
    if let Some(code) = form_code {
        config.form_code = Some(code.to_string());
    }
    // Generated off the AEM tree, so the downloadable schema is the same one the
    // package bundles and matches the form's bindRefs.
    let aem_config = blueprint::load_aem_config(profile_name, &envelope.context).ok()?;
    Some(blueprint::to_xsd(&envelope.content, &aem_config, &config))
}

/// Generate the Redacto PostgreSQL dump for `envelope`, if the profile has a
/// `redacto/config.toml`, its templates resolve against the document, and the
/// document actually has content to emit.
///
/// Returns `None` when the profile has no Redacto section, the document does
/// not carry the XFA variables the profile's identity templates need, or the
/// dump would contain no text assets at all. That last case is the important
/// one: a contentless dump is still valid SQL describing an empty document, so
/// without this guard it reaches the download button looking like a result.
pub fn redacto_sql_for(envelope: &DocumentEnvelope, profile: Option<&str>) -> Option<String> {
    let profile_name = profile?;
    // The envelope carries one merged context, so the page furniture is
    // rendered from it for every language. The agent's `build_redacto` has the
    // per-language contexts and does better; this is the fallback path.
    let (dump, _) = blueprint::to_redacto_dump_for_profile(
        profile_name,
        std::slice::from_ref(&envelope.context),
        &envelope.content,
    )
    .ok()?;
    if dump.is_empty_document() {
        return None;
    }
    Some(dump.to_sql())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blueprint::structured::{ParagraphNode, StructuredNode, TranslatedText};

    fn envelope_with(variables: &[(&str, &str)]) -> DocumentEnvelope {
        let vars = variables
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        DocumentEnvelope {
            context: blueprint::Context::new("de".into(), vars),
            content: vec![StructuredNode::Paragraph(ParagraphNode {
                content: TranslatedText::plain("Ein Textabschnitt."),
                som_path: None,
                source_name: None,
            })],
            state_count: 1,
        }
    }

    /// The download button in both UIs is driven purely by the assembled
    /// `redacto_sql`, so this is the whole data path.
    #[test]
    fn redacto_sql_is_generated_for_the_ubs_profile() {
        let envelope = envelope_with(&[("formrange_code", "AAAD"), ("formrange_entity", "001")]);

        let sql = redacto_sql_for(&envelope, Some("ubs")).expect("ubs profile yields a dump");

        assert!(sql.contains("INSERT INTO app_redacto.documents "), "{sql}");
        assert!(sql.contains("'aaad_001'"), "{sql}");
        assert!(sql.contains("<p>Ein Textabschnitt.</p>"), "{sql}");
    }

    #[test]
    fn redacto_sql_is_absent_without_a_profile_or_config() {
        let envelope = envelope_with(&[("formrange_code", "AAAD"), ("formrange_entity", "001")]);

        assert!(redacto_sql_for(&envelope, None).is_none());
        assert!(redacto_sql_for(&envelope, Some("missing-profile")).is_none());
    }

    /// A document without the XFA variables the profile's identity templates
    /// need must skip the dump, not abort the conversion.
    #[test]
    fn redacto_sql_is_skipped_when_identity_variables_are_missing() {
        assert!(redacto_sql_for(&envelope_with(&[]), Some("ubs")).is_none());
    }

    /// Regression: an empty structured tree used to yield a syntactically valid
    /// dump with a `documents` row and no assets at all, which reached the
    /// download button as an importable file describing an empty document. A
    /// dump with no content is a failure, not an output.
    #[test]
    fn redacto_sql_for_empty_content_returns_none() {
        let mut envelope =
            envelope_with(&[("formrange_code", "AAAD"), ("formrange_entity", "001")]);
        envelope.content = Vec::new();

        assert!(
            redacto_sql_for(&envelope, Some("ubs")).is_none(),
            "a dump with no content assets must not be offered for download"
        );
    }

    #[test]
    fn xsd_schema_uses_the_supplied_form_code() {
        let envelope = envelope_with(&[("formrange_code", "AAAD"), ("formrange_entity", "001")]);

        let xsd = xsd_schema_for(&envelope, Some("ubs"), Some("AAAD_001"))
            .expect("ubs profile yields a schema");

        assert!(xsd.contains("<?xml"), "{xsd}");
        assert!(xsd.contains("AAAD_001"), "{xsd}");
    }

    #[test]
    fn xsd_schema_is_absent_without_a_profile_or_content() {
        let mut envelope =
            envelope_with(&[("formrange_code", "AAAD"), ("formrange_entity", "001")]);

        assert!(xsd_schema_for(&envelope, None, None).is_none());
        assert!(xsd_schema_for(&envelope, Some("missing-profile"), None).is_none());

        envelope.content = Vec::new();
        assert!(
            xsd_schema_for(&envelope, Some("ubs"), None).is_none(),
            "an empty document must not be offered as a schema"
        );
    }

    /// The same guard for a document whose content carries no *renderable*
    /// blocks: input fields alone produce zero assets, which is the shape a
    /// non-text-only document takes.
    #[test]
    fn redacto_sql_for_field_only_content_returns_none() {
        use blueprint::structured::{FieldNode, FieldType};

        let mut envelope =
            envelope_with(&[("formrange_code", "AAAD"), ("formrange_entity", "001")]);
        envelope.content = vec![StructuredNode::Field(FieldNode {
            name: "first_name".into(),
            som_path: None,
            label: Some(TranslatedText::plain("First name")),
            input_type: FieldType::Text {
                regex: None,
                max_length: None,
                min_length: None,
            },
            value: None,
            placeholder: None,
            required: false,
        })];

        assert!(redacto_sql_for(&envelope, Some("ubs")).is_none());
    }
}
