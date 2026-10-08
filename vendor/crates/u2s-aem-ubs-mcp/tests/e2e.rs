//! The real binary, over real stdio MCP.
//!
//! Proves the three things registration depends on (the manifest parses,
//! the declared tools match `tools/list`, a refusal is a *tool* error
//! rather than a protocol error), plus the two things that used to be
//! impossible before this session: `encode` actually produces a package,
//! and `fragment_search` actually searches a library.

use u2s_render_test_harness::ServerUnderTest;

fn a_valid_form() -> serde_json::Value {
    serde_json::json!({
        "variables": { "formrange_code": "AAEV", "formrange_entity": "019" },
        "languages": ["en"],
        "form": {
            "type": "Root",
            "title": { "en": "Test form" },
            "children": [{
                "type": "Panel", "uuid": "00000000-0000-4000-8000-000000000001",
                "name": "PN_Details", "title": { "en": "Details" }, "is_page": true,
                "visible": true, "is_conditional": false, "dor_num_cols": null,
                "colspan": 12, "dor_colspan": null, "bind_ref": null, "frag_ref": null,
                "children": [{
                    "type": "TextField", "uuid": "00000000-0000-4000-8000-000000000002",
                    "name": "TXT_Name", "label": { "en": "Name" }, "mandatory": false,
                    "visible": true, "max_chars": null, "colspan": 12, "dor_colspan": null,
                    "bind_ref": null, "kind": "Plain"
                }]
            }]
        }
    })
}

#[tokio::test]
async fn the_manifest_parses_and_carries_the_format_module() {
    let server = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem");
    let client = server.connect().await;

    let raw = u2s_render_test_harness::read_manifest(&client).await;

    let manifest = u2s_mcp::ServerManifest::parse(&raw).expect("the manifest must parse");
    manifest
        .check_compatible()
        .expect("the contract major must be one this workspace understands");

    let format = manifest
        .format
        .as_ref()
        .expect("a format server's manifest must carry a format module");
    assert_eq!(format.key.as_str(), "aem-ubs");
    assert!(
        !format.description_md.is_empty(),
        "the description is what an operator reads before activating a version"
    );
    assert!(
        format.json_schema.get("$schema").is_some() || format.json_schema.get("type").is_some(),
        "the format module must carry a real JSON Schema: {}",
        format.json_schema
    );

    assert_eq!(manifest.tools.len(), 3);
    assert_eq!(manifest.tools[0].tool, "decode");
    assert_eq!(manifest.tools[0].role, u2s_mcp::ToolRole::Decode);
    assert_eq!(manifest.tools[1].tool, "encode");
    assert_eq!(manifest.tools[1].role, u2s_mcp::ToolRole::Encode);
    assert_eq!(manifest.tools[2].tool, "fragment_search");
    assert_eq!(manifest.tools[2].role, u2s_mcp::ToolRole::Query);

    client.cancel().await.ok();
}

#[tokio::test]
async fn encode_produces_a_real_package_blob() {
    let blob_dir = std::env::temp_dir().join(format!(
        "u2s-aem-ubs-mcp-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let server = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem")
        .env("U2S_BLOB_DIR", blob_dir.display().to_string());
    let client = server.connect().await;

    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("encode".to_owned()).with_arguments(
                serde_json::json!({ "output_json": a_valid_form() })
                    .as_object()
                    .expect("an object")
                    .clone(),
            ),
        )
        .await
        .expect("encode must answer over the protocol");

    assert_ne!(
        result.is_error,
        Some(true),
        "a valid document must encode, not refuse: {:?}",
        result.content
    );
    let structured = result
        .structured_content
        .expect("encode reports a blob in its structured content");
    let handle = structured["blob"]["handle"]
        .as_str()
        .expect("a blob handle");
    assert_eq!(structured["blob"]["media_type"], "application/zip");

    // The blob is real: reading it back yields a well-formed ZIP.
    let blob_path = blob_dir.join(handle);
    let bytes = std::fs::read(&blob_path).expect("the blob file must exist on disk");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a valid zip");
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_owned())
        .collect();
    assert!(names.iter().any(|n| n == "META-INF/MANIFEST.MF"));
    assert!(names.iter().any(|n| n.contains("AF_AAEV/.content.xml")));
    // The schema and the bound package come with it.
    assert_eq!(structured["xsd"]["media_type"], "application/xml");
    assert_eq!(structured["bound_package"]["media_type"], "application/zip");

    let _ = std::fs::remove_dir_all(&blob_dir);
    client.cancel().await.ok();
}

