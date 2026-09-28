//! The native JSON-document and rule tools, and the pure JSON-schema
//! functions they build on ([`u2s_schema`]), with no dependency on rig,
//! `u2s-engine` or `u2s-agent`.
//!
//! [`native`] is the Conversion Agent's whole means of touching the working
//! output document -- see its module doc for the design. [`rules_dir`] is a
//! file-based alternative to the store-backed rule loading `u2s-agent`
//! itself does, for a host that has no database: it reads one rule per
//! subdirectory of a rules directory and produces the same [`native::RuleForCheck`]
//! the tools already dispatch against.
//!
//! Kept as a separate crate from `u2s-agent` so a host outside this
//! workspace -- another Rust application with its own rig-based agent,
//! pinned to a different rig version -- can use these tools without pulling
//! in rig at all.

pub mod native;
pub mod rules_dir;
