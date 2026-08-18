// Keep this module as a compatibility import path while provider-neutral message
// types move out of the application crate.
pub use galaxy_agent_core::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
    MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST,
};

pub(crate) fn flatten_tool_history_for_no_tools_turn(messages: &mut [ConversationMessage]) {
    for message in messages {
        message.content = flatten_tool_history_content(std::mem::replace(
            &mut message.content,
            MessageContent::Text(String::new()),
        ));
    }
}

fn flatten_tool_history_content(content: MessageContent) -> MessageContent {
    match content {
        MessageContent::Text(text) => MessageContent::Text(text),
        MessageContent::ToolUse {
            tool_use_id,
            name,
            input,
        } => MessageContent::Text(flattened_tool_use_text(&tool_use_id, &name, &input)),
        MessageContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => MessageContent::Text(flattened_tool_result_text(&tool_use_id, &content, is_error)),
        MessageContent::MultiPart(parts) => MessageContent::MultiPart(
            parts
                .into_iter()
                .map(flatten_tool_history_content_part)
                .collect(),
        ),
    }
}

fn flatten_tool_history_content_part(part: ContentPart) -> ContentPart {
    match part {
        ContentPart::Text(text) => ContentPart::Text(text),
        ContentPart::Reasoning { text, signature } => ContentPart::Reasoning { text, signature },
        ContentPart::Image { data, mime_type } => ContentPart::Image { data, mime_type },
        ContentPart::ToolUse {
            tool_use_id,
            name,
            input,
        } => ContentPart::Text(flattened_tool_use_text(&tool_use_id, &name, &input)),
        ContentPart::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => ContentPart::Text(flattened_tool_result_text(&tool_use_id, &content, is_error)),
    }
}

fn flattened_tool_use_text(tool_use_id: &str, name: &str, input: &serde_json::Value) -> String {
    format!("Previous tool call `{name}` (id: {tool_use_id}) with input:\n{input}")
}

fn flattened_tool_result_text(tool_use_id: &str, content: &str, is_error: bool) -> String {
    let status = if is_error { "error" } else { "success" };
    format!("Previous tool result for id `{tool_use_id}` ({status}):\n{content}")
}
