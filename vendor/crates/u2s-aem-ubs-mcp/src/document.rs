//! The UBS AEM document: what the Conversion Agent authors, and what `encode`
//! turns into a FileVault package.
//!
//! The document is the multilingual form tree ([`AemNodeTranslated`]) plus the
//! little the UBS profile needs from the source form to place and name it: the
//! XFA template variables, the master-page header and the form's languages.
//! Everything else (the chrome, the step titles, the toolbar, the DAM metadata,
//! the dictionaries, the schema) is the profile's, rendered by the moved writer.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::aem::fragment_parser::ParsedFragment;
use crate::aem::partner::{self, is_partner_generic};
use crate::aem::{
    AemConfig, AemNode, AemNodeTranslated, LowerConflict, aem_to_translated,
    generate_aem_package_from_node_with_passthrough, parse_aem_zip,
};
use crate::context::Context;
use crate::xsd::{apply_bind_refs, generate_xsd_from_aem, generate_xsd_string_from_aem};

/// The profile every document is encoded with. UBS is the only one this crate
/// ships.
const PROFILE: &str = "ubs";

/// A UBS adaptive form, as authored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UbsAemDocument {
    /// The source form's XFA template variables (`<variables><text>`), by name.
    /// The profile derives the form's code, its repository path, its schema and
    /// its DAM metadata from them (`formrange_code`, `formrange_entity`,
    /// `formrange_version`, ...), so copy them from the source as they are.
    pub variables: BTreeMap<String, String>,
    /// The plain text of the source's master-page header (for example the legal
    /// entity's name printed top of page), if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Every language the form ships in, as ISO 639-1 codes. Each text in `form`
    /// may carry only these languages.
    pub languages: Vec<String>,
    /// The form itself. Must be a `Root`.
    pub form: AemNodeTranslated,
}

impl UbsAemDocument {
    /// Read a document the way an author wrote it: the one place untrusted JSON
    /// becomes a document.
    ///
    /// The form's nodes flatten their placement flags into themselves
    /// (`summary_exclude`, `dor_exclude`, ...), and serde cannot refuse unknown
    /// fields next to a flattened struct, so a misspelled flag would vanish and
    /// the content it was meant to hide would reach the DoR. Every key the
    /// document does not keep is refused instead, unless its value is a default
    /// (null, `false`, empty) that changes nothing either way.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, Error> {
        let doc: Self = serde_json::from_value(value.clone())
            .map_err(|e| Error::InvalidDocument(e.to_string()))?;
        let kept = serde_json::to_value(&doc).expect("a document serializes");
        let mut dropped = Vec::new();
        dropped_keys(value, &kept, "", &mut dropped);
        if !dropped.is_empty() {
            return Err(Error::InvalidDocument(format!(
                "unknown fields, which would be ignored: {}",
                dropped.join(", ")
            )));
        }
        Ok(doc)
    }
}

