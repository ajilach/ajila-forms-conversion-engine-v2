//! Parser for AEM fragment `.content.xml` files: extracts a fragment's
//! `fragmentModelRoot` and `bindRef` attributes into a [`ParsedFragment`], which
//! the XSD walk uses to type a fragment reference. The profile's embedded
//! library is loaded through [`crate::profiles::load_aem_fragments`].

/// A parsed AEM fragment with its XSD type binding information.
#[derive(Debug, Clone)]
pub struct ParsedFragment {
    /// Directory name of the fragment (e.g. `"affrg_Address1"`).
    pub dir_name: String,

    /// JCR path used as `fragRef` attribute in the generated XML
    /// (e.g. `"/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_Address1"`).
    pub frag_ref: String,

    /// The AEM `name` attribute for the fragment node
    /// (e.g. `"PN_affrg_Address1"`).
    pub name: String,

    /// XSD complex type name extracted from `fragmentModelRoot`
    /// (e.g. `"AddressType"` from `/AddressType`).
    pub xsd_type_name: String,

    /// Element names extracted from `bindRef` attributes within the fragment
    /// (e.g. `["Street", "Number", "City"]`).
    pub bound_elements: Vec<String>,
}

/// Parse fragment metadata from `.content.xml` text and a known relative
/// fragment directory path.
///
/// `relative_dir_path` is path-like (e.g. `"afforms_ubs_fragmentlib/affrg_Address1"`).
pub fn parse_fragment_content(
    relative_dir_path: &str,
    fragment_ref_prefix: &str,
    content: &str,
) -> Option<ParsedFragment> {
    let relative_dir_path = relative_dir_path.trim_matches('/');
    if relative_dir_path.is_empty() {
        return None;
    }

    // Extract fragmentModelRoot (e.g. fragmentModelRoot="/AddressType")
    let xsd_type_name = extract_attr_value(content, "fragmentModelRoot")?;
    let xsd_type_name = xsd_type_name.trim_start_matches('/').to_string();
    if xsd_type_name.is_empty() {
        return None;
    }

    // Build fragRef from the relative directory path.
    let prefix = fragment_ref_prefix.trim_end_matches('/');
    let frag_ref = format!("{}/{}", prefix, relative_dir_path);

    // Directory name for identity.
    let dir_name = relative_dir_path
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();

    let name = format!("PN_affrg_{}", dir_name.trim_start_matches("affrg_"));

    // Extract all bindRef values to collect bound element names
    let bound_elements = extract_bind_ref_elements(content, &xsd_type_name);

    Some(ParsedFragment {
        dir_name,
        frag_ref,
        name,
        xsd_type_name,
        bound_elements,
    })
}

/// Extract the value of a named XML attribute from raw XML text.
///
/// Looks for `attr_name="value"` patterns. Returns the first match.
fn extract_attr_value(xml: &str, attr_name: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr_name);
    let start = xml.find(&pattern)?;
    let value_start = start + pattern.len();
    let rest = &xml[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract element names from `bindRef` attributes in the XML content.
///
/// `bindRef` values look like `/TypeName/ElementName` — we extract the
/// leaf element name (after the last `/`), but only if the path starts
/// with the expected type root.
fn extract_bind_ref_elements(xml: &str, type_name: &str) -> Vec<String> {
    let mut elements = Vec::new();
    let pattern = "bindRef=\"";
    let mut search_from = 0;

    while let Some(pos) = xml[search_from..].find(pattern) {
        let abs_pos = search_from + pos;
        let value_start = abs_pos + pattern.len();
        if let Some(end) = xml[value_start..].find('"') {
            let bind_ref = &xml[value_start..value_start + end];
            // Expected format: /TypeName/ElementName
            let expected_prefix = format!("/{}/", type_name);
            if let Some(rest) = bind_ref.strip_prefix(&expected_prefix) {
                // Take only the immediate child (no nested paths)
                if !rest.contains('/') && !rest.is_empty() {
                    elements.push(rest.to_string());
                }
            }
            search_from = value_start + end + 1;
        } else {
            break;
        }
    }

    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_attr_value() {
        let xml = r#"fragmentModelRoot="/BankingRelationshipType" guideCss="guideContainer""#;
        assert_eq!(
            extract_attr_value(xml, "fragmentModelRoot"),
            Some("/BankingRelationshipType".into())
        );
        assert_eq!(
            extract_attr_value(xml, "guideCss"),
            Some("guideContainer".into())
        );
        assert_eq!(extract_attr_value(xml, "missing"), None);
    }

    #[test]
    fn test_extract_bind_ref_elements() {
        let xml = r#"
            bindRef="/AddressType/Street"
            bindRef="/AddressType/Number"
            bindRef="/AddressType/City"
            bindRef="/OtherType/Foo"
        "#;
        let elements = extract_bind_ref_elements(xml, "AddressType");
        assert_eq!(elements, vec!["Street", "Number", "City"]);
    }

    #[test]
    fn test_extract_bind_ref_elements_nested_ignored() {
        let xml = r#"bindRef="/AddressType/Nested/Deep""#;
        let elements = extract_bind_ref_elements(xml, "AddressType");
        assert!(elements.is_empty());
    }

    #[test]
    fn test_parse_fragment_name_generation() {
        assert_eq!(
            format!("PN_affrg_{}", "affrg_Address1".trim_start_matches("affrg_")),
            "PN_affrg_Address1"
        );
        assert_eq!(
            format!(
                "PN_affrg_{}",
                "affrg_BankingRelationship1".trim_start_matches("affrg_")
            ),
            "PN_affrg_BankingRelationship1"
        );
    }
}
