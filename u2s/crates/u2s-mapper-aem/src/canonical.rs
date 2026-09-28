//! The round-trip comparison a real package's own quirks need: not byte
//! equality (a real package's own dictionary entry names are not always
//! reproducible -- 56 of 311 measured on the committed fixture do not
//! match any standard UUID derivation), but a canonical structural
//! comparison that still treats every other difference as real.
//!
//! Rules, matching the design plan's own C4/C8 spec:
//!
//! 1. **ZIP**: the *set* of entry names must match -- missing or extra is
//!    always reported. Entry order, compression method and timestamps are
//!    never compared (this crate's own `package.rs` writes a fixed
//!    timestamp specifically so a *byte*-identical golden test does not
//!    flap; this comparison does not even need that, since it never looks
//!    at raw bytes for a `.content.xml`/dictionary entry at all).
//! 2. **JCR XML entries** (`.content.xml`, `schema.xsd`'s literal text):
//!    parsed and compared by tag name, attribute set (order-independent --
//!    JCR attribute order is never semantic), and children **in order**
//!    (sibling order is semantic: a wizard step's position among its
//!    siblings, an option's position in its list).
//! 3. **Typed attribute values** are compared as the exact strings they
//!    are -- `visible="false"` and `visible="{Boolean}false"` are already
//!    different strings, so no separate "type-hint" comparison is needed
//!    beyond comparing the attribute value verbatim.
//! 4. **Dictionary files** (anything under a `.../dictionary/` directory):
//!    compared as a *set* of `(sling:key, sling:message)` pairs, ignoring
//!    entry element names entirely -- justified by the measurement above.
//! 5. **Everything else** (binary entries -- DOR renditions, say):
//!    compared by sha256 digest.

use std::collections::BTreeSet;

use crate::decode::i18n::parse_dictionary_xml;
use crate::jcr::tree::{JcrNode, parse_jcr_xml};

/// One concrete difference, described precisely enough to act on -- never
/// just "these differ".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference(pub String);

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Compares two FileVault packages canonically. An empty result means the
/// two packages carry the same information under the rules above; this
/// crate's own `encode`/`decode` pair is proven lossless over whatever a
/// caller feeds it exactly when this returns empty.
pub fn compare(expected: &[u8], actual: &[u8]) -> Vec<Difference> {
    let mut diffs = Vec::new();

    let expected_files = match crate::decode::zip::open_zip(expected) {
        Ok(files) => files,
        Err(e) => {
            diffs.push(Difference(format!("expected package does not open as a ZIP: {e}")));
            return diffs;
        }
    };
    let actual_files = match crate::decode::zip::open_zip(actual) {
        Ok(files) => files,
        Err(e) => {
            diffs.push(Difference(format!("actual package does not open as a ZIP: {e}")));
            return diffs;
        }
    };

    let expected_names: BTreeSet<&String> = expected_files.keys().collect();
    let actual_names: BTreeSet<&String> = actual_files.keys().collect();

    for missing in expected_names.difference(&actual_names) {
        diffs.push(Difference(format!("missing entry: {missing}")));
    }
    for extra in actual_names.difference(&expected_names) {
        diffs.push(Difference(format!("extra entry: {extra}")));
    }

    for name in expected_names.intersection(&actual_names) {
        let expected_bytes = &expected_files[*name];
        let actual_bytes = &actual_files[*name];
        if is_dictionary_entry(name) {
            compare_dictionary(name, expected_bytes, actual_bytes, &mut diffs);
        } else if name.ends_with(".xml") || name.ends_with(".xsd") {
            compare_xml_entry(name, expected_bytes, actual_bytes, &mut diffs);
        } else {
            compare_binary_entry(name, expected_bytes, actual_bytes, &mut diffs);
        }
    }

    diffs
}

fn is_dictionary_entry(name: &str) -> bool {
    name.contains("/dictionary/") && name.ends_with(".xml")
}

