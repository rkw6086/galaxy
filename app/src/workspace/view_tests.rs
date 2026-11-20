use std::collections::HashMap;

use ai::index::full_source_code_embedding::manager::CodebaseIndexManager;
use ai::project_context::model::ProjectContextModel;
#[cfg(feature = "local_fs")]
use galaxy_files::FileModel;
use pane_group::{NotebookPane, PaneState, SplitPaneState, TerminalPaneId};
use repo_metadata::repositories::DetectedRepositories;
use repo_metadata::watcher::DirectoryWatcher;
#[cfg(feature = "local_fs")]
use repo_metadata::CanonicalizedPath;
#[cfg(feature = "local_fs")]
use repo_metadata::RepoMetadataModel;
use session_sharing_protocol::common::SessionId;
#[cfg(feature = "local_fs")]
use tempfile::TempDir;
use terminal::shared_session::permissions_manager::SessionPermissionsManager;
use terminal::view::ActiveSessionState;
use warp_editor::editor::NavigationKey;
use warpui::platform::WindowStyle;
use warpui::{AddSingletonModel, App, ViewHandle};
use watcher::HomeDirectoryWatcher;

use super::*;
use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::agent_conversations_model::AgentConversationsModel;
use crate::ai::agent_tips::AITipModel;
use crate::ai::ambient_agents::github_auth_notifier::GitHubAuthNotifier;
use crate::ai::blocklist::agent_view::orchestration_pill_bar_model::OrchestrationPillBarModel;
use crate::ai::blocklist::{BlocklistAIHistoryModel, BlocklistAIPermissions};
#[cfg(not(target_family = "wasm"))]
use crate::ai::chatgpt_auth::ChatGPTAuthModel;
use crate::ai::document::ai_document_model::AIDocumentModel;
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::facts::manager::AIFactManager;
use crate::ai::harness_availability::HarnessAvailabilityModel;
use crate::ai::llms::LLMPreferences;
use crate::ai::mcp::gallery::MCPGalleryManager;
use crate::ai::mcp::templatable_manager::TemplatableMCPServerManager;
use crate::ai::mcp::{FileBasedMCPManager, FileMCPWatcher};
use crate::ai::outline::RepoOutlines;
use crate::ai::persisted_workspace::PersistedWorkspace;
use crate::ai::restored_conversations::RestoredAgentConversations;
use crate::ai::skills::SkillManager;
use crate::ai::AIRequestUsageModel;
use crate::cloud_object::model::persistence::CloudModel;
use crate::cloud_object::model::view::CloudViewModel;
use crate::context_chips::prompt::Prompt;
use crate::editor::Event;
use crate::gpu_state::GPUState;
use crate::menu::MenuAction;
use crate::network::NetworkStatus;
use crate::notebooks::editor::keys::NotebookKeybindings;
use crate::notebooks::notebook::NotebookView;
use crate::pane_group::{Direction, PaneGroupAction, PaneId};
use crate::pricing::PricingInfoModel;
#[cfg(not(target_family = "wasm"))]
use crate::remote_server::codebase_index_model::RemoteCodebaseIndexModel;
use crate::resource_center::Tip;
use crate::server::cloud_objects::listener::Listener;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::experiments::ServerExperiments;
use crate::server::server_api::ServerApiProvider;
use crate::server::sync_queue::SyncQueue;
use crate::server::telemetry::context_provider::AppTelemetryContextProvider;
use crate::settings::cloud_preferences_syncer::CloudPreferencesSyncer;
use crate::settings::PrivacySettings;
use crate::settings_view::keybindings::KeybindingChangedNotifier;
use crate::settings_view::DisplayCount;
use crate::suggestions::ignored_suggestions_model::IgnoredSuggestionsModel;
use crate::system::SystemStats;
use crate::tab_configs::tab_config::{TabConfigPaneNode, TabConfigPaneType};
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::history::History;
use crate::terminal::keys::TerminalKeybindings;
use crate::terminal::local_tty::spawner::PtySpawner;
use crate::terminal::shared_session::{
    SharedSessionScrollbackType, SharedSessionSource, SharedSessionStatus,
};
use crate::test_util::settings::initialize_settings_for_tests;
use crate::undo_close::UndoCloseSettings;
#[cfg(feature = "local_fs")]
use crate::user_config::tab_configs_dir;
#[cfg(windows)]
use crate::util::traffic_lights::windows::RendererState;
use crate::warp_managed_paths_watcher::WarpManagedPathsWatcher;
use crate::workflows::local_workflows::LocalWorkflows;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_profiles::UserProfiles;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::{
    experiments, workspace, AgentNotificationsModel, GlobalResourceHandlesProvider, ObjectActions,
};
#[test]
fn query_for_rewind_prefill_uses_custom_display_query_inputs() {
    let context: std::sync::Arc<[crate::ai::agent::AIAgentContext]> = Vec::new().into();
    let input = crate::ai::agent::AIAgentInput::FetchReviewComments {
        repo_path: "/repo".to_string(),
        context,
    };

    assert_eq!(
        query_for_rewind_prefill(&[input]),
        Some(
            crate::search::slash_command_menu::static_commands::commands::PR_COMMENTS
                .name
                .to_string()
        )
    );
}

pub(crate) fn initialize_app(app: &mut App) {
    initialize_settings_for_tests(app);

    // Add the necessary singleton models to the App
    app.add_singleton_model(|_ctx| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AppTelemetryContextProvider::new_context_provider);
    app.add_singleton_model(AuthManager::new_for_test);
    app.add_singleton_model(|_ctx| PtySpawner::new_for_test());
    app.add_singleton_model(|_| Prompt::mock());
    app.add_singleton_model(|ctx| AutoupdateState::new(ServerApiProvider::as_ref(ctx).get()));
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(|_| SystemStats::new());
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(|ctx| {
        crate::local_object_repository::LocalObjectRepository::new(None, None, ctx)
    });
    app.add_singleton_model(UserWorkspaces::default_mock);
    app.add_singleton_model(|_ctx| UserProfiles::new(Vec::new()));
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(TeamUpdateManager::mock);
    app.add_singleton_model(UpdateManager::mock);
    app.add_singleton_model(MCPGalleryManager::new);
    app.add_singleton_model(CloudViewModel::mock);
    app.add_singleton_model(Listener::mock);
    app.add_singleton_model(|_| Appearance::mock());
    app.add_singleton_model(AppearanceManager::new);
    app.add_singleton_model(|_| DisplayCount::mock());
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(|_| KeybindingChangedNotifier::new());
    app.add_singleton_model(|_ctx| RelaunchModel::new());
    app.add_singleton_model(|ctx| ChangelogModel::new(ServerApiProvider::as_ref(ctx).get()));
    app.add_singleton_model(|_| GitHubAuthNotifier::new());
    app.add_singleton_model(|_ctx| SyncedInputState::mock());
    app.add_singleton_model(|_| ResizableData::default());
    app.add_singleton_model(LocalWorkflows::new);
    app.add_singleton_model(UndoCloseStack::new);
    app.add_singleton_model(terminal::shared_session::manager::Manager::new);
    app.add_singleton_model(|_| ActiveSession::default());
    app.add_singleton_model(|_| WorkspaceToastStack);
    app.add_singleton_model(|_| ObjectActions::new(Vec::new()));
    app.add_singleton_model(NotebookKeybindings::new);
    app.add_singleton_model(TerminalKeybindings::new);
    app.add_singleton_model(NotebookManager::mock);
    app.add_singleton_model(|ctx| {
        CloudPreferencesSyncer::new(
            false,                     // force_local_wins_on_startup
            std::path::PathBuf::new(), // unused in tests that don't exercise the hash path
            ctx,
        )
    });
    app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
    // QueuedQueryModel subscribes to history events; register after the
    // history model is in place.
    app.add_singleton_model(crate::ai::blocklist::QueuedQueryModel::new);
    app.add_singleton_model(|ctx| OrchestrationPillBarModel::new(Default::default(), ctx));
    app.add_singleton_model(|_| CLIAgentSessionsModel::new());
    // The blocklist controller created during terminal bootstrap subscribes to
    // OrchestrationEventService and OrchestrationEventStreamer unconditionally,
    // so both singletons must be registered before bootstrap.
    app.add_singleton_model(
        crate::ai::blocklist::orchestration_events::OrchestrationEventService::new,
    );
    app.add_singleton_model(
        crate::ai::blocklist::orchestration_event_streamer::OrchestrationEventStreamer::new,
    );
    app.add_singleton_model(|_| ActiveAgentViewsModel::new());
    app.add_singleton_model(AgentNotificationsModel::new);
    app.add_singleton_model(AgentConversationsModel::new);
    app.add_singleton_model(SessionPermissionsManager::new);
    #[cfg(not(target_family = "wasm"))]
    app.add_singleton_model(|_| ChatGPTAuthModel::new_for_test());
    app.add_singleton_model(LLMPreferences::new);
    app.add_singleton_model(HarnessAvailabilityModel::new);
    app.add_singleton_model(|ctx| AITipModel::new_for_agent_tips(ctx));
    app.add_singleton_model(|_| SettingsPaneManager::new());
    app.add_singleton_model(|_| AIFactManager::new());

    // Initialize file-based MCP dependencies.
    app.add_singleton_model(|_| DetectedRepositories::default());
    app.add_singleton_model(HomeDirectoryWatcher::new_for_test);
    app.add_singleton_model(DirectoryWatcher::new);
    app.add_singleton_model(WarpManagedPathsWatcher::new_for_testing);
    app.add_singleton_model(FileMCPWatcher::new);
    app.add_singleton_model(|_| FileBasedMCPManager::default());

    app.add_singleton_model(|_| TemplatableMCPServerManager::default());
    #[cfg(feature = "local_fs")]
    app.add_singleton_model(FileModel::new);
    app.add_singleton_model(|ctx| {
        AIExecutionProfilesModel::new(&crate::LaunchMode::new_for_unit_test(), ctx)
    });
    app.add_singleton_model(RepoOutlines::new_for_test);
    #[cfg(feature = "voice_input")]
    app.add_singleton_model(voice_input::VoiceInput::new);
    app.add_singleton_model(BlocklistAIPermissions::new);
    app.add_singleton_model(|_| GPUState::new());
    // Register IapManager in a disabled state (no IapState). The settings
    // page's `IapManager::as_ref(ctx).is_enabled()` check panics if the
    // singleton isn't registered, even though it's a no-op on production.
    app.add_singleton_model(|ctx| {
        warp_server_client::iap::IapManager::new(
            None,
            Box::new(|_| futures::FutureExt::boxed(futures::future::ready(None::<String>))),
            ctx,
        )
    });
    app.add_singleton_model(|_| RestoredAgentConversations::new(vec![]));
    app.add_singleton_model(|ctx| {
        AIRequestUsageModel::new_for_test(ServerApiProvider::as_ref(ctx).get_ai_client(), ctx)
    });
    app.add_singleton_model(OneTimeModalModel::new);
    // Register GlobalResourceHandlesProvider before ServerExperiments which depends on it
    let global_resource_handles = GlobalResourceHandles::mock(app);
    app.add_singleton_model(|_| GlobalResourceHandlesProvider::new(global_resource_handles));
    app.add_singleton_model(|ctx| ServerExperiments::new_from_cache(vec![], ctx));
    app.add_singleton_model(DefaultTerminal::new);
    app.add_singleton_model(|_| IgnoredSuggestionsModel::new(vec![]));
    app.add_singleton_model(|_| crate::code_review::git_repo_model::GitRepoModels::new());
    app.add_singleton_model(remote_server::manager::RemoteServerManager::new);
    #[cfg(not(target_family = "wasm"))]
    app.add_singleton_model(RemoteCodebaseIndexModel::new);

    #[cfg(feature = "local_fs")]
    app.add_singleton_model(RepoMetadataModel::new);
    #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
    app.add_singleton_model(ai::index::local_project_index::LocalProjectIndexManager::new);
    app.add_singleton_model(search::files::model::FileSearchModel::new);

    #[cfg(windows)]
    {
        app.add_singleton_model(RendererState::new);
    }

    #[cfg(feature = "local_tty")]
    terminal::available_shells::register(app);
    AltScreenReporting::register(app);

    #[cfg(enable_crash_recovery)]
    crate::crash_recovery::CrashRecovery::register_for_test(app);

    app.update(experiments::init);

    app.add_singleton_model(
        crate::workspace::bonus_grant_notification_model::BonusGrantNotificationModel::new,
    );
    app.add_singleton_model(|ctx| {
        CodebaseIndexManager::new_for_test(ServerApiProvider::as_ref(ctx).get(), ctx)
    });
    // PersistedWorkspace subscribes to project-context updates during construction.
    app.add_singleton_model(|_| ProjectContextModel::default());
    app.add_singleton_model(|ctx| PersistedWorkspace::new(vec![], HashMap::new(), None, ctx));
    app.add_singleton_model(|_| PricingInfoModel::new());
    app.add_singleton_model(AIDocumentModel::new);
    app.add_singleton_model(|_| History::new(vec![]));

    // SkillManager is registered after `HomeDirectoryWatcher`, `DirectoryWatcher`,
    // `WarpManagedPathsWatcher`, `DetectedRepositories`, and `RepoMetadataModel`
    // because `SkillWatcher::new` subscribes to all of them.
    app.add_singleton_model(SkillManager::new);

    // Make sure to initialize the keybindings so that they are available for subviews
    app.update(workspace::init);
}

pub(crate) fn mock_workspace(app: &mut App) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let active_window_id = app.read(|ctx| ctx.windows().active_window());
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Empty {
                previous_active_window: active_window_id,
                shell: None,
            },
            ctx,
        )
    });
    workspace
}

fn restored_workspace(
    app: &mut App,
    window_snapshot: crate::app_state::WindowSnapshot,
) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Restored {
                window_snapshot,
                block_lists: Arc::new(HashMap::new()),
            },
            ctx,
        )
    });
    workspace
}

fn transferred_tab_workspace(
    app: &mut App,
    vertical_tabs_panel_open: bool,
) -> ViewHandle<Workspace> {
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::TransferredTab {
                tab_color: None,
                custom_title: None,
                left_panel_open: false,
                vertical_tabs_panel_open,
                right_panel_open: false,
                is_right_panel_maximized: false,
                is_tab_drag_preview: false,
            },
            ctx,
        )
    });
    workspace
}

#[test]
fn test_tab_bar_traffic_light_space_regression_for_resource_center_overlap() {
    // Regression for #10139: the Resource Center/right panel can be open on
    // Windows/Linux, but vertical-tabs and right-panel state should not decide
    // whether the tab bar reserves space for titlebar controls.
    let cases = [
        (TrafficLightSide::Left, false),
        (TrafficLightSide::Right, true),
    ];

    for (side, should_reserve_space) in cases {
        assert_eq!(
            should_reserve_traffic_light_space_in_tab_bar(side),
            should_reserve_space
        );
    }
}

#[test]
fn test_theme_chooser_does_not_suppress_tab_bar_traffic_light_padding() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let closed_padding = workspace.compute_tab_bar_left_padding(ctx);
            assert!(
                closed_padding > 0.,
                "Tab bar should reserve left padding when no left panel is open"
            );

            workspace.current_workspace_state.is_theme_chooser_open = true;
            assert_eq!(
                workspace.compute_tab_bar_left_padding(ctx),
                closed_padding,
                "Theme chooser should not be treated as a left panel for tab bar padding"
            );

            workspace.open_left_panel(ctx);
            assert_eq!(
                workspace.compute_tab_bar_left_padding(ctx),
                closed_padding,
                "Open tools panel should still reserve tab bar traffic light padding"
            );
        });
    });
}

fn assert_vertical_tabs_tools_panel_preserves_padding(config: HeaderToolbarChipSelection) {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
                report_if_error!(settings
                    .header_toolbar_chip_selection
                    .set_value(config, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let closed_padding = workspace.compute_tab_bar_left_padding(ctx);
            assert!(
                closed_padding > 0.,
                "Vertical tabs should reserve traffic light padding"
            );

            workspace.open_left_panel(ctx);
            assert_eq!(
                workspace.compute_tab_bar_left_padding(ctx),
                closed_padding,
                "An open tools panel should still reserve traffic light padding in vertical tabs"
            );
        });
    });
}

#[test]
fn test_tools_panel_does_not_suppress_vertical_tab_bar_traffic_light_padding() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    for config in [
        HeaderToolbarChipSelection::Custom {
            left: vec![HeaderToolbarItemKind::AgentManagement],
            right: vec![
                HeaderToolbarItemKind::TabsPanel,
                HeaderToolbarItemKind::ToolsPanel,
                HeaderToolbarItemKind::CodeReview,
                HeaderToolbarItemKind::NotificationsMailbox,
            ],
        },
        HeaderToolbarChipSelection::Custom {
            left: vec![
                HeaderToolbarItemKind::TabsPanel,
                HeaderToolbarItemKind::ToolsPanel,
                HeaderToolbarItemKind::AgentManagement,
            ],
            right: vec![
                HeaderToolbarItemKind::CodeReview,
                HeaderToolbarItemKind::NotificationsMailbox,
            ],
        },
    ] {
        assert_vertical_tabs_tools_panel_preserves_padding(config);
    }
}
#[cfg(feature = "local_fs")]
fn open_worktree_sidecar(workspace: &ViewHandle<Workspace>, app: &mut App) {
    workspace.update(app, |workspace, ctx| {
        workspace.open_new_session_dropdown_menu(
            crate::workspace::action::NewSessionMenuAnchor::AddTabButton(Vector2F::zero()),
            ctx,
        );

        let worktree_index = workspace
            .new_session_dropdown_menu
            .read(ctx, |menu, _| {
                menu.items().iter().position(|item| {
                    matches!(
                        item,
                        MenuItem::Item(fields) if fields.label() == "New worktree config"
                    )
                })
            })
            .expect("expected new worktree config item in new-session menu");

        workspace
            .new_session_dropdown_menu
            .update(ctx, |menu, view_ctx| {
                menu.set_selected_by_index(worktree_index, view_ctx);
            });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_hover_takes_precedence_over_selection() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        let beta_repo = temp_root.path().join("beta-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");
        std::fs::create_dir_all(&beta_repo).expect("failed to create beta repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
                persisted.user_added_workspace(beta_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .new_session_sidecar_menu
                .update(ctx, |menu, view_ctx| {
                    menu.set_selected_by_index(1, view_ctx);
                    menu.handle_action(
                        &crate::menu::MenuAction::HoverSubmenuLeafNode {
                            depth: 0,
                            row_index: 2,
                            position: Vector2F::zero(),
                        },
                        view_ctx,
                    );
                });

            workspace.handle_new_session_sidecar_event(&MenuEvent::ItemHovered, ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(2)
            );
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_pointer_entry_does_not_select_top_repo() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        let beta_repo = temp_root.path().join("beta-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");
        std::fs::create_dir_all(&beta_repo).expect("failed to create beta repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
                persisted.user_added_workspace(beta_repo.clone(), ctx);
            });
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_new_session_dropdown_menu(
                crate::workspace::action::NewSessionMenuAnchor::AddTabButton(Vector2F::zero()),
                ctx,
            );

            let worktree_index = workspace
                .new_session_dropdown_menu
                .read(ctx, |menu, _| {
                    menu.items().iter().position(|item| {
                        matches!(
                            item,
                            MenuItem::Item(fields) if fields.label() == "New worktree config"
                        )
                    })
                })
                .expect("expected new worktree config item in new-session menu");

            workspace
                .new_session_dropdown_menu
                .update(ctx, |menu, view_ctx| {
                    menu.handle_action(
                        &crate::menu::MenuAction::HoverSubmenuWithChildren(
                            0,
                            crate::menu::SelectAction::Index {
                                row: worktree_index,
                                item: 0,
                            },
                        ),
                        view_ctx,
                    );
                });
            workspace.update_new_session_sidecar(ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_new_session_sidecar);
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                None
            );
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_close_via_select_item_executes_from_workspace() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        let _cleanup = TabConfigCleanupGuard::new("alpha-repo");

        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .new_session_sidecar_menu
                .update(ctx, |menu, view_ctx| {
                    menu.set_selected_by_index(1, view_ctx);
                });
            workspace.handle_new_session_sidecar_event(
                &MenuEvent::Close {
                    via_select_item: true,
                },
                ctx,
            );
            workspace.handle_new_session_sidecar_event(&MenuEvent::ItemSelected, ctx);
        });

        workspace.read(&app, |workspace, _| {
            assert_eq!(workspace.tab_count(), 2);
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_open_file_notebook_focuses_existing_markdown_pane() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let markdown_path = temp_dir.path().join("README.md");
        std::fs::write(&markdown_path, "# Test\n").expect("failed to write markdown file");

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_file_with_target(
                markdown_path.clone(),
                FileTarget::MarkdownViewer(EditorLayout::SplitPane),
                None,
                CodeSource::Link {
                    path: markdown_path.clone(),
                    range_start: None,
                    range_end: None,
                },
                ctx,
            );
        });

        let markdown_pane_id = workspace.update(&mut app, |workspace, ctx| {
            let pane_group = workspace.active_tab_pane_group();
            pane_group.update(ctx, |pane_group, ctx| {
                let markdown_panes = pane_group.file_notebook_panes(ctx).collect_vec();
                assert_eq!(markdown_panes.len(), 1);
                let pane_id = markdown_panes[0].0;

                pane_group.add_terminal_pane(Direction::Right, None, ctx);
                assert_ne!(pane_group.focused_pane_id(ctx), pane_id);

                pane_id
            })
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_file_with_target(
                markdown_path.clone(),
                FileTarget::MarkdownViewer(EditorLayout::SplitPane),
                None,
                CodeSource::Link {
                    path: markdown_path,
                    range_start: None,
                    range_end: None,
                },
                ctx,
            );
        });

        workspace.read(&app, |workspace, ctx| {
            let pane_group = workspace.active_tab_pane_group().as_ref(ctx);
            assert_eq!(pane_group.file_notebook_panes(ctx).count(), 1);
            assert_eq!(pane_group.focused_pane_id(ctx), markdown_pane_id);
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_search_editor_enter_executes_selection() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        let _cleanup = TabConfigCleanupGuard::new("alpha-repo");

        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Enter);
                });
        });

        workspace.read(&app, |workspace, _| {
            assert_eq!(workspace.tab_count(), 2);
            assert!(workspace.show_new_session_dropdown_menu.is_none());
        });
    });
}

/// RAII guard that removes tab config TOML files whose name starts with
/// `prefix` from `~/.warp/tab_configs/` on drop. Because `Drop` runs even
/// when a test panics, this prevents stale worktree configs from leaking
/// into Warp dev.
#[cfg(feature = "local_fs")]
struct TabConfigCleanupGuard {
    prefix: &'static str,
}

#[cfg(feature = "local_fs")]
impl TabConfigCleanupGuard {
    fn new(prefix: &'static str) -> Self {
        // Eagerly clean up leftovers from any previously-crashed run.
        Self::clean(prefix);
        Self { prefix }
    }

