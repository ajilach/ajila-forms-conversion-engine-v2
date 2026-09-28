//! Pages, from a renderer that does not have any.
//!
//! The XFA engine lays a document out as one tall column, but that column is a
//! stack of whole pages: it is exactly `page_count * page.height` tall and page
//! `k` is the band `[k*H, (k+1)*H)`. "Page 3" is therefore a horizontal band of
//! that column, and per-page rendering is a crop rather than a separate render.
//!
//! This module owns the band arithmetic, in points, so that page geometry,
//! per-page text and region cropping all agree with what `slice_into_pages`
//! will actually cut.

use u2s_render_core::PageGeometry;
use u2s_xfa::Flattened;

/// One page's extent in the tall layout, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    /// 1-based page number.
    pub page: u32,
    pub top: f32,
    pub bottom: f32,
    pub width: f32,
}

impl Band {
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

fn to_f32(d: u2s_xfa::Num) -> f32 {
    use std::str::FromStr;
    f32::from_str(&d.to_string()).unwrap_or(0.0)
}

/// The height of the rendered column: every page at full height, which is
/// exactly what `render_to_image_buffer_plain` uses to size its buffer.
pub fn column_height(flat: &Flattened) -> f32 {
    to_f32(flat.page.column_height())
}

/// Split the column into pages, mirroring `Flattened::slice_into_pages`.
///
/// Deliberately computed in **points at a fixed reference scale**, never in
/// pixels: a page boundary landing on a pixel edge must not be able to appear
/// at one dpi and not another. Page count must not depend on the resolution
/// someone asked for.
pub fn bands(flat: &Flattened) -> Vec<Band> {
    let width = to_f32(flat.page.width);
    let height = to_f32(flat.page.height);
    let count = flat.page.page_count.max(1);

    // A degenerate page height would make every band empty; one band covering
    // the column is the only sensible reading of that.
    if height <= 0.0 {
        return vec![Band {
            page: 1,
            top: 0.0,
            bottom: column_height(flat).max(0.0),
            width,
        }];
    }

    (0..count)
        .map(|k| Band {
            page: k + 1,
            top: k as f32 * height,
            bottom: (k + 1) as f32 * height,
            width,
        })
        .collect()
}

/// The tallest page in the document, in points. Every page is one page tall
/// now, but the callers want a reference height rather than an assumption:
/// it is what the long-edge clamp is measured against, so that a caller asking
/// for one page of a ten-page form gets the resolution they asked for and every
/// page in a walk shares one rasterized scale.
pub fn max_page_height(bands: &[Band]) -> f32 {
    bands.iter().map(|b| b.height()).fold(0.0f32, f32::max)
}

pub fn geometry(flat: &Flattened) -> Vec<PageGeometry> {
    bands(flat)
        .into_iter()
        .map(|b| PageGeometry {
            page: b.page,
            width_pt: b.width,
            height_pt: b.height(),
            rotation: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use u2s_xfa::{Num, Page};

    fn flat(width: f64, height: f64, page_count: u32) -> Flattened {
        let mut page = Page::new(
            Num::try_from(width).expect("width"),
            Num::try_from(height).expect("height"),
        );
        page.page_count = page_count;
        Flattened::new(page, vec![])
    }

    /// Bands must tile the column exactly: no gaps, no overlaps, covering the
    /// whole column height. Everything else — page geometry, per-page text,
    /// region offsets — is derived from this, so a gap here is a page of
    /// content nobody can address.
    fn assert_tiles(bands: &[Band], column: f32) {
        assert!(!bands.is_empty(), "there is always at least one page");
        assert_eq!(bands[0].top, 0.0, "the first band starts at the top");
        for w in bands.windows(2) {
            assert_eq!(w[0].bottom, w[1].top, "bands must be contiguous");
            assert_eq!(w[1].page, w[0].page + 1, "pages are numbered in order");
        }
        assert!(
            (bands.last().unwrap().bottom - column).abs() < 0.01,
            "the last band must reach the end of the column"
        );
    }

    #[test]
    fn a_single_page_form_is_one_band() {
        let f = flat(595.0, 842.0, 1);
        let bands = bands(&f);
        assert_eq!(bands.len(), 1);
        assert_tiles(&bands, 842.0);
    }

    /// Every page is a whole page, including the last one. A short final page
    /// would mean the page furniture drawn at the foot of it had been cropped
    /// away.
    #[test]
    fn every_band_is_exactly_one_page_tall() {
        let f = flat(595.0, 842.0, 3);
        let bands = bands(&f);
        assert_eq!(bands.len(), 3);
        assert_tiles(&bands, 3.0 * 842.0);
        for b in &bands {
            assert_eq!(b.height(), 842.0, "page {} is not a whole page", b.page);
            assert_eq!(b.width, 595.0);
        }
        assert_eq!(max_page_height(&bands), 842.0);
    }

    #[test]
    fn geometry_reports_one_entry_per_page() {
        let f = flat(595.0, 842.0, 2);
        let g = geometry(&f);
        assert_eq!(g.iter().map(|p| p.page).collect::<Vec<_>>(), vec![1, 2]);
        assert!(g.iter().all(|p| (p.height_pt - 842.0).abs() < 0.01));
    }

    /// `Flattened::page_of_y` is duplicated arithmetic — `u2s-xfa` cannot
    /// depend on this crate to share `bands()` directly (the dependency runs
    /// the other way) — so this is the guard against the two drifting apart.
    /// Checked at every band's top and just inside its bottom, which is where
    /// an off-by-one in either implementation would show up first.
    #[test]
    fn page_of_y_agrees_with_bands_at_every_boundary() {
        let f = flat(595.0, 842.0, 3);
        for b in bands(&f) {
            let top = Num::try_from(b.top as f64).expect("top");
            let (page, page_top) = f.page_of_y(top);
            assert_eq!(page, b.page, "band {} top {}", b.page, b.top);
            assert_eq!(
                page_top.to_string().parse::<f32>().unwrap(),
                b.top,
                "band {} top {}",
                b.page,
                b.top
            );

            let just_inside = Num::try_from((b.bottom - 0.01) as f64).expect("just inside");
            let (page, _) = f.page_of_y(just_inside);
            assert_eq!(
                page, b.page,
                "band {} just below its bottom {}",
                b.page, b.bottom
            );
        }
    }
}
