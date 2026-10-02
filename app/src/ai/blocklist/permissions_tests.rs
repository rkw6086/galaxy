use std::path::PathBuf;

use galaxy_core::execution_mode::ExecutionMode;
use galaxy_util::path::EscapeChar;
use galaxyui::{App, EntityId, ModelHandle};
use uuid::Uuid;

use super::{BlocklistAIHistoryModel, BlocklistAIPermissions};
use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::permissions::{
    CommandExecutionPermission, CommandExecutionPermissionDeniedReason, FileWritePermission,
    FileWritePermissionDeniedReason,
};
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::execution_profiles::{ActionPermission, PermissionMode};
use crate::ai::mcp::templatable_manager::TemplatableMCPServerManager;
use crate::auth::AuthStateProvider;
use crate::cloud_object::model::persistence::CloudModel;
use crate::network::NetworkStatus;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::sync_queue::SyncQueue;
use crate::settings::{AgentModeCommandExecutionPredicate, PrivacySettings};
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::test_util::settings::initialize_settings_for_tests_with_mode;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::{
    AgentNotificationsModel, GlobalResourceHandles, GlobalResourceHandlesProvider, LaunchMode,
};

struct PermissionsTestState {
    convo_id: AIConversationId,
    permissions: ModelHandle<BlocklistAIPermissions>,
    history: ModelHandle<BlocklistAIHistoryModel>,
    terminal_view_id: EntityId,
    user_workspaces: ModelHandle<UserWorkspaces>,
    profile_model: ModelHandle<AIExecutionProfilesModel>,
}

fn initialize_permissions_test(app: &mut App) -> PermissionsTestState {
    initialize_permissions_test_with_mode(app, ExecutionMode::App, false)
}

fn initialize_permissions_test_with_mode(
    app: &mut App,
    mode: ExecutionMode,
    is_sandboxed: bool,
) -> PermissionsTestState {
    initialize_settings_for_tests_with_mode(app, mode, is_sandboxed);
    let global_resource_handles = GlobalResourceHandles::mock(app);
    app.add_singleton_model(|_| GlobalResourceHandlesProvider::new(global_resource_handles));
    let history = app.add_singleton_model(|_| BlocklistAIHistoryModel::new(vec![], &[]));
    app.add_singleton_model(|_| CLIAgentSessionsModel::new());
    app.add_singleton_model(|_| ActiveAgentViewsModel::new());
    app.add_singleton_model(AgentNotificationsModel::new);
    let permissions = app.add_singleton_model(BlocklistAIPermissions::new);
    let terminal_view_id = EntityId::new();
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(UpdateManager::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(|ctx| {
        crate::local_object_repository::LocalObjectRepository::new(None, None, ctx)
    });
    app.add_singleton_model(|_| TemplatableMCPServerManager::default());
    let profile_model = app.add_singleton_model(|ctx| {
        AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
    });
    app.add_singleton_model(PrivacySettings::mock);
    let user_workspaces = app.add_singleton_model(UserWorkspaces::default_mock);

    let conversation_id = history.update(app, |history_model, ctx| {
        history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
    });

    PermissionsTestState {
        convo_id: conversation_id,
        permissions,
        history,
        terminal_view_id,
        user_workspaces,
        profile_model,
    }
}

#[test]
fn test_can_write_files_mcp_config_always_denied() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            terminal_view_id,
            convo_id,
            permissions,
            profile_model,
            ..
        } = initialize_permissions_test(&mut app);

        // Even with AlwaysAllow, writing to an MCP config must be denied.
        profile_model.update(&mut app, |model, ctx| {
            model.set_apply_code_diffs(
                *model.active_profile(Some(terminal_view_id), ctx).id(),
                &ActionPermission::AlwaysAllow,
                ctx,
            );
        });

        let mcp_config_paths = vec![
            PathBuf::from("/project/.mcp.json"),
            PathBuf::from("/project/.warp/.mcp.json"),
            PathBuf::from("/project/.codex/config.toml"),
        ];

        for path in mcp_config_paths {
            permissions.read(&app, |model, ctx| {
                let result = model.can_write_files(
                    &convo_id,
                    std::slice::from_ref(&path),
                    Some(terminal_view_id),
                    ctx,
                );
                assert!(
                    !result.is_allowed(),
                    "expected MCP config path {path:?} to be denied"
                );
                assert!(
                    matches!(
                        result,
                        FileWritePermission::Denied(FileWritePermissionDeniedReason::ProtectedPath)
                    ),
                    "expected ProtectedPath denial for {path:?}, got {result:?}"
                );
            });
        }
    })
}