    fn clean(prefix: &str) {
        let dir = tab_configs_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(prefix) && name.ends_with(".toml") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

#[cfg(feature = "local_fs")]
impl Drop for TabConfigCleanupGuard {
    fn drop(&mut self) {
        Self::clean(self.prefix);
    }
}

/// Creates a workspace with a single, shared session.
fn mock_workspace_with_shared_session(app: &mut App) -> ViewHandle<Workspace> {
    use crate::terminal::shared_session::manager::Manager;

    // Create the workspace as a session-sharing sharer.
    let global_resource_handles = GlobalResourceHandles::mock(app);
    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::Empty {
                previous_active_window: None,
                shell: None,
            },
            ctx,
        )
    });

    // Get the single terminal view in the workspace.
    let terminal_view = workspace.read(app, |workspace, ctx| {
        assert_eq!(workspace.tabs.len(), 1);
        workspace
            .active_tab_pane_group()
            .as_ref(ctx)
            .focused_session_view(ctx)
            .unwrap()
    });

    terminal_view.update(app, |view, ctx| {
        view.model.lock().block_list_mut().set_bootstrapped();
        view.attempt_to_share_session(
            SharedSessionScrollbackType::All,
            None,
            SharedSessionSource::user(None),
            false,
            ctx,
        );
    });

    // Make sure the view is registered with the shared session manager.
    app.read(|ctx| {
        let manager = Manager::as_ref(ctx);
        let shared_sessions = manager.shared_views(ctx).collect_vec();
        assert_eq!(shared_sessions.len(), 1);
        assert_eq!(shared_sessions[0].id(), terminal_view.id());
    });

    workspace
}

// Creates a workspace as a viewer of a shared session.
fn mock_workspace_viewing_shared_session(app: &mut App) -> ViewHandle<Workspace> {
    // Create the workspace as a session-sharing sharer.
    let global_resource_handles = GlobalResourceHandles::mock(app);

    let session_id = SessionId::new();

    let (_, workspace) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        Workspace::new(
            global_resource_handles,
            None,
            NewWorkspaceSource::SharedSessionAsViewer { session_id },
            ctx,
        )
    });

    // Get the single terminal view in the workspace.
    let terminal_view = workspace.read(app, |workspace, ctx| {
        assert_eq!(workspace.tabs.len(), 1);
        workspace
            .active_tab_pane_group()
            .as_ref(ctx)
            .focused_session_view(ctx)
            .unwrap()
    });

    // Ensure session is opened as a viewer.
    terminal_view.read(app, |terminal, _ctx| {
        let model = terminal.model.clone();
        assert!(model.lock().shared_session_status().is_viewer());
    });

    workspace
}

/// Disable the warn-before-quit setting. Because we don't fully bootstrap the shell in tests, this
/// is generally needed in tests that close tabs.
fn disable_quit_warning(app: &mut AppContext) {
    GeneralSettings::handle(app).update(app, |settings, ctx| {
        settings
            .show_warning_before_quitting
            .set_value(false, ctx)
            .expect("Failed to disable quit warning");
    });
}

fn get_newly_created_pane_id(panes: &PaneGroup, existing_ids: &[PaneId]) -> PaneId {
    panes
        .pane_ids()
        .find(|id| !existing_ids.contains(id))
        .unwrap()
}

fn split_pane_state(
    panes: &PaneGroup,
    pane_id: impl Into<PaneId>,
    ctx: &AppContext,
) -> SplitPaneState {
    // Split pane state is now inferred from the pane group's focus state
    panes
        .focus_state_handle()
        .as_ref(ctx)
        .split_pane_state_for(pane_id.into())
}

fn active_session_state(
    panes: &PaneGroup,
    pane_id: TerminalPaneId,
    ctx: &AppContext,
) -> ActiveSessionState {
    if panes
        .terminal_view_from_pane_id(pane_id, ctx)
        .expect("Not a terminal pane")
        .as_ref(ctx)
        .is_active_session(ctx)
    {
        ActiveSessionState::Active
    } else {
        ActiveSessionState::Inactive
    }
}

#[test]
fn restore_conversation_in_active_pane_enters_existing_live_conversation_without_loading() {
    let _agent_view = FeatureFlag::AgentView.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let terminal_view = workspace.read(&app, |workspace, ctx| {
            workspace
                .active_tab_pane_group()
                .as_ref(ctx)
                .focused_session_view(ctx)
                .expect("workspace should start with a terminal view")
        });
        let terminal_view_id = terminal_view.read(&app, |view, _| view.view_id());
        let conversation_id =
            BlocklistAIHistoryModel::handle(&app).update(&mut app, |history, ctx| {
                history.start_new_conversation(terminal_view_id, false, false, false, ctx)
            });

        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.tab_count(), 1);

            workspace.restore_conversation_in_active_pane(conversation_id, false, ctx);

            assert_eq!(workspace.tab_count(), 1);
        });

        terminal_view.read(&app, |view, ctx| {
            assert_eq!(view.active_conversation_id(ctx), Some(conversation_id));
            assert_eq!(
                view.model.lock().conversation_transcript_viewer_status(),
                None
            );
        });
    });
}
fn new_session_menu_label(item: &MenuItem<WorkspaceAction>) -> String {
    match item {
        MenuItem::Item(fields) => fields.label().to_string(),
        MenuItem::Separator => "---".to_string(),
        MenuItem::ItemsRow { items } => items
            .iter()
            .map(|fields| fields.label().to_string())
            .collect::<Vec<_>>()
            .join(" | "),
        MenuItem::Submenu { fields, .. } => fields.label().to_string(),
        MenuItem::Header { fields, .. } => fields.label().to_string(),
    }
}

fn reopen_closed_session_menu_item(
    menu_items: &[MenuItem<WorkspaceAction>],
) -> &MenuItemFields<WorkspaceAction> {
    match menu_items.last() {
        Some(MenuItem::Item(fields)) if fields.label() == "Reopen closed session" => fields,
        _ => panic!("expected Reopen closed session to be the last new-session menu item"),
    }
}

#[test]
fn test_reward_modal_no_overlap() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        // Trigger the referral reward response
        workspace.update(&mut app, |view, ctx| {
            view.handle_referral_theme_status_event(
                &ReferralThemeEvent::SentReferralThemeActivated,
                ctx,
            );

            // This _should_ show the reward modal, since the changelog modal is _not_ active
            assert!(view.current_workspace_state.is_reward_modal_open);
        });
    });
}

#[test]
fn test_reward_modal_shows_for_received_referral() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |view, ctx| {
            view.handle_referral_theme_status_event(
                &ReferralThemeEvent::ReceivedReferralThemeActivated,
                ctx,
            );

            assert!(view.current_workspace_state.is_reward_modal_open);
        });
    });
}

#[test]
fn test_tab_renaming_editor_selections() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        // Add second tab and rename both of them to prepare for the test
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.rename_tab_internal(0, "short_title", ctx);
            let selected_text = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("short_title", selected_text);

            // Ensure that whatever is selected, is the full title and not the leftover from
            // the previous, shorter one.
            workspace.rename_tab_internal(1, "very_long_title_this_is", ctx);
            let selected_text = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("very_long_title_this_is", selected_text);

            // Ensure that if we escape, the current editor's contents is going to be cleared
            // as well.
            workspace.handle_tab_rename_editor_event(&Event::Escape, ctx);
            let selected_text = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("", selected_text);
        });
    });
}

#[test]
fn test_tab_renaming_editor_reset() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.rename_tab_internal(0, "short_title", ctx);
            workspace.rename_tab_internal(1, "very_long_title_this_is", ctx);

            // Ensure that when the editor is initially not empty, it will be cleared before a user renames a tab
            workspace.tab_rename_editor.update(ctx, |editor, ctx| {
                editor.insert_selected_text("some-text", ctx);
            });
            workspace.rename_tab_internal(1, "new_very_long_title", ctx);
            let selected_text: String = workspace
                .tab_rename_editor
                .read(ctx, |editor, ctx| editor.selected_text(ctx));
            assert_eq!("new_very_long_title", selected_text);
        });
    });
}

#[test]
fn test_set_active_tab_name() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);

            workspace.handle_action(
                &WorkspaceAction::SetActiveTabName("  Backend API  ".to_string()),
                ctx,
            );
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .display_title(ctx),
                "Backend API"
            );
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Backend API")
            );

            workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);
            assert_ne!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Backend API")
            );

            workspace.handle_action(&WorkspaceAction::ActivateTab(1), ctx);
            workspace.handle_action(&WorkspaceAction::SetActiveTabName("   ".to_string()), ctx);
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx)
                    .as_deref(),
                Some("Backend API")
            );
        });
    });
}

#[test]
fn test_set_active_tab_name_clears_active_rename_editor_state() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.rename_tab_internal(0, "old title", ctx);
            assert!(workspace.current_workspace_state.is_tab_being_renamed());

            workspace.handle_action(
                &WorkspaceAction::SetActiveTabName("new title".to_string()),
                ctx,
            );

            assert!(!workspace.current_workspace_state.is_tab_being_renamed());
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .display_title(ctx),
                "new title"
            );
        });
    });
}

#[test]
fn test_set_active_tab_color() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            let active = workspace.active_tab_index;

            // Setting a color stores it as the manual selection and resolves to it.
            workspace.handle_action(
                &WorkspaceAction::SetActiveTabColor(SelectedTabColor::Color(
                    AnsiColorIdentifier::Magenta,
                )),
                ctx,
            );
            assert_eq!(
                workspace.tabs[active].selected_color,
                SelectedTabColor::Color(AnsiColorIdentifier::Magenta),
            );
            assert_eq!(
                workspace.tabs[active].color(),
                Some(AnsiColorIdentifier::Magenta),
            );

            // Replacing with a different color overwrites the previous selection.
            workspace.handle_action(
                &WorkspaceAction::SetActiveTabColor(SelectedTabColor::Color(
                    AnsiColorIdentifier::Green,
                )),
                ctx,
            );
            assert_eq!(
                workspace.tabs[active].selected_color,
                SelectedTabColor::Color(AnsiColorIdentifier::Green),
            );

            // `Cleared` explicitly suppresses any color (including a directory default).
            workspace.handle_action(
                &WorkspaceAction::SetActiveTabColor(SelectedTabColor::Cleared),
                ctx,
            );
            assert_eq!(
                workspace.tabs[active].selected_color,
                SelectedTabColor::Cleared,
            );
            assert_eq!(workspace.tabs[active].color(), None);

            // `Unset` removes the manual override so a directory default could apply.
            // With no directory default configured, the resolved color is still `None`.
            workspace.handle_action(
                &WorkspaceAction::SetActiveTabColor(SelectedTabColor::Unset),
                ctx,
            );
            assert_eq!(
                workspace.tabs[active].selected_color,
                SelectedTabColor::Unset,
            );
            assert_eq!(workspace.tabs[active].color(), None);

            // Action targets the active tab — switching to tab 0 leaves the second tab
            // unaffected.
            workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);
            workspace.handle_action(
                &WorkspaceAction::SetActiveTabColor(SelectedTabColor::Color(
                    AnsiColorIdentifier::Blue,
                )),
                ctx,
            );
            assert_eq!(
                workspace.tabs[0].selected_color,
                SelectedTabColor::Color(AnsiColorIdentifier::Blue),
            );
            assert_eq!(
                workspace.tabs[active].selected_color,
                SelectedTabColor::Unset,
            );
        });
    });
}

#[test]
fn test_workspace_sessions_retrieves_tabs() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let pane_id = workspace
                .get_pane_group_view(0)
                .map(|tab| tab.read(ctx, |tab, _ctx| tab.pane_id_by_index(0).unwrap()))
                .expect("WindowId was not retrieved.");

            assert!(workspace
                .workspace_sessions(ctx.window_id(), ctx)
                .any(|x| { x.pane_view_locator().pane_id == pane_id }));

            // Add a tab and check if workspace_sessions finds the second session from the new tab.
            workspace.add_terminal_tab(false, ctx);
            let new_pane_id = workspace
                .get_pane_group_view(1)
                .map(|tab| tab.read(ctx, |tab, _ctx| tab.pane_id_by_index(0).unwrap()))
                .expect("WindowId was not retrieved.");

            assert!(workspace
                .workspace_sessions(ctx.window_id(), ctx)
                .any(|x| { x.pane_view_locator().pane_id == new_pane_id }));
        });
    });
}

#[test]
fn test_workspace_sessions_retrieves_panes() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Add a new split pane to the right.
            if let Some(tab_view) = workspace.get_pane_group_view(0) {
                tab_view.update(ctx, |view, ctx| {
                    view.handle_action(&PaneGroupAction::Add(Direction::Right), ctx);
                })
            }

            // Get the EntityId of the new pane added to the current tab.
            let new_pane_id = workspace
                .get_pane_group_view(0)
                .map(|tab| tab.read(ctx, |tab, _ctx| tab.pane_id_by_index(1).unwrap()))
                .expect("WindowId was not retrieved.");
            assert!(workspace
                .workspace_sessions(ctx.window_id(), ctx)
                .any(|x| { x.pane_view_locator().pane_id == new_pane_id }));
        });
    });
}

fn number_of_shared_sessions_in_tab(
    workspace: &Workspace,
    index: usize,
    ctx: &AppContext,
) -> usize {
    workspace
        .get_pane_group_view(index)
        .map_or(0, |view| view.as_ref(ctx).number_of_shared_sessions(ctx))
}

/// Sets up the workspace with three tabs. The middle tab has two panes, where one is shared.
fn setup_session_sharing_test(workspace: &ViewHandle<Workspace>, app: &mut App) -> PaneId {
    let shared_pane_id = workspace.update(app, |workspace, ctx| {
        workspace.add_terminal_tab(false, ctx);
        workspace.add_terminal_tab(false, ctx);

        let tab_view = workspace.get_pane_group_view(1).unwrap();

        tab_view.update(ctx, |view, ctx| {
            assert_eq!(view.pane_count(), 1);
            view.focused_session_view(ctx)
                .unwrap()
                .update(ctx, |terminal, ctx| {
                    terminal.attempt_to_share_session(
                        SharedSessionScrollbackType::None,
                        None,
                        SharedSessionSource::user(None),
                        false,
                        ctx,
                    );
                });

            view.handle_action(&PaneGroupAction::Add(Direction::Right), ctx);
            assert_eq!(view.pane_count(), 2);

            view.pane_id_by_index(0).unwrap()
        })
    });

    workspace.read(app, |workspace, ctx| {
        assert_eq!(number_of_shared_sessions_in_tab(workspace, 1, ctx), 1);

        // Confirmation dialog starts not open.
        assert!(
            !workspace
                .current_workspace_state
                .is_close_session_confirmation_dialog_open
        );
    });

    shared_pane_id
}

#[test]
fn test_close_tab_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let first_tab_id = workspace.get_pane_group_view(0).unwrap().id();

            // Trying to close tab with a shared pane opens dialog.
            workspace.handle_action(&WorkspaceAction::CloseTab(1), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User clicking cancel closes dialog.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::Cancel,
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // Trying to close tab without a shared pane goes through without dialog.
            workspace.handle_action(&WorkspaceAction::CloseTab(2), ctx);
            assert_eq!(workspace.tab_count(), 2);
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // Close the tab with the shared pane.
            workspace.handle_action(&WorkspaceAction::CloseTab(1), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseTab { tab_index: 1 },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.get_pane_group_view(0).unwrap().id(), first_tab_id);
        });
    });
}

#[test]
fn test_close_active_horizontal_tab_activates_tab_to_right() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            let tab_to_right_id = workspace.get_pane_group_view(2).unwrap().id();

            workspace.activate_tab(1, ctx);
            workspace.close_tab(1, true, true, ctx);

            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(workspace.active_tab_index(), 1);
            assert_eq!(workspace.active_tab_pane_group().id(), tab_to_right_id);
        });
    });
}

#[test]
fn test_close_last_horizontal_tab_activates_tab_to_left() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            let tab_to_left_id = workspace.get_pane_group_view(1).unwrap().id();

            workspace.activate_tab(2, ctx);
            workspace.close_tab(2, true, true, ctx);

            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(workspace.active_tab_index(), 1);
            assert_eq!(workspace.active_tab_pane_group().id(), tab_to_left_id);
        });
    });
}

#[test]
fn test_close_active_vertical_tab_activates_tab_below() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            let tab_below_id = workspace.get_pane_group_view(2).unwrap().id();

            workspace.activate_tab(1, ctx);
            workspace.close_tab(1, true, true, ctx);

            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(workspace.active_tab_index(), 1);
            assert_eq!(workspace.active_tab_pane_group().id(), tab_below_id);
        });
    });
}

#[test]
fn test_close_last_vertical_tab_activates_tab_above() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            let tab_above_id = workspace.get_pane_group_view(1).unwrap().id();

            workspace.activate_tab(2, ctx);
            workspace.close_tab(2, true, true, ctx);

            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(workspace.active_tab_index(), 1);
            assert_eq!(workspace.active_tab_pane_group().id(), tab_above_id);
        });
    });
}

#[test]
fn test_close_pane_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let shared_pane_id = setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let shared_pane_group_id = workspace.get_pane_group_view(1).unwrap().id();

            // User tries to close shared pane, dialog comes up.
            workspace.handle_file_tree_event(
                workspace.get_pane_group_view(1).unwrap().clone(),
                &pane_group::Event::CloseSharedSessionPaneRequested {
                    pane_id: shared_pane_id,
                },
                ctx,
            );
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::ClosePane {
                        pane_group_id: shared_pane_group_id,
                        pane_id: shared_pane_id,
                    },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(number_of_shared_sessions_in_tab(workspace, 1, ctx), 0);
            let remaining_pane_id = workspace
                .get_pane_group_view_with_id(shared_pane_group_id)
                .unwrap()
                .as_ref(ctx)
                .pane_id_by_index(0)
                .unwrap();
            assert_ne!(remaining_pane_id, shared_pane_id);
            assert_eq!(workspace.tab_count(), 3);
        });
    });
}

#[test]
fn test_reopen_closed_shared_tab() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let shared_pane_group = workspace.get_pane_group_view(1).unwrap().clone();

            // Close the tab with the shared pane.
            workspace.close_tab(1, true, true, ctx);
            assert_eq!(workspace.tab_count(), 2);

            // Restore the shared tab.
            workspace.restore_closed_tab(1, TabData::new(shared_pane_group.to_owned()), ctx);
        });
        // Restored tab should no longer be shared.
        workspace.read(&app, |workspace, ctx| {
            let pane_group = workspace.get_pane_group_view(1).unwrap();
            assert!(!pane_group.as_ref(ctx).is_terminal_pane_being_shared(ctx));
            assert_eq!(workspace.tab_count(), 3);
        })
    });
}

#[test]
fn test_close_other_tabs_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let last_tab_id = workspace.get_pane_group_view(2).unwrap().id();

            // User tries to close other tabs choosing non-shared tab, dialog comes up.
            workspace.handle_action(&WorkspaceAction::CloseOtherTabs(2), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseOtherTabs { tab_index: 2 },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.get_pane_group_view(0).unwrap().id(), last_tab_id);
        });
    });
}

#[test]
fn test_closing_tab_context_menu_restores_active_tab_focus() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let (window_id, menu) = workspace.update(&mut app, |workspace, ctx| {
            workspace.toggle_tab_right_click_menu(
                0,
                TabContextMenuAnchor::Pointer(Vector2F::zero()),
                ctx,
            );
            (ctx.window_id(), workspace.tab_right_click_menu.clone())
        });
        assert_eq!(app.focused_view_id(window_id), Some(menu.id()));

        menu.update(&mut app, |menu, ctx| {
            menu.handle_action(&MenuAction::Close(false), ctx);
        });
        workspace.update(&mut app, |workspace, ctx| {
            assert!(workspace.show_tab_right_click_menu.is_none());
            assert!(workspace
                .active_tab_pane_group()
                .is_self_or_child_focused(ctx));
        });
    });
}

#[test]
fn test_selecting_rename_from_tab_context_menu_preserves_editor_focus() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let menu = workspace.update(&mut app, |workspace, ctx| {
            workspace.toggle_tab_right_click_menu(
                0,
                TabContextMenuAnchor::Pointer(Vector2F::zero()),
                ctx,
            );
            workspace.tab_right_click_menu.clone()
        });

        menu.update(&mut app, |menu, ctx| {
            assert!(menu.set_selected_by_name("Rename tab", ctx));
            menu.handle_action(&MenuAction::Enter, ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_tab_right_click_menu.is_none());
            assert_eq!(
                workspace.current_workspace_state.tab_being_renamed(),
                Some(0)
            );
            assert!(workspace.tab_rename_editor.is_focused(ctx));
        });
    });
}

#[test]
fn test_selecting_rename_from_pane_context_menu_preserves_editor_focus() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let (menu, locator) = workspace.update(&mut app, |workspace, ctx| {
            let pane_group = workspace.active_tab_pane_group();
            let locator = PaneViewLocator {
                pane_group_id: pane_group.id(),
                pane_id: pane_group.as_ref(ctx).pane_id_from_index(0).unwrap(),
            };
            workspace.toggle_vertical_tabs_pane_context_menu(
                0,
                VerticalTabsPaneContextMenuTarget::ClickedPane(locator),
                Vector2F::zero(),
                ctx,
            );
            (workspace.tab_right_click_menu.clone(), locator)
        });

        menu.update(&mut app, |menu, ctx| {
            assert!(menu.set_selected_by_name("Rename pane", ctx));
            menu.handle_action(&MenuAction::Enter, ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_tab_right_click_menu.is_none());
            assert_eq!(
                workspace.current_workspace_state.pane_being_renamed(),
                Some(locator)
            );
            assert!(workspace.pane_rename_editor.is_focused(ctx));
        });
    });
}

#[test]
fn test_selecting_rename_from_tab_group_context_menu_preserves_editor_focus() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let menu = workspace.update(&mut app, |workspace, ctx| {
            let group = TabGroup::new();
            let group_id = group.id;
            workspace.tab_groups.insert(group_id, group);
            workspace.tabs[0].group_id = Some(group_id);
            workspace.toggle_tab_group_right_click_menu(
                group_id,
                TabContextMenuAnchor::Pointer(Vector2F::zero()),
                ctx,
            );
            workspace.tab_right_click_menu.clone()
        });

        menu.update(&mut app, |menu, ctx| {
            assert!(menu.set_selected_by_name("Rename", ctx));
            menu.handle_action(&MenuAction::Enter, ctx);
        });

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_tab_group_right_click_menu.is_none());
            assert!(workspace
                .current_workspace_state
                .is_any_tab_group_being_renamed());
            assert!(workspace.tab_group_rename_editor.is_focused(ctx));
        });
    });
}

#[test]
fn test_close_tabs_right_confirmation_dialog() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let first_tab_id = workspace.get_pane_group_view(0).unwrap().id();

            // User tries to close all tabs right of the left-most tab, dialog comes up.
            workspace.handle_action(&WorkspaceAction::CloseTabsRight(0), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms.
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseTabsDirection {
                        tab_index: 0,
                        direction: TabMovement::Right,
                    },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.get_pane_group_view(0).unwrap().id(), first_tab_id);
        });
    });
}

