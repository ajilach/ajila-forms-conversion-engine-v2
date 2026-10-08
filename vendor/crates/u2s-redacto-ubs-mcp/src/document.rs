//! The UBS Redacto document: what the Conversion Agent authors, and the UBS
//! page furniture `encode` adds around it.
//!
//! The agent authors the body and its assets in u2s-redacto's own vocabulary.
//! Everything UBS puts on every document is derived here, deterministically,
//! from what each language's source form says about itself:
//!
//! - the metadata: `document_id` `<form code lowercase>_<entity>`, title
//!   `<form code>_<entity>`, the `default.css` style, owner `admin`, `draft`;
//! - the page header: the text the source's master page draws top of page,
//!   one paragraph per line, in the wrapper that floats it clear of the logo;
//! - the page footer: the seven `Footer_Line_*` fields of the UBS master page,
//!   one span each, and the page counter.
//!
//! Ported from `profiles/ubs/redacto/config.toml` and `core/src/redacto/` of
//! `ajilach/ajila-forms-conversion-engine`; `tests/golden_parity.rs` holds this
//! to that engine's dumps.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use u2s_redacto::model::{
    Asset, AssetKey, AssetKind, Component, DocumentId, DocumentMetadata, HtmlFragment, I18nHtml,
    Language, OwnerId, Passthrough, RedactoDocument, Status, StyleName, ValidDocument,
};

/// A UBS Redacto document, as authored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UbsRedactoDocument {
    /// One entry per delivered language, from that language's source form.
    /// The keys are the document's languages.
    pub sources: BTreeMap<Language, RedactoSource>,
    /// Every text and image asset the body references. The keys `ubsHeader`
    /// and `ubsFooter` are the furniture's own.
    pub assets: Vec<Asset>,
    /// The main content, flowing across as many pages as it needs.
    pub body: Vec<Component>,
}

/// What one language's source form says about itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactoSource {
    /// The source's XFA template variables (`<variables><text>`), by name.
    /// `formrange_code` and `formrange_entity` name the document (the master
    /// language's are used); the `Footer_Line_*` ones make up the footer.
    pub variables: BTreeMap<String, String>,
    /// The text the source's master page draws top of page (the validity line
    /// and the legal entity), one line per drawn line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid document: {0}")]
    InvalidDocument(String),
    #[error("the document breaks the Redacto model: {0}")]
    Violations(String),
    #[error(transparent)]
    Encode(#[from] u2s_mapper_redacto::EncodeError),
    #[error("could not read the dump: {0}")]
    Decode(String),
}

/// The language UBS masters a document in when it ships it.
const MASTER_LANGUAGE: &str = "en";
/// The stylesheet the UBS build of the Redacto platform ships (`ajila-redacto-platform`
/// `feature/ubs`); it has no `default.css`, and a document naming one renders blank.
const STYLE: &str = "ubs-default.css";
const OWNER: &str = "admin";
const HEADER_KEY: &str = "ubsHeader";
const FOOTER_KEY: &str = "ubsFooter";

/// The footer fields in the order the UBS master page draws them, as
/// (span class, source variable): e.g. `66300 EN V0 019 AAEV 31.10.2019 N1`.
const FOOTER_FIELDS: &[(&str, &str)] = &[
    ("footer-form-id", "Footer_Line_txtformid"),
    ("footer-language", "Footer_Line_txtlanguage"),
    ("footer-version", "Footer_Line_txtvversion"),
    ("footer-man-code", "Footer_Line_MANCode"),
    ("footer-form-code", "formrange_code"),
    ("footer-release-date", "Footer_Line_txtversiondate"),
    ("footer-j-version", "Footer_Line_txtjversion"),
];

/// Redacto's client-side pagination fills `.page-number` and `.page-count`;
/// every v1 footer carried this counter, so every footer here does too.
const PAGE_COUNTER: &str = "<span class=\"right\">Page <span class=\"page-number\"></span>\
    /<span class=\"page-count\"></span></span>";

/// An empty paragraph that keeps its line height.
const SPACER: &str = "<p><br></p>";

