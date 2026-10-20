//! The UBS AEM document: what the Conversion Agent authors, and what `encode`
//! turns into a FileVault package.
//!
//! The document is the multilingual form tree ([`AemNodeTranslated`]) plus the
//! little the UBS profile needs from the source form to place and name it: the
//! XFA template variables and the form's languages. The source's master-page
//! header is authored like any other text: hidden `TextDraw`s that print in the
//! DoR's header slots (see [`AemAttrs::dor_header_slot`](crate::aem::AemAttrs)).
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

/// Bring a document recorded before the header became authored text up to date,
/// in place. Returns whether anything changed.
///
/// Such a document carries the source's master-page header as one plain string
/// (`header`), from which the writer printed the legal entity in the DoR's
/// second header slot and dropped the validity line. Now the header is the
/// document's own text, so `header` becomes the slot draws that text is
/// authored as: the legal entity in `slot2` (bold, what qualifies it plain, as
/// the writer printed it) and the validity line in `slot3`, each a hidden
/// `TextDraw` on the first page, after the banking relationship where the page
/// has one. The old string was written in the form's master language and
/// nothing else, so that is the one language the draws carry; the others are
/// for the author to add. Anything this cannot place is refused rather than
/// dropped.
pub fn migrate(value: &mut serde_json::Value) -> Result<bool, Error> {
    use serde_json::{Value, json};
    let invalid = |message: String| Error::InvalidDocument(message);
    let Some(object) = value.as_object_mut() else {
        return Err(invalid("a document is a JSON object".into()));
    };
    let header = match object.remove("header") {
        None | Some(Value::Null) => return Ok(false),
        Some(Value::String(header)) => header,
        Some(other) => return Err(invalid(format!("`header` is not a string: {other}"))),
    };
    let lines: Vec<&str> = header
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Ok(true);
    }
    let variables: BTreeMap<String, String> = serde_json::from_value(
        object.get("variables").cloned().unwrap_or(Value::Null),
    )
    .map_err(|e| invalid(format!("`variables`: {e}")))?;
    let languages: Vec<String> = serde_json::from_value(
        object.get("languages").cloned().unwrap_or(Value::Null),
    )
    .map_err(|e| invalid(format!("`languages`: {e}")))?;
    if languages.is_empty() {
        return Err(invalid("`languages` must name at least one language".into()));
    }
    let language = config_for(&variables, &languages)?.base_language();

    // A bare number (AABF_019's `0319 00`, the banking relationship's
    // default) is neither: the old writer dropped it, and so does this.
    let (validity, entity): (Vec<&str>, Vec<&str>) = lines
        .into_iter()
        .filter(|l| !is_bare_number(l))
        .partition(|l| is_validity_line(l));
    let mut draws = Vec::new();
    if let Some((first, rest)) = entity.split_first() {
        // `UBS Europe SE (Succursale Italia)`: the entity in bold, what
        // qualifies it plain.
        let (name, qualifier) = match first.find(" (") {
            Some(at) => (&first[..at], first[at..].trim()),
            None => (*first, ""),
        };
        let tail = std::iter::once(qualifier)
            .chain(rest.iter().copied())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let mut html = format!("<b>{}</b>", crate::util::escape_html(name));
        if !tail.is_empty() {
            html.push(' ');
            html.push_str(&crate::util::escape_html(&tail));
        }
        draws.push(("ST_HeaderSlot2", "slot2", html));
    }
    if !validity.is_empty() {
        draws.push(("ST_HeaderSlot3", "slot3", crate::util::escape_html(&validity.join(" "))));
    }

    let page = object
        .get_mut("form")
        .and_then(|form| form.get_mut("children"))
        .and_then(Value::as_array_mut)
        .and_then(|pages| {
            pages.iter_mut().find(|p| {
                p.get("type").and_then(Value::as_str) == Some("Panel")
                    && p.get("is_page").and_then(Value::as_bool) == Some(true)
            })
        })
        .and_then(|page| page.get_mut("children"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            invalid("the document has a `header` and no page to author it on".into())
        })?;
    for (name, _, _) in &draws {
        if page.iter().any(|c| c.get("name").and_then(Value::as_str) == Some(name)) {
            return Err(invalid(format!(
                "the first page already holds a `{name}`, so the `header` cannot become one"
            )));
        }
    }
    let at = page
        .iter()
        .position(|c| c.get("type").and_then(Value::as_str) == Some("Preface"))
        .map_or(0, |i| i + 1);
    for (offset, (name, slot, html)) in draws.into_iter().enumerate() {
        page.insert(
            at + offset,
            json!({
                "type": "TextDraw",
                "uuid": uuid::Uuid::new_v4(),
                "name": name,
                "content": {language.clone(): format!("<p>{html}</p>\n")},
                "visible": false,
                "colspan": 12,
                "dor_colspan": null,
                "always_in_pdf": true,
                "show_if_hidden": true,
                "summary_exclude": true,
                "dor_header_slot": slot,
            }),
        );
    }
    Ok(true)
}

