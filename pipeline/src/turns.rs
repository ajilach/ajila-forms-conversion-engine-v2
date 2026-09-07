//! The one piece of wire-shape mapping `pipeline` still owns: MIME strings
//! onto rig's image media type enum.
//!
//! Everything else this module used to hold — the model call, tool-result
//! assembly, the invalid-tool-call callback — moved onto rig's own
//! `Agent::runner`; see [`crate::run::run_stage`] and [`crate::hooks`].

/// Map a MIME string onto rig's image media type. `None` for anything the enum
/// does not name — the provider then infers it from the payload rather than
/// being told something wrong.
pub(crate) fn media_media_type(mime: &str) -> Option<rig_core::message::ImageMediaType> {
    use rig_core::message::ImageMediaType as T;
    Some(match mime {
        "image/jpeg" | "image/jpg" => T::JPEG,
        "image/png" => T::PNG,
        "image/gif" => T::GIF,
        "image/webp" => T::WEBP,
        "image/heic" => T::HEIC,
        "image/heif" => T::HEIF,
        "image/svg+xml" => T::SVG,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unknown MIME type must not be asserted as a wrong one.
    #[test]
    fn an_unknown_image_type_is_left_for_the_provider_to_infer() {
        assert_eq!(media_media_type("image/tiff"), None);
        assert_eq!(
            media_media_type("image/jpeg"),
            Some(rig_core::message::ImageMediaType::JPEG)
        );
    }
}
