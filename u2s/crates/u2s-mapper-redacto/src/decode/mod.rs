//! The exact inverse of [`crate::encode`]: a real, delivered `INSERT`
//! script in, a [`ValidDocument`] out. Lossless-or-error by construction --
//! anything this parser cannot represent is a hard [`DecodeError`] naming
//! what could not be represented, never a partial document -- mirroring
//! `u2s-mapper-aem::decode`'s own discipline.
//!
//! **Known, accepted gaps**, named here rather than silently dropped:
//!
//! - **`master_language` is not decode-recoverable.** The platform's own
//!   row model has no such concept -- it is authoring-time-only in the
//!   reference converter's own profile TOML and never persisted. This
//!   decoder picks `en` when the dump declares it, else the
//!   lexicographically first declared language. A decoded-then-re-encoded
//!   document therefore always declares `en` as master whenever the source
//!   document had it among its languages, whatever the source's own
//!   (unrecorded) master actually was.
//! - **A component's own platform-assigned `id`** (every `ComponentJson`
//!   instance carries one) **is discarded**, not carried through --
//!   `u2s_redacto::model::Component` has no field for it (see that type's
//!   own doc: identity is mechanical and re-derived by
//!   [`crate::ids::component_id`] on the next `encode`), so a decoded
//!   document's re-encoded component ids never match the original's.
//! - **A `-lang-` qualified asset reference** (`Version.java`'s own second,
//!   rarer grammar) **is rejected**, not decoded -- the reference converter
//!   never emits one, so no fixture exercises it, and guessing at its
//!   semantics would be worse than refusing.
//! - **`relations` rows are read but not cross-checked.** Every fact this
//!   decoder needs is already recoverable from `assets`/`asset_version`/
//!   `ownerships`/the configuration JSON; a `relations` row missing or
//!   duplicated relative to what `encode` would have produced is not an
//!   error this decoder raises.
//! - **A decoded [`AssetKey`] is an invented label, not a stable identity.**
//!   The row model has nowhere to persist an agent-chosen key -- only the
//!   platform's own generated business UUID survives -- so
//!   [`derive_asset_key`] mints one from that UUID's own hex digits.
//!   `encode` then mints a *fresh* business UUID from whatever key it is
//!   given (a deterministic hash of `(document_id, key)`, not an echo of the
//!   original), so a *second* decode derives a *different* key from that
//!   new UUID. `decode(encode(decode(x)))` is therefore a fixed point of
//!   every asset's and component's *content and structure*, but not of the
//!   specific `AssetKey` strings involved -- exactly the same "structure,
//!   not bytes" caveat `u2s-aem-ubs-mcp`'s own decode/encode round-trip test
//!   already carries, one level further down because this format's row
//!   model preserves less identity than AEM's JCR element names do.

pub mod tokenize;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;
use u2s_redacto::model::{
    Asset, AssetKey, AssetKind, Component, DocumentId, DocumentMetadata, FormPath, HtmlFragment,
    I18nHtml, Language, OwnerId, PanelStyle, Passthrough, RedactoDocument, Status, StyleName,
    ValidDocument, Violation,
};

use crate::config::{ComponentJson, ConfigurationJson};
use crate::rows::{ObjectType, OwnerType, OwnershipType};
use tokenize::{ParsedInsert, parse_i64, parse_insert_line, unquote};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("input is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("malformed dump: {0}")]
    Malformed(String),
    #[error("could not parse the document configuration JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "unsupported document configuration schema {0:?}, expected \"redacto-document/v2\" -- \
         porting the v1 upcast is out of scope for this decoder"
    )]
    UnsupportedSchema(String),
    #[error(
        "asset reference {0:?} carries a -lang- suffix, which this decoder does not yet represent"
    )]
    LanguageQualifiedReference(String),
    #[error("the dump carries no documents row")]
    NoDocument,
    #[error("the dump carries {0} documents rows, expected exactly one")]
    MultipleDocuments(usize),
    #[error("asset reference {0:?} in the configuration has no matching assets row")]
    DanglingAssetReference(String),
    #[error("asset_version row references asset_fk_id {0:?}, which has no matching assets row")]
    DanglingAssetVersion(String),
    #[error("the dump declares no document_version rows, so no language can be recovered")]
    NoLanguages,
    #[error("the dump carries no (USER, OWNER, DOCUMENT) ownership row for the document")]
    NoOwner,
    #[error(
        "asset business ids {0:?} derive the same asset key -- this decoder's key derivation is \
         not collision-free for this input"
    )]
    AssetKeyCollision(String),
    #[error("the decoded document fails validation: {0:?}")]
    Validation(Vec<Violation>),
}

