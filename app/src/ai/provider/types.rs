// Keep this module as a compatibility import path while provider-neutral message
// types move out of the application crate.
pub use galaxy_agent_core::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
    MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST,
};
