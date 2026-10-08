//! `draw_text_mut`, vendored from `imageproc` 0.26.2 (`src/drawing/text.rs`,
//! `src/pixelops.rs`, `src/definitions.rs`).
//!
//! Why a copy instead of the `imageproc` dependency: this one function is
//! called from five places in `flattened.rs`, and `imageproc` costs ~50
//! transitive packages (`nalgebra`, `simba`, `rustfft`, `rustdct`,
//! `matrixmultiply`, `rand_distr`, `wide`, `pulp`, `safe_arch` and more). It
//! also depends on `image` with default features, and cargo unifies features
//! per package, so it would switch every codec on for every crate that links
//! `image`, including in workspaces that vendor this crate.
//!
//! This is a copy, not a reimplementation, so it is pixel-identical. It is
//! monomorphised for `RgbaImage`/`Rgba<u8>` -- the only combination any call
//! site uses -- which is what lets the generic `Canvas` trait, `weighted_sum`
//! and the `Clamp` trait go away. `Rgba<u8>::HAS_ALPHA` is true, so only
//! upstream's alpha branch is reachable and the `weighted_sum` branch is
//! dropped rather than translated.
//!
//! imageproc is MIT licensed; see LICENSE-imageproc alongside this file.

use ab_glyph::{Font, GlyphId, OutlinedGlyph, PxScale, Rect, ScaleFont, point};
use image::{Pixel, Rgba, RgbaImage};

/// `imageproc::definitions::Clamp<f32> for u8`.
///
/// Truncates rather than rounds, and the truncation is load-bearing for
/// producing the same pixels as upstream -- do not "fix" it to `.round()`.
#[inline]
fn clamp_u8(x: f32) -> u8 {
    if x < u8::MAX as f32 {
        if x > u8::MIN as f32 { x as u8 } else { u8::MIN }
    } else {
        u8::MAX
    }
}

/// `imageproc::drawing::text::layout_glyphs`, verbatim.
fn layout_glyphs(
    scale: impl Into<PxScale> + Copy,
    font: &impl Font,
    text: &str,
    mut f: impl FnMut(OutlinedGlyph, Rect),
) -> (u32, u32) {
    if text.is_empty() {
        return (0, 0);
    }
    let font = font.as_scaled(scale);

    let mut w = 0.0;
    let mut prev: Option<GlyphId> = None;

    for c in text.chars() {
        let glyph_id = font.glyph_id(c);
        let glyph = glyph_id.with_scale_and_position(scale, point(w, font.ascent()));
        w += font.h_advance(glyph_id);
        if let Some(g) = font.outline_glyph(glyph) {
            if let Some(prev) = prev {
                w += font.kern(glyph_id, prev);
            }
            prev = Some(glyph_id);
            let bb = g.px_bounds();
            f(g, bb);
        }
    }

    let w = w.ceil();
    let h = font.height().ceil();
    assert!(w >= 0.0);
    assert!(h >= 0.0);
    (1 + w as u32, h as u32)
}

/// `imageproc::drawing::text_size`. Unused by the engine today, kept because
/// it is the one-line companion to `layout_glyphs` and its absence would look
/// like an omission rather than a choice.
#[allow(dead_code)]
pub fn text_size(scale: impl Into<PxScale> + Copy, font: &impl Font, text: &str) -> (u32, u32) {
    layout_glyphs(scale, font, text, |_, _| {})
}

/// `imageproc::drawing::draw_text_mut`, monomorphised for `RgbaImage`.
///
/// Does not support newlines; the caller splits lines itself.
pub fn draw_text_mut(
    canvas: &mut RgbaImage,
    color: Rgba<u8>,
    x: i32,
    y: i32,
    scale: impl Into<PxScale> + Copy,
    font: &impl Font,
    text: &str,
) {
    let image_width = canvas.width() as i32;
    let image_height = canvas.height() as i32;

    layout_glyphs(scale, font, text, |g, bb| {
        let x_shift = x + bb.min.x.round() as i32;
        let y_shift = y + bb.min.y.round() as i32;
        g.draw(|gx, gy, gv| {
            let image_x = gx as i32 + x_shift;
            let image_y = gy as i32 + y_shift;

            if (0..image_width).contains(&image_x) && (0..image_height).contains(&image_y) {
                let image_x = image_x as u32;
                let image_y = image_y as u32;
                let mut pixel = *canvas.get_pixel(image_x, image_y);
                let gv = gv.clamp(0.0, 1.0);
                // Rgba<u8> HAS_ALPHA, so this is upstream's alpha branch.
                let color = color.map_with_alpha(|f| f, |a| clamp_u8(f32::from(a) * gv));
                pixel.blend(&color);
                canvas.put_pixel(image_x, image_y, pixel);
            }
        })
    });
}
