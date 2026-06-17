use serde_json::Value as JsonValue;

#[derive(Clone, Debug)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub content: MessageContent,
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