struct RawAsset {
    id: String,
    asset_id: String,
    asset_type: AssetKind,
}

struct RawAssetVersion {
    language: String,
    // `asset_version.status` is parsed (so a malformed value is still
    // caught) but not carried: `u2s_redacto::model::Asset` tracks no
    // per-version status of its own -- [`DocumentMetadata::status`] is the
    // one document-wide status this model represents, taken from
    // `document_version` instead.
    content: String,
    asset_fk_id: String,
}

struct RawDocument {
    document_id: String,
    form_path: String,
    configuration: String,
}

struct RawDocumentVersion {
    language: String,
    status: Status,
}

struct RawOwnership {
    owner_id: String,
    owner_type: OwnerType,
    ownership_type: OwnershipType,
    object_id: String,
    object_type: ObjectType,
}

/// Parses a real, delivered `INSERT` script into a [`ValidDocument`].
pub fn decode(bytes: &[u8]) -> Result<ValidDocument, DecodeError> {
    let text = std::str::from_utf8(bytes)?;

    let mut raw_assets = Vec::new();
    let mut raw_asset_versions = Vec::new();
    let mut raw_documents = Vec::new();
    let mut raw_document_versions = Vec::new();
    let mut raw_ownerships = Vec::new();

    for line in text.lines() {
        let Some(parsed) = parse_insert_line(line)? else {
            continue;
        };
        match parsed.table.as_str() {
            "assets" => raw_assets.push(parse_asset_row(&parsed)?),
            "asset_version" => raw_asset_versions.push(parse_asset_version_row(&parsed)?),
            "documents" => raw_documents.push(parse_document_row(&parsed)?),
            "document_version" => raw_document_versions.push(parse_document_version_row(&parsed)?),
            "ownerships" => raw_ownerships.push(parse_ownership_row(&parsed)?),
            "relations" => { /* read but not cross-checked -- see module doc */ }
            other => {
                return Err(DecodeError::Malformed(format!(
                    "unrecognised table app_redacto.{other}"
                )));
            }
        }
    }

    let document_row = match raw_documents.len() {
        0 => return Err(DecodeError::NoDocument),
        1 => raw_documents.remove(0),
        n => return Err(DecodeError::MultipleDocuments(n)),
    };

    let configuration: ConfigurationJson = serde_json::from_str(&document_row.configuration)?;
    if configuration.schema != crate::config::SCHEMA {
        return Err(DecodeError::UnsupportedSchema(configuration.schema));
    }

    // Business asset id -> derived AssetKey, and technical PK -> business id,
    // built once and reused by both the asset list and the component walk.
    let business_by_pk: BTreeMap<&str, &str> =
        raw_assets.iter().map(|a| (a.id.as_str(), a.asset_id.as_str())).collect();
    let mut key_by_business: BTreeMap<String, AssetKey> = BTreeMap::new();
    for asset in &raw_assets {
        let key = derive_asset_key(&asset.asset_id)?;
        if let Some(existing) = key_by_business.insert(asset.asset_id.clone(), key.clone())
            && existing != key
        {
            return Err(DecodeError::AssetKeyCollision(asset.asset_id.clone()));
        }
    }

    let mut content_by_business: BTreeMap<&str, BTreeMap<Language, HtmlFragment>> =
        BTreeMap::new();
    for version in &raw_asset_versions {
        let business = *business_by_pk
            .get(version.asset_fk_id.as_str())
            .ok_or_else(|| DecodeError::DanglingAssetVersion(version.asset_fk_id.clone()))?;
        let language = Language::try_from(version.language.as_str())
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        let content = HtmlFragment::try_from(version.content.clone())
            .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        content_by_business.entry(business).or_default().insert(language, content);
    }

    let mut assets = Vec::with_capacity(raw_assets.len());
    for raw in &raw_assets {
        let key = key_by_business
            .get(&raw.asset_id)
            .expect("just inserted above")
            .clone();
        let content = content_by_business.remove(raw.asset_id.as_str()).unwrap_or_default();
        assets.push(Asset {
            key,
            kind: raw.asset_type,
            content: I18nHtml::from_entries(content),
        });
    }

    let languages: BTreeSet<Language> = raw_document_versions
        .iter()
        .map(|v| Language::try_from(v.language.as_str()).map_err(|e| DecodeError::Malformed(e.to_string())))
        .collect::<Result<_, _>>()?;
    if languages.is_empty() {
        return Err(DecodeError::NoLanguages);
    }
    let master_language = choose_master_language(&languages);
    let status = raw_document_versions
        .first()
        .map(|v| v.status)
        .unwrap_or_default();

    let owner_id = raw_ownerships
        .iter()
        .find(|o| {
            o.owner_type == OwnerType::User
                && o.ownership_type == OwnershipType::Owner
                && o.object_type == ObjectType::Document
                && o.object_id == document_row.document_id
        })
        .map(|o| OwnerId::try_from(o.owner_id.as_str()))
        .ok_or(DecodeError::NoOwner)?
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;

    let passthrough = Passthrough {
        raw_fields: configuration
            .document
            .extra
            .iter()
            .map(|(k, v)| (k.clone(), json_value_to_string(v)))
            .collect(),
    };

    let metadata = DocumentMetadata {
        document_id: DocumentId::try_from(document_row.document_id.as_str())
            .map_err(|e| DecodeError::Malformed(e.to_string()))?,
        title: configuration.document.title.clone(),
        style: configuration
            .document
            .style
            .as_deref()
            .map(StyleName::try_from)
            .transpose()
            .map_err(|e| DecodeError::Malformed(e.to_string()))?,
        form_path: Some(
            FormPath::try_from(document_row.form_path.as_str())
                .map_err(|e| DecodeError::Malformed(e.to_string()))?,
        ),
        master_language,
        languages,
        owner_id,
        status,
        passthrough,
    };

    let document = RedactoDocument {
        metadata,
        assets,
        first_header: convert_slot(&configuration.first_header, &key_by_business)?,
        header: convert_slot(&configuration.header, &key_by_business)?,
        body: convert_slot(&configuration.body, &key_by_business)?,
        footer: convert_slot(&configuration.footer, &key_by_business)?,
    };

    document.validate().map_err(DecodeError::Validation)
}

