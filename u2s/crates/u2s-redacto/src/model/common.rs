//! [`Passthrough`] carries whatever a decoded, real delivered document's
//! `documents` row holds beyond `document_id`/`title`/`style`/`form_path` --
//! there is little of it (the platform's schema is six narrow tables, not
//! AEM's open JCR tree), but `u2s-aem::model::common::Passthrough` was
//! retrofitted after a `0.1.0 -> 0.2.0` format-version bump once a real
//! decoded document turned out to carry fields the model had no typed slot
//! for. Carrying the escape hatch from day one avoids repeating that.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A document's raw remainder: any column or configuration-JSON field
/// decode did not fold into a typed field, carried verbatim so a round trip
/// through `u2s-mapper-redacto`'s decoder and encoder loses nothing. Never
/// interpreted by this crate or by [`super::RedactoDocument::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Passthrough {
    pub raw_fields: BTreeMap<String, String>,
}

impl Passthrough {
    pub fn is_empty(&self) -> bool {
        self.raw_fields.is_empty()
    }
}
