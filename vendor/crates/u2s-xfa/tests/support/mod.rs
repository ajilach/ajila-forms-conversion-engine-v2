//! Helpers shared by the engine's integration tests.

use u2s_xfa::Flattened;

/// Count the dark pixels in a rectangle of the whole rendered column, in
/// points (the render is at 1 pixel per point).
pub fn dark_pixels(flat: &Flattened, x: f64, y: f64, w: f64, h: f64) -> usize {
    let img = flat.render_to_image_buffer_plain(1.0).expect("render");
    dark_pixels_in(&img, x, y, w, h)
}

/// As [`dark_pixels`], on an image already rendered at 1 pixel per point, so
/// a test measuring several regions renders once.
pub fn dark_pixels_in(img: &image::RgbaImage, x: f64, y: f64, w: f64, h: f64) -> usize {
    let (x0, y0) = (x.max(0.0) as u32, y.max(0.0) as u32);
    let (x1, y1) = (
        ((x + w) as u32).min(img.width()),
        ((y + h) as u32).min(img.height()),
    );
    let mut dark = 0;
    for py in y0..y1 {
        for px in x0..x1 {
            let p = img.get_pixel(px, py).0;
            if p[0] < 200 && p[1] < 200 && p[2] < 200 {
                dark += 1;
            }
        }
    }
    dark
}
