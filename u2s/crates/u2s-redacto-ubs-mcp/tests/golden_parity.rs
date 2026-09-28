//! Parity with the retired deterministic engine (`tests/fixtures/golden/`).
//!
//! Each golden form carries `dump.sql`, the old engine's Redacto dump, and
//! `document.json`: that dump's body and assets without the furniture, plus
//! each language's source (its XFA variables and page header) exactly as the
//! old engine read them. Encoding the document must give a dump that decodes
//! to the same document as the golden one: the same metadata, header, body and
//! footer, with every asset reference compared by the asset's content, since
//! the old engine drew its asset ids at random.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use u2s_redacto::model::RedactoDocument;
use u2s_redacto_ubs_mcp::{UbsRedactoDocument, encode};

const FORMS: &[&str] = &["AAOS_033_IT", "AAEV_019_EN", "AABF_019"];

fn golden_dir(form: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(form)
}

fn decode(dump: &[u8]) -> RedactoDocument {
    u2s_mapper_redacto::decode::decode(dump)
        .expect("the dump decodes")
        .into_document()
}

/// The document as JSON with each asset reference replaced by the asset
/// itself (kind and content), and the asset list, whose keys are minted from
/// the dump's ids, dropped.
fn resolved(doc: &RedactoDocument) -> Value {
    let assets: HashMap<String, Value> = doc
        .assets
        .iter()
        .map(|a| {
            let a = serde_json::to_value(a).unwrap();
            (
                a["key"].as_str().unwrap().to_string(),
                json!({ "kind": a["kind"], "content": a["content"] }),
            )
        })
        .collect();
    fn resolve(value: &mut Value, assets: &HashMap<String, Value>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Array(keys)) = map.get_mut("assets") {
                    for key in keys.iter_mut() {
                        *key = assets[key.as_str().unwrap()].clone();
                    }
                }
                for (_, v) in map.iter_mut() {
                    resolve(v, assets);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| resolve(v, assets)),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(doc).unwrap();
    value.as_object_mut().unwrap().remove("assets");
    resolve(&mut value, &assets);
    value
}

/// The first path at which two JSON values differ, with both sides.
fn first_difference(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| {
                first_difference(
                    &format!("{path}/{k}"),
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                )
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (a, b))| first_difference(&format!("{path}/{i}"), a, b)),
        _ if a == b => None,
        _ => Some(format!("{path}:\n  golden:  {a}\n  encoded: {b}")),
    }
}

#[test]
fn encoded_dumps_match_the_golden_dumps() {
    let mut failures = Vec::new();
    for form in FORMS {
        let json = std::fs::read_to_string(golden_dir(form).join("document.json")).unwrap();
        let doc: UbsRedactoDocument =
            serde_json::from_str(&json).expect("document.json deserializes");
        let dump = encode(&doc).unwrap_or_else(|e| panic!("{form} encodes: {e}"));

        let golden = resolved(&decode(
            &std::fs::read(golden_dir(form).join("dump.sql")).unwrap(),
        ));
        let encoded = resolved(&decode(&dump));
        if let Some(difference) = first_difference("", &golden, &encoded) {
            failures.push(format!("{form}: {difference}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A delivered dump loads back as a document and encodes to the same dump:
/// the furniture comes off and each language's source is recovered from it.
#[test]
fn decoded_golden_dumps_re_encode_to_the_same_document() {
    let mut failures = Vec::new();
    for form in FORMS {
        let dump = std::fs::read(golden_dir(form).join("dump.sql")).unwrap();
        let doc =
            u2s_redacto_ubs_mcp::decode(&dump).unwrap_or_else(|e| panic!("{form} decodes: {e}"));
        let re_encoded = encode(&doc).unwrap_or_else(|e| panic!("{form} re-encodes: {e}"));
        if let Some(difference) = first_difference(
            "",
            &resolved(&decode(&dump)),
            &resolved(&decode(&re_encoded)),
        ) {
            failures.push(format!("{form}: {difference}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A dump the recovered document would not encode back to is refused rather
/// than silently rewritten: here AABF_019 released rather than a draft, and
/// AABF_019 with a first-page header of its own. (A dump does not record its
/// master language, so there is none to compare.)
#[test]
fn a_dump_ubs_would_not_write_is_refused() {
    let json = std::fs::read_to_string(golden_dir("AABF_019").join("document.json")).unwrap();
    let doc: UbsRedactoDocument = serde_json::from_str(&json).unwrap();
    let redacto = u2s_redacto_ubs_mcp::to_redacto(&doc)
        .unwrap()
        .into_document();

    let mut released = redacto.clone();
    released.metadata.status = u2s_redacto::model::Status::Released;
    let mut first_page = redacto.clone();
    first_page.first_header = first_page.header.clone();

    for (what, variant) in [("status", released), ("first-page header", first_page)] {
        let dump = u2s_mapper_redacto::encode(&variant.validate().unwrap())
            .unwrap()
            .bytes;
        let Err(err) = u2s_redacto_ubs_mcp::decode(&dump) else {
            panic!("a dump with another {what} must be refused");
        };
        assert!(err.to_string().contains(what), "{what}: {err}");
    }
}
