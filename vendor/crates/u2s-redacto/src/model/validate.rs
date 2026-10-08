//! Semantic validation: checks that need more than one node's type to see.
//! Local, single-field constraints (an `AssetKey`'s character set, a
//! `DocumentId`'s length) are enforced by the types themselves at
//! deserialization instead (per `u2s-aem::model::validate`'s own module
//! doc); "at least one" checks that could in principle be local (an empty
//! `body`, an empty `AssetContainer`) are still checked here, mirroring
//! `AemForm::validate`'s own precedent of checking `pages`/`children`
//! non-emptiness in the same place rather than inventing a non-empty-`Vec`
//! wrapper type for each one.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::Regex;

use super::{AssetKey, AssetKind, Component, RedactoDocument};

/// One semantic-validation failure, anchored to the JSON Pointer of the
/// offending value -- the same shape rule scripts emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub pointer: String,
    pub message: String,
}

/// A [`RedactoDocument`] that has passed [`RedactoDocument::validate`].
/// Constructible only by that method, so the encoder (phase 2) can require
/// `&ValidDocument` and make encoding unvalidated output a compile error --
/// the same pattern `u2s-aem::model::ValidForm` uses.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidDocument(RedactoDocument);

impl ValidDocument {
    pub fn document(&self) -> &RedactoDocument {
        &self.0
    }

    pub fn into_document(self) -> RedactoDocument {
        self.0
    }
}

/// Tags this format's asset content may use -- the platform's own Quill
/// vocabulary (`u2s-mapper-redacto`'s decoder accepts exactly this set from
/// a real delivered document too).
const ALLOWED_TAGS: &[&str] = &[
    "p", "strong", "em", "sup", "sub", "u", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li",
    "table", "thead", "tbody", "tr", "td", "th", "a", "span", "br", "img", "div",
];

/// Tags with no closing counterpart -- never pushed onto the well-formedness
/// stack.
const VOID_TAGS: &[&str] = &["br", "img"];

