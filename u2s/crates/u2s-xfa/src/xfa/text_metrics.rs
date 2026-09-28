//! Text Metrics Module - XFA AXTE-Compliant Text Measurement
//!
//! This module implements text measurement according to the XFA Specification
//! (specifically the AXTE Line Positioning appendix, pages 1521-1532).
//!
//! Key AXTE rules implemented:
//! - Line gap is 20% of font size: `sLG = sFS * 0.2`
//! - If (ascent + descent) < font_size, pad ascent: `sA = sFS - sD`
//! - Text height: `TH = A + D` (accumulated ascent + descent)
//! - Derived line spacing: `DS = TH + LG` (or use override if provided)
//! - Full height: `FH = MT + DS + MB` (with line gap removed on last line)
//! - Baseline: `B = MT + TH - D`
//!
//! Per XFA spec section 17 (font element):
//! - typeface: Default is "Courier"
//! - size: Default is 10pt
//! - weight: "normal" or "bold", default "normal"
//! - posture: "normal" or "italic", default "normal"

use crate::xfa::font_manager::{FontVariant, get_font_manager};
use crate::xfa::{Font, KerningMode, Num, Para, num};
use ab_glyph::{Font as AbGlyphFont, FontRef, PxScale, ScaleFont};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use std::collections::HashMap;

/// Computes the correct PxScale for XFA/PDF text measurement at the given font size.
///
/// ab_glyph scales both axes by `scale / height_unscaled()`, where
/// `height_unscaled() = hhea.ascent - hhea.descent`, in font design units.
/// XFA/PDF (and Adobe's CoolType/AXTE) instead scale every metric —
/// horizontal advances as much as ascent, descent and line gap — by
/// `font_size / head.unitsPerEm`. For fonts where `ascent - descent` differs
/// from `unitsPerEm` (e.g. Frutiger 45 Light: 2308 vs 2048), the two
/// denominators disagree and ab_glyph's scaling is off by `unitsPerEm / height`
/// on every axis.
///
/// Passing `PxScale::from(font_size)` straight through (ab_glyph's default)
/// makes `ascent() + descent()` come out to exactly `font_size` for every
/// font, because that is what `height_unscaled()` scales to by construction —
/// masking AXTE's own ascent/descent padding rule (XFA 3.3 Appendix C,
/// "Basic Metric Adjustments": pad ascent so `A + D >= font_size`, not `== `)
/// and flattening every line's measured and rendered height. For Frutiger 45
/// Light at 8pt, the correct AXTE text height is ~9.02pt
/// (`(1866 + 442) / 2048 * 8`), not 8pt.
///
/// This scales both axes uniformly by `height / units_per_em` so that
/// `metric_px = metric_units * font_size / units_per_em` for every metric —
/// advances, ascent, descent and line gap alike — matching AXTE's own
/// `sA`/`sD`/`sLG`.
pub fn xfa_px_scale(font: &impl AbGlyphFont, font_size: f32) -> PxScale {
    if let Some(units_per_em) = font.units_per_em() {
        let height = font.height_unscaled();
        if units_per_em > 0.0 && height > 0.0 {
            PxScale::from(font_size * height / units_per_em)
        } else {
            PxScale::from(font_size)
        }
    } else {
        PxScale::from(font_size)
    }
}

/// Constants for text measurement fallback values per AXTE spec
/// These are used when font metrics are unavailable
mod constants {
    /// Missing glyph width as fraction of font size (60%)
    pub const MISSING_GLYPH_WIDTH_RATIO: f32 = 0.6;
    /// Space character width fallback as fraction of font size (30%)
    pub const SPACE_WIDTH_RATIO: f32 = 0.3;
    /// Em width fallback as fraction of font size (60%)
    pub const EM_WIDTH_RATIO: f64 = 0.6;
    /// Average character width fallback as fraction of em width (80%)
    pub const AVG_CHAR_WIDTH_RATIO: f64 = 0.8;
    /// AXTE line gap ratio: line gap is always 20% of font size
    pub const AXTE_LINE_GAP_RATIO: f64 = 0.2;
    /// Large width for single-line measurement (effectively unlimited)
    pub const SINGLE_LINE_MAX_WIDTH: f64 = 10000.0;
}

/// Font metrics extracted from a font at a specific size
#[derive(Debug, Clone)]
pub struct FontMetrics {
    /// Font size in points
    pub font_size: Num,
    /// Ascent: height above baseline (positive)
    pub ascent: Num,
    /// Descent: depth below baseline (positive, even though it goes downward)
    pub descent: Num,
    /// Line gap: extra space between lines (per AXTE: always 20% of font size)
    pub line_gap: Num,
    /// Em width for character width estimation
    pub em_width: Num,
    /// Average character width
    pub avg_char_width: Num,
}

