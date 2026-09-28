//! The facade against real documents, from the vendored UBS corpus
//! (`corpus/ubs/`) and the vendored fonts (`vendor/fonts/`).

use u2s_render_xfa::states::StateSpec;
use u2s_render_xfa::{DocumentKind, ImageFormat, Limits, RectPt, RenderError, Renderer, Target, fonts};

/// The corpus and fallback fonts are both committed, so absence means a
/// broken checkout, not a machine this suite skips on. Also bumps the
/// document cache (see below) — call this before touching a `Renderer`.
fn require_fonts() {
    assert!(
        fonts::test_support::ensure_registered(),
        "no fonts registered — see crates/u2s-xfa/src/fonts.rs test_support::font_dir"
    );
    ensure_test_cache_size();
}

/// The document cache (default `DOC_CACHE_SIZE`) is one process-wide LRU
/// shared by every `Renderer` in this test binary, because the underlying
/// worker thread is a singleton (font manager state is process-global; see
/// `renderer.rs`). Several tests deliberately use *distinct* real corpus forms
/// so their raster-count assertions cannot collide — but that means more
/// distinct documents than the default cache holds, and under parallel test
/// execution the cache would otherwise thrash: one test's document evicted by
/// another's, silently resetting its per-document counters mid-test.
///
/// Every corpus-based test calls `require_fonts()` before it ever touches a
/// `Renderer`, so gating the bump here — once, via `Once` — guarantees it is
/// set before the worker thread spawns, regardless of which test's thread
/// happens to run first.
fn ensure_test_cache_size() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: runs at most once, inside `Once::call_once`, which blocks
        // every other caller until it returns — so no concurrent env access
        // is possible, and this happens before the first `Renderer::new()`
        // call in the process (which is what actually reads it).
        unsafe {
            std::env::set_var("DOC_CACHE_SIZE", "16");
        }
    });
}

fn renderer() -> Renderer {
    Renderer::new(Limits::default())
}

#[test]
fn info_describes_a_real_form() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let info = renderer().info(&Target::doc(&form, &StateSpec::default())).expect("info");

    assert_eq!(info.kind, DocumentKind::Xfa);
    assert!(info.page_count >= 1);
    assert_eq!(info.pages.len(), info.page_count as usize);
    assert_eq!(info.language, "de", "a German form should detect as German");
    assert!(
        info.packets.iter().any(|p| p == "template"),
        "packets should be named, got {:?}",
        info.packets
    );
    assert!(
        info.pages
            .iter()
            .all(|p| p.width_pt > 100.0 && p.height_pt > 0.0),
        "every page needs real geometry: {:?}",
        info.pages
    );
}

/// Page count must not depend on the resolution someone happens to ask for —
/// which is why the band arithmetic is done in points at a fixed scale rather
/// than in pixels.
#[test]
fn page_count_is_independent_of_dpi() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let expected = r
        .info(&Target::doc(&form, &StateSpec::default()))
        .expect("info")
        .page_count;

    for dpi in [36.0, 72.0, 144.0, 300.0] {
        let (page, _) = r
            .render_page(
                &Target::doc(&form, &StateSpec::default()),
                1,
                Some(dpi),
                None,
                ImageFormat::Jpeg,
            )
            .expect("render");
        assert!(page.width_px > 0);
        assert_eq!(
            r.info(&Target::doc(&form, &StateSpec::default()))
                .expect("info")
                .page_count,
            expected,
            "page count changed at {dpi} dpi"
        );
    }
}

#[test]
fn rendering_a_page_produces_a_page_shaped_image() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let info = r.info(&Target::doc(&form, &StateSpec::default())).expect("info");
    let (page, _) = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            1,
            Some(72.0),
            None,
            ImageFormat::Png,
        )
        .expect("render");

    let band = &info.pages[0];
    assert!(
        (page.width_px as f32 - band.width_pt).abs() < 3.0,
        "at 72 dpi a point is a pixel: {} vs {}",
        page.width_px,
        band.width_pt
    );
    assert!(
        (page.height_px as f32 - band.height_pt).abs() < 3.0,
        "page height should match its band: {} vs {}",
        page.height_px,
        band.height_pt
    );

    // A real form page has ink on it. A blank result would mean the crop landed
    // outside the content, which is exactly the kind of silent wrongness bands
    // are meant to prevent.
    let img = image::load_from_memory(&page.data)
        .expect("decode")
        .to_luma8();
    let dark = img.pixels().filter(|p| p.0[0] < 200).count() as f64;
    let ratio = dark / (img.width() * img.height()) as f64;
    assert!(ratio > 0.005, "page 1 looks blank (dark ratio {ratio:.4})");
}

