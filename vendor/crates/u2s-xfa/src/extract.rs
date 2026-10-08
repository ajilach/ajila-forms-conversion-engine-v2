//! Pulling the XFA XML out of a PDF: Catalog → AcroForm → /XFA.
//!
//! Ported from upstream `core/src/lib.rs`. One deliberate change, marked below:
//! the packetised `/XFA` array's packet **names** are preserved rather than
//! discarded, so a caller can address `template` / `datasets` / `config`
//! individually instead of receiving one concatenated blob.

use std::path::Path;

use crate::XfaError;

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

pub fn extract_xfa_from_pdf(path: impl AsRef<Path>) -> Result<Option<Vec<u8>>, XfaError> {
    extract_xfa_from_pdf_bytes(&std::fs::read(path)?)
}