impl FontMetrics {
    /// Create font metrics from an ab_glyph font at a given size
    /// Per AXTE spec: line gap is always 20% of font size
    pub fn from_font(font: &FontRef<'_>, font_size_pt: Num) -> Self {
        let size_f32 = font_size_pt.to_f32().unwrap_or(10.0);
        let scale = xfa_px_scale(font, size_f32);
        let scaled_font = font.as_scaled(scale);

        // Get metrics from font (these are in pixels at the given scale)
        let ascent_px = scaled_font.ascent();
        let descent_px = scaled_font.descent().abs(); // descent is often negative
        let _line_gap_from_font = scaled_font.line_gap();

        // Convert to Decimal
        let mut ascent = num(ascent_px as f64);
        let descent = num(descent_px as f64);

        // Per AXTE spec: If (ascent + descent) < font_size, pad the ascent
        // "AXTE accommodates a combined ascent and descent larger than the font size.
        //  If the sum of the two is less than the font size, it pads the ascent,
        //  so that the requested font size is always consumed."
        if ascent + descent < font_size_pt {
            ascent = font_size_pt - descent;
        }

        // Per AXTE spec: "AXTE adopts the convention embraced by other Adobe applications
        // that line gap is always determined to be 20% of font size."
        // sLG = sFS * 0.2
        let line_gap = font_size_pt * num(constants::AXTE_LINE_GAP_RATIO);

        // Estimate character width using 'M' as em-width
        let glyph_id = font.glyph_id('M');
        let em_width = if glyph_id.0 != 0 {
            let advance = scaled_font.h_advance(glyph_id);
            num(advance as f64)
        } else {
            font_size_pt * num(constants::EM_WIDTH_RATIO)
        };

        // Average character width (using 'x' for x-height reference, or 'e' as common char)
        let glyph_id_e = font.glyph_id('e');
        let avg_char_width = if glyph_id_e.0 != 0 {
            let advance = scaled_font.h_advance(glyph_id_e);
            num(advance as f64)
        } else {
            em_width * num(constants::AVG_CHAR_WIDTH_RATIO)
        };

        FontMetrics {
            font_size: font_size_pt,
            ascent,
            descent,
            line_gap,
            em_width,
            avg_char_width,
        }
    }

    /// Get text height (ascent + descent)
    /// Per AXTE: TH = A + D
    pub fn text_height(&self) -> Num {
        self.ascent + self.descent
    }

    /// Get derived line spacing (text height + line gap)
    /// Per AXTE: DS = TH + LG
    pub fn derived_line_spacing(&self) -> Num {
        self.text_height() + self.line_gap
    }

    /// Calculate full height for a single line (no margins)
    /// Per AXTE for a single line: FH = TH + LG (but LG removed on last line)
    pub fn single_line_height(&self, is_last_line: bool) -> Num {
        if is_last_line {
            self.text_height()
        } else {
            self.derived_line_spacing()
        }
    }

    /// Get baseline position from top of line
    /// Per AXTE: B = MT + TH - D (when no spacing override)
    pub fn baseline_from_top(&self, margin_top: Num) -> Num {
        margin_top + self.text_height() - self.descent
    }
}

/// A single line of text with its metrics
#[derive(Debug, Clone)]
pub struct TextLine {
    /// The text content of this line
    pub text: String,
    /// Width of this line in points
    pub width: Num,
    /// Accumulated metrics for this line
    pub metrics: LineMetrics,
}

/// Accumulated metrics for a line (may have multiple spans)
#[derive(Debug, Clone)]
pub struct LineMetrics {
    /// Maximum ascent across all spans in this line
    pub ascent: Num,
    /// Maximum descent across all spans in this line
    pub descent: Num,
    /// Maximum line gap across all spans
    pub line_gap: Num,
    /// Line spacing override (from para element), if any
    pub spacing_override: Option<Num>,
    /// Is this the first line in the text block?
    pub is_first_line: bool,
    /// Is this the last line in the text block?
    pub is_last_line: bool,
    /// Top margin (from paragraph)
    pub margin_top: Num,
    /// Bottom margin (from paragraph)
    pub margin_bottom: Num,
    /// Ascent overflow for first line (accented capitals that exceed declared ascent)
    /// Per AXTE: only applied on first line when no spacing override
    pub ascent_overflow: Num,
}

impl LineMetrics {
    /// Create new line metrics
    pub fn new(is_first_line: bool, is_last_line: bool) -> Self {
        LineMetrics {
            ascent: Decimal::ZERO,
            descent: Decimal::ZERO,
            line_gap: Decimal::ZERO,
            spacing_override: None,
            is_first_line,
            is_last_line,
            margin_top: Decimal::ZERO,
            margin_bottom: Decimal::ZERO,
            ascent_overflow: Decimal::ZERO,
        }
    }

