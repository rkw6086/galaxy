use std::sync::Arc;

use chrono::{Duration, Utc};
use futures_util::future::BoxFuture;
use galaxy_core::ui::appearance::Appearance;
use galaxy_editor::editor::EditorView;
use galaxyui::platform::WindowStyle;
use galaxyui::presenter::ChildView;
use galaxyui::r#async::Timer;
use galaxyui::telemetry::EventPayload;
use galaxyui::{
    AddSingletonModel, App, AppContext, Element, Entity, SingletonEntity, TypedActionView, View,
    ViewHandle, WindowId,
};
use itertools::Itertools;

use super::{NotebookEvent, NotebookView, EDIT_WINDOW_DURATION, SAVE_PERIOD};
use crate::auth::auth_manager::AuthManager;
use crate::auth::user::{TEST_USER_EMAIL, TEST_USER_UID};
use crate::auth::{AuthStateProvider, UserUid};
use crate::cloud_object::model::actions::ObjectActions;
use crate::cloud_object::model::persistence::CloudModel;
use crate::cloud_object::model::view::{CloudViewModel, Editor, EditorState};
use crate::cloud_object::{
    Owner, Revision, ServerCloudObject, ServerMetadata, ServerNotebook, ServerPermissions,
};
use crate::drive::OpenGalaxyDriveObjectSettings;
use crate::editor::{DisplayPoint, EditorAction, InteractionState, SelectAction};
use crate::network::NetworkStatus;
use crate::notebooks::active_notebook_data::Mode;
use crate::notebooks::editor::keys::NotebookKeybindings;
use crate::notebooks::editor::notebook_command::NotebookCommand;
use crate::notebooks::editor::view::EditorViewAction;
use crate::notebooks::notebook::FocusedComponent;
use crate::notebooks::{CloudNotebook, CloudNotebookModel, NotebookLocation};
use crate::pane_group::PaneEvent;
use crate::search::files::model::FileSearchModel;
use crate::server::cloud_objects::update_manager::{InitialLoadResponse, UpdateManager};
use crate::server::ids::ClientId;
use crate::server::ids::SyncId::ServerId;
use crate::server::server_api::ServerApiProvider;
use crate::server::sync_queue::{SyncQueue, SyncQueueEvent};
use crate::server::telemetry::context_provider::AppTelemetryContextProvider;
use crate::settings_view::keybindings::KeybindingChangedNotifier;
use crate::terminal::keys::TerminalKeybindings;
use crate::test_util::settings::initialize_settings_for_tests;
use crate::workflows::workflow::Workflow;
use crate::workflows::{WorkflowSource, WorkflowType};
use crate::workspace::ActiveSession;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::user_profiles::{UserProfileWithUID, UserProfiles};
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::{GlobalResourceHandles, GlobalResourceHandlesProvider, PrivacySettings};

fn initialize_app(app: &mut App) {
    initialize_settings_for_tests(app);

    let global_resources = GlobalResourceHandles::mock(app);
    app.add_singleton_model(|_| GlobalResourceHandlesProvider::new(global_resources));
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(|ctx| {
        crate::local_object_repository::LocalObjectRepository::new(
            None,
            Some(Owner::mock_current_user()),
            ctx,
        )
    });
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(|_| Appearance::mock());
    app.add_singleton_model(|_| KeybindingChangedNotifier::new());
    app.add_singleton_model(|_| repo_metadata::repositories::DetectedRepositories::default());
    #[cfg(feature = "local_fs")]
    app.add_singleton_model(repo_metadata::RepoMetadataModel::new);
    app.add_singleton_model(FileSearchModel::new);
    app.add_singleton_model(NotebookKeybindings::new);
    app.add_singleton_model(TerminalKeybindings::new);
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(UserWorkspaces::default_mock);
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(UpdateManager::mock);
    app.add_singleton_model(CloudViewModel::mock);
    app.add_singleton_model(|_| UserProfiles::new(vec![]));
    app.add_singleton_model(|_| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| ActiveSession::default());
    app.add_singleton_model(|_| ObjectActions::new(Vec::new()));
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AppTelemetryContextProvider::new_context_provider);
    app.add_singleton_model(AuthManager::new_for_test);
    #[cfg(feature = "voice_input")]
    app.add_singleton_model(voice_input::VoiceInput::new);
}