fn column<'a>(parsed: &'a ParsedInsert, name: &str) -> Result<&'a str, DecodeError> {
    parsed
        .columns
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| DecodeError::Malformed(format!("missing column {name:?} in a {} row", parsed.table)))
}

fn parse_asset_row(parsed: &ParsedInsert) -> Result<RawAsset, DecodeError> {
    let id = unquote(column(parsed, "id")?)?;
    let asset_id = unquote(column(parsed, "asset_id")?)?;
    let raw_type = unquote(column(parsed, "asset_type")?)?;
    let asset_type = AssetKind::parse_sql_literal(&raw_type)
        .ok_or_else(|| DecodeError::Malformed(format!("unknown asset_type {raw_type:?}")))?;
    Ok(RawAsset { id, asset_id, asset_type })
}

fn parse_asset_version_row(parsed: &ParsedInsert) -> Result<RawAssetVersion, DecodeError> {
    let language = unquote(column(parsed, "language")?)?;
    let raw_status = unquote(column(parsed, "status")?)?;
    Status::parse_sql_literal(&raw_status)
        .ok_or_else(|| DecodeError::Malformed(format!("unknown status {raw_status:?}")))?;
    let content = unquote(column(parsed, "content")?)?;
    let asset_fk_id = unquote(column(parsed, "asset_fk_id")?)?;
    let _version = parse_i64(column(parsed, "version")?, "version")?;
    Ok(RawAssetVersion { language, content, asset_fk_id })
}

fn parse_document_row(parsed: &ParsedInsert) -> Result<RawDocument, DecodeError> {
    let document_id = unquote(column(parsed, "document_id")?)?;
    let form_path = unquote(column(parsed, "form_path")?)?;
    let configuration = unquote(column(parsed, "configuration")?)?;
    Ok(RawDocument { document_id, form_path, configuration })
}

fn parse_document_version_row(parsed: &ParsedInsert) -> Result<RawDocumentVersion, DecodeError> {
    let language = unquote(column(parsed, "language")?)?;
    let raw_status = unquote(column(parsed, "status")?)?;
    let status = Status::parse_sql_literal(&raw_status)
        .ok_or_else(|| DecodeError::Malformed(format!("unknown status {raw_status:?}")))?;
    let _version = parse_i64(column(parsed, "version")?, "version")?;
    Ok(RawDocumentVersion { language, status })
}

