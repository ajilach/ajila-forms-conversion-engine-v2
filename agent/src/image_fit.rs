//! Fits an image to what the model API accepts. One side longer than
//! [`MAX_IMAGE_SIDE`] fails the whole request, and with it every retry of the
//! turn and every resume of a session whose conversation holds it. A tool's
//! image is fitted where it enters a reply (`mcp_reply`), and every request is
//! fitted again on its way out (`runner::client`), which covers a conversation
//! recorded before this existed.

use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::{DynamicImage, GenericImageView as _, ImageFormat, ImageReader};

/// The longest side, in pixels, the model API accepts for an image.
pub const MAX_IMAGE_SIDE: u32 = 8000;

/// The JPEG quality a cut image is re-encoded at: the vision-payload quality
/// the u2s renderers and the verifier's screenshots use.
const JPEG_QUALITY: u8 = 82;

/// An oversized image, cut into parts the model API accepts.
#[derive(Debug)]
pub struct Fitted {
    /// Tells the model what it is looking at, for the text block in front of
    /// the parts.
    pub note: String,
    /// The parts, top to bottom, base64-encoded.
    pub parts: Vec<String>,
    /// Whether the parts are JPEG (otherwise PNG).
    pub jpeg: bool,
}

impl Fitted {
    /// The media type of every part.
    pub fn media_type(&self) -> &'static str {
        if self.jpeg { "image/jpeg" } else { "image/png" }
    }
}

/// `data` (base64) cut to fit, or `None` when it already fits. Only the
/// header is read to decide, so a request full of page renders stays cheap.
/// A larger image (a full-page screenshot of a long panel) is scaled down to
/// [`MAX_IMAGE_SIDE`] wide when wider, then cut into the fewest equal bands no
/// taller, each re-encoded as JPEG when `jpeg` and as PNG otherwise, so it
/// stays readable. An image whose size cannot be read (a format this build
/// does not decode) is `None` as well: no u2s tool emits one.
pub fn fit(data: &str, jpeg: bool) -> Option<Fitted> {
    let bytes = BASE64.decode(data).ok()?;
    let (width, height) = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if width <= MAX_IMAGE_SIDE && height <= MAX_IMAGE_SIDE {
        return None;
    }
    let image = image::load_from_memory(&bytes).ok()?;
    let bands = bands(image);
    let note = format!(
        "[a {width}x{height} px image, shown in {} part(s) top to bottom: the model takes at most {MAX_IMAGE_SIDE} px a side]",
        bands.len()
    );
    let parts = bands.into_iter().map(|band| BASE64.encode(encode(&band, jpeg))).collect();
    Some(Fitted { note, parts, jpeg })
}

fn encode(image: &DynamicImage, jpeg: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let encoded = if jpeg {
        image
            .to_rgb8()
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY))
    } else {
        image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
    };
    encoded.expect("encoding a decoded image into memory cannot fail");
    bytes
}

/// `image` scaled down to [`MAX_IMAGE_SIDE`] wide when wider, then cut into
/// the fewest equal horizontal bands that are each at most that tall.
fn bands(image: DynamicImage) -> Vec<DynamicImage> {
    let image = if image.width() > MAX_IMAGE_SIDE {
        let height = (u64::from(image.height()) * u64::from(MAX_IMAGE_SIDE)).div_ceil(u64::from(image.width()));
        let height = u32::try_from(height).expect("scaling down keeps the height in range").max(1);
        image.resize_exact(MAX_IMAGE_SIDE, height, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let (width, height) = image.dimensions();
    let parts = height.div_ceil(MAX_IMAGE_SIDE);
    let band = height.div_ceil(parts);
    (0..parts)
        .map(|part| part * band)
        .map(|top| image.crop_imm(0, top, width, band.min(height - top)))
        .collect()
}

/// Test images, shared with the callers' tests.
#[cfg(any(test, feature = "test-utils"))]
pub mod testing {
    use super::*;

    /// `image` encoded as `format`, in base64.
    pub fn encoded(image: DynamicImage, format: ImageFormat) -> String {
        let mut bytes = Vec::new();
        image.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
        BASE64.encode(bytes)
    }

    /// The 900x8079 px JPEG screenshot the model API refused, as base64.
    pub fn too_tall_screenshot() -> String {
        let tall = image::RgbImage::from_fn(900, 8079, |_, y| image::Rgb([(y % 256) as u8, 0, 0]));
        encoded(DynamicImage::ImageRgb8(tall), ImageFormat::Jpeg)
    }

    /// The width and height of a base64 image.
    pub fn dimensions(data: &str) -> (u32, u32) {
        image::load_from_memory(&BASE64.decode(data).unwrap()).unwrap().dimensions()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    /// A full-page screenshot of a long panel came back 900x8079 px, and the
    /// model API refused every retry of the turn for it. It must reach the
    /// model whole, in parts it accepts.
    #[test]
    fn an_image_taller_than_the_api_takes_is_cut_into_bands() {
        let fitted = fit(&too_tall_screenshot(), true).expect("too tall");
        assert_eq!(fitted.parts.len(), 2);
        assert_eq!(fitted.media_type(), "image/jpeg");
        let sizes: Vec<_> = fitted.parts.iter().map(|part| dimensions(part)).collect();
        assert!(sizes.iter().all(|&(w, h)| w == 900 && h <= MAX_IMAGE_SIDE), "{sizes:?}");
        assert_eq!(sizes.iter().map(|(_, h)| h).sum::<u32>(), 8079);
        assert!(fitted.note.contains("900x8079"), "{}", fitted.note);
    }

    #[test]
    fn an_image_wider_than_the_api_takes_is_scaled_then_cut() {
        let wide = encoded(DynamicImage::ImageLuma8(image::GrayImage::new(16_000, 20_000)), ImageFormat::Png);
        let fitted = fit(&wide, false).expect("too wide");
        assert_eq!(fitted.media_type(), "image/png");
        let sizes: Vec<_> = fitted.parts.iter().map(|part| dimensions(part)).collect();
        assert_eq!(sizes, vec![(MAX_IMAGE_SIDE, 5000), (MAX_IMAGE_SIDE, 5000)]);
    }

    #[test]
    fn an_image_the_api_takes_is_left_alone() {
        let fits = encoded(DynamicImage::ImageLuma8(image::GrayImage::new(900, MAX_IMAGE_SIDE)), ImageFormat::Png);
        assert!(fit(&fits, false).is_none());
        assert!(fit("not base64 at all", false).is_none());
    }
}