#[test]
fn test_confirmation_dialog_dont_show_again() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Close the tab with the shared pane, dialog comes up
            workspace.handle_action(&WorkspaceAction::CloseTab(1), ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );

            // User confirms, checking "Don't show again".
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: true,
                    open_confirmation_source: OpenDialogSource::CloseTab { tab_index: 1 },
                },
                ctx,
            );
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 2);

            // Share the first tab
            let tab_view = workspace.get_pane_group_view(0).unwrap();
            tab_view.update(ctx, |view, ctx| {
                view.terminal_manager(0, ctx)
                    .unwrap()
                    .as_ref(ctx)
                    .model()
                    .lock()
                    .set_shared_session_status(SharedSessionStatus::ActiveSharer);
            });

            // Close the shared tab. No dialog should come up and action should go through.
            workspace.handle_action(&WorkspaceAction::CloseActiveTab, ctx);
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
            assert_eq!(workspace.tab_count(), 1);
        });
    });
}

#[test]
fn test_close_last_tab_skip_confirmation() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);

        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Close the non-shared tabs so there's just one shared tab left.
            workspace.handle_action(&WorkspaceAction::CloseTab(2), ctx);
            workspace.handle_action(&WorkspaceAction::CloseTab(0), ctx);
            assert_eq!(workspace.tab_count(), 1);
            // Close the last remaining tab with the shared pane, no dialog should come up because
            // we're going to close the window and there's already a confirmation on window close.
            workspace.handle_action(&WorkspaceAction::CloseActiveTab, ctx);
            assert!(
                !workspace
                    .current_workspace_state
                    .is_close_session_confirmation_dialog_open
            );
        });
    });
}

#[test]
fn test_notebook_pane_tracking() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Add a new notebook pane.
            workspace.open_notebook(
                &NotebookSource::New {
                    title: None,
                    owner: Owner::mock_current_user(),
                    initial_folder_id: None,
                },
                &OpenGalaxyDriveObjectSettings::default(),
                ctx,
                true,
            );

            // Get the ID of the new notebook.
            let pane_group = workspace
                .get_pane_group_view(0)
                .expect("Pane group does not exist")
                .clone();
            let notebook_view = pane_group
                .as_ref(ctx)
                .notebook_view_at_pane_index(0, ctx)
                .expect("Notebook view was not created")
                .clone();
            let notebook_pane_id = pane_group
                .as_ref(ctx)
                .pane_id_from_index(0)
                .expect("Notebook view should have been created");
            let notebook_id = notebook_view
                .as_ref(ctx)
                .notebook_id(ctx)
                .expect("Notebook should have an ID");

            // The notebook should be registered with the NotebookManager.
            let (window, locator) = NotebookManager::as_ref(ctx)
                .find_pane(&NotebookSource::Existing(notebook_id))
                .expect("Notebook pane should be registered");
            assert_eq!(window, ctx.window_id());
            assert_eq!(
                locator,
                PaneViewLocator {
                    pane_group_id: pane_group.id(),
                    pane_id: notebook_pane_id,
                }
            );

            // Re-opening the notebook should not create a new view.
            workspace.open_notebook(
                &NotebookSource::Existing(notebook_id),
                &OpenGalaxyDriveObjectSettings::default(),
                ctx,
                true,
            );
            assert_eq!(
                ctx.views_of_type::<NotebookView>(ctx.window_id()),
                Some(vec![notebook_view])
            );

            // Finally, closing the notebook pane should de-register it.
            pane_group.update(ctx, |pane_group, ctx| {
                pane_group.handle_action(&PaneGroupAction::RemoveActive, ctx)
            });
            assert_eq!(
                NotebookManager::handle(ctx)
                    .as_ref(ctx)
                    .find_pane(&NotebookSource::Existing(notebook_id)),
                None
            );
        });
    });
}

#[test]
fn test_set_active_terminal_input_contents_and_focus_app() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let initial_buffer_contents = workspace
                .get_active_input_view_handle(ctx)
                .map(|input_view_handle| input_view_handle.as_ref(ctx).buffer_text(ctx))
                .expect("There should be an active input view");
            assert_eq!(
                "", initial_buffer_contents,
                "initial active input should be empty"
            );

            workspace.set_active_terminal_input_contents_and_focus_app("foobar", ctx);

            assert_eq!(
                "foobar",
                workspace
                    .get_active_input_view_handle(ctx)
                    .map(|input_view_handle| input_view_handle.as_ref(ctx).buffer_text(ctx))
                    .expect("There should be an active input view")
            );
            assert!(ctx.windows().app_is_active());
        });
    });
}

/// Ensures that the terminal model is destroyed when it is no longer needed.
/// This is only a "workspace" test because we want to mimic what a normal
/// user would do and expect (e.g. close a tab and expect that its backing
/// data is correctly deallocated).
///
/// TODO(suraj): we may also want to investigate a more "real" integration test
/// that inspects the application process's overall memory consumption
/// instead of just the terminal model, but this is not easy because
/// 1. we want to measure non-shared memory (i.e. the "memory" value in Activity Monitor)
///    which is not easy; it's easier to measure "real memory" or RSS, but that includes
///    shared memory across processes.
/// 2. the test might be flaky depending on how much memory is actually allocated vs
///    freed up (not something easily controlled).
///
/// For now, this test is still useful because the terminal model is one of the largest data structures
/// maintained by our app, so we want to ensure we're not introducing regressions that cause it to not
/// be deallocated correctly.
#[test]
fn test_terminal_model_isnt_leaked() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Turn off undo-close so that we don't need to wait for deallocation.
        UndoCloseSettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .enabled
                .set_value(false, ctx)
                .expect("Can turn off undo-close via settings.")
        });

        let workspace = mock_workspace(&mut app);

        let terminal_model = workspace.update(&mut app, |workspace, ctx| {
            // Add another tab so that the workspace isn't destroyed when we close the tab.
            workspace.add_terminal_tab(false, ctx);

            // Get a weak reference to the model.
            let model = workspace.get_active_session_terminal_model(ctx).unwrap();
            Arc::downgrade(&model)
        });

        workspace.update(&mut app, |workspace, ctx| {
            // Remove the tab. This should destroy the corresponding terminal view.
            workspace.remove_tab(workspace.active_tab_index(), true, true, ctx);
        });
        // For some reason, the update call above results in more pending effects, one of which
        // contains the actual logic that drops the `TerminalModel`.
        app.update(|_| ());

        // If we can't upgrade the weak reference, that means it was in fact destructed.
        assert!(
            terminal_model.upgrade().is_none(),
            "The terminal model should not exist once the tab is closed."
        )
    });
}

#[test]
fn test_open_or_toggle_warp_drive() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // First, unconditionally open Warp Drive as a system action. WD should be open and welcome tips should not have opening warp drive.
            workspace.open_or_toggle_warp_drive(
                false, /* toggle */
                false, /* explicit_user_action */
                ctx,
            );
            assert!(
                workspace.current_workspace_state.is_warp_drive_open,
                "Warp Drive should be open"
            );
            assert!(
                !workspace
                    .tips_completed
                    .as_ref(ctx)
                    .features_used
                    .contains(&Tip::Action(TipAction::OpenGalaxyDrive)),
                "Warp drive welcome tip should not be completed"
            );

            // Next, toggle warp drive as a user action. WD should be closed and tip should not be filled out.
            workspace.open_or_toggle_warp_drive(
                true, /* toggle */
                true, /* explicit_user_action */
                ctx,
            );
            assert!(
                !workspace.current_workspace_state.is_warp_drive_open,
                "Warp Drive should be closed"
            );
            assert!(
                !workspace
                    .tips_completed
                    .as_ref(ctx)
                    .features_used
                    .contains(&Tip::Action(TipAction::OpenGalaxyDrive)),
                "Warp drive welcome tip should not be completed"
            );

            // Finally, toggle warp drive again as a user action. WD should be open and tip filled out.
            workspace.open_or_toggle_warp_drive(
                true, /* toggle */
                true, /* explicit_user_action */
                ctx,
            );
            assert!(
                workspace.current_workspace_state.is_warp_drive_open,
                "Warp Drive should be open"
            );
            assert!(
                workspace
                    .tips_completed
                    .as_ref(ctx)
                    .features_used
                    .contains(&Tip::Action(TipAction::OpenGalaxyDrive)),
                "Warp drive welcome tip should not be completed"
            );
        });
    });
}

#[test]
fn test_stop_sharing_session() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Create a workspace with a single session that's shared.
        let workspace = mock_workspace_with_shared_session(&mut app);
        let terminal_view = workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.tabs.len(), 1);
            workspace
                .active_tab_pane_group()
                .as_ref(ctx)
                .focused_session_view(ctx)
                .unwrap()
        });

        // Stop sharing the shared session.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.stop_sharing_session(
                &terminal_view.id(),
                SharedSessionActionSource::Tab,
                ctx,
            );
        });

        // Ensure that the session is no longer registered with the shared session manager.
        app.read(|ctx| {
            let manager = Manager::as_ref(ctx);
            let shared_sessions = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_sessions.len(), 0);
        });
    });
}

#[test]
fn test_stop_sharing_all_sessions_in_tab() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Create a workspace with two tabs. First tab has two shared sessions. Second tab has one shared session.
        let workspace = mock_workspace_with_shared_session(&mut app);
        let second_tab_session = workspace.update(&mut app, |workspace, ctx| {
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group.handle_action(&PaneGroupAction::Add(Direction::Right), ctx);
                    pane_group
                        .terminal_view_at_pane_index(1, ctx)
                        .unwrap()
                        .update(ctx, |terminal_view, ctx| {
                            terminal_view.attempt_to_share_session(
                                SharedSessionScrollbackType::None,
                                None,
                                SharedSessionSource::user(None),
                                false,
                                ctx,
                            );
                        });
                });

            workspace.add_terminal_tab(false, ctx);
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group
                        .terminal_view_at_pane_index(0, ctx)
                        .unwrap()
                        .update(ctx, |terminal_view, ctx| {
                            terminal_view.attempt_to_share_session(
                                SharedSessionScrollbackType::None,
                                None,
                                SharedSessionSource::user(None),
                                false,
                                ctx,
                            );
                        });
                });

            workspace
                .active_tab_pane_group()
                .read(ctx, |pane_group, ctx| {
                    pane_group.terminal_view_at_pane_index(0, ctx).unwrap().id()
                })
        });

        // Ensure we have three shared sessions registered.
        app.read(|ctx| {
            let manager = Manager::as_ref(ctx);
            let shared_sessions = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_sessions.len(), 3);
        });

        // Stop sharing all sessions in first tab.
        workspace.update(&mut app, |workspace, ctx| {
            let tab = workspace.tabs[0].pane_group.downgrade();
            workspace.stop_sharing_all_panes_in_tab(&tab, ctx);
        });

        // Ensure that the only remaining shared session is the one in the other tab.
        app.read(|ctx| {
            let manager = Manager::as_ref(ctx);
            let shared_sessions = manager.shared_views(ctx).collect_vec();
            assert_eq!(shared_sessions.len(), 1);
            assert_eq!(shared_sessions[0].id(), second_tab_session);
        });
    });
}

#[test]
fn test_tab_context_menu_share_session_items() {
    let _guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        let shared_pane_id = setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            // Focus the shared session
            workspace.activate_tab(1, ctx);
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group.focus_pane_by_id(shared_pane_id, ctx);
                });
        });

        // When there's a single shared session in a tab (focused), the options
        // for sharing are "Stop sharing" and "Stop sharing all".
        workspace.read(&app, |workspace, ctx| {
            let items =
                workspace.tabs[1].menu_items(1, 3, &workspace.tab_groups, false, true, true, ctx);
            assert!(items[0]
                .is_approximately_same_item_as(&MenuItemFields::new("Stop sharing").into_item()));
            assert!(items[1].is_approximately_same_item_as(
                &MenuItemFields::new("Stop sharing all").into_item()
            ));
        });

        // Focus the other, non-shared pane in the tab
        workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(1, ctx);
            workspace
                .active_tab_pane_group()
                .update(ctx, |pane_group, ctx| {
                    pane_group.pane_by_index(1).unwrap().focus(ctx);
                });
        });

        // When there's a single shared session in a tab (unfocused), the options
        // for sharing are "Share session" and "Stop sharing all".
        workspace.read(&app, |workspace, ctx| {
            let items =
                workspace.tabs[1].menu_items(1, 3, &workspace.tab_groups, false, true, true, ctx);
            assert!(items[0]
                .is_approximately_same_item_as(&MenuItemFields::new("Share session").into_item()));
            assert!(items[1].is_approximately_same_item_as(
                &MenuItemFields::new("Stop sharing all").into_item()
            ));
        });

        // Stop sharing.
        workspace.update(&mut app, |workspace, ctx| {
            let tab = workspace.tabs[1].pane_group.downgrade();
            workspace.stop_sharing_all_panes_in_tab(&tab, ctx);
        });

        // When there's no shared sessions in a tab, the only option is "Share session".
        workspace.read(&app, |workspace, ctx| {
            let items =
                workspace.tabs[1].menu_items(1, 3, &workspace.tab_groups, false, true, true, ctx);
            assert!(items[0]
                .is_approximately_same_item_as(&MenuItemFields::new("Share session").into_item()));
            assert!(items[1].is_approximately_same_item_as(&MenuItem::Separator));
        });
    });
}

#[test]
fn test_view_only_session() {
    let _guard = FeatureFlag::ViewingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Trying to open command search
        let workspace = mock_workspace_viewing_shared_session(&mut app);
        workspace.update(&mut app, |workspace: &mut Workspace, ctx| {
            workspace.handle_action(&WorkspaceAction::ShowCommandSearch(Default::default()), ctx);
        });

        // Ensure command search doesn't work for read-only shared sessions
        workspace.read(&app, |workspace, _ctx| {
            assert!(!workspace.current_workspace_state.is_command_search_open);
        });
    });
}

#[test]
// This tests the end-to-end behavior to correctly switch focus among panels.
// (The only panels that can be focused currently are WD, workspace, & the agent panel.)
fn test_switch_focus_panels() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |view, ctx| {
            view.focus_active_tab(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.active_tab_pane_group().is_self_or_child_focused(ctx),
                "Expected terminal to be focused"
            );
        });

        // Shift focus from terminal to left panel when WD is open
        workspace.update(&mut app, |view, ctx| {
            view.current_workspace_state.is_warp_drive_open = true;
            view.focus_left_panel(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.left_panel_view.is_self_or_child_focused(ctx),
                "Expected Warp Drive panel to be focused"
            );
        });

        // Shift focus from WD to left panel when AI panel is open
        workspace.update(&mut app, |view, ctx| {
            view.current_workspace_state.is_ai_assistant_panel_open = true;
            view.focus_left_panel(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.ai_assistant_panel.is_self_or_child_focused(ctx),
                "Expected AI panel to be focused"
            );
        });

        // Shift focus from AI panel to left panel (terminal)
        workspace.update(&mut app, |view, ctx| {
            view.focus_left_panel(ctx);
        });
        workspace.update(&mut app, |_view, ctx| {
            assert!(
                workspace.is_self_or_child_focused(ctx),
                "Expected terminal to be focused"
            );
        });

        // Shift focus from workspace to right panel when the agent panel is open
        workspace.update(&mut app, |view, ctx| {
            view.current_workspace_state.is_ai_assistant_panel_open = true;
            view.focus_right_panel(ctx);
        });
        workspace.update(&mut app, |view, ctx| {
            assert!(
                view.ai_assistant_panel.is_self_or_child_focused(ctx),
                "Expected AI panel to be focused"
            );
        });

        // Shift focus from WD to right panel (terminal)
        workspace.update(&mut app, |view, ctx| {
            view.focus_right_panel(ctx);
        });
        workspace.update(&mut app, |_view, ctx| {
            assert!(
                workspace.is_self_or_child_focused(ctx),
                "Expected terminal to be focused"
            );
        });
    });
}

#[test]
fn test_focus_notebook() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let pane_group = workspace.read(&app, |workspace, _ctx| {
            workspace
                .get_pane_group_view(0)
                .expect("should have pane group for tab 0")
                .clone()
        });

        let first_terminal_id = pane_group.read(&app, |panes, _ctx| {
            get_newly_created_pane_id(panes, &[])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        let notebook_id = pane_group.update(&mut app, |panes, ctx| {
            // Add a notebook to the left.
            let notebook_view = ctx.add_typed_action_view(NotebookView::new);
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(notebook_view, ctx),
                true, /* focus_new_pane */
                ctx,
            );
            get_newly_created_pane_id(panes, &[first_terminal_id.into()])
        });

        // The new pane should be focused, but the terminal is still the active session.
        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
        });

        // Add a terminal below.
        let second_terminal_id = pane_group.update(&mut app, |panes, ctx| {
            panes.add_terminal_pane(Direction::Down, None, ctx);
            get_newly_created_pane_id(panes, &[first_terminal_id.into(), notebook_id])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        // The new terminal should be both focused and the active session.
        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), second_terminal_id.into());
            assert_eq!(panes.active_session_id(ctx), Some(second_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Inactive
            );
            assert_eq!(
                split_pane_state(panes, second_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                active_session_state(panes, second_terminal_id, ctx),
                ActiveSessionState::Active
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
        });

        // Close the new terminal.
        pane_group.update(&mut app, |panes, ctx| {
            panes.close_pane(second_terminal_id.into(), ctx);
        });

        // Focus should switch to the notebook, and the first terminal session
        // will activate.
        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
        });
    })
}

#[test]
fn test_close_active_session() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let pane_group = workspace.read(&app, |workspace, _ctx| {
            workspace
                .get_pane_group_view(0)
                .expect("should have pane group for tab 0")
                .clone()
        });

        let first_terminal_id = pane_group.read(&app, |panes, _ctx| {
            get_newly_created_pane_id(panes, &[])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        // Add a terminal above.
        let second_terminal_id = pane_group.update(&mut app, |panes, ctx| {
            panes.add_terminal_pane(Direction::Up, None, ctx);
            get_newly_created_pane_id(panes, &[first_terminal_id.into()])
                .as_terminal_pane_id()
                .expect("should be a terminal pane")
        });

        let notebook_id = pane_group.update(&mut app, |panes, ctx| {
            // Add a notebook to the left.
            let notebook_view = ctx.add_typed_action_view(NotebookView::new);
            panes.add_pane_with_direction(
                Direction::Left,
                NotebookPane::new(notebook_view, ctx),
                true, /* focus_new_pane */
                ctx,
            );
            get_newly_created_pane_id(
                panes,
                &[first_terminal_id.into(), second_terminal_id.into()],
            )
        });

        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(second_terminal_id));
        });

        pane_group.update(&mut app, |panes, ctx| {
            // Close the active session, which should leave the notebook focused and activate the
            // remaining session.
            panes.close_pane(second_terminal_id.into(), ctx);
        });

        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), notebook_id);
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
        });

        pane_group.update(&mut app, |panes, ctx| {
            // Now, focus the remaining session, which should keep it activated.
            panes.focus_pane_by_id(first_terminal_id.into(), ctx);
        });

        pane_group.read(&app, |panes, ctx| {
            assert_eq!(panes.focused_pane_id(ctx), first_terminal_id.into());
            assert_eq!(panes.active_session_id(ctx), Some(first_terminal_id));
            assert_eq!(
                split_pane_state(panes, first_terminal_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Focused)
            );
            assert_eq!(
                split_pane_state(panes, notebook_id, ctx),
                SplitPaneState::InSplitPane(PaneState::Unfocused)
            );
            assert_eq!(
                active_session_state(panes, first_terminal_id, ctx),
                ActiveSessionState::Active
            );
        });
    });
}

fn set_left_panel_visibility_across_tabs(is_enabled: bool, ctx: &mut ViewContext<Workspace>) {
    WindowSettings::handle(ctx).update(ctx, |window_settings, ctx| {
        window_settings
            .left_panel_visibility_across_tabs
            .set_value(is_enabled, ctx)
            .expect("Failed to update left_panel_visibility_across_tabs setting");
    });
}

fn add_get_started_tab(workspace: &mut Workspace, ctx: &mut ViewContext<Workspace>) {
    workspace.add_tab_with_pane_layout(
        PanesLayout::Snapshot(Box::new(PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: true,
            custom_vertical_tabs_title: None,
            contents: LeafContents::GetStarted,
        }))),
        Arc::new(HashMap::<PaneUuid, Vec<SerializedBlockListItem>>::new()),
        None,
        ctx,
    );
}

fn find_terminal_tab_index(workspace: &Workspace, ctx: &AppContext) -> usize {
    workspace
        .tabs
        .iter()
        .position(|tab| tab.pane_group.as_ref(ctx).has_terminal_panes())
        .expect("Expected a terminal tab")
}

fn find_non_following_tab_index(workspace: &Workspace, ctx: &AppContext) -> usize {
    workspace
        .tabs
        .iter()
        .position(|tab| {
            !Workspace::should_enable_file_tree_and_global_search_for_pane_group(
                tab.pane_group.as_ref(ctx),
            )
        })
        .expect("Expected a non-following tab")
}

#[test]
fn test_left_panel_window_scoped_reconciles_between_terminal_tabs_when_enabled() {
    let _conversation_list_guard =
        FeatureFlag::AgentViewConversationListView.override_enabled(false);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            set_left_panel_visibility_across_tabs(true, ctx);

            workspace.add_terminal_tab(false, ctx);

            workspace.activate_tab(0, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(!workspace.left_panel_open);

            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);

            workspace.activate_tab(1, ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            workspace.close_left_panel(ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(!workspace.left_panel_open);

            workspace.activate_tab(0, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
        });
    });
}

#[test]
fn test_left_panel_window_scoped_non_following_tab_does_not_reconcile_but_updates_window_state() {
    let _conversation_list_guard =
        FeatureFlag::AgentViewConversationListView.override_enabled(false);
    let _get_started_guard = FeatureFlag::GetStartedTab.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            set_left_panel_visibility_across_tabs(true, ctx);

            // Establish window-scoped desired state = open on a terminal tab.
            workspace.open_left_panel(ctx);
            assert!(workspace.left_panel_open);

            // Create a non-following tab (e.g. Get Started), which should not auto-open even though
            // the window state is open.
            add_get_started_tab(workspace, ctx);
            let non_following_tab_index = find_non_following_tab_index(workspace, ctx);
            workspace.activate_tab(non_following_tab_index, ctx);

            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);

            // User actions in the non-following tab still update window state.
            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);

            workspace.close_left_panel(ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(!workspace.left_panel_open);

            // The window state should reconcile back onto following tabs.
            let terminal_tab_index = find_terminal_tab_index(workspace, ctx);
            workspace.activate_tab(terminal_tab_index, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            // But toggling the window state from a following tab should not auto-open the
            // non-following tab.
            workspace.open_left_panel(ctx);
            assert!(workspace.left_panel_open);

            workspace.activate_tab(non_following_tab_index, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
            assert!(workspace.left_panel_open);
        });
    });
}

