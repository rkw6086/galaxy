//! Agent Client Protocol integration for Galaxy's native agent conversation UI.
//!
//! ACP is a conversation backend, not an LLM provider. The external agent owns
//! its model, authentication, prompt loop, and tool execution. Galaxy owns the
//! process lifecycle, permissions, visible transcript, and local tool bridge.

mod launch;
mod permissions;
mod prompt;
mod response_translator;
mod runtime_model;
mod transport;

pub(crate) use launch::{
    acp_launch_fingerprint, acp_model_id, resolve_acp_launch, validate_acp_dispatch,
    validate_acp_launch_identity,
};
pub(crate) use permissions::resolve_acp_permissions;
pub(crate) use runtime_model::AcpRuntimeModel;
pub(crate) use transport::{
    acp_output_stream, acp_startup_error_stream, galaxy_mcp_server, AcpSessionHandleSlot,
    AcpSessionMetadata, AcpSteeringRequest, GalaxyMcpTarget,
};
