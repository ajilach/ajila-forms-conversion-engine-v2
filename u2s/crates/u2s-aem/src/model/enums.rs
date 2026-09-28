//! Every AEM attribute whose legal values are a closed, enumerable set,
//! typed as an enum instead of a free string. An invalid value fails to
//! deserialize rather than reaching the encoder, and the schema's `enum`
//! keyword documents the exact set to a prompt-reading agent.

use serde::{Deserialize, Serialize};

use super::newtypes::XmlName;

/// AEM.md §5.3 `dorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DorMode {
    None,
    Generate,
}

/// AEM.md §19.1 `formmodel`. `Unbound` means no field carries a `bindRef`;
/// `XmlSchema` means the encoder generates an XSD and assigns every
/// `bindRef` in that same walk, which is why `bindRef` never appears
/// anywhere in this model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataModel {
    Unbound,
    XmlSchema { root_element: XmlName },
}

/// AEM.md §6.3 `multiLine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextInput {
    SingleLine,
    MultiLine,
}

/// AEM.md §19.3 `autofillFieldKeyword`: the HTML `autocomplete` vocabulary
/// AEM exposes, not the full HTML spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AutofillHint {
    Name,
    GivenName,
    FamilyName,
    Email,
    Tel,
    StreetAddress,
    AddressLine1,
    AddressLine2,
    PostalCode,
    Country,
    Bday,
    Organization,
}

/// AEM.md §6.6 `sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// AEM.md §6.7/§6.8 `alignment`, for checkbox and radio button groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OptionAlignment {
    Horizontal,
    Vertical,
}

/// AEM.md §6.1 `assistPriority`. Defaults to `Label`, AEM's own default.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AssistPriority {
    #[default]
    Label,
    Caption,
    Custom,
}

/// A heading level, for a title's `headingLevel` (AEM.md §6.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

/// AEM.md §18.1 named date formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamedDateFormat {
    Short,
    Medium,
    Long,
    Full,
}

/// AEM.md §18.2 named number formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamedNumberFormat {
    Integer,
    Decimal,
    Currency,
    Percent,
}