#[test]
fn test_left_panel_window_scoped_disabled_keeps_per_tab_state() {
    let _conversation_list_guard =
        FeatureFlag::AgentViewConversationListView.override_enabled(false);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            set_left_panel_visibility_across_tabs(false, ctx);

            workspace.add_terminal_tab(false, ctx);

            // Open left panel on tab 0.
            workspace.activate_tab(0, ctx);
            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            // With window scoping disabled, switching tabs should not reconcile the open state.
            workspace.activate_tab(1, ctx);
            assert!(
                !workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            // Each tab can be toggled independently.
            workspace.open_left_panel(ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );

            workspace.activate_tab(0, ctx);
            assert!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .left_panel_open
            );
        });
    });
}

#[test]
fn test_vertical_tabs_panel_visibility_restores_from_window_snapshot() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        let closed_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = false;
            workspace.snapshot(ctx.window_id(), false, ctx)
        });
        let open_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = true;
            workspace.snapshot(ctx.window_id(), false, ctx)
        });

        let restored_closed = restored_workspace(&mut app, closed_snapshot);
        let restored_open = restored_workspace(&mut app, open_snapshot);

        restored_closed.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
        restored_open.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_open_vertical_tabs_panel_is_idempotent() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = false;
            workspace.handle_action(&WorkspaceAction::OpenVerticalTabsPanel, ctx);
            assert!(workspace.vertical_tabs_panel_open);

            workspace.handle_action(&WorkspaceAction::OpenVerticalTabsPanel, ctx);
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_restored_open_when_show_in_restored_windows_enabled() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
                report_if_error!(settings
                    .show_vertical_tab_panel_in_restored_windows
                    .set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        let closed_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = false;
            workspace.snapshot(ctx.window_id(), false, ctx)
        });

        let restored = restored_workspace(&mut app, closed_snapshot);
        restored.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_closed_when_disabled_even_if_persisted_open() {
    // Regression for #9505: when `vertical_tabs_panel_open=true` is persisted
    // and the user then disables vertical tabs, restoring the workspace must
    // not honor the stale snapshot — otherwise a dismiss underlay paints over
    // the window and silently swallows every click.
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Snapshot the workspace with the panel open while vertical tabs are enabled.
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });
        let workspace = mock_workspace(&mut app);
        let open_snapshot = workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = true;
            workspace.snapshot(ctx.window_id(), false, ctx)
        });

        // Disable vertical tabs, then restore. The panel must stay closed.
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
        });
        let restored = restored_workspace(&mut app, open_snapshot);
        restored.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_defaults_open_for_new_window_when_vertical_tabs_enabled() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);

        workspace.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_inherits_transferred_tab_source_window_state() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });

        let transferred_closed = transferred_tab_workspace(&mut app, false);
        let transferred_open = transferred_tab_workspace(&mut app, true);

        transferred_closed.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
        transferred_open.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_vertical_tabs_panel_auto_shows_when_setting_enabled() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });

        // Enabling vertical tabs should auto-open the panel.
        workspace.update(&mut app, |_, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });
        workspace.read(&app, |workspace, _| {
            assert!(workspace.vertical_tabs_panel_open);
        });

        // Disabling vertical tabs should auto-close the panel.
        workspace.update(&mut app, |_, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
        });
        workspace.read(&app, |workspace, _| {
            assert!(!workspace.vertical_tabs_panel_open);
        });
    });
}

#[test]
fn test_active_tab_bar_position_id_tracks_layout() {
    // Cross-window drag hit-testing (`tab_bar_rects_for_window`) targets only
    // the active tab presentation. Regression guard for the bug where the
    // inactive horizontal bar registered as a drop zone while vertical tabs
    // were enabled, lighting up a spurious placeholder over the top bar.
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        // Horizontal tabs (setting off): the horizontal bar is the drop zone.
        app.read(|ctx| {
            assert_eq!(active_tab_bar_position_id(ctx), TAB_BAR_POSITION_ID);
        });

        // Vertical tabs (setting on): only the vertical panel is the drop zone,
        // so the horizontal bar no longer registers as a cross-window target.
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
        });
        app.read(|ctx| {
            assert_eq!(
                active_tab_bar_position_id(ctx),
                VERTICAL_TABS_PANEL_POSITION_ID
            );
        });
    });
}

#[test]
fn test_toggle_tab_configs_menu_opens_vertical_tabs_panel_and_menu() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
            workspace.vertical_tabs_panel_open = true;
        });
        workspace.update(&mut app, |workspace, ctx| {
            workspace.vertical_tabs_panel_open = false;
            workspace.show_new_session_dropdown_menu = None;

            workspace.handle_action(&WorkspaceAction::ToggleTabConfigsMenu, ctx);

            assert!(workspace.vertical_tabs_panel_open);
            assert!(workspace.show_new_session_dropdown_menu.is_some());
        });
    });
}

#[test]
fn test_toggle_tab_configs_menu_keyboard_shortcut_selects_top_item() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.show_new_session_dropdown_menu = None;

            workspace.handle_action(&WorkspaceAction::ToggleTabConfigsMenu, ctx);

            assert!(workspace.show_new_session_dropdown_menu.is_some());
            assert_eq!(
                workspace
                    .new_session_dropdown_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(0)
            );
        });
    });
}

#[test]
fn test_pointer_opened_tab_configs_menu_does_not_select_top_item() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.toggle_new_session_dropdown_menu(
                crate::workspace::action::NewSessionMenuAnchor::Pointer(Vector2F::zero()),
                ctx,
            );

            assert!(workspace.show_new_session_dropdown_menu.is_some());
            assert_eq!(
                workspace
                    .new_session_dropdown_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                None
            );
        });
    });
}

#[test]
fn test_open_tab_config_with_params_does_not_use_worktree_branch_as_implicit_title() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let tab_config = crate::tab_configs::TabConfig {
            name: "Untitled worktree".to_string(),
            title: None,
            color: None,
            panes: vec![TabConfigPaneNode {
                id: "main".to_string(),
                pane_type: Some(TabConfigPaneType::Terminal),
                split: None,
                children: None,
                is_focused: Some(true),
                directory: None,
                commands: Some(vec!["echo {{autogenerated_branch_name}}".to_string()]),
                shell: None,
            }],
            params: HashMap::new(),
            source_path: None,
        };

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_tab_config_with_params(
                tab_config.clone(),
                HashMap::new(),
                Some("mesa-coyote"),
                ctx,
            );
        });

        workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx),
                None
            );
        });
    });
}

#[test]
fn test_open_tab_config_with_params_uses_explicit_title_template() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let tab_config = crate::tab_configs::TabConfig {
            name: "Titled worktree".to_string(),
            title: Some("{{autogenerated_branch_name}}".to_string()),
            color: None,
            panes: vec![TabConfigPaneNode {
                id: "main".to_string(),
                pane_type: Some(TabConfigPaneType::Terminal),
                split: None,
                children: None,
                is_focused: Some(true),
                directory: None,
                commands: Some(vec!["echo {{autogenerated_branch_name}}".to_string()]),
                shell: None,
            }],
            params: HashMap::new(),
            source_path: None,
        };

        workspace.update(&mut app, |workspace, ctx| {
            workspace.open_tab_config_with_params(
                tab_config.clone(),
                HashMap::new(),
                Some("mesa-coyote"),
                ctx,
            );
        });

        workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(
                workspace
                    .active_tab_pane_group()
                    .as_ref(ctx)
                    .custom_title(ctx),
                Some("mesa-coyote".to_string())
            );
        });
    });
}
#[test]
fn test_toggle_tab_configs_menu_does_not_change_vertical_tabs_panel_in_horizontal_mode() {
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings.use_vertical_tabs.set_value(false, ctx));
            });
            workspace.vertical_tabs_panel_open = true;
            workspace.show_new_session_dropdown_menu = None;

            workspace.handle_action(&WorkspaceAction::ToggleTabConfigsMenu, ctx);

            assert!(workspace.vertical_tabs_panel_open);
            assert!(workspace.show_new_session_dropdown_menu.is_some());
        });
    });
}

#[test]
fn test_unified_new_session_menu_uses_new_worktree_config_label_and_order() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let labels = workspace
                .unified_new_session_menu_items(ctx)
                .iter()
                .map(new_session_menu_label)
                .collect::<Vec<_>>();

            assert!(!labels.iter().any(|label| label == "Worktree in"));

            let separator_index = labels
                .iter()
                .position(|label| label == "---")
                .expect("expected a separator in the new-session menu");

            assert_eq!(
                labels.get(separator_index + 1),
                Some(&"New worktree config".to_string())
            );
            assert_eq!(
                labels.get(separator_index + 2),
                Some(&"New tab config".to_string())
            );
        });
    });
}

#[test]
fn test_unified_new_session_menu_includes_reopen_closed_session() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let menu_items = workspace.unified_new_session_menu_items(ctx);
            assert!(matches!(
                menu_items.get(menu_items.len() - 2),
                Some(MenuItem::Separator)
            ));

            let reopen_item = reopen_closed_session_menu_item(&menu_items);
            assert!(reopen_item.is_disabled());
            assert!(matches!(
                reopen_item.on_select_action(),
                Some(action) if matches!(action, WorkspaceAction::ReopenClosedSession)
            ));

            workspace.add_terminal_tab(false, ctx);
            workspace.remove_tab(workspace.active_tab_index(), true, true, ctx);

            let menu_items = workspace.unified_new_session_menu_items(ctx);
            let reopen_item = reopen_closed_session_menu_item(&menu_items);
            assert!(!reopen_item.is_disabled());
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_search_editor_proxies_navigation_and_escape() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let alpha_repo = temp_root.path().join("alpha-repo");
        let beta_repo = temp_root.path().join("beta-repo");
        std::fs::create_dir_all(&alpha_repo).expect("failed to create alpha repo dir");
        std::fs::create_dir_all(&beta_repo).expect("failed to create beta repo dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(alpha_repo.clone(), ctx);
                persisted.user_added_workspace(beta_repo.clone(), ctx);
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_new_session_sidecar);
            assert!(workspace.worktree_sidecar_search_editor.is_focused(ctx));
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(1)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Navigate(NavigationKey::Down));
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(2)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Navigate(NavigationKey::Up));
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(1)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |editor, ctx| {
                    editor.set_buffer_text("beta", ctx);
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert_eq!(workspace.worktree_sidecar_search_query, "beta");
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.items_len()),
                2
            );
            assert_eq!(
                workspace
                    .new_session_sidecar_menu
                    .read(ctx, |menu, _| menu.selected_index()),
                Some(1)
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace
                .worktree_sidecar_search_editor
                .update(ctx, |_, ctx| {
                    ctx.emit(Event::Escape);
                });
        });
        workspace.read(&app, |workspace, ctx| {
            assert!(workspace.show_new_session_dropdown_menu.is_none());
            assert!(!workspace.show_new_session_sidecar);
            assert!(workspace.worktree_sidecar_search_query.is_empty());
            assert!(workspace
                .worktree_sidecar_search_editor
                .as_ref(ctx)
                .buffer_text(ctx)
                .is_empty());
        });
    });
}

#[cfg(feature = "local_fs")]
#[test]
fn test_worktree_sidecar_hides_linked_worktrees_from_repo_list() {
    let _tab_configs_guard = FeatureFlag::TabConfigs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        let temp_root = TempDir::new().expect("failed to create temp dir");
        let main_repo = temp_root.path().join("main-repo");
        let linked_worktree = temp_root.path().join("linked-worktree");
        let external_git_dir = main_repo
            .join(".git")
            .join("worktrees")
            .join("linked-worktree");

        std::fs::create_dir_all(&main_repo).expect("failed to create main repo dir");
        std::fs::create_dir_all(&linked_worktree).expect("failed to create linked worktree dir");
        std::fs::create_dir_all(&external_git_dir).expect("failed to create external git dir");

        workspace.update(&mut app, |_, ctx| {
            PersistedWorkspace::handle(ctx).update(ctx, |persisted, ctx| {
                persisted.user_added_workspace(main_repo.clone(), ctx);
                persisted.user_added_workspace(linked_worktree.clone(), ctx);
            });

            let main_repo_canon =
                CanonicalizedPath::try_from(main_repo.as_path()).expect("canonical main repo");
            let linked_worktree_canon = CanonicalizedPath::try_from(linked_worktree.as_path())
                .expect("canonical linked worktree");
            let external_git_dir_canon = CanonicalizedPath::try_from(external_git_dir.as_path())
                .expect("canonical external git dir");

            let main_repo_std: galaxy_util::standardized_path::StandardizedPath =
                main_repo_canon.into();
            let linked_worktree_std: galaxy_util::standardized_path::StandardizedPath =
                linked_worktree_canon.into();
            let external_git_dir_std: galaxy_util::standardized_path::StandardizedPath =
                external_git_dir_canon.into();

            DetectedRepositories::handle(ctx).update(ctx, |repos, _ctx| {
                repos.insert_test_repo_root(main_repo_std.clone());
                repos.insert_test_repo_root(linked_worktree_std.clone());
            });

            DirectoryWatcher::handle(ctx).update(ctx, |watcher, ctx| {
                watcher
                    .add_directory_with_git_dir(main_repo_std, None, ctx)
                    .expect("register main repo");
                watcher
                    .add_directory_with_git_dir(
                        linked_worktree_std,
                        Some(external_git_dir_std),
                        ctx,
                    )
                    .expect("register linked worktree");
            });
        });

        open_worktree_sidecar(&workspace, &mut app);

        workspace.read(&app, |workspace, ctx| {
            let labels = workspace.new_session_sidecar_menu.read(ctx, |menu, _| {
                menu.items()
                    .iter()
                    .filter_map(|item| match item {
                        MenuItem::Item(fields) => Some(fields.label().to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });

            let main_repo_label = main_repo.to_string_lossy().to_string();
            let linked_worktree_label = linked_worktree.to_string_lossy().to_string();

            assert!(labels.iter().any(|label| label == "Search repos"));
            assert!(labels.iter().any(|label| label == &main_repo_label));
            assert!(!labels.iter().any(|label| label == &linked_worktree_label));
        });
    });
}

#[test]
fn test_vertical_tabs_context_menu_does_not_show_hover_only_tab_bar() {
    let _full_screen_zen_mode_guard = FeatureFlag::FullScreenZenMode.override_enabled(true);
    let _vertical_tabs_guard = FeatureFlag::VerticalTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings
                    .workspace_decoration_visibility
                    .set_value(WorkspaceDecorationVisibility::OnHover, ctx));
                report_if_error!(settings.use_vertical_tabs.set_value(true, ctx));
            });
            workspace.should_show_ai_assistant_warm_welcome = false;
            workspace.vertical_tabs_panel_open = true;

            workspace.show_tab_right_click_menu =
                Some((0, TabContextMenuAnchor::Pointer(Vector2F::zero())));

            assert_eq!(workspace.tab_bar_mode(ctx), ShowTabBar::Hidden);
        });
    });
}

#[test]
fn test_standard_tab_context_menu_shows_hover_only_tab_bar() {
    let _full_screen_zen_mode_guard = FeatureFlag::FullScreenZenMode.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings
                    .workspace_decoration_visibility
                    .set_value(WorkspaceDecorationVisibility::OnHover, ctx));
            });
            workspace.should_show_ai_assistant_warm_welcome = false;

            workspace.show_tab_right_click_menu =
                Some((0, TabContextMenuAnchor::Pointer(Vector2F::zero())));

            assert_eq!(workspace.tab_bar_mode(ctx), ShowTabBar::Stacked);
        });
    });
}

#[test]
fn test_legacy_cloud_setup_guide_action_opens_local_history_without_onboarding() {
    let _agent_management_guard = FeatureFlag::AgentManagementView.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            assert!(
                !workspace
                    .current_workspace_state
                    .is_agent_management_view_open
            );

            workspace.handle_action(&WorkspaceAction::OpenCloudAgentSetupGuide, ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_agent_management_view_open
            );
            assert!(!workspace
                .agent_management_view
                .as_ref(ctx)
                .is_showing_setup_guide());

            workspace.handle_action(&WorkspaceAction::OpenCloudAgentSetupGuide, ctx);
            assert!(
                workspace
                    .current_workspace_state
                    .is_agent_management_view_open
            );
            assert!(!workspace
                .agent_management_view
                .as_ref(ctx)
                .is_showing_setup_guide());
        });
    });
}

#[test]
fn test_tab_mru_order() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);

            let id_a = workspace.tabs[0].pane_group.id();
            let id_b = workspace.tabs[1].pane_group.id();
            let id_c = workspace.tabs[2].pane_group.id();

            workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);
            workspace.handle_action(&WorkspaceAction::ActivateTab(1), ctx);
            workspace.handle_action(&WorkspaceAction::ActivateTab(2), ctx);
            workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);

            assert_eq!(workspace.tab_mru_order(), &[id_a, id_c, id_b]);
        });
    });
}

#[test]
fn test_create_new_tab_group_groups_active_tab() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Workspace starts with one tab from `Empty` source. Create a tab
            // group and verify the active tab is assigned to it.
            assert_eq!(workspace.tab_count(), 1);
            assert!(workspace.tabs[0].group_id.is_none());
            assert!(workspace.tab_groups.is_empty());

            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );

            assert_eq!(workspace.tab_groups.len(), 1);
            let group_id = workspace.tabs[0]
                .group_id
                .expect("active tab should be assigned to the new group");
            assert!(workspace.tab_groups.contains_key(&group_id));
            // New groups start expanded so members are visible.
            assert!(!workspace.tab_groups[&group_id].collapsed);
        });
    });
}

#[test]
fn test_new_tab_group_from_tab_keeps_tab_in_place() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a three-tab workspace (starts with one tab).
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 3);

            // Target the last tab so a move-to-top would be observable.
            let target_index = workspace.tab_count() - 1;
            let target_id = workspace.tabs[target_index].pane_group.id();

            workspace.handle_action(&WorkspaceAction::NewTabGroupFromTab(target_index), ctx);

            // The tab stays at its original index instead of jumping to the top.
            assert_eq!(workspace.tabs[target_index].pane_group.id(), target_id);
            assert_eq!(workspace.active_tab_index(), target_index);

            let group_id = workspace.tabs[target_index]
                .group_id
                .expect("target tab should be assigned to the new group");
            assert!(workspace.tab_groups.contains_key(&group_id));

            // The other tabs remain ungrouped.
            assert!(workspace.tabs[0].group_id.is_none());
            assert!(workspace.tabs[1].group_id.is_none());
        });
    });
}

#[test]
fn test_new_tab_group_from_selected_tabs_anchors_at_earliest_tab() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a four-tab workspace (starts with one tab).
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 4);

            let id0 = workspace.tabs[0].pane_group.id();
            let id1 = workspace.tabs[1].pane_group.id();
            let id2 = workspace.tabs[2].pane_group.id();
            let id3 = workspace.tabs[3].pane_group.id();

            // Select two non-adjacent tabs (indices 1 and 3) with the active
            // tab among the selection so no extra tab is pulled in.
            workspace.activate_tab(1, ctx);
            workspace.tabs[1].in_multi_selection = true;
            workspace.tabs[3].in_multi_selection = true;

            workspace.handle_action(&WorkspaceAction::NewTabGroupFromSelectedTabs, ctx);

            // The group block lands at the earliest selected tab's position
            // (index 1), preserving the relative order of its members.
            let order: Vec<_> = workspace
                .tabs
                .iter()
                .map(|tab| tab.pane_group.id())
                .collect();
            assert_eq!(order, vec![id0, id1, id3, id2]);

            // Both selected tabs share the new group; the others stay ungrouped.
            let group_id = workspace.tabs[1]
                .group_id
                .expect("earliest selected tab should join the new group");
            assert_eq!(workspace.tabs[2].group_id, Some(group_id));
            assert!(workspace.tabs[0].group_id.is_none());
            assert!(workspace.tabs[3].group_id.is_none());

            // Selection flags are cleared after grouping.
            assert!(workspace.tabs.iter().all(|tab| !tab.in_multi_selection));
        });
    });
}

#[test]
fn test_new_tab_group_from_tab_in_group_anchors_after_group() {
    // Pulling a tab out of the middle of an existing group must not split
    // that group: the new single-tab group should land just past the old
    // group's last remaining member.
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a four-tab workspace (starts with one tab).
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 4);

            // Group the first three tabs (indices 0, 1, 2) into one group,
            // leaving the last tab ungrouped.
            let group = TabGroup::new();
            let existing_group_id = group.id;
            workspace.tab_groups.insert(existing_group_id, group);
            for index in 0..3 {
                workspace.tabs[index].group_id = Some(existing_group_id);
            }

            let id0 = workspace.tabs[0].pane_group.id();
            let middle_id = workspace.tabs[1].pane_group.id();
            let id2 = workspace.tabs[2].pane_group.id();
            let id3 = workspace.tabs[3].pane_group.id();

            // Create a new group from the middle member of the existing group.
            workspace.handle_action(&WorkspaceAction::NewTabGroupFromTab(1), ctx);

            // The pulled tab lands right after the old group's run, so the old
            // group's members stay contiguous: [g0, g2, new, ungrouped].
            let order: Vec<_> = workspace
                .tabs
                .iter()
                .map(|tab| tab.pane_group.id())
                .collect();
            assert_eq!(order, vec![id0, id2, middle_id, id3]);

            let new_group_id = workspace.tabs[2]
                .group_id
                .expect("pulled tab should be in the new group");
            assert_ne!(new_group_id, existing_group_id);
            assert_eq!(workspace.active_tab_index(), 2);

            // The old group keeps its two contiguous members.
            assert_eq!(workspace.tabs[0].group_id, Some(existing_group_id));
            assert_eq!(workspace.tabs[1].group_id, Some(existing_group_id));
            assert!(workspace.tabs[3].group_id.is_none());
        });
    });
}

#[test]
fn test_new_tab_group_from_selected_tabs_in_group_anchors_after_group() {
    // When the earliest selected tab sits inside an existing group, the new
    // group block is anchored past that group's last surviving member so the
    // existing group is never split.
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a five-tab workspace (starts with one tab).
            for _ in 0..4 {
                workspace.add_terminal_tab(false, ctx);
            }
            assert_eq!(workspace.tab_count(), 5);

            // Group the first three tabs (indices 0, 1, 2); leave 3 and 4 loose.
            let group = TabGroup::new();
            let existing_group_id = group.id;
            workspace.tab_groups.insert(existing_group_id, group);
            for index in 0..3 {
                workspace.tabs[index].group_id = Some(existing_group_id);
            }

            let id0 = workspace.tabs[0].pane_group.id();
            let id1 = workspace.tabs[1].pane_group.id();
            let id2 = workspace.tabs[2].pane_group.id();
            let id3 = workspace.tabs[3].pane_group.id();
            let id4 = workspace.tabs[4].pane_group.id();

            // Select the middle member of the group (index 1) and the trailing
            // ungrouped tab (index 4), with the active tab among the selection.
            workspace.activate_tab(1, ctx);
            workspace.tabs[1].in_multi_selection = true;
            workspace.tabs[4].in_multi_selection = true;

            workspace.handle_action(&WorkspaceAction::NewTabGroupFromSelectedTabs, ctx);

            // The block is placed just past the old group's surviving run
            // (g0, g2), keeping that group contiguous:
            // [g0, g2, new1, new4, id3].
            let order: Vec<_> = workspace
                .tabs
                .iter()
                .map(|tab| tab.pane_group.id())
                .collect();
            assert_eq!(order, vec![id0, id2, id1, id4, id3]);

            let new_group_id = workspace.tabs[2]
                .group_id
                .expect("selected tab should be in the new group");
            assert_ne!(new_group_id, existing_group_id);
            assert_eq!(workspace.tabs[3].group_id, Some(new_group_id));

            // Old group stays contiguous with its two surviving members.
            assert_eq!(workspace.tabs[0].group_id, Some(existing_group_id));
            assert_eq!(workspace.tabs[1].group_id, Some(existing_group_id));
            assert!(workspace.tabs[4].group_id.is_none());
        });
    });
}

