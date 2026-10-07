//! The reference tools against a scratch database: results, argument
//! validation, the spec/argument agreement, and the MCP handler end to end
//! over an in-memory transport.

use std::sync::Arc;

use references_mcp::reference_db::SCHEMA_SQL;
use references_mcp::specs::tool_specs;
use references_mcp::{Opener, ReferenceStore, ReferencesServer, document_hash};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rusqlite::Connection;
use serde_json::{Value, json};

const PROFILE: &str = "italy";

/// A scratch database with the schema applied. The temp file lives as long as
/// the returned handle.
fn scratch() -> (tempfile::NamedTempFile, ReferenceStore) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_path_buf();
    Connection::open(&path).unwrap().execute_batch(SCHEMA_SQL).unwrap();
    let open: Opener = Arc::new(move || Connection::open(&path).map_err(|e| e.to_string()));
    (file, ReferenceStore::new(open))
}

/// A store holding one reference (with one package file) and one doc, plus the
/// ids of both.
fn seeded() -> (tempfile::NamedTempFile, ReferenceStore, String, String) {
    let (file, store) = scratch();
    let pdfs = vec![("form.pdf".to_string(), b"%PDF-fake".to_vec())];
    let ref_id = references_mcp::compute_ref_id(&pdfs);
    store
        .add_reference(
            PROFILE,
            &ref_id,
            "Account opening",
            "A form for opening a bank account: holder name, nationality and address.\nSecond line.",
            &vec![0.0; 384],
            &[(0, pdfs[0].1.clone())],
            &[(
                "jcr_root/form/.content.xml".to_string(),
                "<root>\n<field name=\"Nationality\"/>\n<field name=\"Street\"/>\n</root>".to_string(),
            )],
        )
        .unwrap();
    let doc_id = references_mcp::compute_doc_id("line one\nline two\nline three");
    store
        .add_doc(PROFILE, &doc_id, "Notes", "line one\nline two\nline three")
        .unwrap();
    (file, store, ref_id, doc_id)
}

