use std::path::PathBuf;
use std::sync::Arc;

use ai::workspace::WorkspaceMetadata;
use chrono::Utc;
use cloud_object_persistence::to_cloud_object_permissions;
use diesel::connection::SimpleConnection;
use galaxy_agent_core::{ConversationMessage, MessageContent, MessageRole};
use galaxy_core::features::FeatureFlag;
use galaxy_graphql::scalars::time::ServerTimestamp;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::Vector2F;

use super::{
    app_database_file_path, database_file_path_for_scope, decode_path, deduplicate_events,
    encode_path, get_all_codebase_index_metadata, read_sqlite_data, save_app_state,
    save_codebase_index_metadata, setup_database, start_writer, GALAXY_SQLITE_FILE_NAME,
};
use crate::ai::agent::conversation::{AIConversation, AIConversationId};
use crate::ai::facts::{AIFact, AIMemory, CloudAIFact};
use crate::app_state::{
    AppState, CodePaneSnapShot, CodePaneTabSnapshot, LeafContents, LeafSnapshot, PaneNodeSnapshot,
    TabGroupSnapshot, TabSnapshot, TerminalPaneSnapshot, WindowSnapshot,
};
use crate::cloud_object::{CloudObjectPermissions, Owner};
use crate::code::editor_management::CodeSource;
use crate::notebooks::{CloudNotebook, CloudNotebookModel};
use crate::persistence::agent::upsert_agent_conversation;
use crate::persistence::model::{
    AgentConversationData, ObjectPermissions, ProviderConversationContext,
};
use crate::persistence::{BlockCompleted, ModelEvent, PersistenceScope};
use crate::server::ids::{ClientId, SyncId};
use crate::tab::SelectedTabColor;
use crate::terminal::model::block::SerializedBlock;
use crate::terminal::ShellLaunchData;
use crate::themes::theme::AnsiColorIdentifier;
use crate::workflows::workflow::Workflow;
use crate::workflows::CloudWorkflow;
use crate::workspace::tab_group::TabGroupId;

#[test]
fn app_scope_database_path_matches_app_database_path() {
    assert_eq!(
        database_file_path_for_scope(&PersistenceScope::App),
        app_database_file_path()
    );
    assert_eq!(
        app_database_file_path()
            .file_name()
            .and_then(|name| name.to_str()),
        Some(GALAXY_SQLITE_FILE_NAME)
    );
}

#[test]
fn legacy_database_migration_moves_database_and_sidecars() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let legacy_path = tempdir.path().join("warp.sqlite");
    let galaxy_path = tempdir.path().join(GALAXY_SQLITE_FILE_NAME);
    std::fs::write(&legacy_path, b"database").expect("legacy database should be created");
    std::fs::write(legacy_path.with_extension("sqlite-wal"), b"wal")
        .expect("legacy WAL should be created");
    std::fs::write(legacy_path.with_extension("sqlite-shm"), b"shm")
        .expect("legacy SHM should be created");

    super::migrate_sqlite_database(&legacy_path, &galaxy_path)
        .expect("legacy database should migrate");

    assert_eq!(std::fs::read(&galaxy_path).unwrap(), b"database");
    assert_eq!(
        std::fs::read(galaxy_path.with_extension("sqlite-wal")).unwrap(),
        b"wal"
    );
    assert_eq!(
        std::fs::read(galaxy_path.with_extension("sqlite-shm")).unwrap(),
        b"shm"
    );
    assert!(!legacy_path.exists());
}

#[test]
fn remote_server_daemon_scope_database_path_uses_identity_data_dir() {
    let path = database_file_path_for_scope(&PersistenceScope::RemoteServerDaemon {
        identity_key: "user@example.com/ssh host".to_string(),
    });
    let expected_data_dir =
        remote_server::setup::remote_server_daemon_data_dir("user@example.com/ssh host");

    assert!(path.is_absolute());
    assert_eq!(
        path,
        PathBuf::from(shellexpand::tilde(&expected_data_dir).into_owned()).join("warp.sqlite")
    );
}

#[test]
fn remote_server_daemon_scope_database_path_handles_empty_identity_key() {
    let path = database_file_path_for_scope(&PersistenceScope::RemoteServerDaemon {
        identity_key: String::new(),
    });
    let expected_data_dir = remote_server::setup::remote_server_daemon_data_dir("");

    assert_eq!(
        path,
        PathBuf::from(shellexpand::tilde(&expected_data_dir).into_owned()).join("warp.sqlite")
    );
}

#[cfg(unix)]
#[test]
fn remote_server_daemon_database_permissions_are_owner_only() {
    use std::fs::Permissions;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let daemon_dir = tempdir.path().join("daemon");
    let database_path = daemon_dir.join("warp.sqlite");

    std::fs::create_dir_all(&daemon_dir).expect("daemon dir should be created");
    std::fs::set_permissions(&daemon_dir, Permissions::from_mode(0o755))
        .expect("daemon dir permissions should be set");
    std::fs::write(&database_path, b"").expect("database file should be created");
    std::fs::set_permissions(&database_path, Permissions::from_mode(0o644))
        .expect("database file permissions should be set");

    super::ensure_owner_only_dir(&daemon_dir).expect("daemon dir should be owner-only");
    super::ensure_owner_only_file(&database_path).expect("database file should be owner-only");

    assert_eq!(daemon_dir.metadata().unwrap().mode() & 0o777, 0o700);
    assert_eq!(database_path.metadata().unwrap().mode() & 0o777, 0o600);
}

fn test_codebase_metadata(path: &str) -> WorkspaceMetadata {
    WorkspaceMetadata {
        path: PathBuf::from(path),
        navigated_ts: Some(Utc::now()),
        modified_ts: None,
        queried_ts: None,
    }
}

#[test]
fn sqlite_read_restores_app_state_and_codebase_metadata() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };
    save_app_state(&mut conn, &app_state).expect("app state should save");

    let metadata = test_codebase_metadata("/tmp/remote-repo");
    save_codebase_index_metadata(&mut conn, metadata.clone())
        .expect("codebase index metadata should save");
    let restored = read_sqlite_data(&mut conn, None).expect("persisted data should load");
    assert_eq!(restored.app_state.windows.len(), 1);
    assert_eq!(restored.codebase_indices.len(), 1);
    assert_eq!(restored.codebase_indices[0].path, metadata.path);
}

