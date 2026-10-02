use super::*;

#[test]
fn legacy_profiles_migrate_without_preserving_unconditional_execution() {
    for (old, mode) in [
        ("AlwaysAllow", PermissionMode::GalaxyDecides),
        ("AgentDecides", PermissionMode::GalaxyDecides),
        ("AlwaysAsk", PermissionMode::AskFirst),
        ("Unknown", PermissionMode::ReadOnly),
    ] {
        let profile: AIExecutionProfile = serde_json::from_value(serde_json::json!({
            "execute_commands": old,
            "mcp_denylist": ["00000000-0000-0000-0000-000000000001"],
            "base_model": "test-model",
        }))
        .unwrap();
        assert_eq!(profile.permission_mode, mode);
        assert!(!profile.execute_commands.is_always_allow());
        assert_eq!(profile.mcp_denylist.len(), 1);
        assert_eq!(profile.base_model, Some(LLMId::from("test-model")));
    }
}

#[test]
fn explicit_mode_overrides_legacy_fields_and_unknown_modes_are_read_only() {
    for mode in ["ReadOnly", "FutureUnsafeMode"] {
        let profile: AIExecutionProfile = serde_json::from_value(serde_json::json!({
            "permission_mode": mode,
            "execute_commands": "AlwaysAllow",
            "apply_code_diffs": "AlwaysAllow",
            "command_allowlist": [],
        }))
        .unwrap();
        assert_eq!(profile.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(profile.execute_commands, ActionPermission::Unknown);
        assert_eq!(profile.apply_code_diffs, ActionPermission::Unknown);
        assert!(profile.run_agents.is_never_allow());
        assert!(!profile.computer_use.is_enabled());
    }
}

#[test]
fn serialized_profiles_only_store_one_permission_mode_and_bounded_approvals() {
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::AskFirst,
        PermissionMode::GalaxyDecides,
    ] {
        let profile = AIExecutionProfile {
            permission_mode: mode,
            approved_command_hashes: (0..300).map(|n| n.to_string()).collect(),
            ..AIExecutionProfile::default()
        };
        let value = serde_json::to_value(&profile).unwrap();
        for field in [
            "execute_commands",
            "apply_code_diffs",
            "read_files",
            "mcp_permissions",
            "write_to_pty",
            "run_agents",
            "computer_use",
            "command_allowlist",
            "directory_allowlist",
            "mcp_allowlist",
        ] {
            assert!(
                value.get(field).is_none(),
                "serialized legacy field {field}"
            );
        }
        let restored: AIExecutionProfile = serde_json::from_value(value).unwrap();
        assert_eq!(restored.permission_mode, mode);
        assert_eq!(restored.approved_command_hashes.len(), 256);
        assert_eq!(restored.approved_command_hashes.first().unwrap(), "44");
    }
}