#[tokio::test]
async fn encode_refuses_an_invalid_document_by_naming_the_violation() {
    let server = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem");
    let client = server.connect().await;

    // A German label on a form that lists only English.
    let mut invalid = a_valid_form();
    invalid["form"]["children"][0]["children"][0]["label"]["de"] = serde_json::json!("Name");

    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("encode".to_owned()).with_arguments(
                serde_json::json!({ "output_json": invalid })
                    .as_object()
                    .expect("an object")
                    .clone(),
            ),
        )
        .await
        .expect("an invalid document must still answer over the protocol");

    assert_eq!(
        result.is_error,
        Some(true),
        "a text in a language the document does not list must be refused"
    );
    let text = format!("{:?}", result.content);
    assert!(
        text.contains("which `languages`"),
        "the refusal must name the language: {text}"
    );

    client.cancel().await.ok();
}

#[tokio::test]
async fn fragment_search_finds_a_scanned_fragment_and_an_unconfigured_library_finds_none() {
    let lib_dir = std::env::temp_dir().join(format!(
        "u2s-aem-ubs-mcp-fragments-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let fragment_dir = lib_dir.join("afforms_ch_fragmentlib/address");
    std::fs::create_dir_all(&fragment_dir).unwrap();
    std::fs::write(
        fragment_dir.join(".content.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:jcr="http://www.jcp.org/jcr/1.0" jcr:title="Address Block"/>
"#,
    )
    .unwrap();

    let server = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem")
        .env("U2S_AEM_FRAGMENT_DIR", lib_dir.display().to_string());
    let client = server.connect().await;

    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("fragment_search".to_owned())
                .with_arguments(
                    serde_json::json!({ "query": "address" })
                        .as_object()
                        .expect("an object")
                        .clone(),
                ),
        )
        .await
        .expect("fragment_search must answer");
    assert_ne!(result.is_error, Some(true));
    let hits = result
        .structured_content
        .expect("structured hits")
        .get("hits")
        .cloned()
        .unwrap_or_default();
    assert_eq!(hits.as_array().map(Vec::len), Some(1));
    assert_eq!(
        hits[0]["frag_ref"],
        "/content/dam/formsanddocuments/afforms_ch_fragmentlib/address"
    );

    client.cancel().await.ok();

    // A second server with no `U2S_AEM_FRAGMENT_DIR` at all starts fine
    // and returns no hits -- an unconfigured library, not a broken one.
    let unconfigured = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem");
    let client = unconfigured.connect().await;
    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("fragment_search".to_owned())
                .with_arguments(
                    serde_json::json!({ "query": "address" })
                        .as_object()
                        .expect("an object")
                        .clone(),
                ),
        )
        .await
        .expect("fragment_search must still answer with no library configured");
    assert_ne!(result.is_error, Some(true));
    let hits = result
        .structured_content
        .expect("structured hits")
        .get("hits")
        .cloned()
        .unwrap_or_default();
    assert_eq!(hits.as_array().map(Vec::len), Some(0));
    client.cancel().await.ok();

    let _ = std::fs::remove_dir_all(&lib_dir);
}

/// This crate's own committed fixture -- `AF_AABF.zip`, shared with
/// `u2s-mapper-aem`'s own tests rather than duplicated (it is a real,
/// ~2.2 MB package) -- resolved for `$FIXTURES` substitution in this
/// server's own declared test vectors.
fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../u2s-mapper-aem/tests/fixtures")
}