    /// Accumulate font metrics into this line
    /// Per AXTE: accumulate separately, then compute derived values
    pub fn accumulate(&mut self, font_metrics: &FontMetrics) {
        self.ascent = self.ascent.max(font_metrics.ascent);
        self.descent = self.descent.max(font_metrics.descent);
        self.line_gap = self.line_gap.max(font_metrics.line_gap);
    }

    /// Accumulate font metrics with baseline shift per AXTE spec
    /// Per AXTE (page 1530):
    /// - If baseline shift < 0 (up-shift): accumulate A + |shift| into ascent
    /// - If baseline shift > 0 (down-shift): accumulate D + |shift| into descent
    pub fn accumulate_with_baseline_shift(
        &mut self,
        font_metrics: &FontMetrics,
        baseline_shift: Option<Num>,
    ) {
        // Basic accumulation
        let mut adjusted_ascent = font_metrics.ascent;
        let mut adjusted_descent = font_metrics.descent;

        // Per AXTE: baseline shift affects ascent/descent accumulation
        if let Some(bs) = baseline_shift {
            if bs < Decimal::ZERO {
                // Up-shift (negative): increases effective ascent
                adjusted_ascent = font_metrics.ascent + bs.abs();
            } else if bs > Decimal::ZERO {
                // Down-shift (positive): increases effective descent
                adjusted_descent = font_metrics.descent + bs;
            }
        }

        self.ascent = self.ascent.max(adjusted_ascent);
        self.descent = self.descent.max(adjusted_descent);
        self.line_gap = self.line_gap.max(font_metrics.line_gap);
    }

    /// Apply ascent overflow adjustment for first line per AXTE
    /// Per AXTE (page 1531):
    /// "if first line in block and AO > 0 and SP == 0 then
    ///    A = A + AO; TH = TH + AO; FH = FH + AO"
    pub fn apply_ascent_overflow(&mut self) {
        if self.is_first_line
            && self.ascent_overflow > Decimal::ZERO
            && self.spacing_override.is_none()
        {
            self.ascent += self.ascent_overflow;
        }
    }

    /// Get text height
    /// Per AXTE: TH = A + D
    pub fn text_height(&self) -> Num {
        self.ascent + self.descent
    }

    /// Calculate derived line spacing
    /// Per AXTE: If line spacing override is set and > font metrics, use it.
    /// Otherwise: DS = TH + LG
    ///
    /// Special case: On first line in block, if spacing override > default spacing,
    /// the extra space is ignored for consistency with other text processors.
    pub fn derived_spacing(&self) -> Num {
        let default_spacing = self.text_height() + self.line_gap;

        if let Some(sp) = self.spacing_override
            && sp > Decimal::ZERO
        {
            // Per AXTE: "If there is a line spacing override and it is larger than
            // the default line spacing, the extra space is ignored on the first line
            // in a text block, for consistency with other text processing applications."
            if self.is_first_line && sp > default_spacing {
                // For first line, cap at default spacing if override is larger
                return default_spacing;
            }
            return sp;
        }

        default_spacing
    }

    /// Calculate full height of this line
    /// Per AXTE: FH = MT + DS + MB
    /// With special handling: line gap removed on last line in block
    pub fn full_height(&self) -> Num {
        let mut fh = self.margin_top + self.derived_spacing() + self.margin_bottom;

        // Per AXTE: "AXTE removes the line gap on the last line in a block
        // so that bottom-aligned text doesn't appear shifted up."
        if self.is_last_line {
            fh -= self.line_gap;
        }

        fh
    }

    /// Get baseline position from top of line
    /// Per AXTE:
    /// - When no line spacing override: B = MT + TH - D
    /// - When spacing override > TH: B = MT + SP - LG - D
    pub fn baseline_from_top(&self) -> Num {
        let th = self.text_height();

        if let Some(sp) = self.spacing_override
            && sp > Decimal::ZERO
            && sp - self.line_gap >= th
        {
            // Spacing override in effect and larger than text height
            return self.margin_top + sp - self.line_gap - self.descent;
        }

        // Default formula
        self.margin_top + th - self.descent
    }
}

/// Text measurement result for a complete text block
#[derive(Debug, Clone)]
pub struct TextBlockMetrics {
    /// All lines with their metrics
    pub lines: Vec<TextLine>,
    /// Total width (width of longest line)
    pub total_width: Num,
    /// Total height (sum of all line heights)
    pub total_height: Num,
}

