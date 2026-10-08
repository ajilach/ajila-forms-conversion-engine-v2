//! Pure fact logic for extrinsic rules.
//!
//! A **fact** is a named question about an input with a strict answer
//! schema, extracted once per input (by an LLM, or by a sandboxed script over
//! the stored ingest data) and handed to a rule's `check(output, ctx)` as
//! `ctx.facts`. A rule whose script declares `const requires = [...]` is
//! **extrinsic**; one that declares nothing is **intrinsic**. The LLM only
//! extracts: the script still decides, so a verdict stays a recorded,
//! reproducible fact about one revision against one run.
//!
//! This crate holds every decision about facts that needs no I/O, so each is
//! tested once and reused by the store, the service layer and the agents:
//!
//! - [`FactKey`]: the one conversion point for a fact's name.
//! - [`FactValue`]: one stored `fact_values` row, as a type that cannot hold
//!   an inconsistent combination of columns.
//! - [`resolve_facts`]: whether a rule revision can be checked against an
//!   input at all, and with which `ctx.facts` -- the single place a verdict
//!   becomes `indeterminate`.
//! - [`extractions_agree`]: the stability gate's comparison.
//! - [`select_sample`]: which inputs a draft fact is extracted on.
//! - [`diagnostics`]: advisory signals over a fact's values.

mod definition;
pub mod diagnostics;
mod key;
mod resolve;
mod sample;
mod value;

pub use definition::{FactSource, FactSourceError, IndexProjection};
pub use key::{FactKey, FactKeyError};
pub use resolve::{FactsForCheck, RuleGrounding, resolve_facts};
pub use sample::{FACT_SAMPLE_INPUTS, select_sample};
pub use value::{Extraction, FactValue, FactValueError, extractions_agree};
