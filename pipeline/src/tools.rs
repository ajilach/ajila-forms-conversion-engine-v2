//! Bridges the agent's own tool catalog onto rig's `DynamicTool`.
//!
//! `ConversionAgent::execute` is a `&mut self` async dispatcher keyed by name —
//! the catalog is data (`{name, description, input_schema}` specs from
//! `ConversionAgent::tools_for_stage`), not a set of typed `rig_agent::Tool`
//! impls. rig's typed `Tool` trait needs a `const NAME`, which a
//! name-dispatched catalog cannot supply, and the object-safe erased dispatch
//! it uses internally is deliberately `pub(crate)` to rig — `DynamicTool` is
//! the one door meant for exactly this shape.
//!
//! Its callback is `Fn + Send + Sync + 'static`, so it cannot hold `&mut
//! ConversionAgent` directly; [`SharedAgent`] is what makes that legal without
//! reintroducing a second copy of the working tree — every stage's tools
//! dispatch against the one agent a run drives, cloning the `Arc` (which `Fn`
//! permits) and locking it only for the call's duration. The lock is never
//! contended: tool calls run at rig's default `tool_concurrency(1)`, so one
//! call finishes before the next starts, which is also what keeps the
//! activity timeline in call order.

use std::sync::Arc;

use agent::{ConversionAgent, ReplyBlock, ToolReply};
use rig_agent::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use rig_core::message::ToolResultContent;
use tokio::sync::Mutex;

use crate::turns::media_media_type;

/// The one `ConversionAgent` a run drives, shared across every stage's tools
/// and the stage driver itself.
pub type SharedAgent = Arc<Mutex<ConversionAgent>>;

/// Build the tools rig offers one stage, from the agent's own catalog specs.
///
/// The catalog stays the single place a tool is declared and scoped
/// (`ConversionAgent::tools_for_stage`, which is what `specs` already is);
/// this only wraps each spec in the erased dispatch rig's runner needs. A spec
/// missing a name is dropped rather than panicking — the catalog's own tests
/// are what guarantee every entry has one.
pub fn dynamic_tools_for(agent: &SharedAgent, specs: &[serde_json::Value]) -> Vec<DynamicTool> {
    specs
        .iter()
        .filter_map(|spec| dynamic_tool_from(agent, spec))
        .collect()
}

fn dynamic_tool_from(agent: &SharedAgent, spec: &serde_json::Value) -> Option<DynamicTool> {
    let name = spec["name"].as_str()?.to_string();
    let description = spec["description"].as_str().unwrap_or_default().to_string();
    let parameters = spec["input_schema"].clone();
    let agent = agent.clone();
    let dispatch_name = name.clone();

    Some(DynamicTool::new(
        name,
        description,
        parameters,
        move |_ctx, args| {
            let agent = agent.clone();
            let name = dispatch_name.clone();
            Box::pin(async move {
                let mut agent = agent.lock().await;
                let reply = agent.execute(&name, &args).await;
                reply_to_tool_output(reply)
            })
        },
    ))
}

/// Convert the agent's own reply shape into rig's canonical tool output.
///
/// An error reply is not a Rust-level failure here, on purpose: today's model
/// sees `Error: <msg>` as ordinary tool-result text and carries on, and that
/// is what a tool-authored [`ToolExecutionError`] reproduces —
/// `ToolExecutionError::new`'s `model_output` is the diagnostic message
/// itself unless overridden, so the model's view is unchanged. What *does*
/// differ from today is purely internal bookkeeping (`ToolResult::is_error`),
/// which is exactly the hook-visible signal `on_tool_result` needs for the
/// `ToolFinished.ok` flag.
fn reply_to_tool_output(reply: ToolReply) -> Result<ToolOutput, ToolExecutionError> {
    match reply {
        ToolReply::Text(text) => Ok(ToolOutput::text(text)),
        ToolReply::Error(msg) => Err(ToolExecutionError::other(format!("Error: {msg}"))),
        ToolReply::Blocks(blocks) => {
            let content: Vec<ToolResultContent> = blocks
                .into_iter()
                .map(|block| match block {
                    ReplyBlock::Text(text) => ToolResultContent::text(text),
                    ReplyBlock::Image { media_type, data } => image_content(&media_type, &data),
                })
                .collect();
            ToolOutput::content(content)
        }
    }
}