/// A header line that dates the form rather than naming its issuer.
fn is_validity_line(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "gültig", "gueltig", "valid", "valido", "valida", "valable", "edition", "ausgabe",
        "version", "stand",
    ];
    let lower = line.to_lowercase();
    if PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // A bare date (`02.01.2018`, `1/2018`) is the same thing without a word.
    let digits = line.chars().filter(|c| c.is_ascii_digit()).count();
    digits >= 4
        && line.chars().any(|c| "./-".contains(c))
        && line.chars().all(|c| c.is_ascii_digit() || " ./-".contains(c))
}

/// A line of digits and spaces alone, which neither dates the form nor names
/// its issuer.
fn is_bare_number(line: &str) -> bool {
    line.chars().any(|c| c.is_ascii_digit()) && line.chars().all(|c| c.is_ascii_digit() || c == ' ')
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
    let config = config_for(&doc.variables, &doc.languages)?;
    check_parties(doc, &config.fragments)?;

    let (node, translations, conflicts) = lower(doc, &config);
    check_header_slots(&node)?;
    check_letterhead(&node)?;
    check_inputs(&node)?;
    check_language_gates(&node, &doc.languages)?;
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
/// into every package (`metadataTextDraw`). The DoR header slots come back as
/// the `TextDraw`s they are.
///
/// What the writer expands is folded back into the nodes it is written for
/// (see [`crate::aem::unexpand`]), and what it regenerates is not kept:
/// a fragment's Initialize script, for one, becomes the profile's again.
pub fn decode(package: &[u8]) -> Result<UbsAemDocument, Error> {
    let parsed = parse_aem_zip(package).map_err(Error::Package)?;
    let variables = source_variables(&parsed.metadata);
    let languages = form_languages(&parsed.metadata)?;
    let config = config_for(&variables, &languages)?;
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
        // What a document keeps of a loaded fragment's Initialize rule: a
        // partner generic's sub-panels, and a signature block's shared place
        // and date. The rest is the profile's, which the writer makes again.
        if let AemNodeTranslated::Fragment { uuid, frag_ref, init, .. } = node
            && let Some(loaded) = parsed.fragment_init.get(uuid)
        {
            if is_partner_generic(frag_ref) {
                init.init_hide = loaded.init_hide.clone();
                init.init_show = loaded.init_show.clone();
            }
            if crate::aem::fragment_init::file_of(frag_ref) == crate::aem::fragment_init::SIGNATURE_GENERIC {
                init.shared_place_date = loaded.shared_place_date;
            }
        }
    });
    Ok(UbsAemDocument {
        variables,
        languages,
        form,
    })
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
    for code in listed.split(',') {
        let iso = iso_language_in(&profile, code);
        if !languages.contains(&iso) {
            languages.push(iso);
        }
    }
    Ok(languages)
}

/// The document's language (ISO 639-1, as a source is detected) for a code
/// the form's metadata files it under: `es` for `SP`, `de` for `DE`.
pub fn iso_language(metadata_code: &str) -> Result<String, Error> {
    let profile = crate::profiles::load_aem_profile(PROFILE).map_err(Error::Profile)?;
    Ok(iso_language_in(&profile, metadata_code))
}

