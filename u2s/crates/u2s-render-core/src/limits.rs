//! Clamps and budgets. Every unbounded dimension a caller can request is
//! clamped here, and every clamp is *reported* back through effective values —
//! silent degradation is as bad as silent truncation.

/// All configurable limits, with environment overrides.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Default render resolution when the caller does not pass `dpi`.
    pub default_dpi: f32,
    /// Requested dpi is clamped into `[min_dpi, max_dpi]`.
    pub min_dpi: f32,
    pub max_dpi: f32,
    /// Default long-edge clamp in pixels (vision-API-safe; the forms engine
    /// established 2576). Callers may pass a smaller `max_edge_px`, never a
    /// larger one than `hard_max_edge_px`.
    pub max_edge_px: u32,
    /// Absolute ceiling on any rendered bitmap's long edge, bounding memory.
    pub hard_max_edge_px: u32,
    /// Batch: maximum images returned by one `render_pages` call.
    pub max_images_per_call: usize,
    /// Batch: byte budget for the encoded images of one response.
    pub max_response_bytes: usize,
    /// A single encoded image at most this large is inlined; larger goes to
    /// the blob store.
    pub max_inline_bytes: usize,
    /// JPEG quality (the forms engine ships 82 for vision payloads).
    pub jpeg_quality: u8,
    /// Page-text window default and ceiling, in characters.
    pub text_limit_default: usize,
    pub text_limit_max: usize,
    /// Match-count window default and ceiling for the text-search tools.
    pub search_limit_default: usize,
    pub search_limit_max: usize,
    /// Pages one text-search call will scan before handing back a cursor.
    /// Bounds the work of a single call on a long document; the caller walks
    /// `next_from` to cover the rest.
    pub search_max_pages: usize,
    /// Characters of context reported either side of a search match.
    pub search_context_radius: usize,
    /// `pdf_info` lists at most this many per-page dimension entries.
    pub max_page_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            default_dpi: 144.0,
            min_dpi: 36.0,
            max_dpi: 600.0,
            max_edge_px: 2576,
            hard_max_edge_px: 8000,
            max_images_per_call: 4,
            max_response_bytes: 4 * 1024 * 1024,
            max_inline_bytes: 1536 * 1024,
            jpeg_quality: 82,
            text_limit_default: 4000,
            text_limit_max: 20_000,
            search_limit_default: 50,
            search_limit_max: 500,
            search_max_pages: 200,
            search_context_radius: 80,
            max_page_entries: 200,
        }
    }
}

impl Limits {
    /// Defaults overridden by environment variables where set.
    pub fn from_env() -> Self {
        let mut l = Limits::default();
        fn env_parse<T: std::str::FromStr>(key: &str, into: &mut T) {
            if let Ok(v) = std::env::var(key)
                && let Ok(parsed) = v.parse::<T>()
            {
                *into = parsed;
            }
        }
        env_parse("DEFAULT_DPI", &mut l.default_dpi);
        env_parse("MAX_EDGE_PX", &mut l.max_edge_px);
        env_parse("MAX_IMAGES_PER_CALL", &mut l.max_images_per_call);
        env_parse("MAX_RESPONSE_BYTES", &mut l.max_response_bytes);
        env_parse("MAX_INLINE_BYTES", &mut l.max_inline_bytes);
        // Only the page cap is overridable: it is an operational latency
        // bound. The other three search limits are prompt surface — the tool
        // descriptions state the defaults, so an env override would make them
        // lie — which is why the `text_limit_*` siblings are absent too.
        env_parse("SEARCH_MAX_PAGES", &mut l.search_max_pages);
        l
    }

    /// The scale factor actually used for a page, from the requested dpi and
    /// edge clamp. Returns `(scale, dpi_effective)`.
    ///
    /// `scale` is pdfium's page-points multiplier: `dpi / 72`. When the
    /// resulting long edge would exceed the clamp, the scale is reduced so the
    /// long edge lands exactly on it — rendering directly at the reduced scale
    /// beats rendering large and downscaling.
    pub fn effective_scale(
        &self,
        width_pt: f32,
        height_pt: f32,
        requested_dpi: Option<f32>,
        requested_max_edge: Option<u32>,
    ) -> (f32, f32) {
        let dpi = requested_dpi
            .unwrap_or(self.default_dpi)
            .clamp(self.min_dpi, self.max_dpi);
        let max_edge = requested_max_edge
            .unwrap_or(self.max_edge_px)
            .min(self.hard_max_edge_px)
            .max(16);
        let long_pt = width_pt.max(height_pt).max(1.0);
        let mut scale = dpi / 72.0;
        if long_pt * scale > max_edge as f32 {
            scale = max_edge as f32 / long_pt;
        }
        (scale, scale * 72.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_is_clamped_into_range() {
        let l = Limits::default();
        let (_, dpi) = l.effective_scale(100.0, 100.0, Some(10_000.0), None);
        assert_eq!(dpi, 600.0);
        let (_, dpi) = l.effective_scale(100.0, 100.0, Some(1.0), None);
        assert_eq!(dpi, 36.0);
    }

    #[test]
    fn long_edge_clamp_reduces_scale_and_reports_it() {
        let l = Limits::default();
        // A4 at 600 dpi would be 7016 px on the long edge; clamp to 2576.
        let (scale, dpi) = l.effective_scale(595.3, 841.9, Some(600.0), None);
        let long_px = (841.9 * scale).round() as u32;
        assert!(long_px <= 2576, "long edge {long_px} exceeds clamp");
        assert!(dpi < 600.0, "effective dpi must reflect the clamp");
    }

    #[test]
    fn caller_max_edge_cannot_exceed_hard_ceiling() {
        let l = Limits::default();
        let (scale, _) = l.effective_scale(595.3, 841.9, Some(600.0), Some(1_000_000));
        let long_px = (841.9 * scale).round() as u32;
        assert!(long_px <= l.hard_max_edge_px);
    }

    #[test]
    fn small_pages_keep_requested_dpi() {
        let l = Limits::default();
        let (scale, dpi) = l.effective_scale(595.3, 841.9, Some(144.0), None);
        assert_eq!(dpi, 144.0);
        assert!((scale - 2.0).abs() < 1e-6);
    }
}
