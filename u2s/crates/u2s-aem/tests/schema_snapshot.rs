//! The committed schema snapshot exists so a model change becomes a
//! reviewable diff rather than a silent mutation under existing runs — the
//! same discipline PLAN.md requires of a format server's manifest. This test
//! asserts the checked-in file still matches what the model generates, and
//! that every object schema with a fixed field set is closed.

const SNAPSHOT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/aem.v1.schema.json");

#[test]
fn schema_matches_committed_snapshot() {
    let generated = u2s_aem::schema();
    let text = std::fs::read_to_string(SNAPSHOT_PATH).unwrap_or_else(|e| {
        panic!(
            "missing schema snapshot at {SNAPSHOT_PATH}: {e}. Generate it with \
             `cargo run -p u2s-aem --example print_schema > crates/u2s-aem/schema/aem.v1.schema.json` \
             after reviewing the diff."
        )
    });
    let committed: serde_json::Value =
        serde_json::from_str(&text).expect("committed snapshot is valid JSON");
    assert_eq!(
        generated, committed,
        "the generated schema no longer matches schema/aem.v1.schema.json — review the diff, \
         then regenerate the snapshot"
    );
}

#[test]
fn every_object_schema_with_named_properties_is_closed() {
    let schema = u2s_aem::schema();
    let mut open = Vec::new();
    walk(&schema, "#", &mut open);
    assert!(
        open.is_empty(),
        "objects with fixed `properties` must set additionalProperties: false: {open:?}"
    );
}

/// Recursively visits every object in the schema document. An object schema
/// that declares `properties` (a fixed field set) must also declare
/// `additionalProperties: false`. Map-shaped schemas (`I18nText`,
/// `I18nRichText`) declare `patternProperties` instead of `properties` and
/// are correctly excluded by this check — they are closed a different way
/// (the pattern itself is the only key shape schemars emits for them).
///
/// A schema object's `properties` keyword is itself a map of
/// `{field_name: subschema}` — not a schema object in its own right, so it
/// must never be checked for closedness itself, and its own keys (field
/// names) must never be mistaken for JSON Schema keywords one level up.
/// `AemForm` has a `Node::Component::properties` field (and `Page` one to
/// match) whose name collides with the `properties` *keyword* purely by
/// coincidence: without this distinction, generic recursion would walk
/// into that field's own subschema map, see a key literally named
/// `"properties"`, and misreport it as an unclosed schema object one level
/// too deep. So `properties`'/`patternProperties`' values recurse over
/// their entries' subschemas directly, skipping the map wrapper itself.
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
                    // `child` is `{field_or_pattern: subschema}`, not a
                    // schema object itself — walk straight into each
                    // subschema, so a field/pattern name that happens to
                    // read as a JSON Schema keyword (like `properties`
                    // itself) is never inspected as one.
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

#[test]
fn a_field_named_properties_does_not_confuse_the_walker() {
    // The regression this session found: `Node::Component::properties` and
    // `Page::properties` collide, by name only, with the JSON Schema
    // `properties` keyword. Exercised directly here (not only through the
    // full generated schema) so a future model change that removes the
    // last field literally named `properties` does not silently retire
    // the coverage.
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "properties": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "inner": { "type": "string" }
                }
            }
        },
        "additionalProperties": false
    });
    let mut open = Vec::new();
    walk(&schema, "#", &mut open);
    assert!(open.is_empty(), "false positive from the field-name collision: {open:?}");
}
