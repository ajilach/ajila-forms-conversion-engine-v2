//! Deterministic UUID minting.
//!
//! The reference converter (`ajila-forms-conversion-engine/core/src/redacto`)
//! mints every row's primary key with `Uuid::new_v4()` and stamps every row
//! with `now()`, so two runs over the same input produce two different
//! dumps. This crate does neither: [`encode`](crate::encode) must be a pure
//! function of its input, so a golden-file comparison never flaps and
//! `decode(encode(x))` is a fixed point up to the deterministic ids these
//! functions mint. Every UUID here is a UUIDv5 derived from a stable seed
//! string under [`NAMESPACE`], and [`CREATED`] is one fixed timestamp,
//! mirroring `u2s-mapper-aem::package`'s own `FIXED_TIMESTAMP` for the same
//! reproducibility reason.

use uuid::Uuid;

/// An arbitrary but fixed 16-byte namespace, private to this crate's own
/// derivations -- there is no registry to look this up against, and none is
/// needed: it only has to be stable across runs, not globally unique.
const NAMESPACE: Uuid = Uuid::from_bytes(*b"u2s-redacto-ns16");

/// The `created` column value every row this crate mints carries. A fixed
/// timestamp, not `now()` -- see this module's own doc on reproducibility.
/// Formatted the way the reference converter's own dumps are (a space, not
/// `T`, three fractional digits, no timezone), so a decoded-then-re-encoded
/// document's `created` column reads the same shape a real delivered one
/// does.
pub const CREATED: &str = "1970-01-01 00:00:00.000";

fn derive(seed: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, seed.as_bytes())
}

/// `assets.id` (the technical primary key, distinct from
/// [`asset_business_id`]).
pub fn asset_pk(document_id: &str, asset_key: &str) -> Uuid {
    derive(&format!("{document_id}/asset/{asset_key}"))
}

/// `assets.asset_id` -- the business identifier written into the
/// configuration's own asset references as `<id>-ver-1`.
pub fn asset_business_id(document_id: &str, asset_key: &str) -> Uuid {
    derive(&format!("{document_id}/asset-id/{asset_key}"))
}

pub fn asset_version_pk(document_id: &str, asset_key: &str, language: &str) -> Uuid {
    derive(&format!("{document_id}/asset-version/{asset_key}/{language}"))
}

pub fn document_pk(document_id: &str) -> Uuid {
    derive(&format!("{document_id}/document"))
}

pub fn document_version_pk(document_id: &str, language: &str) -> Uuid {
    derive(&format!("{document_id}/document-version/{language}"))
}

/// One ownership row's id, keyed by `seed` -- either `"owner"` for the
/// mandatory owner row, or `"asset-origin/{key}"` per asset.
pub fn ownership_pk(document_id: &str, seed: &str) -> Uuid {
    derive(&format!("{document_id}/ownership/{seed}"))
}

pub fn relation_pk(document_id: &str, seed: &str) -> Uuid {
    derive(&format!("{document_id}/relation/{seed}"))
}

/// A component's own `id` field in the platform's configuration JSON --
/// mechanical and encoder-derived (see `u2s_redacto::model::Component`'s own
/// doc on why the model itself carries none), keyed by the component's
/// position (`path`, e.g. `"0"`, `"1.2"`) within its slot.
pub fn component_id(document_id: &str, slot: &str, path: &str) -> Uuid {
    derive(&format!("{document_id}/component/{slot}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        assert_eq!(
            asset_pk("aaev_019", "intro"),
            asset_pk("aaev_019", "intro")
        );
    }

    #[test]
    fn different_seeds_derive_different_uuids() {
        assert_ne!(asset_pk("aaev_019", "intro"), asset_pk("aaev_019", "outro"));
        assert_ne!(
            asset_pk("aaev_019", "intro"),
            asset_business_id("aaev_019", "intro"),
            "the technical PK and the business id must never collide"
        );
    }
}
