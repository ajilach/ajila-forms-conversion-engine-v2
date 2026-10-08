//! Prints the current `RedactoDocument` JSON Schema to stdout. Used to
//! regenerate the committed snapshot after a deliberate model change:
//!
//! ```sh
//! cargo run -p u2s-redacto --example print_schema > crates/u2s-redacto/schema/redacto.v1.schema.json
//! ```
//!
//! Review the diff before committing — `tests/schema_snapshot.rs` fails the
//! build until the snapshot is regenerated, which is the point: a model
//! change becomes a reviewable schema diff, never a silent mutation.

fn main() {
    let schema = u2s_redacto::schema();
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}
