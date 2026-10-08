//! The committed schema snapshot exists so a model change becomes a
//! reviewable diff rather than a silent mutation under existing runs -- the
//! same discipline PLAN.md requires of a format server's manifest. This test
//! asserts the checked-in file still matches what the model generates, and
//! that every object schema with a fixed field set is closed.

const SNAPSHOT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/redacto.v1.schema.json");

#[test]
fn schema_matches_committed_snapshot() {
    let generated = u2s_redacto::schema();
    let text = std::fs::read_to_string(SNAPSHOT_PATH).unwrap_or_else(|e| {
        panic!(
            "missing schema snapshot at {SNAPSHOT_PATH}: {e}. Generate it with \
             `cargo run -p u2s-redacto --example print_schema > crates/u2s-redacto/schema/redacto.v1.schema.json` \
             after reviewing the diff."
        )
    });
    let committed: serde_json::Value =
        serde_json::from_str(&text).expect("committed snapshot is valid JSON");
    assert_eq!(
        generated, committed,
        "the generated schema no longer matches schema/redacto.v1.schema.json -- review the \
         diff, then regenerate the snapshot"
    );
}

#[test]
fn every_object_schema_with_named_properties_is_closed() {
    let schema = u2s_redacto::schema();
    let mut open = Vec::new();
    walk(&schema, "#", &mut open);
    assert!(
        open.is_empty(),
        "objects with fixed `properties` must set additionalProperties: false: {open:?}"
    );
}

/// Recursively visits every object in the schema document. See
/// `u2s-aem/tests/schema_snapshot.rs`'s own copy of this walker for why
/// `properties`/`patternProperties` values must be skipped as map wrappers
/// rather than checked as schema objects themselves.
fn walk(value: &serde_json::Value, path: &str, open: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.contains_key("properties") {
                let closed = matches!(
                    map.get("additionalProperties"),
                    Some(serde_json::Value::Bool(false))
                );
                if !closed {
                    open.push(path.to_string());
                }
            }
            for (key, child) in map {
                if key == "properties" || key == "patternProperties" {
                    if let serde_json::Value::Object(fields) = child {
                        for (field, subschema) in fields {
                            walk(subschema, &format!("{path}/{key}/{field}"), open);
                        }
                    }
                } else {
                    walk(child, &format!("{path}/{key}"), open);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(item, &format!("{path}/{index}"), open);
            }
        }
        _ => {}
    }
}
