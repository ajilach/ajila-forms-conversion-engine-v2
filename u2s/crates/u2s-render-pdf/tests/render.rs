//! Library-level tests against real pdfium and generated fixtures.

mod support;

use u2s_render_pdf::{FormType, ImageFormat, Limits, Pattern, RectPt, RenderError, Renderer};

fn renderer() -> Renderer {
    Renderer::start(Limits::default())
        .expect("pdfium must be available; run scripts/fetch-pdfium.sh or set PDFIUM_LIB_PATH")
}

// ---------------------------------------------------------------- info

#[test]
fn info_reports_geometry_rotation_and_form_type() {
    let r = renderer();
    let info = r.info(support::ten_pages()).expect("info");

    assert_eq!(info.page_count, 10);
    assert_eq!(info.pages.len(), 10);
    assert!(!info.pages_truncated);
    assert_eq!(info.form_type, FormType::None);
    assert!(info.warning.is_none(), "no XFA warning on a plain PDF");

    // Page 1 is A4, page 2 is letter, page 3 is rotated.
    assert!((info.pages[0].width_pt - 595.3).abs() < 1.0);
    assert!((info.pages[1].width_pt - 612.0).abs() < 1.0);
    assert_eq!(info.pages[2].rotation, 90);
}

#[test]
fn info_caps_the_page_list_and_says_so() {
    let limits = Limits {
        max_page_entries: 5,
        ..Limits::default()
    };
    let r = Renderer::start(limits).expect("start");
    let info = r.info(support::many_pages()).expect("info");

    assert_eq!(info.page_count, 250, "the count is never truncated");
    assert_eq!(info.pages.len(), 5, "the listing is");
    assert!(info.pages_truncated);
}

// ------------------------------------------------------------ rendering

#[test]
fn render_page_honours_dpi_and_reports_effective_value() {
    let r = renderer();
    let (page, warning) = r
        .render_page(support::ten_pages(), 1, Some(72.0), None, ImageFormat::Png)
        .expect("render");

    assert_eq!(page.page, 1);
    assert!(warning.is_none());
    // A4 at 72 dpi is one pixel per point.
    assert!(
        (page.width_px as f32 - 595.3).abs() <= 2.0,
        "got {}",
        page.width_px
    );
    assert_eq!(page.dpi_effective, 72.0);
    assert_eq!(page.mime, "image/png");
    assert!(page.data.starts_with(b"\x89PNG"), "PNG magic");
}