impl RedactoDocument {
    pub fn validate(self) -> Result<ValidDocument, Vec<Violation>> {
        let mut violations = Vec::new();

        let master = self.metadata.master_language.clone();
        let languages = self.metadata.languages.clone();
        if !languages.contains(&master) {
            violations.push(Violation {
                pointer: "/metadata/languages".to_owned(),
                message: format!("languages must contain the master language '{master}'"),
            });
        }

        if self.body.is_empty() {
            // The single most valuable check this crate makes: a dump with
            // an empty body is still valid SQL that imports cleanly, which
            // is exactly why an empty one once shipped from the reference
            // implementation unnoticed. The header/footer/firstHeader slots
            // are page furniture resolved separately, so they carry no such
            // requirement.
            violations.push(Violation {
                pointer: "/body".to_owned(),
                message: "a document must have a body".to_owned(),
            });
        }

        let declared: BTreeSet<&AssetKey> = self.assets.iter().map(|a| &a.key).collect();
        let mut duplicate_keys: HashMap<&str, usize> = HashMap::new();
        for asset in &self.assets {
            *duplicate_keys.entry(asset.key.as_str()).or_insert(0) += 1;
        }
        for (index, asset) in self.assets.iter().enumerate() {
            if duplicate_keys[asset.key.as_str()] > 1 {
                violations.push(Violation {
                    pointer: format!("/assets/{index}/key"),
                    message: format!("asset key '{}' is used more than once", asset.key),
                });
            }

            for lang in &languages {
                if asset.content.get(lang).is_none() {
                    violations.push(Violation {
                        pointer: format!("/assets/{index}/content"),
                        message: format!(
                            "asset '{}' has no content for declared language '{lang}' -- \
                             the platform fails a render when a referenced asset lacks the \
                             requested language",
                            asset.key
                        ),
                    });
                }
            }
            for lang in asset.content.languages() {
                if !languages.contains(lang) {
                    violations.push(Violation {
                        pointer: format!("/assets/{index}/content"),
                        message: format!(
                            "language '{lang}' is not declared in metadata.languages"
                        ),
                    });
                }
            }

            if asset.kind == AssetKind::Image {
                for (lang, content) in asset.content.iter() {
                    if !content.as_str().trim_start().starts_with("data:") {
                        violations.push(Violation {
                            pointer: format!("/assets/{index}/content"),
                            message: format!(
                                "image asset '{}' content for language '{lang}' must be a \
                                 complete data: URI",
                                asset.key
                            ),
                        });
                    }
                }
            }
        }

        let mut referenced: BTreeSet<AssetKey> = BTreeSet::new();
        let mut furniture_referenced: BTreeSet<AssetKey> = BTreeSet::new();
        for (slot_name, slot) in [
            ("firstHeader", &self.first_header),
            ("header", &self.header),
            ("body", &self.body),
            ("footer", &self.footer),
        ] {
            let is_furniture = slot_name != "body";
            walk_components(
                slot,
                &format!("/{slot_name}"),
                &declared,
                is_furniture,
                &mut referenced,
                &mut furniture_referenced,
                &mut violations,
            );
        }

        for asset in &self.assets {
            if !referenced.contains(&asset.key) {
                violations.push(Violation {
                    pointer: "/assets".to_owned(),
                    message: format!(
                        "asset '{}' is never referenced from any of firstHeader/header/body/footer",
                        asset.key
                    ),
                });
            }
        }

        // Well-formedness and the mandatory img alt-text check apply to
        // every asset regardless of which slot it is referenced from (or
        // even if it is unreferenced, which is its own violation above) --
        // the platform renders whatever HTML an asset carries verbatim.
        // The heading check applies only where an asset is actually reached
        // from a furniture slot, since a body-only asset is free to carry
        // real headings.
        for (index, asset) in self.assets.iter().enumerate() {
            let in_furniture = furniture_referenced.contains(&asset.key);
            for (lang, content) in asset.content.iter() {
                let pointer = format!("/assets/{index}/content/{lang}");
                check_well_formed(content.as_str(), &pointer, &mut violations);
                check_img_alt(content.as_str(), &pointer, &mut violations);
                if in_furniture && heading_regex().is_match(content.as_str()) {
                    violations.push(Violation {
                        pointer,
                        message: "a furniture slot (firstHeader/header/footer) must not carry \
                                  h1-h3 headings -- the platform unwraps them to <span> with a \
                                  warning because a heading there would inject a bogus PDF \
                                  outline entry"
                            .to_owned(),
                    });
                }
            }
        }

        if violations.is_empty() {
            Ok(ValidDocument(self))
        } else {
            Err(violations)
        }
    }
}

fn walk_components(
    components: &[Component],
    pointer_prefix: &str,
    declared: &BTreeSet<&AssetKey>,
    is_furniture: bool,
    referenced: &mut BTreeSet<AssetKey>,
    furniture_referenced: &mut BTreeSet<AssetKey>,
    violations: &mut Vec<Violation>,
) {
    for (index, component) in components.iter().enumerate() {
        let pointer = format!("{pointer_prefix}/{index}");
        match component {
            Component::AssetContainer { assets } => {
                if assets.is_empty() {
                    violations.push(Violation {
                        pointer: format!("{pointer}/assets"),
                        message: "an assetContainer must reference at least one asset".to_owned(),
                    });
                }
                for (asset_index, key) in assets.iter().enumerate() {
                    if !declared.contains(key) {
                        violations.push(Violation {
                            pointer: format!("{pointer}/assets/{asset_index}"),
                            message: format!(
                                "references asset key '{key}', which is not declared in /assets"
                            ),
                        });
                    } else {
                        referenced.insert(key.clone());
                        if is_furniture {
                            furniture_referenced.insert(key.clone());
                        }
                    }
                }
            }
            Component::StyledPanel { components, .. } => {
                if components.is_empty() {
                    violations.push(Violation {
                        pointer: format!("{pointer}/components"),
                        message: "a styledPanel must contain at least one component".to_owned(),
                    });
                }
                walk_components(
                    components,
                    &format!("{pointer}/components"),
                    declared,
                    is_furniture,
                    referenced,
                    furniture_referenced,
                    violations,
                );
            }
        }
    }
}

