use super::*;

#[test]
fn permissive_profile_only_enables_exact_galaxy_tools() {
    let profile = AIExecutionProfile {
        read_files: ActionPermission::AlwaysAllow,
        apply_code_diffs: ActionPermission::AlwaysAllow,
        execute_commands: ActionPermission::AlwaysAllow,
        mcp_permissions: ActionPermission::AlwaysAllow,
        write_to_pty: WriteToPtyPermission::AlwaysAsk,
        command_denylist: Vec::new(),
        ..AIExecutionProfile::default()
    };

    let permissions = resolve_acp_permissions(&profile);

    assert_eq!(permissions.policy, AcpPermissionPolicy::default());
    assert!(!permissions.auto_approve_protocol_requests);
    assert!(permissions.expose_galaxy_tools);
    assert!(permissions.allow_terminal_execute);
    assert!(!permissions.allow_terminal_interrupt);
}

#[test]
fn agent_decides_and_always_ask_do_not_bypass_galaxy_approval() {
    let profile = AIExecutionProfile {
        web_search_enabled: false,
        ..AIExecutionProfile::default()
    };

    let permissions = resolve_acp_permissions(&profile);

    assert_eq!(permissions.policy, AcpPermissionPolicy::default());
    assert!(!permissions.auto_approve_protocol_requests);
    assert!(permissions.expose_galaxy_tools);
    assert!(!permissions.allow_terminal_execute);
    assert!(!permissions.allow_terminal_interrupt);
}

#[test]
fn run_to_completion_cannot_bypass_profile_gates_or_protocol_checks() {
    // ResponseStream deliberately uses this same resolution in Run to
    // Completion mode. RTC may skip individual confirmations, but Galaxy's
    // native execution path still enforces command denylists and protected
    // paths, which ACP's unstructured permission request cannot evaluate.
    let permissions = resolve_acp_permissions(&AIExecutionProfile::default());

    assert_eq!(permissions.policy, AcpPermissionPolicy::default());
    assert!(!permissions.auto_approve_protocol_requests);
    assert!(permissions.expose_galaxy_tools);
    assert!(!permissions.allow_terminal_execute);
    assert!(!permissions.allow_terminal_interrupt);
}

#[test]
fn exact_mutation_tools_require_both_profile_permissions() {
    let mut profile = AIExecutionProfile {
        execute_commands: ActionPermission::AlwaysAllow,
        write_to_pty: WriteToPtyPermission::AlwaysAllow,
        command_denylist: Vec::new(),
        ..AIExecutionProfile::default()
    };

    let without_mcp = resolve_acp_permissions(&profile);
    assert!(!without_mcp.allow_terminal_execute);
    assert!(!without_mcp.allow_terminal_interrupt);

    profile.mcp_permissions = ActionPermission::AlwaysAllow;
    let with_mcp = resolve_acp_permissions(&profile);
    assert!(with_mcp.allow_terminal_execute);
    assert!(with_mcp.allow_terminal_interrupt);
}

#[test]
fn terminal_execute_is_hidden_when_any_command_denylist_is_effective() {
    let mut profile = AIExecutionProfile::default();
    assert!(!profile.command_denylist.is_empty());
    profile.execute_commands = ActionPermission::AlwaysAllow;
    profile.mcp_permissions = ActionPermission::AlwaysAllow;

    let permissions = resolve_acp_permissions(&profile);

    assert!(!permissions.allow_terminal_execute);
}