#[test]
fn test_toggle_tab_group_collapsed_flips_state() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[0]
                .group_id
                .expect("active tab should be in a group");
            assert!(!workspace.tab_groups[&group_id].collapsed);

            workspace.handle_action(&WorkspaceAction::ToggleTabGroupCollapsed(group_id), ctx);
            assert!(workspace.tab_groups[&group_id].collapsed);

            workspace.handle_action(&WorkspaceAction::ToggleTabGroupCollapsed(group_id), ctx);
            assert!(!workspace.tab_groups[&group_id].collapsed);
        });
    });
}

#[test]
fn test_close_tab_group_removes_group_and_members() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Create a group, then add another tab which inherits the
            // active tab's group_id via `add_tab_with_pane_layout`.
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[workspace.active_tab_index()]
                .group_id
                .expect("active tab should be in a group");

            workspace.add_terminal_tab(false, ctx);

            let group_members: Vec<usize> = workspace
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| tab.group_id == Some(group_id))
                .map(|(idx, _)| idx)
                .collect();
            assert_eq!(
                group_members.len(),
                2,
                "new tab should inherit the active tab's group_id"
            );

            workspace.handle_action(&WorkspaceAction::CloseTabGroup(group_id), ctx);

            // All group members are closed and the group entry is removed.
            assert!(!workspace.tab_groups.contains_key(&group_id));
            assert!(workspace
                .tabs
                .iter()
                .all(|tab| tab.group_id != Some(group_id)));
        });
    });
}

#[test]
fn test_closing_inactive_left_split_pane_preserves_other_chat_and_group() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);
        let workspace = mock_workspace(&mut app);

        let (left, right, outside, left_terminal, group_id) =
            workspace.update(&mut app, |workspace, ctx| {
                workspace.add_terminal_tab(false, ctx);
                workspace.add_terminal_tab(false, ctx);
                let left = workspace.tabs[0].pane_group.clone();
                let right = workspace.tabs[1].pane_group.clone();
                let outside = workspace.tabs[2].pane_group.clone();
                let left_terminal = left.as_ref(ctx).focused_session_view(ctx).unwrap();
                let group = TabGroup::new();
                let group_id = group.id;
                workspace.tab_groups.insert(group_id, group);
                workspace.tabs[0].group_id = Some(group_id);
                workspace.tabs[1].group_id = Some(group_id);
                workspace.tabs[0].selected_color =
                    SelectedTabColor::Color(AnsiColorIdentifier::Magenta);
                workspace.tabs[1].selected_color = SelectedTabColor::Cleared;
                workspace.activate_tab(0, ctx);
                workspace.open_tab_in_split_view(right.id(), ctx);
                workspace.activate_tab(1, ctx);
                assert_eq!(workspace.active_tab_pane_group().id(), right.id());
                assert_eq!(
                    workspace.tabs[0].selected_color,
                    SelectedTabColor::Color(AnsiColorIdentifier::Magenta)
                );
                assert_eq!(workspace.tabs[1].selected_color, SelectedTabColor::Cleared);
                (left, right, outside, left_terminal, group_id)
            });

        // This is the backing-view close handler invoked by the pane-header X.
        left_terminal.update(&mut app, |terminal, ctx| {
            crate::pane_group::pane::BackingView::close(terminal, ctx);
        });

        workspace.update(&mut app, |workspace, ctx| {
            assert_eq!(workspace.tab_count(), 2);
            assert_eq!(workspace.active_tab_pane_group().id(), right.id());
            assert_eq!(workspace.tabs[0].selected_color, SelectedTabColor::Cleared);
            assert!(workspace
                .tabs
                .iter()
                .any(|tab| tab.pane_group.id() == outside.id()));
            assert!(workspace.tab_groups.contains_key(&group_id));
            assert_eq!(group_member_indices(&workspace.tabs, group_id).count(), 1);

            // A late close notification from the removed pane cannot close its
            // former neighbor after tab indices shift.
            workspace.handle_file_tree_event(
                left,
                &pane_group::Event::Exited {
                    add_to_undo_stack: true,
                },
                ctx,
            );
            assert_eq!(workspace.tab_count(), 2);

            workspace.handle_file_tree_event(
                right,
                &pane_group::Event::Exited {
                    add_to_undo_stack: true,
                },
                ctx,
            );
            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.active_tab_pane_group().id(), outside.id());
            assert!(!workspace.tab_groups.contains_key(&group_id));
        });
    });
}

#[test]
fn test_confirm_close_tab_group_keeps_unrelated_tabs_after_reorder() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);
    let _sharing_guard = FeatureFlag::CreatingSharedSessions.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(disable_quit_warning);
        let workspace = mock_workspace(&mut app);
        setup_session_sharing_test(&workspace, &mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let outside_id = workspace.tabs[2].pane_group.id();
            let group = TabGroup::new();
            let group_id = group.id;
            workspace.tab_groups.insert(group_id, group);
            workspace.tabs[0].group_id = Some(group_id);
            workspace.tabs[1].group_id = Some(group_id);
            workspace.close_tab_group(group_id, ctx);
            assert!(workspace.current_workspace_state.is_close_session_confirmation_dialog_open);
            assert!(matches!(
                workspace.close_session_confirmation_dialog.as_ref(ctx).get_open_confirmation_source(),
                Some(OpenDialogSource::CloseTabGroup { group_id: source_id }) if source_id == group_id
            ));

            workspace.move_tab_to_index(2, 0, ctx);
            workspace.handle_close_session_confirmation_dialog_event(
                &CloseSessionConfirmationEvent::CloseSession {
                    dont_show_again: false,
                    open_confirmation_source: OpenDialogSource::CloseTabGroup { group_id },
                },
                ctx,
            );

            assert_eq!(workspace.tab_count(), 1);
            assert_eq!(workspace.active_tab_pane_group().id(), outside_id);
            assert!(!workspace.tab_groups.contains_key(&group_id));
        });
    });
}

#[test]
fn test_new_tab_with_after_all_tabs_setting_lands_top_level_at_end() {
    // With `new_tab_placement = AfterAllTabs`, a new tab lands at the very end
    // of the tab bar, outside any group — even when the active tab is in a
    // group.
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings
                    .new_tab_placement
                    .set_value(NewTabPlacement::AfterAllTabs, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build [g0, g1, ungrouped] by assigning membership directly, so the
            // setup doesn't depend on new-tab placement behavior.
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 3);

            let group = TabGroup::new();
            let group_id = group.id;
            workspace.tab_groups.insert(group_id, group);
            workspace.tabs[0].group_id = Some(group_id);
            workspace.tabs[1].group_id = Some(group_id);

            // Activate a member of the group, then add a new tab.
            workspace.activate_tab(0, ctx);
            workspace.add_terminal_tab(false, ctx);

            // The new tab lands at the very end of the bar and is top-level.
            let last = workspace.tab_count() - 1;
            assert_eq!(workspace.active_tab_index(), last);
            assert_eq!(workspace.tabs[last].group_id, None);

            // The group keeps exactly its original two contiguous members.
            let group_members: Vec<usize> = workspace
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| t.group_id == Some(group_id))
                .map(|(idx, _)| idx)
                .collect();
            assert_eq!(group_members, vec![0, 1]);
        });
    });
}

#[test]
fn test_new_tab_with_after_current_tab_setting_lands_after_active_tab_in_group() {
    // With `new_tab_placement = AfterCurrentTab` and the active tab in the
    // middle of a group, a new tab should land immediately after the active
    // tab and inherit the group_id, preserving group contiguity rather than
    // jumping to the end of the group or past it.
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        app.update(|ctx| {
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                report_if_error!(settings
                    .new_tab_placement
                    .set_value(NewTabPlacement::AfterCurrentTab, ctx));
            });
        });

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Create a group and grow it to two contiguous members so we can
            // activate the first one (i.e. a member that isn't at the end of
            // the group's run).
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[workspace.active_tab_index()]
                .group_id
                .expect("active tab should be in a group");
            workspace.add_terminal_tab(false, ctx);

            // Activate the first grouped tab so the next insertion happens in
            // the middle of the group's contiguous run.
            let first_grouped_idx = workspace
                .tabs
                .iter()
                .position(|t| t.group_id == Some(group_id))
                .expect("expected at least one grouped tab");
            workspace.activate_tab(first_grouped_idx, ctx);

            let expected_new_idx = first_grouped_idx + 1;

            workspace.add_terminal_tab(false, ctx);

            // The new tab lands immediately after the previously-active
            // grouped tab, inherits its group_id, and keeps the group's run
            // contiguous.
            assert_eq!(workspace.active_tab_index(), expected_new_idx);
            assert_eq!(
                workspace.tabs[expected_new_idx].group_id,
                Some(group_id),
                "new tab should inherit the active tab's group_id"
            );

            let group_indices: Vec<usize> = workspace
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| t.group_id == Some(group_id))
                .map(|(idx, _)| idx)
                .collect();
            assert_eq!(
                group_indices.len(),
                3,
                "group should have grown to three members"
            );
            assert!(
                group_indices.windows(2).all(|w| w[1] == w[0] + 1),
                "group's tab indices should be contiguous, got {group_indices:?}"
            );
        });
    });
}

#[test]
fn test_move_tab_to_group_expands_collapsed_group() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Create a group and a second ungrouped tab.
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[workspace.active_tab_index()]
                .group_id
                .expect("active tab should be in a group");
            workspace.add_terminal_tab(false, ctx);

            // Find the ungrouped tab.
            let ungrouped_idx = workspace
                .tabs
                .iter()
                .position(|t| t.group_id.is_none())
                .expect("expected an ungrouped tab");

            // Collapse the group, then move the ungrouped tab into it.
            workspace.handle_action(&WorkspaceAction::ToggleTabGroupCollapsed(group_id), ctx);
            assert!(
                workspace.tab_groups[&group_id].collapsed,
                "group should be collapsed"
            );

            workspace.handle_action(
                &WorkspaceAction::MoveTabToGroup {
                    tab_index: ungrouped_idx,
                    group_id,
                },
                ctx,
            );

            assert!(
                !workspace.tab_groups[&group_id].collapsed,
                "group should expand when a tab is moved into it"
            );
        });
    });
}

#[test]
fn test_active_tab_add_split_view_uses_new_tab_action() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let assert_add_split_view_action = |items: Vec<MenuItem<WorkspaceAction>>| {
                let fields = items
                    .iter()
                    .find_map(|item| match item {
                        MenuItem::Item(fields) if fields.label() == "Add split view" => {
                            Some(fields)
                        }
                        _ => None,
                    })
                    .expect("active tab should offer Add split view");
                assert!(matches!(
                    fields.on_select_action(),
                    Some(WorkspaceAction::AddDefaultTabInSplitView)
                ));
            };
            // Unsplit state: active tab should offer the action.
            assert_add_split_view_action(
                workspace.build_split_view_menu_items_for_tab(workspace.active_tab_index()),
            );

            // Split state: active tab (also a split member) should still offer it.
            let first_id = workspace.active_tab_pane_group().id();
            workspace.add_terminal_tab(false, ctx);
            workspace.open_tab_in_split_view(first_id, ctx);
            assert_add_split_view_action(
                workspace.build_split_view_menu_items_for_tab(workspace.active_tab_index()),
            );
        });
    });
}

#[test]
fn test_add_split_view_from_unsplit_opens_new_tab_in_split() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            let source_id = workspace.active_tab_pane_group().id();
            assert!(
                !workspace.split_view_state.is_active(),
                "should start unsplit"
            );

            workspace.handle_action(&WorkspaceAction::AddDefaultTabInSplitView, ctx);

            assert_eq!(workspace.tab_count(), 2, "a new tab should be created");
            assert!(
                workspace.split_view_state.is_active(),
                "split should be active"
            );
            assert_eq!(
                workspace.split_view_state.len(),
                2,
                "split should have 2 members"
            );
            assert!(
                workspace.split_view_state.contains(source_id),
                "source tab is a member"
            );

            let new_tab_id = workspace.active_tab_pane_group().id();
            assert_ne!(source_id, new_tab_id, "a distinct new tab was created");
            assert!(
                workspace.split_view_state.contains(new_tab_id),
                "new tab is a member"
            );
            assert_eq!(
                workspace.split_view_state.focused_id(),
                Some(new_tab_id),
                "new tab is focused"
            );
        });
    });
}

#[test]
fn test_add_split_view_preserves_existing_split_members() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a 2-tab split [tab_a, tab_b].
            let tab_a_id = workspace.active_tab_pane_group().id();
            workspace.add_terminal_tab(false, ctx);
            let tab_b_id = workspace.active_tab_pane_group().id();
            workspace.open_tab_in_split_view(tab_a_id, ctx);
            assert!(
                workspace.split_view_state.is_active(),
                "prerequisite: split active"
            );

            // Make tab_a the active/source tab.
            let tab_a_index = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == tab_a_id)
                .unwrap();
            workspace.activate_tab(tab_a_index, ctx);

            // Add split view — should grow to 3 members.
            workspace.handle_action(&WorkspaceAction::AddDefaultTabInSplitView, ctx);

            assert_eq!(workspace.tab_count(), 3);
            assert!(workspace.split_view_state.is_active());
            assert_eq!(workspace.split_view_state.len(), 3, "both prior + new");
            assert!(
                workspace.split_view_state.contains(tab_a_id),
                "tab_a retained"
            );
            assert!(
                workspace.split_view_state.contains(tab_b_id),
                "tab_b retained"
            );

            let new_tab_id = workspace.active_tab_pane_group().id();
            assert!(
                workspace.split_view_state.contains(new_tab_id),
                "new tab added"
            );
            assert_eq!(workspace.split_view_state.focused_id(), Some(new_tab_id));
        });
    });
}

#[test]
fn test_add_split_view_not_shown_for_inactive_tab() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            let active_index = workspace.active_tab_index();
            let inactive_index = active_index - 1;

            let inactive_items = workspace.build_split_view_menu_items_for_tab(inactive_index);
            let has_add = inactive_items
                .iter()
                .any(|item| matches!(item, MenuItem::Item(f) if f.label() == "Add split view"));
            assert!(!has_add, "inactive tab must not offer Add split view");
        });
    });
}

#[test]
fn test_add_split_view_hidden_at_max_capacity() {
    // When the split already has MAX_VISIBLE members the "Add split view"
    // item must not appear because there is no room for another tab.
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a full 4-tab split.
            let first_id = workspace.active_tab_pane_group().id();
            for _ in 1..SplitViewState::MAX_VISIBLE {
                workspace.add_terminal_tab(false, ctx);
                let new_id = workspace.active_tab_pane_group().id();
                workspace.open_tab_in_split_view(new_id, ctx);
            }
            // Activate the first (source) tab so it is the active_tab_index.
            let first_index = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == first_id)
                .unwrap();
            workspace.set_active_tab_index(first_index, ctx);

            assert_eq!(
                workspace.split_view_state.len(),
                SplitViewState::MAX_VISIBLE,
                "split must be at capacity"
            );

            // Menu must not offer "Add split view".
            let items = workspace.build_split_view_menu_items_for_tab(workspace.active_tab_index());
            let has_add = items
                .iter()
                .any(|item| matches!(item, MenuItem::Item(f) if f.label() == "Add split view"));
            assert!(!has_add, "Add split view must be hidden at capacity");

            // Dispatching the action must be a no-op (no new tab, no split change).
            let tab_count_before = workspace.tab_count();
            let split_len_before = workspace.split_view_state.len();
            workspace.handle_action(&WorkspaceAction::AddDefaultTabInSplitView, ctx);
            assert_eq!(
                workspace.tab_count(),
                tab_count_before,
                "no new tab when at capacity"
            );
            assert_eq!(
                workspace.split_view_state.len(),
                split_len_before,
                "split unchanged when at capacity"
            );
        });
    });
}

#[test]
fn test_add_default_tab_cmd_t_unchanged() {
    // `AddDefaultTab` (Cmd-T) must remain a plain new-tab action with no
    // split-view side-effects.
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert!(!workspace.split_view_state.is_active());

            workspace.handle_action(&WorkspaceAction::AddDefaultTab, ctx);

            assert_eq!(workspace.tab_count(), 2, "a new tab should be created");
            assert!(
                !workspace.split_view_state.is_active(),
                "Cmd-T must not activate split view"
            );
        });
    });
}

#[test]
fn test_add_split_view_source_is_middle_tab() {
    // The source tab is not the first or last tab.  Set-difference ID detection
    // must still find the correct new tab regardless of insertion order.
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build: [tab_first, tab_source, tab_last]
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);

            // Activate the middle tab (index 1) as the source.
            workspace.activate_tab(1, ctx);
            let source_id = workspace.active_tab_pane_group().id();
            assert_eq!(workspace.tab_count(), 3);
            assert!(!workspace.split_view_state.is_active());

            workspace.handle_action(&WorkspaceAction::AddDefaultTabInSplitView, ctx);

            assert_eq!(workspace.tab_count(), 4, "one new tab created");
            assert!(workspace.split_view_state.is_active(), "split is active");
            assert_eq!(workspace.split_view_state.len(), 2);
            assert!(
                workspace.split_view_state.contains(source_id),
                "source (middle) tab is a split member"
            );
            // The new tab must be in the split and focused.
            let new_id = workspace.active_tab_pane_group().id();
            assert_ne!(source_id, new_id);
            assert!(
                workspace.split_view_state.contains(new_id),
                "new tab is a split member"
            );
            assert_eq!(workspace.split_view_state.focused_id(), Some(new_id));
        });
    });
}

#[test]
fn test_add_split_view_preserves_resized_weights() {
    // After the user drags a divider to adjust slot proportions, dispatching
    // "Add split view" from a split member must carry those proportions to the
    // restored split (up to the capped member count).
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Create a 2-tab split and simulate a manual weight adjustment.
            let tab_a_id = workspace.active_tab_pane_group().id();
            workspace.add_terminal_tab(false, ctx);
            let tab_b_id = workspace.active_tab_pane_group().id();
            workspace.open_tab_in_split_view(tab_a_id, ctx);
            assert!(workspace.split_view_state.is_active());

            // Simulate the user having resized: give tab_a weight 2.0, tab_b weight 1.0.
            workspace.split_view_state.set_slot_weight(tab_a_id, 2.0);
            workspace.split_view_state.set_slot_weight(tab_b_id, 1.0);

            // Activate tab_a as the split source.
            let tab_a_index = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == tab_a_id)
                .unwrap();
            workspace.activate_tab(tab_a_index, ctx);

            // Dispatch Add split view.
            workspace.handle_action(&WorkspaceAction::AddDefaultTabInSplitView, ctx);

            assert_eq!(workspace.split_view_state.len(), 3);
            // Weights for the two prior members must be preserved.
            assert_eq!(
                workspace.split_view_state.slot_weight(tab_a_id),
                2.0,
                "tab_a weight preserved"
            );
            assert_eq!(
                workspace.split_view_state.slot_weight(tab_b_id),
                1.0,
                "tab_b weight preserved"
            );
            // The new tab gets the default weight.
            let new_id = workspace.active_tab_pane_group().id();
            assert_eq!(
                workspace.split_view_state.slot_weight(new_id),
                1.0,
                "new tab gets default weight"
            );
        });
    });
}

#[test]
fn test_add_split_view_stale_async_source_no_op() {
    // Simulate the Docker async stale-source race: the source tab is closed
    // between the action dispatch and async completion.  The split must not
    // be entered (no singleton or stale state left behind).
    // We test this via `enter_split_view_with_new_tab` with a pending snapshot
    // whose source_id references a now-closed tab (empty prior_split_ids).
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Capture the original source tab, then close it.
            let source_id = workspace.active_tab_pane_group().id();
            // Snapshot *before* closing (simulates action-dispatch state).
            let pending = workspace.snapshot_split_view_source();

            // Add a new tab so we still have more than one, then close the source.
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            // Close the original source tab by index.
            let source_index = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == source_id)
                .unwrap();
            workspace.remove_tab(source_index, false, true, ctx);
            assert!(
                workspace.tab_index_for_pane_group_id(source_id).is_none(),
                "source tab should be gone"
            );

            // Simulate async completion: a new tab was created (last tab).
            let new_tab_id = workspace.tabs[workspace.tabs.len() - 1].pane_group.id();

            // enter_split_view_with_new_tab with stale source must be a no-op.
            workspace.enter_split_view_with_new_tab(new_tab_id, &pending, ctx);

            assert!(
                !workspace.split_view_state.is_active(),
                "split must not be entered when source is gone"
            );
        });
    });
}

#[test]
fn test_is_pending_split_source_current_staleness_cases() {
    // Exercises every staleness condition checked by `is_pending_split_source_current`:
    //   (a) active tab switched to a different live tab,
    //   (b) split composition changed while source is still active,
    //   (c) split reached MAX_VISIBLE capacity,
    //   (d) all conditions valid — must return true.
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Build a 2-tab split: source (index 0) and tab_b.
            let source_id = workspace.active_tab_pane_group().id();
            workspace.add_terminal_tab(false, ctx);
            let tab_b_id = workspace.active_tab_pane_group().id();
            workspace.open_tab_in_split_view(source_id, ctx);
            // Activate source so it is the active tab.
            let source_index = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == source_id)
                .unwrap();
            workspace.activate_tab(source_index, ctx);
            assert_eq!(
                workspace.active_tab_pane_group().id(),
                source_id,
                "source is active"
            );
            assert!(workspace.split_view_state.is_active());

            // Snapshot the valid state.
            let pending = workspace.snapshot_split_view_source();

            // (d) Valid state — must return true.
            assert!(
                workspace.is_pending_split_source_current(&pending),
                "(d) all conditions met: should be current"
            );

            // (a) Switch active tab away (source is still alive).
            let tab_b_index = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == tab_b_id)
                .unwrap();
            workspace.activate_tab(tab_b_index, ctx);
            assert!(
                !workspace.is_pending_split_source_current(&pending),
                "(a) active tab switched: should be stale"
            );
            // Restore active tab to source.
            workspace.activate_tab(source_index, ctx);

            // (b) Split composition changed: remove tab_b.
            workspace.remove_from_split_view(tab_b_id, ctx);
            assert!(
                !workspace.split_view_state.is_active(),
                "prerequisite: split exited"
            );
            assert!(
                !workspace.is_pending_split_source_current(&pending),
                "(b) split composition changed: should be stale"
            );
            // Rebuild the original split.
            workspace.open_tab_in_split_view(source_id, ctx);
            workspace.activate_tab(source_index, ctx);

            // (c) Fill the split to MAX_VISIBLE capacity.
            for _ in 2..SplitViewState::MAX_VISIBLE {
                workspace.add_terminal_tab(false, ctx);
                let extra_id = workspace.active_tab_pane_group().id();
                workspace.open_tab_in_split_view(extra_id, ctx);
            }
            workspace.activate_tab(source_index, ctx);
            assert_eq!(
                workspace.split_view_state.len(),
                SplitViewState::MAX_VISIBLE,
                "split is at capacity"
            );
            assert!(
                !workspace.is_pending_split_source_current(&pending),
                "(c) at capacity: should be stale"
            );
        });
    });
}

