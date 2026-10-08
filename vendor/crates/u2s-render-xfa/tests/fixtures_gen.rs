//! The synthetic fixtures must survive the real path: `/XFA` extraction, XDP
//! parse, layout, render. If they do, the e2e suite needs neither the customer
//! corpus nor a licensed font to be meaningful.

mod support;

use u2s_render_xfa::states::StateSpec;
use u2s_render_xfa::{DocumentKind, ImageFormat, Limits, Pattern, Renderer, Target, fonts};

/// The fonts are committed, so failing to register them is a broken checkout,
/// not a reason to skip.
fn require_fonts() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
}

#[test]
fn synthetic_fixtures_travel_the_real_extraction_path() {
    for p in [support::minimal(), support::overflow(), support::choices()] {
        let bytes = std::fs::read(&p).expect("read");
        let packets = u2s_render_xfa::extract_xfa_from_pdf_bytes(&bytes)
            .expect("extract")
            .unwrap_or_else(|| panic!("{} should carry XFA", p.display()));
        assert!(!packets.is_empty(), "{} yielded no XFA bytes", p.display());
    }
}

#[test]
fn the_minimal_fixture_is_a_one_page_form() {
    require_fonts();
    let r = Renderer::new(Limits::default());
    let info = r
        .info(&Target::doc(support::minimal(), &StateSpec::default()))
        .expect("info");

    assert_eq!(info.kind, DocumentKind::Xfa);
    assert_eq!(info.page_count, 1);
    assert!(
        info.packets.iter().any(|p| p == "template"),
        "{:?}",
        info.packets
    );

    let (page, _) = r
        .render_page(
            &Target::doc(support::minimal(), &StateSpec::default()),
            1,
            Some(72.0),
            None,
            ImageFormat::Png,
        )
        .expect("render");
    // 612pt wide at 72 dpi.
    assert!(
        (page.width_px as i64 - 612).abs() <= 2,
        "got {}",
        page.width_px
    );

    let text = r
        .page_text(&Target::doc(support::minimal(), &StateSpec::default()), 1, None, None)
        .expect("text");
    assert!(
        text.text.contains("Minimal XFA Fixture"),
        "the draw's text should be readable, got {:?}",
        text.text
    );
}

#[test]
fn the_overflow_fixture_really_has_several_pages() {
    require_fonts();
    let r = Renderer::new(Limits::default());
    let info = r
        .info(&Target::doc(support::overflow(), &StateSpec::default()))
        .expect("info");
    assert!(
        info.page_count >= 2,
        "60 rows of 24pt must overflow a 720pt content area, got {} page(s)",
        info.page_count
    );

    // And the cursor walk covers them, which is what the e2e battery needs.
    let mut seen = Vec::new();
    let mut from = Some(1u32);
    while let Some(start) = from {
        let batch = r
            .render_pages(
                &Target::doc(support::overflow(), &StateSpec::default()),
                None,
                Some(start),
                Some(1),
                Some(36.0),
                None,
                ImageFormat::Jpeg,
            )
            .expect("batch");
        seen.extend(batch.pages.iter().map(|p| p.page));
        from = batch.next_from;
    }
    assert_eq!(seen, (1..=info.page_count).collect::<Vec<_>>());
}

#[test]
fn the_choices_fixture_offers_a_state_space() {
    require_fonts();
    let bytes = std::fs::read(support::choices()).expect("read");
    let xfa = u2s_render_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    let nodes = u2s_render_xfa::XfaNode::parse(&xfa).expect("parse");

    let c = u2s_render_xfa::states::controls(&nodes).expect("controls");
    assert!(
        !c.controls.is_empty(),
        "the radio group and checkbox should be discoverable"
    );
    assert!(
        c.space_size >= 2,
        "space_size {} is not a space",
        c.space_size
    );
}

// ------------------------------------------------------------- text search

