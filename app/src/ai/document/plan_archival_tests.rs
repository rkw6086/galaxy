use std::sync::mpsc::sync_channel;

use ai::document::AIDocumentId;
use galaxyui::App;

use super::*;
use crate::notebooks::CloudNotebookModel;
use crate::persistence::ModelEvent;
use crate::test_util::settings::initialize_settings_for_tests;

fn completed_plan(completed_at: DateTime<Utc>) -> CompletedPlan {
    CompletedPlan {
        conversation_id: AIConversationId::new(),
        completed_at,
        archived_to: None,
        original_folder: None,
    }
}

#[test]
fn completed_plan_archives_after_seven_full_days_only() {
    let now = Utc::now();
    let plan = completed_plan(now - chrono::Duration::days(7));
    assert!(!plan.is_due(now - chrono::Duration::milliseconds(1)));
    assert!(plan.is_due(now));
    assert!(!completed_plan(now + chrono::Duration::days(1)).is_due(now));
}

#[test]
fn archived_plan_deadline_and_recovery_location_survive_restart() {
    let mut plan = completed_plan(Utc::now() - chrono::Duration::days(8));
    plan.archived_to = Some(SyncId::ClientId(ClientId::new()));
    plan.original_folder = Some(SyncId::ClientId(ClientId::new()));
    let serialized = serde_json::to_string(&plan).unwrap();
    let restored: CompletedPlan = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored, plan);
    assert!(!restored.is_due(Utc::now()));
}

#[test]
fn revisiting_a_completed_plan_preserves_its_original_deadline() {
    let mut model = PlanArchivalModel {
        completed_plans: HashMap::new(),
        ..Default::default()
    };
    let conversation_id = AIConversationId::new();
    let completed_at = Utc::now() - chrono::Duration::days(6);
    model.observe_completion("plan".to_string(), conversation_id, completed_at);
    model.observe_completion("plan".to_string(), conversation_id, Utc::now());
    assert_eq!(model.completed_plans["plan"].completed_at, completed_at);
}

#[test]
fn legacy_completed_plan_records_are_preserved() {
    let record = completed_plan(Utc::now() - chrono::Duration::days(4));
    let records = HashMap::from([("plan".to_string(), record.clone())]);
    let json = serde_json::to_string(&records).unwrap();
    let model = PlanArchivalModel::from_persisted(&json).unwrap();
    assert_eq!(model.completed_plans["plan"], record);
    assert!(model.reopened_plans.is_empty());
}

