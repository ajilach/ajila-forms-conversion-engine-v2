//! Tidying a stage's history before it is sent: what the token window alone
//! does not do.
//!
//! [`crate::token_counter::RunnerContextBudget`] runs [`tidy`] before its
//! token window on every turn. It does what the window cannot:
//!
//! - **It drops results a later call superseded.** An `aem_verify_controls`
//!   listing is the whole form's control inventory (over 100,000 characters
//!   on a real form) and only the latest one describes the form as it stands;
//!   every earlier one is re-read on every later turn for nothing. Likewise a
//!   screenshot of a form that has since been closed (the Author rebuilds,
//!   closes and reopens) shows a build that no longer exists. Each is
//!   replaced by a one-line note saying what it was and what superseded it.
//!   The replacement is stable — a superseded result stays superseded — so
//!   it moves the provider's cached prefix once, not every turn.
//!
//! Images are not this module's business: `agent::image_fit` cuts an
//! oversized one where it enters a reply and again on every request.
//!
//! Pure functions over rig messages, unit-tested below.

use std::collections::HashMap;

use rig_core::message::{AssistantContent, Message, Text, ToolResultContent, UserContent};
#[cfg(test)]
use rig_core::message::Image;

/// Tools whose latest result supersedes every earlier one.
const LATEST_ONLY: &[&str] = &["aem_verify_controls"];
/// Tools whose result describes one open AEM form (`form` argument), and is
/// stale once another form is open.
const PER_FORM_IMAGES: &[&str] = &["aem_verify_screenshot"];

/// Replaces the results later calls superseded (see the module docs). Runs
/// on a stage's history every turn.
pub fn tidy(messages: Vec<Message>) -> Vec<Message> {
    let calls = tool_calls(&messages);
    let superseded = superseded_results(&messages, &calls);
    messages.into_iter().map(|message| replace_superseded(message, &superseded)).collect()
}

/// A tool call as the assistant made it: its tool and arguments.
struct Call {
    name: String,
    args: serde_json::Value,
}

/// Every tool call in `messages`, by call id, in order of appearance.
fn tool_calls(messages: &[Message]) -> Vec<(String, Call)> {
    let mut calls = Vec::new();
    for message in messages {
        if let Message::Assistant { content, .. } = message {
            for block in content.iter() {
                if let AssistantContent::ToolCall(call) = block {
                    calls.push((
                        call.id.as_str().to_owned(),
                        Call { name: call.function.name.clone(), args: call.function.arguments.clone() },
                    ));
                }
            }
        }
    }
    calls
}

/// The call ids whose results are superseded, with the note that replaces
/// each.
fn superseded_results(messages: &[Message], calls: &[(String, Call)]) -> HashMap<String, String> {
    let answered: std::collections::HashSet<String> = result_ids(messages);
    let mut superseded = HashMap::new();
    // Latest-only tools: every answered call but the last.
    for tool in LATEST_ONLY {
        let ids: Vec<&String> =
            calls.iter().filter(|(id, c)| c.name == *tool && answered.contains(id.as_str())).map(|(id, _)| id).collect();
        if let Some((_, older)) = ids.split_last() {
            for id in older {
                superseded.insert(
                    (*id).clone(),
                    format!("[{tool} result omitted: a later {tool} call superseded it; read that one]"),
                );
            }
        }
    }
    // Per-form images: stale once a later call names another form.
    let latest_form = calls
        .iter()
        .rev()
        .find_map(|(_, c)| c.name.starts_with("aem_verify_").then(|| c.args.get("form")?.as_str()).flatten());
    if let Some(latest) = latest_form {
        for (id, call) in calls {
            let form = call.args.get("form").and_then(serde_json::Value::as_str);
            if PER_FORM_IMAGES.contains(&call.name.as_str()) && form.is_some_and(|f| f != latest) {
                superseded.insert(
                    id.clone(),
                    format!(
                        "[{} images omitted: they show form {}, which has since been closed; the open form is {latest}]",
                        call.name,
                        form.unwrap_or_default()
                    ),
                );
            }
        }
    }
    superseded
}

/// The call ids that have a result in `messages`.
fn result_ids(messages: &[Message]) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for message in messages {
        if let Message::User { content } = message {
            for block in content.iter() {
                if let UserContent::ToolResult(result) = block {
                    ids.insert(result.call.as_str().to_owned());
                }
            }
        }
    }
    ids
}

/// `message` with the content of each superseded tool result replaced by its
/// note. A result that is already only that note is left as it is.
fn replace_superseded(message: Message, superseded: &HashMap<String, String>) -> Message {
    if superseded.is_empty() {
        return message;
    }
    match message {
        Message::User { content } => {
            let content = content
                .into_iter()
                .map(|block| match block {
                    UserContent::ToolResult(mut result) => {
                        if let Some(note) = superseded.get(result.call.as_str()) {
                            result.content = vec![ToolResultContent::Text(Text::from(note.as_str()))];
                        }
                        UserContent::ToolResult(result)
                    }
                    other => other,
                })
                .collect();
            Message::User { content }
        }
        other => other,
    }
}