#[test]
fn test_move_selected_tabs_to_group_expands_collapsed_group() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Add two extra tabs while no group exists so they remain ungrouped.
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);

            // Create a group from the first tab (moves it to index 0) leaving
            // the other two tabs ungrouped.
            workspace.handle_action(&WorkspaceAction::NewTabGroupFromTab(0), ctx);
            let group_id = workspace.tabs[0]
                .group_id
                .expect("tab 0 should be in the new group");

            // Collapse the group.
            workspace.handle_action(&WorkspaceAction::ToggleTabGroupCollapsed(group_id), ctx);
            assert!(
                workspace.tab_groups[&group_id].collapsed,
                "group should be collapsed"
            );

            // Select the two ungrouped tabs and move them to the group.
            let ungrouped_indices: Vec<usize> = workspace
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| t.group_id.is_none())
                .map(|(i, _)| i)
                .collect();
            assert_eq!(ungrouped_indices.len(), 2);
            workspace.activate_tab(ungrouped_indices[0], ctx);
            workspace.tabs[ungrouped_indices[0]].in_multi_selection = true;
            workspace.tabs[ungrouped_indices[1]].in_multi_selection = true;

            workspace.handle_action(&WorkspaceAction::MoveSelectedTabsToGroup { group_id }, ctx);

            assert!(
                !workspace.tab_groups[&group_id].collapsed,
                "group should expand when selected tabs are moved into it"
            );
        });
    });
}

#[test]
fn test_new_tab_in_group_expands_collapsed_group_non_member_active() {
    // When the active tab is NOT a member of the group, `new_tab_in_group`
    // must still expand the target group.
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            // Create a group, then activate an ungrouped tab so the active
            // tab is NOT a member of the group.
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[workspace.active_tab_index()]
                .group_id
                .expect("active tab should be in a group");
            workspace.add_terminal_tab(false, ctx);

            let ungrouped_idx = workspace
                .tabs
                .iter()
                .position(|t| t.group_id.is_none())
                .expect("expected an ungrouped tab");
            workspace.activate_tab(ungrouped_idx, ctx);

            // Collapse the group, then open a new tab inside it.
            workspace.handle_action(&WorkspaceAction::ToggleTabGroupCollapsed(group_id), ctx);
            assert!(
                workspace.tab_groups[&group_id].collapsed,
                "group should be collapsed"
            );

            workspace.handle_action(&WorkspaceAction::NewTabInGroup(group_id), ctx);

            assert!(
                !workspace.tab_groups[&group_id].collapsed,
                "group should expand when a new tab is opened in it"
            );
        });
    });
}

#[test]
fn test_new_tab_in_group_expands_collapsed_group_member_active() {
    // When the active tab IS a member of the group, `new_tab_in_group` takes
    // the inheritance path; the group must still expand.
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[workspace.active_tab_index()]
                .group_id
                .expect("active tab should be in a group");

            // Collapse the group, keeping the group member as the active tab.
            workspace.handle_action(&WorkspaceAction::ToggleTabGroupCollapsed(group_id), ctx);
            assert!(
                workspace.tab_groups[&group_id].collapsed,
                "group should be collapsed"
            );

            workspace.handle_action(&WorkspaceAction::NewTabInGroup(group_id), ctx);

            assert!(
                !workspace.tab_groups[&group_id].collapsed,
                "group should expand when a new tab is opened in it"
            );
        });
    });
}

#[test]
fn test_pin_unpin_ungrouped_tab_moves_to_and_from_boundary() {
    let _pinned_guard = FeatureFlag::PinnedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 3);

            let id0 = workspace.tabs[0].pane_group.id();
            let id1 = workspace.tabs[1].pane_group.id();
            let id2 = workspace.tabs[2].pane_group.id();

            // Pin tab at index 2: it should move to the front of the list.
            workspace.handle_action(&WorkspaceAction::PinTab(2), ctx);
            let order: Vec<_> = workspace.tabs.iter().map(|t| t.pane_group.id()).collect();
            assert_eq!(order, vec![id2, id0, id1]);
            assert!(workspace.tabs[0].pinned);
            assert!(!workspace.tabs[1].pinned);
            assert!(!workspace.tabs[2].pinned);

            // Unpin tab at index 0: it should move to the start of the unpinned region.
            workspace.handle_action(&WorkspaceAction::UnpinTab(0), ctx);
            let order: Vec<_> = workspace.tabs.iter().map(|t| t.pane_group.id()).collect();
            assert_eq!(order, vec![id2, id0, id1]);
            assert!(workspace.tabs.iter().all(|t| !t.pinned));
        });
    });
}

#[test]
fn test_pin_unpin_tab_group_moves_block_without_syncing_members() {
    // The group's own `pinned` flag is the sole source of truth for grouped
    // tabs — members keep `tab.pinned = false` regardless.
    let _pinned_guard = FeatureFlag::PinnedTabs.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 4);

            let id0 = workspace.tabs[0].pane_group.id();
            let id1 = workspace.tabs[1].pane_group.id();
            let id2 = workspace.tabs[2].pane_group.id();
            let id3 = workspace.tabs[3].pane_group.id();

            // Group tabs at indices 2, 3.
            let group = TabGroup::new();
            let group_id = group.id;
            workspace.tab_groups.insert(group_id, group);
            workspace.tabs[2].group_id = Some(group_id);
            workspace.tabs[3].group_id = Some(group_id);

            // Pin the group: the block moves to the front; only the group's
            // flag flips — member tabs keep `pinned = false`.
            workspace.handle_action(&WorkspaceAction::PinTabGroup(group_id), ctx);
            let order: Vec<_> = workspace.tabs.iter().map(|t| t.pane_group.id()).collect();
            assert_eq!(order, vec![id2, id3, id0, id1]);
            assert!(workspace.tab_groups[&group_id].pinned);
            assert!(workspace.tabs.iter().all(|t| !t.pinned));

            // Unpin the group: block moves to the start of the unpinned
            // region; group's flag clears.
            workspace.handle_action(&WorkspaceAction::UnpinTabGroup(group_id), ctx);
            assert!(!workspace.tab_groups[&group_id].pinned);
            assert!(workspace.tabs.iter().all(|t| !t.pinned));

            // Group is still contiguous.
            let group_indices: Vec<usize> = workspace
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| t.group_id == Some(group_id))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(group_indices.len(), 2);
            assert_eq!(group_indices[1] - group_indices[0], 1);
        });
    });
}

#[test]
fn test_pin_tab_on_grouped_tab_extracts_then_pins() {
    let _pinned_guard = FeatureFlag::PinnedTabs.override_enabled(true);
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            assert_eq!(workspace.tab_count(), 3);

            let id0 = workspace.tabs[0].pane_group.id();
            let id1 = workspace.tabs[1].pane_group.id();
            let id2 = workspace.tabs[2].pane_group.id();

            // Group tabs 0 and 1; tab 1 is the target.
            let group = TabGroup::new();
            let group_id = group.id;
            workspace.tab_groups.insert(group_id, group);
            workspace.tabs[0].group_id = Some(group_id);
            workspace.tabs[1].group_id = Some(group_id);

            // Pin tab at index 1: extracts from group, then pins as ungrouped.
            workspace.handle_action(&WorkspaceAction::PinTab(1), ctx);

            // Pinned tab (id1) is at the front, ungrouped.
            assert_eq!(workspace.tabs[0].pane_group.id(), id1);
            assert!(workspace.tabs[0].pinned);
            assert!(workspace.tabs[0].group_id.is_none());

            // Source group still has its one remaining member (id0).
            assert_eq!(workspace.tabs[1].pane_group.id(), id0);
            assert_eq!(workspace.tabs[1].group_id, Some(group_id));
            assert!(!workspace.tabs[1].pinned);

            // Ungrouped tab id2 remains untouched.
            assert_eq!(workspace.tabs[2].pane_group.id(), id2);
            assert!(workspace.tabs[2].group_id.is_none());
            assert!(!workspace.tabs[2].pinned);
        });
    });
}

/// Regression test for #14241.
///
/// Creating a tab group opens the inline name editor and also spawns a terminal. About
/// a second later that terminal's bootstrap block becomes visible and takes focus, which
/// blurs the editor while the user is still typing. Blur used to be treated as
/// confirmation, so whatever fragment had been typed became the group's name — and was
/// persisted.
///
/// A rename the user never finished must not be committed.
#[test]
fn test_tab_group_rename_blur_does_not_commit_unfinished_name() {
    let _grouped_tabs_guard = FeatureFlag::GroupedTabs.override_enabled(true);

    App::test((), |mut app| async move {
        initialize_app(&mut app);

        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(
                &WorkspaceAction::SelectNewSessionMenuItem(NewSessionMenuItem::CreateNewTabGroup),
                ctx,
            );
            let group_id = workspace.tabs[0]
                .group_id
                .expect("active tab should be assigned to the new group");
            assert_eq!(workspace.tab_groups[&group_id].name, None);

            workspace.rename_tab_group(group_id, ctx);

            // The user gets three characters in before the terminal is ready.
            workspace
                .tab_group_rename_editor
                .update(ctx, |editor, ctx| {
                    editor.clear_buffer_and_reset_undo_stack(ctx);
                    editor.user_insert("Bui", ctx);
                });

            // The auto-created terminal takes focus; the editor blurs with no user intent
            // to finish.
            workspace.handle_tab_group_rename_editor_event(&EditorEvent::Blurred, ctx);

            assert_eq!(
                workspace.tab_groups[&group_id].name, None,
                "a rename interrupted by the terminal stealing focus must not be committed"
            );
        });
    });
}

// ── SplitViewState unit tests ───────────────────────────────────────────────────────
//
// These tests exercise the pure data structure only – no App context is
// required, so they run without any heavy framework bootstrapping.

#[cfg(test)]
mod split_view_state_tests {
    use galaxyui::EntityId;

    use crate::workspace::view::SplitViewState;

    fn make_id() -> EntityId {
        EntityId::new()
    }

    // Helper: build a state with `n` pre-added IDs; returns (state, ids).
    fn state_with(n: usize) -> (SplitViewState, Vec<EntityId>) {
        let mut s = SplitViewState::default();
        let ids: Vec<EntityId> = (0..n).map(|_| make_id()).collect();
        for &id in &ids {
            s.add(id);
        }
        (s, ids)
    }

    #[test]
    fn default_state_is_inactive() {
        let s = SplitViewState::default();
        assert!(!s.is_active());
        assert_eq!(s.len(), 0);
        assert!(s.ids().is_empty());
        assert_eq!(s.focused_id(), None);
    }

    #[test]
    fn adding_one_id_does_not_make_split_active() {
        let (s, ids) = state_with(1);
        assert!(!s.is_active(), "one slot alone is not an active split");
        assert_eq!(s.len(), 1);
        assert!(s.contains(ids[0]));
    }

    #[test]
    fn adding_two_ids_makes_split_active() {
        let (s, _ids) = state_with(2);
        assert!(s.is_active());
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn members_share_stable_color_until_split_is_cleared() {
        let (mut state, ids) = state_with(3);
        let color = state.color_for(ids[0]);
        assert!(color.is_some());
        assert_eq!(state.color_for(ids[1]), color);
        assert_eq!(state.color_for(ids[2]), color);
        assert_eq!(state.color_for(make_id()), None);

        state.set_focused_id(ids[2]);
        assert_eq!(state.color_for(ids[0]), color);
        assert_eq!(state.color_for(ids[2]), color);
        state.remove(ids[0]);
        assert_eq!(state.color_for(ids[0]), None);
        assert_eq!(state.color_for(ids[1]), color);
        assert_eq!(state.color_for(ids[2]), color);

        state.clear();
        assert_eq!(state.color_for(ids[1]), None);
        assert_eq!(state.color_for(ids[2]), None);
    }

    #[test]
    fn first_added_id_becomes_focused() {
        let (s, ids) = state_with(2);
        assert_eq!(s.focused_id(), Some(ids[0]));
    }

    #[test]
    fn add_is_idempotent_for_same_id() {
        let mut s = SplitViewState::default();
        let id = make_id();
        assert!(s.add(id));
        assert!(!s.add(id), "second add of same id must return false");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn add_respects_max_visible_cap() {
        let (mut s, _) = state_with(SplitViewState::MAX_VISIBLE);
        assert_eq!(s.len(), SplitViewState::MAX_VISIBLE);
        let extra = make_id();
        assert!(!s.add(extra), "adding beyond MAX_VISIBLE must return false");
        assert_eq!(s.len(), SplitViewState::MAX_VISIBLE);
        assert!(!s.contains(extra));
    }

    #[test]
    fn remove_nonexistent_id_returns_false() {
        let (mut s, _) = state_with(2);
        let outside = make_id();
        assert!(!s.remove(outside));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn remove_existing_id_returns_true_and_shrinks() {
        let (mut s, ids) = state_with(3);
        assert!(s.remove(ids[1]));
        assert_eq!(s.len(), 2);
        assert!(!s.contains(ids[1]));
    }

    #[test]
    fn remove_focused_id_transfers_focus_to_first_remaining() {
        let (mut s, ids) = state_with(3);
        // ids[0] is focused by default.
        assert_eq!(s.focused_id(), Some(ids[0]));
        s.remove(ids[0]);
        // After removal, focus should shift to what is now ids[1] (first remaining).
        assert_eq!(s.focused_id(), Some(ids[1]));
    }

    #[test]
    fn remove_non_focused_id_does_not_change_focus() {
        let (mut s, ids) = state_with(3);
        s.remove(ids[2]); // remove last, focus stays on ids[0]
        assert_eq!(s.focused_id(), Some(ids[0]));
    }

    #[test]
    fn clear_resets_to_inactive() {
        let (mut s, _) = state_with(3);
        s.clear();
        assert!(!s.is_active());
        assert_eq!(s.len(), 0);
        assert_eq!(s.focused_id(), None);
    }

    #[test]
    fn set_focused_id_updates_focused_slot() {
        let (mut s, ids) = state_with(2);
        s.set_focused_id(ids[1]);
        assert_eq!(s.focused_id(), Some(ids[1]));
    }

    #[test]
    fn ids_returns_ordered_slice() {
        let (s, ids) = state_with(3);
        assert_eq!(s.ids(), ids.as_slice());
    }

    // ── Lifecycle integration scenarios ───────────────────────────────────────

    /// Simulates: user opens split, closes one slot → auto-exit.
    #[test]
    fn removing_down_to_one_requires_manual_clear_to_fully_exit() {
        let (mut s, ids) = state_with(2);
        assert!(s.is_active());
        s.remove(ids[1]);
        // One slot remaining – not "active" per the >= 2 invariant.
        assert!(!s.is_active());
        // The state still holds one entry; callers must call clear() when
        // is_active() becomes false.
        // This mirrors the production code in remove_tab:
        //   if split_view_state.remove(id) && !split_view_state.is_active() {
        //       split_view_state.clear();
        //   }
        s.clear();
        assert_eq!(s.len(), 0);
    }

    /// Simulates: tab reorder is stable because we use EntityId, not an index.
    #[test]
    fn membership_stable_after_simulated_reorder() {
        // Reorder just means the IDs live in a different position in
        // `workspace.tabs`; the split state itself holds stable IDs and is
        // unaffected.
        let (mut s, ids) = state_with(2);
        // Imagine the workspace reorders: ids[0] swaps with ids[1] in tabs vec.
        // The split state must still report both as members.
        assert!(s.contains(ids[0]));
        assert!(s.contains(ids[1]));
        // A new ID not in the split is not a member.
        let other = make_id();
        assert!(!s.contains(other));
        // We can still add the third slot.
        assert!(s.add(other));
        assert_eq!(s.len(), 3);
    }

    /// Simulates: tab closed while in split → split auto-exits when < 2.
    #[test]
    fn close_last_non_active_slot_exits_split() {
        let mut s = SplitViewState::default();
        let id_a = make_id();
        let id_b = make_id();
        s.add(id_a);
        s.add(id_b);
        assert!(s.is_active());

        // Simulate remove_tab calling remove() then checking is_active().
        let removed = s.remove(id_b);
        assert!(removed);
        assert!(
            !s.is_active(),
            "split should auto-deactivate when only one slot remains"
        );
        s.clear();
        assert_eq!(s.len(), 0);
        assert_eq!(s.focused_id(), None);
    }

    // ── remove_from_split_view regression: focused-slot removed ──────────────
    //
    // Regression: survivor focused_id was captured *after* clear(), which
    // wiped it.  These two tests verify the pure SplitViewState transitions
    // that the workspace function relies on before and after the fix.

    /// 2-slot split: remove the focused slot → state collapses to inactive,
    /// focused_id transferred to the survivor while still readable.
    #[test]
    fn remove_focused_in_2slot_transfers_focus_before_clear() {
        let (mut s, ids) = state_with(2);
        // ids[0] is focused by default (first add sets focused_id).
        assert_eq!(s.focused_id(), Some(ids[0]));

        // Remove the focused slot.
        let removed = s.remove(ids[0]);
        assert!(removed);

        // After remove() — before clear() — focused_id must point at the survivor.
        // This is the value the workspace must capture before calling clear().
        assert_eq!(
            s.focused_id(),
            Some(ids[1]),
            "focused_id must transfer to survivor immediately after remove, before clear"
        );

        // Now the state is no longer active (only 1 slot).
        assert!(!s.is_active());
        // Calling clear() (as the production code does for 2→1) must wipe focused_id.
        s.clear();
        assert_eq!(s.focused_id(), None, "clear() must wipe focused_id");
    }

    /// 3-slot split: remove the focused middle slot → 2 survivors remain,
    /// focused_id transferred to the first survivor, split still active.
    #[test]
    fn remove_focused_in_3slot_transfers_focus_to_first_remaining() {
        let mut s = SplitViewState::default();
        let id_a = make_id();
        let id_b = make_id();
        let id_c = make_id();
        s.add(id_a); // focused_id = id_a
        s.add(id_b);
        s.add(id_c);

        // Make B the focused slot.
        s.set_focused_id(id_b);
        assert_eq!(s.focused_id(), Some(id_b));

        // Remove B (the focused slot).
        let removed = s.remove(id_b);
        assert!(removed);

        // After remove — split still active (2 remaining).
        assert!(s.is_active(), "split must remain active after 3→2");
        assert_eq!(s.len(), 2);

        // focused_id must have been transferred to the first remaining slot (A).
        assert_eq!(
            s.focused_id(),
            Some(id_a),
            "focused_id must transfer to first remaining slot (A) after removing focused B"
        );
        assert!(s.contains(id_a));
        assert!(s.contains(id_c));
        assert!(!s.contains(id_b));
    }

    // ── Resize weight tests ──────────────────────────────────────────────────

    /// Default flex weights are 1.0 for every slot.
    #[test]
    fn default_slot_weights_are_equal() {
        let (s, ids) = state_with(3);
        for &id in &ids {
            assert_eq!(s.slot_weight(id), 1.0, "all initial weights must be 1.0");
        }
    }

    /// Unknown IDs fall back to 1.0.
    #[test]
    fn slot_weight_unknown_id_returns_one() {
        let (s, _) = state_with(2);
        assert_eq!(s.slot_weight(make_id()), 1.0);
    }

    /// Removing a slot also removes its weight entry.
    /// **Proves cleanup removes a mutated weight, not just the initial 1.0.**
    #[test]
    fn weight_cleaned_up_on_remove() {
        let (mut s, ids) = state_with(3);
        // Mutate ids[1]'s weight via a drag so we can prove it was cleaned up.
        let divider_id = s.dividers()[1].id; // right divider (between ids[1] and ids[2])
        s.start_resize(divider_id, 0.0);
        // Apply a drag so ids[1] weight differs from 1.0.
        s.apply_resize_move(100.0, &ids, 1024.0);
        // Now ids[1] weight should be != 1.0.
        let mutated_weight = s.slot_weight(ids[1]);
        assert_ne!(
            mutated_weight, 1.0,
            "weight must have been mutated by the drag"
        );
        s.end_resize();

        // Remove ids[1] – its mutated weight entry must be purged.
        s.remove(ids[1]);
        assert_eq!(
            s.slot_weight(ids[1]),
            1.0,
            "removed slot weight must be purged (falls back to 1.0); was {mutated_weight}"
        );
        // Other slots still hold their own weights.
        assert!(s.slot_weight(ids[0]).is_finite());
        assert!(s.slot_weight(ids[2]).is_finite());
    }

    /// `clear()` removes all weight entries, including mutated ones.
    #[test]
    fn weight_cleaned_up_on_clear() {
        let (mut s, ids) = state_with(2);
        // Mutate left slot weight so we can prove clear actually purges it.
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        s.apply_resize_move(612.0, &ids, 1024.0);
        s.end_resize();
        // After the drag, ids[0] weight should be != 1.0.
        assert_ne!(
            s.slot_weight(ids[0]),
            1.0,
            "weight must have been mutated before testing clear"
        );

        s.clear();
        for &id in &ids {
            assert_eq!(
                s.slot_weight(id),
                1.0,
                "clear must purge all weight entries (falls back to 1.0)"
            );
        }
    }

    /// Divider count equals `len - 1` after adding slots.
    #[test]
    fn divider_count_matches_gaps() {
        let mut s = SplitViewState::default();
        for n in 1..=SplitViewState::MAX_VISIBLE {
            let id = make_id();
            s.add(id);
            let expected_dividers = n.saturating_sub(1);
            assert_eq!(
                s.dividers().len(),
                expected_dividers,
                "expected {expected_dividers} dividers for {n} slots"
            );
        }
    }

    /// Divider count is reduced when a slot is removed.
    #[test]
    fn divider_count_decreases_on_remove() {
        let (mut s, ids) = state_with(3);
        assert_eq!(s.dividers().len(), 2);
        s.remove(ids[2]);
        assert_eq!(s.dividers().len(), 1);
        s.remove(ids[1]);
        // 1 slot → 0 dividers.
        assert_eq!(s.dividers().len(), 0);
    }

    /// Dividers are cleared when `clear()` is called.
    #[test]
    fn dividers_cleared_on_clear() {
        let (mut s, _) = state_with(3);
        s.clear();
        assert_eq!(s.dividers().len(), 0);
    }

    /// Applying a resize move adjusts the two adjacent weights and preserves
    /// their sum.  Uses `start_resize` → `apply_resize_move`.
    #[test]
    fn apply_resize_move_adjusts_adjacent_weights_preserving_sum() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;

        // Start a drag at x = 512.0 (simulated center of a 1024px window).
        s.start_resize(divider_id, 512.0);

        let initial_left = s.slot_weight(ids[0]);
        let initial_right = s.slot_weight(ids[1]);
        let initial_sum = initial_left + initial_right;

        // Drag 100 px to the right → left slot should grow, right should shrink.
        let changed = s.apply_resize_move(612.0, &ids, 1024.0);
        assert!(changed, "a 100px drag must result in a weight change");

        let new_left = s.slot_weight(ids[0]);
        let new_right = s.slot_weight(ids[1]);
        assert!(
            new_left > initial_left,
            "left slot weight must increase after dragging right"
        );
        assert!(
            new_right < initial_right,
            "right slot weight must decrease after dragging right"
        );
        // Total weight is preserved (up to floating-point tolerance).
        let new_sum = new_left + new_right;
        assert!(
            (new_sum - initial_sum).abs() < 1e-3,
            "total weight of the two adjacent slots must be preserved; got {new_sum} vs {initial_sum}"
        );
    }

    /// A zero delta produces no change.
    #[test]
    fn apply_resize_move_no_op_on_zero_delta() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        // Send the same X value – delta == 0.
        let changed = s.apply_resize_move(512.0, &ids, 1024.0);
        assert!(!changed, "zero delta must not produce a weight change");
    }

    /// `apply_resize_move` is a no-op when no drag is active.
    #[test]
    fn apply_resize_move_no_op_without_active_drag() {
        let (mut s, ids) = state_with(2);
        // No start_resize called.
        let changed = s.apply_resize_move(600.0, &ids, 1024.0);
        assert!(!changed, "no drag → no change");
    }

    /// `end_resize` clears the drag state so subsequent move events are no-ops.
    #[test]
    fn end_resize_stops_drag() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        // First move succeeds.
        assert!(s.apply_resize_move(612.0, &ids, 1024.0));
        s.end_resize();
        // After end_resize, moves are no-ops.
        assert!(!s.apply_resize_move(700.0, &ids, 1024.0));
    }

