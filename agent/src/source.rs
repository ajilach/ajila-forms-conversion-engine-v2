//! What a source PDF says about itself: its language and its XFA template
//! variables, read through `u2s-xfa`.

use std::collections::BTreeMap;

/// One source PDF's language and XFA `<variables><text>` values.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceContext {
    /// The template's locale as a language code (`de`, `en`, `es`, ...).
    pub language: String,
    pub variables: BTreeMap<String, String>,
}

/// The context of an XFA PDF, or `None` for a PDF that carries no XFA.
pub fn read(pdf: &[u8]) -> Result<Option<SourceContext>, String> {
    let Some(xfa) = u2s_xfa::extract::extract_xfa_from_pdf_bytes(pdf)
        .map_err(|e| format!("could not read the PDF: {e}"))?
    else {
        return Ok(None);
    };
    let nodes = u2s_xfa::xfa::XfaNode::parse(&xfa).map_err(|e| format!("could not parse the XFA: {e}"))?;
    let (language, variables) = u2s_xfa::xfa::extract_context_from_nodes(&nodes);
    Ok(Some(SourceContext {
        language,
        variables: variables.into_iter().collect(),
    }))
}
