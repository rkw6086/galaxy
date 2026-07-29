use serde_json::Value as JsonValue;

pub const MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST: usize = 64_000;

#[derive(Clone, Debug)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub content: MessageContent,
}

impl ConversationMessage {
    pub fn truncate_tool_results_for_provider_request(&mut self) {
        truncate_tool_results_in_content(&mut self.content);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub enum MessageContent {
    Text(String),
    ToolUse {
        tool_use_id: String,
        name: String,
        input: JsonValue,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    MultiPart(Vec<ContentPart>),
}

#[derive(Clone, Debug)]
pub enum ContentPart {
    Text(String),
    Image {
        data: Vec<u8>,
        mime_type: String,
    },
    ToolUse {
        tool_use_id: String,
        name: String,
        input: JsonValue,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

fn truncate_tool_results_in_content(content: &mut MessageContent) {
    match content {
        MessageContent::Text(_) | MessageContent::ToolUse { .. } => {}
        MessageContent::ToolResult { content, .. } => truncate_tool_result_text(content),
        MessageContent::MultiPart(parts) => {
            for part in parts {
                if let ContentPart::ToolResult { content, .. } = part {
                    truncate_tool_result_text(content);
                }
            }
        }
    }
}

fn truncate_tool_result_text(content: &mut String) {
    let char_count = content.chars().count();
    if char_count <= MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST {
        return;
    }

    let omitted_chars = char_count.saturating_sub(MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST);
    let marker = format!("\n... [tool result truncated; omitted {omitted_chars} chars] ...\n");
    let marker_chars = marker.chars().count();
    let retained_chars = MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST.saturating_sub(marker_chars);
    let head_chars = retained_chars / 2;
    let tail_chars = retained_chars.saturating_sub(head_chars);
    let head: String = content.chars().take(head_chars).collect();
    let tail: String = content
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    *content = format!("{head}{marker}{tail}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_large_tool_results_for_provider_request() {
        let prefix = "start:";
        let suffix = ":end";
        let middle = "x".repeat(MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST + 1_000);
        let mut message = ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "toolu_1".to_string(),
                content: format!("{prefix}{middle}{suffix}"),
                is_error: false,
            },
        };

        message.truncate_tool_results_for_provider_request();

        let MessageContent::ToolResult { content, .. } = message.content else {
            panic!("expected tool result");
        };
        assert!(content.len() <= MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST + 128);
        assert!(content.starts_with(prefix));
        assert!(content.ends_with(suffix));
        assert!(content.contains("tool result truncated"));
    }
}
