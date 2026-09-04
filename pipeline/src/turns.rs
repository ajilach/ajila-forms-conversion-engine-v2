//! The LLM seam. The controller drives model calls through [`TurnProvider`] and
//! never names a provider, a model, or an API key.
//!
//! What crosses the seam is rig's provider-neutral message model, not one
//! vendor's wire shape: the controller assembles no JSON and the transport does
//! no dialect translation. Note what the trait does *not* take: `max_tokens` and
//! the model id. Those are provider knowledge, so they live inside the
//! implementation — which is why the controller carries no model tables.

use std::future::Future;

use rig_agent::agent::run::streamed::StreamedTurn;
use rig_core::completion::{ToolDefinition, Usage};
use rig_core::message::{Message, ToolResultContent, UserContent};

use crate::observer::AbortFlag;

/// What one streamed model call produced.
///
/// The assembled [`StreamedTurn`] goes straight back into the run state
/// machine; the rest is what the observer reports.
pub struct ModelReply {
    /// The turn, ready for `AgentRun::streamed_turn`.
    pub turn: StreamedTurn,
    /// Token usage the provider reported for this call.
    pub usage: Usage,
    /// The model's visible text for the turn.
    pub text: String,
    /// Real prompt-token count the API billed for this request — i.e. how full
    /// the context window was. 0 if the API didn't report usage.
    pub prompt_tokens: usize,
}

/// Runs one model call.
///
/// Implementations own the transport, the credentials, the model choice, and
/// the context budget: the controller hands over the prompt plus the history
/// the run has accumulated, and gets an assembled turn back. Eviction happens
/// on the copy handed over, so what the provider sees may be shorter than what
/// the run remembers.
pub trait TurnProvider {
    fn call_model(
        &self,
        prompt: Message,
        history: Vec<Message>,
        tools: &[ToolDefinition],
        system: &str,
        abort: &AbortFlag,
    ) -> impl Future<Output = Result<ModelReply, String>>;
}

/// Convert the agent's tool catalog (`{name, description, input_schema}`) into
/// the shape a model is offered.
///
/// The catalog stays the single place a tool is declared and scoped; this is
/// only the wire shape it is presented in.
pub fn tool_definitions(specs: &[serde_json::Value]) -> Vec<ToolDefinition> {
    specs
        .iter()
        .map(|spec| ToolDefinition {
            name: spec["name"].as_str().unwrap_or_default().to_string(),
            description: spec["description"].as_str().unwrap_or_default().to_string(),
            parameters: spec["input_schema"].clone(),
        })
        .collect()
}

/// Build the tool-result content for one executed call.
///
/// rig's `ToolResult` carries no `is_error` flag, so a failure is marked the way
/// the OpenAI-compatible path has always marked it: an `Error:` prefix on the
/// text. That is the whole signal the model gets, so the wording is load-bearing.
pub fn tool_result_content(reply: agent::ToolReply) -> Vec<ToolResultContent> {
    use agent::{ReplyBlock, ToolReply};

    fn image(media_type: &str, data: &str) -> ToolResultContent {
        ToolResultContent::image_base64(data, media_media_type(media_type), None)
    }

    match reply {
        ToolReply::Text(text) => vec![ToolResultContent::text(text)],
        ToolReply::Error(msg) => vec![ToolResultContent::text(format!("Error: {msg}"))],
        ToolReply::Image { media_type, images } => images
            .iter()
            .map(|b64| image(media_type, b64))
            .collect(),
        ToolReply::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| match block {
                ReplyBlock::Text(text) => ToolResultContent::text(text),
                ReplyBlock::Image { media_type, data } => image(&media_type, &data),
            })
            .collect(),
    }
}

/// Map a MIME string onto rig's image media type. `None` for anything the enum
/// does not name — the provider then infers it from the payload rather than
/// being told something wrong.
fn media_media_type(mime: &str) -> Option<rig_core::message::ImageMediaType> {
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

/// Build the `user` content carrying a batch of tool results.
pub fn tool_results(results: Vec<(rig_core::message::ToolCallId, String, agent::ToolReply)>) -> Vec<UserContent> {
    results
        .into_iter()
        .map(|(id, name, reply)| UserContent::tool_result(id, name, tool_result_content(reply)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::{ReplyBlock, ToolReply};

    /// A browser result interleaves a snapshot with a screenshot; the order and
    /// the per-image media type must survive into the API message.
    #[test]
    fn mixed_blocks_become_ordered_text_and_image_content() {
        let content = tool_result_content(ToolReply::Blocks(vec![
            ReplyBlock::Text("snapshot".into()),
            ReplyBlock::Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            },
            ReplyBlock::Text("done".into()),
        ]));

        assert_eq!(content.len(), 3);
        assert!(matches!(&content[0], ToolResultContent::Text(t) if t.text == "snapshot"));
        match &content[1] {
            ToolResultContent::Image(image) => {
                assert_eq!(image.media_type, Some(rig_core::message::ImageMediaType::PNG));
                assert_eq!(
                    image.data,
                    rig_core::message::DocumentSourceKind::Base64("AAAA".into())
                );
            }
            other => panic!("expected an image block, got {other:?}"),
        }
        assert!(matches!(&content[2], ToolResultContent::Text(t) if t.text == "done"));
    }

    /// rig's tool result has no `is_error` flag, so the prefix is the only
    /// signal the model gets that a call failed. Pin it.
    #[test]
    fn a_failed_tool_is_marked_in_the_text() {
        let content = tool_result_content(ToolReply::Error("no such state".into()));
        assert!(matches!(
            &content[0],
            ToolResultContent::Text(t) if t.text == "Error: no such state"
        ));
    }

    /// The catalog's shape is `input_schema`; a model is offered `parameters`.
    /// A silent mismatch here would send every tool with an empty schema.
    #[test]
    fn catalog_specs_become_tool_definitions() {
        let specs = vec![serde_json::json!({
            "name": "get_xfa",
            "description": "Read the XFA.",
            "input_schema": {"type": "object", "properties": {"state": {"type": "string"}}},
        })];
        let defs = tool_definitions(&specs);
        assert_eq!(defs[0].name, "get_xfa");
        assert_eq!(defs[0].description, "Read the XFA.");
        assert_eq!(defs[0].parameters["type"], "object");
        assert!(defs[0].parameters["properties"]["state"].is_object());
    }

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
