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
        PermissionMode::AllowAll,
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
            "ask_user_question",
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

#[test]
fn explicit_command_approvals_and_provider_selection_survive_roundtrip_and_mode_changes() {
    let mut profile = AIExecutionProfile {
        permission_mode: PermissionMode::AllowAll,
        command_allowlist: vec![
            AgentModeCommandExecutionPredicate::new_regex("^cargo test$").unwrap(),
        ],
        orchestration_providers: Some(vec!["My LiteLLM".to_string()]),
        ..Default::default()
    };
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::AskFirst,
        PermissionMode::GalaxyDecides,
        PermissionMode::AllowAll,
    ] {
        profile.permission_mode = mode;
        profile.project_permission_mode();
        let saved = serde_json::to_value(&profile).unwrap();
        let restored: AIExecutionProfile = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.permission_mode, mode);
        assert_eq!(restored.command_allowlist, profile.command_allowlist);
        assert!(restored.includes_orchestration_provider("My LiteLLM"));
        assert!(!restored.includes_orchestration_provider("ChatGPT"));
    }
}

#[test]
fn missing_provider_selection_keeps_existing_profiles_unrestricted() {
    let profile: AIExecutionProfile = serde_json::from_str("{}").unwrap();
    assert!(profile.includes_orchestration_provider("New provider"));
    let excluded = AIExecutionProfile {
        orchestration_providers: Some(Vec::new()),
        ..Default::default()
    };
    assert!(!excluded.includes_orchestration_provider("New provider"));
}

#[test]
fn allow_all_projects_every_tool_to_always_allow() {
    let mut profile = AIExecutionProfile {
        permission_mode: PermissionMode::AllowAll,
        ..Default::default()
    };
    profile.project_permission_mode();
    assert_eq!(profile.execute_commands, ActionPermission::AlwaysAllow);
    assert_eq!(profile.apply_code_diffs, ActionPermission::AlwaysAllow);
    assert_eq!(profile.mcp_permissions, ActionPermission::AlwaysAllow);
    assert_eq!(profile.computer_use, ComputerUsePermission::AlwaysAllow);
    assert_eq!(profile.run_agents, RunAgentsPermission::AlwaysAllow);
}

#[test]
fn clarification_availability_follows_permission_mode_after_projection_and_restore() {
    for (mode, expected) in [
        (
            PermissionMode::AskFirst,
            AskUserQuestionPermission::AlwaysAsk,
        ),
        (
            PermissionMode::AllowAll,
            AskUserQuestionPermission::AlwaysAsk,
        ),
        (
            PermissionMode::GalaxyDecides,
            AskUserQuestionPermission::Never,
        ),
        (PermissionMode::ReadOnly, AskUserQuestionPermission::Never),
    ] {
        let mut profile = AIExecutionProfile {
            permission_mode: mode,
            ..Default::default()
        };
        profile.project_permission_mode();
        assert_eq!(profile.ask_user_question, expected, "{mode:?}");

        let saved = serde_json::to_value(&profile).unwrap();
        assert!(saved.get("ask_user_question").is_none());
        let restored: AIExecutionProfile = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.ask_user_question, expected, "{mode:?}");
    }
}

#[test]
fn model_start_timeout_secs_defaults_to_none() {
    let profile = AIExecutionProfile::default();
    assert!(profile.model_start_timeout_secs.is_none());
}

#[test]
fn model_start_timeout_secs_survives_round_trip() {
    for timeout in [None, Some(60u32), Some(300), Some(900)] {
        let profile = AIExecutionProfile {
            model_start_timeout_secs: timeout,
            ..Default::default()
        };
        let saved = serde_json::to_value(&profile).unwrap();
        let restored: AIExecutionProfile = serde_json::from_value(saved).unwrap();
        assert_eq!(
            restored.model_start_timeout_secs, timeout,
            "timeout={timeout:?}"
        );
    }
}

#[test]
fn model_start_timeout_secs_absent_in_legacy_json_restores_to_none() {
    // A profile serialized before this field was added must restore without error
    // and the field must be `None` (use the built-in default).
    let profile: AIExecutionProfile = serde_json::from_value(serde_json::json!({
        "permission_mode": "AskFirst",
        "base_model": "test-model",
    }))
    .unwrap();
    assert!(profile.model_start_timeout_secs.is_none());
}