/// Container so that [`NotebookView`] can be registered as a typed action view.
struct Root {
    notebook: ViewHandle<NotebookView>,
    events: Vec<NotebookEvent>,
}

impl Entity for Root {
    type Event = ();
}

impl View for Root {
    fn ui_name() -> &'static str {
        "Root"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        ChildView::new(&self.notebook).finish()
    }
}

impl TypedActionView for Root {
    type Action = ();
}

fn create_notebook(app: &mut App) -> (WindowId, ViewHandle<NotebookView>, ViewHandle<Root>) {
    let (window, root) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
        let notebook = ctx.add_typed_action_view(NotebookView::new);

        ctx.subscribe_to_view(&notebook, |me: &mut Root, _, event, _| {
            me.events.push(event.clone())
        });

        Root {
            notebook,
            events: Vec::new(),
        }
    });
    let notebook = app.read(|ctx| root.as_ref(ctx).notebook.clone());
    (window, notebook, root)
}

/// Opens a notebook in the given view.
fn open_notebook(
    app: &mut App,
    handle: &ViewHandle<NotebookView>,
    notebook: CloudNotebook,
) -> BoxFuture<'static, ()> {
    let load_future = handle.update(app, |view, ctx| {
        view.load(notebook, &OpenGalaxyDriveObjectSettings::default(), ctx)
    });
    app.update(|ctx| ctx.await_spawned_future(load_future.future_id()))
}

fn cloud_notebook(title: impl Into<String>, data: impl Into<String>) -> CloudNotebook {
    CloudNotebook::new_local(
        CloudNotebookModel {
            title: title.into(),
            data: data.into(),
            ai_document_id: None,
            conversation_id: None,
        },
        Owner::mock_current_user(),
        None,
        ClientId::new(),
    )
}

/// Mock a server notebook
fn mock_server_notebook(title: impl Into<String>, data: impl Into<String>) -> ServerNotebook {
    let metadata_ts = Utc::now().into();
    ServerNotebook::new(
        ServerId(123.into()),
        CloudNotebookModel {
            title: title.into(),
            data: data.into(),
            ai_document_id: None,
            conversation_id: None,
        },
        ServerMetadata {
            uid: 123.into(),
            revision: Revision::now(),
            metadata_last_updated_ts: metadata_ts,
            trashed_ts: None,
            folder_id: None,
            is_welcome_object: false,
            creator_uid: None,
            last_editor_uid: None,
            current_editor_uid: None,
        },
        ServerPermissions {
            space: Owner::mock_current_user(),
            guests: Vec::new(),
            anyone_link_sharing: None,
            permissions_last_updated_ts: metadata_ts,
        },
    )
}

/// Send changed objects to [`UpdateManager`] so that tests requiring "up-to-date" metadata can run.
async fn initial_load(app: &mut App, updated_notebooks: impl Into<Vec<ServerNotebook>>) {
    let response = InitialLoadResponse {
        updated_notebooks: updated_notebooks.into(),
        deleted_notebooks: Default::default(),
        updated_workflows: Default::default(),
        deleted_workflows: Default::default(),
        updated_folders: Default::default(),
        deleted_folders: Default::default(),
        user_profiles: Default::default(),
        updated_generic_string_objects: Default::default(),
        deleted_generic_string_objects: Default::default(),
        action_histories: Default::default(),
        mcp_gallery: Default::default(),
    };

    let load_complete = UpdateManager::handle(app).update(app, |update_manager, ctx| {
        update_manager.mock_initial_load(response, ctx);
        update_manager.initial_load_complete()
    });
    load_complete.await
}

