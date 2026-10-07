//! One typed argument struct per tool, deserialized once at the top of
//! [`crate::ReferencesServer::dispatch`]. A missing required field, a wrong
//! type or an unknown field is an error, never a silent default. An optional
//! field may be absent or `null`, both meaning "not given"; the defaults are
//! the documented ones (`limit` 0 means the default window, `top_k` is at
//! least 1), applied in the server.

use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ListReferenceForms {}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct SearchReferences {
    pub query: String,
    pub top_k: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct GrepReferences {
    pub query: String,
    pub regex: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ReadReferenceFile {
    pub ref_id: String,
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct GetReferencePackage {
    pub ref_id: String,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ListReferenceDocs {}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ReadReferenceDoc {
    pub doc_id: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct GrepReferenceDocs {
    pub query: String,
    pub regex: Option<bool>,
}
