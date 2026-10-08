//! The `documents.configuration` JSON -- the platform's own
//! `redacto-document/v2` contract
//! (`ajila-redacto-platform/.context/architecture/document-config.md`).
//!
//! [`build_configuration`] is the encode-side walk: every
//! [`u2s_redacto::model::Component`] the agent authored becomes a
//! [`ComponentJson`] with a mechanically derived `id`
//! ([`crate::ids::component_id`]) and its `AssetKey`s rewritten to the
//! platform's own `<business-id>-ver-1` references. [`decode::parse`] is the
//! exact inverse, reached from [`crate::decode`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use u2s_redacto::model::{Component, DocumentMetadata, RedactoDocument};

use crate::ids;

pub const SCHEMA: &str = "redacto-document/v2";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationJson {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub document: DocumentMetaJson,
    #[serde(rename = "firstHeader", default, skip_serializing_if = "Vec::is_empty")]
    pub first_header: Vec<ComponentJson>,
    #[serde(default)]
    pub header: Vec<ComponentJson>,
    pub body: Vec<ComponentJson>,
    #[serde(default)]
    pub footer: Vec<ComponentJson>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetaJson {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Any `document.*` field this JSON carried beyond `id`/`title`/`style`
    /// -- captured so a decoded document that has one round-trips instead of
    /// silently dropping it. `u2s_redacto::model::Passthrough::raw_fields`
    /// is a flat string map, so a non-string extra value is stringified via
    /// its own JSON rendering on encode and left as a JSON string on decode
    /// (see [`decode::parse`]) -- an accepted, narrow gap for a value shape
    /// the reference converter never actually produces.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ComponentJson {
    AssetContainer { id: String, assets: Vec<String> },
    StyledPanel {
        id: String,
        style: String,
        components: Vec<ComponentJson>,
    },
}

pub fn build_configuration(
    doc: &RedactoDocument,
    asset_business_ids: &BTreeMap<&str, String>,
) -> ConfigurationJson {
    let document_id = doc.metadata.document_id.as_str();
    ConfigurationJson {
        schema: SCHEMA.to_owned(),
        document: build_document_meta(&doc.metadata),
        first_header: build_slot(&doc.first_header, document_id, "firstHeader", asset_business_ids),
        header: build_slot(&doc.header, document_id, "header", asset_business_ids),
        body: build_slot(&doc.body, document_id, "body", asset_business_ids),
        footer: build_slot(&doc.footer, document_id, "footer", asset_business_ids),
    }
}

fn build_document_meta(metadata: &DocumentMetadata) -> DocumentMetaJson {
    let extra = metadata
        .passthrough
        .raw_fields
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    DocumentMetaJson {
        id: metadata.document_id.to_string(),
        title: metadata.title.clone(),
        style: metadata.style.as_ref().map(|s| s.to_string()),
        extra,
    }
}

fn build_slot(
    components: &[Component],
    document_id: &str,
    slot: &str,
    asset_business_ids: &BTreeMap<&str, String>,
) -> Vec<ComponentJson> {
    components
        .iter()
        .enumerate()
        .map(|(index, component)| {
            build_component(
                component,
                document_id,
                slot,
                &index.to_string(),
                asset_business_ids,
            )
        })
        .collect()
}

fn build_component(
    component: &Component,
    document_id: &str,
    slot: &str,
    path: &str,
    asset_business_ids: &BTreeMap<&str, String>,
) -> ComponentJson {
    let id = ids::component_id(document_id, slot, path).to_string();
    match component {
        Component::AssetContainer { assets } => ComponentJson::AssetContainer {
            id,
            assets: assets
                .iter()
                .map(|key| {
                    let business_id = asset_business_ids
                        .get(key.as_str())
                        .expect("validate() guarantees every reference resolves to a declared asset");
                    format!("{business_id}-ver-1")
                })
                .collect(),
        },
        Component::StyledPanel { style, components } => ComponentJson::StyledPanel {
            id,
            style: style.to_string(),
            components: components
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    build_component(
                        child,
                        document_id,
                        slot,
                        &format!("{path}.{index}"),
                        asset_business_ids,
                    )
                })
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use u2s_redacto::model::AssetKey;

    fn key(name: &str) -> AssetKey {
        AssetKey::try_from(name).unwrap()
    }

    #[test]
    fn an_asset_container_becomes_a_ver_one_reference() {
        let mut ids_map = BTreeMap::new();
        ids_map.insert("intro", "d91a68dc-1348-4113-94e4-4d1e0c3db317".to_owned());
        let component = Component::AssetContainer {
            assets: vec![key("intro")],
        };
        let json = build_component(&component, "aaev_019", "body", "0", &ids_map);
        match json {
            ComponentJson::AssetContainer { assets, .. } => {
                assert_eq!(assets, vec!["d91a68dc-1348-4113-94e4-4d1e0c3db317-ver-1"]);
            }
            other => panic!("expected an assetContainer, got {other:?}"),
        }
    }

    #[test]
    fn component_ids_are_deterministic_and_positional() {
        let mut ids_map = BTreeMap::new();
        ids_map.insert("a", "11111111-1111-1111-1111-111111111111".to_owned());
        let components = vec![
            Component::AssetContainer { assets: vec![key("a")] },
            Component::AssetContainer { assets: vec![key("a")] },
        ];
        let first = build_slot(&components, "doc", "body", &ids_map);
        let second = build_slot(&components, "doc", "body", &ids_map);
        assert_eq!(first, second, "the same input must produce the same ids");

        let (ComponentJson::AssetContainer { id: id0, .. }, ComponentJson::AssetContainer { id: id1, .. }) =
            (&first[0], &first[1])
        else {
            panic!("expected two assetContainers");
        };
        assert_ne!(id0, id1, "different positions must derive different component ids");
    }

    #[test]
    fn document_meta_serializes_without_a_style_when_absent() {
        use u2s_redacto::model::{DocumentId, Language, OwnerId, Passthrough, Status};
        use std::collections::BTreeSet;

        let metadata = DocumentMetadata {
            document_id: DocumentId::try_from("aaev_019").unwrap(),
            title: "AAEV_019".to_owned(),
            style: None,
            form_path: None,
            master_language: Language::try_from("en").unwrap(),
            languages: BTreeSet::from([Language::try_from("en").unwrap()]),
            owner_id: OwnerId::try_from("admin").unwrap(),
            status: Status::Draft,
            passthrough: Passthrough::default(),
        };
        let json = build_document_meta(&metadata);
        let value = serde_json::to_value(&json).unwrap();
        assert!(value.get("style").is_none());
    }
}