/// The pointers of the keys in `input` that `kept` lacks, where it matters.
fn dropped_keys(
    input: &serde_json::Value,
    kept: &serde_json::Value,
    at: &str,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    let is_default = |v: &Value| match v {
        Value::Null | Value::Bool(false) => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::Number(_) | Value::Bool(true) => false,
    };
    match (input, kept) {
        (Value::Object(input), Value::Object(kept)) => {
            for (key, value) in input {
                let pointer = format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
                match kept.get(key) {
                    Some(k) => dropped_keys(value, k, &pointer, out),
                    None if is_default(value) => {}
                    None => out.push(pointer),
                }
            }
        }
        (Value::Array(input), Value::Array(kept)) => {
            for (i, (value, k)) in input.iter().zip(kept).enumerate() {
                dropped_keys(value, k, &format!("{at}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// What [`encode`] builds from a document.
#[derive(Debug, Clone)]
pub struct UbsAemBuild {
    /// The package as UBS deploys it: no `bindRef`s, no schema.
    pub package: Vec<u8>,
    /// The same form bound to its schema: every field carries a `bindRef` and
    /// the package ships the XSD. `None` when the profile names no schema path.
    pub bound_package: Option<Vec<u8>>,
    /// The form's data schema, derived from the same tree as the `bindRef`s.
    /// `None` when the profile has no XSD configuration.
    pub xsd: Option<String>,
}

/// Encode a document into the UBS package, its bound twin and its schema.
pub fn encode(doc: &UbsAemDocument) -> Result<UbsAemBuild, Error> {
    validate(doc)?;
    let config = config_for(&doc.variables, doc.header.as_deref(), &doc.languages)?;
    check_parties(doc, &config.fragments)?;

    let (node, translations, conflicts) = lower(doc, &config);
    // The dictionary is keyed by master text, so a text translated two ways
    // would ship only one of them.
    if !conflicts.is_empty() {
        let detail = conflicts
            .iter()
            .map(|c| {
                format!(
                    "{:?} in {}: {:?} and {:?}",
                    c.master_text, c.lang, c.existing, c.incoming
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::InvalidDocument(format!(
            "these texts read the same in the master language but are translated differently, \
             and the dictionary can hold only one translation per text: {detail}"
        )));
    }
    let passthrough = doc.form.passthrough_map();
    let package =
        generate_aem_package_from_node_with_passthrough(&node, &config, translations, &passthrough)
            .map_err(Error::InvalidDocument)?;

    // The bound twin needs its own lowering: `bindRef`s are only derived when
    // `bind_to_xsd` is on.
    let bound = (!config.bind_to_xsd && config.xsd_path.is_some())
        .then(|| {
            let mut bound_config = config.clone();
            bound_config.bind_to_xsd = true;
            let (bound_node, bound_translations, _) = lower(doc, &bound_config);
            let bound_package = generate_aem_package_from_node_with_passthrough(
                &bound_node,
                &bound_config,
                bound_translations,
                &passthrough,
            )
            .map_err(Error::InvalidDocument)?;
            Ok::<_, Error>((bound_package, bound_node))
        })
        .transpose()?;

    let xsd = config.xsd_config.as_ref().map(|xsd_config| {
        let schema_tree = bound.as_ref().map_or(&node, |(_, bound_node)| bound_node);
        generate_xsd_string_from_aem(schema_tree, xsd_config, &config.fragments)
    });

    check_package_chars(&package)?;
    if let Some((bound_package, _)) = &bound {
        check_package_chars(bound_package)?;
    }
    Ok(UbsAemBuild {
        package,
        bound_package: bound.map(|(package, _)| package),
        xsd,
    })
}

/// Refuse a package whose XML holds a character XML 1.0 cannot carry
/// (`u2s_mapper_aem::jcr::check_xml_chars`): AEM would reject it on install,
/// and a text from the document is the only way one gets in.
fn check_package_chars(package: &[u8]) -> Result<(), Error> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(package))
        .expect("the package writer writes a valid zip");
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("an entry of a valid zip");
        if !file.name().ends_with(".xml") {
            continue;
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .expect("the package writer writes UTF-8 XML");
        if let Err(e) = u2s_mapper_aem::jcr::check_xml_chars(&text) {
            return Err(Error::InvalidDocument(format!(
                "a text of the document holds a character a form cannot carry: {} in {}",
                e,
                file.name()
            )));
        }
    }
    Ok(())
}

/// Lift a UBS package back into a document, so a deployed form or a reference
/// package can be loaded and edited.
///
/// The form's variables and languages come from the metadata the profile writes
/// into every package (`metadataTextDraw`), and the header from the DoR header
/// slot the preface writes from it: the issuing entity and what qualifies it.
/// The header's validity line never reaches the slot, so it does not come back.
///
/// What the writer expands is folded back into the nodes it is written for
/// (see [`crate::aem::unexpand`]), and what it regenerates is not kept:
/// a fragment's Initialize script, for one, becomes the profile's again.
pub fn decode(package: &[u8]) -> Result<UbsAemDocument, Error> {
    let parsed = parse_aem_zip(package).map_err(Error::Package)?;
    let header = recorded_header(&parsed.root)?;
    let variables = source_variables(&parsed.metadata);
    let languages = form_languages(&parsed.metadata)?;
    let config = config_for(&variables, None, &languages)?;
    // The package's own strings are in the language the profile masters the
    // form in, which is its fixed master only when the form ships that one.
    let root = crate::aem::unexpand(parsed.root);
    let mut form = aem_to_translated(
        &root,
        &parsed.translations,
        &languages,
        &config.base_language(),
        &parsed.raw_by_uuid,
    );
    form.visit_mut(&mut |node| {
        if let AemNodeTranslated::Fragment {
            uuid,
            frag_ref,
            init_hide,
            init_show,
            ..
        } = node
            && is_partner_generic(frag_ref)
            && let Some(visibility) = parsed.fragment_init.get(uuid)
        {
            *init_hide = visibility.hide.clone();
            *init_show = visibility.show.clone();
        }
    });
    Ok(UbsAemDocument {
        variables,
        header,
        languages,
        form,
    })
}

/// The name the preface lowering gives the DoR header slot it writes.
const HEADER_SLOT: &str = "ST_HeaderSlot2";

/// The page header as far as the DoR header slot records it, or `None` for a
/// form without a slot. The slot holds `<b>entity</b> qualifier`, which a header
/// of the entity on one line and the qualifier on the next writes again; a slot
/// that does not come back from that is refused rather than approximated.
fn recorded_header(root: &AemNode) -> Result<Option<String>, Error> {
    fn find(node: &AemNode) -> Option<&str> {
        match node {
            AemNode::TextDraw { name, content, .. } if name == HEADER_SLOT => Some(content),
            AemNode::Root { children, .. }
            | AemNode::Panel { children, .. }
            | AemNode::Repeatable { children, .. } => children.iter().find_map(find),
            _ => None,
        }
    }
    let Some(content) = find(root) else {
        return Ok(None);
    };
    let unrecognised = || {
        Error::Package(format!(
            "the DoR header slot `{content}` is not the preface's"
        ))
    };
    let slot = content
        .trim_end()
        .strip_prefix("<p>")
        .and_then(|c| c.strip_suffix("</p>"))
        .ok_or_else(unrecognised)?;
    let (entity, tail) = slot
        .strip_prefix("<b>")
        .and_then(|c| c.split_once("</b>"))
        .ok_or_else(unrecognised)?;
    let unescape = |s: &str| {
        quick_xml::escape::unescape(s)
            .map(|s| s.into_owned())
            .map_err(|_| unrecognised())
    };
    let header = [unescape(entity)?, unescape(tail.trim())?]
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    // The slot the header writes, as the parser reads it back.
    let rewritten = crate::aem::header_slot_text(&header)
        .map(|value| quick_xml::escape::unescape(&value).map(|v| v.into_owned()));
    match rewritten {
        Some(Ok(value)) if value == slot => Ok(Some(header)),
        _ => Err(unrecognised()),
    }
}

/// The metadata attributes the profile writes itself rather than copying a
/// source variable into them.
const DERIVED_METADATA: &[&str] = &[
    "formrange_afmasterlanguage",
    "formrange_aftype",
    "formrange_language",
];

/// The source's XFA variables, as far as the package's metadata records them.
fn source_variables(metadata: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .iter()
        .filter(|(key, _)| !DERIVED_METADATA.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The form's languages from the metadata's `formrange_language` (`DE,EN,SP`).
///
/// The metadata files a language under the profile's own code, which for
/// Spanish is `sp`, not the `es` a source is detected as; the profile's
/// synonyms (`sp = ["es"]`) name the ISO 639-1 code to use instead.
fn form_languages(metadata: &BTreeMap<String, String>) -> Result<Vec<String>, Error> {
    let listed = metadata
        .get("formrange_language")
        .filter(|l| !l.trim().is_empty())
        .ok_or_else(|| {
            Error::Package("the form metadata names no languages (`formrange_language`)".into())
        })?;
    let profile = crate::profiles::load_aem_profile(PROFILE).map_err(Error::Profile)?;
    let mut languages: Vec<String> = Vec::new();
    for code in listed.split(',').map(|c| c.trim().to_lowercase()) {
        let iso = profile
            .language_synonyms
            .get(&code)
            .and_then(|synonyms| synonyms.iter().find(|s| s.len() == 2))
            .cloned()
            .unwrap_or(code);
        if !languages.contains(&iso) {
            languages.push(iso);
        }
    }
    Ok(languages)
}

/// Reject what the writer would otherwise silently drop or misplace.
fn validate(doc: &UbsAemDocument) -> Result<(), Error> {
    if !matches!(doc.form, AemNodeTranslated::Root { .. }) {
        return Err(Error::InvalidDocument("`form` must be a Root node".into()));
    }
    if doc.languages.is_empty() {
        return Err(Error::InvalidDocument(
            "`languages` must name at least one language".into(),
        ));
    }
    let unlisted: Vec<String> = doc
        .form
        .text_languages()
        .into_iter()
        .filter(|lang| !doc.languages.contains(lang))
        .collect();
    if !unlisted.is_empty() {
        return Err(Error::InvalidDocument(format!(
            "texts are written in {unlisted:?}, which `languages` ({:?}) does not list; \
             add them or remove those translations",
            doc.languages
        )));
    }
    Ok(())
}

/// Every party is one Repeatable around one partner generic, and every
/// fragment's `init_hide` and `init_show` name sub-panels the fragment
/// library says it has, as [`partner::init_problems`] requires.
fn check_parties(doc: &UbsAemDocument, library: &[ParsedFragment]) -> Result<(), Error> {
    let mut problems = Vec::new();
    doc.form.visit(&mut |node| match node {
        AemNodeTranslated::Fragment {
            name,
            frag_ref,
            init_hide,
            init_show,
            ..
        } if !init_hide.is_empty() || !init_show.is_empty() => {
            if !partner::is_partner_generic(frag_ref) {
                problems.push(format!(
                    "`init_hide` and `init_show` set the sub-panels of a UBS partner generic ({}), \
                     and `{name}` ({frag_ref}) is none",
                    partner::PARTNER_GENERICS.join(", ")
                ));
            } else if let Some(sub_panels) = partner::sub_panels(library, frag_ref) {
                problems.extend(partner::init_problems(name, sub_panels, init_hide, init_show));
            } else {
                problems.push(format!(
                    "`{name}` references {frag_ref}, which the fragment library does not hold"
                ));
            }
        }
        AemNodeTranslated::Repeatable { name, children, .. } => {
            let parties = children
                .iter()
                .filter(|c| {
                    matches!(c, AemNodeTranslated::Fragment { frag_ref, .. } if partner::is_partner_generic(frag_ref))
                })
                .count();
            if parties > 1 {
                problems.push(format!(
                    "`{name}` holds {parties} partner generics: a party is one Repeatable around one \
                     partner generic, with at most a content Panel beside it for fields the generic lacks"
                ));
            }
        }
        _ => {}
    });
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidDocument(problems.join("; ")))
    }
}

/// The profile's configuration, resolved against a document's variables,
/// header and languages.
fn config_for(
    variables: &BTreeMap<String, String>,
    header: Option<&str>,
    languages: &[String],
) -> Result<AemConfig, Error> {
    let variables: HashMap<String, String> = variables.clone().into_iter().collect();
    let mut ctx = Context::new(languages[0].clone(), variables);
    ctx.header = header.map(String::from);
    let mut config = crate::profiles::load_aem_config(PROFILE, &ctx).map_err(Error::AemConfig)?;
    config.languages = languages.to_vec();
    Ok(config)
}

/// Lower the document against `config`, re-deriving the `bindRef`s from the
/// lowered tree when the config binds to a schema, so the package's `bindRef`s
/// and its XSD always describe the tree as it is now.
fn lower(
    doc: &UbsAemDocument,
    config: &AemConfig,
) -> (AemNode, crate::aem::I18nDict, Vec<LowerConflict>) {
    let (mut node, dict, conflicts) = doc
        .form
        .lower_checked(&config.master_language, &config.languages);
    if let Some(xsd_config) = config.xsd_config.as_ref().filter(|_| config.bind_to_xsd) {
        let result = generate_xsd_from_aem(&node, xsd_config, &config.fragments);
        apply_bind_refs(&mut node, &result.bind_refs);
    }
    (node, dict, conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(children: serde_json::Value) -> serde_json::Value {
        json!({
            "variables": {"formrange_code": "AAEV", "formrange_entity": "019"},
            "languages": ["en", "de"],
            "form": {"type": "Root", "title": {"en": "Form"}, "children": children}
        })
    }

    fn draw(name: &str, content: serde_json::Value) -> serde_json::Value {
        json!({
            "type": "TextDraw", "uuid": "00000000-0000-0000-0000-000000000001", "name": name,
            "content": content, "visible": true, "colspan": 12, "dor_colspan": null
        })
    }

    /// A misspelled placement flag would otherwise be dropped, and the draw it
    /// was meant to keep out of the DoR would reach it.
    #[test]
    fn a_misspelled_node_field_is_refused() {
        let mut node = draw("ST_A", json!({"en": "Internal"}));
        node["sumary_exclude"] = json!(true);
        let Err(Error::InvalidDocument(message)) =
            UbsAemDocument::from_json(&document(json!([node])))
        else {
            panic!("an unknown field must be refused");
        };
        assert!(
            message.contains("/form/children/0/sumary_exclude"),
            "{message}"
        );
    }

    #[test]
    fn a_field_left_at_its_default_is_accepted() {
        let mut node = draw("ST_A", json!({"en": "Text"}));
        node["dor_exclude"] = json!(false);
        node["passthrough"] = json!({});
        UbsAemDocument::from_json(&document(json!([node]))).expect("defaults change nothing");
    }

    /// One page holding one fragment, with the sub-panels it hides.
    fn fragment_page(frag_ref: &str, init_hide: serde_json::Value) -> serde_json::Value {
        document(json!([{
            "type": "Panel", "uuid": "00000000-0000-0000-0000-000000000010", "name": "PN_Client",
            "title": {"en": "Client"}, "is_page": true, "visible": true, "is_conditional": false,
            "dor_num_cols": null, "colspan": 12, "dor_colspan": null, "bind_ref": null,
            "frag_ref": null,
            "children": [{
                "type": "Fragment", "uuid": "00000000-0000-0000-0000-000000000011",
                "name": "PN_CPGRP", "frag_ref": frag_ref, "visible": true, "bind_ref": null,
                "init_hide": init_hide
            }]
        }]))
    }

    const CONTRACTUAL_PARTNER: &str =
        "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1";

    fn form_xml(package: &[u8]) -> String {
        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(package)).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut xml = String::new();
                archive.by_index(i).unwrap().read_to_string(&mut xml).unwrap();
                xml
            })
            .find(|xml| xml.contains("guideContainer"))
            .expect("the package holds the form")
    }

    /// A partner generic's unneeded sub-panels are hidden by one Initialize
    /// rule on the fragment, and a decode reads them back.
    #[test]
    fn init_hide_is_one_initialize_rule_on_the_fragment_and_decodes_back() {
        let doc = UbsAemDocument::from_json(&fragment_page(
            CONTRACTUAL_PARTNER,
            json!(["PN_EntityBasic", "PN_Address"]),
        ))
        .unwrap();
        let build = encode(&doc).unwrap();
        let xml = form_xml(&build.package);
        assert!(
            xml.contains(concat!(
                r"window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);\\n",
                "window.forms.ubs.hideAFHideDor(this.PN_Address);"
            )),
            "{xml}"
        );
        assert_eq!(xml.matches("&quot;event&quot;:&quot;Initialize&quot;").count(), 1);

        let decoded = decode(&build.package).unwrap();
        let mut hidden = Vec::new();
        decoded.form.visit(&mut |node| {
            if let AemNodeTranslated::Fragment { init_hide, .. } = node {
                hidden.extend(init_hide.iter().map(|n| n.as_str().to_string()));
            }
        });
        assert_eq!(hidden, ["PN_EntityBasic", "PN_Address"]);
    }

    /// A translation that spans lines and carries `>` and `'` comes back
    /// from the package as it went in: the dictionary spells a newline as
    /// `&#xa;` (an XML reader turns a raw one into a space) and leaves `>`
    /// and `'` as AEM does.
    #[test]
    fn a_multi_line_translation_survives_the_dictionary() {
        let field = json!({
            "type": "TextField", "uuid": "00000000-0000-0000-0000-000000000020",
            "name": "TXT_Note", "label": {"en": "Note", "de": "Zeile 1\nZeile 2 > 1, l'autre"},
            "mandatory": false, "visible": true,
            "max_chars": null, "colspan": 12, "dor_colspan": null, "bind_ref": null
        });
        let doc = UbsAemDocument::from_json(&document(json!([{
            "type": "Panel", "uuid": "00000000-0000-0000-0000-000000000010", "name": "PN_Client",
            "title": {"en": "Client", "de": "Kunde"}, "is_page": true, "visible": true,
            "is_conditional": false, "dor_num_cols": null, "colspan": 12, "dor_colspan": null,
            "bind_ref": null, "frag_ref": null, "children": [field]
        }])))
        .unwrap();
        let build = encode(&doc).unwrap();

        let decoded = decode(&build.package).unwrap();
        let mut label = None;
        decoded.form.visit(&mut |node| {
            if let AemNodeTranslated::TextField { name, label: text, .. } = node
                && name == "TXT_Note"
            {
                label = text.get("de").map(str::to_owned);
            }
        });
        assert_eq!(label.as_deref(), Some("Zeile 1\nZeile 2 > 1, l'autre"));

        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&build.package)).unwrap();
        let dictionary = (0..archive.len())
            .map(|i| {
                let mut entry = archive.by_index(i).unwrap();
                let mut xml = String::new();
                entry.read_to_string(&mut xml).unwrap();
                (entry.name().to_owned(), xml)
            })
            .find(|(name, _)| name.ends_with("/dictionary/de.xml"))
            .expect("the package carries the German dictionary")
            .1;
        assert!(
            dictionary.contains("sling:message=\"Zeile 1&#xa;Zeile 2 > 1, l'autre\""),
            "{dictionary}"
        );
    }

    /// The value of `name` on the first `tag` element of `xml`.
    fn attribute(xml: &str, tag: &str, name: &str) -> Option<String> {
        let element = xml.split(&format!("<{tag}")).nth(1)?.split('>').next()?;
        let value = element.split(&format!(" {name}=\"")).nth(1)?;
        Some(value.split('"').next()?.to_owned())
    }

    /// The bound package names the schema it binds to the way the issued
    /// corpus does (`AFForms/<code>.xsd`, root `UBSAF_<code>`), the schema is
    /// in the package at that path, and every `bindRef` is a path under its
    /// root element. The unbound package names no schema.
    #[test]
    fn the_bound_package_names_the_schema_its_bind_refs_use() {
        use std::io::Read;
        let field = json!({
            "type": "TextField", "uuid": "00000000-0000-0000-0000-000000000020",
            "name": "TXT_Name", "label": {"en": "Name"}, "mandatory": false, "visible": true,
            "max_chars": null, "colspan": 12, "dor_colspan": null, "bind_ref": null
        });
        let doc = UbsAemDocument::from_json(&document(json!([{
            "type": "Panel", "uuid": "00000000-0000-0000-0000-000000000010", "name": "PN_Client",
            "title": {"en": "Client"}, "is_page": true, "visible": true, "is_conditional": false,
            "dor_num_cols": null, "colspan": 12, "dor_colspan": null, "bind_ref": null,
            "frag_ref": null, "children": [field]
        }])))
        .unwrap();
        let build = encode(&doc).unwrap();

        let unbound = form_xml(&build.package);
        assert_eq!(attribute(&unbound, "guideContainer", "xsdRef"), None);
        assert!(!unbound.contains("bindRef="));

        let bound_zip = build.bound_package.expect("the profile builds a bound twin");
        let bound = form_xml(&bound_zip);
        let xsd_ref = "/content/dam/formsanddocuments/afforms_xsd/AFForms/AAEV.xsd";
        assert_eq!(attribute(&bound, "guideContainer", "schemaType").as_deref(), Some("xmlschema"));
        assert_eq!(attribute(&bound, "guideContainer", "xsdRef").as_deref(), Some(xsd_ref));
        assert_eq!(
            attribute(&bound, "guideContainer", "xsdRootElement").as_deref(),
            Some("UBSAF_AAEV")
        );
        let bind_refs: Vec<&str> = bound
            .split("bindRef=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert!(!bind_refs.is_empty(), "the bound form binds its fields:\n{bound}");
        assert!(bind_refs.iter().all(|r| r.starts_with("/UBSAF_AAEV/")), "{bind_refs:?}");

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bound_zip)).unwrap();
        let mut schema = String::new();
        archive
            .by_name(&format!("jcr_root{xsd_ref}"))
            .expect("the schema is where xsdRef points")
            .read_to_string(&mut schema)
            .unwrap();
        assert!(schema.contains("name=\"UBSAF_AAEV\""), "{schema}");
    }

    /// The same document builds the same package: nothing in it depends on
    /// when it was built.
    #[test]
    fn encoding_is_a_function_of_the_document() {
        let doc = UbsAemDocument::from_json(&fragment_page(CONTRACTUAL_PARTNER, json!([]))).unwrap();
        let (first, second) = (encode(&doc).unwrap(), encode(&doc).unwrap());
        assert!(first.package == second.package, "two encodes differ");
        assert!(first.bound_package == second.bound_package);
    }

    /// Only a partner generic has sub-panels a form hides this way.
    #[test]
    fn init_hide_on_another_fragment_is_refused() {
        let doc = UbsAemDocument::from_json(&fragment_page(
            "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1",
            json!(["PN_Date"]),
        ))
        .unwrap();
        let Err(Error::InvalidDocument(message)) = encode(&doc) else {
            panic!("init_hide on a signature fragment must be refused");
        };
        assert!(message.contains("PN_CPGRP"), "{message}");
    }

    /// [`fragment_page`] with the fragment also showing `init_show`.
    fn fragment_page_showing(
        frag_ref: &str,
        init_hide: serde_json::Value,
        init_show: serde_json::Value,
    ) -> serde_json::Value {
        let mut page = fragment_page(frag_ref, init_hide);
        page["form"]["children"][0]["children"][0]["init_show"] = init_show;
        page
    }

    fn refusal(json: serde_json::Value) -> String {
        let doc = UbsAemDocument::from_json(&json).unwrap();
        match encode(&doc) {
            Err(Error::InvalidDocument(message)) => message,
            Err(other) => panic!("refused for another reason: {other}"),
            Ok(_) => panic!("the document must be refused"),
        }
    }

    /// The hidden sub-panels a form needs are shown by the same Initialize
    /// rule that hides the ones it does not, every call in the order the
    /// fragment holds its sub-panels, and a load gives both lists back.
    #[test]
    fn init_show_and_init_hide_are_one_initialize_rule_in_fragment_order_and_decode_back() {
        let doc = UbsAemDocument::from_json(&fragment_page_showing(
            CONTRACTUAL_PARTNER,
            json!(["PN_Address", "PN_EntityBasic"]),
            json!(["PN_DateIncorporation", "PN_DOBNationality"]),
        ))
        .unwrap();
        let build = encode(&doc).unwrap();
        let xml = form_xml(&build.package);
        assert!(
            xml.contains(concat!(
                r"window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);\\n",
                r"window.forms.ubs.hideAFHideDor(this.PN_Address);\\n",
                r"window.forms.ubs.showAFShowDor(this.PN_DOBNationality);\\n",
                "window.forms.ubs.showAFShowDor(this.PN_DateIncorporation);"
            )),
            "{xml}"
        );
        assert_eq!(xml.matches("&quot;event&quot;:&quot;Initialize&quot;").count(), 1);

        let decoded = decode(&build.package).unwrap();
        let mut lists = Vec::new();
        decoded.form.visit(&mut |node| {
            if let AemNodeTranslated::Fragment { init_hide, init_show, .. } = node {
                let names = |list: &[crate::aem::ComponentName]| {
                    list.iter().map(|n| n.as_str().to_string()).collect::<Vec<_>>()
                };
                lists.push((names(init_hide), names(init_show)));
            }
        });
        assert_eq!(
            lists,
            [(
                vec!["PN_EntityBasic".to_string(), "PN_Address".to_string()],
                vec!["PN_DOBNationality".to_string(), "PN_DateIncorporation".to_string()]
            )]
        );
    }

    /// Only a partner generic ships sub-panels hidden that a form shows.
    #[test]
    fn init_show_on_another_fragment_is_refused() {
        let message = refusal(fragment_page_showing(
            "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1",
            json!([]),
            json!(["PN_DOBNationality"]),
        ));
        assert!(message.contains("PN_CPGRP"), "{message}");
    }

    /// Showing a sub-panel the generic already shows, or one it does not
    /// have, does nothing at runtime: it is a mistake, so it is refused.
    #[test]
    fn showing_a_sub_panel_that_is_not_shipped_hidden_is_refused() {
        let message = refusal(fragment_page_showing(
            CONTRACTUAL_PARTNER,
            json!([]),
            json!(["PN_Address"]),
        ));
        assert!(message.contains("already shows"), "{message}");
        let message = refusal(fragment_page_showing(
            CONTRACTUAL_PARTNER,
            json!([]),
            json!(["PN_PlaceOfBirth"]),
        ));
        assert!(message.contains("does not have"), "{message}");
    }

    /// What a fragment can show is what the library says it ships hidden: a
    /// `Basic` partner generic ships its company name shown and has no
    /// address at all.
    #[test]
    fn a_basic_variant_is_held_to_its_own_sub_panels() {
        const BASIC: &str =
            "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_PartnertoPartnerGenericBasic1";
        let shown = refusal(fragment_page_showing(BASIC, json!([]), json!(["PN_EntityBasic"])));
        assert!(shown.contains("already shows"), "{shown}");
        let missing = refusal(fragment_page_showing(BASIC, json!(["PN_Address"]), json!([])));
        assert!(missing.contains("does not have"), "{missing}");
        let doc = UbsAemDocument::from_json(&fragment_page_showing(
            BASIC,
            json!(["PN_EntityBasic"]),
            json!(["PN_DOBNationality"]),
        ))
        .unwrap();
        encode(&doc).expect("hiding its company name and showing its date of birth is valid");
    }

    #[test]
    fn a_sub_panel_both_hidden_and_shown_is_refused() {
        let message = refusal(fragment_page_showing(
            CONTRACTUAL_PARTNER,
            json!(["PN_EntityBasic"]),
            json!(["PN_EntityBasic"]),
        ));
        assert!(message.contains("both hides and shows `PN_EntityBasic`"), "{message}");
    }

    /// The names are written into a script as they are, so only identifiers
    /// get in.
    #[test]
    fn an_init_hide_entry_that_is_not_a_component_name_is_refused() {
        let Err(Error::InvalidDocument(message)) = UbsAemDocument::from_json(&fragment_page(
            CONTRACTUAL_PARTNER,
            json!(["PN_Address); alert(1"]),
        )) else {
            panic!("a script fragment must not pass as a component name");
        };
        assert!(message.contains("not a component name"), "{message}");
    }

    /// The dictionary holds one translation per master text, so a text
    /// translated two ways would ship only one of them.
    #[test]
    fn a_text_translated_two_ways_is_refused() {
        let doc = UbsAemDocument::from_json(&document(json!([
            draw(
                "ST_A",
                json!({"en": "<p>Account</p>", "de": "<p>Konto</p>"})
            ),
            draw(
                "ST_B",
                json!({"en": "<p>Account</p>", "de": "<p>Depot</p>"})
            ),
        ])))
        .unwrap();
        let Err(Error::InvalidDocument(message)) = encode(&doc) else {
            panic!("a conflicting translation must be refused");
        };
        assert!(
            message.contains("Konto") && message.contains("Depot"),
            "{message}"
        );
    }
}
