use galaxy_acp::AcpPermissionPolicy;

use crate::ai::execution_profiles::{AIExecutionProfile, ActionPermission, WriteToPtyPermission};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AcpPermissionResolution {
    pub(crate) policy: AcpPermissionPolicy,
    pub(crate) auto_approve_protocol_requests: bool,
    pub(crate) expose_galaxy_tools: bool,
    pub(crate) allow_terminal_execute: bool,
    pub(crate) allow_terminal_interrupt: bool,
}

pub(crate) fn resolve_acp_permissions(profile: &AIExecutionProfile) -> AcpPermissionResolution {
    let mcp_allowed = profile.mcp_permissions == ActionPermission::AlwaysAllow;
    AcpPermissionResolution {
        // ACP permission requests expose only a broad category and display
        // text. They do not provide the structured command, path, or MCP server
        // identity required to enforce Galaxy's allowlists, denylists, and
        // protected-path rules. Keep every resource-affecting protocol
        // category denied until a request can be evaluated by those native
        // permission checks.
        policy: AcpPermissionPolicy::default(),
        // Run to Completion still honors command denylists and protected file
        // paths in Galaxy. It therefore cannot safely become ACP's blanket
        // `auto_approve`, which bypasses all category checks.
        auto_approve_protocol_requests: false,
        // The pane-pinned status tool is read-only and is useful even when the
        // profile does not permit MCP mutations. Individual mutation tools are
        // still omitted below unless every relevant permission is explicit.
        expose_galaxy_tools: true,
        allow_terminal_execute: mcp_allowed
            && profile.execute_commands == ActionPermission::AlwaysAllow
            // The MCP subprocess cannot call `can_autoexecute_command` with
            // Galaxy's effective predicates. Until command authorization is
            // moved into the app-side handler, exposing arbitrary command
            // execution is safe only when there is no denylist to bypass.
            && profile.command_denylist.is_empty(),
        allow_terminal_interrupt: mcp_allowed
            && profile.write_to_pty == WriteToPtyPermission::AlwaysAllow,
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
