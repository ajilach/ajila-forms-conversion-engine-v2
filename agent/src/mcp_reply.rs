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
    let structured = result.structured_content.as_ref().and_then(|value| {
        let repeated = result.content.iter().any(|content| match content {
            ContentBlock::Text(t) => serde_json::from_str::<Value>(&t.text).is_ok_and(|v| &v == value),
            _ => false,
        });
        (!repeated).then(|| {
            let mut value = value.clone();
            if let Some(blobs) = blobs {
                add_blob_paths(&mut value, blobs);
            }
            value.to_string()
        })
    });
    let blocks: Vec<ReplyBlock> = result
        .content
        .into_iter()
        .map(|content| match content {
            ContentBlock::Text(t) => ReplyBlock::Text(t.text),
            ContentBlock::Image(i) => ReplyBlock::Image {
                media_type: i.mime_type,
                data: i.data,
            },
            ContentBlock::ResourceLink(r) => ReplyBlock::Text(format!("[resource link: {}]", r.uri)),
            _ => ReplyBlock::Text("[non-text content omitted]".into()),
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
}
