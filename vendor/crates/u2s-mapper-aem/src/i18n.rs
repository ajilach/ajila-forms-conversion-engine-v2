//! Sling i18n dictionary emission (AEM.md §10).
//!
//! `u2s-aem` keeps exactly one source of truth for translated text: every
//! `I18nText`/`I18nRichText` lives inline on the node that owns it, keyed
//! by language, already validated (`AemForm::validate`) to carry the
//! form's master language. `Node::Component`'s own translatable
//! `properties` (`JcrValue::Text`/`JcrValue::RichText`) are included the
//! same way `Node::i18n_texts`/`i18n_rich_texts` surface a typed leaf
//! kind's fields -- one shared mechanism, per this crate's redesign (see
//! `u2s_aem::model::Node`'s own doc). The reference engine needs a separate
//! translation-extraction pass because it has *two* trees -- the original
//! and the converted one -- that must produce byte-identical dictionary
//! keys by hand. That problem does not exist here: this module is one walk
//! collecting strings already on the tree, nothing more.

use std::collections::BTreeMap;

use u2s_aem::model::{AemForm, Language, Node, ValidForm};

/// One language's dictionary: `master-language text -> this language's
/// text`, for every string that has both. A string with no translation
/// into a given language simply has no entry for that language --
/// `AemForm::validate` guarantees every string has a master-language
/// entry, never that it has every language's.
pub type Dictionary = BTreeMap<String, String>;

/// Every configured language's dictionary, keyed by [`Language`], including
/// an (always empty, unless a future writer adds one) entry for the master
/// language itself. The master language's own dictionary carries no
/// self-mapping entries here: a master-language string is already inline in
/// the form XML, so a `master -> master` dictionary entry would add no
/// information -- and the real `AF_AABF.zip` fixture confirms this is not
/// how AEM's own authoring tool behaves either (its master dictionary
/// carries only a handful of identity entries, for reasons this pass could
/// not determine a rule for and does not attempt to reproduce; see
/// `specs/AEM.md`'s own note). An earlier revision of this function
/// disagreed and populated the master dictionary in full -- that was wrong,
/// not merely incomplete, once checked against a real package.
pub fn collect_dictionaries(form: &ValidForm) -> BTreeMap<Language, Dictionary> {
    let inner: &AemForm = form.form();
    let master = &inner.metadata.master_language;
    let mut out: BTreeMap<Language, Dictionary> = inner
        .metadata
        .languages
        .iter()
        .map(|lang| (lang.clone(), Dictionary::new()))
        .collect();

    if let Some(title) = &inner.metadata.title {
        collect_text(title, master, &mut out);
    }
    for page in &inner.pages {
        // A page's own translatable properties (`jcr:title`, most often) --
        // the same `JcrValue::Text` shape a `Node::Component`'s properties
        // use, but `Page` is not itself a `Node` (see its own doc), so
        // there is no `i18n_texts()` to call here.
        for value in page.properties.values() {
            if let u2s_aem::model::JcrValue::Text(text) = value {
                collect_text(text, master, &mut out);
            }
        }
        walk(&page.children, master, &mut out);
    }
    // `metadata.toolbar`'s own actions carry translatable properties
    // (`jcr:title`, most often) the same as any other `Node` -- missed
    // entirely by an earlier revision of this function, which only walked
    // `pages`. Verified against the real fixture: its toolbar's `jcr:title`
    // values ("Next", "Submit", "Preview", ...) are genuinely translated in
    // every configured language, so skipping them was a real loss, not an
    // accepted gap.
    walk(&inner.metadata.toolbar, master, &mut out);
    out
}

fn walk(nodes: &[Node], master: &Language, out: &mut BTreeMap<Language, Dictionary>) {
    for node in nodes {
        // `Node::i18n_texts`/`i18n_rich_texts` already cover a
        // `Node::Component`'s own translatable properties (`jcr:title`,
        // most often) the same way they cover a typed leaf kind's fields --
        // one shared mechanism, no separate "node title" special case.
        for (_, text) in node.i18n_texts() {
            collect_text(text, master, out);
        }
        for (_, text) in node.i18n_rich_texts() {
            collect_rich_text(text, master, out);
        }
        if let Some(options) = node.options() {
            for option in options.options() {
                collect_text(&option.label, master, out);
            }
        }
        if let Some(children) = node.children() {
            walk(children, master, out);
        }
    }
}

fn collect_text(
    text: &u2s_aem::model::I18nText,
    master: &Language,
    out: &mut BTreeMap<Language, Dictionary>,
) {
    let Some(master_value) = text.get(master) else {
        // `AemForm::validate` guarantees this cannot happen for a
        // `ValidForm`; a hard failure here would be reachable only from a
        // bug in that guarantee, and this function has no error return to
        // report one, so it degrades to "nothing to key by" instead.
        return;
    };
    for lang in text.languages() {
        if lang == master {
            // No `master -> master` self-entry -- see this module's own
            // doc on `collect_dictionaries`.
            continue;
        }
        let Some(value) = text.get(lang) else { continue };
        if let Some(dictionary) = out.get_mut(lang) {
            dictionary.insert(master_value.as_str().to_owned(), value.as_str().to_owned());
        }
    }
}

fn collect_rich_text(
    text: &u2s_aem::model::I18nRichText,
    master: &Language,
    out: &mut BTreeMap<Language, Dictionary>,
) {
    let Some(master_value) = text.get(master) else {
        return;
    };
    for lang in text.languages() {
        if lang == master {
            continue;
        }
        let Some(value) = text.get(lang) else { continue };
        if let Some(dictionary) = out.get_mut(lang) {
            dictionary.insert(master_value.as_str().to_owned(), value.as_str().to_owned());
        }
    }
}