/// The composition that the whole tool is for: a hit's page, offset and
/// length, handed to `page_text`, must return the match itself.
#[test]
fn a_search_hit_addresses_itself_in_page_text() {
    require_fonts();
    let r = Renderer::new(Limits::default());
    let found = r
        .search_text(
            &Target::doc(support::minimal(), &StateSpec::default()),
            None,
            Pattern::parse("Ada", false).expect("pattern"),
            None,
        )
        .expect("search");

    assert_eq!(found.total_matches, 1, "the fixture's FirstName is 'Ada'");
    assert!(!found.truncated);
    assert_eq!(found.through, 1);
    assert_eq!(found.next_from, None, "a one-page form runs out");
    assert_eq!(found.budget_hit, None);

    let m = &found.matches[0];
    assert_eq!(m.page, 1);
    let read = r
        .page_text(
            &Target::doc(support::minimal(), &StateSpec::default()),
            m.page,
            Some(m.offset),
            Some(m.length),
        )
        .expect("page_text");
    assert_eq!(read.text, "Ada", "a hit must address itself");
}

/// The page cap hands back a cursor, and walking it covers every page exactly
/// once — the same guarantee the render cursor makes.
#[test]
fn a_capped_search_walk_covers_every_page_exactly_once() {
    require_fonts();
    // One page per call, so the cursor is exercised on every hop.
    let limits = Limits {
        search_max_pages: 1,
        ..Limits::default()
    };
    let r = Renderer::new(limits);

    let page_count = r
        .info(&Target::doc(support::overflow(), &StateSpec::default()))
        .expect("info")
        .page_count;
    assert!(page_count > 1, "the overflow fixture must be multi-page");

    let mut seen = Vec::new();
    let mut from = Some(1u32);
    let mut hops = 0;
    while let Some(start) = from {
        let found = r
            .search_text(
                &Target::doc(support::overflow(), &StateSpec::default()),
                Some(start),
                Pattern::parse("row", false).expect("pattern"),
                None,
            )
            .expect("search");

        assert_eq!(found.through, start, "one page per call at this cap");
        if found.next_from.is_some() {
            assert_eq!(found.budget_hit, Some("pages"));
        }
        seen.push(found.through);
        from = found.next_from;

        hops += 1;
        assert!(hops < 500, "cursor failed to terminate");
    }

    let expected: Vec<u32> = (1..=page_count).collect();
    assert_eq!(
        seen, expected,
        "the walk must cover every page once, in order"
    );

    // The whole walk pays for one prepare: the layout is cached per state.
    assert_eq!(r.prepare_count_for(support::overflow()), 1);
}

/// A spent match limit is reported, not silently applied, and it hands back a
/// cursor pointing at the first page nobody looked at.
#[test]
fn a_spent_match_limit_is_reported_with_a_cursor() {
    require_fonts();
    let r = Renderer::new(Limits::default());
    let found = r
        .search_text(
            &Target::doc(support::overflow(), &StateSpec::default()),
            None,
            Pattern::parse("row", false).expect("pattern"),
            Some(2),
        )
        .expect("search");

    assert_eq!(found.matches.len(), 2, "the limit bounds what is returned");
    assert!(
        found.total_matches > 2,
        "but the total counts the whole page it was spent on: {}",
        found.total_matches
    );
    assert!(found.truncated, "truncation must never be silent");
    assert_eq!(found.budget_hit, Some("matches"));
    assert!(
        found.next_from.is_some(),
        "a spent limit leaves pages unscanned"
    );
}

#[test]
fn a_page_past_the_end_names_the_valid_range() {
    require_fonts();
    let r = Renderer::new(Limits::default());
    let err = r
        .search_text(
            &Target::doc(support::minimal(), &StateSpec::default()),
            Some(99),
            Pattern::parse("Ada", false).expect("pattern"),
            None,
        )
        .expect_err("page 99 of a one-page form");
    assert!(err.to_string().contains("valid range"), "{err}");
}
