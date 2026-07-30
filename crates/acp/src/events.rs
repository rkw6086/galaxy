use agent_client_protocol::schema::v1::{
    ContentBlock, Cost, RequestPermissionRequest, SessionConfigOption, SessionId, StopReason,
    ToolCallId, ToolCallStatus,
};

use crate::PermissionDecision;

/// Visible events produced while Galaxy drives an ACP turn.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AcpEvent {
    /// A new or restored ACP session is ready.
    SessionStarted {
        /// Agent-owned session identifier.
        session_id: SessionId,
        /// Agent implementation metadata from initialization.
        agent_info: Option<agent_client_protocol::schema::v1::Implementation>,
        /// Agent capabilities from initialization.
        capabilities: agent_client_protocol::schema::v1::AgentCapabilities,
        /// Whether this agent advertised `session/load`.
        can_load: bool,
        /// Whether this agent advertised the Codex `_session/steering`
        /// extension.
        can_steer: bool,
    },
    /// A streamed text fragment from the agent.
    AgentText {
        /// Markdown-capable text fragment.
        text: String,
    },
    /// A streamed reasoning fragment from the agent.
    AgentThought {
        /// Thought text fragment.
        text: String,
    },
    /// A non-text content block from an agent message or thought.
    AgentContent {
        /// ACP content block.
        content: ContentBlock,
        /// `true` when this block came from an agent-thought update.
        thought: bool,
    },
    /// Content supplied by the user while a prompt is already running.
    ///
    /// ACP agents use this update to echo accepted live steering input. Galaxy
    /// can render it as a user message without exposing hidden system context.
    UserContent {
        /// ACP content block accepted by the agent.
        content: ContentBlock,
    },
    /// A tool call began.
    ToolCall {
        /// ACP tool-call identifier.
        id: ToolCallId,
        /// Human-readable title.
        title: String,
        /// Current execution status.
        status: ToolCallStatus,
        /// Bounded, control-sequence-free output suitable for Galaxy's tool pane.
        output: Option<String>,
    },
    /// A tool call changed.
    ToolCallUpdate {
        /// ACP tool-call identifier.
        id: ToolCallId,
        /// New title, when supplied by the agent.
        title: Option<String>,
        /// New status, when supplied by the agent.
        status: Option<ToolCallStatus>,
        /// Bounded, control-sequence-free output supplied by this update.
        output: Option<String>,
    },
    /// The agent advertised or changed the complete session configuration.
    ConfigOptions { options: Vec<SessionConfigOption> },
    /// Context-window or cost information changed.
    Usage {
        /// Tokens currently in context.
        used: u64,
        /// Total context-window size.
        size: u64,
        /// Optional cumulative cost.
        cost: Option<Cost>,
    },
    /// The agent asked for permission.
    PermissionRequested {
        /// Original request, suitable for rendering in Galaxy.
        request: RequestPermissionRequest,
    },
    /// Galaxy's permission hook resolved a request.
    PermissionResolved {
        /// Session to which the permission applies.
        session_id: SessionId,
        /// Decision returned by the hook.
        decision: PermissionDecision,
    },
    /// The prompt turn completed.
    Finished {
        /// ACP stop reason.
        stop_reason: StopReason,
    },
    /// A recoverable or terminal runtime error for this turn.
    Error {
        /// Safe, user-presentable error description.
        message: String,
    },
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