fn text(result: &CallToolResult) -> String {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result
        .content
        .iter()
        .map(|c| match c {
            ContentBlock::Text(t) => t.text.clone(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect()
}

fn json_of(result: &CallToolResult) -> Value {
    serde_json::from_str(&text(result)).expect("the reply is JSON")
}

fn error_of(server: &ReferencesServer, tool: &str, args: Value) -> String {
    server.dispatch(tool, &args).expect_err(tool).to_string()
}

#[test]
fn document_hash_is_the_one_both_keys_use() {
    // Pinned in `store.rs` for references; the agent's session keys call the
    // same function.
    assert_eq!(
        document_hash(&[("input.pdf".into(), b"reference-id-test".to_vec())]),
        "c0314ff86abc2dcf5cfd090203e5d24eb456bbff56e290ead6cd302fd16af90a"
    );
}

#[test]
fn list_reference_forms_lists_the_profiles_references() {
    let (_db, store, ref_id, _) = seeded();
    let server = ReferencesServer::new(store.clone(), PROFILE.into());
    let list = json_of(&server.dispatch("list_reference_forms", &json!({})).unwrap());
    assert_eq!(list[0]["ref_id"], ref_id);
    assert_eq!(list[0]["label"], "Account opening");
    assert_eq!(list[0]["pdf_count"], 1);
    assert_eq!(list[0]["files"], json!(["jcr_root/form/.content.xml"]));

    // A call without arguments (`null`) is an empty object.
    assert!(server.dispatch("list_reference_forms", &Value::Null).is_ok());

    // Another profile sees nothing: the profile is the host's, not an argument.
    let other = ReferencesServer::new(store, "germany".into());
    let list = json_of(&other.dispatch("list_reference_forms", &json!({})).unwrap());
    assert_eq!(list, json!([]));
}

#[test]
fn grep_references_finds_description_and_package_text() {
    let (_db, store, ref_id, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let hits = json_of(
        &server
            .dispatch("grep_references", &json!({"query": "nationality"}))
            .unwrap(),
    );
    let wheres: Vec<&str> = hits.as_array().unwrap().iter().map(|h| h["where"].as_str().unwrap()).collect();
    assert_eq!(wheres, ["description", "jcr_root/form/.content.xml"]);
    assert_eq!(hits[0]["ref_id"], ref_id);

    let regex = json_of(
        &server
            .dispatch("grep_references", &json!({"query": "Str(ee|oo)t", "regex": true}))
            .unwrap(),
    );
    assert_eq!(regex.as_array().unwrap().len(), 1);
    assert!(regex[0]["snippet"].as_str().unwrap().contains("Street"));
}

/// Models often send `null` for an optional argument they do not use; that
/// means "not given", not a mistyped value.
#[test]
fn a_null_optional_argument_means_not_given() {
    let (_db, store, ref_id, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let absent = text(
        &server
            .dispatch("grep_references", &json!({"query": "nationality"}))
            .unwrap(),
    );
    let null = text(
        &server
            .dispatch("grep_references", &json!({"query": "nationality", "regex": null}))
            .unwrap(),
    );
    assert_eq!(absent, null);

    let path = "jcr_root/form/.content.xml";
    let absent = text(
        &server
            .dispatch("read_reference_file", &json!({"ref_id": ref_id, "path": path}))
            .unwrap(),
    );
    let null = text(
        &server
            .dispatch(
                "read_reference_file",
                &json!({"ref_id": ref_id, "path": path, "offset": null, "limit": null}),
            )
            .unwrap(),
    );
    assert_eq!(absent, null);
}

#[test]
fn read_reference_file_reads_description_and_files_by_line_window() {
    let (_db, store, ref_id, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let whole = text(
        &server
            .dispatch("read_reference_file", &json!({"ref_id": ref_id, "path": "description"}))
            .unwrap(),
    );
    assert!(whole.ends_with("Second line."));
    let window = text(
        &server
            .dispatch(
                "read_reference_file",
                &json!({"ref_id": ref_id, "path": "jcr_root/form/.content.xml", "offset": 1, "limit": 1}),
            )
            .unwrap(),
    );
    assert_eq!(window, "<field name=\"Nationality\"/>");
    let err = error_of(&server, "read_reference_file", json!({"ref_id": ref_id, "path": "nope"}));
    assert!(err.contains("No such file"), "{err}");
}

#[test]
fn get_reference_package_lists_the_files() {
    let (_db, store, ref_id, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let paths = json_of(&server.dispatch("get_reference_package", &json!({"ref_id": ref_id})).unwrap());
    assert_eq!(paths, json!(["jcr_root/form/.content.xml"]));
    let none = json_of(&server.dispatch("get_reference_package", &json!({"ref_id": "unknown"})).unwrap());
    assert_eq!(none, json!([]));
}

#[test]
fn reference_docs_are_listed_read_and_grepped() {
    let (_db, store, _, doc_id) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let list = json_of(&server.dispatch("list_reference_docs", &json!({})).unwrap());
    assert_eq!(list, json!([{"doc_id": doc_id, "label": "Notes"}]));
    let window = text(
        &server
            .dispatch("read_reference_doc", &json!({"doc_id": doc_id, "offset": 1, "limit": 1}))
            .unwrap(),
    );
    assert_eq!(window, "line two");
    let hits = json_of(&server.dispatch("grep_reference_docs", &json!({"query": "THREE"})).unwrap());
    assert_eq!(hits[0]["doc_id"], doc_id);
    assert!(hits[0]["snippet"].as_str().unwrap().contains("line three"));
    let err = error_of(&server, "read_reference_doc", json!({"doc_id": "unknown"}));
    assert!(err.contains("Unknown doc_id"), "{err}");
}

#[test]
fn search_references_matches_by_meaning_and_literally() {
    let (_db, store) = scratch();
    let description = "A form for opening a bank account: holder name, nationality and address.";
    let pdfs = vec![("form.pdf".to_string(), b"%PDF-fake".to_vec())];
    let ref_id = references_mcp::compute_ref_id(&pdfs);
    store
        .add_reference(
            PROFILE,
            &ref_id,
            "Account opening",
            description,
            &references_mcp::embed_description(description).unwrap(),
            &[],
            &[],
        )
        .unwrap();
    let server = ReferencesServer::new(store, PROFILE.into());
    let hits = json_of(
        &server
            .dispatch(
                "search_references",
                &json!({"query": "opening a bank account for a new customer", "top_k": 2}),
            )
            .unwrap(),
    );
    assert_eq!(hits[0]["ref_id"], ref_id);
    assert_eq!(hits[0]["matched"], "semantic");
    assert!(hits[0]["score"].as_f64().unwrap() > 0.4);

    let literal = json_of(&server.dispatch("search_references", &json!({"query": "nationality"})).unwrap());
    assert!(literal.as_array().unwrap().iter().any(|h| h["matched"] == "description"));
}

#[test]
fn search_references_refuses_a_blank_query() {
    let (_db, store) = scratch();
    let server = ReferencesServer::new(store, PROFILE.into());
    let err = error_of(&server, "search_references", json!({"query": "  "}));
    assert!(err.starts_with("search_references requires a non-empty query"), "{err}");
}

#[test]
fn a_missing_wrong_typed_or_unknown_argument_is_an_error_not_a_default() {
    let (_db, store, _, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());

    let missing = error_of(&server, "read_reference_file", json!({"ref_id": "x"}));
    assert!(missing.contains("invalid arguments for read_reference_file") && missing.contains("path"), "{missing}");
    let missing = error_of(&server, "grep_references", json!({}));
    assert!(missing.contains("query"), "{missing}");

    // The misspelling that used to read as "" is now named.
    let unknown = error_of(&server, "get_reference_package", json!({"ref_id": "x", "refid": "y"}));
    assert!(unknown.contains("unknown field `refid`"), "{unknown}");
    let unknown = error_of(&server, "list_reference_forms", json!({"profile": "germany"}));
    assert!(unknown.contains("unknown field `profile`"), "{unknown}");

    let wrong = error_of(&server, "grep_references", json!({"query": "x", "regex": "yes"}));
    assert!(wrong.contains("regex") || wrong.contains("boolean"), "{wrong}");
    let negative = error_of(&server, "read_reference_doc", json!({"doc_id": "x", "offset": -1}));
    assert!(negative.contains("invalid arguments"), "{negative}");

    assert_eq!(error_of(&server, "nope", json!({})), "unknown tool nope");
}

/// The properties and `required` of every spec are exactly what its argument
/// struct accepts: a sample built from the schema deserializes, an object of
/// only the required fields deserializes, and the fields serde expects are the
/// schema's properties.
#[test]
fn every_spec_agrees_with_its_argument_struct() {
    let (_db, store, _, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let rejected = |result: Result<CallToolResult, references_mcp::Error>| {
        result.err().is_some_and(|e| e.0.starts_with("invalid arguments"))
    };
    let specs = tool_specs();
    assert_eq!(specs.len(), 8);
    for spec in &specs {
        let name = spec["name"].as_str().unwrap();
        let schema = &spec["input_schema"];
        let properties = schema["properties"].as_object().unwrap();
        let required: Vec<&str> = schema["required"].as_array().unwrap().iter().map(|r| r.as_str().unwrap()).collect();
        let sample = |only_required: bool| -> Value {
            properties
                .iter()
                .filter(|(key, _)| !only_required || required.contains(&key.as_str()))
                .map(|(key, prop)| {
                    let value = match prop["type"].as_str().unwrap() {
                        "string" => json!("x"),
                        "integer" => json!(1),
                        "boolean" => json!(true),
                        other => panic!("{name}.{key}: unhandled type {other}"),
                    };
                    (key.clone(), value)
                })
                .collect::<serde_json::Map<_, _>>()
                .into()
        };
        assert!(!rejected(server.dispatch(name, &sample(false))), "{name}: all properties");
        assert!(!rejected(server.dispatch(name, &sample(true))), "{name}: required only");

        // Every required field is needed: dropping any one is refused.
        for dropped in &required {
            let mut args = sample(true);
            args.as_object_mut().unwrap().remove(*dropped);
            assert!(rejected(server.dispatch(name, &args)), "{name} accepted a call without {dropped}");
        }

        // serde names the fields it accepts when it meets an unknown one.
        let mut args = sample(true);
        args["zz_unknown"] = json!(1);
        let message = server.dispatch(name, &args).unwrap_err().0;
        let mut expected: Vec<&str> = message
            .split("expected ")
            .nth(1)
            .map(|rest| rest.split('`').skip(1).step_by(2).collect())
            .unwrap_or_default();
        expected.sort();
        let mut listed: Vec<&str> = properties.keys().map(String::as_str).collect();
        listed.sort();
        assert_eq!(expected, listed, "{name}: struct fields and schema properties differ ({message})");
    }
}

#[tokio::test]
async fn the_mcp_handler_lists_and_calls_tools() {
    let (_db, store, ref_id, _) = seeded();
    let server = ReferencesServer::new(store, PROFILE.into());
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let serving = tokio::spawn(async move { server.serve(server_io).await.unwrap().waiting().await });
    let client = ().serve(client_io).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    let specs = tool_specs();
    assert_eq!(names, specs.iter().map(|s| s["name"].as_str().unwrap()).collect::<Vec<_>>());
    assert_eq!(tools[1].description.as_deref(), specs[1]["description"].as_str());

    let args = json!({"ref_id": ref_id, "path": "description", "limit": 1});
    let reply = client
        .call_tool(CallToolRequestParams::new("read_reference_file").with_arguments(args.as_object().unwrap().clone()))
        .await
        .unwrap();
    assert!(text(&reply).starts_with("A form for opening a bank account"));

    // A bad call is a tool error the model can read, not a protocol failure.
    let bad = client
        .call_tool(CallToolRequestParams::new("get_reference_package"))
        .await
        .unwrap();
    assert_eq!(bad.is_error, Some(true));
    assert!(format!("{bad:?}").contains("invalid arguments for get_reference_package"));

    client.cancel().await.unwrap();
    let _ = serving.await;
}