impl TextBlockMetrics {
    /// Get the y-offset for the first line based on vertical alignment
    /// Per AXTE: Block-level first line offset calculation
    pub fn first_line_offset(&self, block_height: Num, v_align: crate::xfa::VAlign) -> Num {
        let total_height = self.total_height;

        // Per AXTE: "Under certain circumstances, AXTE may store lines whose total height
        // is greater than the block height. In such a case, the block is treated as being top-aligned."
        if total_height > block_height {
            return Decimal::ZERO;
        }

        match v_align {
            crate::xfa::VAlign::Top => Decimal::ZERO,
            crate::xfa::VAlign::Middle => (block_height - total_height) / num(2.0),
            crate::xfa::VAlign::Bottom => block_height - total_height,
        }
    }
}

/// Text measurement engine with font caching
/// Uses the font_manager module for font resolution according to XFA spec
pub struct TextMeasurer {
    /// Cached fonts by variant (using font_manager's static data)
    cached_fonts: HashMap<FontVariant, FontRef<'static>>,
    /// Cached font metrics by (variant hash, size)
    metrics_cache: HashMap<(u64, u32), FontMetrics>,
    /// Current font variant in use
    current_variant: Option<FontVariant>,
}

impl TextMeasurer {
    /// Create a new text measurer
    pub fn new() -> Self {
        TextMeasurer {
            cached_fonts: HashMap::new(),
            metrics_cache: HashMap::new(),
            current_variant: None,
        }
    }

    /// Get or load a font for the given XFA font style
    /// Per XFA spec: uses typeface, weight, and posture to select appropriate font
    pub fn get_font_for_style(&mut self, xfa_font: &Font) -> Result<&FontRef<'static>, String> {
        let variant = FontVariant::from_xfa_font(xfa_font);

        // Check if already cached
        if !self.cached_fonts.contains_key(&variant) {
            // Load through font_manager
            let manager = get_font_manager();
            let mut manager = manager.lock().map_err(|e| format!("Lock error: {}", e))?;
            let font = manager.get_font(xfa_font).map_err(|e| e.to_string())?;
            self.cached_fonts.insert(variant.clone(), font);
        }

