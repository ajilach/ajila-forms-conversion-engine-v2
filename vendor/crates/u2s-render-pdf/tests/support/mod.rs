//! Fixture generation with lopdf. Building these in code rather than checking
//! in binaries keeps the repo free of third-party PDF licensing and makes each
//! fixture's *intent* readable — the 250-page pagination document in
//! particular has no business being a checked-in blob.

use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};

/// Where generated fixtures live. Built once per test binary run.
pub fn fixture_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/generated");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

pub fn path(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

struct PageSpec {
    width: f32,
    height: f32,
    rotate: i64,
    content: Content,
}

fn build(pages: Vec<PageSpec>) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut kids = Vec::new();
    for spec in pages {
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            spec.content.encode().expect("encode content"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), spec.width.into(), spec.height.into()],
            "Rotate" => spec.rotate,
        });
        kids.push(page_id.into());
    }

    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
            "Resources" => resources_id,
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();
    doc
}

fn text_page(width: f32, height: f32, rotate: i64, lines: &[&str]) -> PageSpec {
    let mut ops = vec![Operation::new("BT", vec![])];
    let mut y = height - 72.0;
    for line in lines {
        ops.push(Operation::new("Tf", vec!["F1".into(), 24.into()]));
        ops.push(Operation::new("Td", vec![72.0.into(), y.into()]));
        ops.push(Operation::new(
            "Tj",
            vec![Object::string_literal(line.to_string())],
        ));
        // Td is relative, so undo the offset before the next line.
        ops.push(Operation::new("Td", vec![(-72.0).into(), (-y).into()]));
        y -= 36.0;
    }
    ops.push(Operation::new("ET", vec![]));
    PageSpec {
        width,
        height,
        rotate,
        content: Content { operations: ops },
    }
}

/// 10 pages, mixed A4/letter, page 3 rotated 90 degrees.
pub fn ten_pages() -> PathBuf {
    let p = path("ten-pages.pdf");
    if p.exists() {
        return p;
    }
    let pages = (1..=10)
        .map(|i| {
            let (w, h) = if i % 2 == 0 {
                (612.0, 792.0)
            } else {
                (595.3, 841.9)
            };
            let rotate = if i == 3 { 90 } else { 0 };
            text_page(w, h, rotate, &[&format!("Page {i}"), "u2s render fixture"])
        })
        .collect();
    build(pages).save(&p).expect("save ten-pages");
    p
}

/// A page carrying a filled Bezier curve and a clipped region: the features a
/// content-stream reconstruction drops, and therefore the fixture that proves
/// why this renderer exists.
pub fn curves_and_clipping() -> PathBuf {
    let p = path("image-and-curves.pdf");
    if p.exists() {
        return p;
    }
    let ops = vec![
        // Clip to a rectangle, then paint a wide band that the clip cuts off.
        Operation::new("q", vec![]),
        Operation::new("re", vec![50.into(), 500.into(), 200.into(), 200.into()]),
        Operation::new("W", vec![]),
        Operation::new("n", vec![]),
        Operation::new("rg", vec![0.9.into(), 0.2.into(), 0.2.into()]),
        Operation::new("re", vec![0.into(), 400.into(), 600.into(), 400.into()]),
        Operation::new("f", vec![]),
        Operation::new("Q", vec![]),
        // A filled cubic Bezier blob.
        Operation::new("rg", vec![0.1.into(), 0.3.into(), 0.8.into()]),
        Operation::new("m", vec![100.into(), 200.into()]),
        Operation::new(
            "c",
            vec![
                150.into(),
                380.into(),
                350.into(),
                380.into(),
                400.into(),
                200.into(),
            ],
        ),
        Operation::new(
            "c",
            vec![
                350.into(),
                100.into(),
                150.into(),
                100.into(),
                100.into(),
                200.into(),
            ],
        ),
        Operation::new("f", vec![]),
    ];
    build(vec![PageSpec {
        width: 595.3,
        height: 841.9,
        rotate: 0,
        content: Content { operations: ops },
    }])
    .save(&p)
    .expect("save curves");
    p
}

/// Multi-line text for the windowing tests.
pub fn unicode_text() -> PathBuf {
    let p = path("unicode-text.pdf");
    if p.exists() {
        return p;
    }
    build(vec![text_page(
        595.3,
        841.9,
        0,
        &["Hello world", "Gruesse aus Zuerich", "0123456789"],
    )])
    .save(&p)
    .expect("save unicode");
    p
}

/// Password-protected; must be refused with the encrypted-document message.
pub fn encrypted() -> PathBuf {
    let p = path("encrypted.pdf");
    if p.exists() {
        return p;
    }
    let mut doc = build(vec![text_page(595.3, 841.9, 0, &["Secret"])]);
    // The standard security handler derives its key from the file ID, so the
    // trailer must carry one before encryption.
    let id = Object::Array(vec![
        Object::String(vec![7u8; 16], lopdf::StringFormat::Hexadecimal),
        Object::String(vec![7u8; 16], lopdf::StringFormat::Hexadecimal),
    ]);
    doc.trailer.set("ID", id);
    let state =
        lopdf::encryption::EncryptionState::try_from(lopdf::encryption::EncryptionVersion::V2 {
            document: &doc,
            owner_password: "owner",
            user_password: "user",
            key_length: 16,
            permissions: lopdf::encryption::Permissions::all(),
        })
        .expect("encryption state");
    doc.encrypt(&state).expect("encrypt");
    doc.save(&p).expect("save encrypted");
    p
}

/// Valid header, truncated body: must be refused as a format error.
pub fn truncated() -> PathBuf {
    let p = path("truncated.pdf");
    if p.exists() {
        return p;
    }
    let full = std::fs::read(ten_pages()).expect("read ten-pages");
    std::fs::write(&p, &full[..full.len() / 3]).expect("write truncated");
    p
}

/// Not a PDF at all.
pub fn not_a_pdf() -> PathBuf {
    let p = path("not-a-pdf.txt");
    if p.exists() {
        return p;
    }
    std::fs::write(&p, b"this is plainly not a PDF").expect("write");
    p
}

/// One very large page, to exercise the long-edge clamp.
pub fn huge_page() -> PathBuf {
    let p = path("huge-page.pdf");
    if p.exists() {
        return p;
    }
    build(vec![text_page(5000.0, 5000.0, 0, &["Huge"])])
        .save(&p)
        .expect("save huge");
    p
}

/// 250 pages for the pagination walk. Generated, never checked in.
pub fn many_pages() -> PathBuf {
    let p = path("many-pages.pdf");
    if p.exists() {
        return p;
    }
    let pages = (1..=250)
        .map(|i| text_page(595.3, 841.9, 0, &[&format!("Page {i}")]))
        .collect();
    build(pages).save(&p).expect("save many-pages");
    p
}

/// A real XFA form from the vendored UBS corpus (`corpus/ubs/`).
pub fn xfa_form() -> PathBuf {
    u2s_test_assets::corpus_form("AAAA_019_DE.pdf")
}
