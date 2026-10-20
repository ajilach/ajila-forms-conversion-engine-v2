//! JSON Schema, generated and checked.
//!
//! - [`tool_input_schema`]: a tool's arguments, generated from a Rust type
//!   (see the `generate` module), and [`tool_schema_violations`] for the
//!   shape providers accept.
//! - [`validate`]: every value that breaks a schema, by JSON Pointer.
//! - [`skeleton`]: a minimal document satisfying a schema's required shape.
//! - [`empty_optionals`]: the optional properties a schema lets be empty.
//!
//! Everything here is pure: no I/O, no provider types.

mod empties;
mod generate;
mod ref_resolve;
mod skeleton;
mod validate;

pub use empties::empty_optionals;
pub use generate::{NoArgs, parse_tool_args, tool_input_schema, tool_schema_violations};
pub use skeleton::{SkeletonError, skeleton};
pub use validate::{SchemaError, Violation, validate};