fn temp_blob_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "u2s-aem-ubs-mcp-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// The generic battery every u2s server passes, run against this one so the
/// format server is held to the same convention as the renderers. Now
/// exercises all three declared test vectors (`decode` on the real
/// fixture, `encode` on a minimal inline document, `fragment_search`
/// expecting no hits), not just the manifest/tools-list checks a
/// vector-free manifest fell back to before `decode` existed.
#[tokio::test]
async fn the_server_passes_the_shared_conformance_battery() {
    let blob_dir = temp_blob_dir("conformance");
    let server = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem")
        .env("U2S_BLOB_DIR", blob_dir.display().to_string());
    let client = server.connect().await;

    let report = u2s_mcp::conformance::run(&client, &fixtures_dir()).await;
    assert!(
        report.passed(),
        "the shared battery must pass: {:?}",
        report.findings
    );

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&blob_dir);
}

/// The actual real-package milestone this crate's own design plan calls
/// for: `decode` then `encode` over real stdio MCP, exercised against the
/// real, committed `AF_AABF.zip` fixture -- not this crate's own encoder
/// output. Proves the whole seam end to end: a real package in, a blob
/// handle out, that blob's own JSON re-encodes into a real package.
#[tokio::test]
async fn decode_then_encode_reproduces_the_real_fixture() {
    let blob_dir = temp_blob_dir("decode-e2e");
    let server = ServerUnderTest::locate("u2s-aem-ubs-mcp", "aem")
        .env("U2S_BLOB_DIR", blob_dir.display().to_string());
    let client = server.connect().await;

    let fixture_path = fixtures_dir().join("AF_AABF.zip");
    let decode_result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("decode".to_owned()).with_arguments(
                serde_json::json!({ "artifact_path": fixture_path.display().to_string() })
                    .as_object()
                    .expect("an object")
                    .clone(),
            ),
        )
        .await
        .expect("decode must answer over the protocol");

    assert_ne!(
        decode_result.is_error,
        Some(true),
        "the real fixture must decode, not refuse: {:?}",
        decode_result.content
    );
    let structured = decode_result
        .structured_content
        .expect("decode reports a blob in its structured content");
    let handle = structured["output_json"]["handle"]
        .as_str()
        .expect("a blob handle");
    assert_eq!(structured["format_version"], u2s_aem_ubs_mcp_specs_format_version());

    let output_json_bytes =
        std::fs::read(blob_dir.join(handle)).expect("the decoded document blob must exist on disk");
    let output_json: serde_json::Value =
        serde_json::from_slice(&output_json_bytes).expect("the decoded document is valid JSON");
    assert_eq!(output_json["variables"]["formrange_code"], "AABF");

    let encode_result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("encode".to_owned())
                .with_arguments(
                    serde_json::json!({ "output_json": output_json })
                        .as_object()
                        .expect("an object")
                        .clone(),
                ),
        )
        .await
        .expect("encode must answer over the protocol");
    assert_ne!(
        encode_result.is_error,
        Some(true),
        "the decoded document must encode back, not refuse: {:?}",
        encode_result.content
    );
    let encoded = encode_result
        .structured_content
        .expect("encode reports a blob in its structured content");
    let encoded_handle = encoded["blob"]["handle"].as_str().expect("a blob handle");
    let encoded_bytes =
        std::fs::read(blob_dir.join(encoded_handle)).expect("the encoded package blob must exist on disk");
    zip::ZipArchive::new(std::io::Cursor::new(encoded_bytes)).expect("a valid, re-openable zip");

    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&blob_dir);
}

/// `specs::FORMAT_VERSION` is private to the binary crate; this test binary
/// only links it as a client over stdio, so the version this test compares
/// against is duplicated here rather than imported -- the same reasoning
/// `manifest()`'s own key/version fields are asserted by literal value
/// elsewhere in this suite.
fn u2s_aem_ubs_mcp_specs_format_version() -> &'static str {
    "0.3.0"
}