    /// Removing a slot while drag is active clears the drag state.
    #[test]
    fn remove_slot_while_dragging_clears_drag() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        // Remove one slot while dragging.
        s.remove(ids[1]);
        // The drag state must have been cleared by remove().
        // A subsequent move event must be a no-op.
        let ids_after = vec![ids[0]];
        assert!(!s.apply_resize_move(600.0, &ids_after, 1024.0));
    }

    // ── Bounds / nonfinite / exact-pixel coverage ──────────────────────────────────

    /// NaN row_width is rejected (no fallback); returns false and weights unchanged.
    #[test]
    fn nan_row_width_rejected() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        let w0_before = s.slot_weight(ids[0]);
        let changed = s.apply_resize_move(612.0, &ids, f32::NAN);
        assert!(!changed, "NaN row_width must be rejected (returns false)");
        assert_eq!(
            s.slot_weight(ids[0]),
            w0_before,
            "weights must be unchanged after NaN row_width"
        );
    }

    /// Infinite row_width is rejected.
    #[test]
    fn infinite_row_width_rejected() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        let w0_before = s.slot_weight(ids[0]);
        let changed = s.apply_resize_move(612.0, &ids, f32::INFINITY);
        assert!(!changed, "Infinite row_width must be rejected");
        assert_eq!(s.slot_weight(ids[0]), w0_before);
    }

    /// Zero row_width is rejected.
    #[test]
    fn zero_row_width_rejected() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        let w0_before = s.slot_weight(ids[0]);
        let changed = s.apply_resize_move(612.0, &ids, 0.0);
        assert!(!changed, "Zero row_width must be rejected");
        assert_eq!(s.slot_weight(ids[0]), w0_before);
    }

    /// NaN new_x is rejected; `previous_x` must NOT be poisoned.
    #[test]
    fn nan_new_x_rejected() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        // NaN must be rejected immediately without touching any state.
        let changed = s.apply_resize_move(f32::NAN, &ids, 1024.0);
        assert!(!changed, "NaN new_x must be rejected");
        // A subsequent valid move from the same start must still work.
        let changed2 = s.apply_resize_move(612.0, &ids, 1024.0);
        assert!(
            changed2,
            "a valid drag after a NaN rejection must succeed (previous_x not poisoned)"
        );
        assert!(s.slot_weight(ids[0]).is_finite());
        assert!(s.slot_weight(ids[1]).is_finite());
    }

    /// Nonfinite start_x is rejected; drag is not started.
    #[test]
    fn nonfinite_start_x_rejected() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        // start_resize now returns bool.
        assert!(
            !s.start_resize(divider_id, f32::NAN),
            "NaN start_x must be rejected"
        );
        assert!(
            !s.start_resize(divider_id, f32::INFINITY),
            "Inf start_x must be rejected"
        );
        // No drag is active, so apply_resize_move is a no-op.
        assert!(!s.apply_resize_move(600.0, &ids, 1024.0));
    }

    // ── Exact pixel assertions ─────────────────────────────────────────────────

    /// Helper: given two equal-weight slots in a row of `row_width` px,
    /// drag the divider `delta` px and return the resulting slot pixel widths.
    ///
    /// Slot px = weight / total_weight * (row_width - n_dividers * 1px).
    /// With 2 equal-weight slots: each slot starts at (row_width - 1) / 2 px.
    fn slot_px_after_drag(row_width: f32, delta: f32) -> (f32, f32) {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        // Start at the midpoint of the row.
        let start_x = row_width / 2.0;
        s.start_resize(divider_id, start_x);
        s.apply_resize_move(start_x + delta, &ids, row_width);
        // Convert weights back to pixels.
        let total_w = s.slot_weight(ids[0]) + s.slot_weight(ids[1]);
        let slot_px = row_width - 1.0; // 1 divider
        let left_px = s.slot_weight(ids[0]) / total_w * slot_px;
        let right_px = s.slot_weight(ids[1]) / total_w * slot_px;
        (left_px, right_px)
    }

    /// At 1024px with a 100px drag, each slot moves exactly 100px.
    #[test]
    fn exact_pixel_at_1024px_100px_drag() {
        let row_width = 1024.0_f32;
        // Starting widths: (1024-1)/2 = 511.5px each.
        let start_w = (row_width - 1.0) / 2.0;
        let (left_px, right_px) = slot_px_after_drag(row_width, 100.0);
        assert!(
            (left_px - (start_w + 100.0)).abs() < 0.5,
            "left slot must grow by ~100px; got {left_px}"
        );
        assert!(
            (right_px - (start_w - 100.0)).abs() < 0.5,
            "right slot must shrink by ~100px; got {right_px}"
        );
    }

    /// At 800px, the same 100px drag still moves 100px.
    #[test]
    fn exact_pixel_at_800px_100px_drag() {
        let row_width = 800.0_f32;
        let start_w = (row_width - 1.0) / 2.0;
        let (left_px, right_px) = slot_px_after_drag(row_width, 100.0);
        assert!(
            (left_px - (start_w + 100.0)).abs() < 0.5,
            "left at 800px: {left_px}"
        );
        assert!(
            (right_px - (start_w - 100.0)).abs() < 0.5,
            "right at 800px: {right_px}"
        );
    }

    /// At 400px, a 100px drag moves 100px (even when slots are small).
    #[test]
    fn exact_pixel_at_400px_100px_drag() {
        let row_width = 400.0_f32;
        let start_w = (row_width - 1.0) / 2.0; // 199.5px each
        let (left_px, right_px) = slot_px_after_drag(row_width, 100.0);
        assert!(
            (left_px - (start_w + 100.0)).abs() < 0.5,
            "left at 400px: {left_px}"
        );
        assert!(
            (right_px - (start_w - 100.0)).abs() < 0.5,
            "right at 400px: {right_px}"
        );
    }

    // ── Detached slot subset ─────────────────────────────────────────────────────

    /// When one slot is detached (not in visible_ids), the flex conversion uses
    /// only the visible slot weights so the drag distance is correct.
    ///
    /// Setup: 3 slots A, B, C all with weight 1.0.  Slot C is detached so
    /// visible_ids = [A, B].  Row width is 1023px (2 visible slots = 1 divider).
    /// Starting at the midpoint of the visible row (511px), drag 100px right.
    /// Expected: left grows by 100px, right shrinks by 100px (using only A+B weights).
    #[test]
    fn detached_slot_excluded_from_flex_conversion() {
        let (mut s, ids) = state_with(3); // ids = [A, B, C]
        let divider_id = s.dividers()[0].id; // gap between A and B

        // Simulate C being detached: pass only [A, B] as visible_ids.
        let visible = &ids[..2];
        let row_width = 1023.0_f32; // 2 visible slots, 1 divider = 1px
                                    // Starting widths for A and B in this row:
                                    // slot_px = (1023 - 1) / 2 = 511px each (using only visible weights A+B = 2.0).
        let start_x = 511.0_f32; // midpoint of visible row
        s.start_resize(divider_id, start_x);
        let changed = s.apply_resize_move(start_x + 100.0, visible, row_width);
        assert!(changed, "drag must succeed with detached slot excluded");

        // Convert to pixel widths using the visible total weight only.
        let total_visible_w = s.slot_weight(ids[0]) + s.slot_weight(ids[1]);
        let slot_px = row_width - 1.0; // 1 divider
        let a_px = s.slot_weight(ids[0]) / total_visible_w * slot_px;
        let b_px = s.slot_weight(ids[1]) / total_visible_w * slot_px;
        assert!(
            (a_px - 611.0).abs() < 0.5,
            "A must be ~611px after 100px drag; got {a_px}"
        );
        assert!(
            (b_px - 411.0).abs() < 0.5,
            "B must be ~411px after 100px drag; got {b_px}"
        );
        // C's weight is not affected.
        assert_eq!(
            s.slot_weight(ids[2]),
            1.0,
            "detached slot C weight must be unchanged"
        );
    }

    // ── Clamping at minimum slot boundary ──────────────────────────────────────

    /// Dragging hugely to the right clamps at right=50px rather than rejecting.
    /// `previous_x` advances to the clamped position, not raw new_x.
    #[test]
    fn huge_drag_clamped_to_min_slot() {
        let row_width = 1024.0_f32;
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        // Start at the midpoint (each slot ~511.5px).
        s.start_resize(divider_id, 512.0);

        // Drag far past the right boundary (right slot would go to 0).
        let changed = s.apply_resize_move(2000.0, &ids, row_width);
        assert!(
            changed,
            "huge drag must still produce a weight change (clamped)"
        );

        // Right slot must be pinned at MIN_SLOT_PX (50px), not zero/negative.
        let total_w = s.slot_weight(ids[0]) + s.slot_weight(ids[1]);
        let slot_px = row_width - 1.0;
        let right_px = s.slot_weight(ids[1]) / total_w * slot_px;
        assert!(
            (right_px - 50.0).abs() < 1.0,
            "right slot must be clamped to ~50px; got {right_px}"
        );
        assert!(
            s.slot_weight(ids[0]) > s.slot_weight(ids[1]),
            "left slot must be larger after rightward clamp drag"
        );
    }

    /// Dragging into the boundary then further: second drag is a no-op because
    /// previous_x tracks new_x, so the second drag delta is still rightward
    /// and clamped to zero.
    #[test]
    fn drag_at_boundary_then_further_right_is_no_op() {
        let row_width = 1024.0_f32;
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);

        // First drag: clamp to right boundary.
        let changed1 = s.apply_resize_move(2000.0, &ids, row_width);
        assert!(changed1);
        let w0_at_boundary = s.slot_weight(ids[0]);
        let w1_at_boundary = s.slot_weight(ids[1]);

        // Second drag: even further right.  right slot is already at min;
        // delta = 3000 - 2000 = +1000 → clamped to 0 → no weight change.
        let changed2 = s.apply_resize_move(3000.0, &ids, row_width);
        assert!(!changed2, "drag past clamped boundary must be a no-op");
        assert_eq!(s.slot_weight(ids[0]), w0_at_boundary);
        assert_eq!(s.slot_weight(ids[1]), w1_at_boundary);
    }

    /// Dragging leftward from the right boundary retreats correctly (reversal).
    /// previous_x = new_x, so after reaching x=2000, previous_x=2000.
    /// Drag to boundary_x-100 = 873 gives delta = 873-2000 = -1127 → leftward.
    #[test]
    fn drag_reversal_after_clamp() {
        let row_width = 1024.0_f32;
        let boundary_x = row_width - 1.0 - 50.0; // 973
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        // Clamp to boundary; previous_x = 2000.
        s.apply_resize_move(2000.0, &ids, row_width);

        // Drag leftward: new_x < previous_x (2000), delta is negative → reversal.
        let changed = s.apply_resize_move(boundary_x - 100.0, &ids, row_width);
        assert!(changed, "reversal drag must succeed after right clamp");

        let total_w = s.slot_weight(ids[0]) + s.slot_weight(ids[1]);
        let slot_px = row_width - 1.0;
        let right_px = s.slot_weight(ids[1]) / total_w * slot_px;
        assert!(
            right_px > 50.0,
            "right slot must grow above 50px on reversal; got {right_px}"
        );
    }

    /// 2000→1999 must NOT be sticky: with previous_x tracking new_x, dragging
    /// from x=2000 (clamped, previous_x=2000) to x=1999 gives delta=-1 which
    /// is a valid leftward move producing a weight change immediately.
    #[test]
    fn reversal_one_px_from_clamp_is_not_sticky() {
        let row_width = 1024.0_f32;
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);

        // Reach the right boundary: previous_x becomes 2000.
        s.apply_resize_move(2000.0, &ids, row_width);
        let w1_at_boundary = s.slot_weight(ids[1]);

        // Move back exactly 1px.  This must produce a weight change (not sticky).
        let changed = s.apply_resize_move(1999.0, &ids, row_width);
        assert!(
            changed,
            "2000→1999 must immediately produce a weight change (no sticky frame)"
        );
        // Right slot must have grown (divider moved left by 1px).
        assert!(
            s.slot_weight(ids[1]) > w1_at_boundary,
            "right slot weight must increase when dragging left from boundary"
        );
    }

    /// Outward drag past boundary then immediate reversal by 1px works.
    /// Tests the full sequence: free move → boundary → 1px reversal.
    #[test]
    fn outward_then_1px_reversal_works() {
        let row_width = 1024.0_f32;
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);

        // Free move: 100px to the right.
        let changed1 = s.apply_resize_move(612.0, &ids, row_width);
        assert!(changed1, "free move must change weights");
        let w0_mid = s.slot_weight(ids[0]);

        // Large outward drag past boundary.
        let changed2 = s.apply_resize_move(2000.0, &ids, row_width);
        assert!(changed2, "outward drag must change weights (clamped)");
        let w1_boundary = s.slot_weight(ids[1]);

        // Reverse by 1px: must immediately change weights.
        let changed3 = s.apply_resize_move(1999.0, &ids, row_width);
        assert!(
            changed3,
            "1px reversal after outward clamp must change weights"
        );
        assert!(
            s.slot_weight(ids[1]) > w1_boundary,
            "right weight must grow on 1px reversal"
        );
        // Left slot must still be larger than its mid-drag weight (we're near the
        // right boundary, left is large).
        assert!(
            s.slot_weight(ids[0]) > w0_mid,
            "left weight must still exceed mid-drag value near boundary"
        );
    }

    /// Adjacent weights remain finite and positive after many small drag steps.
    #[test]
    fn repeated_drags_stay_finite_and_positive() {
        let (mut s, ids) = state_with(2);
        let divider_id = s.dividers()[0].id;
        s.start_resize(divider_id, 512.0);
        let row_width = 1024.0_f32;
        let mut x = 512.0_f32;
        for _ in 0..20 {
            x += 10.0;
            s.apply_resize_move(x, &ids, row_width);
        }
        s.end_resize();
        assert!(
            s.slot_weight(ids[0]).is_finite() && s.slot_weight(ids[0]) > 0.0,
            "left weight must remain finite and positive"
        );
        assert!(
            s.slot_weight(ids[1]).is_finite() && s.slot_weight(ids[1]) > 0.0,
            "right weight must remain finite and positive"
        );
        let pair_sum = s.slot_weight(ids[0]) + s.slot_weight(ids[1]);
        assert!(
            (pair_sum - 2.0).abs() < 1e-2,
            "pair sum must stay close to 2.0; got {pair_sum}"
        );
    }
} // end split_view_state_tests
#[test]
fn test_split_pane_focused_event_reconciles_tab_selection_and_header_membership() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            let first = workspace.get_pane_group_view(0).unwrap().clone();
            let second = workspace.get_pane_group_view(1).unwrap().clone();
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(second.id(), ctx);

            // A tab-bar selection and a pane focus event can arrive separately.
            workspace.split_view_state.set_focused_id(second.id());
            workspace.handle_file_tree_event(second.clone(), &pane_group::Event::PaneFocused, ctx);
            assert_eq!(workspace.active_tab_pane_group().id(), second.id());
            assert_eq!(workspace.split_view_state.focused_id(), Some(second.id()));

            // Reconcile stale split focus even when the selected tab is correct.
            workspace.split_view_state.set_focused_id(first.id());
            workspace.handle_file_tree_event(second.clone(), &pane_group::Event::PaneFocused, ctx);
            assert_eq!(workspace.active_tab_pane_group().id(), second.id());
            assert_eq!(workspace.split_view_state.focused_id(), Some(second.id()));
            assert_eq!(workspace.split_view_state.ids(), &[first.id(), second.id()]);
            assert!(
                !first
                    .as_ref(ctx)
                    .focus_state_handle()
                    .as_ref(ctx)
                    .workspace_split_view()
                    .unwrap()
                    .is_focused
            );
            assert!(
                second
                    .as_ref(ctx)
                    .focus_state_handle()
                    .as_ref(ctx)
                    .workspace_split_view()
                    .unwrap()
                    .is_focused
            );
        });
    });
}

#[test]
fn test_split_focus_pane_group_event_preserves_members_and_exits_for_another_tab() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            let first = workspace.get_pane_group_view(0).unwrap().clone();
            let second = workspace.get_pane_group_view(1).unwrap().clone();
            let outside = workspace.get_pane_group_view(2).unwrap().clone();
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(second.id(), ctx);

            workspace.handle_file_tree_event(
                second.clone(),
                &pane_group::Event::FocusPaneGroup,
                ctx,
            );
            assert_eq!(workspace.active_tab_pane_group().id(), second.id());
            assert_eq!(workspace.split_view_state.focused_id(), Some(second.id()));
            assert_eq!(workspace.split_view_state.ids(), &[first.id(), second.id()]);

            workspace.handle_file_tree_event(
                outside.clone(),
                &pane_group::Event::FocusPaneGroup,
                ctx,
            );
            assert_eq!(workspace.active_tab_pane_group().id(), outside.id());
            assert!(workspace.split_view_state.ids().is_empty());
            for pane_group in [first, second, outside] {
                assert_eq!(
                    pane_group
                        .as_ref(ctx)
                        .focus_state_handle()
                        .as_ref(ctx)
                        .workspace_split_view(),
                    None
                );
            }
        });
    });
}

#[test]
fn test_open_active_tab_in_split_view_does_not_create_membership() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            let active = workspace.active_tab_pane_group().clone();
            workspace.open_tab_in_split_view(active.id(), ctx);
            assert!(workspace.split_view_state.ids().is_empty());
            assert_eq!(workspace.split_view_state.focused_id(), None);
            assert_eq!(
                active
                    .as_ref(ctx)
                    .focus_state_handle()
                    .as_ref(ctx)
                    .workspace_split_view(),
                None
            );
        });
    });
}

#[test]
fn test_rename_outside_split_tab_exits_split_even_when_rename_is_cancelled() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            workspace.add_terminal_tab(false, ctx);
            let first = workspace.get_pane_group_view(0).unwrap().clone();
            let outside = workspace.get_pane_group_view(1).unwrap().clone();
            let third = workspace.get_pane_group_view(2).unwrap().clone();
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(third.id(), ctx);
            assert_eq!(workspace.split_view_state.ids(), &[first.id(), third.id()]);

            workspace.handle_action(&WorkspaceAction::RenameTab(1), ctx);
            assert_eq!(workspace.active_tab_pane_group().id(), outside.id());
            assert!(workspace.current_workspace_state.is_tab_being_renamed());
            assert!(workspace.split_view_state.ids().is_empty());
            workspace.handle_action(&WorkspaceAction::CancelActiveRename, ctx);
            assert!(!workspace.current_workspace_state.is_tab_being_renamed());
            assert_eq!(workspace.active_tab_pane_group().id(), outside.id());
            assert!(workspace.split_view_state.ids().is_empty());
            for pane_group in [first, outside, third] {
                assert_eq!(
                    pane_group
                        .as_ref(ctx)
                        .focus_state_handle()
                        .as_ref(ctx)
                        .workspace_split_view(),
                    None
                );
            }
        });
    });
}

// ── Issue 2: remove_tab must prefer remaining split member (workspace lifecycle) ──

/// Scenario: tabs A B C D E with split view [A, C, E], C is active.
/// Closing C must activate A or E (a remaining split member), not D
/// (the adjacent non-member), and the split [A, E] must remain active.
///
/// This is a workspace-lifecycle test that exercises `remove_tab` and the
/// new `remaining_split_ids` preference path end-to-end.
#[test]
fn test_close_active_split_tab_activates_remaining_split_member() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        // Start with 1 tab (A).  Add B, C, D, E – we now have 5 tabs 0..4.
        let (id_a, id_b, id_c, id_d, id_e) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B → index 1
            workspace.add_terminal_tab(false, ctx); // C → index 2
            workspace.add_terminal_tab(false, ctx); // D → index 3
            workspace.add_terminal_tab(false, ctx); // E → index 4
            assert_eq!(workspace.tab_count(), 5);

            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            let c = workspace.get_pane_group_view(2).unwrap().id();
            let d = workspace.get_pane_group_view(3).unwrap().id();
            let e = workspace.get_pane_group_view(4).unwrap().id();
            (a, b, c, d, e)
        });

        // Build split [A, C, E] and make C the active/focused slot.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(0, ctx);
            // Open split: A (already active) + C.
            workspace.open_tab_in_split_view(id_c, ctx);
            // Add E to the existing split.
            workspace.open_tab_in_split_view(id_e, ctx);

            assert!(
                workspace.split_view_state.is_active(),
                "split must be active"
            );
            assert_eq!(workspace.split_view_state.len(), 3);
            assert!(workspace.split_view_state.contains(id_a));
            assert!(workspace.split_view_state.contains(id_c));
            assert!(workspace.split_view_state.contains(id_e));

            // Make C the active/focused tab.
            workspace.handle_pane_group_focus_in_split(
                id_c,
                workspace
                    .tabs
                    .iter()
                    .position(|t| t.pane_group.id() == id_c)
                    .unwrap(),
                ctx,
            );
            assert_eq!(workspace.split_view_state.focused_id(), Some(id_c));
            assert_eq!(workspace.active_tab_pane_group().id(), id_c);
        });

        // Close C (the active split slot).
        workspace.update(&mut app, |workspace, ctx| {
            let idx_c = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == id_c)
                .unwrap();
            workspace.close_tab(idx_c, true, true, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            // Split must still be active with [A, E].
            assert!(
                workspace.split_view_state.is_active(),
                "split must remain active after closing one of three members"
            );
            assert_eq!(workspace.split_view_state.len(), 2);
            assert!(
                workspace.split_view_state.contains(id_a),
                "A must remain in split"
            );
            assert!(
                workspace.split_view_state.contains(id_e),
                "E must remain in split"
            );
            // The workspace must NOT have activated D (the non-member adjacent tab).
            let active_id = workspace.active_tab_pane_group().id();
            assert_ne!(
                active_id, id_d,
                "must NOT activate the adjacent non-split member D"
            );
            assert_ne!(active_id, id_b, "must NOT activate B");
            // The activated tab must be a remaining split member.
            assert!(
                active_id == id_a || active_id == id_e,
                "active tab after close must be a remaining split member (A or E), got neither"
            );
        });
    });
}