/// Every page rendered individually must equal the corresponding slice of one
/// whole-column render. If these disagree, page boundaries mean different
/// things in different code paths.
#[test]
fn per_page_renders_tile_the_whole_column() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let info = r.info(&Target::doc(&form, &StateSpec::default())).expect("info");

    let mut total_height = 0u32;
    for page in 1..=info.page_count {
        let (rendered, _) = r
            .render_page(
                &Target::doc(&form, &StateSpec::default()),
                page,
                Some(72.0),
                None,
                ImageFormat::Png,
            )
            .expect("render");
        assert_eq!(rendered.page, page);
        total_height += rendered.height_px;
    }

    // The bands tile the content, so the pages stacked back up should account
    // for the whole column, within rounding of one pixel per boundary.
    let column: f32 = info.pages.iter().map(|p| p.height_pt).sum();
    assert!(
        (total_height as f32 - column).abs() <= info.page_count as f32 + 2.0,
        "pages ({total_height}px) should account for the column ({column}pt)"
    );
}

#[test]
fn the_cursor_walk_covers_every_page_once() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let expected = r
        .info(&Target::doc(&form, &StateSpec::default()))
        .expect("info")
        .page_count;

    let mut seen = Vec::new();
    let mut from = Some(1u32);
    while let Some(start) = from {
        let batch = r
            .render_pages(
                &Target::doc(&form, &StateSpec::default()),
                None,
                Some(start),
                Some(1),
                Some(36.0),
                None,
                ImageFormat::Jpeg,
            )
            .expect("batch");
        assert!(!batch.pages.is_empty());
        seen.extend(batch.pages.iter().map(|p| p.page));
        from = batch.next_from;
    }
    assert_eq!(seen, (1..=expected).collect::<Vec<_>>());
}

#[test]
fn page_text_is_page_local_and_windows_honestly() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let full = r
        .page_text(&Target::doc(&form, &StateSpec::default()), 1, None, Some(1_000_000))
        .expect("text");
    assert!(!full.truncated);
    assert!(full.total_chars > 0, "page 1 should have text");

    // Tiled windows must reconstruct the page exactly.
    let mut rebuilt = String::new();
    let mut offset = 0;
    loop {
        let w = r
            .page_text(&Target::doc(&form, &StateSpec::default()), 1, Some(offset), Some(64))
            .expect("window");
        assert_eq!(w.total_chars, full.total_chars, "total is of the page");
        rebuilt.push_str(&w.text);
        if !w.truncated {
            break;
        }
        offset += w.text.chars().count();
    }
    assert_eq!(rebuilt, full.text);

    // And text belongs to its own page. On a multi-page form the pages should
    // not be identical, which they would be if the band filter did nothing.
    let info = r.info(&Target::doc(&form, &StateSpec::default())).expect("info");
    if info.page_count > 1 {
        let second = r
            .page_text(&Target::doc(&form, &StateSpec::default()), 2, None, Some(1_000_000))
            .expect("text");
        assert_ne!(full.text, second.text, "each page must have its own text");
    }
}

