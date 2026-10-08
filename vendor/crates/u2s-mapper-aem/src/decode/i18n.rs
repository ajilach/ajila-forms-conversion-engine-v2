//! Sling i18n dictionary reading (AEM.md §10): the inverse of
//! `crate::i18n::write_dictionary_xml`.
//!
//! Entry element names (`fd_<uuid-or-index>`) are never read -- the
//! canonical round-trip comparison (`crate::canonical`, not built in this
//! pass) treats them as insignificant, and a measured fact backs that:
//! roughly a fifth of a real package's own entry names are not derivable
//! from any standard UUID scheme, so they cannot be a decode target either.
//! What decode reads is `sling:key` (the master-language text, `fd_`-
//! prefixed) and `sling:message` (the translated text) -- exactly the pair
//! `write_dictionary_xml` writes.

use std::collections::{BTreeMap, HashMap};

use u2s_aem::model::Language;

use crate::i18n::Dictionary;
use crate::jcr::tree::{self, JcrXmlError};

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("dictionary file {path:?} is not valid JCR XML: {source}")]
    Invalid { path: String, source: JcrXmlError },
}

/// Parses one language's dictionary file into `master text -> translated
/// text`, the same shape [`Dictionary`] already is on the encode side.
/// Ignores any entry with no `sling:key`/`sling:message` pair rather than
/// erroring -- a passthrough concern for a future decoder that carries the
/// dictionary folder's own raw structure, not this function's job today.
pub fn parse_dictionary_xml(xml: &str) -> Result<Dictionary, JcrXmlError> {
    let root = tree::parse_jcr_xml(xml)?;
    let mut dictionary = Dictionary::new();
    for entry in &root.children {
        let (Some(key), Some(message)) = (entry.attr("sling:key"), entry.attr("sling:message"))
        else {
            continue;
        };
        let master_text = key.strip_prefix("fd_").unwrap_or(key);
        dictionary.insert(master_text.to_owned(), message.to_owned());
    }
    Ok(dictionary)
}

/// Reads every `<locale>.xml` file under a form's own dictionary
/// directory. The language set is discovered from the files actually
/// present, not assumed from anywhere else in the package -- the real
/// corpus's own DAM `<dictionary>` node list can disagree with the files
/// shipped (measured on the committed fixture), and the files are the
/// delivered artefact, so they are the source of truth here.
pub fn read_dictionaries(
    zip_files: &HashMap<String, Vec<u8>>,
    dictionary_dir: &str,
) -> Result<BTreeMap<Language, Dictionary>, DictionaryError> {
    let prefix = format!("{dictionary_dir}/");
    let mut out = BTreeMap::new();
    let mut paths: Vec<&String> = zip_files
        .keys()
        .filter(|path| {
            path.starts_with(&prefix) && path.ends_with(".xml") && !path[prefix.len()..].contains('/')
        })
        .collect();
    paths.sort();
    for path in paths {
        let file_name = &path[prefix.len()..path.len() - ".xml".len()];
        let Ok(language) = Language::try_from(file_name) else {
            continue;
        };
        let bytes = &zip_files[path];
        let xml = String::from_utf8_lossy(bytes);
        let dictionary =
            parse_dictionary_xml(&xml).map_err(|source| DictionaryError::Invalid {
                path: path.clone(),
                source,
            })?;
        out.insert(language, dictionary);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_and_message_pairs_ignoring_the_element_name() {
        let xml = r#"<jcr:root xmlns:jcr="http://www.jcp.org/jcr/1.0">
            <fd_anything jcr:primaryType="nt:folder" sling:key="fd_Submit" sling:message="Submit"/>
            <fd_04f8 jcr:primaryType="nt:folder" sling:key="fd_Name" sling:message="Vorname"/>
        </jcr:root>"#;
        let dict = parse_dictionary_xml(xml).unwrap();
        assert_eq!(dict.get("Submit").map(String::as_str), Some("Submit"));
        assert_eq!(dict.get("Name").map(String::as_str), Some("Vorname"));
    }

    #[test]
    fn ignores_entries_missing_key_or_message() {
        let xml = r#"<jcr:root><fd_0 jcr:primaryType="nt:folder" sling:key="fd_Only"/></jcr:root>"#;
        let dict = parse_dictionary_xml(xml).unwrap();
        assert!(dict.is_empty());
    }

    #[test]
    fn read_dictionaries_discovers_languages_from_files_present() {
        let mut files = HashMap::new();
        files.insert(
            "jcr_root/.../dictionary/en.xml".to_owned(),
            br#"<jcr:root><fd_0 sling:key="fd_Name" sling:message="Name"/></jcr:root>"#.to_vec(),
        );
        files.insert(
            "jcr_root/.../dictionary/de-ch.xml".to_owned(),
            br#"<jcr:root><fd_0 sling:key="fd_Name" sling:message="Vorname"/></jcr:root>"#.to_vec(),
        );
        // A file outside the dictionary directory must not be picked up.
        files.insert(
            "jcr_root/.../assets/other/fr.xml".to_owned(),
            b"<jcr:root/>".to_vec(),
        );
        let out = read_dictionaries(&files, "jcr_root/.../dictionary").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.get(&Language::try_from("de-ch").unwrap())
                .and_then(|d| d.get("Name"))
                .map(String::as_str),
            Some("Vorname")
        );
    }
}
