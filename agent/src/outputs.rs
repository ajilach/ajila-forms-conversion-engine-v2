//! Assembling a finished run's artefacts from the agent's working state.
//!
//! Split from the UI so the rule *what ships is the build of the final
//! document* is one function with tests, and so the MCP server can reach the
//! same exports the desktop app offers.

use serde_json::Value;

use crate::{ConversionAgent, OutputTarget};

/// The artefacts a finished run produces.
pub struct Outputs {
    /// The run's final document.
    pub document: Value,
    /// The AEM package, the same form bound to its schema, and the schema.
    pub package: Option<Vec<u8>>,
    pub package_bound: Option<Vec<u8>>,
    pub xsd: Option<String>,
    /// The Redacto dump.
    pub redacto_sql: Option<String>,
    /// Human-readable notes about anything the run could not produce.
    pub warnings: Vec<String>,
}

/// The run's artefacts: the build of its final document. A document edited
/// after its last build is built again here, so what ships is what was
/// authored last; one that does not encode ships nothing, and says why.
pub fn build(agent: &mut ConversionAgent) -> Outputs {
    let mut warnings = Vec::new();
    if let Err(e) = agent.ensure_built() {
        warnings.push(format!("No {} built: {e}", agent.target().label()));
    }
    Outputs {
        document: agent.document().clone(),
        package: agent.package(),
        package_bound: agent.package_bound(),
        xsd: agent.xsd(),
        redacto_sql: agent
            .redacto_dump()
            .map(|dump| String::from_utf8(dump).expect("the dump is UTF-8 SQL")),
        warnings,
    }
    .only_for(agent.target())
}

impl Outputs {
    /// Keep only what `target` produces.
    fn only_for(mut self, target: OutputTarget) -> Self {
        match target {
            OutputTarget::Aem => self.redacto_sql = None,
            OutputTarget::Redacto => {
                self.package = None;
                self.package_bound = None;
                self.xsd = None;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn agent_for(target: OutputTarget) -> ConversionAgent {
        crate::db::claim_scratch_db_for_test();
        let pdf = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../forms/AAEV_019_EN.pdf")).unwrap();
        ConversionAgent::new(
            Some("ubs".into()),
            vec![("AAEV_019_EN.pdf".into(), pdf)],
            format!("test-outputs-{}", uuid::Uuid::new_v4()),
            target,
        )
        .unwrap()
    }

    /// What ships is the authored document, built: a sentence that appears
    /// nowhere in the source reaches the dump only from the document.
    #[test]
    fn redacto_outputs_are_the_build_of_the_authored_document() {
        let mut agent = agent_for(OutputTarget::Redacto);
        let mut doc = agent.document().clone();
        doc["assets"] = json!([{ "key": "marker", "kind": "text", "content": { "en": "<p>AUTHORED-BY-THE-AGENT-MARKER</p>" } }]);
        doc["body"] = json!([{ "type": "assetContainer", "assets": ["marker"] }]);
        doc["sources"]["en"]["header"] = json!("AUTHORED-HEADER");
        agent.seed_document(doc.clone()).unwrap();

        let outputs = build(&mut agent);
        let sql = outputs.redacto_sql.expect("the authored document yields a dump");
        assert!(sql.contains("AUTHORED-BY-THE-AGENT-MARKER"));
        assert!(sql.contains("AUTHORED-HEADER"), "the page header reaches the dump");
        assert_eq!(outputs.document, doc);
        assert!(outputs.package.is_none() && outputs.xsd.is_none());
    }

    /// A document that does not encode ships nothing and says why, rather than
    /// a valid-looking dump describing an empty document.
    #[test]
    fn an_empty_redacto_document_ships_nothing_and_says_why() {
        let mut agent = agent_for(OutputTarget::Redacto);
        let outputs = build(&mut agent);
        assert!(outputs.redacto_sql.is_none());
        assert!(
            outputs.warnings.iter().any(|w| w.contains("No Redacto Document built") && w.contains("body")),
            "{:?}",
            outputs.warnings
        );
    }

    /// An AEM run ships no Redacto dump.
    #[test]
    fn an_aem_run_ships_no_redacto_dump() {
        let mut agent = agent_for(OutputTarget::Aem);
        let outputs = build(&mut agent);
        assert!(outputs.redacto_sql.is_none());
    }
}
