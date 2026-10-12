//! Converts an MCP tool result into the [`ToolReply`] the model sees. One
//! conversion for every in-process server: the vendored u2s ones and the
//! reference tools.

use std::path::Path;

use rmcp3::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use crate::conversion::{ReplyBlock, ToolReply};

/// Converts a u2s tool result into what the model sees. A server that pairs
/// its content with structured metadata (a verifier's whole report next to a
/// one-line summary, a render's warning next to the images) gets that
/// metadata appended as JSON, unless the text already is it; with a `blobs`
/// directory, every blob handle in it is given the `doc_path` the `pdf_*`
/// tools read it by (a server without blobs passes `None`).
pub fn reply_from_result(result: CallToolResult, blobs: Option<&Path>) -> ToolReply {
    // The structured content as the model sees it: with its blobs' paths.
    // A text block that repeats it (`CallToolResult::structured`) is shown
    // as this instead, so its artefacts are readable too.
    let original = result.structured_content.as_ref();
    let shown = original.map(|value| {
        let mut value = value.clone();
        if let Some(blobs) = blobs {
            add_blob_paths(&mut value, blobs);
        }
        value.to_string()
    });
    let is_repeat = |text: &str| original.is_some_and(|value| serde_json::from_str::<Value>(text).is_ok_and(|v| &v == value));
    let repeated = result.content.iter().any(|content| matches!(content, ContentBlock::Text(t) if is_repeat(&t.text)));
    let structured = shown.clone().filter(|_| !repeated);
    let blocks: Vec<ReplyBlock> = result
        .content
        .into_iter()
        .flat_map(|content| match content {
            ContentBlock::Text(t) if is_repeat(&t.text) => {
                vec![ReplyBlock::Text(shown.clone().expect("a repeat implies structured content"))]
            }
            ContentBlock::Text(t) => vec![ReplyBlock::Text(t.text)],
            ContentBlock::Image(i) => image_blocks(i.mime_type, i.data),
            ContentBlock::ResourceLink(r) => vec![ReplyBlock::Text(format!("[resource link: {}]", r.uri))],
            _ => vec![ReplyBlock::Text("[non-text content omitted]".into())],
        })
        .chain(structured.map(ReplyBlock::Text))
        .collect();
    let text = || {
        blocks
            .iter()
            .filter_map(|b| match b {
                ReplyBlock::Text(t) => Some(t.as_str()),
                ReplyBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if result.is_error == Some(true) {
        let message = text();
        return ToolReply::Error(if message.is_empty() {
            "the tool reported an error".into()
        } else {
            message
        });
    }
    if blocks.iter().all(|b| matches!(b, ReplyBlock::Text(_))) {
        ToolReply::Text(text())
    } else {
        ToolReply::Blocks(blocks)
    }
}

/// A tool's image as blocks the model API accepts: untouched when it fits,
/// otherwise a note and its parts (see [`crate::image_fit::fit`]).
fn image_blocks(media_type: String, data: String) -> Vec<ReplyBlock> {
    let Some(fitted) = crate::image_fit::fit(&data) else {
        return vec![ReplyBlock::Image { media_type, data }];
    };
    let part_type = fitted.media_type();
    let parts = fitted.parts.into_iter().map(|data| ReplyBlock::Image {
        media_type: part_type.into(),
        data,
    });
    std::iter::once(ReplyBlock::Text(fitted.note)).chain(parts).collect()
}

/// Gives every object naming a blob `handle` that exists in `blobs` the
/// path of that blob as `doc_path`.
fn add_blob_paths(value: &mut Value, blobs: &Path) {
    match value {
        Value::Object(object) => {
            let path = object
                .get("handle")
                .and_then(Value::as_str)
                .and_then(|handle| u2s_blob::BlobHandle::parse(handle).ok())
                .map(|handle| blobs.join(handle.to_string()))
                .filter(|path| path.is_file());
            for child in object.values_mut() {
                add_blob_paths(child, blobs);
            }
            if let Some(path) = path {
                object.insert("doc_path".into(), Value::String(path.display().to_string()));
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| add_blob_paths(item, blobs)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_their_order_and_errors_stay_errors() {
        let mixed = CallToolResult::success(vec![
            ContentBlock::text("first"),
            ContentBlock::image("aGVsbG8=", "image/png"),
            ContentBlock::text("last"),
        ]);
        match reply_from_result(mixed, None) {
            ToolReply::Blocks(blocks) => {
                assert!(matches!(&blocks[0], ReplyBlock::Text(t) if t == "first"));
                assert!(matches!(&blocks[1], ReplyBlock::Image { media_type, .. } if media_type == "image/png"));
                assert!(matches!(&blocks[2], ReplyBlock::Text(t) if t == "last"));
            }
            _ => panic!("text and images must stay blocks, in order"),
        }
        assert!(matches!(
            reply_from_result(CallToolResult::error(vec![ContentBlock::text("no such form")]), None),
            ToolReply::Error(e) if e == "no such form"
        ));
        assert!(matches!(
            reply_from_result(CallToolResult::error(vec![]), None),
            ToolReply::Error(e) if e == "the tool reported an error"
        ));
    }

    /// An image the model API would refuse reaches it as a note and its
    /// parts, in place; one it takes passes untouched.
    #[test]
    fn a_tool_image_reaches_the_model_in_parts_it_takes() {
        use crate::image_fit::testing::{decoded_dimensions, encoded, too_tall_screenshot};
        let result = CallToolResult::success(vec![
            ContentBlock::text("first"),
            ContentBlock::image(too_tall_screenshot(), "image/jpg"),
        ]);
        let ToolReply::Blocks(blocks) = reply_from_result(result, None) else {
            panic!("an image result stays blocks");
        };
        let [ReplyBlock::Text(first), ReplyBlock::Text(note), parts @ ..] = &blocks[..] else {
            panic!("expected the text, a note and the parts: {}", blocks.len());
        };
        assert_eq!(first, "first");
        assert!(note.contains("900x8079"), "{note}");
        let heights = parts.iter().map(|part| match part {
            ReplyBlock::Image { media_type, data } if media_type == "image/jpeg" => decoded_dimensions(data).1,
            _ => panic!("expected a JPEG part"),
        });
        assert_eq!(heights.sum::<u32>(), 8079);

        let small = encoded(image::DynamicImage::ImageLuma8(image::GrayImage::new(900, 1200)), image::ImageFormat::Png);
        let result = CallToolResult::success(vec![ContentBlock::image(small.clone(), "image/png")]);
        let ToolReply::Blocks(blocks) = reply_from_result(result, None) else {
            panic!("an image result stays blocks");
        };
        assert!(matches!(&blocks[..], [ReplyBlock::Image { data, .. }] if *data == small));
    }

    /// A verifier puts its whole report (findings, artefacts) in the
    /// structured content next to a one-line summary. The model must see it,
    /// and every artefact must name a `doc_path` the `pdf_*` tools accept.
    #[test]
    fn structured_reports_reach_the_model_with_readable_artefacts() {
        let blobs = tempfile::tempdir().unwrap();
        let handle = format!("{}.pdf", "ab".repeat(32));
        std::fs::write(blobs.path().join(&handle), b"%PDF-1.7").unwrap();
        let mut result = CallToolResult::success(vec![ContentBlock::text(
            "3 step(s), 1 artefact(s), 0 error(s), 1 warning(s), 900ms",
        )]);
        result.structured_content = Some(serde_json::json!({
            "findings": [{ "severity": "warning", "message": "Style not found: default.css" }],
            "artefacts": [{ "kind": "download", "label": "rendered (en)",
                            "blob": { "handle": handle, "media_type": "application/pdf" } }],
        }));
        let ToolReply::Text(text) = reply_from_result(result, Some(blobs.path())) else {
            panic!("a text-only result stays text");
        };
        assert!(text.starts_with("3 step(s)"), "{text}");
        assert!(text.contains("Style not found: default.css"), "{text}");
        let doc_path = blobs.path().join(&handle).display().to_string();
        assert!(text.contains(&format!("\"doc_path\":{}", serde_json::json!(doc_path))), "{text}");

        // A result whose text already is its structured content is not
        // repeated.
        let plain = CallToolResult::structured(serde_json::json!({ "closed": true }));
        let ToolReply::Text(text) = reply_from_result(plain, Some(blobs.path())) else {
            panic!("a text-only result stays text");
        };
        assert_eq!(text.matches("closed").count(), 1, "{text}");
    }

    /// `aem_verify_submit` replies with `CallToolResult::structured`, whose
    /// text is its structured content: the artefact it returns must still name
    /// the `doc_path` the `pdf_*` tools read it by.
    #[test]
    fn a_structured_only_result_names_its_artefacts_doc_path() {
        let blobs = tempfile::tempdir().unwrap();
        let handle = format!("{}.pdf", "cd".repeat(32));
        std::fs::write(blobs.path().join(&handle), b"%PDF-1.7").unwrap();
        let result = CallToolResult::structured(serde_json::json!({
            "artefacts": [{ "kind": "download", "label": "Document of Record",
                            "blob": { "handle": handle, "media_type": "application/pdf" } }],
        }));
        let ToolReply::Text(text) = reply_from_result(result, Some(blobs.path())) else {
            panic!("a text-only result stays text");
        };
        let doc_path = blobs.path().join(&handle).display().to_string();
        assert!(text.contains(&format!("\"doc_path\":{}", serde_json::json!(doc_path))), "{text}");
        assert_eq!(text.matches("Document of Record").count(), 1, "{text}");
    }
}