/// The platform unwraps `h1`-`h3` found in a furniture slot to `<span>`,
/// with a warning, because `applyBookmarks` walks the DOM and a heading
/// there would inject a bogus PDF outline entry. Checked statically here so
/// that silent degradation becomes a refusal instead. Applied per rendered
/// asset content, at the point where an asset is found reachable from a
/// furniture slot -- so a body-only asset never gets flagged, but one
/// legitimately shared between the body and a furniture slot does.
fn heading_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<h[1-3]\b").expect("valid built-in pattern"))
}

fn img_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<img\b([^>]*)>").expect("valid built-in pattern"))
}

fn alt_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\balt\s*=\s*"([^"]*)""#).expect("valid built-in pattern"))
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<(/?)([A-Za-z][A-Za-z0-9]*)([^>]*)>").expect("valid built-in pattern")
    })
}

/// A mechanical structural check, not a full HTML5 parser: every tag is
/// drawn from [`ALLOWED_TAGS`] and every non-void opening tag has a
/// matching closing tag in the right order. Good enough for agent-authored
/// fragments over a small, fixed vocabulary; a real browser-grade parser is
/// not warranted for this format's own tag set.
fn check_well_formed(html: &str, pointer: &str, violations: &mut Vec<Violation>) {
    let mut stack: Vec<String> = Vec::new();
    for caps in tag_regex().captures_iter(html) {
        let closing = &caps[1] == "/";
        let tag = caps[2].to_ascii_lowercase();
        let rest = &caps[3];

        if !ALLOWED_TAGS.contains(&tag.as_str()) {
            violations.push(Violation {
                pointer: pointer.to_owned(),
                message: format!("disallowed tag <{tag}> -- not part of the Quill vocabulary"),
            });
            continue;
        }
        if VOID_TAGS.contains(&tag.as_str()) {
            continue;
        }
        if rest.trim_end().ends_with('/') {
            // Self-closing, e.g. `<br/>` written with a non-void spelling.
            continue;
        }
        if closing {
            match stack.pop() {
                Some(open) if open == tag => {}
                Some(open) => violations.push(Violation {
                    pointer: pointer.to_owned(),
                    message: format!("expected closing tag </{open}>, found </{tag}>"),
                }),
                None => violations.push(Violation {
                    pointer: pointer.to_owned(),
                    message: format!("unexpected closing tag </{tag}> with nothing open"),
                }),
            }
        } else {
            stack.push(tag);
        }
    }
    if !stack.is_empty() {
        violations.push(Violation {
            pointer: pointer.to_owned(),
            message: format!("unclosed tag(s): {stack:?}"),
        });
    }
}