/// The complete Redacto document: the authored body plus the UBS furniture.
pub fn to_redacto(doc: &UbsRedactoDocument) -> Result<ValidDocument, Error> {
    let languages: BTreeSet<Language> = doc.sources.keys().cloned().collect();
    let master = master_language(&languages)?;
    let master_source = &doc.sources[&master];
    for key in [HEADER_KEY, FOOTER_KEY] {
        if doc.assets.iter().any(|a| a.key.as_str() == key) {
            return Err(Error::InvalidDocument(format!(
                "the asset key `{key}` is the UBS furniture's own; rename that asset"
            )));
        }
    }

    let code = required_variable(master_source, "formrange_code")?;
    let entity = required_variable(master_source, "formrange_entity")?;
    let document_id = format!("{}_{entity}", code.to_lowercase());

    let mut assets = doc.assets.clone();
    let header = furniture_asset(HEADER_KEY, &languages, SPACER, |language| {
        let text = |l: &Language| non_blank(doc.sources[l].header.as_deref());
        text(language).or_else(|| text(&master)).map(header_html)
    })?;
    // A language falls back to the master's fields only when none of its own
    // has a value: mixing one language's version with another's form id would
    // be nonsense. Pagination has to keep working either way.
    let footer = furniture_asset(FOOTER_KEY, &languages, PAGE_COUNTER, |language| {
        let fields = |l: &Language| {
            let fields = footer_fields(&doc.sources[l]);
            fields.iter().any(|(_, v)| !v.is_empty()).then_some(fields)
        };
        fields(language)
            .or_else(|| fields(&master))
            .map(|f| footer_html(&f))
    })?;
    let section = |asset: Option<Asset>, assets: &mut Vec<Asset>| match asset {
        Some(asset) => {
            let container = Component::AssetContainer {
                assets: vec![asset.key.clone()],
            };
            assets.push(asset);
            vec![container]
        }
        None => Vec::new(),
    };
    let header = section(header, &mut assets);
    let footer = section(footer, &mut assets);

    let invalid = |what: &str, e: &dyn std::fmt::Display| {
        Error::InvalidDocument(format!(
            "the {what} derived from the source is invalid: {e}"
        ))
    };
    let redacto = RedactoDocument {
        metadata: DocumentMetadata {
            document_id: DocumentId::try_from(document_id.as_str())
                .map_err(|e| invalid("document id", &e))?,
            title: format!("{code}_{entity}"),
            style: Some(StyleName::try_from(STYLE).expect("a valid style name")),
            form_path: None,
            master_language: master,
            languages,
            owner_id: OwnerId::try_from(OWNER).expect("a valid owner"),
            status: Status::Draft,
            passthrough: Passthrough::default(),
        },
        assets,
        // Empty: Redacto then uses `header` on the first page too.
        first_header: Vec::new(),
        header,
        body: doc.body.clone(),
        footer,
    };
    redacto.validate().map_err(|violations| {
        Error::Violations(
            violations
                .iter()
                .map(|v| format!("{}: {}", v.pointer, v.message))
                .collect::<Vec<_>>()
                .join("; "),
        )
    })
}

/// Encode a document into the Redacto platform's `INSERT` script.
pub fn encode(doc: &UbsRedactoDocument) -> Result<Vec<u8>, Error> {
    Ok(u2s_mapper_redacto::encode(&to_redacto(doc)?)?.bytes)
}

/// UBS masters a document in English when it ships English, and otherwise in
/// its first language.
fn master_language(languages: &BTreeSet<Language>) -> Result<Language, Error> {
    let english = Language::try_from(MASTER_LANGUAGE).expect("a valid language");
    if languages.contains(&english) {
        return Ok(english);
    }
    languages
        .first()
        .cloned()
        .ok_or_else(|| Error::InvalidDocument("`sources` must name at least one language".into()))
}

fn required_variable<'a>(source: &'a RedactoSource, name: &str) -> Result<&'a str, Error> {
    source
        .variables
        .get(name)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            Error::InvalidDocument(format!(
                "the master language's source has no `{name}` variable, which names the document"
            ))
        })
}

fn non_blank(text: Option<&str>) -> Option<&str> {
    text.filter(|t| !t.trim().is_empty())
}

/// One furniture asset with a version per language, or `None` when no
/// language has any text for it. Redacto fails a render when a referenced
/// asset lacks a version for a language, so a language without text of its
/// own or from the master gets `blank`.
fn furniture_asset(
    key: &str,
    languages: &BTreeSet<Language>,
    blank: &str,
    content: impl Fn(&Language) -> Option<String>,
) -> Result<Option<Asset>, Error> {
    let rendered: Vec<(Language, Option<String>)> =
        languages.iter().map(|l| (l.clone(), content(l))).collect();
    if rendered.iter().all(|(_, c)| c.is_none()) {
        return Ok(None);
    }
    let entries = rendered
        .into_iter()
        .map(|(language, content)| {
            let html = HtmlFragment::try_from(content.unwrap_or_else(|| blank.to_string()))
                .map_err(|e| Error::InvalidDocument(format!("the {key} asset: {e}")))?;
            Ok((language, html))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(Some(Asset {
        key: AssetKey::try_from(key).expect("a valid furniture key"),
        kind: AssetKind::Text,
        content: I18nHtml::from_entries(entries),
    }))
}

/// The page header: one paragraph per line (a blank line inside keeps its
/// height), escaped, in the `.right .preserve-spaces` wrapper. `.right` floats
/// the text clear of the page logo, which the v2 authored header has to ask
/// for; `.preserve-spaces` keeps its whitespace.
fn header_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.trim().split('\n').map(str::trim_end).collect();
    let inner: String = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                SPACER.to_string()
            } else {
                format!("<p>{}</p>", escape_html(line))
            }
        })
        .collect();
    format!(r#"<div class="right preserve-spaces">{inner}</div>"#)
}

