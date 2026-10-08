//! The six typed row structs (`app_redacto`'s own tables) and the three
//! enum domains that exist only at this mechanical layer -- `owner_type`,
//! `ownership_type`, `object_type` are encoder bookkeeping the Conversion
//! Agent never authors, unlike `u2s_redacto::model::{AssetKind, Status}`,
//! which the agent does choose.
//!
//! [`build`] is the one place this crate decides "how is an already-decided
//! document spelled as rows": both UUIDs per asset, the mandatory owner row,
//! one origin ownership + relation row per asset, one `document_version`
//! per declared language. None of it is a judgment call -- see this crate's
//! own module doc.

use std::collections::BTreeMap;

use u2s_redacto::model::{AssetKind, Status, ValidDocument};

use crate::config;
use crate::ids;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerType {
    User,
    Document,
    Fragment,
}

impl OwnerType {
    pub fn as_sql_literal(self) -> &'static str {
        match self {
            OwnerType::User => "USER",
            OwnerType::Document => "DOCUMENT",
            OwnerType::Fragment => "FRAGMENT",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "USER" => Some(OwnerType::User),
            "DOCUMENT" => Some(OwnerType::Document),
            "FRAGMENT" => Some(OwnerType::Fragment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipType {
    Origin,
    Owner,
    Member,
}

impl OwnershipType {
    pub fn as_sql_literal(self) -> &'static str {
        match self {
            OwnershipType::Origin => "ORIGIN",
            OwnershipType::Owner => "OWNER",
            OwnershipType::Member => "MEMBER",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ORIGIN" => Some(OwnershipType::Origin),
            "OWNER" => Some(OwnershipType::Owner),
            "MEMBER" => Some(OwnershipType::Member),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Document,
    Fragment,
    Text,
    Image,
}

impl ObjectType {
    pub fn as_sql_literal(self) -> &'static str {
        match self {
            ObjectType::Document => "DOCUMENT",
            ObjectType::Fragment => "FRAGMENT",
            ObjectType::Text => "TEXT",
            ObjectType::Image => "IMAGE",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "DOCUMENT" => Some(ObjectType::Document),
            "FRAGMENT" => Some(ObjectType::Fragment),
            "TEXT" => Some(ObjectType::Text),
            "IMAGE" => Some(ObjectType::Image),
            _ => None,
        }
    }

    fn from_asset_kind(kind: AssetKind) -> Self {
        match kind {
            AssetKind::Text => ObjectType::Text,
            AssetKind::Image => ObjectType::Image,
        }
    }
}

pub struct AssetRow {
    pub id: String,
    pub created: String,
    pub asset_id: String,
    pub asset_type: AssetKind,
}

pub struct AssetVersionRow {
    pub id: String,
    pub created: String,
    pub language: String,
    pub version: i64,
    pub status: Status,
    pub content: String,
    pub asset_fk_id: String,
}

pub struct DocumentRow {
    pub id: String,
    pub created: String,
    pub document_id: String,
    pub form_path: String,
    /// The already-serialized `redacto-document/v2` JSON -- built once in
    /// [`config`], not re-derived by the SQL writer.
    pub configuration: String,
}

pub struct DocumentVersionRow {
    pub id: String,
    pub created: String,
    pub language: String,
    pub version: i64,
    pub status: Status,
    pub document_fk_id: String,
}

pub struct OwnershipRow {
    pub id: String,
    pub created: String,
    pub owner_id: String,
    pub owner_type: OwnerType,
    pub ownership_type: OwnershipType,
    pub object_id: String,
    pub object_type: ObjectType,
}

pub struct RelationRow {
    pub id: String,
    pub created: String,
    pub relates_to: String,
    pub object_id: String,
    pub object_type: ObjectType,
}

/// A complete set of rows describing one Redacto document, in the shape
/// [`crate::sql::to_sql`] serializes.
#[derive(Default)]
pub struct Dump {
    pub assets: Vec<AssetRow>,
    pub asset_versions: Vec<AssetVersionRow>,
    pub documents: Vec<DocumentRow>,
    pub document_versions: Vec<DocumentVersionRow>,
    pub ownerships: Vec<OwnershipRow>,
    pub relations: Vec<RelationRow>,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("could not serialize the document configuration: {0}")]
    Config(#[from] serde_json::Error),
}

/// The one function that turns an already-validated, already-decided
/// document into rows. Every value here is either carried straight from the
/// document or minted by [`ids`] from a stable seed -- never chosen.
pub fn build(doc: &ValidDocument) -> Result<Dump, BuildError> {
    let doc = doc.document();
    let document_id = doc.metadata.document_id.as_str();
    let document_pk = ids::document_pk(document_id).to_string();
    let created = ids::CREATED.to_owned();

    let form_path = doc
        .metadata
        .form_path
        .as_ref()
        .map(|p| p.to_string())
        .unwrap_or_else(|| format!("/content/forms/af/redacto-documents/{document_id}"));

    let mut dump = Dump::default();
    let mut asset_business_ids: BTreeMap<&str, String> = BTreeMap::new();

    for asset in &doc.assets {
        let key = asset.key.as_str();
        let pk = ids::asset_pk(document_id, key).to_string();
        let business_id = ids::asset_business_id(document_id, key).to_string();
        asset_business_ids.insert(key, business_id.clone());

        dump.assets.push(AssetRow {
            id: pk.clone(),
            created: created.clone(),
            asset_id: business_id.clone(),
            asset_type: asset.kind,
        });

        // Every declared language, not merely every language the asset
        // happens to carry content for -- `ValidDocument` already guarantees
        // the two coincide (`RedactoDocument::validate`'s coverage check).
        for language in &doc.metadata.languages {
            let content = asset
                .content
                .get(language)
                .expect("validate() guarantees every declared language has content")
                .to_string();
            dump.asset_versions.push(AssetVersionRow {
                id: ids::asset_version_pk(document_id, key, language.as_str()).to_string(),
                created: created.clone(),
                language: language.to_string(),
                version: 1,
                status: doc.metadata.status,
                content,
                asset_fk_id: pk.clone(),
            });
        }

        let object_type = ObjectType::from_asset_kind(asset.kind);
        let asset_ref = format!("{business_id}-ver-1");
        dump.ownerships.push(OwnershipRow {
            id: ids::ownership_pk(document_id, &format!("asset-origin/{key}")).to_string(),
            created: created.clone(),
            owner_id: document_id.to_owned(),
            owner_type: OwnerType::Document,
            ownership_type: OwnershipType::Origin,
            object_id: asset_ref.clone(),
            object_type,
        });
        dump.relations.push(RelationRow {
            id: ids::relation_pk(document_id, &format!("asset/{key}")).to_string(),
            created: created.clone(),
            relates_to: document_id.to_owned(),
            object_id: asset_ref,
            object_type,
        });
    }

    let configuration = config::build_configuration(doc, &asset_business_ids);
    let configuration_json = serde_json::to_string(&configuration)?;

    dump.documents.push(DocumentRow {
        id: document_pk.clone(),
        created: created.clone(),
        document_id: document_id.to_owned(),
        form_path,
        configuration: configuration_json,
    });

    for language in &doc.metadata.languages {
        dump.document_versions.push(DocumentVersionRow {
            id: ids::document_version_pk(document_id, language.as_str()).to_string(),
            created: created.clone(),
            language: language.to_string(),
            version: 1,
            status: doc.metadata.status,
            document_fk_id: document_pk.clone(),
        });
    }

    // The mandatory owner row: Redacto rejects every authoring write
    // against a document with no `(owner_id, USER, OWNER, document_id,
    // DOCUMENT)` ownership row.
    dump.ownerships.push(OwnershipRow {
        id: ids::ownership_pk(document_id, "owner").to_string(),
        created,
        owner_id: doc.metadata.owner_id.to_string(),
        owner_type: OwnerType::User,
        ownership_type: OwnershipType::Owner,
        object_id: document_id.to_owned(),
        object_type: ObjectType::Document,
    });

    Ok(dump)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_literals_round_trip_through_parse() {
        for owner_type in [OwnerType::User, OwnerType::Document, OwnerType::Fragment] {
            assert_eq!(OwnerType::parse(owner_type.as_sql_literal()), Some(owner_type));
        }
        for ownership_type in [OwnershipType::Origin, OwnershipType::Owner, OwnershipType::Member] {
            assert_eq!(
                OwnershipType::parse(ownership_type.as_sql_literal()),
                Some(ownership_type)
            );
        }
        for object_type in [
            ObjectType::Document,
            ObjectType::Fragment,
            ObjectType::Text,
            ObjectType::Image,
        ] {
            assert_eq!(ObjectType::parse(object_type.as_sql_literal()), Some(object_type));
        }
    }
}