fn iso_language_in(profile: &crate::aem::AemProfile, metadata_code: &str) -> String {
    let code = metadata_code.trim().to_lowercase();
    profile
        .language_synonyms
        .get(&code)
        .and_then(|synonyms| synonyms.iter().find(|s| s.len() == 2))
        .cloned()
        .unwrap_or(code)
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

/// Every party is one Repeatable around its partner generic (several copies of
/// it when the source splits the party around fields of its own, as
/// [`partner::split_problems`] requires), and every fragment's Initialize
/// lists name components the fragment library says it has, as
/// [`fragment_init::problems`](crate::aem::fragment_init::problems) requires.
fn check_parties(doc: &UbsAemDocument, library: &[ParsedFragment]) -> Result<(), Error> {
    let mut problems = Vec::new();
    // A fragment that is a Repeatable's direct child repeats with its rows.
    let mut in_rows = Vec::new();
    doc.form.visit(&mut |node| {
        if let AemNodeTranslated::Repeatable { children, .. } = node {
            in_rows.extend(children.iter().filter_map(|c| match c {
                AemNodeTranslated::Fragment { uuid, .. } => Some(*uuid),
                _ => None,
            }));
        }
    });
    doc.form.visit(&mut |node| match node {
        AemNodeTranslated::Fragment { uuid, name, frag_ref, init, .. } => {
            problems.extend(crate::aem::fragment_init::problems(
                name,
                frag_ref,
                init,
                in_rows.contains(uuid),
                library,
            ));
        }
        AemNodeTranslated::Repeatable { name, children, .. } => {
            let parts: Vec<partner::PartyPart<'_>> = children
                .iter()
                .filter_map(|c| match c {
                    AemNodeTranslated::Fragment { name, frag_ref, init, .. }
                        if partner::is_partner_generic(frag_ref) =>
                    {
                        Some(partner::PartyPart { name: name.as_str(), frag_ref: frag_ref.as_str(), init })
                    }
                    _ => None,
                })
                .collect();
            if parts.len() > 1 {
                problems.extend(partner::split_problems(name, &parts, library));
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

/// A form is a letter at most once, and its letterhead is where the letter
/// layout reads it, holding what the letter window can print:
///
/// - one letterhead, a direct child of the first page, after the banking
///   relationship (the `Preface`) where the page has one;
/// - a sender takes the DoR header's `slot2`, so nothing else may print there;
/// - without a sender some other node must fill a header slot (the banking
///   relationship fills `slot1`): the summary script writes the letter's
///   header, and with it the receiver window, date and subject, only when a
///   slot has content;
/// - its addresses and texts are ones the letter window prints
///   ([`letterhead::problems`](crate::aem::letterhead::problems)).
fn check_letterhead(root: &AemNode) -> Result<(), Error> {
    fn walk<'a>(
        node: &'a AemNode,
        letterheads: &mut Vec<&'a AemNode>,
        slots: &mut Vec<(&'a str, &'a str)>,
        preface: &mut bool,
    ) {
        match node {
            AemNode::Letterhead { .. } => letterheads.push(node),
            AemNode::Preface { .. } => *preface = true,
            _ => {}
        }
        let slot = node.attrs().and_then(|a| a.dor_header_slot.as_deref());
        if let (Some(name), Some(slot)) = (node.name(), slot) {
            slots.push((name, slot));
        }
        if let AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } = node
        {
            children.iter().for_each(|c| walk(c, letterheads, slots, preface));
        }
    }
    let (mut letterheads, mut slots, mut preface) = (Vec::new(), Vec::new(), false);
    walk(root, &mut letterheads, &mut slots, &mut preface);
    let mut problems = Vec::new();
    if letterheads.len() > 1 {
        let names: Vec<String> =
            letterheads.iter().filter_map(|l| l.name()).map(|n| format!("`{n}`")).collect();
        problems.push(format!(
            "a form has at most one letterhead, and this one has {}: {}",
            names.len(),
            names.join(", ")
        ));
    }
    for letterhead in letterheads {
        let AemNode::Letterhead {
            uuid, name, sender, receiver, subject, your_reference, our_reference, ..
        } = letterhead
        else {
            unreachable!("only letterheads are collected");
        };
        let references = [your_reference.as_ref(), our_reference.as_ref()];
        problems.extend(
            crate::aem::letterhead::problems(sender.as_ref(), receiver, subject, references)
                .into_iter()
                .map(|p| format!("`{name}`: {p}")),
        );
        if let Ok(fields) = crate::aem::letterhead::subject_fields(subject) {
            problems.extend(subject_field_problems(root, name, &fields));
        }
        if !on_first_page_after_preface(root, *uuid) {
            problems.push(format!(
                "the letterhead `{name}` must be a direct child of the first page, after the banking \
                 relationship (`Preface`) where the page has one"
            ));
        }
        let slot2: Vec<String> = slots
            .iter()
            .filter(|(_, slot)| *slot == crate::aem::letterhead::SENDER_SLOT)
            .map(|(n, _)| format!("`{n}`"))
            .collect();
        if sender.is_some() && !slot2.is_empty() {
            problems.push(format!(
                "the letterhead `{name}` prints its sender in the DoR header's `{}`, which {} takes too: \
                 leave that slot to the letterhead, or set its `sender` to null",
                crate::aem::letterhead::SENDER_SLOT,
                slot2.join(", ")
            ));
        }
        if sender.is_none() && !preface && slots.is_empty() {
            problems.push(format!(
                "the letterhead `{name}` has no sender, and nothing else fills a DoR header slot, so the \
                 letter's header (receiver, date, subject) would not print: give it a `sender`, or author \
                 the header line the source prints (a slot draw)"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidDocument(problems.join("; ")))
    }
}

/// What makes a field a letter's subject names (`{NAME}`) one it cannot show:
/// it must be exactly one text or number field of the form, whose value is
/// what a person typed (a choice's value is its key).
fn subject_field_problems(root: &AemNode, letterhead: &str, fields: &[String]) -> Vec<String> {
    fn kinds<'a>(node: &'a AemNode, name: &str, out: &mut Vec<&'a AemNode>) {
        if node.name() == Some(name) {
            out.push(node);
        }
        if let AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } = node
        {
            children.iter().for_each(|c| kinds(c, name, out));
        }
    }
    let mut problems = Vec::new();
    for field in fields {
        let mut named = Vec::new();
        kinds(root, field, &mut named);
        match named.as_slice() {
            [] => problems.push(format!(
                "the letterhead `{letterhead}`'s subject names `{{{field}}}`, and the form has no field \
                 of that name"
            )),
            [AemNode::TextField { .. } | AemNode::NumberField { .. }] => {}
            [_] => problems.push(format!(
                "the letterhead `{letterhead}`'s subject names `{{{field}}}`, which is no text or number \
                 field: only a value a person types prints as it is"
            )),
            many => problems.push(format!(
                "the letterhead `{letterhead}`'s subject names `{{{field}}}`, and {} components share \
                 that name",
                many.len()
            )),
        }
    }
    problems
}

/// Whether the node `uuid` is a direct child of the first page, after its
/// `Preface` if it has one.
fn on_first_page_after_preface(root: &AemNode, uuid: uuid::Uuid) -> bool {
    let AemNode::Root { children, .. } = root else {
        return false;
    };
    let Some(AemNode::Panel { children, .. }) =
        children.iter().find(|c| matches!(c, AemNode::Panel { is_page: true, .. }))
    else {
        return false;
    };
    let Some(at) = children.iter().position(|c| c.uuid() == Some(uuid)) else {
        return false;
    };
    children[..at].iter().any(|c| matches!(c, AemNode::Preface { .. }))
        || !children.iter().any(|c| matches!(c, AemNode::Preface { .. }))
}

/// The DoR header slots a node may print in.
const HEADER_SLOTS: &[&str] = &["slot1", "slot2", "slot3"];

/// Every input's [`InputProps`](crate::aem::InputProps) names what the form
/// holds: a choice opens on one of its own options, a copy and a calculation
/// read inputs of the form other than the input itself, and an input takes
/// its value from a calculation or from a copy, not from both. Anything else
/// would be written as a rule that names a field AEM cannot find, or a value
/// no option shows.
fn check_inputs(root: &AemNode) -> Result<(), Error> {
    fn collect<'a>(node: &'a AemNode, inputs: &mut Vec<&'a AemNode>) {
        if node.input().is_some() {
            inputs.push(node);
        }
        if let AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } = node
        {
            for child in children {
                collect(child, inputs);
            }
        }
    }
    let mut inputs = Vec::new();
    collect(root, &mut inputs);
    let names: Vec<&str> = inputs.iter().filter_map(|node| node.name()).collect();
    let mut problems = Vec::new();
    for node in &inputs {
        let (Some(name), Some(input)) = (node.name(), node.input()) else {
            continue;
        };
        let options = match node {
            AemNode::RadioButton { options, .. }
            | AemNode::Dropdown { options, .. }
            | AemNode::Checkbox { options, .. } => Some(options),
            _ => None,
        };
        if let (Some(options), Some(value)) = (options, input.default_value.as_deref())
            && !options.iter().any(|option| option.value == value)
        {
            let values = options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>().join(", ");
            problems.push(format!(
                "`{name}` opens on `{value}`, which is none of its option values ({values})"
            ));
        }
        let mut sources: Vec<String> = Vec::new();
        if let Some(source) = &input.copy_from {
            sources.push(source.as_str().to_owned());
            if input.calculate.is_some() {
                problems.push(format!(
                    "`{name}` both calculates its value and copies `{}`: it takes one or the other",
                    source.as_str()
                ));
            }
        }
        if let Some(template) = &input.calculate {
            match crate::aem::letterhead::subject_references(template) {
                Ok(references) => sources.extend(references),
                Err(e) => problems.push(format!("`{name}` calculates its value from {e}")),
            }
        }
        for source in &sources {
            if source == name {
                problems.push(format!("`{name}` takes its value from itself"));
            } else if !names.contains(&source.as_str()) {
                problems.push(format!(
                    "`{name}` takes its value from `{source}`, which is no input of the form"
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidDocument(problems.join("; ")))
    }
}

/// Every node shown only in some languages (`only_languages`) names languages
/// the form ships, and not all of them; it is a node the writer can gate (an
/// input, a draw, a message box, an HTML block or a Panel that no condition
/// shows), authored visible: the gate decides when it shows.
fn check_language_gates(root: &AemNode, languages: &[String]) -> Result<(), Error> {
    fn walk(node: &AemNode, languages: &[String], problems: &mut Vec<String>) {
        if let Some(attrs) = node.attrs()
            && !attrs.only_languages.is_empty()
        {
            let name = node.name().unwrap_or("the node");
            for language in &attrs.only_languages {
                if !languages.contains(language) {
                    problems.push(format!(
                        "`{name}` shows only in `{language}`, which the form does not ship ({languages:?})"
                    ));
                }
            }
            if languages.iter().all(|l| attrs.only_languages.contains(l)) {
                problems.push(format!(
                    "`{name}` shows in every language of the form: leave `only_languages` empty"
                ));
            }
            let gateable = match node {
                AemNode::Panel { is_conditional, .. } => !is_conditional,
                AemNode::TextField { .. }
                | AemNode::NumberField { .. }
                | AemNode::DatePicker { .. }
                | AemNode::Dropdown { .. }
                | AemNode::Checkbox { .. }
                | AemNode::RadioButton { .. }
                | AemNode::TextDraw { .. }
                | AemNode::TitleDraw { .. }
                | AemNode::MessageBox { .. }
                | AemNode::HtmlDisplayer { .. } => true,
                _ => false,
            };
            if !gateable {
                problems.push(format!(
                    "`{name}` cannot be shown by language: only an input, a draw, a message box, an HTML \
                     block or a Panel no condition shows can; wrap it in a Panel and gate that"
                ));
            }
            let visible = match node {
                AemNode::Panel { visible, .. }
                | AemNode::TextField { visible, .. }
                | AemNode::NumberField { visible, .. }
                | AemNode::DatePicker { visible, .. }
                | AemNode::Dropdown { visible, .. }
                | AemNode::Checkbox { visible, .. }
                | AemNode::RadioButton { visible, .. }
                | AemNode::TextDraw { visible, .. }
                | AemNode::TitleDraw { visible, .. }
                | AemNode::MessageBox { visible, .. }
                | AemNode::HtmlDisplayer { visible, .. } => *visible,
                _ => true,
            };
            if !visible {
                problems.push(format!(
                    "`{name}` is shown by language, so it is authored visible: its gate shows and hides it"
                ));
            }
        }
        if let AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } = node
        {
            for child in children {
                walk(child, languages, problems);
            }
        }
    }
    let mut problems = Vec::new();
    walk(root, languages, &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidDocument(problems.join("; ")))
    }
}

/// Every `dorHeaderSlot` names one of the DoR's three header slots, and slot 1
/// is the banking relationship's: no master-page line takes it, and no field
/// either where the banking relationship fills it.
///
/// Two nodes may share a slot: the issued forms print alternative account
/// numbers in the same one, and only one of them is filled.
fn check_header_slots(root: &AemNode) -> Result<(), Error> {
    fn walk<'a>(node: &'a AemNode, slots: &mut Vec<(&'a str, &'a str, bool)>, preface: &mut bool) {
        if matches!(node, AemNode::Preface { .. }) {
            *preface = true;
        }
        if let (Some(name), Some(slot)) = (
            node.name(),
            node.attrs().and_then(|a| a.dor_header_slot.as_deref()),
        ) {
            slots.push((name, slot, node.is_header_slot_draw()));
        }
        if let AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } = node
        {
            for child in children {
                walk(child, slots, preface);
            }
        }
    }
    let mut slots = Vec::new();
    let mut preface = false;
    walk(root, &mut slots, &mut preface);
    let mut problems = Vec::new();
    for (name, slot, line) in slots {
        if !HEADER_SLOTS.contains(&slot) {
            problems.push(format!(
                "`{name}` prints in the DoR header slot `{slot}`, which is none of {HEADER_SLOTS:?}"
            ));
        } else if slot == "slot1" && (preface || line) {
            problems.push(format!(
                "`{name}` takes the DoR header slot `slot1`, which is the banking relationship's: \
                 the legal entity is `slot2` and the validity line `slot3`"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidDocument(problems.join("; ")))
    }
}

/// The profile's configuration, resolved against a document's variables and
/// languages.
fn config_for(
    variables: &BTreeMap<String, String>,
    languages: &[String],
) -> Result<AemConfig, Error> {
    let variables: HashMap<String, String> = variables.clone().into_iter().collect();
    let ctx = Context::new(languages[0].clone(), variables);
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
    // The static texts' text is shaped here, on the multilingual tree, so a
    // rewritten master and its translations stay one dictionary entry.
    let mut form = doc.form.clone();
    crate::aem::text_shape::shape_texts(&mut form);
    let (mut node, dict, conflicts) =
        form.lower_checked(&config.master_language, &config.languages);
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
    fn a_metadata_language_code_maps_to_the_documents_language() {
        assert_eq!(iso_language("SP").unwrap(), "es");
        assert_eq!(iso_language("DE").unwrap(), "de");
        assert_eq!(iso_language("en").unwrap(), "en");
    }

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
            if let AemNodeTranslated::Fragment { init, .. } = node {
                hidden.extend(init.init_hide.iter().map(|n| n.as_str().to_string()));
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

    /// A heading authored as bare text reaches the package as a paragraph, in
    /// the form and in the dictionary alike, so the platform can mark it as a
    /// heading and its translation stays keyed by it. A bare address in a static
    /// text is a link in every language.
    #[test]
    fn static_texts_are_paragraphs_and_their_addresses_links() {
        let heading = json!({
            "type": "TitleDraw", "uuid": "00000000-0000-0000-0000-000000000021",
            "name": "TTL_Contact", "heading_level": 3,
            "content": {"en": "Contact", "de": "Kontakt"}, "colspan": 12, "dor_colspan": null
        });
        let body = draw(
            "ST_Contact",
            json!({"en": "<p>Write to a@ubs.com.</p>", "de": "<p>Schreiben Sie an a@ubs.com.</p>"}),
        );
        let doc = UbsAemDocument::from_json(&document(json!([{
            "type": "Panel", "uuid": "00000000-0000-0000-0000-000000000010", "name": "PN_Info",
            "title": {"en": "Information", "de": "Information"}, "is_page": true, "visible": true,
            "is_conditional": false, "dor_num_cols": null, "colspan": 12, "dor_colspan": null,
            "bind_ref": null, "frag_ref": null, "children": [heading, body]
        }])))
        .unwrap();
        let build = encode(&doc).unwrap();

        let xml = form_xml(&build.package);
        assert!(xml.contains("_value=\"&lt;p>Contact&lt;/p>\""), "{xml}");
        let link =
            "&lt;a href=&quot;mailto:a@ubs.com&quot; target=&quot;_blank&quot;>a@ubs.com&lt;/a>";
        assert!(xml.contains(&format!("Write to {link}.")), "{xml}");

        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&build.package)).unwrap();
        let name = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_owned())
            .find(|n| n.ends_with("/dictionary/de.xml"))
            .expect("the package carries the German dictionary");
        let mut dictionary = String::new();
        archive.by_name(&name).unwrap().read_to_string(&mut dictionary).unwrap();
        assert!(
            dictionary.contains("sling:key=\"fd_&lt;p>Contact&lt;/p>\"")
                && dictionary.contains("sling:message=\"&lt;p>Kontakt&lt;/p>\""),
            "the heading's translation stays keyed by its master: {dictionary}"
        );
        assert!(dictionary.contains(&format!("Schreiben Sie an {link}.")), "{dictionary}");
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

    /// Any fragment hides what it has, by its path inside the fragment, and
    /// nothing it does not have.
    #[test]
    fn init_hide_names_a_component_the_fragment_has() {
        let signature = "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1";
        let doc = UbsAemDocument::from_json(&fragment_page(signature, json!(["PN_Date"]))).unwrap();
        let Err(Error::InvalidDocument(message)) = encode(&doc) else {
            panic!("init_hide of a component the signature fragment does not have must be refused");
        };
        assert!(message.contains("which it does not have"), "{message}");
        let doc = UbsAemDocument::from_json(&fragment_page(
            signature,
            json!(["PN_GenericSignature.TXT_Place_Generic"]),
        ))
        .unwrap();
        let build = encode(&doc).expect("a component the fragment has can be hidden");
        let xml = form_xml(&build.package);
        assert!(xml.contains("hideAFHideDor(this.PN_GenericSignature.TXT_Place_Generic)"), "{xml}");
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
            if let AemNodeTranslated::Fragment { init, .. } = node {
                let (init_hide, init_show) = (&init.init_hide, &init.init_show);
                let names = |list: &[crate::aem::ComponentPath]| {
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
        assert!(message.contains("which it does not have"), "{message}");
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
        assert!(message.contains("not a component path"), "{message}");
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

    /// A legacy document with `header`, its first page holding `children`.
    fn legacy(header: &str, children: serde_json::Value) -> serde_json::Value {
        let mut value = page_document(children);
        value["header"] = json!(header);
        value
    }

    /// A document whose first page holds `children`.
    fn page_document(children: serde_json::Value) -> serde_json::Value {
        json!({
            "variables": {"formrange_code": "AAEV", "formrange_entity": "019"},
            "languages": ["en", "de"],
            "form": {"type": "Root", "title": {"en": "Form"}, "children": [{
                "type": "Panel", "uuid": "00000000-0000-0000-0000-0000000000a1",
                "name": "PN_Page", "title": {"en": "Page"}, "children": children,
                "is_page": true, "visible": true, "is_conditional": false,
                "dor_num_cols": null, "colspan": 12, "dor_colspan": null, "bind_ref": null
            }]}
        })
    }

    fn preface() -> serde_json::Value {
        json!({"type": "Preface", "uuid": "00000000-0000-0000-0000-0000000000b1", "name": "PN_BR"})
    }

    /// The first page's children after migrating `header`, as (name, slot, text).
    fn migrated(header: &str, children: serde_json::Value) -> Vec<(String, String, String)> {
        let mut value = legacy(header, children);
        assert!(migrate(&mut value).unwrap());
        assert!(value.get("header").is_none());
        let doc = UbsAemDocument::from_json(&value).expect("the migrated document reads");
        encode(&doc).expect("the migrated document encodes");
        value["form"]["children"][0]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap().to_string(),
                    c["dor_header_slot"].as_str().unwrap_or("").to_string(),
                    c["content"]["en"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }

    /// The legal entity goes to slot 2 (bold, what qualifies it plain) and the
    /// validity line to slot 3, right after the banking relationship, in the
    /// master language.
    #[test]
    fn a_legacy_header_becomes_the_slot_draws() {
        let children = migrated(
            "Valid from 09.10.2025\nUBS Europe SE (Succursale Italia)",
            json!([preface()]),
        );
        assert_eq!(
            children,
            vec![
                ("PN_BR".into(), "".into(), "".into()),
                (
                    "ST_HeaderSlot2".into(),
                    "slot2".into(),
                    "<p><b>UBS Europe SE</b> (Succursale Italia)</p>\n".into()
                ),
                ("ST_HeaderSlot3".into(), "slot3".into(), "<p>Valid from 09.10.2025</p>\n".into()),
            ]
        );
    }

    /// AABF_019's header opens with the banking relationship's default
    /// number, which is neither the entity nor a date.
    #[test]
    fn a_bare_number_in_a_legacy_header_is_dropped() {
        let children = migrated("0319 00\nUBS Europe SE", json!([preface()]));
        assert_eq!(
            children,
            vec![
                ("PN_BR".into(), "".into(), "".into()),
                ("ST_HeaderSlot2".into(), "slot2".into(), "<p><b>UBS Europe SE</b></p>\n".into()),
            ]
        );
    }

    #[test]
    fn a_legacy_header_of_one_kind_writes_only_its_slot() {
        let entity = migrated("UBS Europe SE", json!([preface()]));
        assert_eq!(entity.len(), 2);
        assert_eq!(entity[1].1, "slot2");
        // Without a banking relationship the draws open the page.
        let validity = migrated("Gültig ab 02.01.2018", json!([]));
        assert_eq!(
            validity,
            vec![("ST_HeaderSlot3".into(), "slot3".into(), "<p>Gültig ab 02.01.2018</p>\n".into())]
        );
    }

    #[test]
    fn a_document_without_a_header_is_left_alone() {
        let mut value = document(json!([]));
        let before = value.clone();
        assert!(!migrate(&mut value).unwrap());
        assert_eq!(value, before);
    }

    /// The header cannot be dropped, so a document with nowhere to put it is
    /// refused.
    #[test]
    fn a_legacy_header_with_no_page_is_refused() {
        let mut value = document(json!([]));
        value["header"] = json!("UBS Europe SE");
        let Err(Error::InvalidDocument(message)) = migrate(&mut value) else {
            panic!("a header with no page must be refused");
        };
        assert!(message.contains("no page"), "{message}");
    }

    fn slot_draw(name: &str, slot: &str) -> serde_json::Value {
        json!({
            "type": "TextDraw", "uuid": uuid::Uuid::new_v4(), "name": name,
            "content": {"en": "<p>UBS Europe SE</p>", "de": "<p>UBS Europe SE</p>"},
            "visible": false, "colspan": 12, "dor_colspan": null,
            "always_in_pdf": true, "show_if_hidden": true, "summary_exclude": true,
            "dor_header_slot": slot
        })
    }

    fn encode_refusal(value: serde_json::Value) -> String {
        let doc = UbsAemDocument::from_json(&value).unwrap();
        match encode(&doc) {
            Err(Error::InvalidDocument(message)) => message,
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("the document must be refused"),
        }
    }

    fn text_field(name: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut field = json!({
            "type": "TextField", "uuid": uuid::Uuid::new_v4(), "name": name,
            "label": {"en": name, "de": name}, "mandatory": false, "visible": true,
            "max_chars": null, "colspan": 6, "dor_colspan": null, "bind_ref": null
        });
        for (key, value) in extra.as_object().unwrap() {
            field[key] = value.clone();
        }
        field
    }

    fn radio(default_value: &str) -> serde_json::Value {
        json!({
            "type": "RadioButton", "uuid": uuid::Uuid::new_v4(), "name": "RB_Kind",
            "label": {"en": "Kind", "de": "Art"},
            "options": [
                {"label": {"en": "A", "de": "A"}, "value": "1"},
                {"label": {"en": "B", "de": "B"}, "value": "2"}
            ],
            "alignment": "Vertical", "mandatory": false, "visible": true, "colspan": 12,
            "dor_colspan": null, "conditions": [], "bind_ref": null,
            "default_value": default_value
        })
    }

    /// A choice opens on one of its own options, and a copy or a calculation
    /// reads another input the form holds; an input takes one of the two.
    #[test]
    fn input_properties_name_what_the_form_holds() {
        let unknown = encode_refusal(page_document(json!([radio("3")])));
        assert!(unknown.contains("`RB_Kind` opens on `3`"), "{unknown}");

        let missing = encode_refusal(page_document(json!([
            text_field("TXT_Copy", json!({"copy_from": "TXT_Gone"}))
        ])));
        assert!(missing.contains("`TXT_Gone`, which is no input"), "{missing}");

        let itself = encode_refusal(page_document(json!([
            text_field("TXT_Header", json!({"calculate": "0319 {TXT_Header}"}))
        ])));
        assert!(itself.contains("from itself"), "{itself}");

        let both = encode_refusal(page_document(json!([
            text_field("TXT_Account", json!({})),
            text_field("TXT_Header", json!({"calculate": "{TXT_Account}", "copy_from": "TXT_Account"}))
        ])));
        assert!(both.contains("one or the other"), "{both}");

        let build = encode(
            &UbsAemDocument::from_json(&page_document(json!([
                radio("2"),
                text_field("TXT_Account", json!({})),
                text_field("TXT_Header", json!({"calculate": "0319 {TXT_Account}", "read_only": true})),
                text_field("TXT_Holder", json!({"copy_from": "TXT_Name"})),
                text_field("TXT_Name", json!({"copy_from": "TXT_Holder"}))
            ])))
            .unwrap(),
        )
        .expect("the properties name what the form holds");
        let xml = build_form_xml(&build.package);
        for expected in [
            r#"_value="2""#,
            r#"enabled="{Boolean}false""#,
            "calculated-value",
            "copied-value",
            "fd:calc=",
            "fd:valueCommit=",
        ] {
            assert!(xml.contains(expected), "the form must carry {expected}");
        }
    }

    /// The form page of a built package.
    fn build_form_xml(package: &[u8]) -> String {
        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(package)).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let mut text = String::new();
            if file.read_to_string(&mut text).is_ok() && text.contains("guideContainer") {
                return text;
            }
        }
        panic!("the package holds the form");
    }

    /// Slot 1 is the banking relationship's, and a slot is one of three.
    #[test]
    fn a_header_slot_the_form_cannot_print_in_is_refused() {
        let taken = encode_refusal(page_document(json!([preface(), slot_draw("ST_Entity", "slot1")])));
        assert!(taken.contains("banking relationship's"), "{taken}");
        let line = encode_refusal(page_document(json!([slot_draw("ST_Entity", "slot1")])));
        assert!(line.contains("banking relationship's"), "{line}");
        let unknown = encode_refusal(page_document(json!([slot_draw("ST_Entity", "slot4")])));
        assert!(unknown.contains("`slot4`"), "{unknown}");

        // A field may print its value in slot 1 where no banking relationship fills it.
        let field = json!({
            "type": "TextField", "uuid": uuid::Uuid::new_v4(), "name": "TXT_Account",
            "label": {"en": "Account", "de": "Konto"}, "mandatory": false, "visible": true,
            "max_chars": null, "colspan": 12, "dor_colspan": null, "bind_ref": null,
            "dor_header_slot": "slot1"
        });
        encode(&UbsAemDocument::from_json(&page_document(json!([field]))).unwrap())
            .expect("slot 1 is free for a field");
    }
}