/// The footer's (span class, value) pairs for one source, values trimmed.
fn footer_fields(source: &RedactoSource) -> Vec<(&'static str, String)> {
    FOOTER_FIELDS
        .iter()
        .map(|(class, variable)| {
            let value = source.variables.get(*variable).map_or("", |v| v.trim());
            (*class, value.to_string())
        })
        .collect()
}

/// The footer: each non-blank field as its own span, one space apart, in a
/// `.redacto-reading-order` span so the renderer tags them as reading order,
/// then the page counter, which is not reading-order content.
fn footer_html(fields: &[(&str, String)]) -> String {
    let spans: Vec<String> = fields
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(class, value)| format!("<span class=\"{class}\">{}</span>", escape_html(value)))
        .collect();
    if spans.is_empty() {
        return PAGE_COUNTER.to_string();
    }
    format!(
        r#"<span class="redacto-reading-order">{}</span>{PAGE_COUNTER}"#,
        spans.join(" ")
    )
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Lift a UBS Redacto dump back into a document: the furniture comes off, and
/// each language's source is recovered from it (the header text, the footer's
/// variables, and the form code and entity from the document id).
pub fn decode(dump: &[u8]) -> Result<UbsRedactoDocument, Error> {
    let doc = u2s_mapper_redacto::decode::decode(dump)
        .map_err(|e| Error::Decode(e.to_string()))?
        .into_document();
    let furniture_asset = |section: &[Component], what: &str| -> Result<Option<Asset>, Error> {
        match section {
            [] => Ok(None),
            [Component::AssetContainer { assets }] if assets.len() == 1 => doc
                .assets
                .iter()
                .find(|a| a.key == assets[0])
                .cloned()
                .map(Some)
                .ok_or_else(|| Error::Decode(format!("the {what} references a missing asset"))),
            _ => Err(Error::Decode(format!(
                "the {what} is not the UBS furniture (one asset container with one asset)"
            ))),
        }
    };
    let header = furniture_asset(&doc.header, "header")?;
    let footer = furniture_asset(&doc.footer, "footer")?;

    // The id is `<code lowercase>_<entity>` and the title `<code>_<entity>`: the
    // title keeps the code's case. Whether this is right is checked below.
    let id = doc.metadata.document_id.as_str();
    let (_, entity) = id
        .rsplit_once('_')
        .ok_or_else(|| Error::Decode(format!("the document id `{id}` is not `<code>_<entity>`")))?;
    let code = doc
        .metadata
        .title
        .strip_suffix(entity)
        .and_then(|t| t.strip_suffix('_'))
        .ok_or_else(|| {
            Error::Decode(format!(
                "the title `{}` is not `<code>_{entity}`",
                doc.metadata.title
            ))
        })?;

    let mut sources = BTreeMap::new();
    for language in &doc.metadata.languages {
        let mut source = RedactoSource::default();
        source
            .variables
            .insert("formrange_code".into(), code.to_string());
        source
            .variables
            .insert("formrange_entity".into(), entity.into());
        if let Some(html) = header.as_ref().and_then(|a| a.content.get(language)) {
            source.header = parse_header_html(html.as_str())?;
        }
        if let Some(html) = footer.as_ref().and_then(|a| a.content.get(language)) {
            for (class, value) in parse_footer_html(html.as_str()) {
                let variable = FOOTER_FIELDS
                    .iter()
                    .find(|(c, _)| *c == class)
                    .map(|(_, v)| *v)
                    .ok_or_else(|| Error::Decode(format!("unknown footer field `{class}`")))?;
                source.variables.insert(variable.into(), value);
            }
        }
        sources.insert(language.clone(), source);
    }

    let furniture: Vec<&AssetKey> = header.iter().chain(&footer).map(|a| &a.key).collect();
    let decoded = UbsRedactoDocument {
        sources,
        assets: doc
            .assets
            .iter()
            .filter(|a| !furniture.contains(&&a.key))
            .cloned()
            .collect(),
        body: doc.body.clone(),
    };
    rebuilds_the_dump(&decoded, &doc, header.as_ref(), footer.as_ref())?;
    Ok(decoded)
}

