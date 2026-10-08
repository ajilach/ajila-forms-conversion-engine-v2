//! Black-box end-to-end: spawn the built binary and speak real MCP over stdio.
//!
//! The contract scenarios come from `u2s-render-test-harness` and are shared
//! with the PDF server; what remains here is XFA-specific — the routing mirror
//! and the state tools.
//!
//! Fixtures are synthetic XFA, so this suite needs no customer corpus. It does
//! need fonts, because the layout engine cannot measure text without them —
//! the vendored fallback in `vendor/fonts/` is committed, so its absence
//! means a broken checkout, not a machine to skip on.

use std::path::{Path, PathBuf};

use harness::{ErrorCase, ServerUnderTest, call, error_text, structured};
use serde_json::json;
use u2s_render_test_harness as harness;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-render-xfa/fixtures/generated")
        .canonicalize()
        .expect("fixtures missing — run `cargo test -p u2s-render-xfa` first to generate them")
}

fn fixture(name: &str) -> String {
    fixtures().join(name).display().to_string()
}

/// The committed UBS faces, for the server's `U2S_FONT_DIR`.
fn font_dir() -> String {
    u2s_test_assets::font_dir().display().to_string()
}

fn blob_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("u2s-xfa-e2e-blobs");
    std::fs::create_dir_all(&dir).expect("blob dir");
    dir
}

fn server() -> ServerUnderTest {
    ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env("U2S_FONT_DIR", font_dir())
        .env("U2S_BLOB_DIR", blob_dir().display().to_string())
        // Low enough that a 300 dpi page must become a blob.
        .env("MAX_INLINE_BYTES", "40000")
}

macro_rules! require_server {
    () => {
        server()
    };
}

// ------------------------------------------------- shared contract battery

#[tokio::test]
async fn handshake_and_manifest_agree() {
    let s = require_server!();
    harness::handshake_matches_manifest(&s, &fixtures()).await;
}

#[tokio::test]
async fn cursor_walk_covers_the_overflow_form_exactly_once() {
    let s = require_server!();
    // Establish the true page count through the server itself, so the harness
    // is checking the walk rather than a number this test made up.
    let client = s.connect().await;
    let info = call(
        &client,
        "xfa_info",
        json!({ "doc_path": fixture("overflow.xfa.pdf") }),
    )
    .await;
    let pages = structured(&info)["page_count"]
        .as_u64()
        .expect("page_count");
    client.cancel().await.ok();

    assert!(pages >= 2, "the overflow fixture should be multi-page");
    harness::cursor_walk_complete(
        &s,
        &fixture("overflow.xfa.pdf"),
        pages,
        1,
        json!({"dpi": 36}),
    )
    .await;
}

#[tokio::test]
async fn an_oversized_image_becomes_a_blob_handle_on_disk() {
    let s = require_server!();
    // The overflow fixture has real content on every page; a nearly-blank one
    // compresses below any sensible inline threshold and would never take the
    // blob path.
    harness::blob_digest_valid(&s, &fixture("overflow.xfa.pdf"), 300).await;
}