#[test]
fn sqlite_writer_reuses_codebase_index_metadata_events() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let conn = setup_database(&database_path).expect("database should initialize");

    let writer = start_writer(conn, database_path.clone()).expect("writer should start");
    let metadata = test_codebase_metadata("/tmp/writer-repo");
    writer
        .sender
        .send(ModelEvent::UpsertCodebaseIndexMetadata {
            index_metadata: Box::new(metadata.clone()),
        })
        .expect("upsert event should send");
    writer
        .sender
        .send(ModelEvent::Terminate)
        .expect("terminate event should send");
    writer.handle.join().expect("writer should terminate");

    let mut conn = setup_database(&database_path).expect("database should reopen");
    let restored = get_all_codebase_index_metadata(&mut conn).expect("metadata should load");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].path, metadata.path);

    let writer = start_writer(conn, database_path.clone()).expect("writer should restart");
    writer
        .sender
        .send(ModelEvent::DeleteCodebaseIndexMetadata {
            repo_path: metadata.path,
        })
        .expect("delete event should send");
    writer
        .sender
        .send(ModelEvent::Terminate)
        .expect("terminate event should send");
    writer.handle.join().expect("writer should terminate");

    let mut conn = setup_database(&database_path).expect("database should reopen");
    let restored = get_all_codebase_index_metadata(&mut conn).expect("metadata should load");
    assert!(restored.is_empty());
}

#[test]
fn sqlite_writer_restores_and_deletes_local_rules() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let conn = setup_database(&database_path).expect("database should initialize");
    let id = SyncId::ClientId(ClientId::new());
    let fact = AIFact::Memory(AIMemory {
        name: Some("Rust".to_string()),
        content: "Never unwrap".to_string(),
        is_autogenerated: false,
        suggested_logging_id: None,
    });
    let rule = crate::local_object_repository::new_local_rule(id, fact.clone());

    let writer = start_writer(conn, database_path.clone()).expect("writer should start");
    writer
        .sender
        .send(ModelEvent::UpsertGenericStringObject {
            object: Box::new(rule),
        })
        .expect("rule upsert should send");
    writer
        .sender
        .send(ModelEvent::Terminate)
        .expect("terminate event should send");
    writer.handle.join().expect("writer should terminate");

    let mut conn = setup_database(&database_path).expect("database should reopen");
    let restored = read_sqlite_data(&mut conn, None).expect("persisted data should load");
    let restored_rule = restored
        .cloud_objects
        .iter()
        .find_map(|object| {
            let rule: Option<&CloudAIFact> = object.into();
            rule
        })
        .expect("local rule should be restored");
    assert_eq!(restored_rule.id, id);
    assert_eq!(restored_rule.model().string_model, fact);
    assert!(!restored_rule.metadata.has_pending_content_changes());

    let writer = start_writer(conn, database_path.clone()).expect("writer should restart");
    writer
        .sender
        .send(ModelEvent::DeleteObjects {
            ids: vec![(id, crate::cloud_object::ObjectIdType::GenericStringObject)],
        })
        .expect("rule deletion should send");
    writer
        .sender
        .send(ModelEvent::Terminate)
        .expect("terminate event should send");
    writer.handle.join().expect("writer should terminate");

    let mut conn = setup_database(&database_path).expect("database should reopen");
    let restored = read_sqlite_data(&mut conn, None).expect("persisted data should load");
    assert!(restored.cloud_objects.iter().all(|object| {
        let rule: Option<&CloudAIFact> = object.into();
        rule.is_none()
    }));
}

#[test]
fn sqlite_writer_restores_and_deletes_local_notebooks_and_workflows() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let conn = setup_database(&database_path).expect("database should initialize");
    let notebook_id = SyncId::ClientId(ClientId::new());
    let workflow_id = SyncId::ClientId(ClientId::new());
    let notebook = crate::local_object_repository::new_local_notebook(
        notebook_id,
        None,
        CloudNotebookModel {
            title: "Local notebook".to_string(),
            data: "echo local".to_string(),
            ai_document_id: None,
            conversation_id: None,
        },
    );
    let workflow = crate::local_object_repository::new_local_workflow(
        workflow_id,
        None,
        Workflow::new("Local workflow", "cargo test"),
    );

    let writer = start_writer(conn, database_path.clone()).expect("writer should start");
    writer
        .sender
        .send(ModelEvent::UpsertNotebook { notebook })
        .expect("notebook upsert should send");
    writer
        .sender
        .send(ModelEvent::UpsertWorkflow { workflow })
        .expect("workflow upsert should send");
    writer
        .sender
        .send(ModelEvent::Terminate)
        .expect("terminate event should send");
    writer.handle.join().expect("writer should terminate");

    let mut conn = setup_database(&database_path).expect("database should reopen");
    let restored = read_sqlite_data(&mut conn, None).expect("persisted data should load");
    let restored_notebook = restored
        .cloud_objects
        .iter()
        .find_map(|object| {
            let notebook: Option<&CloudNotebook> = object.into();
            notebook
        })
        .expect("local notebook should be restored");
    let restored_workflow = restored
        .cloud_objects
        .iter()
        .find_map(|object| {
            let workflow: Option<&CloudWorkflow> = object.into();
            workflow
        })
        .expect("local workflow should be restored");
    assert_eq!(restored_notebook.id, notebook_id);
    assert_eq!(restored_notebook.model().title, "Local notebook");
    assert!(!restored_notebook.metadata.has_pending_content_changes());
    assert_eq!(restored_workflow.id, workflow_id);
    assert_eq!(restored_workflow.model().data.name(), "Local workflow");
    assert!(!restored_workflow.metadata.has_pending_content_changes());

    let writer = start_writer(conn, database_path.clone()).expect("writer should restart");
    writer
        .sender
        .send(ModelEvent::DeleteObjects {
            ids: vec![
                (notebook_id, crate::cloud_object::ObjectIdType::Notebook),
                (workflow_id, crate::cloud_object::ObjectIdType::Workflow),
            ],
        })
        .expect("local object deletion should send");
    writer
        .sender
        .send(ModelEvent::Terminate)
        .expect("terminate event should send");
    writer.handle.join().expect("writer should terminate");

    let mut conn = setup_database(&database_path).expect("database should reopen");
    let restored = read_sqlite_data(&mut conn, None).expect("persisted data should load");
    assert!(restored.cloud_objects.iter().all(|object| {
        let notebook: Option<&CloudNotebook> = object.into();
        let workflow: Option<&CloudWorkflow> = object.into();
        notebook.is_none() && workflow.is_none()
    }));
}