/// Both encoders produce well-formed output with the right mime. Note this
/// deliberately does *not* assert jpeg < png: on near-blank synthetic pages PNG
/// wins easily, and only on photographic content does the reverse hold. JPEG is
/// the default because real scans are photographic, not because it always wins.
#[test]
fn both_encoders_produce_well_formed_images() {
    let r = renderer();
    let (jpeg, _) = r
        .render_page(
            support::ten_pages(),
            1,
            Some(144.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("jpeg");
    let (png, _) = r
        .render_page(support::ten_pages(), 1, Some(144.0), None, ImageFormat::Png)
        .expect("png");

    assert_eq!(jpeg.mime, "image/jpeg");
    assert!(jpeg.data.starts_with(&[0xFF, 0xD8]), "JPEG magic");
    assert_eq!(png.mime, "image/png");
    assert!(png.data.starts_with(b"\x89PNG"), "PNG magic");
    assert_eq!(
        (jpeg.width_px, jpeg.height_px),
        (png.width_px, png.height_px),
        "format must not change geometry"
    );
}

#[test]
fn long_edge_clamp_binds_on_a_huge_page() {
    let r = renderer();
    let (page, _) = r
        .render_page(
            support::huge_page(),
            1,
            Some(600.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("render");

    let long = page.width_px.max(page.height_px);
    assert!(long <= 2576, "long edge {long} exceeds the default clamp");
    assert!(
        page.dpi_effective < 600.0,
        "effective dpi {} must report the clamp",
        page.dpi_effective
    );
}

#[test]
fn a_smaller_caller_max_edge_is_respected() {
    let r = renderer();
    let (page, _) = r
        .render_page(
            support::ten_pages(),
            1,
            Some(300.0),
            Some(400),
            ImageFormat::Jpeg,
        )
        .expect("render");
    assert!(page.width_px.max(page.height_px) <= 400);
}

/// The reason this crate exists rather than reusing the forms engine's
/// content-stream reconstruction: pdfium honours curves and clipping, which a
/// reconstruction of text runs and axis-aligned rectangles cannot.
#[test]
fn curves_and_clipping_are_actually_rasterized() {
    let r = renderer();
    let (page, _) = r
        .render_page(
            support::curves_and_clipping(),
            1,
            Some(72.0),
            None,
            ImageFormat::Png,
        )
        .expect("render");

    let img = image::load_from_memory(&page.data)
        .expect("decode png")
        .to_rgb8();
    let px = |x: u32, y: u32| {
        let p = img.get_pixel(x, y).0;
        (p[0] as i32, p[1] as i32, p[2] as i32)
    };
    let is_red = |(r, g, b): (i32, i32, i32)| r > 180 && g < 120 && b < 120;
    let is_white = |(r, g, b): (i32, i32, i32)| r > 230 && g > 230 && b > 230;
    let is_blue = |(r, _g, b): (i32, i32, i32)| b > 150 && r < 120;

    // The red band spans the full page width but is clipped to x 50..250,
    // y 500..700 in PDF space — which is y 142..342 from the top.
    assert!(
        is_red(px(150, 240)),
        "inside the clip must be painted, got {:?}",
        px(150, 240)
    );
    assert!(
        is_white(px(400, 240)),
        "outside the clip must be untouched — clipping was ignored, got {:?}",
        px(400, 240)
    );

    // The filled Bezier sits around y 100..300 in PDF space, i.e. y 542..742.
    assert!(
        is_blue(px(250, 640)),
        "the Bezier fill must be rasterized, got {:?}",
        px(250, 640)
    );
}

// ----------------------------------------------------------- pagination

/// The core invariant: walking `next_from` to exhaustion yields every page
/// exactly once, in order, whatever the caps are.
#[test]
fn cursor_walk_covers_every_page_exactly_once() {
    for (limit, max_bytes) in [(1usize, 4 << 20), (3, 4 << 20), (4, 40_000), (2, 1)] {
        let limits = Limits {
            max_response_bytes: max_bytes,
            ..Limits::default()
        };
        let r = Renderer::start(limits).expect("start");

        let mut seen = Vec::new();
        let mut from = Some(1u32);
        let mut hops = 0;
        while let Some(start) = from {
            let batch = r
                .render_pages(
                    support::ten_pages(),
                    None,
                    Some(start),
                    Some(limit),
                    Some(72.0),
                    None,
                    ImageFormat::Jpeg,
                )
                .expect("batch");
            assert!(!batch.pages.is_empty(), "a batch must make progress");
            seen.extend(batch.pages.iter().map(|p| p.page));
            from = batch.next_from;

            hops += 1;
            assert!(hops < 100, "cursor failed to terminate");
        }

        let expected: Vec<u32> = (1..=10).collect();
        assert_eq!(
            seen, expected,
            "limit={limit} max_bytes={max_bytes} produced {seen:?}"
        );
    }
}

#[test]
fn count_cap_is_reported_and_never_exceeded() {
    let r = renderer();
    let batch = r
        .render_pages(
            support::ten_pages(),
            None,
            Some(1),
            Some(2),
            Some(72.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("batch");

    assert_eq!(batch.pages.len(), 2);
    assert_eq!(batch.budget_hit, Some("count"));
    assert_eq!(batch.next_from, Some(3));
}

#[test]
fn byte_budget_stops_a_batch_before_the_count_cap() {
    // A budget far below one page forces a stop after the first image.
    let limits = Limits {
        max_response_bytes: 1,
        ..Limits::default()
    };
    let r = Renderer::start(limits).expect("start");
    let batch = r
        .render_pages(
            support::ten_pages(),
            None,
            Some(1),
            Some(4),
            Some(144.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("batch");

    assert_eq!(
        batch.pages.len(),
        1,
        "an over-budget page still comes back alone"
    );
    assert_eq!(batch.budget_hit, Some("bytes"));
    assert_eq!(batch.next_from, Some(2));
}

#[test]
fn the_last_batch_reports_no_cursor() {
    let r = renderer();
    let batch = r
        .render_pages(
            support::ten_pages(),
            None,
            Some(9),
            Some(4),
            Some(72.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("batch");

    assert_eq!(batch.pages.len(), 2);
    assert_eq!(batch.next_from, None, "the walk is complete");
    assert_eq!(batch.budget_hit, None);
}

#[test]
fn an_explicit_page_list_is_rendered_in_the_order_given() {
    let r = renderer();
    let batch = r
        .render_pages(
            support::ten_pages(),
            Some(vec![7, 2, 5]),
            None,
            Some(4),
            Some(72.0),
            None,
            ImageFormat::Jpeg,
        )
        .expect("batch");
    assert_eq!(
        batch.pages.iter().map(|p| p.page).collect::<Vec<_>>(),
        vec![7, 2, 5]
    );
}

#[test]
fn walking_a_large_document_terminates_and_is_complete() {
    let r = renderer();
    let mut seen = Vec::new();
    let mut from = Some(1u32);
    while let Some(start) = from {
        let batch = r
            .render_pages(
                support::many_pages(),
                None,
                Some(start),
                Some(4),
                Some(36.0),
                None,
                ImageFormat::Jpeg,
            )
            .expect("batch");
        seen.extend(batch.pages.iter().map(|p| p.page));
        from = batch.next_from;
    }
    assert_eq!(seen.len(), 250);
    assert_eq!(seen.first(), Some(&1));
    assert_eq!(seen.last(), Some(&250));
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
}

// --------------------------------------------------------------- region

#[test]
fn region_renders_a_crop_of_the_page() {
    let r = renderer();
    let rect = RectPt {
        x: 50.0,
        y: 50.0,
        width: 200.0,
        height: 100.0,
    };
    let page = r
        .render_region(support::ten_pages(), 1, rect, Some(144.0), ImageFormat::Png)
        .expect("region");

    // 200x100 pt at 144 dpi is 400x200 px, within rounding.
    assert!(
        (page.width_px as i64 - 400).abs() <= 2,
        "got {}",
        page.width_px
    );
    assert!(
        (page.height_px as i64 - 200).abs() <= 2,
        "got {}",
        page.height_px
    );
}

#[test]
fn a_region_outside_the_page_is_refused_with_the_dimensions() {
    let r = renderer();
    let rect = RectPt {
        x: 500.0,
        y: 500.0,
        width: 400.0,
        height: 400.0,
    };
    let err = r
        .render_region(support::ten_pages(), 1, rect, None, ImageFormat::Jpeg)
        .expect_err("must refuse");

    assert!(matches!(err, RenderError::RegionOutOfBounds { .. }));
    let msg = err.to_string();
    assert!(msg.contains("outside page 1"), "{msg}");
    assert!(msg.contains("595"), "message names the page size: {msg}");
}

// ----------------------------------------------------------------- text

#[test]
fn page_text_windows_reconstruct_the_whole_page() {
    let r = renderer();
    let full = r
        .page_text(support::unicode_text(), 1, None, Some(100_000))
        .expect("full text");
    assert!(!full.truncated);
    assert!(full.text.contains("Hello world"), "got {:?}", full.text);

    let mut rebuilt = String::new();
    let mut offset = 0;
    loop {
        let window = r
            .page_text(support::unicode_text(), 1, Some(offset), Some(7))
            .expect("window");
        rebuilt.push_str(&window.text);
        assert_eq!(window.total_chars, full.total_chars);
        if !window.truncated {
            break;
        }
        offset += window.text.chars().count();
    }
    assert_eq!(
        rebuilt, full.text,
        "tiled windows must reconstruct the page"
    );
}

#[test]
fn text_limit_is_capped_at_the_configured_ceiling() {
    let limits = Limits {
        text_limit_max: 5,
        ..Limits::default()
    };
    let r = Renderer::start(limits).expect("start");
    let w = r
        .page_text(support::unicode_text(), 1, None, Some(1_000_000))
        .expect("text");
    assert!(w.text.chars().count() <= 5);
    assert!(w.truncated);
}

// ------------------------------------------------------------- taxonomy

#[test]
fn a_missing_file_names_the_path() {
    let r = renderer();
    let err = r.info("/nonexistent/nope.pdf").expect_err("must fail");
    assert!(matches!(err, RenderError::Io { .. }));
    assert!(err.to_string().contains("/nonexistent/nope.pdf"));
}

#[test]
fn a_non_pdf_is_refused() {
    let r = renderer();
    let err = r.info(support::not_a_pdf()).expect_err("must fail");
    assert!(
        matches!(
            err,
            RenderError::UnsupportedInput { .. } | RenderError::Backend { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn an_encrypted_pdf_says_so_plainly() {
    let r = renderer();
    let err = r.info(support::encrypted()).expect_err("must fail");
    assert!(matches!(err, RenderError::Encrypted { .. }), "got {err:?}");
    assert!(err.to_string().contains("password-protected"));
}

#[test]
fn a_truncated_pdf_is_refused() {
    let r = renderer();
    let err = r.info(support::truncated()).expect_err("must fail");
    assert!(
        matches!(
            err,
            RenderError::UnsupportedInput { .. } | RenderError::Backend { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn an_out_of_range_page_names_the_valid_range() {
    let r = renderer();
    let err = r
        .render_page(support::ten_pages(), 15, None, None, ImageFormat::Jpeg)
        .expect_err("must fail");

    assert!(matches!(
        err,
        RenderError::PageOutOfRange {
            page: 15,
            page_count: 10
        }
    ));
    let msg = err.to_string();
    assert!(msg.contains("15"), "{msg}");
    assert!(msg.contains("1..10"), "message must name the range: {msg}");
}

#[test]
fn page_zero_is_out_of_range_because_pages_are_one_based() {
    let r = renderer();
    let err = r
        .render_page(support::ten_pages(), 0, None, None, ImageFormat::Jpeg)
        .expect_err("must fail");
    assert!(matches!(err, RenderError::PageOutOfRange { page: 0, .. }));
}

#[test]
fn the_worker_survives_an_error_and_serves_the_next_call() {
    let r = renderer();
    let _ = r.info(support::not_a_pdf()).expect_err("must fail");
    let info = r.info(support::ten_pages()).expect("still healthy");
    assert_eq!(info.page_count, 10);
}

// ---------------------------------------------------------------- cache

#[test]
fn repeated_info_calls_hit_the_metadata_cache() {
    // A private copy: the worker and its cache are process-wide, so a shared
    // fixture would let a concurrent test pollute the open count.
    let src = support::ten_pages();
    let mine = support::path("cache-probe.pdf");
    std::fs::copy(&src, &mine).expect("copy fixture");

    let r = renderer();
    for _ in 0..5 {
        r.info(&mine).expect("info");
    }
    let opens = r.open_count_for(&mine);
    assert_eq!(opens, 1, "5 info calls opened the document {opens} times");
}

#[test]
fn concurrent_calls_serialize_without_error() {
    use std::sync::Arc;
    let r = Arc::new(renderer());
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                let page = (i % 10) + 1;
                r.render_page(
                    support::ten_pages(),
                    page,
                    Some(36.0),
                    None,
                    ImageFormat::Jpeg,
                )
                .map(|(p, _)| p.page)
            })
        })
        .collect();

    for h in handles {
        let page = h.join().expect("thread").expect("render");
        assert!((1..=10).contains(&page));
    }
}

// ------------------------------------------------------------------ XFA

#[test]
fn an_xfa_document_is_flagged_for_the_other_renderer() {
    let xfa = support::xfa_form();
    let r = renderer();
    let info = r.info(&xfa).expect("info");

    assert!(
        info.form_type.is_xfa(),
        "expected an XFA form type, got {:?}",
        info.form_type
    );
    let warning = info.warning.expect("XFA documents must carry a warning");
    assert!(warning.contains("XFA renderer"), "{warning}");

    // Rendering still succeeds — pdfium draws the shim — but the warning rides
    // along so a caller cannot mistake it for the real form.
    let (page, warning) = r
        .render_page(&xfa, 1, Some(72.0), None, ImageFormat::Jpeg)
        .expect("render");
    assert!(page.width_px > 0);
    assert!(
        warning.is_some(),
        "the render must carry the routing warning"
    );
}

// --------------------------------------------------------- text search

fn pattern(query: &str) -> Pattern {
    Pattern::parse(query, false).expect("pattern")
}

/// The composition the whole tool is for: a hit's page, offset and length,
/// handed to `page_text`, must return the match itself.
#[test]
fn a_search_hit_addresses_itself_in_page_text() {
    let r = renderer();
    let found = r
        .search_text(support::unicode_text(), None, pattern("Zuerich"), None)
        .expect("search");

    assert_eq!(found.total_matches, 1);
    assert!(!found.truncated);
    assert_eq!(found.through, 1);
    assert_eq!(found.next_from, None, "a one-page document runs out");
    assert_eq!(found.budget_hit, None);

    let m = &found.matches[0];
    assert_eq!(m.page, 1);
    let read = r
        .page_text(
            support::unicode_text(),
            m.page,
            Some(m.offset),
            Some(m.length),
        )
        .expect("page_text");
    assert_eq!(read.text, "Zuerich", "a hit must address itself");
}

/// A match is tagged with the page it is on, not the page the walk started at.
#[test]
fn a_match_names_the_page_it_is_on() {
    let r = renderer();
    // 3 digits: "Page 1" also prefixes Page 10..Page 199, so a shorter query
    // would not prove the page tag rather than the first hit.
    let found = r
        .search_text(support::many_pages(), None, pattern("Page 137"), None)
        .expect("search");

    assert_eq!(found.total_matches, 1, "exactly one page says 'Page 137'");
    assert_eq!(found.matches[0].page, 137);
}

/// A literal is matched case-insensitively, and the case in the document is
/// what comes back — the offset addresses the original text, not a folded copy.
#[test]
fn a_literal_search_is_case_insensitive_but_reports_the_documents_own_case() {
    let r = renderer();
    let found = r
        .search_text(support::unicode_text(), None, pattern("hello WORLD"), None)
        .expect("search");

    assert_eq!(found.total_matches, 1);
    let m = &found.matches[0];
    let read = r
        .page_text(
            support::unicode_text(),
            m.page,
            Some(m.offset),
            Some(m.length),
        )
        .expect("page_text");
    assert_eq!(read.text, "Hello world");
}

/// The page cap hands back a cursor, and walking it covers every page exactly
/// once — the same guarantee the render cursor makes.
#[test]
fn a_capped_search_walk_covers_every_page_exactly_once() {
    let limits = Limits {
        search_max_pages: 60,
        ..Limits::default()
    };
    let r = Renderer::start(limits).expect("start");

    let mut seen: Vec<u32> = Vec::new();
    let mut from = Some(1u32);
    let mut hops = 0;
    let mut page_stops = 0;

    while let Some(start) = from {
        let found = r
            .search_text(
                support::many_pages(),
                Some(start),
                pattern("Page"),
                Some(500),
            )
            .expect("search");

        assert!(found.through >= start, "a call must make progress");
        if found.budget_hit == Some("pages") {
            page_stops += 1;
        }
        seen.extend(start..=found.through);
        from = found.next_from;

        hops += 1;
        assert!(hops < 500, "cursor failed to terminate");
    }

    let expected: Vec<u32> = (1..=250).collect();
    assert_eq!(
        seen, expected,
        "the walk must cover every page once, in order"
    );
    assert!(page_stops > 0, "the page cap must genuinely have bound");
}

/// A spent match limit hands back a cursor. Filling to *exactly* the limit
/// drops nothing, so `truncated` is false while `budget_hit` still says the
/// limit is what stopped the scan — the one direction of the contract that is
/// easy to get wrong.
#[test]
fn a_spent_match_limit_hands_back_a_cursor_without_claiming_truncation() {
    let r = renderer();
    // One match per page, so the fifth page spends the limit exactly.
    let found = r
        .search_text(support::many_pages(), None, pattern("Page"), Some(5))
        .expect("search");

    assert_eq!(found.matches.len(), 5, "the limit bounds what is returned");
    assert_eq!(found.total_matches, 5, "and nothing was dropped");
    assert!(!found.truncated, "nothing dropped is not truncation");
    assert_eq!(found.budget_hit, Some("matches"));
    assert_eq!(
        found.next_from,
        Some(6),
        "the cursor names the first unscanned page"
    );
    assert_eq!(found.through, 5);
}

/// When a single page holds more matches than the limit, that page is still
/// scanned whole — so the surplus is counted, reported as truncation, and not
/// silently dropped.
#[test]
fn matches_dropped_within_a_page_are_reported_as_truncated() {
    let r = renderer();
    let found = r
        .search_text(support::unicode_text(), None, pattern("e"), Some(2))
        .expect("search");

    assert_eq!(found.matches.len(), 2, "the limit bounds what is returned");
    assert!(
        found.total_matches > 2,
        "the page that spent the limit is still counted whole: {}",
        found.total_matches
    );
    assert!(found.truncated, "truncation must never be silent");
    assert_eq!(found.through, 1, "the page was scanned, not abandoned");
}

/// `limit` is clamped at both ends: a zero would return nothing beside a
/// non-zero total, which reads as "everything was truncated".
#[test]
fn a_zero_match_limit_is_clamped_rather_than_returning_nothing() {
    let r = renderer();
    let found = r
        .search_text(support::unicode_text(), None, pattern("Hello"), Some(0))
        .expect("search");
    assert_eq!(found.matches.len(), 1);
    assert!(!found.truncated);
}

#[test]
fn searching_from_a_page_past_the_end_names_the_valid_range() {
    let r = renderer();
    let err = r
        .search_text(support::unicode_text(), Some(99), pattern("Hello"), None)
        .expect_err("page 99 of a one-page document");
    match err {
        RenderError::PageOutOfRange { page, page_count } => {
            assert_eq!((page, page_count), (99, 1));
        }
        other => panic!("expected PageOutOfRange, got {other}"),
    }
}

/// A whole-document scan opens the file once, not once per page.
#[test]
fn a_search_opens_the_document_once() {
    // A private copy: the worker and its open counter are process-wide, so a
    // shared fixture would let a concurrent test pollute the count.
    let mine = support::path("search-open-probe.pdf");
    std::fs::copy(support::many_pages(), &mine).expect("copy fixture");

    let r = renderer();
    r.search_text(&mine, None, pattern("Page 200"), None)
        .expect("search");

    let opens = r.open_count_for(&mine);
    assert_eq!(
        opens, 1,
        "a 250-page scan opened the document {opens} times"
    );
}