#[test]
fn a_region_crop_matches_the_same_rect_of_the_full_page() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let rect = RectPt {
        x: 40.0,
        y: 40.0,
        width: 200.0,
        height: 100.0,
    };
    let region = r
        .render_region(
            &Target::doc(&form, &StateSpec::default()),
            1,
            rect,
            Some(72.0),
            ImageFormat::Png,
        )
        .expect("region");

    assert!(
        (region.width_px as f32 - 200.0).abs() <= 3.0,
        "got {}",
        region.width_px
    );
    assert!(
        (region.height_px as f32 - 100.0).abs() <= 3.0,
        "got {}",
        region.height_px
    );

    // Cut the same rect out of the full page and compare: the region tool must
    // not be looking at a different part of the document.
    let (page, _) = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            1,
            Some(72.0),
            None,
            ImageFormat::Png,
        )
        .expect("page");
    let page_img = image::load_from_memory(&page.data)
        .expect("decode")
        .to_rgba8();
    let expected =
        image::imageops::crop_imm(&page_img, 40, 40, region.width_px, region.height_px).to_image();
    let actual = image::load_from_memory(&region.data)
        .expect("decode")
        .to_rgba8();

    let differing = actual
        .pixels()
        .zip(expected.pixels())
        .filter(|(a, b)| a.0 != b.0)
        .count();
    let ratio = differing as f64 / (actual.width() * actual.height()) as f64;
    assert!(
        ratio < 0.02,
        "region crop should match the page's own rect (differing {ratio:.3})"
    );
}

#[test]
fn a_plain_pdf_is_refused_with_a_pointer_to_the_other_renderer() {
    // No real fonts needed (this must fail before layout), but it still spawns
    // the process-wide worker via `renderer()` below — so it must bump the
    // cache size first like every other test, or it can win the race to spawn
    // the worker first and leave the whole binary stuck on the default `4`,
    // starving the other tests' cache-count assertions.
    ensure_test_cache_size();
    let plain = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../u2s-render-pdf/fixtures/generated/ten-pages.pdf");
    if !plain.exists() {
        eprintln!("skipping: run `cargo test -p u2s-render-pdf` first");
        return;
    }
    let err = renderer()
        .info(&Target::doc(&plain, &StateSpec::default()))
        .expect_err("must refuse");
    assert!(
        matches!(err, RenderError::UnsupportedInput { .. }),
        "got {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("not an XFA form"), "{msg}");
    assert!(
        msg.contains("pdf_render_page"),
        "must name the right tool: {msg}"
    );
}

#[test]
fn an_out_of_range_page_names_the_valid_range() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();
    let count = r
        .info(&Target::doc(&form, &StateSpec::default()))
        .expect("info")
        .page_count;
    let err = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            count + 5,
            None,
            None,
            ImageFormat::Jpeg,
        )
        .expect_err("must fail");
    assert!(
        matches!(err, RenderError::PageOutOfRange { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains(&format!("1..{count}")));
}

/// Preparing a document costs an XML parse, a JavaScript run and a full layout
/// pass. Every operation needs it, so it had better happen once.
#[test]
fn a_document_is_prepared_once_however_many_operations_run() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAB_019_DE.pdf");
    let r = renderer();
    let _ = r.info(&Target::doc(&form, &StateSpec::default())).expect("info");
    let _ = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            1,
            Some(36.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("render");
    let _ = r
        .page_text(&Target::doc(&form, &StateSpec::default()), 1, None, None)
        .expect("text");
    let _ = r.info(&Target::doc(&form, &StateSpec::default())).expect("info");

    assert_eq!(
        r.prepare_count_for(&form),
        1,
        "four operations should share one prepared layout"
    );
}

/// Rendering a non-default state must actually differ from the default. If the
/// state parameter were only reaching the cache key and not the layout, every
/// other state test would still pass while the images were identical.
#[test]
fn a_requested_state_renders_differently_from_the_default() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAA_019_DE.pdf");
    let r = renderer();

    let bytes = std::fs::read(&form).expect("read");
    let xfa = u2s_render_xfa::states::controls(
        &u2s_xfa::XfaNode::parse(
            &u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
                .expect("extract")
                .expect("xfa"),
        )
        .expect("parse"),
    )
    .expect("controls");

    let Some(target) = xfa.controls.iter().find(|c| !c.options.is_empty()) else {
        eprintln!("skipping: no settable control on this form");
        return;
    };
    let spec = StateSpec {
        selections: vec![u2s_render_xfa::states::SelectionSpec {
            field: target.field.clone(),
            value: target.options[0].value.clone(),
        }],
    };

    // Render whichever page the target control actually lands on, rather
    // than assuming page 1: a positioned subform that used to straddle a
    // page break (rendering part of its controls on page 1) may now move
    // whole to a later page, and that is a pagination fix, not a reason for
    // this control's page to be stable across engine versions.
    let chosen_controls = r
        .controls(&Target::doc(&form, &spec))
        .expect("controls chosen");
    let target_ctrl = chosen_controls
        .controls
        .iter()
        .find(|c| c.field == target.field)
        .expect("target control listed");
    let Some(pos) = target_ctrl.positions.first() else {
        eprintln!("skipping: {} has no position (not laid out)", target.field);
        return;
    };
    let page = pos.page;

    let (default, _) = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            page,
            Some(36.0),
            None,
            ImageFormat::Png,
        )
        .expect("default");
    let (chosen, _) = r
        .render_page(&Target::doc(&form, &spec), page, Some(36.0), None, ImageFormat::Png)
        .expect("chosen");

    // Same page, same dpi — so any difference is the state itself.
    assert_eq!(default.page, chosen.page);
    assert_ne!(
        default.data, chosen.data,
        "selecting {} = {} changed nothing; the state is not reaching the layout",
        target.field, target.options[0].value
    );

    // And the two states must be cached separately rather than colliding.
    assert_eq!(
        r.prepare_count_for(&form),
        2,
        "two distinct states should prepare twice, not share one entry"
    );
}