/// Closing a split-member tab when only one member remains collapses to
/// single-tab mode.  Uses A/B/C with split [A,C] and C active so that the
/// adjacent non-member B would be incorrectly chosen by old code.
#[test]
fn test_close_split_to_one_exits_split_view() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        let (id_a, id_b, id_c) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B at index 1
            workspace.add_terminal_tab(false, ctx); // C at index 2
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            let c = workspace.get_pane_group_view(2).unwrap().id();
            (a, b, c)
        });

        // Open split [A, C] and make C the active/focused slot.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(id_c, ctx);
            assert!(workspace.split_view_state.is_active());
            workspace.handle_pane_group_focus_in_split(
                id_c,
                workspace
                    .tabs
                    .iter()
                    .position(|t| t.pane_group.id() == id_c)
                    .unwrap(),
                ctx,
            );
            assert_eq!(workspace.split_view_state.focused_id(), Some(id_c));
        });

        // Close C: only A remains in the split → split must exit, activating A not B.
        workspace.update(&mut app, |workspace, ctx| {
            let idx_c = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == id_c)
                .unwrap();
            workspace.close_tab(idx_c, true, true, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            assert!(
                !workspace.split_view_state.is_active(),
                "split must exit when only one member remains"
            );
            assert_eq!(workspace.split_view_state.len(), 0);
            assert_eq!(workspace.tab_count(), 2, "A and B must still exist");
            // Must activate the surviving split member A, not the adjacent non-member B.
            let active = workspace.active_tab_pane_group().id();
            assert_ne!(active, id_b, "must NOT activate non-member B");
            assert_eq!(active, id_a, "must activate surviving split member A");
        });
    });
}

// ── Drag-out / detach regression tests ───────────────────────────────────
//
// These tests enter the production bookkeeping path after preview creation;
// the App fixture does not create or transfer a native preview window.

/// Dragging the active non-adjacent split member (A from split [A,C,E]) out
/// of the window must:
/// - Exit split view entirely.
/// - Activate a surviving split member (C or E), not the adjacent tab B.
/// - Record the dragged tab's ID for drop bookkeeping.
#[test]
fn test_detach_active_split_tab_prefers_split_survivor_not_adjacent() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        // Build tabs A(0) B(1) C(2) D(3) E(4), split [A,C,E] active A.
        let (id_a, id_b, id_c, _id_d, id_e) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B
            workspace.add_terminal_tab(false, ctx); // C
            workspace.add_terminal_tab(false, ctx); // D
            workspace.add_terminal_tab(false, ctx); // E
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            let c = workspace.get_pane_group_view(2).unwrap().id();
            let d = workspace.get_pane_group_view(3).unwrap().id();
            let e = workspace.get_pane_group_view(4).unwrap().id();
            (a, b, c, d, e)
        });

        workspace.update(&mut app, |workspace, ctx| {
            // Open split [A,C,E] with A active/focused.
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(id_c, ctx);
            workspace.open_tab_in_split_view(id_e, ctx);
            assert!(workspace.split_view_state.is_active());
            assert_eq!(workspace.active_tab_pane_group().id(), id_a);
        });

        // on_tab_drag marks the placeholder before starting the preview.
        workspace.update(&mut app, |workspace, ctx| {
            let idx_a = workspace
                .tabs
                .iter()
                .position(|t| t.pane_group.id() == id_a)
                .unwrap();

            workspace.tabs[idx_a].detached = true;
            workspace.finish_multi_tab_detach(idx_a, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            // Split must be fully cleared.
            assert!(
                !workspace.split_view_state.is_active(),
                "split must exit on detach"
            );
            assert_eq!(workspace.split_view_state.len(), 0);

            // Active tab must be a surviving split member (C or E), NOT B.
            let active_id = workspace.active_tab_pane_group().id();
            assert_ne!(active_id, id_b, "must NOT activate adjacent non-member B");
            assert!(
                active_id == id_c || active_id == id_e,
                "must activate a surviving split member (C or E)"
            );

            assert_eq!(
                workspace.detached_tab_pane_group_id,
                Some(id_a),
                "detached_tab_pane_group_id must be set to A"
            );
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_drop_result(
                DropResult::RemoveSourceTab {
                    transferred_tab_index: 0,
                },
                ctx,
            );
        });
        workspace.read(&app, |workspace, _ctx| {
            assert_eq!(workspace.tab_count(), 4);
            assert!(workspace.tabs.iter().all(|tab| tab.pane_group.id() != id_a));
            let active_id = workspace.active_tab_pane_group().id();
            assert!(active_id == id_c || active_id == id_e);
            assert!(workspace.split_view_state.ids().is_empty());
        });
    });
}

#[test]
fn test_drop_tab_cleans_detach_bookkeeping_without_changing_survivor_selection() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        let (id_a, id_c) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B
            workspace.add_terminal_tab(false, ctx); // C
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let c = workspace.get_pane_group_view(2).unwrap().id();
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(c, ctx);
            assert!(workspace.split_view_state.is_active());
            (a, c)
        });

        workspace.update(&mut app, |workspace, ctx| {
            workspace.tabs[0].detached = true;
            workspace.finish_multi_tab_detach(0, ctx);
            assert_eq!(workspace.active_tab_pane_group().id(), id_c);
            assert_eq!(workspace.detached_tab_pane_group_id, Some(id_a));
        });

        // Exercise DropTab cleanup without manufacturing a preview/put-back
        // route; the cross-window model has no active drag in this fixture.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(&WorkspaceAction::DropTab, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            assert_eq!(workspace.active_tab_pane_group().id(), id_c);
            assert!(workspace.tabs.iter().all(|tab| !tab.detached));
            assert_eq!(workspace.detached_tab_pane_group_id, None);
            assert!(workspace.split_view_state.ids().is_empty());
        });
    });
}

// ---------------------------------------------------------------------------
// Drag-to-split tests
// ---------------------------------------------------------------------------

/// Helper: set a `PendingDragSplit` on `workspace` directly (bypasses the
/// position-cache geometry that is not populated in headless tests).
fn set_pending_drag_split(
    workspace: &mut Workspace,
    target_id: galaxyui::EntityId,
    source_id: galaxyui::EntityId,
) {
    workspace.pending_drag_split = Some(crate::workspace::view::PendingDragSplit {
        target_id,
        source_id,
    });
}

/// Dropping with a pending split opens both tabs in split view with the target
/// (hovered) tab on the left and the dragged (source) tab on the right.
#[test]
fn test_drop_with_pending_split_opens_split_view_target_left_source_right() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        let (id_a, id_b) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B at index 1
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            // Simulate dragging A over the center of B.
            set_pending_drag_split(workspace, b, a); // target=B, source=A
            (a, b)
        });

        // Trigger DropTab (non-cross-window; no active detach).
        workspace.update(&mut app, |workspace, ctx| {
            workspace.handle_action(&WorkspaceAction::DropTab, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            // Split view must now be active with exactly two slots.
            assert!(
                workspace.split_view_state.is_active(),
                "split view must be active after drag-to-split drop"
            );
            let ids = workspace.split_view_state.ids();
            assert_eq!(ids.len(), 2, "split must have exactly two slots");
            // Target (B) must be on the left (first), source (A) on the right.
            assert_eq!(ids[0], id_b, "target tab must be the left (first) slot");
            assert_eq!(ids[1], id_a, "source tab must be the right (second) slot");
            // pending_drag_split must be cleared.
            assert_eq!(
                workspace.pending_drag_split, None,
                "pending_drag_split must be cleared after drop"
            );
        });
    });
}

/// Dropping without any pending split does NOT create a split view.
#[test]
fn test_drop_without_pending_split_does_not_create_split() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B
                                                    // No pending_drag_split.
            workspace.handle_action(&WorkspaceAction::DropTab, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            assert!(
                !workspace.split_view_state.is_active(),
                "no split should be created when pending_drag_split is None"
            );
        });
    });
}

/// Dragging a tab over itself should not set a pending split.
/// (The center-zone loop skips `i == current_index`, so self-hover is never
/// possible at the logic level; this test validates the invariant holds via
/// the state API.)
#[test]
fn test_pending_drag_split_self_target_not_allowed() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            let id_a = workspace.get_pane_group_view(0).unwrap().id();
            // Attempting to set source == target must result in a no-op split
            // (IDs match, so `open_tab_in_split_view` guard rejects it).
            set_pending_drag_split(workspace, id_a, id_a); // self-target
            workspace.handle_action(&WorkspaceAction::DropTab, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            // With source == target, `open_tab_in_split_view` returns early
            // because a tab cannot split with itself.
            assert!(
                !workspace.split_view_state.is_active(),
                "a tab cannot be split with itself"
            );
            assert_eq!(workspace.pending_drag_split, None);
        });
    });
}

/// Stale IDs: if the target tab was closed between setting the pending split
/// and the drop, the split is not committed and no crash occurs.
#[test]
fn test_pending_drag_split_stale_target_id_is_ignored() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        let (id_a, id_b) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            (a, b)
        });

        // Close B (the target), then set a pending split referencing the now-
        // gone ID and drop.
        workspace.update(&mut app, |workspace, ctx| {
            workspace.close_tab(1, true, false, ctx);
            assert_eq!(workspace.tab_count(), 1);
        });

        workspace.update(&mut app, |workspace, ctx| {
            // Set stale pending split (target B no longer exists).
            workspace.pending_drag_split = Some(crate::workspace::view::PendingDragSplit {
                target_id: id_b,
                source_id: id_a,
            });
            workspace.handle_action(&WorkspaceAction::DropTab, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            assert!(
                !workspace.split_view_state.is_active(),
                "stale target ID must not create a split"
            );
            assert_eq!(workspace.pending_drag_split, None);
        });
    });
}

/// StartTabDrag always clears any pending_drag_split from a previous gesture.
#[test]
fn test_start_tab_drag_clears_pending_drag_split() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            // Simulate a stale pending split from a previous drag.
            set_pending_drag_split(workspace, b, a);
            assert!(workspace.pending_drag_split.is_some());
            // A new StartTabDrag must clear it.
            workspace.handle_action(&WorkspaceAction::StartTabDrag, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            assert_eq!(
                workspace.pending_drag_split, None,
                "StartTabDrag must clear pending_drag_split"
            );
        });
    });
}

/// When an existing split view is active and a drag-to-split drop happens,
/// the existing split is exited first so the result is exactly the two
/// dragged tabs, not a larger accumulated split.
#[test]
fn test_pending_drag_split_exits_prior_split_before_creating_new_pair() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        let (id_a, id_b, id_c) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx); // B
            workspace.add_terminal_tab(false, ctx); // C
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            let c = workspace.get_pane_group_view(2).unwrap().id();
            // Open an existing split [A, C].
            workspace.activate_tab(0, ctx);
            workspace.open_tab_in_split_view(c, ctx);
            assert!(workspace.split_view_state.is_active());
            (a, b, c)
        });

        // Now drag A onto B's center zone and drop.
        workspace.update(&mut app, |workspace, ctx| {
            set_pending_drag_split(workspace, id_b, id_a); // target=B, source=A
            workspace.handle_action(&WorkspaceAction::DropTab, ctx);
        });

        workspace.read(&app, |workspace, _ctx| {
            let ids = workspace.split_view_state.ids();
            assert_eq!(ids.len(), 2);
            // The prior member C must NOT be in the new split.
            assert!(!ids.contains(&id_c), "prior split member C must be absent");
            assert_eq!(ids[0], id_b, "target B must be left");
            assert_eq!(ids[1], id_a, "source A must be right");
        });
    });
}

// ---------------------------------------------------------------------------
// Pure geometry tests for `select_drag_split_target`
// These do not require App::test; they call the extracted helper directly.
// ---------------------------------------------------------------------------

fn make_candidate(index: usize, x: f32, width: f32) -> DragSplitCandidate {
    DragSplitCandidate {
        index,
        id: galaxyui::EntityId::new(),
        rect: RectF::from_points(
            pathfinder_geometry::vector::vec2f(x, 0.),
            pathfinder_geometry::vector::vec2f(x + width, 34.),
        ),
        skip: false,
    }
}

fn make_candidate_v(index: usize, y: f32, height: f32) -> DragSplitCandidate {
    DragSplitCandidate {
        index,
        id: galaxyui::EntityId::new(),
        rect: RectF::from_points(
            pathfinder_geometry::vector::vec2f(0., y),
            pathfinder_geometry::vector::vec2f(220., y + height),
        ),
        skip: false,
    }
}

/// Cursor at exact center of tab B → hits B's center zone.
#[test]
fn test_select_split_target_center_hits() {
    let a = make_candidate(0, 0., 150.); // source
    let b = make_candidate(1, 150., 150.); // target, center at x=225
                                           // Cursor at exact center of B (x=225), y=17 (within 0..34 perp).
    let cursor = pathfinder_geometry::vector::vec2f(225., 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, Some(b.id), "center cursor must hit B");
}

/// Cursor at leading edge of tab B → misses (edge zone, not center).
#[test]
fn test_select_split_target_leading_edge_misses() {
    let a = make_candidate(0, 0., 150.);
    let b = make_candidate(1, 150., 150.); // center zone: 150+50=200 .. 150+100=250
                                           // Cursor at x=155 — inside B but in the leading edge zone.
    let cursor = pathfinder_geometry::vector::vec2f(155., 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "leading-edge cursor must not hit split zone");
}

/// Cursor at trailing edge of tab B → misses.
#[test]
fn test_select_split_target_trailing_edge_misses() {
    let a = make_candidate(0, 0., 150.);
    let b = make_candidate(1, 150., 150.); // center zone: 200..250
                                           // Cursor at x=295 — inside B but in the trailing edge zone.
    let cursor = pathfinder_geometry::vector::vec2f(295., 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "trailing-edge cursor must not hit split zone");
}

/// Self-hover: source_index matches the only center-zone candidate → None.
#[test]
fn test_select_split_target_self_skipped() {
    let tab = make_candidate(0, 0., 150.);
    let cursor = pathfinder_geometry::vector::vec2f(75., 17.); // center of tab
    let result = select_drag_split_target(&[tab], 0, cursor, false);
    assert_eq!(result, None, "source tab must never be a split target");
}

/// `skip=true` on the only candidate → None.
#[test]
fn test_select_split_target_skipped_candidate() {
    let mut b = make_candidate(1, 150., 150.);
    b.skip = true;
    let a = make_candidate(0, 0., 150.);
    let cursor = pathfinder_geometry::vector::vec2f(225., 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "skip=true candidate must not be a target");
}

/// Zero-width tab rect → skipped (non-visible / collapsed).
#[test]
fn test_select_split_target_zero_width_skipped() {
    let a = make_candidate(0, 0., 150.);
    let b = make_candidate(1, 150., 0.); // zero width → collapsed/hidden
    let cursor = pathfinder_geometry::vector::vec2f(150., 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "zero-width tab must not match");
}

/// Perpendicular axis out of bounds → rejected (stale position-cache entry).
#[test]
fn test_select_split_target_perpendicular_out_of_bounds_rejected() {
    let a = make_candidate(0, 0., 150.);
    let b = make_candidate(1, 150., 150.); // rect y=[0..34]
                                           // Cursor is in the center zone on X but above the tab on Y.
    let cursor = pathfinder_geometry::vector::vec2f(225., 100.); // y=100 > 34
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "out-of-bounds perpendicular must be rejected");
}

/// Vertical layout: cursor at center on the Y axis hits target.
#[test]
fn test_select_split_target_vertical_center_hits() {
    let a = make_candidate_v(0, 0., 36.);
    let b = make_candidate_v(1, 36., 36.); // center zone y: 36+12=48 .. 36+24=60
                                           // Cursor at y=54 (center of B), x=110 (within 0..220 perp).
    let cursor = pathfinder_geometry::vector::vec2f(110., 54.);
    let result = select_drag_split_target(&[a, b], 0, cursor, true);
    assert_eq!(result, Some(b.id), "vertical center cursor must hit B");
}

/// Vertical layout: cursor at leading edge on Y → misses.
#[test]
fn test_select_split_target_vertical_leading_edge_misses() {
    let a = make_candidate_v(0, 0., 36.);
    let b = make_candidate_v(1, 36., 36.); // center zone y: 48..60
                                           // Cursor at y=38 — in leading edge zone.
    let cursor = pathfinder_geometry::vector::vec2f(110., 38.);
    let result = select_drag_split_target(&[a, b], 0, cursor, true);
    assert_eq!(
        result, None,
        "vertical leading-edge cursor must not hit split zone"
    );
}

/// is_tab_split_drop_target returns false when pending_drag_split is None.
#[test]
fn test_is_tab_split_drop_target_no_pending() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
        });
        workspace.read(&app, |workspace, _ctx| {
            assert!(!workspace.is_tab_split_drop_target(0));
            assert!(!workspace.is_tab_split_drop_target(1));
        });
    });
}

/// is_tab_split_drop_target returns true only for the target tab's index.
#[test]
fn test_is_tab_split_drop_target_correct_index() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);

        let (id_a, id_b) = workspace.update(&mut app, |workspace, ctx| {
            workspace.add_terminal_tab(false, ctx);
            let a = workspace.get_pane_group_view(0).unwrap().id();
            let b = workspace.get_pane_group_view(1).unwrap().id();
            // B is the target, A is the source.
            set_pending_drag_split(workspace, b, a);
            (a, b)
        });

        workspace.read(&app, |workspace, _ctx| {
            // Index 1 (B) is the target → true.
            assert!(
                workspace.is_tab_split_drop_target(1),
                "target tab index must return true"
            );
            // Index 0 (A, the source) is not the target → false.
            assert!(
                !workspace.is_tab_split_drop_target(0),
                "source tab index must return false"
            );
            let _ = (id_a, id_b); // silence unused warning
        });
    });
}

// ---------------------------------------------------------------------------
// is_eligible_split_candidate tests
// ---------------------------------------------------------------------------

fn bar_rect() -> RectF {
    RectF::from_points(
        pathfinder_geometry::vector::vec2f(0., 0.),
        pathfinder_geometry::vector::vec2f(600., 34.),
    )
}

fn tab_inside_bar(x: f32) -> RectF {
    RectF::from_points(
        pathfinder_geometry::vector::vec2f(x, 0.),
        pathfinder_geometry::vector::vec2f(x + 150., 34.),
    )
}

fn tab_outside_bar() -> RectF {
    // rect whose center (875, 17) is outside bar_rect (0..600 x 0..34)
    RectF::from_points(
        pathfinder_geometry::vector::vec2f(800., 0.),
        pathfinder_geometry::vector::vec2f(950., 34.),
    )
}

/// Visible tab inside the bar → eligible.
#[test]
fn test_eligible_visible_inside_bar() {
    assert!(
        is_eligible_split_candidate(tab_inside_bar(0.), &[bar_rect()], false, false),
        "visible tab inside bar must be eligible"
    );
}

/// Detached tab (cross-window preview) → ineligible even if rect is in bar.
#[test]
fn test_eligible_detached_rejected() {
    assert!(
        !is_eligible_split_candidate(tab_inside_bar(0.), &[bar_rect()], true, false),
        "detached tab must not be eligible"
    );
}

/// Collapsed-group member → ineligible.
#[test]
fn test_eligible_collapsed_group_rejected() {
    assert!(
        !is_eligible_split_candidate(tab_inside_bar(0.), &[bar_rect()], false, true),
        "collapsed-group member must not be eligible"
    );
}

/// Stale rect outside the active bar (nonzero dims) → ineligible.
/// This is the key case: a tab with a real nonzero rect that is simply outside
/// the visible bar region (e.g. a floating ghost chip or overflow-clipped slot)
/// must be excluded even though its geometry looks valid.
#[test]
fn test_eligible_stale_nonzero_rect_outside_bar_rejected() {
    assert!(
        !is_eligible_split_candidate(tab_outside_bar(), &[bar_rect()], false, false),
        "nonzero rect outside bar must not be eligible (stale position-cache entry)"
    );
}

/// Sole-grouped-member tab with valid rect inside bar → eligible target.
/// The sole-member exemption only affects the *source* draggable path; a
/// different tab can legitimately drop onto a sole-member tab to open a split.
#[test]
fn test_eligible_sole_grouped_member_is_eligible_target() {
    // is_eligible_split_candidate does not receive a sole-member flag;
    // the caller no longer sets skip=true for sole members.  Verify the
    // helper treats a plain in-bar non-detached non-collapsed tab as eligible.
    assert!(
        is_eligible_split_candidate(tab_inside_bar(150.), &[bar_rect()], false, false),
        "sole-grouped-member tab must be eligible as a split target"
    );
}

/// Empty bar_rects (first frame, cache not populated) → all rects accepted.
#[test]
fn test_eligible_empty_bar_rects_accepts_all() {
    // When the position cache hasn’t populated yet, bar_rects is empty and
    // the helper should not reject any non-detached, non-collapsed candidate.
    assert!(
        is_eligible_split_candidate(tab_outside_bar(), &[], false, false),
        "empty bar_rects must accept all non-detached non-collapsed candidates"
    );
}

// ---------------------------------------------------------------------------
// NaN cursor / NaN rect tests for select_drag_split_target
// ---------------------------------------------------------------------------

/// NaN cursor x → returns None immediately.
#[test]
fn test_select_split_target_nan_cursor_x_returns_none() {
    let a = make_candidate(0, 0., 150.);
    let b = make_candidate(1, 150., 150.);
    let cursor = pathfinder_geometry::vector::vec2f(f32::NAN, 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "NaN cursor x must return None");
}

/// NaN cursor y → returns None.
#[test]
fn test_select_split_target_nan_cursor_y_returns_none() {
    let a = make_candidate(0, 0., 150.);
    let b = make_candidate(1, 150., 150.);
    let cursor = pathfinder_geometry::vector::vec2f(225., f32::NAN);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "NaN cursor y must return None");
}

/// NaN rect min_x → that candidate is skipped, None returned.
#[test]
fn test_select_split_target_nan_rect_skipped() {
    let a = make_candidate(0, 0., 150.);
    // Manually construct a candidate with a NaN rect.
    let mut b = make_candidate(1, 150., 150.);
    b.rect = RectF::from_points(
        pathfinder_geometry::vector::vec2f(f32::NAN, 0.),
        pathfinder_geometry::vector::vec2f(f32::NAN + 150., 34.),
    );
    let cursor = pathfinder_geometry::vector::vec2f(225., 17.);
    let result = select_drag_split_target(&[a, b], 0, cursor, false);
    assert_eq!(result, None, "candidate with NaN rect must be skipped");
}
