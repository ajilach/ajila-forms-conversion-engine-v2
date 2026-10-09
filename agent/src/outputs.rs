//! Assembling a finished run's artefacts from the agent's working state.
//!
//! Split from the UI so the rule *what ships is the build of the final
//! document* is one function with tests.

use serde_json::Value;

use crate::ConversionAgent;

/// The artefacts a finished run produces.
pub struct Outputs {
    /// The run's final document.
    pub document: Value,
    /// The AEM package, the same form bound to its schema, and the schema.
    pub package: Option<Vec<u8>>,
    pub package_bound: Option<Vec<u8>>,
    pub xsd: Option<String>,
    /// Human-readable notes about anything the run could not produce.
    pub warnings: Vec<String>,
}

/// The run's artefacts: the build of its final document. A document edited
/// after its last build is built again here, so what ships is what was
/// authored last; one that does not encode ships nothing, and says why.
pub fn build(agent: &mut ConversionAgent) -> Outputs {
    let mut warnings = Vec::new();
    if let Err(e) = agent.ensure_built() {
        warnings.push(format!("No AEM package built: {e}"));
    }
    Outputs {
        document: agent.document().clone(),
        package: agent.package(),
        package_bound: agent.package_bound(),
        xsd: agent.xsd(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use serde_json::json;

    use super::*;

    fn agent() -> ConversionAgent {
        crate::db::claim_scratch_db_for_test();
        let pdf = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../forms/AAEV_019_EN.pdf")).unwrap();
        ConversionAgent::new(
            Some("ubs".into()),
            vec![("AAEV_019_EN.pdf".into(), pdf)],
            format!("test-outputs-{}", uuid::Uuid::new_v4()),
        )
        .unwrap()
    }

    /// One page titled `title`.
    fn page(title: Value) -> Value {
        json!({
            "type": "Panel", "uuid": "6a9f2f5e-8c8e-4a8e-9b0e-1f2d3c4b5a61", "name": "PN_Details",
            "title": title, "children": [],
            "is_page": true, "visible": true, "is_conditional": false, "dor_num_cols": null,
            "colspan": 12, "dor_colspan": null, "bind_ref": null, "frag_ref": null
        })
    }

    /// Every file of a package, concatenated as text.
    fn package_text(package: &[u8]) -> String {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(package)).unwrap();
        let mut text = String::new();
        for i in 0..archive.len() {
            let mut bytes = Vec::new();
            archive.by_index(i).unwrap().read_to_end(&mut bytes).unwrap();
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
        text
    }

    /// What ships is the authored document, built: a title that appears
    /// nowhere in the source reaches the package only from the document.
    #[test]
    fn the_outputs_are_the_build_of_the_authored_document() {
        let mut agent = agent();
        let mut doc = agent.document().clone();
        doc["form"]["children"] = json!([page(json!({ "en": "AUTHORED-BY-THE-AGENT-MARKER" }))]);
        agent.seed_document(doc.clone()).unwrap();

        let outputs = build(&mut agent);
        assert!(outputs.warnings.is_empty(), "{:?}", outputs.warnings);
        let package = outputs.package.expect("the authored document yields a package");
        assert!(package_text(&package).contains("AUTHORED-BY-THE-AGENT-MARKER"));
        assert_eq!(outputs.document, doc);
    }

    /// A document that does not encode ships nothing and says why, rather than
    /// a valid-looking package describing another document.
    #[test]
    fn a_document_that_does_not_encode_ships_nothing_and_says_why() {
        let mut agent = agent();
        let mut doc = agent.document().clone();
        doc["form"]["children"] = json!([page(json!({ "fr": "Une langue que le formulaire ne liste pas" }))]);
        agent.seed_document(doc).unwrap();

        let outputs = build(&mut agent);
        assert!(outputs.package.is_none() && outputs.xsd.is_none());
        assert!(
            outputs.warnings.iter().any(|w| w.starts_with("No AEM package built")),
            "{:?}",
            outputs.warnings
        );
    }
}
