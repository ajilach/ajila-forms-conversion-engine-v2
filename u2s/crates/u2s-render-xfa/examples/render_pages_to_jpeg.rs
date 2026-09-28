//! Render every page of an XFA form to one JPEG per page.
//!
//! Usage: cargo run -p u2s-render-xfa --example render_pages_to_jpeg -- <doc.pdf> <out_dir>
//!
//! Requires U2S_FONT_DIR (a directory of .ttf/.otf files) so the layout
//! engine can measure text; without it XFA layout silently produces wrong
//! page breaks rather than failing.

use std::path::PathBuf;

use u2s_render_xfa::states::StateSpec;
use u2s_render_xfa::{ImageFormat, Limits, Renderer, Target, fonts};

fn main() {
    let mut args = std::env::args().skip(1);
    let doc_path = args.next().expect("usage: <doc.pdf> <out_dir>");
    let out_dir = PathBuf::from(args.next().expect("usage: <doc.pdf> <out_dir>"));

    fonts::register_from_env()
        .expect("set U2S_FONT_DIR to a directory of .ttf/.otf fonts before rendering XFA forms");
    std::fs::create_dir_all(&out_dir).expect("create output dir");

    let renderer = Renderer::new(Limits::default());
    let target = Target::doc(&doc_path, &StateSpec::default());

    let info = renderer.info(&target).expect("xfa_info");
    println!("document: {} pages, kind {:?}", info.page_count, info.kind);

    let batch = renderer
        .render_pages(
            &target,
            None,
            Some(1),
            Some(info.page_count as usize),
            None,
            None,
            ImageFormat::Jpeg,
        )
        .expect("render_pages");

    if let Some(w) = &batch.warning {
        eprintln!("warning: {w}");
    }

    for page in &batch.pages {
        let path = out_dir.join(format!("page-{:03}.jpg", page.page));
        std::fs::write(&path, &page.data).expect("write page");
        println!(
            "wrote {} ({}x{} px, {} bytes)",
            path.display(),
            page.width_px,
            page.height_px,
            page.data.len()
        );
    }
}