#[test]
fn test_deduplicate_snapshots() {
    let local_notebook = CloudNotebook::new_local(
        CloudNotebookModel {
            title: "Hello".to_string(),
            data: "World".to_string(),
            ai_document_id: None,
            conversation_id: None,
        },
        Owner::mock_current_user(),
        None,
        ClientId::new(),
    );
    let completed_block_1 = BlockCompleted {
        pane_id: vec![1, 2, 3],
        block: Arc::new(SerializedBlock::default()),
        is_local: true,
    };
    let completed_block_2 = BlockCompleted {
        pane_id: vec![4, 5, 6],
        block: Arc::new(SerializedBlock::default()),
        is_local: true,
    };
    let snapshot_1 = AppState {
        active_window_index: Some(1),
        block_lists: Default::default(),
        windows: Default::default(),
        running_mcp_servers: Default::default(),
    };
    let snapshot_2 = AppState {
        active_window_index: Some(2),
        block_lists: Default::default(),
        windows: Default::default(),
        running_mcp_servers: Default::default(),
    };
    let snapshot_3 = AppState {
        active_window_index: Some(3),
        block_lists: Default::default(),
        windows: Default::default(),
        running_mcp_servers: Default::default(),
    };

    let original_events = vec![
        ModelEvent::UpsertNotebook {
            notebook: local_notebook.clone(),
        },
        ModelEvent::Snapshot(snapshot_1.clone()),
        ModelEvent::SaveBlock(completed_block_1.clone()),
        ModelEvent::Snapshot(snapshot_2.clone()),
        ModelEvent::SaveBlock(completed_block_2.clone()),
        ModelEvent::Snapshot(snapshot_3.clone()),
        ModelEvent::UpsertNotebook {
            notebook: local_notebook.clone(),
        },
    ];

    let filtered_events = deduplicate_events(original_events);
    assert_eq!(filtered_events.len(), 5);

    assert!(matches!(
        &filtered_events[0],
        &ModelEvent::UpsertNotebook { .. }
    ));
    // The first snapshot should have been filtered out.
    assert!(matches!(&filtered_events[1], &ModelEvent::SaveBlock(_)));
    // The second snapshot should have been filtered out.
    assert!(matches!(&filtered_events[2], &ModelEvent::SaveBlock(_)));
    // The third snapshot should be preserved.
    match &filtered_events[3] {
        ModelEvent::Snapshot(snapshot) => assert_eq!(snapshot, &snapshot_3),
        other => panic!("Expected ModelEvent::Snapshot, got {other:?}"),
    }
    assert!(matches!(
        &filtered_events[4],
        &ModelEvent::UpsertNotebook { .. }
    ));
}

#[test]
fn test_deduplicate_no_snapshots() {
    let original_events = vec![ModelEvent::SaveBlock(BlockCompleted {
        pane_id: vec![1, 2, 3],
        block: Default::default(),
        is_local: true,
    })];
    let filtered_events = deduplicate_events(original_events);
    assert_eq!(filtered_events.len(), 1);
    assert!(matches!(&filtered_events[0], &ModelEvent::SaveBlock(_)));
}

fn test_terminal_window_snapshot(vertical_tabs_panel_open: bool) -> WindowSnapshot {
    WindowSnapshot {
        tabs: vec![TabSnapshot {
            custom_title: None,
            root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                is_focused: true,
                custom_vertical_tabs_title: None,
                contents: LeafContents::Terminal(TerminalPaneSnapshot {
                    uuid: vec![u8::from(vertical_tabs_panel_open) + 1],
                    cwd: Some("/tmp".to_string()),
                    shell_launch_data: Some(ShellLaunchData::Executable {
                        executable_path: PathBuf::from("/bin/zsh"),
                        shell_type: crate::terminal::shell::ShellType::Zsh,
                    }),
                    is_active: true,
                    is_read_only: false,
                    input_config: None,
                    llm_model_override: None,
                    active_profile_id: None,
                    conversation_ids_to_restore: vec![],
                    active_conversation_id: None,
                }),
            }),
            default_directory_color: None,
            selected_color: SelectedTabColor::default(),
            left_panel: None,
            right_panel: None,
            group_id: None,
            pinned: false,
        }],
        active_tab_index: 0,
        bounds: None,
        fullscreen_state: Default::default(),
        quake_mode: false,
        universal_search_width: None,
        warp_ai_width: None,
        voltron_width: None,
        warp_drive_index_width: None,
        left_panel_open: false,
        vertical_tabs_panel_open,
        left_panel_width: None,
        right_panel_width: None,
        agent_management_filters: None,
        tab_groups: vec![],
        split_groups: vec![],
    }
}

#[test]
fn test_sqlite_round_trips_vertical_tabs_panel_open() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![
            test_terminal_window_snapshot(false),
            test_terminal_window_snapshot(true),
        ],
        active_window_index: Some(1),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.active_window_index, Some(1));
    assert_eq!(
        restored
            .windows
            .iter()
            .map(|window| window.vertical_tabs_panel_open)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn test_sqlite_round_trips_custom_vertical_tabs_title() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![TabSnapshot {
                custom_title: None,
                root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: true,
                    custom_vertical_tabs_title: Some("Production API".to_string()),
                    contents: LeafContents::Terminal(TerminalPaneSnapshot {
                        uuid: vec![42],
                        cwd: Some("/tmp".to_string()),
                        shell_launch_data: Some(ShellLaunchData::Executable {
                            executable_path: PathBuf::from("/bin/zsh"),
                            shell_type: crate::terminal::shell::ShellType::Zsh,
                        }),
                        is_active: true,
                        is_read_only: false,
                        input_config: None,
                        llm_model_override: None,
                        active_profile_id: None,
                        conversation_ids_to_restore: vec![],
                        active_conversation_id: None,
                    }),
                }),
                default_directory_color: None,
                selected_color: SelectedTabColor::default(),
                left_panel: None,
                right_panel: None,
                group_id: None,
                pinned: false,
            }],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![],
            split_groups: vec![],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    let PaneNodeSnapshot::Leaf(LeafSnapshot {
        custom_vertical_tabs_title,
        ..
    }) = &restored.windows[0].tabs[0].root
    else {
        panic!("Expected terminal pane leaf");
    };
    assert_eq!(
        custom_vertical_tabs_title.as_deref(),
        Some("Production API")
    );
}

