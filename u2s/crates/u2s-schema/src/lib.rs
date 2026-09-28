//! Strict-schema adaptation.
//!
//! OpenAI's structured-output `strict: true` accepts only a subset of JSON
//! Schema, and rig hardcodes `strict: true` whenever it maps an `output_schema`
//! onto `response_format` — so an arbitrary output-format schema cannot be
//! handed to a provider unadapted. Registered output formats bring whatever
//! schema they like, so the pipeline is:
//!
//! ```text
//! original ──adapt──► strict-safe ──generate──► value ──restore──► value
//!                                                       ──validate against ORIGINAL
//! ```
//!
//! Two rules keep this honest:
//!
//! 1. **Adaptation only ever loosens.** Constraint keywords the strict subset
//!    cannot express (`pattern`, `minimum`, `format`, …) are stripped and
//!    recorded, never approximated. The authoritative check is always
//!    [`validate`] against the *original* schema, so a stripped constraint
//!    resurfaces as a violation rather than passing silently.
//! 2. **Anything that cannot be loosened safely is a hard error.** `allOf`,
//!    `not` and `if`/`then`/`else` change meaning when dropped, so [`adapt`]
//!    refuses them by pointer instead of quietly mangling the schema.
//!
//! One transformation is not a loosening but a re-encoding: strict mode requires
//! `additionalProperties: false` on every object, which leaves no way to express
//! a map. A map-shaped object becomes an array of `{key, value}` pairs, and
//! [`restore`] turns it back. Without this the day-one AEM schema cannot be
//! generated at all — its `I18nText` and `I18nRichText` are `patternProperties`
//! maps keyed by language code.
//!
//! [`adapt`] and [`restore`] are inverses, and [`restore`] is driven by the
//! *original* schema rather than by bookkeeping from [`adapt`] — so the two
//! cannot drift out of step as the adapter grows.
//!
//! Everything here is pure: no I/O, no provider types.

mod adapt;
mod ref_resolve;
mod restore;
mod skeleton;
mod strict;
mod validate;

pub use adapt::{AdaptError, Adapted, Relaxation, adapt};
pub use restore::restore;
pub use skeleton::{SkeletonError, skeleton};
pub use strict::strict_violations;
pub use validate::{SchemaError, Violation, validate};

/// The property name pairs use for a re-encoded map's key.
pub const MAP_KEY: &str = "key";
/// The property name pairs use for a re-encoded map's value.
pub const MAP_VALUE: &str = "value";