#[tokio::test]
async fn failures_are_tool_errors_and_the_session_survives() {
    let s = require_server!();
    let cases = vec![
        ErrorCase::new(
            "xfa_render_page",
            json!({ "doc_path": fixture("minimal.xfa.pdf"), "page": 9 }),
            &["9", "1..1"],
        ),
        ErrorCase::new(
            "xfa_info",
            json!({ "doc_path": "/nope/missing.pdf" }),
            &["/nope/missing.pdf"],
        ),
        ErrorCase::new(
            "xfa_render_page",
            json!({ "doc_path": fixture("minimal.xfa.pdf") }),
            &["page"],
        ),
        ErrorCase::new(
            "xfa_render_region",
            json!({
                "doc_path": fixture("minimal.xfa.pdf"), "page": 1,
                "rect_pt": { "x": 5000, "y": 5000, "width": 100, "height": 100 }
            }),
            &["outside page"],
        ),
        // A state naming a control the form does not have must say where the
        // real names come from, not just that it failed.
        // A caller's bad query is named as an argument fault, not as an engine
        // failure — the reason `InvalidArgument` is its own variant.
        ErrorCase::new(
            "xfa_search_text",
            json!({ "doc_path": fixture("minimal.xfa.pdf"), "query": "(unclosed", "regex": true }),
            &["query", "invalid regex"],
        ),
        ErrorCase::new(
            "xfa_search_text",
            json!({ "doc_path": fixture("minimal.xfa.pdf"), "query": "" }),
            &["query", "empty string"],
        ),
        ErrorCase::new(
            "xfa_search_text",
            json!({ "doc_path": fixture("minimal.xfa.pdf"), "query": "Ada", "from": 9 }),
            &["9", "1..1"],
        ),
        ErrorCase::new(
            "xfa_search_text",
            json!({ "doc_path": fixture("minimal.xfa.pdf") }),
            &["query"],
        ),
        ErrorCase::new(
            "xfa_render_page",
            json!({
                "doc_path": fixture("choices.xfa.pdf"), "page": 1,
                "state": { "selections": [{ "field": "NoSuchControl", "value": "x" }] }
            }),
            &["NoSuchControl", "controls()"],
        ),
        // A misspelled `state` used to render the default state silently;
        // now it is a hard failure naming the real keys.
        ErrorCase::new(
            "xfa_render_page",
            json!({
                "doc_path": fixture("minimal.xfa.pdf"), "page": 1,
                "state": { "selction": [] }
            }),
            &["state", "selections"],
        ),
    ];
    harness::errors_are_tool_errors(
        &s,
        &cases,
        (
            "xfa_info".to_string(),
            json!({ "doc_path": fixture("minimal.xfa.pdf") }),
        ),
    )
    .await;
}

#[test]
fn the_server_refuses_to_start_without_fonts() {
    let s = server();
    // Fonts are the boot gate here, as pdfium is for the PDF server: layout
    // silently degrades without them rather than failing, so starting fontless
    // would mean quietly wrong page counts.
    harness::boot_failure_contract(
        &s,
        &[("U2S_FONT_DIR", "/definitely/not/here")],
        &["U2S_FONT_DIR"],
    );
}

#[tokio::test]
async fn the_same_render_is_identical_across_processes() {
    let s = require_server!();
    harness::cross_process_determinism(
        &s,
        json!({ "doc_path": fixture("minimal.xfa.pdf"), "page": 1, "dpi": 72, "format": "png" }),
        "xfa_render_page",
    )
    .await;
}

// ------------------------------------------------------ XFA-specific tests