#[test]
fn test_sqlite_round_trips_code_pane_with_multiple_tabs() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![TabSnapshot {
                custom_title: None,
                root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: true,
                    custom_vertical_tabs_title: None,
                    contents: LeafContents::Code(CodePaneSnapShot::Local {
                        tabs: vec![
                            CodePaneTabSnapshot {
                                path: Some(PathBuf::from("/tmp/main.rs")),
                            },
                            CodePaneTabSnapshot {
                                path: Some(PathBuf::from("/tmp/lib.rs")),
                            },
                            CodePaneTabSnapshot { path: None },
                        ],
                        active_tab_index: 1,
                        source: Some(CodeSource::FileTree {
                            location: crate::code::buffer_location::LocalOrRemotePath::Local(
                                PathBuf::from("/tmp/main.rs"),
                            ),
                        }),
                    }),
                }),
                default_directory_color: None,
                selected_color: SelectedTabColor::default(),
                left_panel: None,
                right_panel: None,
                group_id: None,
                pinned: false,
            }],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![],
            split_groups: vec![],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    let restored_tab = &restored.windows[0].tabs[0];
    let PaneNodeSnapshot::Leaf(LeafSnapshot {
        contents:
            LeafContents::Code(CodePaneSnapShot::Local {
                tabs,
                active_tab_index,
                source,
            }),
        ..
    }) = &restored_tab.root
    else {
        panic!("Expected code pane leaf");
    };

    assert_eq!(tabs.len(), 3);
    assert_eq!(*active_tab_index, 1);
    assert_eq!(tabs[0].path, Some(PathBuf::from("/tmp/main.rs")));
    assert_eq!(tabs[1].path, Some(PathBuf::from("/tmp/lib.rs")));
    assert_eq!(tabs[2].path, None);
    assert!(matches!(source, Some(CodeSource::FileTree { .. })));
}

/// Verifies that a tab group and its membership round-trip through save/restore.
#[test]
fn test_sqlite_round_trips_tab_groups() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let group_id = TabGroupId::new();
    let tab_in_group = TabSnapshot {
        custom_title: None,
        root: PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: true,
            custom_vertical_tabs_title: None,
            contents: LeafContents::Terminal(TerminalPaneSnapshot {
                uuid: vec![1],
                cwd: Some("/tmp/grouped".to_string()),
                shell_launch_data: Some(ShellLaunchData::Executable {
                    executable_path: PathBuf::from("/bin/zsh"),
                    shell_type: crate::terminal::shell::ShellType::Zsh,
                }),
                is_active: true,
                is_read_only: false,
                input_config: None,
                llm_model_override: None,
                active_profile_id: None,
                conversation_ids_to_restore: vec![],
                active_conversation_id: None,
            }),
        }),
        default_directory_color: None,
        selected_color: SelectedTabColor::default(),
        left_panel: None,
        right_panel: None,
        group_id: Some(group_id),
        pinned: false,
    };
    let tab_outside_group = TabSnapshot {
        custom_title: None,
        root: PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: false,
            custom_vertical_tabs_title: None,
            contents: LeafContents::Terminal(TerminalPaneSnapshot {
                uuid: vec![2],
                cwd: Some("/tmp/ungrouped".to_string()),
                shell_launch_data: Some(ShellLaunchData::Executable {
                    executable_path: PathBuf::from("/bin/zsh"),
                    shell_type: crate::terminal::shell::ShellType::Zsh,
                }),
                is_active: false,
                is_read_only: false,
                input_config: None,
                llm_model_override: None,
                active_profile_id: None,
                conversation_ids_to_restore: vec![],
                active_conversation_id: None,
            }),
        }),
        default_directory_color: None,
        selected_color: SelectedTabColor::default(),
        left_panel: None,
        right_panel: None,
        group_id: None,
        pinned: false,
    };

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![tab_in_group, tab_outside_group],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![TabGroupSnapshot {
                id: group_id,
                name: Some("Backend".to_string()),
                color: SelectedTabColor::Color(AnsiColorIdentifier::Blue),
                collapsed: true,
                pinned: false,
            }],
            split_groups: vec![],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    let restored_window = &restored.windows[0];
    assert_eq!(restored_window.tab_groups.len(), 1);
    let restored_group = &restored_window.tab_groups[0];
    assert_eq!(restored_group.name.as_deref(), Some("Backend"));
    assert_eq!(
        restored_group.color,
        SelectedTabColor::Color(AnsiColorIdentifier::Blue)
    );
    assert!(restored_group.collapsed);

    // The in-memory `TabGroupId` is minted fresh on restore, so we check that
    // the grouped tab points at the restored group, and the ungrouped tab
    // remains ungrouped.
    assert_eq!(restored_window.tabs.len(), 2);
    assert_eq!(restored_window.tabs[0].group_id, Some(restored_group.id));
    assert_eq!(restored_window.tabs[1].group_id, None);
}

