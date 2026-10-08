//! The render fidelity split, as a test rather than a claim.
//!
//! pdfium cannot render an XFA form: it draws a static "please update your
//! reader" shim, which *looks* like a successful render. This is the test that
//! shows the difference on the same document — the reason two renderers exist
//! rather than one, checked rather than asserted in prose.
//!
//! Needs a real XFA form, since the synthetic fixtures have no shim to
//! compare against (they carry no AcroForm fallback content) — uses the
//! vendored UBS corpus. Font-gated on the XFA side for the same reason every
//! other layout test is; both the corpus and the fallback fonts are
//! committed, so absence means a broken checkout, not a machine to skip on.

use std::path::PathBuf;

use serde_json::json;
use u2s_render_test_harness::{ServerUnderTest, call, dark_pixel_ratio, structured};

/// The committed UBS faces, for the server's `U2S_FONT_DIR`.
fn xfa_font_dir() -> String {
    u2s_test_assets::font_dir().display().to_string()
}

fn blob_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("u2s-cross-server-{name}"));
    std::fs::create_dir_all(&dir).expect("blob dir");
    dir
}

/// Render page 1 of a real XFA form through both servers at the same low
/// resolution and compare. pdfium must produce the shim — sparse, largely
/// blank, generic boilerplate text — while this engine must produce the real
/// form: substantially more ink, and the document's own content readable.
#[tokio::test]
async fn pdfium_renders_the_shim_while_this_engine_renders_the_real_form() {
    let form = u2s_test_assets::corpus_form("AAAA_019_DE.pdf");
    let fonts = xfa_font_dir();
    let path = form.display().to_string();

    // pdfium's side.
    let pdf_server = ServerUnderTest::locate("u2s-render-pdf-mcp", "pdf")
        .env("U2S_BLOB_DIR", blob_dir("pdf").display().to_string());
    let pdf_client = pdf_server.connect().await;

    let pdf_info = call(&pdf_client, "pdf_info", json!({ "doc_path": path })).await;
    let form_type = structured(&pdf_info)["form_type"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        form_type == "xfa_full" || form_type == "xfa_foreground",
        "the corpus fixture must actually be an XFA form, got {form_type}"
    );

    let pdf_render = call(
        &pdf_client,
        "pdf_render_page",
        json!({ "doc_path": path, "page": 1, "dpi": 72, "format": "png" }),
    )
    .await;
    assert!(
        structured(&pdf_render)["warning"].is_string(),
        "pdfium must flag this as likely a shim"
    );
    let shim_bytes = pdf_render
        .content
        .iter()
        .find_map(|b| b.as_image().map(|i| i.data.clone()))
        .map(|b64| base64_decode(&b64))
        .expect("inline shim image");
    pdf_client.cancel().await.ok();

    // Our engine's side.
    let xfa_server = ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env("U2S_FONT_DIR", fonts)
        .env("U2S_BLOB_DIR", blob_dir("xfa").display().to_string());
    let xfa_client = xfa_server.connect().await;

    let xfa_render = call(
        &xfa_client,
        "xfa_render_page",
        json!({ "doc_path": path, "page": 1, "dpi": 72, "format": "png" }),
    )
    .await;
    let real_bytes = xfa_render
        .content
        .iter()
        .find_map(|b| b.as_image().map(|i| i.data.clone()))
        .map(|b64| base64_decode(&b64))
        .expect("inline real image");

    let real_text = call(
        &xfa_client,
        "xfa_page_text",
        json!({ "doc_path": path, "page": 1, "limit": 100000 }),
    )
    .await;
    xfa_client.cancel().await.ok();

    let shim_ratio = dark_pixel_ratio(&shim_bytes);
    let real_ratio = dark_pixel_ratio(&real_bytes);
    assert!(
        real_ratio > shim_ratio * 2.0,
        "the real form ({real_ratio:.4}) should carry substantially more ink \
         than pdfium's shim ({shim_ratio:.4})"
    );

    // And the real engine can read the document's own content — a supplier
    // name that would never appear in a generic "update your reader" notice.
    let text = structured(&real_text)["text"].as_str().unwrap_or("");
    assert!(
        text.contains("UBS"),
        "the real form's own text should be readable, got {text:?}"
    );

    assert_ne!(
        shim_bytes, real_bytes,
        "the two renderers must not agree on this document"
    );
}

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