        self.current_variant = Some(variant.clone());
        self.cached_fonts
            .get(&variant)
            .ok_or_else(|| "Font not in cache".to_string())
    }

    /// Load a font using default XFA settings (Courier, normal, normal)
    /// This is a compatibility method for code that doesn't specify font style
    pub fn load_font(&mut self) -> Result<(), String> {
        let default_font = Font::default();
        self.get_font_for_style(&default_font)?;
        Ok(())
    }

    /// Get the currently loaded font (or load default if none)
    /// Returns a clone of the font variant to avoid borrow issues
    fn get_current_font(&mut self) -> Result<FontRef<'static>, String> {
        // Check if we have a cached font for current variant
        if let Some(ref variant) = self.current_variant.clone()
            && let Some(font) = self.cached_fonts.get(variant)
        {
            return Ok(font.clone());
        }

        // Load default font and return a clone
        let default_font = Font::default();
        let font = self.get_font_for_style(&default_font)?;
        Ok(font.clone())
    }

    /// Get font metrics for a given font size and style (with caching)
    /// Per XFA spec: size defaults to 10pt
    pub fn get_metrics_for_style(&mut self, xfa_font: &Font) -> Result<FontMetrics, String> {
        let font = self.get_font_for_style(xfa_font)?.clone();
        let variant = FontVariant::from_xfa_font(xfa_font);

        // Create a unique cache key from variant and size
        let variant_hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            variant.hash(&mut hasher);
            hasher.finish()
        };
        let size_key = (xfa_font.size * num(100.0)).to_u32().unwrap_or(1000);
        let cache_key = (variant_hash, size_key);

        if let Some(metrics) = self.metrics_cache.get(&cache_key) {
            return Ok(metrics.clone());
        }

        let metrics = FontMetrics::from_font(&font, xfa_font.size);
        self.metrics_cache.insert(cache_key, metrics.clone());

        Ok(metrics)
    }

    /// Get font metrics for a given font size (with caching)
    /// Uses the current font variant or default
    pub fn get_metrics(&mut self, font_size: Num) -> Result<FontMetrics, String> {
        let font = self.get_current_font()?;

        // Use font size in hundredths of a point as cache key (with 0 for default variant)
        let cache_key = (0u64, (font_size * num(100.0)).to_u32().unwrap_or(1000));

        if let Some(metrics) = self.metrics_cache.get(&cache_key) {
            return Ok(metrics.clone());
        }

        let metrics = FontMetrics::from_font(&font, font_size);
        self.metrics_cache.insert(cache_key, metrics.clone());

        Ok(metrics)
    }

    /// Measure width of a single character with fallback
    /// Returns the horizontal advance for the glyph, or a fallback width
    fn measure_char_width(
        font: &FontRef<'_>,
        scaled_font: &ab_glyph::PxScaleFont<&FontRef<'_>>,
        ch: char,
        size_f32: f32,
    ) -> f32 {
        let glyph_id = font.glyph_id(ch);
        if glyph_id.0 != 0 {
            scaled_font.h_advance(glyph_id)
        } else {
            // Fallback for missing glyphs
            size_f32 * constants::MISSING_GLYPH_WIDTH_RATIO
        }
    }

    /// Get kerning adjustment between two characters
    /// Per XFA spec: kerning is only applied when kerningMode="pair"
    fn get_kerning(
        font: &FontRef<'_>,
        prev_char: char,
        curr_char: char,
        scale: PxScale,
        kerning_mode: KerningMode,
    ) -> f32 {
        if kerning_mode != KerningMode::Pair {
            return 0.0;
        }

        let prev_glyph = font.glyph_id(prev_char);
        let curr_glyph = font.glyph_id(curr_char);

        if prev_glyph.0 != 0 && curr_glyph.0 != 0 {
            font.as_scaled(scale).kern(prev_glyph, curr_glyph)
        } else {
            0.0
        }
    }

    /// Measure text width using specified font style
    /// Per XFA spec: applies letter spacing between characters and kerning if enabled
    pub fn measure_text_width_styled(
        &mut self,
        text: &str,
        xfa_font: &Font,
    ) -> Result<Num, String> {
        let font = self.get_font_for_style(xfa_font)?.clone();
        let size_f32 = xfa_font.size.to_f32().unwrap_or(10.0);

        // Apply horizontal scale if specified
        let h_scale = xfa_font
            .font_horizontal_scale
            .and_then(|s| s.to_f32())
            .map(|s| s / 100.0)
            .unwrap_or(1.0);

        let scale = xfa_px_scale(&font, size_f32);
        let scaled_font = font.as_scaled(scale);

        // Per XFA spec: letterSpacing affects inter-character spacing
        let letter_spacing = xfa_font
            .letter_spacing
            .and_then(|ls| ls.to_f32())
            .unwrap_or(0.0);

        let chars: Vec<char> = text.chars().collect();
        let mut width: f32 = 0.0;
        let mut prev_char: Option<char> = None;

        for (i, &ch) in chars.iter().enumerate() {
            // Add kerning from previous character if enabled
            if let Some(prev) = prev_char {
                width += Self::get_kerning(&font, prev, ch, scale, xfa_font.kerning_mode);
            }

            // Add character width
            let char_width = Self::measure_char_width(&font, &scaled_font, ch, size_f32);
            width += char_width * h_scale;

            // Add letter spacing between characters (not after last character)
            if i < chars.len() - 1 {
                width += letter_spacing;
            }

            prev_char = Some(ch);
        }

        Ok(num(width as f64))
    }

    /// Measure text width (backward compatible - uses current/default font)
    /// Note: For full XFA compliance (kerning, letter spacing), use measure_text_width_styled
    pub fn measure_text_width(&mut self, text: &str, font_size: Num) -> Result<Num, String> {
        let font = self.get_current_font()?;
        let size_f32 = font_size.to_f32().unwrap_or(10.0);
        let scale = xfa_px_scale(&font, size_f32);
        let scaled_font = font.as_scaled(scale);

        let mut width: f32 = 0.0;
        for ch in text.chars() {
            width += Self::measure_char_width(&font, &scaled_font, ch, size_f32);
        }

        Ok(num(width as f64))
    }

    /// Wrap text to fit within a maximum width using specified font style
    /// Per XFA spec: applies letter spacing and kerning during width calculation
    pub fn wrap_text_styled(
        &mut self,
        text: &str,
        max_width: Num,
        xfa_font: &Font,
    ) -> Result<Vec<String>, String> {
        self.wrap_text_styled_with_indent(text, max_width, xfa_font, 0.0)
    }

    /// Wrap text to fit within a maximum width using specified font style,
    /// with a first-line indent that reduces available width on the first line.
    /// Per XFA spec §2: first line starts at marginLeft + textIndent.
    pub fn wrap_text_styled_with_indent(
        &mut self,
        text: &str,
        max_width: Num,
        xfa_font: &Font,
        first_line_indent: f32,
    ) -> Result<Vec<String>, String> {
        let font = self.get_font_for_style(xfa_font)?.clone();
        let size_f32 = xfa_font.size.to_f32().unwrap_or(10.0);
        let letter_spacing = xfa_font
            .letter_spacing
            .and_then(|ls| ls.to_f32())
            .unwrap_or(0.0);
        let h_scale = xfa_font
            .font_horizontal_scale
            .and_then(|s| s.to_f32())
            .map(|s| s / 100.0)
            .unwrap_or(1.0);
        Self::wrap_text_internal(
            &font,
            text,
            max_width,
            size_f32,
            letter_spacing,
            h_scale,
            xfa_font.kerning_mode,
            first_line_indent,
        )
    }

    /// Wrap text to fit within a maximum width
    /// Returns a vector of lines
    pub fn wrap_text(
        &mut self,
        text: &str,
        max_width: Num,
        font_size: Num,
    ) -> Result<Vec<String>, String> {
        let font = self.get_current_font()?;
        let size_f32 = font_size.to_f32().unwrap_or(10.0);
        Self::wrap_text_internal(
            &font,
            text,
            max_width,
            size_f32,
            0.0,
            1.0,
            KerningMode::None,
            0.0,
        )
    }

    /// Internal text wrapping implementation
    /// Supports letter spacing, horizontal scale, and kerning per XFA spec.
    /// `first_line_indent` reduces available width on the first line (per XFA §2 textIndent).
    fn wrap_text_internal(
        font: &FontRef<'_>,
        text: &str,
        max_width: Num,
        size_f32: f32,
        letter_spacing: f32,
        h_scale: f32,
        kerning_mode: KerningMode,
        first_line_indent: f32,
    ) -> Result<Vec<String>, String> {
        let scale = xfa_px_scale(font, size_f32);
        let scaled_font = font.as_scaled(scale);
        let max_width_f32 = max_width.to_f32().unwrap_or(1000.0);

        let mut lines = Vec::new();
        let mut current_line = String::new();
        let mut current_width: f32 = 0.0;
        let space_glyph = font.glyph_id(' ');
        let space_width = if space_glyph.0 != 0 {
            scaled_font.h_advance(space_glyph) * h_scale
        } else {
            size_f32 * constants::SPACE_WIDTH_RATIO * h_scale
        };

        /// Measure word width with letter spacing and kerning
        fn measure_word_width(
            font: &FontRef<'_>,
            scaled_font: &ab_glyph::PxScaleFont<&FontRef<'_>>,
            scale: PxScale,
            word: &str,
            size_f32: f32,
            letter_spacing: f32,
            h_scale: f32,
            kerning_mode: KerningMode,
        ) -> f32 {
            let chars: Vec<char> = word.chars().collect();
            let mut width: f32 = 0.0;
            let mut prev_char: Option<char> = None;

            for (i, &ch) in chars.iter().enumerate() {
                // Add kerning from previous character if enabled
                if let Some(prev) = prev_char
                    && kerning_mode == KerningMode::Pair
                {
                    let prev_glyph = font.glyph_id(prev);
                    let curr_glyph = font.glyph_id(ch);
                    if prev_glyph.0 != 0 && curr_glyph.0 != 0 {
                        width += font.as_scaled(scale).kern(prev_glyph, curr_glyph);
                    }
                }

                // Add character width
                let glyph_id = font.glyph_id(ch);
                let char_width = if glyph_id.0 != 0 {
                    scaled_font.h_advance(glyph_id)
                } else {
                    size_f32 * constants::MISSING_GLYPH_WIDTH_RATIO
                };
                width += char_width * h_scale;

                // Add letter spacing between characters (not after last)
                if i < chars.len() - 1 {
                    width += letter_spacing;
                }

                prev_char = Some(ch);
            }
            width
        }

        let mut is_first_line = true;

        for word in text.split_whitespace() {
            // Measure word width with all XFA text properties
            let word_width = measure_word_width(
                font,
                &scaled_font,
                scale,
                word,
                size_f32,
                letter_spacing,
                h_scale,
                kerning_mode,
            );

            // Per XFA spec §2: first line effective width = max_width - textIndent
            let effective_max = if is_first_line {
                max_width_f32 - first_line_indent
            } else {
                max_width_f32
            };

            if current_line.is_empty() {
                // First word on line
                current_line = word.to_string();
                current_width = word_width;
            } else if current_width + space_width + word_width <= effective_max {
                // Word fits on current line
                current_line.push(' ');
                current_line.push_str(word);
                current_width += space_width + word_width;
            } else {
                // Word doesn't fit, start new line
                lines.push(current_line);
                current_line = word.to_string();
                current_width = word_width;
                is_first_line = false;
            }
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }

        if lines.is_empty() {
            lines.push(String::new());
        }

        Ok(lines)
    }

    /// Measure a text block with wrapping and full metrics
    /// This is the main entry point for text sizing
    /// Uses font style (typeface, weight, posture) per XFA spec
    pub fn measure_text_block(
        &mut self,
        text: &str,
        font: &Option<Font>,
        para: &Option<Para>,
        max_width: Num,
    ) -> Result<TextBlockMetrics, String> {
        // Get font style or use XFA defaults
        let xfa_font = font.clone().unwrap_or_default();
        let _font_size = xfa_font.size;

        // Load the appropriate font for this style
        self.get_font_for_style(&xfa_font)?;

        // Get paragraph margins
        let margin_top = para
            .as_ref()
            .and_then(|p| p.space_above)
            .unwrap_or(Decimal::ZERO);
        let margin_bottom = para
            .as_ref()
            .and_then(|p| p.space_below)
            .unwrap_or(Decimal::ZERO);
        let line_height_override = para.as_ref().and_then(|p| p.line_height);

        // Per XFA spec §2: textIndent adjusts the first line's left margin.
        // margin_left/margin_right reduce available width for all lines.
        let margin_left = para
            .as_ref()
            .and_then(|p| p.margin_left)
            .unwrap_or(Decimal::ZERO);
        let margin_right = para
            .as_ref()
            .and_then(|p| p.margin_right)
            .unwrap_or(Decimal::ZERO);
        let text_indent = para
            .as_ref()
            .and_then(|p| p.text_indent)
            .unwrap_or(Decimal::ZERO);
        let effective_width = max_width - margin_left - margin_right;
        let first_line_indent = text_indent.to_f32().unwrap_or(0.0);

        // Wrap text using the styled font, accounting for text-indent on first line
        let wrapped_lines =
            self.wrap_text_styled_with_indent(text, effective_width, &xfa_font, first_line_indent)?;
        let num_lines = wrapped_lines.len();

        // Get base font metrics for this style
        let base_metrics = self.get_metrics_for_style(&xfa_font)?;

        // Build text lines with metrics
        let mut lines = Vec::new();
        let mut total_height = Decimal::ZERO;
        let mut max_width_result = Decimal::ZERO;

        for (i, line_text) in wrapped_lines.into_iter().enumerate() {
            let is_first = i == 0;
            let is_last = i == num_lines - 1;

            // Measure line width using styled measurement
            let line_width = self.measure_text_width_styled(&line_text, &xfa_font)?;
            max_width_result = max_width_result.max(line_width);

            // Create line metrics
            let mut line_metrics = LineMetrics::new(is_first, is_last);
            line_metrics.accumulate(&base_metrics);
            line_metrics.spacing_override = line_height_override;

            // Apply paragraph margins only to first and last lines
            if is_first {
                line_metrics.margin_top = margin_top;
            }
            if is_last {
                line_metrics.margin_bottom = margin_bottom;
            }

            // Accumulate total height
            total_height += line_metrics.full_height();

            lines.push(TextLine {
                text: line_text,
                width: line_width,
                metrics: line_metrics,
            });
        }

        Ok(TextBlockMetrics {
            lines,
            total_width: max_width_result,
            total_height,
        })
    }

    /// Calculate the natural size of a text draw element
    /// Returns (width, height) in points
    pub fn calculate_draw_size(
        &mut self,
        text: &str,
        font: &Option<Font>,
        para: &Option<Para>,
        explicit_width: Option<Num>,
        explicit_height: Option<Num>,
    ) -> Result<(Num, Num), String> {
        // If both dimensions are explicit, return them
        if let (Some(w), Some(h)) = (explicit_width, explicit_height) {
            return Ok((w, h));
        }

        let _font_size = font.as_ref().map(|f| f.size).unwrap_or_else(|| num(10.0));

        // Use explicit width if provided, otherwise measure text for single-line width
        let max_width = explicit_width.unwrap_or_else(|| num(constants::SINGLE_LINE_MAX_WIDTH));

        let metrics = self.measure_text_block(text, font, para, max_width)?;

        let width = explicit_width.unwrap_or(metrics.total_width);
        let height = explicit_height.unwrap_or(metrics.total_height);

        Ok((width, height))
    }
}