#[test]
fn test_can_autoexecute_command_denylist_precedence() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            convo_id,
            permissions,
            user_workspaces,
            profile_model,
            terminal_view_id,
            ..
        } = initialize_permissions_test(&mut app);

        // Set up profile with denylist
        profile_model.update(&mut app, |model, ctx| {
            model.add_to_command_denylist(
                *model.active_profile(Some(terminal_view_id), ctx).id(),
                &AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap(),
                ctx,
            );
        });

        // Test that profile denylist is respected when no workspace denylist
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "rm file.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(!result.is_allowed());
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));
        });

        // Set workspace denylist
        user_workspaces.update(&mut app, |model, ctx| {
            model.setup_test_workspace(ctx);
            model.update_ai_autonomy_settings(
                |settings| {
                    settings.execute_commands_denylist =
                        Some(vec![AgentModeCommandExecutionPredicate::new_regex(
                            "git .*",
                        )
                        .unwrap()]);
                },
                ctx,
            );
        });

        // Org + user denylists are merged: both should be active
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "git status",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(!result.is_allowed());
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));

            let result = model.can_autoexecute_command(
                &convo_id,
                "rm file.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(
                matches!(
                    result,
                    CommandExecutionPermission::Denied(
                        CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                    )
                ),
                "user denylist entries should be merged with org denylist, not replaced"
            );
        });
    })
}

#[test]
fn test_can_autoexecute_command_denylist_matches_env_prefixed_commands() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            convo_id,
            permissions,
            profile_model,
            terminal_view_id,
            ..
        } = initialize_permissions_test(&mut app);

        profile_model.update(&mut app, |model, ctx| {
            let profile_id = *model.active_profile(Some(terminal_view_id), ctx).id();
            model.set_execute_commands(profile_id, &ActionPermission::AlwaysAllow, ctx);
            model.add_to_command_denylist(
                profile_id,
                &AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap(),
                ctx,
            );
        });

        for command in [
            "X=1 rm file.txt",
            "echo ok && X=1 rm file.txt",
            "echo $(X=1 rm file.txt)",
        ] {
            permissions.read(&app, |model, ctx| {
                let result = model.can_autoexecute_command(
                    &convo_id,
                    command,
                    EscapeChar::Backslash,
                    false,
                    None,
                    Some(terminal_view_id),
                    ctx,
                );
                assert!(
                    matches!(
                        result,
                        CommandExecutionPermission::Denied(
                            CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                        )
                    ),
                    "{command:?} should be denied by the rm denylist, got {result:?}"
                );
            });
        }

        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "X=1 git status",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::AlwaysAskEnabled
                )
            ));
        });
    })
}

#[test]
fn test_can_autoexecute_command_denylist_beats_run_to_completion() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            convo_id,
            permissions,
            history,
            profile_model,
            terminal_view_id,
            ..
        } = initialize_permissions_test(&mut app);

        // Add a denylist rule that matches the test command.
        profile_model.update(&mut app, |model, ctx| {
            model.add_to_command_denylist(
                *model.active_profile(Some(terminal_view_id), ctx).id(),
                &AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap(),
                ctx,
            );
        });

        // Toggle run-to-completion override for this conversation.
        history.update(&mut app, |history, ctx| {
            history.toggle_autoexecute_override(&convo_id, terminal_view_id, ctx);
        });

        // Despite run-to-completion, denylist must take precedence and deny execution.
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "rm important.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(!result.is_allowed());
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));
        });
    })
}