/// Wait for all edits to be saved.
async fn ensure_saved(app: &mut App, notebook_view: &ViewHandle<NotebookView>) {
    loop {
        let has_edits = notebook_view.read(app, |notebook, _| {
            notebook.content_is_dirty || notebook.title_is_dirty
        });
        if has_edits {
            Timer::after(SAVE_PERIOD).await;
        } else {
            break;
        }
    }

    // Ensure that any updates from the debounced save were processed.
    app.update(|_| ());
}

/// Test that command-block execution events are correctly translated into workflows.
#[test]
fn test_command_block_dispatches_event() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, []).await;

        let (window, notebook, root) = create_notebook(&mut app);
        open_notebook(
            &mut app,
            &notebook,
            cloud_notebook(
                "Test Notebook",
                r#"A command:
```
echo hello
```
"#,
            ),
        )
        .await;

        // First, make sure the editor is focused.
        notebook.update(&mut app, |notebook, ctx| {
            notebook.focus_input(ctx);
        });

        app.update(|ctx| {
            let input = &notebook.as_ref(ctx).input;
            let command = input
                .as_ref(ctx)
                .runnable_command_at(11.into(), ctx)
                .expect("Command should exist")
                .as_any()
                .downcast_ref::<NotebookCommand>()
                .expect("Should convert");

            // Use the command's own to_workflow implementation to use as much of the real code
            // path as possible.
            let workflow = command
                .to_workflow(ctx)
                .expect("Can't convert command to a workflow");

            ctx.dispatch_typed_action_for_view(
                window,
                input.id(),
                &EditorViewAction::RunWorkflow(workflow),
            );
        });

        app.read(|ctx| {
            let events = &root.as_ref(ctx).events;
            assert!(
                events.contains(&NotebookEvent::RunWorkflow {
                    workflow: Arc::new(WorkflowType::Notebook(Workflow::new(
                        "Command from Test Notebook",
                        "echo hello"
                    ))),
                    source: WorkflowSource::Notebook {
                        notebook_id: None,
                        team_uid: None,
                        location: NotebookLocation::PersonalCloud,
                    },
                }),
                "No RunWorkflow event in {events:#?}"
            );
        })
    });
}

#[test]
fn test_focus_tracking() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, []).await;

        let (window, notebook, root) = create_notebook(&mut app);
        open_notebook(
            &mut app,
            &notebook,
            cloud_notebook("Test Notebook", "This is a notebook"),
        )
        .await;

        let (title_view, input_view) = notebook.read(&app, |notebook, _| {
            (notebook.title.clone(), notebook.input.clone())
        });

        // Focus the title editor by selecting.
        app.update(|ctx| {
            ctx.dispatch_typed_action_for_view(
                window,
                title_view.id(),
                &EditorAction::Select(SelectAction::begin(DisplayPoint::new(0, 4))),
            );
        });
        app.read(|ctx| {
            assert_eq!(
                notebook.as_ref(ctx).last_focused_component,
                FocusedComponent::Title
            );

            let events = &root.as_ref(ctx).events;
            assert_eq!(
                events,
                &[
                    // This is from focusing the title editor.
                    NotebookEvent::Pane(PaneEvent::FocusSelf)
                ]
            );
        });

        // When blurring the notebook and restoring focus, focus should go to the title editor.
        root.update(&mut app, |_, ctx| ctx.focus_self());
        notebook.update(&mut app, |notebook, ctx| notebook.focus(ctx));
        assert_eq!(app.focused_view_id(window), Some(title_view.id()));
        app.read(|ctx| {
            let events = &root.as_ref(ctx).events;
            assert_eq!(
                events,
                &[
                    // This is the prior focus event.
                    NotebookEvent::Pane(PaneEvent::FocusSelf),
                    // This is from focusing the title editor again.
                    NotebookEvent::Pane(PaneEvent::FocusSelf)
                ]
            );
        });

        // Focus the input view, which should emit a focused event.
        input_view.update(&mut app, |view, ctx| view.focus(ctx));
        app.read(|ctx| {
            assert_eq!(
                notebook.as_ref(ctx).last_focused_component,
                FocusedComponent::Input
            );

            let events = &root.as_ref(ctx).events;
            assert_eq!(
                events,
                &[
                    // These are prior events.
                    NotebookEvent::Pane(PaneEvent::FocusSelf),
                    NotebookEvent::Pane(PaneEvent::FocusSelf),
                    // This is from focusing the input editor.
                    NotebookEvent::Pane(PaneEvent::FocusSelf),
                ]
            );
        });

        // Now, focus should be restored to the input editor.
        root.update(&mut app, |_, ctx| ctx.focus_self());
        notebook.update(&mut app, |notebook, ctx| notebook.focus(ctx));
        assert_eq!(app.focused_view_id(window), Some(input_view.id()));
    });
}