/// The expensive step is rasterizing the tall column, not cropping a page out
/// of it. A cursor walk over one document at one resolution must rasterize
/// once, not once per page — otherwise a 60-page form costs 60 full-document
/// renders to read sequentially.
#[test]
fn a_cursor_walk_rasterizes_the_column_once() {
    // A form not requested by any other facade test at this dpi: the raster
    // cache lives on the process-wide `Prepared` cache, so a document another
    // test already warmed at the same scale would make "one raster" true by
    // accident regardless of test order.
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAI_019_DE.pdf");
    let r = renderer();
    let count = r
        .info(&Target::doc(&form, &StateSpec::default()))
        .expect("info")
        .page_count;

    let walk = |r: &Renderer| {
        for page in 1..=count {
            let _ = r
                .render_page(
                    &Target::doc(&form, &StateSpec::default()),
                    page,
                    Some(155.0),
                    None,
                    ImageFormat::Jpeg,
                )
                .expect("render");
        }
    };

    walk(&r);
    let after_first = r.raster_count_for(&form).expect("raster count");
    assert_eq!(
        after_first, 1,
        "{count} pages at one resolution should rasterize the column exactly once"
    );

    walk(&r);
    assert_eq!(
        r.raster_count_for(&form).expect("raster count"),
        after_first,
        "a repeated walk over an already-rasterized column must not rasterize again"
    );
}

/// Two different resolutions of the same document are two different columns,
/// so the cache must not confuse them — the point of keying by scale.
///
/// Uses dpi values no other test in this file requests, and its own document:
/// the raster cache lives on the process-wide `Prepared` cache and outlives any
/// one test, so reusing a (document, dpi) pair another test already rendered
/// would make this pass by accident regardless of whether the cache actually
/// distinguishes scales.
#[test]
fn different_resolutions_are_not_confused_by_the_cache() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAAL_019_DE.pdf");
    let r = renderer();

    let (a, _) = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            1,
            Some(91.0),
            None,
            ImageFormat::Png,
        )
        .expect("91dpi");
    let (b, _) = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            1,
            Some(133.0),
            None,
            ImageFormat::Png,
        )
        .expect("133dpi");
    assert_ne!(
        a.width_px, b.width_px,
        "the two requests must actually differ in scale"
    );
    assert_eq!(
        r.raster_count_for(&form).expect("raster count"),
        2,
        "two distinct scales should rasterize twice"
    );

    // And asking for the first resolution again must hit the cache, not
    // rasterize a third time.
    let (a_again, _) = r
        .render_page(
            &Target::doc(&form, &StateSpec::default()),
            1,
            Some(91.0),
            None,
            ImageFormat::Png,
        )
        .expect("91dpi again");
    assert_eq!(a.width_px, a_again.width_px);
    assert_eq!(
        r.raster_count_for(&form).expect("raster count"),
        2,
        "a repeated resolution should hit the cache"
    );
}