fn image_content(media_type: &str, data: &str) -> ToolResultContent {
    ToolResultContent::image_base64(data, media_media_type(media_type), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::OutputTarget;

    fn bare_agent() -> SharedAgent {
        Arc::new(Mutex::new(ConversionAgent::new(None, Vec::new(), "test-tools-bridge".into(), OutputTarget::Redacto)
            .expect("an agent without sources starts")))
    }

    /// A text reply becomes plain text output — the common case, and the one
    /// every tool call that isn't an image or an error takes.
    #[tokio::test]
    async fn a_text_reply_becomes_text_output() {
        let output = reply_to_tool_output(ToolReply::Text("hello".into())).unwrap();
        assert_eq!(output.as_text(), Some("hello"));
    }

    /// An error reply is not a Rust-level failure: the model still has to see
    /// exactly the wording it sees today, `Error: <msg>`, as the tool's
    /// result text — not a bare `<msg>` with the failure signalled only out of
    /// band, which would be a silent prompt-surface change.
    #[tokio::test]
    async fn an_error_reply_carries_the_same_wording_the_model_saw_before() {
        let err = reply_to_tool_output(ToolReply::Error("no such state".into()))
            .expect_err("an error reply must be a Rust-level error");
        assert_eq!(err.model_feedback(), Some("Error: no such state"));
    }

    /// Every image in a multi-image reply must reach the model — a page-image
    /// tool call.
    #[tokio::test]
    async fn every_image_in_a_reply_survives() {
        let output = reply_to_tool_output(ToolReply::Blocks(vec![
            ReplyBlock::Image {
                media_type: "image/jpeg".into(),
                data: "aaaa".into(),
            },
            ReplyBlock::Image {
                media_type: "image/jpeg".into(),
                data: "bbbb".into(),
            },
        ]))
        .unwrap();
        assert_eq!(output.as_content().len(), 2);
    }

    /// A browser result interleaves text and images in order — a snapshot
    /// next to a screenshot — and the order has to survive.
    #[tokio::test]
    async fn mixed_blocks_stay_ordered() {
        let output = reply_to_tool_output(ToolReply::Blocks(vec![
            ReplyBlock::Text("snapshot".into()),
            ReplyBlock::Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            },
            ReplyBlock::Text("done".into()),
        ]))
        .unwrap();
        let content = output.into_content();
        assert_eq!(content.len(), 3);
        assert!(matches!(&content[0], ToolResultContent::Text(t) if t.text == "snapshot"));
        assert!(matches!(&content[1], ToolResultContent::Image(_)));
        assert!(matches!(&content[2], ToolResultContent::Text(t) if t.text == "done"));
    }

    /// The bridge dispatches by name to the real agent, not a stub — a
    /// catalog spec turns into a callable that actually reaches
    /// `ConversionAgent::execute`.
    #[tokio::test]
    async fn a_built_tool_dispatches_to_the_real_agent() {
        let agent = bare_agent();
        let tools = dynamic_tools_for(
            &agent,
            &[serde_json::json!({
                "name": "get_source_info",
                "description": "Info about the source.",
                "input_schema": {"type": "object", "properties": {}},
            })],
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "get_source_info");

        // Dispatch through rig's own registry, exactly as the runner would —
        // this is what proves the closure really reaches the shared agent
        // rather than a placeholder, using no lower-level shortcut.
        let set = rig_agent::tool::ToolSet::from_dynamic_tools(tools);
        let mut ctx = rig_agent::tool::ToolContext::new();
        let result = set.execute("get_source_info", "{}", &mut ctx).await;
        assert!(result.is_success(), "{result:?}");
        assert!(
            result
                .output()
                .as_text()
                .unwrap()
                .contains(r#""documents":[]"#)
        );
    }

    /// A spec missing a name is dropped, not panicked on — the catalog's own
    /// tests are what guarantee a real spec always has one.
    #[test]
    fn a_spec_with_no_name_is_dropped() {
        let agent = bare_agent();
        let tools = dynamic_tools_for(&agent, &[serde_json::json!({"description": "no name"})]);
        assert!(tools.is_empty());
    }
}
