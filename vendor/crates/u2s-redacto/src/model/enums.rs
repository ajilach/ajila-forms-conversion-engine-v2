//! Every Redacto attribute whose legal values are a closed, enumerable set,
//! typed as an enum instead of a free string. An invalid value fails to
//! deserialize rather than reaching the encoder, and the schema's `enum`
//! keyword documents the exact set to a prompt-reading agent.

use serde::{Deserialize, Serialize};

/// `assets.asset_type`. Redacto has no field-input concept at all -- the
/// forms engine skips input fields with a warning, since "the Redacto
/// target supports text-only documents" -- so this is the whole domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Text,
    Image,
}

impl AssetKind {
    /// The literal `u2s-mapper-redacto` writes into the `asset_type`
    /// column.
    pub fn as_sql_literal(self) -> &'static str {
        match self {
            AssetKind::Text => "TEXT",
            AssetKind::Image => "IMAGE",
        }
    }

    /// The exact inverse of [`AssetKind::as_sql_literal`], for
    /// `u2s-mapper-redacto`'s decoder. Kept beside the encoding half rather
    /// than duplicated at the decode site, so the domain has one place that
    /// knows both directions.
    pub fn parse_sql_literal(value: &str) -> Option<Self> {
        match value {
            "TEXT" => Some(AssetKind::Text),
            "IMAGE" => Some(AssetKind::Image),
            _ => None,
        }
    }
}

/// `asset_version.status` / `document_version.status`. Every variant the
/// encoder ever mints is [`Status::Draft`] on a fresh conversion; the other
/// three exist because a decoded, real delivered document may carry them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Draft,
    InReview,
    Released,
    Deprecated,
}

impl Status {
    /// The literal `u2s-mapper-redacto` writes into the `status` column.
    pub fn as_sql_literal(self) -> &'static str {
        match self {
            Status::Draft => "DRAFT",
            Status::InReview => "IN_REVIEW",
            Status::Released => "RELEASED",
            Status::Deprecated => "DEPRECATED",
        }
    }

    /// The exact inverse of [`Status::as_sql_literal`]; see
    /// [`AssetKind::parse_sql_literal`]'s own doc for why this lives here.
    pub fn parse_sql_literal(value: &str) -> Option<Self> {
        match value {
            "DRAFT" => Some(Status::Draft),
            "IN_REVIEW" => Some(Status::InReview),
            "RELEASED" => Some(Status::Released),
            "DEPRECATED" => Some(Status::Deprecated),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_defaults_to_draft() {
        assert_eq!(Status::default(), Status::Draft);
    }

    #[test]
    fn sql_literals_match_the_platform_ddl() {
        assert_eq!(AssetKind::Text.as_sql_literal(), "TEXT");
        assert_eq!(AssetKind::Image.as_sql_literal(), "IMAGE");
        assert_eq!(Status::Draft.as_sql_literal(), "DRAFT");
        assert_eq!(Status::InReview.as_sql_literal(), "IN_REVIEW");
        assert_eq!(Status::Released.as_sql_literal(), "RELEASED");
        assert_eq!(Status::Deprecated.as_sql_literal(), "DEPRECATED");
    }
}