/// The single biggest risk in resolving a control's SOM path against the
/// flattened layout is a join that silently matches nothing on a real form
/// (as opposed to the small hand-built fixtures, where an accidental match is
/// easy). `AABK_019_DE.pdf` is the densest form in the corpus — 129 exclGroups
/// and 297 radios — so a join that only works by coincidence on a small
/// document is the most likely thing to fail here.
#[test]
fn every_visible_control_on_a_control_dense_real_form_has_a_position() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AABK_019_DE.pdf");
    let r = renderer();
    let target = Target::doc(&form, &StateSpec::default());

    let info = r.info(&target).expect("info");
    let c = r.controls(&target).expect("controls");
    assert!(
        c.controls.len() > 100,
        "this form is supposed to be control-dense, got {}",
        c.controls.len()
    );

    let visible: Vec<_> = c.controls.iter().filter(|ctrl| ctrl.visible).collect();
    assert!(!visible.is_empty(), "a freshly opened form shows something");

    let mut missing = Vec::new();
    for ctrl in &visible {
        if ctrl.positions.is_empty() {
            missing.push(ctrl.field.clone());
            continue;
        }
        for pos in &ctrl.positions {
            let page = info
                .pages
                .iter()
                .find(|p| p.page == pos.page)
                .unwrap_or_else(|| panic!("{}: page {} does not exist", ctrl.field, pos.page));
            assert!(
                pos.x >= -0.5
                    && pos.y >= -0.5
                    && pos.x + pos.width <= page.width_pt + 0.5
                    && pos.y + pos.height <= page.height_pt + 0.5,
                "{}: {pos:?} is outside page {} ({}x{}pt)",
                ctrl.field,
                pos.page,
                page.width_pt,
                page.height_pt
            );
        }
    }
    assert!(
        missing.is_empty(),
        "{} of {} visible controls have no position: {missing:?}",
        missing.len(),
        visible.len()
    );
}

/// AAJB_033_IT's "B) Clienti Connessi" section has three question blocks
/// (B1, "Rapporto che il cliente ha con questo conto", B2), each a section
/// heading `<draw>` with `<keep next="contentArea"/>` directly followed by
/// an unsplittable positioned subform. Per the Acrobat capture
/// (`pdf-screenshots/aajb/Screenshot 2026-09-22 at 11.06.54.png` and
/// `...11.17.13.png`), page 1 ends with the "50:50 di capitale/ diritto di
/// voto" radio row and page 2 opens with the "B2)" heading -- the heading
/// and the question block it introduces must move together.
#[test]
fn a_heading_with_keep_next_moves_with_its_question_block() {
    require_fonts();
    let form = u2s_xfa::corpus::test_support::form("AAJB_033_IT.pdf");
    let r = renderer();
    let target = Target::doc(&form, &StateSpec::default());

    let info = r.info(&target).expect("info");
    assert!(info.page_count >= 2, "expected a multi-page form");

    let page1 = r
        .page_text(&target, 1, None, Some(1_000_000))
        .expect("page 1 text");
    assert!(
        page1.text.contains("50:50 di capitale"),
        "page 1 should end with the 50:50 radio row, has: {:?}",
        page1.text
    );
    assert!(
        !page1.text.contains("B2)"),
        "the B2) heading has a keep-next constraint into the unsplittable \
         question block that follows it, which does not fit on page 1, so \
         it must not appear there; page 1 has: {:?}",
        page1.text
    );

    let page2 = r
        .page_text(&target, 2, None, Some(1_000_000))
        .expect("page 2 text");
    let heading_at = page2
        .text
        .find("B2) C'è una connessione economica?")
        .unwrap_or_else(|| panic!("page 2 should open with the B2) heading, has: {:?}", page2.text));
    let question_at = page2
        .text
        .find("Il cliente ha una connessione economica")
        .expect("page 2 should contain the B2 question block");
    assert!(
        heading_at < question_at,
        "the B2) heading must lead its question block on page 2"
    );
}