#[test]
fn dirty_and_reverted_plans_stay_visible_until_new_work_accepts_the_edits() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.add_singleton_model(|_| CloudModel::new(None, Vec::new(), None));
        app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let documents = app.add_singleton_model(|_| AIDocumentModel::new_for_test());
        let repository = app.add_singleton_model(|ctx| {
            LocalObjectRepository::new(None, None, ctx)
        });
        let document_id = AIDocumentId::new();
        let notebook_id = SyncId::ClientId(ClientId::new());
        repository.update(&mut app, |repository, ctx| {
            repository.create_notebook_with_id(
                notebook_id,
                None,
                CloudNotebookModel {
                    title: "Reopened plan".to_string(),
                    data: "New unfinished work".to_string(),
                    ai_document_id: Some(document_id),
                    conversation_id: None,
                },
                ctx,
            );
        });
        let archival = app.add_model(PlanArchivalModel::new);
        let conversation_id = AIConversationId::new();
        let old_completion = Utc::now() - chrono::Duration::days(8);
        archival.update(&mut app, |model, ctx| {
            model.observe_completion(document_id.to_string(), conversation_id, old_completion);
            model.archive_plan(&document_id.to_string(), ctx);
        });
        assert!(CloudModel::as_ref(&app)
            .get_notebook(&notebook_id)
            .unwrap()
            .metadata
            .folder_id
            .is_some());

        // Typing in the editor emits Dirty; reverting a version also emits a
        // user-originated DocumentUpdated event. Either must reopen the plan.
        for event in [
            AIDocumentModelEvent::DocumentUserEditStatusUpdated {
                document_id,
                status: AIDocumentUserEditStatus::Dirty,
            },
            AIDocumentModelEvent::DocumentUpdated {
                document_id,
                version: Default::default(),
                source: AIDocumentUpdateSource::User,
            },
        ] {
            documents.update(&mut app, |_, ctx| ctx.emit(event));
            archival.update(&mut app, |model, ctx| {
                model.observe_completion(document_id.to_string(), conversation_id, old_completion);
                model.sweep(ctx);
                assert!(!model.completed_plans.contains_key(&document_id.to_string()));
                assert_eq!(
                    model.reopened_plans[&document_id.to_string()].conversation_id,
                    conversation_id,
                );
                let json = serde_json::to_string(model).unwrap();
                let mut restored = PlanArchivalModel::from_persisted(&json).unwrap();
                restored.observe_completion(
                    document_id.to_string(),
                    conversation_id,
                    old_completion,
                );
                assert!(restored.completed_plans.is_empty());
            });
            assert!(CloudModel::as_ref(&app)
                .get_notebook(&notebook_id)
                .unwrap()
                .metadata
                .folder_id
                .is_none());
        }

        archival.update(&mut app, |model, ctx| {
            // Merely restoring clean editor state cannot close a reopened plan.
            model.accept_document_edits(document_id, ctx);
            assert!(model.reopened_plans.contains_key(&document_id.to_string()));
            model.begin_exchange(conversation_id, ctx);
            assert!(model.reopened_plans.contains_key(&document_id.to_string()));
            model.accept_document_edits(document_id, ctx);
            assert!(model.reopened_plans.is_empty());
            let finished_again = Utc::now();
            model.observe_completion(document_id.to_string(), conversation_id, finished_again);
            assert_eq!(model.completed_plans[&document_id.to_string()].completed_at, finished_again);
            assert!(!model.completed_plans[&document_id.to_string()].is_due(finished_again));
        });
    });
}

#[test]
fn a_successful_draft_conversation_does_not_complete_a_plan() {
    let mut conversation = AIConversation::new(false, false);
    conversation.set_status_for_test(ConversationStatus::Success);
    assert!(completed_work_time(&conversation).is_none());

    for status in [
        ConversationStatus::InProgress,
        ConversationStatus::WaitingForEvents,
        ConversationStatus::TransientError,
        ConversationStatus::Error,
        ConversationStatus::Cancelled,
    ] {
        conversation.set_status_for_test(status);
        assert!(completed_work_time(&conversation).is_none());
    }
}