/// Verifies that the `pinned` flag on tabs and tab groups round-trips through
/// save/restore so the user's pinned layout survives an app restart.
#[test]
fn test_sqlite_round_trips_pinned_state() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let pinned_group_id = TabGroupId::new();
    let unpinned_group_id = TabGroupId::new();

    let pinned_tab = TabSnapshot {
        custom_title: None,
        root: PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: true,
            custom_vertical_tabs_title: None,
            contents: LeafContents::Terminal(TerminalPaneSnapshot {
                uuid: vec![10],
                cwd: Some("/tmp/pinned".to_string()),
                shell_launch_data: Some(ShellLaunchData::Executable {
                    executable_path: PathBuf::from("/bin/zsh"),
                    shell_type: crate::terminal::shell::ShellType::Zsh,
                }),
                is_active: true,
                is_read_only: false,
                input_config: None,
                llm_model_override: None,
                active_profile_id: None,
                conversation_ids_to_restore: vec![],
                active_conversation_id: None,
            }),
        }),
        default_directory_color: None,
        selected_color: SelectedTabColor::default(),
        left_panel: None,
        right_panel: None,
        group_id: None,
        pinned: true,
    };
    let unpinned_tab = TabSnapshot {
        custom_title: None,
        root: PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: false,
            custom_vertical_tabs_title: None,
            contents: LeafContents::Terminal(TerminalPaneSnapshot {
                uuid: vec![11],
                cwd: Some("/tmp/unpinned".to_string()),
                shell_launch_data: Some(ShellLaunchData::Executable {
                    executable_path: PathBuf::from("/bin/zsh"),
                    shell_type: crate::terminal::shell::ShellType::Zsh,
                }),
                is_active: false,
                is_read_only: false,
                input_config: None,
                llm_model_override: None,
                active_profile_id: None,
                conversation_ids_to_restore: vec![],
                active_conversation_id: None,
            }),
        }),
        default_directory_color: None,
        selected_color: SelectedTabColor::default(),
        left_panel: None,
        right_panel: None,
        group_id: Some(unpinned_group_id),
        pinned: false,
    };
    let tab_in_pinned_group = TabSnapshot {
        custom_title: None,
        root: PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: false,
            custom_vertical_tabs_title: None,
            contents: LeafContents::Terminal(TerminalPaneSnapshot {
                uuid: vec![12],
                cwd: Some("/tmp/pinned-group".to_string()),
                shell_launch_data: Some(ShellLaunchData::Executable {
                    executable_path: PathBuf::from("/bin/zsh"),
                    shell_type: crate::terminal::shell::ShellType::Zsh,
                }),
                is_active: false,
                is_read_only: false,
                input_config: None,
                llm_model_override: None,
                active_profile_id: None,
                conversation_ids_to_restore: vec![],
                active_conversation_id: None,
            }),
        }),
        default_directory_color: None,
        selected_color: SelectedTabColor::default(),
        left_panel: None,
        right_panel: None,
        group_id: Some(pinned_group_id),
        pinned: false,
    };

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![pinned_tab, tab_in_pinned_group, unpinned_tab],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![
                TabGroupSnapshot {
                    id: pinned_group_id,
                    name: Some("Pinned".to_string()),
                    color: SelectedTabColor::default(),
                    collapsed: false,
                    pinned: true,
                },
                TabGroupSnapshot {
                    id: unpinned_group_id,
                    name: Some("Loose".to_string()),
                    color: SelectedTabColor::default(),
                    collapsed: false,
                    pinned: false,
                },
            ],
            split_groups: vec![],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    let restored_window = &restored.windows[0];

    // Tabs come back in insertion order; pinned flag should match what we saved.
    assert_eq!(restored_window.tabs.len(), 3);
    assert!(restored_window.tabs[0].pinned);
    assert!(!restored_window.tabs[1].pinned);
    assert!(!restored_window.tabs[2].pinned);

    // Both groups round-trip with their pinned state preserved. Group ids are
    // minted fresh on restore, so we look them up by name.
    assert_eq!(restored_window.tab_groups.len(), 2);
    let restored_pinned_group = restored_window
        .tab_groups
        .iter()
        .find(|group| group.name.as_deref() == Some("Pinned"))
        .expect("pinned group should restore");
    let restored_loose_group = restored_window
        .tab_groups
        .iter()
        .find(|group| group.name.as_deref() == Some("Loose"))
        .expect("unpinned group should restore");
    assert!(restored_pinned_group.pinned);
    assert!(!restored_loose_group.pinned);
}

fn assert_encode_then_decode_preserves_original_path(original_path: PathBuf) {
    let bytes = encode_path(original_path.clone());
    let decoded_path = decode_path(bytes);
    assert_eq!(original_path, decoded_path);
}

/// Test that a local path can be encoded and decoded. We use this when persisting a local
/// file path for notebooks in sqlite. We need this test because Windows `OsString`s are
/// often arbitrary sequences of 16-bit values, unlike Unix which uses sequences of 8-bit
/// values (bytes). Since `diesel::sql_types::Binary` deals with sequences of bytes (`u8`)
/// we need to perform special casting on `OsString`s on Windows.
#[test]
fn test_path_encode_decode() {
    // Empty path
    assert_encode_then_decode_preserves_original_path(PathBuf::new());

    // Windows-style paths
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"C:\windows\system32.dll"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("c:temp"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\emoji\🙈.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\ñoñàscii\temp.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\hindi\हिन्दी"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\cjk\狗没有耐心"));

    // Unix-style paths
    assert_encode_then_decode_preserves_original_path(PathBuf::from(
        "/home/persistence/example.sql",
    ));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("./database/log.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/emoji/🙈.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/ñoñàscii/temp.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/hindi/हिन्दी"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/cjk/狗没有耐心"));
}

#[test]
fn test_deserialize_corrupted_guests() {
    let _ = FeatureFlag::SharedWithMe.override_enabled(true);
    // Use a hardcoded timestamp to ensure this test works on systems with more-than-microsecond
    // precision.
    let permissions_ts_micros = 123456;
    let permissions_ts =
        ServerTimestamp::from_unix_timestamp_micros(permissions_ts_micros).unwrap();

    let db_permissions = ObjectPermissions {
        id: 42,
        object_metadata_id: 10,
        subject_type: "TEAM".to_string(),
        subject_id: Some("7".to_string()),
        subject_uid: "team_uid12345678912345".to_string(),
        permissions_last_updated_at: Some(permissions_ts_micros),
        // This is not a valid set of encoded object guests.
        object_guests: Some(vec![1, 2, 3]),
        anyone_with_link_access_level: None,
        anyone_with_link_source: None,
    };

    // The overall permissions should successfully convert, minus the object guests.
    let cloud_permissions = to_cloud_object_permissions(&db_permissions, None);
    assert_eq!(
        cloud_permissions,
        Some(CloudObjectPermissions {
            owner: Owner::Team {
                team_uid: crate::server::ids::ServerId::from_string_lossy("team_uid12345678912345"),
            },
            permissions_last_updated_ts: Some(permissions_ts),
            anyone_with_link: None,
            guests: vec![],
        })
    );
}

// Regression: GH#10083. The macOS green-tile button could leave a 1px-wide
// window bound in `AppContext::window_bounds`, which previously round-tripped
// through SQLite and restored as an unusable 1px sliver. Bounds below the
// platform minimum window size must be dropped on save.
#[test]
fn test_sqlite_drops_too_small_bounds_on_save() {
    use diesel::prelude::*;

    use crate::persistence::schema::windows;

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let mut snapshot = test_terminal_window_snapshot(false);
    snapshot.bounds = Some(RectF::new(
        Vector2F::new(0.0, -1410.0),
        Vector2F::new(1.0, 1410.0),
    ));

    let app_state = AppState {
        windows: vec![snapshot],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    // Query the row directly so the assertion isolates the save guard and is
    // not masked by the read-side guard in `read_sqlite_data`.
    let row: (Option<f32>, Option<f32>, Option<f32>, Option<f32>) = windows::dsl::windows
        .select((
            windows::columns::window_width,
            windows::columns::window_height,
            windows::columns::origin_x,
            windows::columns::origin_y,
        ))
        .first(&mut conn)
        .expect("a windows row should have been inserted");

    assert_eq!(
        row,
        (None, None, None, None),
        "save-path guard must persist NULL bound columns for sub-minimum geometry"
    );
}

// Regression: GH#10083. Users whose warp.sqlite already contains a 1px row
// (because they hit the bug on an earlier build) must still recover to default
// geometry on next launch rather than restoring the sliver.
#[test]
fn test_sqlite_drops_too_small_bounds_on_read() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    // Save with no bounds so a row exists, then corrupt it directly to bypass
    // the save-path guard and simulate a pre-existing bad row.
    let app_state = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };
    save_app_state(&mut conn, &app_state).expect("app state should save");

    conn.batch_execute(
        "UPDATE windows \
         SET window_width = 1.0, window_height = 1410.0, \
             origin_x = 0.0, origin_y = -1410.0",
    )
    .expect("corrupting update should succeed");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    assert!(
        restored.windows[0].bounds.is_none(),
        "tiny persisted bounds must be discarded on read so users recover from a corrupt DB"
    );
}

