use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use galaxyui::{App, SingletonEntity};

use super::*;
use crate::cloud_object::model::persistence::CloudModel;
use crate::local_object_repository::local_owner;
use crate::server::ids::ClientId;
use crate::workflows::workflow_enum::{EnumVariants, WorkflowEnum};

#[derive(Debug, PartialEq, Eq)]
enum EventKind {
    Folder {
        folder_id: nodes::FolderId,
        object_id: Option<String>,
    },
    File {
        file_id: FileId,
        object_id: Option<String>,
    },
    FileSavedLocally(FileId),
}

#[test]
fn local_import_queue_persists_nested_content_and_reports_completion() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| CloudModel::new(None, Vec::new(), None));
        app.add_singleton_model(|ctx| {
            crate::local_object_repository::LocalObjectRepository::new(None, None, ctx)
        });

        let queue = app.add_model(|_| ImportQueue::new());
        let events = Rc::new(RefCell::new(Vec::new()));
        let events_for_subscription = events.clone();
        app.update(|ctx| {
            ctx.subscribe_to_model(&queue, move |_, event: &ImportQueueEvent, _| {
                let event = match event {
                    ImportQueueEvent::FolderCompleted {
                        folder_id,
                        server_id,
                    } => EventKind::Folder {
                        folder_id: *folder_id,
                        object_id: server_id.clone(),
                    },
                    ImportQueueEvent::FileCompleted { file_id, server_id } => EventKind::File {
                        file_id: *file_id,
                        object_id: server_id.clone(),
                    },
                    ImportQueueEvent::FileSavedLocally(file_id) => {
                        EventKind::FileSavedLocally(*file_id)
                    }
                };
                events_for_subscription.borrow_mut().push(event);
            });
        });

        let parent_client_id = ClientId::new();
        let child_client_id = ClientId::new();
        let notebook_client_id = ClientId::new();
        let workflow_client_id = ClientId::new();
        let workflow_enum_client_id = ClientId::new();

        queue.update(&mut app, |queue, ctx| {
            queue.enqueue(
                ImportQueueArgs {
                    owner: local_owner(),
                    parent_id: ParentId::InitialFolder(None),
                    content: RequestContent::Folder {
                        name: "Imported".to_string(),
                        client_id: parent_client_id,
                        folder_id: nodes::FolderId::from(1),
                    },
                },
                ctx,
            );
            queue.enqueue(
                ImportQueueArgs {
                    owner: local_owner(),
                    parent_id: ParentId::FolderToUpload(parent_client_id),
                    content: RequestContent::Folder {
                        name: "Nested".to_string(),
                        client_id: child_client_id,
                        folder_id: nodes::FolderId::from(2),
                    },
                },
                ctx,
            );
            queue.enqueue(
                ImportQueueArgs {
                    owner: local_owner(),
                    parent_id: ParentId::FolderToUpload(child_client_id),
                    content: RequestContent::Notebook {
                        title: "Imported notes".to_string(),
                        data: "hello".to_string(),
                        client_id: notebook_client_id,
                        file_id: FileId(0),
                    },
                },
                ctx,
            );
            queue.enqueue(
                ImportQueueArgs {
                    owner: local_owner(),
                    parent_id: ParentId::FolderToUpload(child_client_id),
                    content: RequestContent::Workflow {
                        workflows: vec![(
                            crate::workflows::workflow::Workflow::new(
                                "Imported workflow",
                                "echo imported",
                            ),
                            workflow_client_id,
                        )],
                        workflow_enums: HashMap::from([(
                            workflow_enum_client_id,
                            WorkflowEnum {
                                name: "Environment".to_string(),
                                is_shared: false,
                                variants: EnumVariants::Static(vec!["dev".to_string()]),
                            },
                        )]),
                        file_id: FileId(1),
                    },
                },
                ctx,
            );
        });

        let parent_id = SyncId::ClientId(parent_client_id);
        let child_id = SyncId::ClientId(child_client_id);
        let notebook_id = SyncId::ClientId(notebook_client_id);
        let workflow_id = SyncId::ClientId(workflow_client_id);
        let workflow_enum_id = SyncId::ClientId(workflow_enum_client_id);

        app.update(|ctx| {
            let cloud_model = CloudModel::as_ref(ctx);
            let parent = cloud_model.get_folder(&parent_id).expect("parent folder");
            assert_eq!(parent.permissions.owner, local_owner());
            assert_eq!(
                cloud_model
                    .get_folder(&child_id)
                    .unwrap()
                    .metadata
                    .folder_id,
                Some(parent_id)
            );
            assert_eq!(
                cloud_model
                    .get_notebook(&notebook_id)
                    .unwrap()
                    .metadata
                    .folder_id,
                Some(child_id)
            );
            assert_eq!(
                cloud_model
                    .get_workflow(&workflow_id)
                    .unwrap()
                    .metadata
                    .folder_id,
                Some(child_id)
            );
            assert_eq!(
                cloud_model
                    .get_workflow_enum(&workflow_enum_id)
                    .unwrap()
                    .model()
                    .string_model
                    .name,
                "Environment"
            );
        });

        let events = events.borrow();
        assert!(events.contains(&EventKind::Folder {
            folder_id: nodes::FolderId::from(1),
            object_id: Some(parent_id.uid()),
        }));
        assert!(events.contains(&EventKind::Folder {
            folder_id: nodes::FolderId::from(2),
            object_id: Some(child_id.uid()),
        }));
        assert!(events.contains(&EventKind::File {
            file_id: FileId(0),
            object_id: Some(notebook_id.uid()),
        }));
        assert!(events.contains(&EventKind::File {
            file_id: FileId(1),
            object_id: Some(workflow_id.uid()),
        }));
    });
}