#[test]
fn archival_preserves_notebook_and_resuming_restores_original_folder() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let (sender, receiver) = sync_channel(32);
        app.add_singleton_model(|_| CloudModel::new(None, Vec::new(), None));
        app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        app.add_singleton_model(|_| AIDocumentModel::new_for_test());
        let repository = app.add_singleton_model(|ctx| {
            LocalObjectRepository::new(Some(sender), None, ctx)
        });
        let archival = app.add_model(|_| PlanArchivalModel {
            completed_plans: HashMap::new(),
            ..Default::default()
        });
        let original_folder = SyncId::ClientId(ClientId::new());
        let notebook_id = SyncId::ClientId(ClientId::new());
        let document_id = AIDocumentId::new();
        let notebook_model = CloudNotebookModel {
            title: "Retained plan".to_string(),
            data: "# Plan\n- [x] Finished work".to_string(),
            ai_document_id: Some(document_id),
            conversation_id: None,
        };
        repository.update(&mut app, |repository, ctx| {
            repository.create_folder_with_id(
                original_folder,
                "Project".to_string(),
                None,
                ctx,
            );
            repository.create_notebook_with_id(
                notebook_id,
                Some(original_folder),
                notebook_model.clone(),
                ctx,
            );
        });
        let conversation_id = AIConversationId::new();
        archival.update(&mut app, |model, ctx| {
            model.observe_completion(
                document_id.to_string(),
                conversation_id,
                Utc::now() - chrono::Duration::days(8),
            );
            model.persist(ctx);
        });

        // Simulate startup with persisted completion records and no loaded
        // conversations/documents. The notebook is still retained in Drive.
        let archival = app.add_model(PlanArchivalModel::new);
        archival.update(&mut app, |model, ctx| model.sweep(ctx));

        let archive_folder = archival.read(&app, |model, _| {
            model.completed_plans[&document_id.to_string()]
                .archived_to
                .expect("plan should have been archived")
        });
        let cloud_model = CloudModel::as_ref(&app);
        let notebook = cloud_model.get_notebook(&notebook_id).unwrap();
        assert_eq!(notebook.model(), &notebook_model);
        assert_eq!(notebook.metadata.folder_id, Some(archive_folder));
        assert!(!notebook.is_trashed(cloud_model));
        let folder = cloud_model.get_folder(&archive_folder).unwrap();
        assert_eq!(folder.model().name, ARCHIVE_FOLDER_NAME);
        let parent = cloud_model
            .get_folder(&folder.metadata.folder_id.unwrap())
            .unwrap();
        assert_eq!(parent.model().name, PLAN_FOLDER_NAME);

        let events = receiver.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            ModelEvent::UpsertNotebook { notebook }
                if notebook.id == notebook_id
                    && notebook.metadata.folder_id == Some(archive_folder)
                    && notebook.model() == &notebook_model
        )));
        assert!(!events.iter().any(|event| matches!(event, ModelEvent::DeleteObjects { .. })));

        archival.update(&mut app, |model, ctx| {
            model.resume_conversation(conversation_id, ctx);
            assert!(model.completed_plans.is_empty());
        });
        let cloud_model = CloudModel::as_ref(&app);
        let notebook = cloud_model.get_notebook(&notebook_id).unwrap();
        assert_eq!(notebook.metadata.folder_id, Some(original_folder));
        assert_eq!(notebook.model(), &notebook_model);
        assert!(!notebook.is_trashed(cloud_model));
    });
}

#[test]
fn manual_restore_does_not_rearchive_a_completed_plan() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.add_singleton_model(|_| CloudModel::new(None, Vec::new(), None));
        app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        app.add_singleton_model(|_| AIDocumentModel::new_for_test());
        let repository = app.add_singleton_model(|ctx| {
            LocalObjectRepository::new(None, None, ctx)
        });
        let archive_folder = SyncId::ClientId(ClientId::new());
        let notebook_id = SyncId::ClientId(ClientId::new());
        let document_id = AIDocumentId::new();
        repository.update(&mut app, |repository, ctx| {
            repository.create_folder_with_id(archive_folder, "Archive".to_string(), None, ctx);
            repository.create_notebook_with_id(
                notebook_id,
                Some(archive_folder),
                CloudNotebookModel {
                    title: "Recovered plan".to_string(),
                    data: "Keep this plan visible".to_string(),
                    ai_document_id: Some(document_id),
                    conversation_id: None,
                },
                ctx,
            );
        });
        let mut record = completed_plan(Utc::now() - chrono::Duration::days(8));
        record.archived_to = Some(archive_folder);
        let archival = app.add_model(|_| PlanArchivalModel {
            completed_plans: HashMap::from([(document_id.to_string(), record)]),
            ..Default::default()
        });

        assert!(repository.update(&mut app, |repository, ctx| {
            repository.move_object_to_location(
                CloudObjectTypeAndId::Notebook(notebook_id),
                CloudObjectLocation::Space(Space::Personal),
                ctx,
            )
        }));
        archival.update(&mut app, |model, ctx| model.sweep(ctx));

        let cloud_model = CloudModel::as_ref(&app);
        let notebook = cloud_model.get_notebook(&notebook_id).unwrap();
        assert!(notebook.metadata.folder_id.is_none());
        assert_eq!(notebook.model().data, "Keep this plan visible");
        archival.read(&app, |model, _| {
            assert!(!model.completed_plans[&document_id.to_string()].is_due(Utc::now()));
        });
    });
}