#[test]
#[ignore]
fn test_edit_telemetry() {
    fn edit_events() -> Vec<serde_json::Value> {
        galaxyui::telemetry::flush_events()
            .into_iter()
            .filter_map(|event| match event.payload {
                EventPayload::NamedEvent { name, value, .. } if name == "Notebook Edited" => value,
                _ => None,
            })
            .collect_vec()
    }

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, []).await;

        let (_, notebook, _) = create_notebook(&mut app);
        open_notebook(
            &mut app,
            &notebook,
            cloud_notebook("Test Notebook", "This is a notebook"),
        )
        .await;
        let input_view = notebook.read(&app, |notebook, _| notebook.input.clone());

        // The notebook should show in edit mode, with telemetry recording.
        notebook.update(&mut app, |notebook, ctx| {
            notebook.grab_edit_access(ctx);
            assert_eq!(
                notebook.active_notebook_data.as_ref(ctx).mode,
                Mode::Editing
            );
            assert!(notebook.edit_telemetry_handle.is_some());
            notebook.focus_input(ctx);
        });

        // With no edits, there are no events.
        ensure_saved(&mut app, &notebook).await;
        Timer::after(2 * EDIT_WINDOW_DURATION).await;
        assert!(edit_events().is_empty());

        // Make a small edit, which should get reported as non-meaningful.
        input_view.update(&mut app, |input, ctx| {
            input.user_typed("Hi", ctx);
        });

        ensure_saved(&mut app, &notebook).await;
        Timer::after(2 * EDIT_WINDOW_DURATION).await;
        assert_eq!(
            edit_events(),
            vec![serde_json::json!({
                "notebook_id": None::<()>,
                "meaningful_change": false,
            })]
        );

        // If we switch to view mode, we stop recording elemetry.
        notebook.update(&mut app, |notebook, ctx| {
            notebook.switch_to_view(ctx);
            assert!(notebook.edit_telemetry_handle.is_none());
        });

        // Telemetry resumes when we switch to editing.
        notebook.update(&mut app, |notebook, ctx| {
            notebook.switch_to_edit(ctx);
            notebook.focus_input(ctx);
            assert!(notebook.edit_telemetry_handle.is_some());
        });

        // Finally, a meaningful edit is recorded as such.
        input_view.update(&mut app, |input, ctx| {
            input.user_typed(
                "This is a very very very very long edit. This is a heavy notebook user.",
                ctx,
            );
        });

        ensure_saved(&mut app, &notebook).await;
        Timer::after(2 * EDIT_WINDOW_DURATION).await;
        assert_eq!(
            edit_events(),
            vec![serde_json::json!({
                "notebook_id": None::<()>,
                "meaningful_change": true,
            })]
        );
    });
}

