//! Pulling the XFA XML out of a PDF: Catalog → AcroForm → /XFA.
//!
//! Ported from upstream `core/src/lib.rs`. One deliberate change, marked below:
//! the packetised `/XFA` array's packet **names** are preserved rather than
//! discarded, so a caller can address `template` / `datasets` / `config`
//! individually instead of receiving one concatenated blob.

use std::path::Path;

use crate::XfaError;
use crate::xfa::font_manager::EmbeddedFont;

/// One `/XFA` packet: its declared name and its decompressed bytes.
#[derive(Debug, Clone)]
pub struct XfaPacket {
    pub name: String,
    pub content: Vec<u8>,
}

/// Every packet of a document's XFA, in declaration order.
///
/// A single-stream `/XFA` yields one packet named `xdp` holding the whole
/// document; the array form yields one entry per declared packet.
pub fn extract_xfa_packets(pdf_bytes: &[u8]) -> Result<Option<Vec<XfaPacket>>, XfaError> {
    let doc =
        lopdf::Document::load_mem(pdf_bytes).map_err(|e| XfaError::PdfParse(e.to_string()))?;

    let catalog = doc
        .catalog()
        .map_err(|e| XfaError::PdfParse(e.to_string()))?;
    let Ok(acroform_ref) = catalog.get(b"AcroForm") else {
        return Ok(None);
    };
    let acroform = match acroform_ref {
        lopdf::Object::Dictionary(d) => d.clone(),
        lopdf::Object::Reference(r) => match doc.get_object(*r) {
            Ok(lopdf::Object::Dictionary(d)) => d.clone(),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    let Ok(xfa_obj) = acroform.get(b"XFA").cloned() else {
        return Ok(None);
    };

    // `decompress` is best-effort and errors on an already-uncompressed
    // stream, which is legitimate — so the result is ignored deliberately
    // rather than by omission. A genuinely corrupt stream surfaces later as an
    // XML parse failure, naming the offset.
    let single = |mut stream: lopdf::Stream| {
        let _ = stream.decompress();
        vec![XfaPacket {
            name: "xdp".to_string(),
            content: stream.content.clone(),
        }]
    };

    match xfa_obj {
        lopdf::Object::Stream(stream) => Ok(Some(single(stream))),
        lopdf::Object::Reference(r) => match doc.get_object(r).cloned() {
            Ok(lopdf::Object::Stream(stream)) => Ok(Some(single(stream))),
            _ => Ok(None),
        },
        lopdf::Object::Array(arr) => {
            // The array alternates ["name", stream, "name", stream, ...].
            // Upstream steps by 2 from index 1, taking only the streams and
            // dropping every name; keeping them is the difference between
            // returning 40 MB and returning the 200 KB that was asked for.
            let mut packets = Vec::new();
            for pair in arr.chunks(2) {
                let [name_obj, stream_obj] = pair else {
                    continue;
                };
                let name = match name_obj {
                    lopdf::Object::String(bytes, _) => String::from_utf8_lossy(bytes).into_owned(),
                    lopdf::Object::Name(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    _ => continue,
                };
                let resolved = match stream_obj {
                    lopdf::Object::Reference(r) => doc.get_object(*r).ok().cloned(),
                    lopdf::Object::Stream(_) => Some(stream_obj.clone()),
                    _ => None,
                };
                if let Some(lopdf::Object::Stream(mut stream)) = resolved {
                    let _ = stream.decompress(); // see the note above
                    packets.push(XfaPacket {
                        name,
                        content: stream.content.clone(),
                    });
                }
            }
            Ok((!packets.is_empty()).then_some(packets))
        }
        _ => Ok(None),
    }
}

/// The concatenation of every packet — what the parser consumes.
///
/// The result is generally *not* one well-formed XML document but a sequence of
/// packet fragments, which is why `XfaNode::parse` returns multiple roots.
pub fn extract_xfa_from_pdf_bytes(pdf_bytes: &[u8]) -> Result<Option<Vec<u8>>, XfaError> {
    Ok(extract_xfa_packets(pdf_bytes)?.map(|packets| {
        packets
            .into_iter()
            .flat_map(|p| p.content)
            .collect::<Vec<u8>>()
    }))
}

/// The fonts embedded in a PDF: every font descriptor's `FontFile2`
/// (TrueType) or `FontFile3` of subtype `OpenType`, under the descriptor's
/// `FontName` without its subset prefix. A `FontFile3` of another subtype
/// (a bare CFF program, `Type1C`/`CIDFontType0C`) is not an OpenType face
/// and is reported and left out; a `FontFile` (Type 1) likewise.
pub fn extract_embedded_fonts(pdf_bytes: &[u8]) -> Result<Vec<EmbeddedFont>, XfaError> {
    let doc = lopdf::Document::load_mem(pdf_bytes).map_err(|e| XfaError::PdfParse(e.to_string()))?;
    let mut fonts: Vec<EmbeddedFont> = Vec::new();
    for (_, object) in doc.objects.iter() {
        let Ok(dict) = object.as_dict() else {
            continue;
        };
        let is_descriptor = dict
            .get(b"Type")
            .ok()
            .and_then(|t| t.as_name().ok())
            .is_some_and(|t| t == b"FontDescriptor");
        if !is_descriptor {
            continue;
        }
        let name = dict
            .get(b"FontName")
            .ok()
            .and_then(|n| n.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let name = match name.split_once('+') {
            Some((prefix, rest)) if prefix.len() == 6 && prefix.chars().all(|c| c.is_ascii_uppercase()) => {
                rest.to_string()
            }
            _ => name,
        };
        let stream_of = |key: &[u8]| -> Option<lopdf::Stream> {
            match dict.get(key).ok()? {
                lopdf::Object::Reference(r) => doc.get_object(*r).ok()?.as_stream().ok().cloned(),
                lopdf::Object::Stream(s) => Some(s.clone()),
                _ => None,
            }
        };
        let stream = if let Some(s) = stream_of(b"FontFile2") {
            Some(s)
        } else if let Some(s) = stream_of(b"FontFile3") {
            let subtype = s
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|t| t.as_name().ok())
                .map(|t| String::from_utf8_lossy(t).into_owned())
                .unwrap_or_default();
            if subtype == "OpenType" {
                Some(s)
            } else {
                log::warn!("embedded font {name:?} is a {subtype} program, not an OpenType face; it is not used");
                None
            }
        } else {
            if dict.get(b"FontFile").is_ok() {
                log::warn!("embedded font {name:?} is a Type 1 program; it is not used");
            }
            None
        };
        let Some(mut stream) = stream else {
            continue;
        };
        let _ = stream.decompress();
        if fonts.iter().any(|f| f.name == name) {
            continue;
        }
        fonts.push(EmbeddedFont { name, data: stream.content.clone() });
    }
    Ok(fonts)
}

pub fn extract_xfa_from_pdf(path: impl AsRef<Path>) -> Result<Option<Vec<u8>>, XfaError> {
    extract_xfa_from_pdf_bytes(&std::fs::read(path)?)
}

#[cfg(test)]
mod embedded_font_tests {
    use super::*;
    use lopdf::{Dictionary, Object, Stream, dictionary};

    /// A PDF whose one font descriptor embeds DejaVu Sans as a subset
    /// TrueType program (`FontFile2`), the way a producer writes it.
    fn pdf_with_embedded(data: &[u8], key: &[u8], subtype: Option<&str>) -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.7");
        let mut stream_dict = Dictionary::new();
        if let Some(st) = subtype {
            stream_dict.set("Subtype", Object::Name(st.as_bytes().to_vec()));
        }
        let file = doc.add_object(Stream::new(stream_dict, data.to_vec()));
        let descriptor = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => Object::Name(b"ABCDEF+DejaVuSans".to_vec()),
            key => Object::Reference(file),
        });
        let font = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => Object::Name(b"ABCDEF+DejaVuSans".to_vec()),
            "FontDescriptor" => Object::Reference(descriptor),
        });
        let pages = doc.add_object(dictionary! { "Type" => "Pages", "Kids" => Object::Array(vec![]), "Count" => 0 });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages) });
        doc.trailer.set("Root", Object::Reference(catalog));
        let _ = font;
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("save");
        out
    }

    #[test]
    fn a_truetype_font_file_is_extracted_without_its_subset_prefix() {
        let data = std::fs::read(u2s_test_assets::fallback_font_dir().join("DejaVuSans.ttf")).expect("font");
        let pdf = pdf_with_embedded(&data, b"FontFile2", None);
        let fonts = extract_embedded_fonts(&pdf).expect("extract");
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].name, "DejaVuSans");
        assert_eq!(fonts[0].data.len(), data.len());
    }

    #[test]
    fn a_cff_program_is_left_out_and_an_opentype_one_is_taken() {
        let data = std::fs::read(u2s_test_assets::fallback_font_dir().join("DejaVuSans.ttf")).expect("font");
        let cff = pdf_with_embedded(b"not a face", b"FontFile3", Some("Type1C"));
        assert!(extract_embedded_fonts(&cff).expect("extract").is_empty());
        let otf = pdf_with_embedded(&data, b"FontFile3", Some("OpenType"));
        assert_eq!(extract_embedded_fonts(&otf).expect("extract").len(), 1);
    }
}