#[test]
fn failed_snapshot_is_not_cached_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = setup_database(&dir.path().join("galaxy.sqlite")).unwrap();
    let snapshot = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Vec::new(),
    };
    conn.batch_execute("CREATE TRIGGER reject_window BEFORE INSERT ON windows BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
    let mut last_saved = None;
    assert!(super::save_snapshot_if_changed(&mut conn, snapshot.clone(), &mut last_saved).is_err());
    assert!(last_saved.is_none());
    conn.batch_execute("DROP TRIGGER reject_window;").unwrap();
    super::save_snapshot_if_changed(&mut conn, snapshot.clone(), &mut last_saved).unwrap();
    assert_eq!(last_saved, Some(snapshot));
    assert_eq!(
        read_sqlite_data(&mut conn, None)
            .unwrap()
            .app_state
            .windows
            .len(),
        1
    );
}

#[test]
fn sqlite_busy_writes_retry_but_permanent_failures_do_not() {
    let mut attempts = 0;
    super::retry_sqlite_write(|| {
        attempts += 1;
        if attempts < 3 {
            return Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::Unknown,
                Box::new("database is locked".to_owned()),
            )
            .into());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(attempts, 3);
    attempts = 0;
    assert!(super::retry_sqlite_write(|| -> anyhow::Result<()> {
        attempts += 1;
        anyhow::bail!("permanent failure")
    })
    .is_err());
    assert_eq!(attempts, 1);
}

#[test]
fn saved_profile_editors_do_not_mark_session_restore_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = setup_database(&dir.path().join("galaxy.sqlite")).unwrap();
    let mut window = test_terminal_window_snapshot(false);
    for contents in [
        LeafContents::ExecutionProfileEditor,
        LeafContents::CustomRouterEditor,
    ] {
        let mut editor_tab = window.tabs[0].clone();
        editor_tab.root = PaneNodeSnapshot::Leaf(LeafSnapshot {
            is_focused: true,
            custom_vertical_tabs_title: None,
            contents,
        });
        window.tabs.push(editor_tab);
    }
    let snapshot = AppState {
        windows: vec![window],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Vec::new(),
    };
    save_app_state(&mut conn, &snapshot).unwrap();
    let (restored, errors) = super::read_sqlite_data_with_recovery(&mut conn, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(restored.app_state.windows[0].tabs.len(), 3);
    assert!(matches!(
        &restored.app_state.windows[0].tabs[0].root,
        PaneNodeSnapshot::Leaf(LeafSnapshot {
            contents: LeafContents::Terminal(_),
            ..
        })
    ));
    for tab in &restored.app_state.windows[0].tabs[1..] {
        assert!(matches!(
            &tab.root,
            PaneNodeSnapshot::Leaf(LeafSnapshot {
                contents: LeafContents::ExecutionProfileEditor,
                ..
            })
        ));
    }
}

#[test]
fn incomplete_restore_keeps_independent_domains_and_reports_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = setup_database(&dir.path().join("galaxy.sqlite")).unwrap();
    let snapshot = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Vec::new(),
    };
    save_app_state(&mut conn, &snapshot).unwrap();
    conn.batch_execute("DROP TABLE ignored_suggestions;")
        .unwrap();
    let (restored, errors) = super::read_sqlite_data_with_recovery(&mut conn, None);
    assert_eq!(restored.app_state.windows.len(), 1);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("ignored_suggestions"));
}

#[test]
fn corrupt_tab_is_reported_instead_of_silently_replacing_saved_layout() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = setup_database(&dir.path().join("galaxy.sqlite")).unwrap();
    let snapshot = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Vec::new(),
    };
    save_app_state(&mut conn, &snapshot).unwrap();
    conn.batch_execute("DELETE FROM terminal_panes;").unwrap();
    let (_, errors) = super::read_sqlite_data_with_recovery(&mut conn, None);
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|error| error.contains("session layout")));
    // Reading alone must leave the recoverable tab record in place.
    conn.batch_execute("CREATE TRIGGER require_saved_tab BEFORE DELETE ON tabs BEGIN SELECT RAISE(ABORT, 'must retain tab'); END;").unwrap();
    assert!(save_app_state(&mut conn, &snapshot).is_err());
}

#[test]
fn provider_context_round_trips_through_sqlite_without_terminal_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = setup_database(&dir.path().join("galaxy.sqlite")).unwrap();
    let id = AIConversationId::new();
    let context = ProviderConversationContext {
        messages: vec![ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("Most recent response".into()),
        }],
        tool_result_archive: vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "old-call".into(),
                content: "archived output".into(),
                is_error: false,
            },
        }],
    };
    upsert_agent_conversation(
        &mut conn,
        &id.to_string(),
        &[],
        AgentConversationData {
            provider_context: Some(context.clone()),
            progressive_summary: Some("Earlier conversation".into()),
            ..Default::default()
        },
    )
    .unwrap();
    conn.batch_execute("INSERT INTO agent_conversations (conversation_id, conversation_data) VALUES ('corrupt', 'not-json');").unwrap();
    let (healthy, errors) = super::read_sqlite_data_with_recovery(&mut conn, None);
    assert_eq!(healthy.multi_agent_conversations.len(), 1);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("corrupt"));
    conn.batch_execute("DELETE FROM agent_conversations WHERE conversation_id = 'corrupt';")
        .unwrap();
    let mut data = read_sqlite_data(&mut conn, None).unwrap();
    assert!(data.app_state.block_lists.is_empty());
    let saved = data.multi_agent_conversations.pop().unwrap();
    let restored = AIConversation::new_restored_synthesizing_on_empty(
        id,
        saved.tasks,
        Some(serde_json::from_str(&saved.conversation.conversation_data).unwrap()),
    )
    .unwrap();
    assert_eq!(restored.provider_context(), context);
    assert_eq!(restored.progressive_summary(), Some("Earlier conversation"));
}