#[test]
fn test_local_notebook_ignores_legacy_remote_editor() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let (_, notebook_view, _) = create_notebook(&mut app);
        let mut cloud_notebook = cloud_notebook("Test Notebook", "A notebook");
        cloud_notebook.metadata.current_editor_uid = Some("legacy-user".to_string());

        CloudModel::handle(&app).update(&mut app, |model, _| {
            model.add_object(cloud_notebook.id, cloud_notebook.clone())
        });
        open_notebook(&mut app, &notebook_view, cloud_notebook).await;

        notebook_view.read(&app, |notebook, ctx| {
            assert_eq!(
                notebook.active_notebook_data.as_ref(ctx).current_editor(),
                None
            );
            assert_eq!(notebook.mode_app_ctx(ctx), Mode::Editing);
        });
    });
}

#[test]
fn test_close_with_pending_changes() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, vec![]).await;

        // Stop dequeueing, so that we can verify the queue contents.
        SyncQueue::handle(&app).update(&mut app, |sync_queue, _ctx| {
            sync_queue.stop_dequeueing();
            assert_eq!(sync_queue.queue().len(), 0);
        });

        // Create a notebook with a server ID, so it can be synced.
        let cloud_notebook =
            CloudNotebook::new_from_server(mock_server_notebook("Test", "Some text"));
        let notebook_id = cloud_notebook.id;

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(cloud_notebook.id, cloud_notebook.clone());
        });

        let (_, notebook_view, _) = create_notebook(&mut app);

        open_notebook(&mut app, &notebook_view, cloud_notebook).await;

        // Edit the notebook. It should not be saved yet.
        notebook_view.update(&mut app, |notebook: &mut NotebookView, ctx| {
            notebook.input.update(ctx, |input, ctx| {
                input.user_typed("Hello ", ctx);
            })
        });

        app.read(|ctx| {
            let object = CloudModel::as_ref(ctx)
                .get_by_uid(&notebook_id.uid())
                .expect("Notebook should exist");
            assert!(!object.metadata().has_pending_content_changes());
        });

        // Closing the notebook should force a save.
        notebook_view.update(&mut app, |notebook, ctx| notebook.on_detach(ctx));

        app.read(|ctx| {
            let object = CloudModel::as_ref(ctx)
                .get_by_uid(&notebook_id.uid())
                .expect("Notebook should exist");
            assert!(!object.metadata().has_pending_content_changes());
            let notebook = CloudModel::as_ref(ctx)
                .get_notebook(&notebook_id)
                .expect("Notebook should exist");
            assert_eq!(notebook.model().title, "Test");
            assert_eq!(notebook.model().data, "Hello Some text");
            assert!(SyncQueue::as_ref(ctx).queue().is_empty());
        })
    });
}

#[test]
fn test_close_unmodified() {
    // If we close a notebook with no pending changes, it should not save.

    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, vec![]).await;

        // Stop dequeueing, so that we can verify the queue contents.
        SyncQueue::handle(&app).update(&mut app, |sync_queue, _ctx| {
            sync_queue.stop_dequeueing();
            assert_eq!(sync_queue.queue().len(), 0);
        });

        // Create a notebook with a server ID, so it can be synced.
        let cloud_notebook =
            CloudNotebook::new_from_server(mock_server_notebook("Test", "Some text"));
        let notebook_id = cloud_notebook.id;

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(cloud_notebook.id, cloud_notebook.clone());
        });

        let (_, notebook_view, _) = create_notebook(&mut app);

        open_notebook(&mut app, &notebook_view, cloud_notebook).await;

        // Close the notebook with no changes.
        notebook_view.update(&mut app, |notebook, ctx| notebook.on_detach(ctx));

        app.read(|ctx| {
            let object = CloudModel::as_ref(ctx)
                .get_by_uid(&notebook_id.uid())
                .expect("Notebook should exist");
            assert!(!object.metadata().has_pending_content_changes());

            let sync_queue = SyncQueue::as_ref(ctx).queue();
            assert!(sync_queue.is_empty());
        })
    });
}