#[test]
fn test_merged_denylist_deduplication() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            permissions,
            user_workspaces,
            profile_model,
            terminal_view_id,
            ..
        } = initialize_permissions_test(&mut app);

        let rm_predicate = AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap();

        profile_model.update(&mut app, |model, ctx| {
            model.add_to_command_denylist(
                *model.active_profile(Some(terminal_view_id), ctx).id(),
                &rm_predicate,
                ctx,
            );
        });

        user_workspaces.update(&mut app, |model, ctx| {
            model.setup_test_workspace(ctx);
            model.update_ai_autonomy_settings(
                |settings| {
                    settings.execute_commands_denylist = Some(vec![
                        AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap(),
                        AgentModeCommandExecutionPredicate::new_regex("git .*").unwrap(),
                    ]);
                },
                ctx,
            );
        });

        permissions.read(&app, |model, ctx| {
            let denylist = model.get_execute_commands_denylist(ctx, Some(terminal_view_id));
            let rm_count = denylist.iter().filter(|p| p.to_string() == "rm .*").count();
            assert_eq!(rm_count, 1, "duplicate entries should be deduplicated");
            assert!(
                denylist.iter().any(|p| p.to_string() == "git .*"),
                "org entry 'git .*' should be in merged list"
            );
        });
    })
}

#[test]
fn test_get_org_execute_commands_denylist() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            permissions,
            user_workspaces,
            ..
        } = initialize_permissions_test(&mut app);

        permissions.read(&app, |_, ctx| {
            let org_list = BlocklistAIPermissions::get_org_execute_commands_denylist(ctx);
            assert!(org_list.is_empty());
        });

        user_workspaces.update(&mut app, |model, ctx| {
            model.setup_test_workspace(ctx);
            model.update_ai_autonomy_settings(
                |settings| {
                    settings.execute_commands_denylist =
                        Some(vec![AgentModeCommandExecutionPredicate::new_regex(
                            "git .*",
                        )
                        .unwrap()]);
                },
                ctx,
            );
        });

        permissions.read(&app, |_, ctx| {
            let org_list = BlocklistAIPermissions::get_org_execute_commands_denylist(ctx);
            assert_eq!(org_list.len(), 1);
            assert_eq!(org_list[0].to_string(), "git .*");
        });
    })
}

#[test]
fn test_empty_org_denylist_allows_user_entries() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            convo_id,
            permissions,
            user_workspaces,
            profile_model,
            terminal_view_id,
            ..
        } = initialize_permissions_test(&mut app);

        profile_model.update(&mut app, |model, ctx| {
            model.add_to_command_denylist(
                *model.active_profile(Some(terminal_view_id), ctx).id(),
                &AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap(),
                ctx,
            );
        });

        user_workspaces.update(&mut app, |model, ctx| {
            model.setup_test_workspace(ctx);
            model.update_ai_autonomy_settings(
                |settings| {
                    settings.execute_commands_denylist = Some(vec![]);
                },
                ctx,
            );
        });

        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "rm file.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(
                !result.is_allowed(),
                "user denylist entry should be active even when org denylist is empty"
            );
        });
    })
}

