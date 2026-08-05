//! Runtime support for driving ACP agents from Galaxy.
//!
//! This crate deliberately contains no Galaxy UI or application-model code. It
//! owns the official SDK connection and subprocess, then exposes ACP sessions
//! through `galaxy_agent_core::AgentRuntime`. Low-level ACP events remain an
//! internal protocol boundary rather than leaking into application UI code.

mod agent_runtime;
mod config;
mod events;
mod permissions;
mod runtime;

pub use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, Cost, ImageContent, McpServer, McpServerHttp, McpServerStdio,
    PermissionOptionId, SessionConfigKind as SessionConfigOptionType, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionId, StopReason, TextContent, ToolCallId, ToolCallStatus,
};
pub use agent_runtime::{
    AcpAgentRuntime, AcpAgentRuntimeConfig, AcpRuntimeState, AcpRuntimeStateHandle,
};
pub use config::{
    AcpAgentPreset, AcpLaunchConfig, AcpManagerConfig, CODEX_ACP_NPM_VERSION, OPENCODE_NPM_VERSION,
};
pub use events::AcpEvent;
pub use permissions::{
    AcpPermissionPolicy, DenyByDefaultPermissionHandler, PermissionContext, PermissionDecision,
    PermissionHandler,
};
pub use runtime::{
    AcpRuntimeError, AcpSessionHandle, AcpSessionManager, AcpSteeringOutcome, AcpTurnRequest,
};