fn compare_dictionary(name: &str, expected: &[u8], actual: &[u8], diffs: &mut Vec<Difference>) {
    let expected_xml = String::from_utf8_lossy(expected);
    let actual_xml = String::from_utf8_lossy(actual);
    let (expected_dict, actual_dict) =
        match (parse_dictionary_xml(&expected_xml), parse_dictionary_xml(&actual_xml)) {
            (Ok(e), Ok(a)) => (e, a),
            (Err(e), _) => {
                diffs.push(Difference(format!("{name}: expected side does not parse as a dictionary: {e}")));
                return;
            }
            (_, Err(e)) => {
                diffs.push(Difference(format!("{name}: actual side does not parse as a dictionary: {e}")));
                return;
            }
        };
    let expected_pairs: BTreeSet<(String, String)> = expected_dict.into_iter().collect();
    let actual_pairs: BTreeSet<(String, String)> = actual_dict.into_iter().collect();
    for (key, message) in expected_pairs.difference(&actual_pairs) {
        diffs.push(Difference(format!("{name}: missing dictionary entry {key:?} = {message:?}")));
    }
    for (key, message) in actual_pairs.difference(&expected_pairs) {
        diffs.push(Difference(format!("{name}: extra dictionary entry {key:?} = {message:?}")));
    }
}

fn compare_xml_entry(name: &str, expected: &[u8], actual: &[u8], diffs: &mut Vec<Difference>) {
    let expected_xml = String::from_utf8_lossy(expected);
    let actual_xml = String::from_utf8_lossy(actual);
    let (expected_tree, actual_tree) = match (parse_jcr_xml(&expected_xml), parse_jcr_xml(&actual_xml)) {
        (Ok(e), Ok(a)) => (e, a),
        (Err(e), _) => {
            diffs.push(Difference(format!("{name}: expected side does not parse as JCR XML: {e}")));
            return;
        }
        (_, Err(e)) => {
            diffs.push(Difference(format!("{name}: actual side does not parse as JCR XML: {e}")));
            return;
        }
    };
    compare_nodes(name, &expected_tree, &actual_tree, diffs);
}

fn compare_nodes(path: &str, expected: &JcrNode, actual: &JcrNode, diffs: &mut Vec<Difference>) {
    if expected.tag_name != actual.tag_name {
        diffs.push(Difference(format!(
            "{path}: tag name differs: expected {:?}, got {:?}",
            expected.tag_name, actual.tag_name
        )));
        return;
    }

    let expected_attrs: std::collections::BTreeMap<&str, &str> = expected
        .attributes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let actual_attrs: std::collections::BTreeMap<&str, &str> = actual
        .attributes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    for (key, expected_value) in &expected_attrs {
        match actual_attrs.get(key) {
            None => diffs.push(Difference(format!("{path}: missing attribute {key:?} (expected {expected_value:?})"))),
            Some(actual_value) if actual_value != expected_value => diffs.push(Difference(format!(
                "{path}: attribute {key:?} differs: expected {expected_value:?}, got {actual_value:?}"
            ))),
            Some(_) => {}
        }
    }
    for key in actual_attrs.keys() {
        if !expected_attrs.contains_key(key) {
            diffs.push(Difference(format!("{path}: extra attribute {key:?} = {:?}", actual_attrs[key])));
        }
    }

    if expected.children.len() != actual.children.len() {
        diffs.push(Difference(format!(
            "{path}: child count differs: expected {} ({:?}), got {} ({:?})",
            expected.children.len(),
            expected.children.iter().map(|c| c.tag_name.as_str()).collect::<Vec<_>>(),
            actual.children.len(),
            actual.children.iter().map(|c| c.tag_name.as_str()).collect::<Vec<_>>(),
        )));
        // Still compare whatever pairs up positionally, so a single
        // inserted/removed child does not hide every difference after it.
    }
    for (index, (expected_child, actual_child)) in
        expected.children.iter().zip(actual.children.iter()).enumerate()
    {
        let child_path = format!("{path}/{}[{index}]", expected_child.tag_name);
        compare_nodes(&child_path, expected_child, actual_child, diffs);
    }
}