/// Every `<img>` needs a non-blank `alt` under PDF/UA -- warned about by the
/// platform (`Alt=""` passes a casual tag-tree read and fails validation),
/// rejected outright here.
fn check_img_alt(html: &str, pointer: &str, violations: &mut Vec<Violation>) {
    for caps in img_regex().captures_iter(html) {
        let attrs = &caps[1];
        let has_alt = alt_regex()
            .captures(attrs)
            .map(|c| !c[1].trim().is_empty())
            .unwrap_or(false);
        if !has_alt {
            violations.push(Violation {
                pointer: pointer.to_owned(),
                message: "<img> must carry a non-blank alt attribute".to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Asset, DocumentId, DocumentMetadata, HtmlFragment, I18nHtml, Language, OwnerId,
        PanelStyle, Passthrough, Status,
    };

    fn lang(code: &str) -> Language {
        Language::try_from(code).unwrap()
    }

    fn html(content: &str) -> HtmlFragment {
        HtmlFragment::try_from(content.to_owned()).unwrap()
    }

    fn key(name: &str) -> AssetKey {
        AssetKey::try_from(name).unwrap()
    }

    fn minimal_doc() -> RedactoDocument {
        RedactoDocument {
            metadata: DocumentMetadata {
                document_id: DocumentId::try_from("aaev_019").unwrap(),
                title: "AAEV_019".to_owned(),
                style: None,
                form_path: None,
                master_language: lang("en"),
                languages: BTreeSet::from([lang("en")]),
                owner_id: OwnerId::try_from("admin").unwrap(),
                status: Status::Draft,
                passthrough: Passthrough::default(),
            },
            assets: vec![Asset {
                key: key("intro"),
                kind: AssetKind::Text,
                content: I18nHtml::single(lang("en"), html("<p>Hello</p>")),
            }],
            first_header: Vec::new(),
            header: Vec::new(),
            body: vec![Component::AssetContainer {
                assets: vec![key("intro")],
            }],
            footer: Vec::new(),
        }
    }

    #[test]
    fn a_minimal_document_validates() {
        minimal_doc().validate().expect("minimal document must validate");
    }

    #[test]
    fn empty_body_is_rejected() {
        let mut doc = minimal_doc();
        doc.body = Vec::new();
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/body"));
    }

    #[test]
    fn master_language_not_in_languages_is_rejected() {
        let mut doc = minimal_doc();
        doc.metadata.languages = BTreeSet::from([lang("de")]);
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/metadata/languages"));
    }

    #[test]
    fn missing_translation_for_a_declared_language_is_rejected() {
        let mut doc = minimal_doc();
        doc.metadata.languages = BTreeSet::from([lang("en"), lang("de")]);
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/assets/0/content"));
    }

    #[test]
    fn empty_asset_container_is_rejected() {
        let mut doc = minimal_doc();
        doc.body = vec![Component::AssetContainer { assets: Vec::new() }];
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/body/0/assets"));
    }

    #[test]
    fn dangling_asset_reference_is_rejected() {
        let mut doc = minimal_doc();
        doc.body = vec![Component::AssetContainer {
            assets: vec![key("does-not-exist")],
        }];
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/body/0/assets/0"));
    }

    #[test]
    fn empty_styled_panel_is_rejected() {
        let mut doc = minimal_doc();
        doc.body.push(Component::StyledPanel {
            style: PanelStyle::try_from("layout-split").unwrap(),
            components: Vec::new(),
        });
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/body/1/components"));
    }

    #[test]
    fn unreferenced_asset_is_rejected() {
        let mut doc = minimal_doc();
        doc.assets.push(Asset {
            key: key("orphan"),
            kind: AssetKind::Text,
            content: I18nHtml::single(lang("en"), html("<p>Orphan</p>")),
        });
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/assets"));
    }

    #[test]
    fn heading_in_header_slot_is_not_itself_rejected_but_disallowed_tag_is() {
        // The heading check applies to rendered content; disallowed tags are
        // rejected regardless of slot.
        let mut doc = minimal_doc();
        doc.assets[0].content =
            I18nHtml::single(lang("en"), html("<script>alert(1)</script>"));
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("disallowed tag")));
    }

    #[test]
    fn img_without_alt_is_rejected() {
        let mut doc = minimal_doc();
        doc.assets[0].content = I18nHtml::single(lang("en"), html(r#"<img src="x.png">"#));
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("alt")));
    }

    #[test]
    fn img_with_alt_is_accepted() {
        let mut doc = minimal_doc();
        doc.assets[0].content =
            I18nHtml::single(lang("en"), html(r#"<img src="x.png" alt="a logo">"#));
        doc.validate().expect("img with alt text must validate");
    }

    #[test]
    fn unclosed_tag_is_rejected() {
        let mut doc = minimal_doc();
        doc.assets[0].content = I18nHtml::single(lang("en"), html("<p>unclosed"));
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("unclosed")));
    }

    #[test]
    fn duplicate_asset_key_is_rejected() {
        let mut doc = minimal_doc();
        doc.assets.push(Asset {
            key: key("intro"),
            kind: AssetKind::Text,
            content: I18nHtml::single(lang("en"), html("<p>Again</p>")),
        });
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.pointer == "/assets/0/key"));
    }

    #[test]
    fn image_asset_content_must_be_a_data_uri() {
        let mut doc = minimal_doc();
        doc.assets.push(Asset {
            key: key("logo"),
            kind: AssetKind::Image,
            content: I18nHtml::single(lang("en"), html("not-a-data-uri")),
        });
        doc.body.push(Component::AssetContainer {
            assets: vec![key("logo")],
        });
        let violations = doc.validate().unwrap_err();
        assert!(violations.iter().any(|v| v.message.contains("data: URI")));
    }
}
