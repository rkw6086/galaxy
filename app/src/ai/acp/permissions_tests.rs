use super::*;
use crate::ai::execution_profiles::{ActionPermission, WriteToPtyPermission};

#[test]
fn modes_do_not_grant_unstructured_terminal_or_protocol_permissions() {
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::AskFirst,
        PermissionMode::GalaxyDecides,
    ] {
        let profile = AIExecutionProfile {
            permission_mode: mode,
            execute_commands: ActionPermission::AlwaysAllow,
            mcp_permissions: ActionPermission::AlwaysAllow,
            write_to_pty: WriteToPtyPermission::AlwaysAllow,
            ..AIExecutionProfile::default()
        };
        let permissions = resolve_acp_permissions(&profile);
        assert_eq!(permissions.policy, AcpPermissionPolicy::default());
        assert!(!permissions.auto_approve_protocol_requests);
        assert!(!permissions.allow_terminal_execute);
        assert!(!permissions.allow_terminal_interrupt);
        assert_eq!(
            permissions.expose_galaxy_tools,
            mode != PermissionMode::ReadOnly
        );
    }
}