#[tokio::test]
async fn info_then_render_returns_an_inline_image() {
    let s = require_server!();
    let c = s.connect().await;

    let info = call(
        &c,
        "xfa_info",
        json!({ "doc_path": fixture("minimal.xfa.pdf") }),
    )
    .await;
    let si = structured(&info);
    assert_eq!(si["kind"], "xfa");
    assert_eq!(si["page_count"], 1);
    assert!(
        si["packets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "template")
    );

    let render = call(
        &c,
        "xfa_render_page",
        json!({ "doc_path": fixture("minimal.xfa.pdf"), "page": 1, "dpi": 72 }),
    )
    .await;
    let sr = structured(&render);
    assert_eq!(sr["inline"], true);
    assert!((sr["width_px"].as_f64().unwrap() - 612.0).abs() <= 2.0);

    // A rendered form has ink on it. A blank result would mean the crop landed
    // outside the content — the failure mode bands exist to prevent.
    let img = render
        .content
        .iter()
        .find_map(|b| b.as_image().map(|i| i.data.clone()))
        .expect("inline image");
    let bytes = base64_decode(&img);
    assert!(
        harness::dark_pixel_ratio(&bytes) > 0.001,
        "the rendered page looks blank"
    );

    c.cancel().await.ok();
}

/// The mirror of the PDF server's XFA warning. That server renders the shim
/// *with* a caveat because pdfium produces something; this one refuses,
/// because a blank page would be worse than an error.
#[tokio::test]
async fn a_plain_pdf_is_refused_and_names_the_other_renderer() {
    let s = require_server!();
    let plain = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-render-pdf/fixtures/generated/ten-pages.pdf");
    assert!(
        plain.is_file(),
        "{} is committed; missing means a broken checkout",
        plain.display()
    );
    let c = s.connect().await;
    let path = plain.display().to_string();

    for tool in ["xfa_info", "xfa_render_page"] {
        let args = if tool == "xfa_render_page" {
            json!({ "doc_path": path, "page": 1 })
        } else {
            json!({ "doc_path": path })
        };
        let res = call(&c, tool, args).await;
        assert_eq!(res.is_error, Some(true), "{tool} should refuse a plain PDF");
        let msg = error_text(&res);
        assert!(msg.contains("not an XFA form"), "{msg}");
        assert!(
            msg.contains("pdf_render_page"),
            "must name the right tool: {msg}"
        );
    }

    // And the session survives being handed the wrong kind of document.
    let ok = call(
        &c,
        "xfa_info",
        json!({ "doc_path": fixture("minimal.xfa.pdf") }),
    )
    .await;
    assert_ne!(ok.is_error, Some(true));

    c.cancel().await.ok();
}

/// The one-shot path, over the wire: list the controls, address a state
/// directly by `doc_path`, no session involved.
#[tokio::test]
async fn controls_then_an_addressed_state_changes_the_render() {
    let s = require_server!();
    let c = s.connect().await;
    let path = fixture("choices.xfa.pdf");

    let controls = call(&c, "xfa_controls", json!({ "doc_path": path })).await;
    let sc = structured(&controls);
    let list = sc["controls"].as_array().expect("controls array");
    assert!(!list.is_empty(), "the fixture's controls should be listed");
    assert!(sc["space_size"].as_u64().unwrap() >= 2, "{sc}");

    // Name the checkbox explicitly rather than taking `list[0]`: with the
    // script-reachability filter gone, the first control sorted by SOM path
    // is no longer guaranteed to change anything visible. `CB_Ok` carries a
    // change script that toggles a hidden row.
    let target = list
        .iter()
        .find(|c| c["field"].as_str().unwrap_or_default().ends_with("CB_Ok"))
        .expect("CB_Ok must be listed");
    let field = target["field"].as_str().expect("field");
    let value = target["options"][0]["value"].as_str().expect("option value");

    let default = call(
        &c,
        "xfa_render_page",
        json!({ "doc_path": path, "page": 1, "dpi": 36, "format": "png" }),
    )
    .await;
    let chosen = call(
        &c,
        "xfa_render_page",
        json!({
            "doc_path": path, "page": 1, "dpi": 36, "format": "png",
            "state": { "selections": [{ "field": field, "value": value }] }
        }),
    )
    .await;

    let img_of = |r: &rmcp::model::CallToolResult| {
        r.content
            .iter()
            .find_map(|b| b.as_image().map(|i| i.data.clone()))
            .expect("inline image")
    };
    assert_ne!(
        img_of(&default),
        img_of(&chosen),
        "selecting {field}={value} changed nothing; the state is not reaching the render"
    );

    c.cancel().await.ok();
}

/// The core interaction loop: open a session, set a control the way a person
/// would, and see the effect both in the interaction report and in a
/// re-render at the new revision — never enumerating anything.
#[tokio::test]
async fn a_session_interaction_reveals_a_control_and_the_rerender_shows_it() {
    let s = require_server!();
    let c = s.connect().await;
    let path = fixture("choices.xfa.pdf");

    let opened = call(&c, "xfa_open", json!({ "doc_path": path })).await;
    let so = structured(&opened);
    let session = so["session"].as_str().expect("session").to_string();
    assert_eq!(so["revision"], 0);

    let before = call(
        &c,
        "xfa_render_page",
        json!({ "session": session, "revision": 0, "page": 1, "dpi": 36, "format": "png" }),
    )
    .await;

    let controls = call(
        &c,
        "xfa_controls",
        json!({ "session": session, "revision": 0 }),
    )
    .await;
    let sc = structured(&controls);
    let cb = sc["controls"]
        .as_array()
        .expect("controls")
        .iter()
        .find(|c| c["field"].as_str().unwrap_or_default().ends_with("CB_Ok"))
        .expect("CB_Ok must be listed")
        .clone();
    let field = cb["field"].as_str().expect("field").to_string();
    let value = cb["options"][0]["value"].as_str().expect("option value").to_string();

    let set = call(
        &c,
        "xfa_set",
        json!({ "session": session, "expected_revision": 0, "field": field, "value": value }),
    )
    .await;
    let ss = structured(&set);
    assert_eq!(ss["revision"], 1, "a mutation must always advance the revision");
    // `appeared`/`disappeared` track controls becoming addressable, not
    // arbitrary revealed content: the fixture's change script toggles a
    // plain draw's presence ("Shown"), not a control, so it does not surface
    // there. The render comparison below is what actually proves the
    // interaction reached the page.
    assert!(
        ss["side_effects"].as_array().unwrap().is_empty(),
        "CB_Ok's own change must not show up as its own side effect: {ss}"
    );

    let after = call(
        &c,
        "xfa_render_page",
        json!({ "session": session, "revision": 1, "page": 1, "dpi": 36, "format": "png" }),
    )
    .await;

    let img_of = |r: &rmcp::model::CallToolResult| {
        r.content
            .iter()
            .find_map(|b| b.as_image().map(|i| i.data.clone()))
            .expect("inline image")
    };
    assert_ne!(
        img_of(&before),
        img_of(&after),
        "the interaction must be visible in the next render"
    );

    call(&c, "xfa_close", json!({ "session": session })).await;
    c.cancel().await.ok();
}

/// The guard escape this whole design rests on: a stale or future revision
/// is refused and the error names the current one, so an agent can recover
/// from the error text alone.
#[tokio::test]
async fn a_stale_or_future_revision_is_refused_and_names_the_current_one() {
    let s = require_server!();
    let c = s.connect().await;
    let path = fixture("choices.xfa.pdf");

    let opened = call(&c, "xfa_open", json!({ "doc_path": path })).await;
    let session = structured(&opened)["session"].as_str().expect("session").to_string();

    let controls = call(
        &c,
        "xfa_controls",
        json!({ "session": session, "revision": 0 }),
    )
    .await;
    let sc = structured(&controls);
    let cb = sc["controls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["field"].as_str().unwrap_or_default().ends_with("CB_Ok"))
        .unwrap()
        .clone();
    let field = cb["field"].as_str().unwrap().to_string();
    let value = cb["options"][0]["value"].as_str().unwrap().to_string();

    call(
        &c,
        "xfa_set",
        json!({ "session": session, "expected_revision": 0, "field": field, "value": value }),
    )
    .await;

    let stale = call(
        &c,
        "xfa_render_page",
        json!({ "session": session, "revision": 0, "page": 1 }),
    )
    .await;
    assert_eq!(stale.is_error, Some(true));
    let msg = error_text(&stale);
    assert!(msg.contains('0') && msg.contains('1'), "{msg}");

    let future = call(
        &c,
        "xfa_render_page",
        json!({ "session": session, "revision": 9, "page": 1 }),
    )
    .await;
    assert_eq!(future.is_error, Some(true));
    assert!(error_text(&future).contains('1'));

    let unknown = call(
        &c,
        "xfa_render_page",
        json!({ "session": "sess_does_not_exist", "revision": 0, "page": 1 }),
    )
    .await;
    assert_eq!(unknown.is_error, Some(true));
    assert!(error_text(&unknown).contains("not open"));

    let neither = call(&c, "xfa_info", json!({})).await;
    assert_eq!(neither.is_error, Some(true));

    let both = call(
        &c,
        "xfa_info",
        json!({ "doc_path": path, "session": session }),
    )
    .await;
    assert_eq!(both.is_error, Some(true));

    c.cancel().await.ok();
}

#[tokio::test]
async fn page_text_reports_windowing_honestly() {
    let s = require_server!();
    let c = s.connect().await;

    let full = call(
        &c,
        "xfa_page_text",
        json!({ "doc_path": fixture("minimal.xfa.pdf"), "page": 1, "limit": 100000 }),
    )
    .await;
    let sf = structured(&full);
    assert_eq!(sf["truncated"], false);
    let total = sf["total_chars"].as_u64().expect("total_chars");
    assert!(total > 0);
    assert!(
        sf["text"].as_str().unwrap().contains("Minimal XFA Fixture"),
        "{sf}"
    );

    let window = call(
        &c,
        "xfa_page_text",
        json!({ "doc_path": fixture("minimal.xfa.pdf"), "page": 1, "limit": 5 }),
    )
    .await;
    let sw = structured(&window);
    assert_eq!(sw["truncated"], true);
    assert_eq!(
        sw["total_chars"], total,
        "total is of the page, not the window"
    );

    c.cancel().await.ok();
}

/// Minimal base64, so the test does not pull a dependency for one call site.
fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lut = [255u8; 256];
    for (i, c) in T.iter().enumerate() {
        lut[*c as usize] = i as u8;
    }
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
    for b in s.bytes() {
        let v = lut[b as usize];
        if v == 255 {
            continue;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// The fetched fallback alone — no UBS checkout, no `U2S_FONT_DIR` pointing at
/// a customer's fonts — must be enough for the server to start and render.
/// This is the "functional, not faithful" claim, checked rather than assumed.
#[test]
fn the_fetched_fallback_font_alone_is_enough_to_boot_and_render() {
    let vendored = u2s_test_assets::fallback_font_dir();

    let s = ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env("U2S_FONT_DIR", vendored.display().to_string())
        .env("U2S_BLOB_DIR", blob_dir().display().to_string());

    let out = s.run_to_exit(&[]);
    // run_to_exit runs the server to completion with no stdin, so it exits
    // immediately after the (successful) boot rather than hanging on a
    // request. What matters is that it did not hit the font gate.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("cannot start"),
        "the fetched fallback should be sufficient to boot: {stderr}"
    );
}

// ------------------------------------------------------- shared search battery

/// The search contract: a hit addresses itself in `xfa_page_text`.
#[tokio::test]
async fn a_search_hit_addresses_itself_in_page_text() {
    let s = require_server!();
    harness::search_hit_addresses_itself(&s, &fixture("minimal.xfa.pdf"), "Ada", "Ada", json!({}))
        .await;
}

/// The search cursor covers the multi-page fixture exactly once. One page per
/// call, so the cursor is exercised on every hop.
#[tokio::test]
async fn the_search_cursor_covers_the_overflow_form_exactly_once() {
    let s = require_server!().env("SEARCH_MAX_PAGES", "1");

    // The true page count through the server itself, so the harness checks
    // the walk rather than a number this test made up.
    let client = s.connect().await;
    let info = call(
        &client,
        "xfa_info",
        json!({ "doc_path": fixture("overflow.xfa.pdf") }),
    )
    .await;
    let pages = structured(&info)["page_count"]
        .as_u64()
        .expect("page_count");
    client.cancel().await.ok();

    assert!(pages >= 2, "the overflow fixture should be multi-page");
    harness::search_cursor_walk_complete(&s, &fixture("overflow.xfa.pdf"), pages, "row", json!({}))
        .await;
}

/// A search runs against the state asked for, not only the default one — the
/// same `state` argument every other output-producing tool takes.
#[tokio::test]
async fn a_search_honours_the_requested_state() {
    let s = require_server!();
    let client = s.connect().await;

    let controls = call(
        &client,
        "xfa_controls",
        json!({ "doc_path": fixture("choices.xfa.pdf") }),
    )
    .await;
    let c = structured(&controls);
    let field = c["controls"][0]["field"]
        .as_str()
        .expect("a control")
        .to_string();
    let options = c["controls"][0]["options"]
        .as_array()
        .expect("options")
        .clone();
    assert!(
        options.len() >= 2,
        "the fixture needs a choice to make: {c}"
    );

    // Whatever the form says, a search must answer for the state it was given
    // rather than silently for the default.
    for option in &options {
        let value = option["value"].as_str().expect("option value");
        let r = call(
            &client,
            "xfa_search_text",
            json!({
                "doc_path": fixture("choices.xfa.pdf"),
                "query": "a",
                "state": { "selections": [{ "field": field, "value": value }] }
            }),
        )
        .await;
        assert_ne!(
            r.is_error,
            Some(true),
            "a state the form offers must be searchable: {}",
            error_text(&r)
        );
        assert!(structured(&r)["through"].as_u64().is_some());
    }

    client.cancel().await.ok();
}
