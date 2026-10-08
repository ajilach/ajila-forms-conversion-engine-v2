//! Does the port actually parse, lay out and draw? Corpus- and font-gated where
//! it must be; the synthetic templates run anywhere.

use u2s_xfa::{Flattened, FlattenedNodeKind, XfaNode, fonts};

/// Fonts and the corpus are both committed; a caller that needs either and
/// finds it missing has a broken checkout, not a machine to skip on.
fn ensure_fonts() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
}

fn template(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<subform name="form1" layout="tb"><pageSet><pageArea name="P1" w="612pt" h="792pt">
<contentArea x="36pt" y="36pt" w="540pt" h="720pt"/></pageArea></pageSet>
<subform name="Body" layout="tb" w="540pt">{body}</subform></subform></template></xdp:xdp>"#
    )
}

fn draw(name: &str, text: &str, h: u32) -> String {
    format!(
        r#"<draw name="{name}" w="400pt" h="{h}pt"><font typeface="Helvetica" size="11pt"/><value><text>{text}</text></value></draw>"#
    )
}

#[test]
fn a_synthetic_template_parses_and_lays_out() {
    ensure_fonts();
    let xdp = template(&format!(
        "{}{}",
        draw("Title", "Synthetic XDP Title", 30),
        r#"<field name="FirstName" w="300pt" h="22pt"><ui><textEdit/></ui><value><text>Ada</text></value></field>"#
    ));

    let nodes = XfaNode::parse(xdp.as_bytes()).expect("parse");
    let flat = Flattened::from_xfa_simple(&nodes).expect("flatten");

    // The pageArea's own dimensions win over the A4 default.
    assert_eq!(flat.page.width.round().to_string(), "612");
    assert_eq!(flat.page.height.round().to_string(), "792");

    let texts: Vec<String> = flat
        .iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Text { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("Synthetic XDP Title")),
        "draw text should survive layout, got {texts:?}"
    );

    let fields: Vec<(String, String)> = flat
        .iter_nodes()
        .filter_map(|n| match &n.kind {
            FlattenedNodeKind::Field { name, value, .. } => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(fields, vec![("FirstName".to_string(), "Ada".to_string())]);
}

#[test]
fn content_taller_than_the_content_area_produces_page_breaks() {
    ensure_fonts();
    let body: String = (1..=60)
        .map(|i| draw(&format!("R{i}"), &format!("Row {i}"), 24))
        .collect();
    let nodes = XfaNode::parse(template(&body).as_bytes()).expect("parse");
    let flat = Flattened::from_xfa_simple(&nodes).expect("flatten");

    assert!(
        !flat.page.page_breaks.is_empty(),
        "60 rows of 24pt overflow a 720pt content area and must break"
    );

    let img = flat.render_to_image_buffer_plain(1.0).expect("render");
    let pages = flat.slice_into_pages(img, 1.0);
    assert!(
        pages.len() >= 2,
        "expected a multi-page slice, got {}",
        pages.len()
    );
    assert!(pages.iter().all(|p| p.width() > 0 && p.height() > 0));
}

#[test]
fn rendering_is_byte_identical_within_a_process() {
    ensure_fonts();
    let nodes = XfaNode::parse(template(&draw("T", "Determinism", 30)).as_bytes()).expect("parse");

    let once = {
        let flat = Flattened::from_xfa_simple(&nodes).expect("flatten");
        flat.render_to_image_buffer_plain(1.0)
            .expect("render")
            .into_raw()
    };
    let twice = {
        let flat = Flattened::from_xfa_simple(&nodes).expect("flatten");
        flat.render_to_image_buffer_plain(1.0)
            .expect("render")
            .into_raw()
    };
    assert_eq!(once, twice, "the same input must render identically");
}

/// The real thing: a UBS XFA form through extraction, parse and layout.
#[test]
fn a_real_xfa_form_flattens() {
    ensure_fonts();
    let form = u2s_test_assets::corpus_form("AAAA_019_DE.pdf");

    let bytes = std::fs::read(&form).expect("read");
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("this fixture is an XFA form");
    let nodes = XfaNode::parse(&xfa).expect("parse");
    let flat = Flattened::from_xfa_simple(&nodes).expect("flatten");

    assert!(flat.node_count() > 100, "a real form has substance");

    // Captions arrive as Text nodes, not as Field.label — verified against the
    // real corpus, and the reason page-text extraction reads Text content
    // rather than trusting labels.
    let has_caption_text = flat.iter_nodes().any(|n| {
        matches!(
            &n.kind, FlattenedNodeKind::Text { content, .. } if content.contains("Adress")
        )
    });
    assert!(
        has_caption_text,
        "expected German caption text among the Text nodes"
    );

    let img = flat.render_to_image_buffer_plain(1.0).expect("render");
    assert!(img.width() > 500 && img.height() > 500);
}

/// Deviation 7: upstream discards the `/XFA` array's packet names and returns
/// one concatenated blob. Keeping them is what lets a caller ask for the
/// template packet instead of forty megabytes.
#[test]
fn xfa_packets_keep_their_names() {
    let form = u2s_test_assets::corpus_form("AAAA_019_DE.pdf");
    let bytes = std::fs::read(&form).expect("read");
    let packets = u2s_xfa::extract_xfa_packets(&bytes)
        .expect("extract")
        .expect("this fixture is an XFA form");

    assert!(
        packets.len() > 1,
        "a packetised /XFA array should yield several packets"
    );
    let names: Vec<&str> = packets.iter().map(|p| p.name.as_str()).collect();
    assert!(
        names.contains(&"template"),
        "the template packet must be addressable by name, got {names:?}"
    );
    assert!(
        packets.iter().all(|p| !p.content.is_empty()),
        "every named packet must carry its decompressed bytes"
    );

    // And the concatenating helper must still agree with the sum of the parts,
    // since the parser consumes the fragment sequence.
    let joined = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    let total: usize = packets.iter().map(|p| p.content.len()).sum();
    assert_eq!(joined.len(), total);
}

/// A non-XFA PDF must report cleanly rather than erroring — this is how the
/// renderer will tell that a document belongs to the PDF server instead.
#[test]
fn a_plain_pdf_has_no_xfa() {
    let plain = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-render-pdf/fixtures/generated/ten-pages.pdf");
    assert!(
        plain.is_file(),
        "{} is committed; missing means a broken checkout",
        plain.display()
    );
    let bytes = std::fs::read(plain).expect("read");
    assert!(
        u2s_xfa::extract_xfa_packets(&bytes)
            .expect("extract")
            .is_none(),
        "a plain PDF must report no XFA, not an error"
    );
}

/// Fidelity level 3 on a real form: the form's own scripts run, so the result
/// should differ from the bare template. If they were identical, the script
/// executor would be doing nothing and nobody would notice.
#[test]
fn running_scripts_changes_the_result() {
    ensure_fonts();
    let form = u2s_test_assets::corpus_form("AAAA_019_DE.pdf");
    let bytes = std::fs::read(&form).expect("read");
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    let nodes = XfaNode::parse(&xfa).expect("parse");

    let scripted = u2s_xfa::prepare_default(&nodes).expect("prepare");
    assert_eq!(scripted.fidelity, u2s_xfa::Fidelity::Scripts);
    assert!(
        scripted.warning.is_none(),
        "a healthy form should not degrade"
    );

    let template = u2s_xfa::prepare_template_only(&nodes).expect("template");

    // Scripts populate values and toggle visibility, so the two layouts should
    // not be identical. Compare rendered bytes: this catches value *and*
    // layout differences.
    let a = scripted
        .flattened
        .render_to_image_buffer_plain(1.0)
        .expect("render")
        .into_raw();
    let b = template
        .flattened
        .render_to_image_buffer_plain(1.0)
        .expect("render")
        .into_raw();
    assert_ne!(
        a, b,
        "fidelity 3 should differ from the bare template — if not, scripts are not running"
    );
}

/// Preparing the same form twice must give byte-identical output. Script
/// execution involves a JS engine and hash-keyed value maps, so this is where
/// nondeterminism would show up if it existed.
#[test]
fn fidelity_three_is_deterministic() {
    ensure_fonts();
    let form = u2s_test_assets::corpus_form("AAAA_019_DE.pdf");
    let bytes = std::fs::read(&form).expect("read");
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    let nodes = XfaNode::parse(&xfa).expect("parse");

    let first = u2s_xfa::prepare_default(&nodes).expect("prepare");
    let second = u2s_xfa::prepare_default(&nodes).expect("prepare");

    assert_eq!(first.fidelity, second.fidelity);
    assert_eq!(
        first.flattened.page.page_breaks, second.flattened.page.page_breaks,
        "page breaks must not vary between runs"
    );
    let a = first
        .flattened
        .render_to_image_buffer_plain(1.0)
        .expect("render")
        .into_raw();
    let b = second
        .flattened
        .render_to_image_buffer_plain(1.0)
        .expect("render")
        .into_raw();
    assert_eq!(a, b, "the same form must render identically");
}
