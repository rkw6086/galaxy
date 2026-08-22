use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST: usize = 64_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub content: MessageContent,
}

impl ConversationMessage {
    pub fn truncate_tool_results_for_provider_request(&mut self) {
        truncate_tool_results_in_content(&mut self.content);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ContentPart {
    Text(String),
    Reasoning {
        text: String,
        signature: Option<String>,
    },
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub conversation_id: Option<String>,
    pub model: ModelId,
    pub system_prompt: Option<String>,
    /// Current runtime input when the runtime owns conversation history.
    ///
    /// Provider runtimes normally consume `messages`, while session-oriented
    /// runtimes such as ACP consume this single prompt and retain their own
    /// history. Keeping the distinction explicit prevents applications from
    /// serializing a prompt into provider history and translating it back.
    pub prompt: Option<MessageContent>,
    pub messages: Vec<ConversationMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: Option<u64>,
    pub metadata: BTreeMap<String, JsonValue>,
}

impl TurnRequest {
    pub fn new(model: impl Into<ModelId>, messages: Vec<ConversationMessage>) -> Self {
        Self {
            conversation_id: None,
            model: model.into(),
            system_prompt: None,
            prompt: None,
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_prompt(mut self, prompt: MessageContent) -> Self {
        self.prompt = Some(prompt);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub status: ToolResultStatus,
}

impl ToolResult {
    pub fn is_error(&self) -> bool {
        matches!(
            self.status,
            ToolResultStatus::Error | ToolResultStatus::Denied
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultStatus {
    Success,
    Error,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionKind {
    Read,
    Write,
    Execute,
    Network,
    ExternalTool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub call_id: String,
    pub kind: PermissionKind,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    AllowOnce,
    AlwaysAllow,
    Denied { reason: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolEvent {
    Proposed {
        call: ToolCall,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    PermissionResolved {
        request_id: String,
        call_id: String,
        decision: PermissionDecision,
    },
    Started {
        call_id: String,
    },
    Completed {
        result: ToolResult,
    },
}

impl ToolEvent {
    pub fn call_id(&self) -> &str {
        match self {
            ToolEvent::Proposed { call } => &call.id,
            ToolEvent::PermissionRequested { request } => &request.call_id,
            ToolEvent::PermissionResolved { call_id, .. } | ToolEvent::Started { call_id } => {
                call_id
            }
            ToolEvent::Completed { result } => &result.call_id,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens not served by or written to a provider prompt cache.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input tokens served from the provider prompt cache.
    pub cached_input_tokens: u64,
    /// Input tokens written to the provider prompt cache.
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.output_tokens)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Completed,
    Cancelled,
    MaxTokens,
    ContextWindowExceeded,
    Refusal,
    ToolLoopLimit,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeActivityStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeActivity {
    pub id: String,
    pub title: String,
    pub status: Option<RuntimeActivityStatus>,
    pub output: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentEvent {
    TurnStarted {
        runtime_request_id: String,
    },
    /// A transport heartbeat proving that the current model stream is still connected.
    /// Consumers should use this to refresh idle timeouts without rendering output.
    KeepAlive,
    /// Progress received while the provider is streaming a tool call's JSON arguments.
    /// The arguments remain buffered by the provider runtime and are not executed until the
    /// complete tool call has been parsed and emitted as [`ToolEvent::Proposed`].
    ToolCallProgress {
        call_id: String,
        name: Option<String>,
        arguments_bytes: u64,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ReasoningCompleted {
        text: String,
        signature: Option<String>,
    },
    Tool {
        event: ToolEvent,
    },
    UsageUpdated {
        usage: Usage,
    },
    /// Progress owned by a session runtime rather than a Galaxy-executed tool.
    RuntimeActivityUpdated {
        activity: RuntimeActivity,
    },
    /// Current context occupancy reported by a session runtime.
    ContextUsageUpdated {
        used_tokens: u64,
        context_size: u64,
    },
    /// Visible user input accepted while a turn was already running.
    UserInputAccepted {
        text: String,
    },
    /// A displayable runtime lifecycle or permission notice.
    RuntimeNotice {
        message: String,
    },
    TurnStopped {
        reason: StopReason,
    },
}

fn truncate_tool_results_in_content(content: &mut MessageContent) {
    match content {
        MessageContent::Text(_) | MessageContent::ToolUse { .. } => {}
        MessageContent::ToolResult { content, .. } => truncate_tool_result_text(content),
        MessageContent::MultiPart(parts) => {
            for part in parts {
                match part {
                    ContentPart::ToolResult { content, .. } => {
                        truncate_tool_result_text(content);
                    }
                    ContentPart::Text(_)
                    | ContentPart::Reasoning { .. }
                    | ContentPart::Image { .. }
                    | ContentPart::ToolUse { .. } => {}
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
#[path = "types_tests.rs"]
mod tests;