// ===========================================================================
// Split group persistence tests
// ===========================================================================

/// A `WindowSplitGroupSnapshot` round-trips through save/restore.
#[test]
fn test_sqlite_round_trips_split_groups() {
    use crate::app_state::WindowSplitGroupSnapshot;

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![
                TabSnapshot {
                    custom_title: Some("Tab A".to_string()),
                    root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                        is_focused: true,
                        custom_vertical_tabs_title: None,
                        contents: LeafContents::Terminal(TerminalPaneSnapshot {
                            uuid: vec![1],
                            cwd: Some("/tmp".to_string()),
                            shell_launch_data: Some(ShellLaunchData::Executable {
                                executable_path: PathBuf::from("/bin/zsh"),
                                shell_type: crate::terminal::shell::ShellType::Zsh,
                            }),
                            is_active: true,
                            is_read_only: false,
                            input_config: None,
                            llm_model_override: None,
                            active_profile_id: None,
                            conversation_ids_to_restore: vec![],
                            active_conversation_id: None,
                        }),
                    }),
                    default_directory_color: None,
                    selected_color: SelectedTabColor::default(),
                    left_panel: None,
                    right_panel: None,
                    group_id: None,
                    pinned: false,
                },
                TabSnapshot {
                    custom_title: Some("Tab B".to_string()),
                    root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                        is_focused: false,
                        custom_vertical_tabs_title: None,
                        contents: LeafContents::Terminal(TerminalPaneSnapshot {
                            uuid: vec![2],
                            cwd: Some("/tmp".to_string()),
                            shell_launch_data: Some(ShellLaunchData::Executable {
                                executable_path: PathBuf::from("/bin/zsh"),
                                shell_type: crate::terminal::shell::ShellType::Zsh,
                            }),
                            is_active: false,
                            is_read_only: false,
                            input_config: None,
                            llm_model_override: None,
                            active_profile_id: None,
                            conversation_ids_to_restore: vec![],
                            active_conversation_id: None,
                        }),
                    }),
                    default_directory_color: None,
                    selected_color: SelectedTabColor::default(),
                    left_panel: None,
                    right_panel: None,
                    group_id: None,
                    pinned: false,
                },
            ],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![],
            split_groups: vec![WindowSplitGroupSnapshot {
                tab_indices: vec![0, 1],
                slot_weights: vec![2.0, 1.0],
                focused_slot_index: Some(0),
            }],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    let win = &restored.windows[0];
    assert_eq!(win.split_groups.len(), 1, "split group must round-trip");
    let sg = &win.split_groups[0];
    assert_eq!(sg.tab_indices, vec![0, 1]);
    assert_eq!(sg.slot_weights, vec![2.0, 1.0]);
    assert_eq!(sg.focused_slot_index, Some(0));
}

/// An empty `split_groups` column (legacy DB) produces an empty vec on restore.
#[test]
fn test_sqlite_legacy_no_split_groups_column_gives_empty() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    // Save without split_groups (empty vec = no column written).
    let app_state = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };
    save_app_state(&mut conn, &app_state).expect("app state should save");

    // Explicitly NULL the split_groups column to simulate a pre-migration DB.
    conn.batch_execute("UPDATE windows SET split_groups = NULL;")
        .expect("should null split_groups column");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert!(
        restored.windows[0].split_groups.is_empty(),
        "NULL split_groups must restore as empty vec (legacy DB fallback)"
    );
}

/// Malformed JSON in `split_groups` is silently dropped rather than crashing.
#[test]
fn test_sqlite_corrupt_split_groups_json_gives_empty() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };
    save_app_state(&mut conn, &app_state).expect("app state should save");

    // Inject invalid JSON.
    conn.batch_execute("UPDATE windows SET split_groups = 'this is not json';")
        .expect("should corrupt split_groups");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert!(
        restored.windows[0].split_groups.is_empty(),
        "malformed JSON must be silently ignored, returning empty split_groups"
    );
}

/// Out-of-range tab indices are dropped; groups with fewer than 2 valid
/// survivors are pruned entirely.
#[test]
fn test_sqlite_split_groups_out_of_range_indices_pruned() {
    use crate::app_state::WindowSplitGroupSnapshot;

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    // A window with one tab; a split group that references indices 0 and 99.
    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![TabSnapshot {
                custom_title: None,
                root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: true,
                    custom_vertical_tabs_title: None,
                    contents: LeafContents::Terminal(TerminalPaneSnapshot {
                        uuid: vec![10],
                        cwd: None,
                        shell_launch_data: None,
                        is_active: true,
                        is_read_only: false,
                        input_config: None,
                        llm_model_override: None,
                        active_profile_id: None,
                        conversation_ids_to_restore: vec![],
                        active_conversation_id: None,
                    }),
                }),
                default_directory_color: None,
                selected_color: SelectedTabColor::default(),
                left_panel: None,
                right_panel: None,
                group_id: None,
                pinned: false,
            }],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![],
            split_groups: vec![WindowSplitGroupSnapshot {
                tab_indices: vec![0, 99], // 99 is out of range
                slot_weights: vec![1.0, 1.0],
                focused_slot_index: None,
            }],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert!(
        restored.windows[0].split_groups.is_empty(),
        "split group with only 1 valid index must be pruned on restore"
    );
}

/// Duplicate tab indices in a persisted split group are dropped.
#[test]
fn test_sqlite_split_groups_duplicate_indices_pruned() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![test_terminal_window_snapshot(false)],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };
    save_app_state(&mut conn, &app_state).expect("app state should save");

    // Inject a split group with duplicate indices via raw JSON.
    conn.batch_execute(
        "UPDATE windows SET split_groups = '[{\"tab_indices\":[0,0],\"slot_weights\":[1.0,1.0],\"focused_slot_index\":null}]';"
    ).expect("should inject duplicate indices");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert!(
        restored.windows[0].split_groups.is_empty(),
        "split group with duplicate tab indices must be pruned"
    );
}