/// One language's dictionary file, `<locale>.xml`.
///
/// Entry element names are `fd_<index>` in stable, sorted order --
/// deterministic without a UUID dependency, and JCR does not require the
/// element name to encode anything beyond being a unique, valid NCName
/// within its parent.
pub fn write_dictionary_xml(language: &Language, dictionary: &Dictionary) -> String {
    use std::fmt::Write as _;

    let mut body = String::new();
    for (index, (key, message)) in dictionary.iter().enumerate() {
        let escaped_key = crate::jcr::escape_attribute_value(key);
        let escaped_message = crate::jcr::escape_attribute_value(message);
        let _ = writeln!(
            body,
            "    <fd_{index} jcr:mixinTypes=\"[sling:Message]\"\n\
             \x20              jcr:primaryType=\"nt:folder\"\n\
             \x20              sling:key=\"fd_{escaped_key}\"\n\
             \x20              sling:message=\"{escaped_message}\"/>"
        );
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="{sling}"
          xmlns:jcr="{jcr}"
          xmlns:mix="{mix}"
          xmlns:nt="{nt}"
          jcr:language="{language}"
          jcr:mixinTypes="[mix:language]"
          jcr:primaryType="sling:Folder">
{body}</jcr:root>
"#,
        sling = crate::jcr::ns::SLING,
        jcr = crate::jcr::ns::JCR,
        mix = crate::jcr::ns::MIX,
        nt = crate::jcr::ns::NT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn every_configured_language_gets_a_dictionary_including_the_master() {
        let mut form = build_form(
            u2s_aem::model::DataModel::Unbound,
            vec![a_page_with(vec![a_text_field("Name")])],
        );
        // Add a second language so translation actually has something to
        // carry -- `a_text_field`'s fixture is English-only.
        form = with_language(form, "de");
        let dictionaries = collect_dictionaries(&form);
        assert!(dictionaries.contains_key(&lang("en")));
        assert!(dictionaries.contains_key(&lang("de")));
    }

    #[test]
    fn a_translated_label_is_keyed_by_its_master_language_text() {
        // Built with both languages declared from the start: `validate()`
        // checks every string's language keys against `metadata.languages`
        // in one pass, so a translation added after the fact needs the
        // language added before that pass, not after it (see
        // `with_language`, which is for the *other* test's simpler case of
        // adding a language nothing yet uses).
        let mut field = a_text_field("Name");
        set_label_translation(&mut field, "de", "Vorname");
        let page = a_page_with(vec![field]);
        let form = u2s_aem::model::AemForm {
            metadata: u2s_aem::model::FormMetadata {
                form_name: u2s_aem::model::FormName::try_from("TestForm").unwrap(),
                title: None,
                master_language: lang("en"),
                languages: std::collections::BTreeSet::from([lang("en"), lang("de")]),
                dor: u2s_aem::model::DorMode::None,
                data_model: u2s_aem::model::DataModel::Unbound,
                toolbar: Vec::new(),
                folder_path: Vec::new(),
                root_panel_layout: None,
                chrome: Default::default(),
                dam_chrome: Default::default(),
                page_content: Default::default(),
                root_panel: Default::default(),
                toolbar_chrome: Default::default(),
            },
            pages: vec![page],
        }
        .validate()
        .expect("both languages are declared, so the translation validates");

        let dictionaries = collect_dictionaries(&form);
        let de = dictionaries.get(&lang("de")).expect("german dictionary");
        assert_eq!(de.get("Name").map(String::as_str), Some("Vorname"));
    }

    #[test]
    fn a_string_missing_a_translation_has_no_entry_for_that_language() {
        let form = with_language(
            build_form(
                u2s_aem::model::DataModel::Unbound,
                vec![a_page_with(vec![a_text_field("Name")])],
            ),
            "de",
        );
        let dictionaries = collect_dictionaries(&form);
        let de = dictionaries.get(&lang("de")).expect("german dictionary exists");
        assert!(
            de.is_empty(),
            "no translation was ever given, so nothing should be keyed: {de:?}"
        );
    }

    #[test]
    fn dictionary_xml_escapes_keys_and_messages() {
        let mut dict = Dictionary::new();
        dict.insert("A & B".to_owned(), "C < D".to_owned());
        let xml = write_dictionary_xml(&lang("en"), &dict);
        assert!(xml.contains("A &amp; B"));
        assert!(xml.contains("C &lt; D"));
    }

    /// Test-only: adds a language to a form's `metadata.languages` without
    /// needing a full builder round-trip, since [`ValidForm`] is otherwise
    /// sealed.
    fn with_language(form: ValidForm, code: &str) -> ValidForm {
        let mut inner = form.into_form();
        inner.metadata.languages.insert(lang(code));
        inner.validate().expect("adding a language keeps the form valid")
    }

    /// `I18nText` exposes no public insert, only `single` -- rebuilt here
    /// through its `#[serde(transparent)]` JSON shape (`{"<lang>":
    /// "<text>", ...}`), which is also exactly how the Conversion Agent's
    /// own `json_patch` calls will ever populate one.
    fn set_label_translation(node: &mut Node, code: &str, text: &str) {
        if let Node::TextField { field, .. } = node {
            let mut map = serde_json::Map::new();
            for existing_lang in field.label.languages() {
                let value = field.label.get(existing_lang).expect("present").as_str();
                map.insert(existing_lang.as_str().to_owned(), serde_json::json!(value));
            }
            map.insert(code.to_owned(), serde_json::json!(text));
            field.label = serde_json::from_value(serde_json::Value::Object(map))
                .expect("re-parses as I18nText");
        }
    }
}
