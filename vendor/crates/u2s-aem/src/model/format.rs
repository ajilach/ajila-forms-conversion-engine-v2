//! Picture-clause formats (AEM.md §18) and field validation, modelled
//! structurally so the encoder renders the clause string
//! (`date{YYYY-MM-DD}`, `num{z,zzz,zz9.99}`) rather than one being written
//! by hand and risking a malformed clause.

use serde::{Deserialize, Serialize};

use super::enums::{NamedDateFormat, NamedNumberFormat};
use super::newtypes::{DatePattern, NumberPattern, TextPattern};
use super::text::I18nText;

/// AEM.md §18.1: a date display/validation format, either a named preset or
/// an explicit symbol pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DateFormat {
    Named(NamedDateFormat),
    Pattern(DatePattern),
}

/// AEM.md §18.2: a number display/validation format, either a named preset
/// or an explicit symbol pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NumberFormat {
    Named(NamedNumberFormat),
    Pattern(NumberPattern),
}

/// A validation constraint plus the message shown when it fails
/// (AEM.md §18.3 `validatePictureClause` / `validatePictureClauseMessage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Validation {
    pub pattern: TextPattern,
    pub message: Option<I18nText>,
}

/// AEM.md §6.5 `yearRangeFrom` / `yearRangeTo`: independent offsets in years
/// from the current year, not a min/max pair, so no ordering is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearRange {
    pub before_today: u16,
    pub after_today: u16,
}
