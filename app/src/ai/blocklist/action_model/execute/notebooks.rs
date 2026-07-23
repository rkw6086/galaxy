//! Executors for the `create_notebook`, `read_notebook`, and `edit_notebook` tools.
//!
//! These tools interact with Galaxy Drive CloudNotebook objects directly,
//! as opposed to the plan tools which work through AIDocumentModel.

use futures::future::BoxFuture;
use futures::FutureExt;
use galaxyui::{Entity, ModelContext, SingletonEntity};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};
use crate::ai::agent::{
    AIAgentAction, AIAgentActionType, CreateDocumentsRequest, CreateDocumentsResult,
    DocumentContext, EditDocumentsRequest, EditDocumentsResult, ReadDocumentsRequest,
    ReadDocumentsResult,
};
use crate::ai::document::ai_document_model::{AIDocumentId, AIDocumentVersion};
use crate::cloud_object::model::persistence::CloudModel;
use crate::notebooks::CloudNotebookModel;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::ids::ClientId;
use crate::workspaces::user_workspaces::UserWorkspaces;

pub struct NotebookExecutor;

impl NotebookExecutor {
    pub fn new() -> Self {
        Self
    }

    pub(super) fn should_autoexecute(
        &self,
        _input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> bool {
        true
    }

    pub(super) fn execute_create(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentAction {
            action: AIAgentActionType::CreateDocuments(CreateDocumentsRequest { documents }),
            ..
        } = action
        else {
            return ActionExecution::<CreateDocumentsResult>::InvalidAction;
        };

        let Some(owner) = UserWorkspaces::as_ref(ctx).personal_drive(ctx) else {
            return ActionExecution::Sync(
                CreateDocumentsResult::Error("No personal drive available.".to_string()).into(),
            );
        };

        let mut created = Vec::new();

        for document in documents {
            let client_id = ClientId::new();
            let document_id = AIDocumentId::new();
            let model = CloudNotebookModel {
                title: document.title.clone(),
                data: document.content.clone(),
                ai_document_id: Some(document_id),
                conversation_id: None,
            };

            UpdateManager::handle(ctx).update(ctx, |update_manager, ctx| {
                update_manager.create_notebook(
                    client_id,
                    owner,
                    None, // root of personal space
                    model,
                    crate::cloud_object::CloudObjectEventEntrypoint::Unknown,
                    false,
                    ctx,
                );
            });

            created.push(DocumentContext {
                document_id,
                document_version: AIDocumentVersion::default(),
                content: document.content.clone(),
                line_ranges: vec![],
            });
        }

        ActionExecution::Sync(
            CreateDocumentsResult::Success {
                created_documents: created,
            }
            .into(),
        )
    }

    pub(super) fn execute_read(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentAction {
            action: AIAgentActionType::ReadDocuments(ReadDocumentsRequest { document_ids }),
            ..
        } = action
        else {
            return ActionExecution::<ReadDocumentsResult>::InvalidAction;
        };

        let cloud_model = CloudModel::as_ref(ctx);
        let mut documents = Vec::new();

        for id in document_ids {
            // Look up the notebook by its ai_document_id field, falling back to SyncId matching
            let notebook = cloud_model
                .get_all_active_notebooks()
                .find(|nb| nb.model().ai_document_id.as_ref() == Some(id))
                .or_else(|| {
                    cloud_model
                        .get_all_active_notebooks()
                        .find(|nb| nb.id.uid() == id.to_string())
                });

            if let Some(notebook) = notebook {
                documents.push(DocumentContext {
                    document_id: *id,
                    document_version: AIDocumentVersion::default(),
                    content: format!("# {}\n\n{}", notebook.model().title, notebook.model().data),
                    line_ranges: vec![],
                });
            }
        }

        ActionExecution::Sync(ReadDocumentsResult::Success { documents }.into())
    }

    pub(super) fn execute_edit(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentAction {
            action: AIAgentActionType::EditDocuments(EditDocumentsRequest { diffs }),
            ..
        } = action
        else {
            return ActionExecution::<EditDocumentsResult>::InvalidAction;
        };

        let mut updated_documents = Vec::new();
        let mut errors = Vec::new();

        for diff in diffs {
            let cloud_model = CloudModel::as_ref(ctx);
            // Look up the notebook by its ai_document_id field, falling back to SyncId matching
            let notebook_data = cloud_model
                .get_all_active_notebooks()
                .find(|nb| nb.model().ai_document_id.as_ref() == Some(&diff.document_id))
                .or_else(|| {
                    cloud_model
                        .get_all_active_notebooks()
                        .find(|nb| nb.id.uid() == diff.document_id.to_string())
                })
                .map(|nb| (nb.id, nb.model().data.clone()));

            let Some((notebook_id, current_data)) = notebook_data else {
                errors.push(format!("Notebook {} not found.", diff.document_id));
                continue;
            };

            // Simple search/replace on the notebook data
            if !current_data.contains(&diff.search) {
                errors.push(format!(
                    "Could not find search text in notebook {}.",
                    diff.document_id
                ));
                continue;
            }

            let new_data = current_data.replacen(&diff.search, &diff.replace, 1);
            let new_data_arc = std::sync::Arc::new(new_data.clone());

            UpdateManager::handle(ctx).update(ctx, |update_manager, ctx| {
                update_manager.update_notebook_data(new_data_arc, notebook_id, ctx);
            });

            updated_documents.push(DocumentContext {
                document_id: diff.document_id,
                document_version: AIDocumentVersion::default(),
                content: new_data,
                line_ranges: vec![],
            });
        }

        if !errors.is_empty() {
            return ActionExecution::Sync(EditDocumentsResult::Error(errors.join("\n")).into());
        }

        ActionExecution::Sync(EditDocumentsResult::Success { updated_documents }.into())
    }

    pub(super) fn preprocess_action(
        &mut self,
        _input: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

impl Entity for NotebookExecutor {
    type Event = ();
}