/// Refuse a dump the recovered document does not encode back to: one with a
/// first-page header, or another style, owner, status or title than UBS gives
/// it, or furniture the parsers read back only in part. The
/// assets and body are carried over as they are, so the metadata and the
/// furniture are what can differ.
fn rebuilds_the_dump(
    decoded: &UbsRedactoDocument,
    dump: &RedactoDocument,
    header: Option<&Asset>,
    footer: Option<&Asset>,
) -> Result<(), Error> {
    let rebuilt = to_redacto(decoded)?.into_document();
    let not_ubs = |what: &str| Error::Decode(format!("the dump's {what} is not what UBS writes"));
    if !dump.first_header.is_empty() {
        return Err(not_ubs("first-page header"));
    }
    let (m, r) = (&dump.metadata, &rebuilt.metadata);
    let effective_path = |p: &Option<u2s_redacto::model::FormPath>| {
        p.as_ref().map_or_else(
            || {
                format!(
                    "/content/forms/af/redacto-documents/{}",
                    r.document_id.as_str()
                )
            },
            |p| p.as_str().to_string(),
        )
    };
    for (what, same) in [
        ("document id", m.document_id == r.document_id),
        ("title", m.title == r.title),
        ("style", m.style == r.style),
        (
            "form path",
            effective_path(&m.form_path) == effective_path(&r.form_path),
        ),
        ("master language", m.master_language == r.master_language),
        ("languages", m.languages == r.languages),
        ("owner", m.owner_id == r.owner_id),
        ("status", m.status == r.status),
    ] {
        if !same {
            return Err(not_ubs(what));
        }
    }
    let rebuilt_asset = |key: &str| rebuilt.assets.iter().find(|a| a.key.as_str() == key);
    for (what, original, key) in [
        ("header", header, HEADER_KEY),
        ("footer", footer, FOOTER_KEY),
    ] {
        let same = match (original, rebuilt_asset(key)) {
            (None, None) => true,
            (Some(o), Some(r)) => o.content == r.content,
            _ => false,
        };
        if !same {
            return Err(not_ubs(what));
        }
    }
    Ok(())
}