#[test]
fn test_only_user_title_edits_are_persisted_locally() {
    // This tests that we only persist user edits and don't echo back received title changes.
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, vec![]).await;

        // Stop dequeueing, so that we can verify the queue contents.
        SyncQueue::handle(&app).update(&mut app, |sync_queue, _ctx| {
            sync_queue.stop_dequeueing();
            assert_eq!(sync_queue.queue().len(), 0);
        });

        let (_, notebook_view, _) = create_notebook(&mut app);

        // Create a notebook with a server ID, so it can be synced.
        let mut server_notebook = mock_server_notebook("Initial Title", "Notebook contents");
        let cloud_notebook: CloudNotebook = CloudNotebook::new_from_server(server_notebook.clone());
        let notebook_id = cloud_notebook.id;

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(cloud_notebook.id, cloud_notebook.clone());
        });
        open_notebook(&mut app, &notebook_view, cloud_notebook).await;

        // When a new title comes in, we should update the buffer but not emit a change.
        server_notebook.model.title = "New Title".to_string();
        server_notebook.metadata.revision = (Utc::now() + Duration::seconds(2)).into();
        CloudModel::handle(&app).update(&mut app, |cloud_model, ctx| {
            cloud_model.upsert_from_server_notebook(server_notebook, ctx);
        });

        notebook_view.read(&app, |notebook, ctx| {
            assert_eq!(notebook.title(ctx), "New Title");
            assert!(!notebook.title_is_dirty);
        });

        // When the _user_ edits the title, that should be synced.
        notebook_view.update(&mut app, |notebook, ctx| {
            notebook.title.update(ctx, |title, ctx| {
                title.user_insert("!!!", ctx);
            });
        });
        // This is outside the `update` callback so that it runs after the event is dispatched.
        notebook_view.read(&app, |notebook, _| {
            assert!(notebook.title_is_dirty);
        });

        ensure_saved(&mut app, &notebook_view).await;

        app.read(|ctx| {
            let notebook = CloudModel::as_ref(ctx)
                .get_notebook(&notebook_id)
                .expect("Notebook should exist");
            assert_eq!(notebook.model().title, "New Title!!!");
            assert!(!notebook.metadata.has_pending_content_changes());
            assert!(SyncQueue::as_ref(ctx).queue().is_empty());
        });
    });
}

#[test]
fn test_legacy_conflict_does_not_block_local_editing() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        initial_load(&mut app, vec![]).await;

        let (_, notebook_view, _) = create_notebook(&mut app);

        let mut server_notebook = mock_server_notebook("A Notebook", "Local Data");
        let mut cloud_notebook: CloudNotebook =
            CloudNotebook::new_from_server(server_notebook.clone());
        server_notebook.model.data = "Remote Data".to_string();
        cloud_notebook.set_conflicting_object(Arc::new(server_notebook.clone()));

        CloudModel::handle(&app).update(&mut app, |cloud_model, _| {
            cloud_model.add_object(cloud_notebook.id, cloud_notebook.clone());
        });
        open_notebook(&mut app, &notebook_view, cloud_notebook).await;

        app.read(|ctx| {
            let active_notebook_data = notebook_view.as_ref(ctx).active_notebook_data.as_ref(ctx);
            assert!(!active_notebook_data.has_conflicts());
            assert_eq!(active_notebook_data.mode, Mode::Editing);
            assert_eq!(
                notebook_view
                    .as_ref(ctx)
                    .input
                    .as_ref(ctx)
                    .interaction_state(ctx),
                InteractionState::Editable
            );
        });
    });
}

#[test]
fn test_untitled_notebook() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let (_, notebook, _) = create_notebook(&mut app);

        notebook.update(&mut app, |notebook, ctx| {
            notebook.open_new_notebook(None, Owner::mock_current_user(), None, ctx);
        });

        notebook.read(&app, |notebook, ctx| {
            assert_eq!(notebook.title(ctx), "Untitled");
        });

        notebook.update(&mut app, |notebook, ctx| {
            notebook.switch_to_edit(ctx);
            notebook.focus_title(ctx);
            notebook.title.update(ctx, |title, ctx| {
                title.user_insert("My Notebook", ctx);
            });
            assert_eq!(notebook.title(ctx), "My Notebook");
        });
    });
}
