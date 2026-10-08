//! RGBA → encoded bytes. JPEG drops alpha (rendered pages are opaque, and the
//! vision APIs the images feed have no use for it); PNG keeps it.

use image::RgbaImage;

use crate::error::RenderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
}

impl ImageFormat {
    pub fn mime(&self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Png => "png",
        }
    }

    /// Parse a caller-supplied format string; `None` input means the default
    /// (JPEG — the cheap-in-context choice).
    pub fn parse(s: Option<&str>) -> Result<Self, RenderError> {
        match s {
            None | Some("jpeg") | Some("jpg") => Ok(ImageFormat::Jpeg),
            Some("png") => Ok(ImageFormat::Png),
            Some(other) => Err(RenderError::backend(
                "encode",
                format!("unknown image format {other:?} — use \"jpeg\" or \"png\""),
            )),
        }
    }
}

pub fn encode(
    img: &RgbaImage,
    format: ImageFormat,
    jpeg_quality: u8,
) -> Result<Vec<u8>, RenderError> {
    let mut out = Vec::new();
    match format {
        ImageFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            let encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, jpeg_quality);
            rgb.write_with_encoder(encoder)
                .map_err(|e| RenderError::backend("encode", format!("jpeg encode: {e}")))?;
        }
        ImageFormat::Png => {
            let encoder = image::codecs::png::PngEncoder::new(&mut out);
            img.write_with_encoder(encoder)
                .map_err(|e| RenderError::backend("encode", format!("png encode: {e}")))?;
        }
    }
    Ok(out)
}
