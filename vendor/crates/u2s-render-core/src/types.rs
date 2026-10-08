//! Wire types shared by every renderer. Field names are the contract — they
//! serialize straight into MCP tool results, and the `pdf_*` and `xfa_*` tool
//! surfaces are deliberately identical in shape.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageGeometry {
    /// 1-based.
    pub page: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    /// Page rotation in degrees: 0, 90, 180 or 270.
    pub rotation: u16,
}

/// One rendered page. `data` is the encoded image; the caller decides whether
/// it is small enough to inline or must go to the blob store.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub page: u32,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi_effective: f32,
    pub mime: &'static str,
    pub data: Vec<u8>,
}

/// A cursor-paginated batch. `next_from` is `None` exactly when the walk is
/// complete.
#[derive(Debug, Clone)]
pub struct RenderedPages {
    pub pages: Vec<RenderedPage>,
    pub next_from: Option<u32>,
    /// Which cap stopped this batch: `"count"`, `"bytes"`, or `None` when the
    /// document simply ran out.
    pub budget_hit: Option<&'static str>,
    /// Set when the document should have been sent to a different renderer.
    pub warning: Option<String>,
}

/// A window of a page's extracted text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageText {
    pub page: u32,
    pub text: String,
    /// Character offset this window starts at.
    pub offset: usize,
    /// Total characters on the page, so a caller can size its walk.
    pub total_chars: usize,
    pub truncated: bool,
}

/// One search hit. `offset` and `length` are **character** offsets into that
/// page's text, so `page` + `offset` + `length` fed to a page-text call
/// returns exactly the match — which is what makes finding and reading two
/// steps rather than one guess.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMatch {
    pub page: u32,
    pub offset: usize,
    pub length: usize,
    pub context: String,
}

/// The result of one text-search call.
///
/// Two independent honesty signals, because two different things can be cut
/// short: `truncated` says matches were dropped from the pages that *were*
/// scanned, `next_from` says pages were never looked at. The relations hold in
/// one direction only — `truncated` implies `budget_hit == Some("matches")`,
/// and that implies `next_from.is_some()`; neither converse does, since a call
/// can fill to exactly `limit` with nothing dropped.
///
/// **Pages are atomic**: each is either scanned whole or not at all, so
/// `next_from` always names the first page never examined and a resumed walk
/// never re-reports a match. That is the same exactly-once guarantee
/// [`crate::render_batch`] makes for pages of images; the alternative was a
/// second cursor dimension for an offset within a page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSearch {
    pub matches: Vec<TextMatch>,
    /// Matches found in the pages **this call** scanned — not the document's
    /// total, which is unknown until the walk finishes. May exceed
    /// `matches.len()`; see `truncated`. Note the page that spends the match
    /// limit is still scanned whole, so all of its matches are counted here
    /// even though only some are returned.
    pub total_matches: usize,
    /// The match limit dropped entries from the range scanned.
    pub truncated: bool,
    /// The last page actually scanned. With `next_from` this names the range
    /// `total_matches` refers to.
    pub through: u32,
    /// Where to continue; `None` exactly when the document ran out.
    pub next_from: Option<u32>,
    /// Which cap stopped the scan: `"matches"`, `"pages"`, or `None` when the
    /// document simply ran out.
    pub budget_hit: Option<&'static str>,
}

/// A rectangle in points with the origin at the page's **top-left**, which is
/// how a reader describes a region. PDF's own convention is bottom-left, so the
/// PDF renderer converts once at its boundary; XFA is natively top-left.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RectPt {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