#[test]
fn ordinary_notebooks_are_never_archived() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.add_singleton_model(|_| CloudModel::new(None, Vec::new(), None));
        app.add_singleton_model(|_| AIDocumentModel::new_for_test());
        let repository = app.add_singleton_model(|ctx| {
            LocalObjectRepository::new(None, None, ctx)
        });
        let notebook_id = SyncId::ClientId(ClientId::new());
        repository.update(&mut app, |repository, ctx| {
            repository.create_notebook_with_id(
                notebook_id,
                None,
                CloudNotebookModel {
                    title: "Ordinary notes".to_string(),
                    data: "Keep these notes".to_string(),
                    ai_document_id: None,
                    conversation_id: None,
                },
                ctx,
            );
        });
        let archival = app.add_model(|_| PlanArchivalModel {
            completed_plans: HashMap::new(),
            ..Default::default()
        });
        archival.update(&mut app, |model, ctx| {
            model.observe_completion(
                notebook_id.to_string(),
                AIConversationId::new(),
                Utc::now() - chrono::Duration::days(8),
            );
            model.archive_plan(&notebook_id.to_string(), ctx);
        });
        let cloud_model = CloudModel::as_ref(&app);
        let notebook = cloud_model.get_notebook(&notebook_id).unwrap();
        assert!(notebook.metadata.folder_id.is_none());
        assert_eq!(notebook.model().data, "Keep these notes");
        assert_eq!(cloud_model.get_all_active_folders().count(), 0);
    });
}

#[test]
fn editing_an_archived_plan_from_drive_reopens_it_without_loading_its_conversation() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.add_singleton_model(|_| CloudModel::new(None, Vec::new(), None));
        app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        app.add_singleton_model(|_| AIDocumentModel::new_for_test());
        let repository = app.add_singleton_model(|ctx| {
            LocalObjectRepository::new(None, None, ctx)
        });
        let document_id = AIDocumentId::new();
        let notebook_id = SyncId::ClientId(ClientId::new());
        repository.update(&mut app, |repository, ctx| {
            repository.create_notebook_with_id(
                notebook_id,
                None,
                CloudNotebookModel {
                    title: "Completed plan".to_string(),
                    data: "- [x] Done".to_string(),
                    ai_document_id: Some(document_id),
                    conversation_id: None,
                },
                ctx,
            );
        });
        let archival = app.add_model(PlanArchivalModel::new);
        let conversation_id = AIConversationId::new();
        let completed_at = Utc::now() - chrono::Duration::days(8);
        archival.update(&mut app, |model, ctx| {
            model.observe_completion(document_id.to_string(), conversation_id, completed_at);
            model.archive_plan(&document_id.to_string(), ctx);
        });
        repository.update(&mut app, |repository, ctx| {
            repository.update_notebook_data(
                notebook_id,
                "- [ ] New unfinished work".to_string(),
                ctx,
            );
        });
        archival.update(&mut app, |model, ctx| {
            model.observe_completion(document_id.to_string(), conversation_id, completed_at);
            model.sweep(ctx);
            assert!(model.completed_plans.is_empty());
            assert_eq!(
                model.reopened_plans[&document_id.to_string()].conversation_id,
                conversation_id,
            );
        });
        let cloud_model = CloudModel::as_ref(&app);
        let notebook = cloud_model.get_notebook(&notebook_id).unwrap();
        assert!(notebook.metadata.folder_id.is_none());
        assert_eq!(notebook.model().data, "- [ ] New unfinished work");
        assert!(!notebook.is_trashed(cloud_model));
    });
}
