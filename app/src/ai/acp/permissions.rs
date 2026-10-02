use galaxy_acp::AcpPermissionPolicy;

use crate::ai::execution_profiles::{AIExecutionProfile, PermissionMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AcpPermissionResolution {
    pub(crate) policy: AcpPermissionPolicy,
    pub(crate) auto_approve_protocol_requests: bool,
    pub(crate) expose_galaxy_tools: bool,
    pub(crate) allow_terminal_execute: bool,
    pub(crate) allow_terminal_interrupt: bool,
}

pub(crate) fn resolve_acp_permissions(profile: &AIExecutionProfile) -> AcpPermissionResolution {
    AcpPermissionResolution {
        // External ACP clients retain their own protocol permission flow. Galaxy
        // mutation tools require structured app-side review and cannot be granted
        // by a broad ACP category or a legacy AlwaysAllow field.
        policy: AcpPermissionPolicy::default(),
        auto_approve_protocol_requests: false,
        expose_galaxy_tools: profile.permission_mode != PermissionMode::ReadOnly,
        allow_terminal_execute: false,
        allow_terminal_interrupt: false,
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