#[test]
fn test_denylist_matches_multiline_commands() {
    App::test((), |mut app| async move {
        let PermissionsTestState {
            convo_id,
            permissions,
            profile_model,
            terminal_view_id,
            ..
        } = initialize_permissions_test(&mut app);

        // Add denylist rule for rm
        profile_model.update(&mut app, |model, ctx| {
            model.add_to_command_denylist(
                *model.active_profile(Some(terminal_view_id), ctx).id(),
                &AgentModeCommandExecutionPredicate::new_regex("rm .*").unwrap(),
                ctx,
            );
        });

        // Single-line rm command should be denied
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "rm file.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(!result.is_allowed());
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));
        });

        // Multiline rm command with backslash continuations should also be denied (POSIX)
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "rm file1.txt \\\nfile2.txt \\\nfile3.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(
                !result.is_allowed(),
                "multiline rm command should be denied by denylist"
            );
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));
        });

        // Env-prefixed multiline rm command should also be denied after normalization.
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "X=1 rm file1.txt \\\nfile2.txt \\\nfile3.txt",
                EscapeChar::Backslash,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(
                !result.is_allowed(),
                "env-prefixed multiline rm command should be denied by denylist"
            );
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));
        });

        // Multiline rm command with backtick continuations should also be denied (PowerShell)
        permissions.read(&app, |model, ctx| {
            let result = model.can_autoexecute_command(
                &convo_id,
                "rm file1.txt `\nfile2.txt `\nfile3.txt",
                EscapeChar::Backtick,
                false,
                None,
                Some(terminal_view_id),
                ctx,
            );
            assert!(
                !result.is_allowed(),
                "multiline rm command with backtick continuations should be denied by denylist"
            );
            assert!(matches!(
                result,
                CommandExecutionPermission::Denied(
                    CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                )
            ));
        });
    })
}

#[test]
fn the_mode_controls_legacy_projections_and_clears_remembered_commands() {
    App::test((), |mut app| async move {
        let state = initialize_permissions_test(&mut app);
        for mode in [
            PermissionMode::GalaxyDecides,
            PermissionMode::ReadOnly,
            PermissionMode::AskFirst,
        ] {
            state.profile_model.update(&mut app, |profiles, ctx| {
                let id = *profiles
                    .active_profile(Some(state.terminal_view_id), ctx)
                    .id();
                profiles.set_permission_mode(id, mode, ctx);
                // Legacy setters are not permission authority.
                profiles.set_execute_commands(id, &ActionPermission::AlwaysAllow, ctx);
                profiles.set_mcp_permissions(id, &ActionPermission::AlwaysAllow, ctx);
            });
            state.permissions.read(&app, |permissions, ctx| {
                assert_eq!(
                    permissions.get_permission_mode(ctx, Some(state.terminal_view_id)),
                    mode
                );
                assert_eq!(
                    permissions.get_execute_commands_setting(ctx, Some(state.terminal_view_id)),
                    mode.action_permission()
                );
                assert_eq!(
                    permissions.get_mcp_permissions_setting(ctx, Some(state.terminal_view_id)),
                    mode.action_permission()
                );
                let profile =
                    permissions.active_permissions_profile(ctx, Some(state.terminal_view_id));
                assert_eq!(
                    profile.computer_use.is_enabled(),
                    mode != PermissionMode::ReadOnly
                );
                assert_eq!(
                    profile.run_agents.is_enabled(),
                    mode != PermissionMode::ReadOnly
                );
            });
        }
        state.profile_model.update(&mut app, |profiles, ctx| {
            let id = *profiles
                .active_profile(Some(state.terminal_view_id), ctx)
                .id();
            profiles.remember_approved_command(id, "exact-command-digest".to_owned(), ctx);
            profiles.remember_approved_command(id, "exact-command-digest".to_owned(), ctx);
            assert_eq!(
                profiles
                    .active_profile(Some(state.terminal_view_id), ctx)
                    .data()
                    .approved_command_hashes
                    .len(),
                1
            );
            profiles.set_permission_mode(id, PermissionMode::GalaxyDecides, ctx);
            profiles.remember_approved_command(id, "cannot-grant-in-other-mode".to_owned(), ctx);
            assert!(profiles
                .active_profile(Some(state.terminal_view_id), ctx)
                .data()
                .approved_command_hashes
                .is_empty());
        });
    });
}