fn compare_binary_entry(name: &str, expected: &[u8], actual: &[u8], diffs: &mut Vec<Difference>) {
    if expected != actual {
        diffs.push(Difference(format!(
            "{name}: binary content differs ({} bytes vs {} bytes)",
            expected.len(),
            actual.len()
        )));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::test_support::*;

    fn a_package() -> Vec<u8> {
        let form = build_form(
            xml_schema("Root"),
            vec![a_page_named("Page1", vec![a_text_field("Name")])],
        );
        crate::encode(&form).expect("encodes").bytes
    }

    #[test]
    fn identical_packages_have_no_differences() {
        let bytes = a_package();
        assert!(compare(&bytes, &bytes).is_empty());
    }

    #[test]
    fn a_missing_entry_is_reported() {
        let bytes = a_package();
        let mut files = crate::decode::zip::open_zip(&bytes).unwrap();
        files.remove("META-INF/vault/config.xml");
        let rebuilt = rezip(&files);
        let diffs = compare(&bytes, &rebuilt);
        assert!(diffs.iter().any(|d| d.0.contains("missing entry: META-INF/vault/config.xml")));
    }

    #[test]
    fn an_extra_entry_is_reported() {
        let bytes = a_package();
        let mut files = crate::decode::zip::open_zip(&bytes).unwrap();
        files.insert("jcr_root/extra.txt".to_owned(), b"hello".to_vec());
        let rebuilt = rezip(&files);
        let diffs = compare(&bytes, &rebuilt);
        assert!(diffs.iter().any(|d| d.0.contains("extra entry: jcr_root/extra.txt")));
    }

    #[test]
    fn a_differing_attribute_is_reported() {
        let bytes = a_package();
        let mut files = crate::decode::zip::open_zip(&bytes).unwrap();
        let key = "jcr_root/content/forms/af/TestForm/.content.xml".to_owned();
        let original = String::from_utf8(files[&key].clone()).unwrap();
        let mutated = original.replacen("visible=\"{Boolean}true\"", "visible=\"{Boolean}false\"", 1);
        assert_ne!(original, mutated, "the fixture must actually contain the attribute being mutated");
        files.insert(key, mutated.into_bytes());
        let rebuilt = rezip(&files);
        let diffs = compare(&bytes, &rebuilt);
        assert!(
            diffs.iter().any(|d| d.0.contains("attribute \"visible\" differs")),
            "expected a visible-attribute diff, got: {diffs:?}"
        );
    }

    #[test]
    fn dictionary_entries_compare_by_key_message_pairs_ignoring_element_names() {
        // Two dictionaries with the same (key, message) content under
        // differently-named entry elements must compare equal -- the
        // whole point of this rule (see the module doc's own measurement).
        let a = br#"<jcr:root><fd_0000 sling:key="fd_Name" sling:message="Name"/></jcr:root>"#;
        let b = br#"<jcr:root><fd_9999 sling:key="fd_Name" sling:message="Name"/></jcr:root>"#;
        let mut diffs = Vec::new();
        compare_dictionary("en.xml", a, b, &mut diffs);
        assert!(diffs.is_empty(), "expected no diffs, got: {diffs:?}");
    }

    #[test]
    fn dictionary_entries_with_different_messages_are_reported() {
        let a = br#"<jcr:root><fd_0 sling:key="fd_Name" sling:message="Name"/></jcr:root>"#;
        let b = br#"<jcr:root><fd_0 sling:key="fd_Name" sling:message="Vorname"/></jcr:root>"#;
        let mut diffs = Vec::new();
        compare_dictionary("de.xml", a, b, &mut diffs);
        assert!(!diffs.is_empty());
    }

    #[test]
    fn child_order_is_significant() {
        let a = r#"<root><a name="1"/><b name="2"/></root>"#;
        let b = r#"<root><b name="2"/><a name="1"/></root>"#;
        let expected = parse_jcr_xml(a).unwrap();
        let actual = parse_jcr_xml(b).unwrap();
        let mut diffs = Vec::new();
        compare_nodes("#", &expected, &actual, &mut diffs);
        assert!(!diffs.is_empty(), "reordered children must be reported");
    }

    /// Rebuilds a ZIP from a `HashMap<String, Vec<u8>>` -- the inverse of
    /// `decode::zip::open_zip`, needed only by these tests to mutate one
    /// entry and re-serialize.
    fn rezip(files: &HashMap<String, Vec<u8>>) -> Vec<u8> {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let mut names: Vec<&String> = files.keys().collect();
        names.sort();
        for name in names {
            zip.start_file(name, options).unwrap();
            zip.write_all(&files[name]).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
}