/// Blocker 4 regression: when the middle of three tabs fails to restore, the
/// group membership and weights must be remapped to the surviving slot indices.
/// Group [0,1,2] where tab 1 (B) fails becomes [0,1] (A and C at their new
/// restored positions, with their respective weights preserved).
#[test]
fn test_sqlite_split_groups_remaps_after_dropped_middle_tab() {
    use crate::app_state::WindowSplitGroupSnapshot;

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    // Build a 3-tab window with a split [A(0), B(1), C(2)] with weights [2.0, 1.0, 1.5].
    fn make_terminal_tab_snapshot(uuid: u8) -> TabSnapshot {
        use crate::app_state::{
            LeafContents, LeafSnapshot, PaneNodeSnapshot, TerminalPaneSnapshot,
        };
        TabSnapshot {
            custom_title: None,
            root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                is_focused: uuid == 1,
                custom_vertical_tabs_title: None,
                contents: LeafContents::Terminal(TerminalPaneSnapshot {
                    uuid: vec![uuid],
                    cwd: Some("/tmp".to_string()),
                    shell_launch_data: Some(ShellLaunchData::Executable {
                        executable_path: PathBuf::from("/bin/zsh"),
                        shell_type: crate::terminal::shell::ShellType::Zsh,
                    }),
                    is_active: uuid == 1,
                    is_read_only: false,
                    input_config: None,
                    llm_model_override: None,
                    active_profile_id: None,
                    conversation_ids_to_restore: vec![],
                    active_conversation_id: None,
                }),
            }),
            default_directory_color: None,
            selected_color: SelectedTabColor::default(),
            left_panel: None,
            right_panel: None,
            group_id: None,
            pinned: false,
        }
    }

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![
                make_terminal_tab_snapshot(10), // A at DB pos 0
                make_terminal_tab_snapshot(11), // B at DB pos 1
                make_terminal_tab_snapshot(12), // C at DB pos 2
            ],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![],
            split_groups: vec![WindowSplitGroupSnapshot {
                tab_indices: vec![0, 1, 2], // A, B, C
                slot_weights: vec![2.0, 1.0, 1.5],
                focused_slot_index: Some(2), // C is focused
            }],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    // Corrupt B's pane_node row so read_root_node fails for it.
    // This simulates a tab that fails to restore (dropped from saved_tabs).
    conn.batch_execute(
        "DELETE FROM pane_nodes WHERE tab_id = (
            SELECT id FROM tabs ORDER BY id ASC LIMIT 1 OFFSET 1
        );",
    )
    .expect("should corrupt middle tab");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    // B failed to restore, so saved_tabs = [A(slot 0), C(slot 1)].
    // The split group was [0,1,2] in DB order; B (DB pos 1) is gone.
    // Remapped: A stays at restored slot 0, C moves to restored slot 1.
    // The group must survive with 2 members.
    assert_eq!(
        restored.windows[0].tabs.len(),
        2,
        "B must have been dropped"
    );
    let sg = &restored.windows[0].split_groups;
    assert_eq!(sg.len(), 1, "split group must survive after B is dropped");
    let sg0 = &sg[0];
    assert_eq!(sg0.tab_indices, vec![0, 1], "A at 0, C at 1 after remap");
    // Weights: A had 2.0 (DB pos 0), C had 1.5 (DB pos 2).
    assert_eq!(
        sg0.slot_weights,
        vec![2.0, 1.5],
        "weights must follow the surviving tabs"
    );
    // focused_slot_index: C was originally at pos 2 in [A,B,C].
    // After dropping B, the surviving sequence is [A,C]; C is at pos 1.
    assert_eq!(
        sg0.focused_slot_index,
        Some(1),
        "focus must remap to C's new position"
    );
}

/// Blocker 5 regression: a split group with 5 members (> MAX_VISIBLE=4) is
/// capped to exactly MAX_VISIBLE=4 when restored.  Uses a window with 5 real
/// tabs so all DB-index → restored-slot mappings are valid; the 5th entry is
/// dropped by the .take(MAX_VISIBLE) cap.  Weights and focus travel with their
/// original positions (not zipped sequentially with the filtered list).
#[test]
fn test_sqlite_split_groups_oversized_capped_at_max_visible() {
    use crate::app_state::WindowSplitGroupSnapshot;

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("warp.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    // Build 5 distinct terminal tabs.
    fn tab(uuid: u8) -> TabSnapshot {
        TabSnapshot {
            custom_title: None,
            root: PaneNodeSnapshot::Leaf(LeafSnapshot {
                is_focused: uuid == 10,
                custom_vertical_tabs_title: None,
                contents: LeafContents::Terminal(TerminalPaneSnapshot {
                    uuid: vec![uuid],
                    cwd: Some("/tmp".to_string()),
                    shell_launch_data: Some(ShellLaunchData::Executable {
                        executable_path: PathBuf::from("/bin/zsh"),
                        shell_type: crate::terminal::shell::ShellType::Zsh,
                    }),
                    is_active: uuid == 10,
                    is_read_only: false,
                    input_config: None,
                    llm_model_override: None,
                    active_profile_id: None,
                    conversation_ids_to_restore: vec![],
                    active_conversation_id: None,
                }),
            }),
            default_directory_color: None,
            selected_color: SelectedTabColor::default(),
            left_panel: None,
            right_panel: None,
            group_id: None,
            pinned: false,
        }
    }

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![tab(10), tab(11), tab(12), tab(13), tab(14)],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            warp_ai_width: None,
            voltron_width: None,
            warp_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
            tab_groups: vec![],
            // 5-member group, weights 1.0..5.0, focus=4 (last).
            split_groups: vec![WindowSplitGroupSnapshot {
                tab_indices: vec![0, 1, 2, 3, 4],
                slot_weights: vec![1.0, 2.0, 3.0, 4.0, 5.0],
                focused_slot_index: Some(4),
            }],
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows[0].tabs.len(), 5, "all 5 tabs must restore");
    let sg = &restored.windows[0].split_groups;
    assert_eq!(sg.len(), 1, "exactly 1 group must be restored");
    let sg0 = &sg[0];
    // MAX_VISIBLE = 4, so the 5th member (tab index 4, weight 5.0) is dropped.
    assert_eq!(
        sg0.tab_indices.len(),
        4,
        "capped group must have exactly MAX_VISIBLE=4 members"
    );
    // The first 4 tabs have restored-slot indices 0..3.
    assert_eq!(sg0.tab_indices, vec![0, 1, 2, 3], "first 4 tabs survive");
    // Weights must travel with original positions: tab[0]→1.0 … tab[3]→4.0.
    assert_eq!(
        sg0.slot_weights,
        vec![1.0, 2.0, 3.0, 4.0],
        "weights must follow original positions through the cap"
    );
    // focused_slot_index was 4 (dropped) → no matching surviving orig_pos →
    // falls back to None (the workspace restore then uses first()).
    // At the SQLite layer focused_slot_index is None when the focus was dropped.
    assert_eq!(
        sg0.focused_slot_index, None,
        "focused_slot_index must be None when the focused member was capped out"
    );
}