impl Default for TextMeasurer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_gap_is_20_percent() {
        // Per AXTE spec: line gap is always 20% of font size
        let mut measurer = TextMeasurer::new();
        if measurer.load_font().is_ok() {
            let metrics = measurer.get_metrics(num(10.0)).unwrap();
            assert_eq!(metrics.line_gap, num(2.0)); // 20% of 10pt = 2pt

            let metrics = measurer.get_metrics(num(12.0)).unwrap();
            assert_eq!(metrics.line_gap, num(2.4)); // 20% of 12pt = 2.4pt
        }
    }

    /// Per AXTE (XFA 3.3 Appendix C, "Basic Metric Adjustments"), ascent and
    /// descent are scaled by `font_size / head.unitsPerEm`, the same
    /// denominator as every other metric and advance in the font -- not by
    /// ab_glyph's default `hhea.ascent - hhea.descent`. For a real font
    /// where those two denominators differ (true of both vendored faces:
    /// Frutiger 45 Light is 2308 vs `unitsPerEm` 2048, DejaVu Sans is 2384
    /// vs 2048), a correctly scaled 8pt `text_height()` is measurably more
    /// than 8pt, not exactly 8pt.
    #[test]
    fn text_height_is_scaled_by_the_fonts_own_units_per_em() {
        assert!(
            crate::fonts::test_support::ensure_registered(),
            "no fonts registered — see crates/u2s-xfa/src/fonts.rs test_support::font_dir"
        );
        let mut measurer = TextMeasurer::new();
        let font = Font {
            size: num(8.0),
            ..Font::default()
        };
        let metrics = measurer
            .get_metrics_for_style(&font)
            .expect("a registered font should measure");

        let text_height: f64 = metrics.text_height().to_string().parse().unwrap();
        assert!(
            text_height > 8.6,
            "8pt text height should be well above 8pt once ascent/descent are scaled \
             by unitsPerEm (Frutiger 45 Light: ~9.02pt, DejaVu Sans: ~9.31pt), got {text_height}"
        );
        assert!(
            text_height < 10.0,
            "8pt text height should still be close to 8pt, not inflated, got {text_height}"
        );
    }

    #[test]
    fn test_text_height() {
        let mut measurer = TextMeasurer::new();
        if measurer.load_font().is_ok() {
            let metrics = measurer.get_metrics(num(10.0)).unwrap();
            // Text height should be at least font size due to ascent padding
            assert!(metrics.text_height() >= num(10.0));
        }
    }

    #[test]
    fn test_line_metrics_full_height() {
        let mut line_metrics = LineMetrics::new(true, true);
        line_metrics.ascent = num(8.0);
        line_metrics.descent = num(2.0);
        line_metrics.line_gap = num(2.0);

        // Single line (first and last): full height should be TH (no line gap at end)
        // FH = MT + DS + MB - LG (on last line)
        // = 0 + (8+2+2) + 0 - 2 = 10
        assert_eq!(line_metrics.full_height(), num(10.0));
    }

    #[test]
    fn test_line_metrics_baseline() {
        let mut line_metrics = LineMetrics::new(true, true);
        line_metrics.ascent = num(8.0);
        line_metrics.descent = num(2.0);
        line_metrics.margin_top = num(0.0);

        // B = MT + TH - D = 0 + 10 - 2 = 8
        assert_eq!(line_metrics.baseline_from_top(), num(8.0));
    }

    #[test]
    fn test_baseline_shift_accumulation() {
        // Per AXTE: negative shift increases ascent, positive increases descent
        let font_metrics = FontMetrics {
            font_size: num(10.0),
            ascent: num(8.0),
            descent: num(2.0),
            line_gap: num(2.0),
            em_width: num(6.0),
            avg_char_width: num(5.0),
        };

        // Test negative (up) shift
        let mut line_metrics = LineMetrics::new(false, false);
        line_metrics.accumulate_with_baseline_shift(&font_metrics, Some(num(-2.0)));
        assert_eq!(line_metrics.ascent, num(10.0)); // 8 + |-2| = 10
        assert_eq!(line_metrics.descent, num(2.0)); // unchanged

        // Test positive (down) shift
        let mut line_metrics = LineMetrics::new(false, false);
        line_metrics.accumulate_with_baseline_shift(&font_metrics, Some(num(3.0)));
        assert_eq!(line_metrics.ascent, num(8.0)); // unchanged
        assert_eq!(line_metrics.descent, num(5.0)); // 2 + 3 = 5
    }

    #[test]
    fn test_ascent_overflow_first_line() {
        let mut line_metrics = LineMetrics::new(true, false); // first line
        line_metrics.ascent = num(8.0);
        line_metrics.descent = num(2.0);
        line_metrics.ascent_overflow = num(1.5);

        // Before applying overflow
        assert_eq!(line_metrics.text_height(), num(10.0));

        // Apply overflow (no spacing override, first line)
        line_metrics.apply_ascent_overflow();
        assert_eq!(line_metrics.ascent, num(9.5)); // 8 + 1.5
        assert_eq!(line_metrics.text_height(), num(11.5)); // 9.5 + 2
    }

    #[test]
    fn test_ascent_overflow_not_applied_with_spacing_override() {
        let mut line_metrics = LineMetrics::new(true, false);
        line_metrics.ascent = num(8.0);
        line_metrics.ascent_overflow = num(1.5);
        line_metrics.spacing_override = Some(num(14.0)); // override present

        line_metrics.apply_ascent_overflow();
        assert_eq!(line_metrics.ascent, num(8.0)); // unchanged due to override
    }
}