#[cfg(test)]
/// How many images `messages` holds, in user content and tool results alike.
fn count_images(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| match message {
            Message::User { content } => content
                .iter()
                .map(|block| match block {
                    UserContent::Image(_) => 1,
                    UserContent::ToolResult(result) => {
                        result.content.iter().filter(|c| matches!(c, ToolResultContent::Image(_))).count()
                    }
                    _ => 0,
                })
                .sum(),
            Message::Assistant { content, .. } => {
                content.iter().filter(|c| matches!(c, AssistantContent::Image(_))).count()
            }
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
/// Every image in `message`, mutably.
fn images_mut(message: &mut Message) -> Vec<&mut Image> {
    let mut images = Vec::new();
    match message {
        Message::User { content } => {
            for block in content.iter_mut() {
                match block {
                    UserContent::Image(image) => images.push(image),
                    UserContent::ToolResult(result) => {
                        for inner in result.content.iter_mut() {
                            if let ToolResultContent::Image(image) = inner {
                                images.push(image);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Message::Assistant { content, .. } => {
            for block in content.iter_mut() {
                if let AssistantContent::Image(image) = block {
                    images.push(image);
                }
            }
        }
        _ => {}
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use rig_core::message::{DocumentSourceKind, ImageMediaType, ToolCall, ToolCallId, ToolFunction, ToolResult};
    use serde_json::json;

    fn call(id: &str, name: &str, args: serde_json::Value) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: ToolCallId::new(id).expect("an id"),
                provider: None,
                function: ToolFunction { name: name.into(), arguments: args },
                signature: None,
                additional_params: None,
            })],
        }
    }

    fn result(id: &str, content: Vec<ToolResultContent>) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: ToolCallId::new(id).expect("an id"),
                provider: None,
                name: String::new(),
                content,
            })],
        }
    }

    fn text(t: &str) -> ToolResultContent {
        ToolResultContent::text(t)
    }

    fn jpeg(width: u32, height: u32) -> ToolResultContent {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 50))
            .unwrap();
        ToolResultContent::image_base64(
            base64::engine::general_purpose::STANDARD.encode(&out),
            Some(ImageMediaType::JPEG),
            None,
        )
    }

    fn result_texts(message: &Message) -> Vec<String> {
        let Message::User { content } = message else { return vec![] };
        content
            .iter()
            .flat_map(|block| match block {
                UserContent::ToolResult(r) => r
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ToolResultContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            })
            .collect()
    }

    fn dims(message: &Message) -> Vec<(u32, u32)> {
        let mut message = message.clone();
        images_mut(&mut message)
            .into_iter()
            .filter_map(|img| match &img.data {
                DocumentSourceKind::Base64(b) => {
                    let bytes = base64::engine::general_purpose::STANDARD.decode(b).ok()?;
                    image::load_from_memory(&bytes).ok().map(|i| (i.width(), i.height()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_the_latest_controls_listing_is_kept() {
        let history = vec![
            call("c1", "aem_verify_controls", json!({"form": "f1", "revision": 0})),
            result("c1", vec![text("first listing")]),
            call("c2", "aem_verify_set", json!({"form": "f1"})),
            result("c2", vec![text("set")]),
            call("c3", "aem_verify_controls", json!({"form": "f1", "revision": 6})),
            result("c3", vec![text("second listing")]),
        ];
        let tidied = tidy(history);
        assert!(result_texts(&tidied[1])[0].contains("superseded"), "{:?}", result_texts(&tidied[1]));
        assert_eq!(result_texts(&tidied[3]), ["set"]);
        assert_eq!(result_texts(&tidied[5]), ["second listing"]);
        // Stable: tidying again changes nothing.
        assert_eq!(tidy(tidied.clone()), tidied);
    }

    #[test]
    fn a_controls_call_still_waiting_for_its_result_does_not_supersede() {
        let history = vec![
            call("c1", "aem_verify_controls", json!({"form": "f1"})),
            result("c1", vec![text("the listing")]),
            call("c2", "aem_verify_controls", json!({"form": "f1"})),
        ];
        let tidied = tidy(history);
        assert_eq!(result_texts(&tidied[1]), ["the listing"]);
    }

    #[test]
    fn screenshots_of_a_closed_form_are_dropped() {
        let history = vec![
            call("s1", "aem_verify_screenshot", json!({"form": "f1", "revision": 2})),
            result("s1", vec![jpeg(40, 40), text("{}")]),
            call("o2", "aem_verify_open", json!({})),
            result("o2", vec![text("{\"form\": \"f2\"}")]),
            call("s2", "aem_verify_screenshot", json!({"form": "f2", "revision": 0})),
            result("s2", vec![jpeg(40, 40)]),
        ];
        let tidied = tidy(history);
        assert_eq!(count_images(&tidied), 1);
        assert!(result_texts(&tidied[1])[0].contains("since been closed"));
        assert_eq!(dims(&tidied[5]), [(40, 40)]);
    }

}
