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

/// The longest side, in pixels, the model API accepts for an image in a
/// request that carries many images, which a stage's history does after its
/// first renders (a lone image may have 8000 px a side).
pub const MAX_IMAGE_SIDE: u32 = 3000;

/// The longest side of a part an oversized image is cut into: the API scales
/// every image down to about 1.15 megapixels, so a taller part would arrive
/// too small to read.
pub const PART_SIDE: u32 = 2000;

/// How much of the base64 [`dimensions`] decodes first: enough for a PNG's or
/// a JPEG's header, a small share of a page render.
const HEADER_CHARS: usize = 64 * 1024;

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
    /// Whether the parts are JPEG, as the image was (otherwise PNG).
    pub jpeg: bool,
}

impl Fitted {
    /// The media type of every part.
    pub fn media_type(&self) -> &'static str {
        if self.jpeg { "image/jpeg" } else { "image/png" }
    }
}

/// The width and height of a base64 image, read from its header: only the
/// first [`HEADER_CHARS`] are decoded unless the header lies further in.
/// `None` when it is not base64 or not an image this build reads.
pub fn dimensions(data: &str) -> Option<(u32, u32)> {
    let probe = |bytes: &[u8]| {
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()
    };
    let head = &data.as_bytes()[..data.len().min(HEADER_CHARS) / 4 * 4];
    BASE64
        .decode(head)
        .ok()
        .and_then(|bytes| probe(&bytes))
        .or_else(|| probe(&BASE64.decode(data).ok()?))
}

/// `data` (base64) cut to fit, or `None` when it already fits, which only its
/// header is read to decide. A side over [`MAX_IMAGE_SIDE`] (a full-page
/// screenshot of a long panel) gets the image scaled down to [`PART_SIDE`]
/// wide when wider, then cut into the fewest equal bands no taller, each
/// re-encoded in the image's own format (JPEG stays JPEG, anything else
/// becomes PNG), so it stays readable. An image whose size cannot be read (a
/// format this build does not decode) is `None` as well: no u2s tool emits one.
pub fn fit(data: &str) -> Option<Fitted> {
    let (width, height) = dimensions(data)?;
    if width <= MAX_IMAGE_SIDE && height <= MAX_IMAGE_SIDE {
        return None;
    }
    let bytes = BASE64.decode(data).ok()?;
    let jpeg = image::guess_format(&bytes).ok() == Some(ImageFormat::Jpeg);
    let bands = bands(image::load_from_memory(&bytes).ok()?);
    let scaled = if width > PART_SIDE { format!(", scaled down to {PART_SIDE} px wide") } else { String::new() };
    let note = format!(
        "[a {width}x{height} px image, too large for the model in one piece: shown{scaled} in {} part(s) top to bottom]",
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

/// `image` scaled down to [`PART_SIDE`] wide when wider, then cut into the
/// fewest equal horizontal bands that are each at most that tall.
fn bands(image: DynamicImage) -> Vec<DynamicImage> {
    let image = if image.width() > PART_SIDE {
        let height = (u64::from(image.height()) * u64::from(PART_SIDE)).div_ceil(u64::from(image.width()));
        let height = u32::try_from(height).expect("scaling down keeps the height in range").max(1);
        image.resize_exact(PART_SIDE, height, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let (width, height) = image.dimensions();
    let parts = height.div_ceil(PART_SIDE);
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

    /// The width and height of a base64 image, decoded in full.
    pub fn decoded_dimensions(data: &str) -> (u32, u32) {
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
        let fitted = fit(&too_tall_screenshot()).expect("too tall");
        assert_eq!(fitted.media_type(), "image/jpeg");
        let sizes: Vec<_> = fitted.parts.iter().map(|part| decoded_dimensions(part)).collect();
        assert_eq!(sizes.len(), 5, "{sizes:?}");
        assert!(sizes.iter().all(|&(w, h)| w == 900 && h <= PART_SIDE), "{sizes:?}");
        assert_eq!(sizes.iter().map(|(_, h)| h).sum::<u32>(), 8079);
        assert!(fitted.note.contains("900x8079") && !fitted.note.contains("scaled"), "{}", fitted.note);
    }

    /// The parts keep the format the image is in, whatever it was labelled.
    #[test]
    fn an_image_wider_than_the_api_takes_is_scaled_then_cut_as_png() {
        let wide = encoded(DynamicImage::ImageLuma8(image::GrayImage::new(8001, 4001)), ImageFormat::Png);
        let fitted = fit(&wide).expect("too wide");
        assert_eq!(fitted.media_type(), "image/png");
        let sizes: Vec<_> = fitted.parts.iter().map(|part| decoded_dimensions(part)).collect();
        assert_eq!(sizes, vec![(PART_SIDE, 1001)]);
        assert!(fitted.note.contains("scaled down"), "{}", fitted.note);
    }

    /// A 900x4000 screenshot fails a request that already carries many images
    /// ("max allowed size for many-image requests: 3000 pixels").
    #[test]
    fn an_image_over_the_many_image_limit_is_cut_too() {
        let tall = encoded(DynamicImage::ImageLuma8(image::GrayImage::new(900, 4000)), ImageFormat::Png);
        let fitted = fit(&tall).expect("over the many-image limit");
        let sizes: Vec<_> = fitted.parts.iter().map(|part| decoded_dimensions(part)).collect();
        assert_eq!(sizes, vec![(900, 2000), (900, 2000)]);
    }

    #[test]
    fn an_image_the_api_takes_is_left_alone() {
        let fits = encoded(DynamicImage::ImageLuma8(image::GrayImage::new(900, MAX_IMAGE_SIDE)), ImageFormat::Png);
        assert!(fit(&fits).is_none());
        assert!(fit("not base64 at all").is_none());
    }

    /// The header is enough to size an image, and a payload shorter than the
    /// probe is read whole.
    #[test]
    fn dimensions_come_from_the_header() {
        assert_eq!(dimensions(&too_tall_screenshot()), Some((900, 8079)));
        let small = encoded(DynamicImage::ImageLuma8(image::GrayImage::new(3, 2)), ImageFormat::Png);
        assert_eq!(dimensions(&small), Some((3, 2)));
        assert_eq!(dimensions("####"), None);
    }
}