/// The header text of [`header_html`]'s output, or `None` for the blank
/// paragraph a language with no header text gets.
fn parse_header_html(html: &str) -> Result<Option<String>, Error> {
    if html == SPACER {
        return Ok(None);
    }
    let inner = html
        .strip_prefix(r#"<div class="right preserve-spaces">"#)
        .and_then(|h| h.strip_suffix("</div>"))
        .ok_or_else(|| Error::Decode(format!("the header `{html}` is not the UBS header")))?;
    let lines: Vec<String> = inner
        .split_terminator("</p>")
        .map(|p| {
            let line = p.strip_prefix("<p>").unwrap_or(p);
            if line == "<br>" {
                String::new()
            } else {
                unescape_html(line)
            }
        })
        .collect();
    Ok(Some(lines.join("\n")))
}

/// The (span class, value) pairs of [`footer_html`]'s output.
fn parse_footer_html(html: &str) -> Vec<(String, String)> {
    let fields = html.strip_suffix(PAGE_COUNTER).unwrap_or(html);
    let fields = fields
        .strip_prefix(r#"<span class="redacto-reading-order">"#)
        .and_then(|f| f.strip_suffix("</span>"))
        .unwrap_or("");
    fields
        .split("</span>")
        .filter_map(|span| {
            let span = span.trim_start().strip_prefix("<span class=\"")?;
            let (class, value) = span.split_once("\">")?;
            Some((class.to_string(), unescape_html(value)))
        })
        .collect()
}

fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang(code: &str) -> Language {
        Language::try_from(code).unwrap()
    }

    fn source(header: Option<&str>, fields: &[(&str, &str)]) -> RedactoSource {
        let mut variables: BTreeMap<String, String> =
            [("formrange_code", "AAEV"), ("formrange_entity", "019")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        variables.extend(fields.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        RedactoSource {
            variables,
            header: header.map(String::from),
        }
    }

    fn document(sources: &[(&str, RedactoSource)]) -> UbsRedactoDocument {
        UbsRedactoDocument {
            sources: sources.iter().map(|(l, s)| (lang(l), s.clone())).collect(),
            assets: vec![Asset {
                key: AssetKey::try_from("intro").unwrap(),
                kind: AssetKind::Text,
                content: I18nHtml::from_entries(sources.iter().map(|(l, _)| {
                    (
                        lang(l),
                        HtmlFragment::try_from("<p>Hello</p>".to_string()).unwrap(),
                    )
                })),
            }],
            body: vec![Component::AssetContainer {
                assets: vec![AssetKey::try_from("intro").unwrap()],
            }],
        }
    }

    fn furniture(doc: &UbsRedactoDocument, key: &str, language: &str) -> Option<String> {
        let valid = to_redacto(doc).unwrap();
        valid
            .document()
            .assets
            .iter()
            .find(|a| a.key.as_str() == key)
            .map(|a| a.content.get(&lang(language)).unwrap().as_str().to_string())
    }

    #[test]
    fn the_document_is_named_from_the_master_sources_code_and_entity() {
        let doc = to_redacto(&document(&[
            ("de", source(None, &[])),
            ("en", source(None, &[])),
        ]))
        .unwrap();
        let metadata = &doc.document().metadata;
        assert_eq!(metadata.document_id.as_str(), "aaev_019");
        assert_eq!(metadata.title, "AAEV_019");
        assert_eq!(metadata.master_language, lang("en"));
    }

    #[test]
    fn a_form_without_english_is_mastered_in_its_first_language() {
        let doc = to_redacto(&document(&[("it", source(None, &[]))])).unwrap();
        assert_eq!(doc.document().metadata.master_language, lang("it"));
    }

    #[test]
    fn a_language_without_a_header_takes_the_masters() {
        let doc = document(&[
            ("de", source(None, &[])),
            (
                "en",
                source(Some("Valid from 02.01.2018\nUBS Europe SE"), &[]),
            ),
        ]);
        assert_eq!(
            furniture(&doc, HEADER_KEY, "de").unwrap(),
            r#"<div class="right preserve-spaces"><p>Valid from 02.01.2018</p><p>UBS Europe SE</p></div>"#
        );
    }

    #[test]
    fn no_header_anywhere_means_no_header_section() {
        let doc = document(&[("en", source(None, &[]))]);
        assert!(to_redacto(&doc).unwrap().document().header.is_empty());
    }

    /// A language falls back to the master's footer only as a whole: its own
    /// language code next to the master's form id would be nonsense.
    #[test]
    fn a_footer_falls_back_to_the_masters_only_as_a_whole() {
        let doc = document(&[
            ("de", source(None, &[("Footer_Line_txtlanguage", "DE")])),
            (
                "en",
                source(
                    None,
                    &[
                        ("Footer_Line_txtformid", "66300"),
                        ("Footer_Line_txtlanguage", "EN"),
                    ],
                ),
            ),
            ("es", source(None, &[])),
        ]);
        assert_eq!(
            furniture(&doc, FOOTER_KEY, "de").unwrap(),
            format!(
                r#"<span class="redacto-reading-order"><span class="footer-language">DE</span> <span class="footer-form-code">AAEV</span></span>{PAGE_COUNTER}"#
            )
        );
        // `es` has only the form code, which every source carries.
        assert!(
            furniture(&doc, FOOTER_KEY, "es")
                .unwrap()
                .contains("footer-form-code\">AAEV")
        );
    }

    #[test]
    fn the_furniture_keys_are_reserved() {
        let mut doc = document(&[("en", source(None, &[]))]);
        doc.assets[0].key = AssetKey::try_from(HEADER_KEY).unwrap();
        assert!(matches!(to_redacto(&doc), Err(Error::InvalidDocument(_))));
    }

    #[test]
    fn a_source_without_a_form_code_is_refused() {
        let mut doc = document(&[("en", source(None, &[]))]);
        doc.sources
            .get_mut(&lang("en"))
            .unwrap()
            .variables
            .remove("formrange_code");
        let Err(Error::InvalidDocument(message)) = to_redacto(&doc) else {
            panic!("a missing form code must be refused")
        };
        assert!(message.contains("formrange_code"), "{message}");
    }

    #[test]
    fn the_furniture_parses_back() {
        let text = "Valid from 02.01.2018\n\nUBS Europe SE & Co";
        assert_eq!(
            parse_header_html(&header_html(text)).unwrap().as_deref(),
            Some(text)
        );
        let fields = vec![
            ("footer-form-id", "66300".to_string()),
            ("footer-language", "EN".to_string()),
        ];
        let parsed = parse_footer_html(&footer_html(&fields));
        assert_eq!(
            parsed,
            vec![
                ("footer-form-id".to_string(), "66300".to_string()),
                ("footer-language".to_string(), "EN".to_string())
            ]
        );
    }
}
