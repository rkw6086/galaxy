//! Runtime support for driving ACP agents from Galaxy.
//!
//! This crate deliberately contains no Galaxy UI or application-model code. It
//! owns the ACP subprocess and translates the stable ACP v1 stream into a small
//! event surface that the app can consume.

mod config;
mod events;
mod permissions;
mod runtime;

pub use agent_client_protocol::schema::v1::{
    ContentBlock, Cost, ImageContent, McpServer, McpServerHttp, McpServerStdio, PermissionOptionId,
    SessionId, StopReason, TextContent, ToolCallId, ToolCallStatus,
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