fn parse_ownership_row(parsed: &ParsedInsert) -> Result<RawOwnership, DecodeError> {
    let owner_id = unquote(column(parsed, "owner_id")?)?;
    let raw_owner_type = unquote(column(parsed, "owner_type")?)?;
    let owner_type = OwnerType::parse(&raw_owner_type)
        .ok_or_else(|| DecodeError::Malformed(format!("unknown owner_type {raw_owner_type:?}")))?;
    let raw_ownership_type = unquote(column(parsed, "ownership_type")?)?;
    let ownership_type = OwnershipType::parse(&raw_ownership_type).ok_or_else(|| {
        DecodeError::Malformed(format!("unknown ownership_type {raw_ownership_type:?}"))
    })?;
    let object_id = unquote(column(parsed, "object_id")?)?;
    let raw_object_type = unquote(column(parsed, "object_type")?)?;
    let object_type = ObjectType::parse(&raw_object_type)
        .ok_or_else(|| DecodeError::Malformed(format!("unknown object_type {raw_object_type:?}")))?;
    Ok(RawOwnership { owner_id, owner_type, ownership_type, object_id, object_type })
}

/// Derives a stable [`AssetKey`] from a business asset id, since the
/// original agent-authored key name is not persisted anywhere in the row
/// model -- only the platform's own generated UUID survives. The first two
/// hex groups give sixteen hex digits, comfortably within [`AssetKey`]'s own
/// fifty-character bound and vanishingly unlikely to collide within one
/// document's own asset count.
fn derive_asset_key(business_id: &str) -> Result<AssetKey, DecodeError> {
    let compact: String = business_id.chars().filter(char::is_ascii_hexdigit).take(16).collect();
    if compact.len() < 8 {
        return Err(DecodeError::Malformed(format!(
            "asset_id {business_id:?} does not look like a UUID"
        )));
    }
    AssetKey::try_from(format!("a{compact}")).map_err(|e| DecodeError::Malformed(e.to_string()))
}

fn choose_master_language(languages: &BTreeSet<Language>) -> Language {
    let en = Language::try_from("en").expect("valid built-in language code");
    if languages.contains(&en) {
        en
    } else {
        languages.iter().next().cloned().expect("checked non-empty by the caller")
    }
}

fn json_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Splits an asset reference (`<business-id>-ver-<n>`, optionally
/// `-lang-<locale>`) and resolves it to a declared [`AssetKey`].
fn resolve_asset_ref(
    reference: &str,
    key_by_business: &BTreeMap<String, AssetKey>,
) -> Result<AssetKey, DecodeError> {
    let Some(ver_at) = reference.find("-ver-") else {
        return Err(DecodeError::Malformed(format!(
            "asset reference {reference:?} carries no -ver- marker"
        )));
    };
    let business = &reference[..ver_at];
    let remainder = &reference[ver_at + "-ver-".len()..];
    if remainder.contains("-lang-") {
        return Err(DecodeError::LanguageQualifiedReference(reference.to_owned()));
    }
    key_by_business
        .get(business)
        .cloned()
        .ok_or_else(|| DecodeError::DanglingAssetReference(reference.to_owned()))
}

fn convert_slot(
    components: &[ComponentJson],
    key_by_business: &BTreeMap<String, AssetKey>,
) -> Result<Vec<Component>, DecodeError> {
    components.iter().map(|c| convert_component(c, key_by_business)).collect()
}

fn convert_component(
    component: &ComponentJson,
    key_by_business: &BTreeMap<String, AssetKey>,
) -> Result<Component, DecodeError> {
    match component {
        ComponentJson::AssetContainer { assets, .. } => {
            let assets = assets
                .iter()
                .map(|reference| resolve_asset_ref(reference, key_by_business))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Component::AssetContainer { assets })
        }
        ComponentJson::StyledPanel { style, components, .. } => {
            let style = PanelStyle::try_from(style.as_str())
                .map_err(|e| DecodeError::Malformed(e.to_string()))?;
            let components = convert_slot(components, key_by_business)?;
            Ok(Component::StyledPanel { style, components })
        }
    }
}
