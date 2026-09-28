//! The generic Redacto document output model: one root structure
//! (`RedactoDocument`), its enums and newtypes, and semantic validation.
//! This is the *entire* intermediate JSON the Conversion Agent edits --
//! lowering it into the platform's `INSERT` script is an encoder concern,
//! not modelled here.
//!
//! Mirrors `u2s-aem::model`'s split file-for-file: `common` (the
//! passthrough escape hatch), `enums`, `newtypes`, `text` (language-keyed
//! content) and `validate` (whole-document semantic checks). See that
//! crate's own module doc for the reasoning this one repeats.

pub mod common;
pub mod enums;
pub mod newtypes;
pub mod text;
pub mod validate;

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use common::Passthrough;
pub use enums::{AssetKind, Status};
pub use newtypes::{AssetKey, DocumentId, FormPath, OwnerId, PanelStyle, StyleName};
pub use text::{HtmlFragment, I18nHtml, Language};
pub use validate::{ValidDocument, Violation};

/// The Redacto output document. `schema_for!(RedactoDocument)` (see
/// [`crate::schema`]) IS the format schema published in the format server's
/// manifest -- there is no hand-maintained schema file for it to drift from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactoDocument {
    pub metadata: DocumentMetadata,
    /// Every text/image asset this document references, keyed by
    /// [`AssetKey`]. The encoder mints both of the platform's own UUIDs
    /// (`assets.id` and the distinct `assets.asset_id`) from this key --
    /// see [`AssetKey`]'s own doc.
    pub assets: Vec<Asset>,
    /// Top of the first page only. Redacto backfills this slot from
    /// [`RedactoDocument::header`] when it is empty -- the fallback is
    /// mandatory, not optional, because the platform's `@page:first` rule
    /// otherwise outranks `@page` and page one silently loses its header --
    /// so an empty list here is a legitimate, common choice, not a gap.
    #[serde(default)]
    pub first_header: Vec<Component>,
    /// Top of every page (page one only when [`first_header`](Self::first_header)
    /// is non-empty).
    #[serde(default)]
    pub header: Vec<Component>,
    /// The main content, flowing across as many pages as it needs.
    /// Non-empty -- checked in [`RedactoDocument::validate`]: an empty body
    /// is still valid SQL that imports cleanly, which is exactly why an
    /// empty one once shipped from the reference implementation unnoticed.
    pub body: Vec<Component>,
    /// Bottom of every page.
    #[serde(default)]
    pub footer: Vec<Component>,
}

impl RedactoDocument {
    /// Structural parse: agents edit the JSON as a `Value` through
    /// `u2s-jsondoc`; the mapper deserializes exactly once, at encode.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentMetadata {
    /// Mirrors `documents.document_id` and `configuration.document.id`.
    pub document_id: DocumentId,
    /// Human-readable title, rendered into the HTML `<title>`. Plain text,
    /// language-neutral -- the platform's own `document.title` is not a
    /// translated field.
    pub title: String,
    /// Stylesheet resolved from the Redacto bundle. `None` means the
    /// platform's own default (the first style alphabetically).
    #[serde(default)]
    pub style: Option<StyleName>,
    /// AEM authoring path of the document. `None` means the encoder derives
    /// `/content/forms/af/redacto-documents/{document_id}`.
    #[serde(default)]
    pub form_path: Option<FormPath>,
    pub master_language: Language,
    /// Every delivered language; must contain `master_language` (checked in
    /// [`RedactoDocument::validate`]). The platform fails a render when an
    /// asset referenced for a declared language has no version for it, so
    /// this set is exactly what the encoder must produce one
    /// `asset_version`/`document_version` row per.
    pub languages: BTreeSet<Language>,
    /// Authoring user recorded as the document owner. Redacto rejects every
    /// authoring write against a document with no
    /// `(owner_id, USER, OWNER, document_id, DOCUMENT)` ownership row -- the
    /// encoder derives that row mechanically from this value.
    pub owner_id: OwnerId,
    #[serde(default)]
    pub status: Status,
    /// Everything a decoded document's own `documents` row carried that no
    /// field above represents. See [`Passthrough`]'s own doc for why this
    /// makes decode lossless without a dedicated typed field per future
    /// column.
    #[serde(default)]
    pub passthrough: Passthrough,
}

/// A component of the platform's `redacto-document/v2` configuration tree.
/// Exactly two types exist -- earlier draft types were removed from the
/// platform itself, so this model has no more to offer than it does.
///
/// Component identity (the `id` every JSON instance of this shape carries
/// on the platform) is mechanical and encoder-derived, not agent-authored --
/// the agent expresses composition, the encoder spells it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Component {
    /// A block carrying content, referencing text/image assets by
    /// [`AssetKey`], in render order. Non-empty -- checked in
    /// [`RedactoDocument::validate`].
    AssetContainer {
        #[serde(default)]
        assets: Vec<AssetKey>,
    },
    /// A container that groups and styles nested components. A `style` on
    /// an `assetContainer` is silently dropped by the platform
    /// (`ComponentRenderService` hardcodes its fragment style), which is
    /// why only this variant carries one. Non-empty `components` -- checked
    /// in [`RedactoDocument::validate`].
    StyledPanel {
        style: PanelStyle,
        #[serde(default)]
        components: Vec<Component>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub key: AssetKey,
    pub kind: AssetKind,
    /// The rendered body, one HTML fragment per language. An `Image`
    /// asset's content is a complete `data:` URI per language (typically a
    /// single `zxx` "no linguistic content" entry, since an image usually
    /// has no per-language variant) -- see
    /// [`validate`](super::validate)'s image-content check.
    pub content: I18nHtml,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_round_trips_through_json() {
        let doc = RedactoDocument {
            metadata: DocumentMetadata {
                document_id: DocumentId::try_from("aaev_019").unwrap(),
                title: "AAEV_019".to_owned(),
                style: Some(StyleName::try_from("default.css").unwrap()),
                form_path: None,
                master_language: Language::try_from("en").unwrap(),
                languages: BTreeSet::from([Language::try_from("en").unwrap()]),
                owner_id: OwnerId::try_from("admin").unwrap(),
                status: Status::Draft,
                passthrough: Passthrough::default(),
            },
            assets: vec![Asset {
                key: AssetKey::try_from("intro").unwrap(),
                kind: AssetKind::Text,
                content: I18nHtml::single(
                    Language::try_from("en").unwrap(),
                    HtmlFragment::try_from("<p>Hello</p>".to_owned()).unwrap(),
                ),
            }],
            first_header: Vec::new(),
            header: Vec::new(),
            body: vec![Component::AssetContainer {
                assets: vec![AssetKey::try_from("intro").unwrap()],
            }],
            footer: Vec::new(),
        };

        let json = serde_json::to_value(&doc).unwrap();
        let reparsed: RedactoDocument = serde_json::from_value(json).unwrap();
        assert_eq!(doc, reparsed);
    }
}
