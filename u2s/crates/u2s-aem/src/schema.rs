//! Schema generation: [`schema`] IS the format schema, generated once from
//! [`crate::model::AemForm`] via `schemars`. There is no hand-maintained
//! schema file for it to drift from — the committed snapshot under
//! `schema/` exists to make a model change a reviewable diff, not to be
//! edited by hand.

use schemars::schema_for;

use crate::model::AemForm;

/// The JSON Schema for [`AemForm`]: the entire intermediate output JSON the
/// Conversion Agent edits.
pub fn schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(AemForm)).expect("AemForm schema serializes")
}
