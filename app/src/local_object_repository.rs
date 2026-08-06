use std::sync::mpsc::SyncSender;

use chrono::Utc;
use galaxy_graphql::scalars::time::ServerTimestamp;
use galaxyui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::ai::execution_profiles::{
    AIExecutionProfile, CloudAIExecutionProfile, CloudAIExecutionProfileModel,
};
use crate::ai::facts::{AIFact, CloudAIFact, CloudAIFactModel};
use crate::ai::mcp::templatable::{
    CloudTemplatableMCPServer, CloudTemplatableMCPServerModel, TemplatableMCPServer,
};
use crate::auth::UserUid;
use crate::cloud_object::model::generic_string_model::GenericStringObjectId;
use crate::cloud_object::model::persistence::{CloudModel, CloudModelEvent};
use crate::cloud_object::{
    CloudObject, CloudObjectLocation, CloudObjectMetadata, CloudObjectPermissions,
    CloudObjectStatuses, CloudObjectSyncStatus, GenericCloudObject, GenericStringObjectFormat,
    JsonObjectType, ObjectIdType, Owner, Revision, Space,
};
use crate::drive::folders::{CloudFolder, CloudFolderModel};
use crate::drive::CloudObjectTypeAndId;
use crate::env_vars::{CloudEnvVarCollection, CloudEnvVarCollectionModel, EnvVarCollection};
use crate::notebooks::{CloudNotebook, CloudNotebookModel};
use crate::persistence::ModelEvent;
use crate::server::ids::{ClientId, SyncId};
use crate::workflows::workflow::Workflow;
use crate::workflows::workflow_enum::{CloudWorkflowEnum, CloudWorkflowEnumModel, WorkflowEnum};
use crate::workflows::{CloudWorkflow, CloudWorkflowModel};

const LOCAL_OWNER_ID: &str = "local-galaxy-user";

/// Transitional local representation of a rule.
///
/// The SQLite schema still stores rules in the legacy generic-string-object
/// rows, so this alias preserves restart compatibility while callers move to
/// the local repository API.
pub type LocalRule = CloudAIFact;

/// Transitional local representation of an execution profile.
///
/// Execution profiles already live in the generic-string-object SQLite table.
/// Keeping that representation lets Galaxy adopt existing profiles without a
/// destructive database migration while removing their network sync path.
pub type LocalExecutionProfile = CloudAIExecutionProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalObjectRepositoryEvent {
    Rules,
    ExecutionProfiles { id: Option<SyncId> },
    Notebooks { id: Option<SyncId> },
    Workflows { id: Option<SyncId> },
}

/// Galaxy-owned access to locally persisted objects.
///
/// During the storage migration this repository writes the existing SQLite
/// generic object rows and mirrors changes into `CloudModel` for consumers that
/// have not moved yet. It never enqueues network work or requires an account.
pub struct LocalObjectRepository {
    model_event_sender: Option<SyncSender<ModelEvent>>,
    legacy_profile_owner: Option<Owner>,
}

impl LocalObjectRepository {
    pub fn new(
        model_event_sender: Option<SyncSender<ModelEvent>>,
        legacy_profile_owner: Option<Owner>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let cloud_model = CloudModel::handle(ctx);
        ctx.subscribe_to_model(&cloud_model, |_, _, event, ctx| {
            if event_affects_rules(event) {
                ctx.emit(LocalObjectRepositoryEvent::Rules);
                ctx.notify();
            }
            if let Some(id) = execution_profile_change(event) {
                ctx.emit(LocalObjectRepositoryEvent::ExecutionProfiles { id });
                ctx.notify();
            }
            if let Some(id) = notebook_change(event) {
                ctx.emit(LocalObjectRepositoryEvent::Notebooks { id });
                ctx.notify();
            }
            if let Some(id) = workflow_change(event) {
                ctx.emit(LocalObjectRepositoryEvent::Workflows { id });
                ctx.notify();
            }
        });

        Self {
            model_event_sender,
            legacy_profile_owner,
        }
    }

    pub fn rules(&self, app: &AppContext) -> Vec<LocalRule> {
        let cloud_model = CloudModel::as_ref(app);
        cloud_model
            .get_all_objects_of_type::<GenericStringObjectId, CloudAIFactModel>()
            .filter(|rule| !rule.is_trashed(cloud_model))
            .cloned()
            .collect()
    }

    pub fn rule(&self, id: &SyncId, app: &AppContext) -> Option<LocalRule> {
        CloudModel::as_ref(app)
            .get_object_of_type::<GenericStringObjectId, CloudAIFactModel>(id)
            .cloned()
    }

    pub fn create_rule(&mut self, fact: AIFact, ctx: &mut ModelContext<Self>) -> SyncId {
        let id = SyncId::ClientId(ClientId::new());
        self.create_rule_with_id(id, fact, ctx);
        id
    }

    pub fn create_rule_with_id(&mut self, id: SyncId, fact: AIFact, ctx: &mut ModelContext<Self>) {
        let rule = new_local_rule(id, fact);
        self.upsert_rule(rule, ctx);
    }

    pub fn update_rule(&mut self, id: SyncId, fact: AIFact, ctx: &mut ModelContext<Self>) -> bool {
        let Some(mut rule) = self.rule(&id, ctx) else {
            return false;
        };

        rule.set_model(CloudAIFactModel::new(fact));
        set_locally_saved_metadata(&mut rule.metadata);
        self.upsert_rule(rule, ctx);
        true
    }

    pub fn delete_rule(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) -> bool {
        if self.rule(&id, ctx).is_none() {
            return false;
        }

        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.delete_object(id, ctx);
        });
        self.save(ModelEvent::DeleteObjects {
            ids: vec![(id, ObjectIdType::GenericStringObject)],
        });
        true
    }

    pub fn execution_profiles(&self, app: &AppContext) -> Vec<LocalExecutionProfile> {
        let cloud_model = CloudModel::as_ref(app);
        cloud_model
            .get_all_objects_of_type::<GenericStringObjectId, CloudAIExecutionProfileModel>()
            .filter(|profile| {
                !profile.is_trashed(cloud_model) && self.is_local_execution_profile(profile)
            })
            .cloned()
            .collect()
    }

    pub fn execution_profile(
        &self,
        id: &SyncId,
        app: &AppContext,
    ) -> Option<LocalExecutionProfile> {
        let cloud_model = CloudModel::as_ref(app);
        cloud_model
            .get_object_of_type::<GenericStringObjectId, CloudAIExecutionProfileModel>(id)
            .filter(|profile| {
                !profile.is_trashed(cloud_model) && self.is_local_execution_profile(profile)
            })
            .cloned()
    }

    pub fn create_execution_profile_with_id(
        &mut self,
        id: SyncId,
        profile: AIExecutionProfile,
        ctx: &mut ModelContext<Self>,
    ) {
        let profile = new_local_execution_profile(id, profile);
        self.upsert_execution_profile(profile, ctx);
    }

    pub fn update_execution_profile(
        &mut self,
        id: SyncId,
        profile: AIExecutionProfile,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut object) = self.execution_profile(&id, ctx) else {
            return false;
        };

        object.set_model(CloudAIExecutionProfileModel::new(profile));
        set_locally_saved_metadata(&mut object.metadata);
        self.upsert_execution_profile(object, ctx);
        true
    }

    pub fn delete_execution_profile(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) -> bool {
        if self.execution_profile(&id, ctx).is_none() {
            return false;
        }

        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.delete_object(id, ctx);
        });
        self.save(ModelEvent::DeleteObjects {
            ids: vec![(id, ObjectIdType::GenericStringObject)],
        });
        true
    }

    pub fn notebook(&self, id: &SyncId, app: &AppContext) -> Option<CloudNotebook> {
        CloudModel::as_ref(app).get_notebook(id).cloned()
    }

    pub fn folder(&self, id: &SyncId, app: &AppContext) -> Option<CloudFolder> {
        CloudModel::as_ref(app).get_folder(id).cloned()
    }

    pub fn create_folder_with_id(
        &mut self,
        id: SyncId,
        name: String,
        parent_folder_id: Option<SyncId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.upsert_folder(
            new_local_folder(id, parent_folder_id, CloudFolderModel::new(&name, false)),
            ctx,
        );
    }

    pub fn update_folder_name(
        &mut self,
        id: SyncId,
        name: String,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut folder) = self.folder(&id, ctx) else {
            return false;
        };
        folder.set_model(CloudFolderModel {
            name,
            is_open: folder.model().is_open,
            is_warp_pack: folder.model().is_warp_pack,
        });
        set_locally_saved_metadata(&mut folder.metadata);
        self.upsert_folder(folder, ctx);
        true
    }

    pub fn set_folder_trashed(
        &mut self,
        id: SyncId,
        trashed: bool,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut folder) = self.folder(&id, ctx) else {
            return false;
        };
        folder.metadata.trashed_ts = trashed.then(|| ServerTimestamp::new(Utc::now()));
        set_locally_saved_metadata(&mut folder.metadata);
        self.upsert_folder(folder, ctx);
        true
    }

    pub fn delete_folder(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) -> bool {
        if self.folder(&id, ctx).is_none() {
            return false;
        }

        let ids = CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.delete_object_and_descendants(id.uid(), ctx)
        });
        if ids.is_empty() {
            return false;
        }
        self.save(ModelEvent::DeleteObjects { ids });
        true
    }

    pub fn open_folder(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) {
        self.set_folder_open_state(id, true, ctx);
    }

    pub fn close_folder(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) {
        self.set_folder_open_state(id, false, ctx);
    }

    pub fn toggle_folder_open(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) {
        let Some(folder) = self.folder(&id, ctx) else {
            return;
        };
        self.set_folder_open_state(id, !folder.model().is_open, ctx);
    }

    pub fn collapse_local_folders_in_location(
        &mut self,
        location: CloudObjectLocation,
        ctx: &mut ModelContext<Self>,
    ) {
        let folder_ids = {
            let cloud_model = CloudModel::as_ref(ctx);
            cloud_model
                .get_all_active_and_inactive_folders()
                .filter(|folder| folder.permissions.owner == local_owner())
                .filter(|folder| match location {
                    CloudObjectLocation::Folder(parent_id) => {
                        if folder.id == parent_id {
                            return true;
                        }

                        let mut ancestor_id = folder.metadata.folder_id;
                        while let Some(id) = ancestor_id {
                            if id == parent_id {
                                return true;
                            }
                            ancestor_id = cloud_model
                                .get_folder(&id)
                                .and_then(|ancestor| ancestor.metadata.folder_id);
                        }
                        false
                    }
                    CloudObjectLocation::Space(Space::Personal) => true,
                    CloudObjectLocation::Space(Space::Shared)
                    | CloudObjectLocation::Space(Space::Team { .. })
                    | CloudObjectLocation::Trash => false,
                })
                .map(|folder| folder.id)
                .collect::<Vec<_>>()
        };

        for folder_id in folder_ids {
            self.close_folder(folder_id, ctx);
        }
    }

    fn set_folder_open_state(&self, id: SyncId, is_open: bool, ctx: &mut ModelContext<Self>) {
        let Some(mut folder) = self.folder(&id, ctx) else {
            return;
        };
        folder.set_model(CloudFolderModel {
            name: folder.model().name.clone(),
            is_open,
            is_warp_pack: folder.model().is_warp_pack,
        });
        self.upsert_folder(folder, ctx);
    }

    pub fn create_notebook_with_id(
        &mut self,
        id: SyncId,
        folder_id: Option<SyncId>,
        notebook: CloudNotebookModel,
        ctx: &mut ModelContext<Self>,
    ) {
        self.upsert_notebook(new_local_notebook(id, folder_id, notebook), ctx);
    }

    pub fn update_notebook(
        &mut self,
        id: SyncId,
        notebook: CloudNotebookModel,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut object) = self.notebook(&id, ctx) else {
            return false;
        };
        object.set_model(notebook);
        set_locally_saved_metadata(&mut object.metadata);
        self.upsert_notebook(object, ctx);
        true
    }

    pub fn update_notebook_data(
        &mut self,
        id: SyncId,
        data: String,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(notebook) = self.notebook(&id, ctx) else {
            return false;
        };
        self.update_notebook(
            id,
            CloudNotebookModel {
                title: notebook.model().title.clone(),
                data,
                ai_document_id: notebook.model().ai_document_id,
                conversation_id: notebook.model().conversation_id.clone(),
            },
            ctx,
        )
    }

    pub fn update_notebook_title(
        &mut self,
        id: SyncId,
        title: String,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(notebook) = self.notebook(&id, ctx) else {
            return false;
        };
        self.update_notebook(
            id,
            CloudNotebookModel {
                title,
                data: notebook.model().data.clone(),
                ai_document_id: notebook.model().ai_document_id,
                conversation_id: notebook.model().conversation_id.clone(),
            },
            ctx,
        )
    }

    pub fn delete_notebook(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) -> bool {
        if self.notebook(&id, ctx).is_none() {
            return false;
        }
        self.delete_local_object(id, ObjectIdType::Notebook, ctx)
    }

    pub fn duplicate_notebook(
        &mut self,
        id: SyncId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<SyncId> {
        let source = self.notebook(&id, ctx)?;
        let cloud_model = CloudModel::as_ref(ctx);
        let mut model = source.model().clone();
        model.title = next_duplicate_name(&model.title, |candidate| {
            cloud_model
                .get_all_active_notebooks()
                .any(|notebook| notebook.model().title == candidate)
        });
        let duplicate_id = SyncId::ClientId(ClientId::new());
        self.create_notebook_with_id(duplicate_id, source.metadata.folder_id, model, ctx);
        Some(duplicate_id)
    }

    pub fn set_notebook_trashed(
        &mut self,
        id: SyncId,
        trashed: bool,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut notebook) = self.notebook(&id, ctx) else {
            return false;
        };
        notebook.metadata.trashed_ts = trashed.then(|| ServerTimestamp::new(Utc::now()));
        set_locally_saved_metadata(&mut notebook.metadata);
        self.upsert_notebook(notebook, ctx);
        true
    }

    pub fn workflow(&self, id: &SyncId, app: &AppContext) -> Option<CloudWorkflow> {
        CloudModel::as_ref(app).get_workflow(id).cloned()
    }

    pub fn env_var_collection(
        &self,
        id: &SyncId,
        app: &AppContext,
    ) -> Option<CloudEnvVarCollection> {
        CloudModel::as_ref(app)
            .get_object_of_type::<GenericStringObjectId, CloudEnvVarCollectionModel>(id)
            .cloned()
    }

    pub fn create_env_var_collection_with_id(
        &mut self,
        id: SyncId,
        folder_id: Option<SyncId>,
        collection: EnvVarCollection,
        ctx: &mut ModelContext<Self>,
    ) {
        self.upsert_generic_string_object(
            GenericCloudObject::new(
                id,
                CloudEnvVarCollectionModel::new(collection),
                locally_saved_metadata(folder_id),
                local_permissions(),
            ),
            ctx,
        );
    }

    pub fn update_env_var_collection(
        &mut self,
        id: SyncId,
        collection: EnvVarCollection,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut object) = self.env_var_collection(&id, ctx) else {
            return false;
        };
        object.set_model(CloudEnvVarCollectionModel::new(collection));
        set_locally_saved_metadata(&mut object.metadata);
        self.upsert_generic_string_object(object, ctx);
        true
    }

    pub fn delete_env_var_collection(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) -> bool {
        if self.env_var_collection(&id, ctx).is_none() {
            return false;
        }
        self.delete_local_object(id, ObjectIdType::GenericStringObject, ctx)
    }

    pub fn set_env_var_collection_trashed(
        &mut self,
        id: SyncId,
        trashed: bool,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut object) = self.env_var_collection(&id, ctx) else {
            return false;
        };
        object.metadata.trashed_ts = trashed.then(|| ServerTimestamp::new(Utc::now()));
        set_locally_saved_metadata(&mut object.metadata);
        self.upsert_generic_string_object(object, ctx);
        true
    }

    pub fn duplicate_env_var_collection(
        &mut self,
        id: SyncId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<SyncId> {
        let source = self.env_var_collection(&id, ctx)?;
        let cloud_model = CloudModel::as_ref(ctx);
        let mut collection = source.model().string_model.clone();
        let title = collection.title.clone().unwrap_or_default();
        let name = next_duplicate_name(title.as_str(), |candidate| {
            cloud_model
                .get_all_active_env_var_collections()
                .any(|collection| {
                    collection.model().string_model.title.as_deref() == Some(candidate)
                })
        });
        collection.title = Some(name);
        let duplicate_id = SyncId::ClientId(ClientId::new());
        self.create_env_var_collection_with_id(
            duplicate_id,
            source.metadata.folder_id,
            collection,
            ctx,
        );
        Some(duplicate_id)
    }

    pub fn templatable_mcp_server(
        &self,
        uuid: uuid::Uuid,
        app: &AppContext,
    ) -> Option<CloudTemplatableMCPServer> {
        CloudModel::as_ref(app)
            .get_all_objects_of_type::<GenericStringObjectId, CloudTemplatableMCPServerModel>()
            .find(|server| server.model().string_model.uuid == uuid)
            .cloned()
    }

    pub fn create_templatable_mcp_server_with_id(
        &mut self,
        id: SyncId,
        server: TemplatableMCPServer,
        ctx: &mut ModelContext<Self>,
    ) {
        self.upsert_templatable_mcp_server(
            GenericCloudObject::new(
                id,
                CloudTemplatableMCPServerModel::new(server),
                locally_saved_metadata(None),
                local_permissions(),
            ),
            ctx,
        );
    }

    pub fn update_templatable_mcp_server(
        &mut self,
        uuid: uuid::Uuid,
        server: TemplatableMCPServer,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut object) = self.templatable_mcp_server(uuid, ctx) else {
            return false;
        };
        object.set_model(CloudTemplatableMCPServerModel::new(server));
        set_locally_saved_metadata(&mut object.metadata);
        self.upsert_templatable_mcp_server(object, ctx);
        true
    }

    pub fn delete_templatable_mcp_server(
        &mut self,
        uuid: uuid::Uuid,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(object) = self.templatable_mcp_server(uuid, ctx) else {
            return false;
        };
        self.delete_local_object(object.id, ObjectIdType::GenericStringObject, ctx)
    }

    pub fn create_workflow_with_id(
        &mut self,
        id: SyncId,
        folder_id: Option<SyncId>,
        workflow: Workflow,
        ctx: &mut ModelContext<Self>,
    ) {
        self.upsert_workflow(new_local_workflow(id, folder_id, workflow), ctx);
    }

    pub fn create_workflow_enum_with_id(
        &mut self,
        id: SyncId,
        workflow_enum: WorkflowEnum,
        ctx: &mut ModelContext<Self>,
    ) {
        self.upsert_workflow_enum(
            GenericCloudObject::new(
                id,
                CloudWorkflowEnumModel::new(workflow_enum),
                locally_saved_metadata(None),
                local_permissions(),
            ),
            ctx,
        );
    }

    pub fn update_workflow(
        &mut self,
        id: SyncId,
        workflow: Workflow,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut object) = self.workflow(&id, ctx) else {
            return false;
        };
        object.set_model(CloudWorkflowModel::new(workflow));
        set_locally_saved_metadata(&mut object.metadata);
        self.upsert_workflow(object, ctx);
        true
    }

    pub fn delete_workflow(&mut self, id: SyncId, ctx: &mut ModelContext<Self>) -> bool {
        if self.workflow(&id, ctx).is_none() {
            return false;
        }
        self.delete_local_object(id, ObjectIdType::Workflow, ctx)
    }

    pub fn duplicate_workflow(
        &mut self,
        id: SyncId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<SyncId> {
        let source = self.workflow(&id, ctx)?;
        let cloud_model = CloudModel::as_ref(ctx);
        let mut workflow = source.model().data.clone();
        let name = next_duplicate_name(workflow.name(), |candidate| {
            cloud_model
                .get_all_active_workflows()
                .any(|workflow| workflow.model().data.name() == candidate)
        });
        workflow.set_name(&name);
        let duplicate_id = SyncId::ClientId(ClientId::new());
        self.create_workflow_with_id(duplicate_id, source.metadata.folder_id, workflow, ctx);
        Some(duplicate_id)
    }

    pub fn set_workflow_trashed(
        &mut self,
        id: SyncId,
        trashed: bool,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(mut workflow) = self.workflow(&id, ctx) else {
            return false;
        };
        workflow.metadata.trashed_ts = trashed.then(|| ServerTimestamp::new(Utc::now()));
        set_locally_saved_metadata(&mut workflow.metadata);
        self.upsert_workflow(workflow, ctx);
        true
    }

    fn upsert_rule(&self, rule: LocalRule, ctx: &mut ModelContext<Self>) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(rule.clone(), ctx);
        });
        self.save(ModelEvent::UpsertGenericStringObject {
            object: Box::new(rule),
        });
    }

    fn upsert_execution_profile(
        &self,
        profile: LocalExecutionProfile,
        ctx: &mut ModelContext<Self>,
    ) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(profile.clone(), ctx);
        });
        self.save(ModelEvent::UpsertGenericStringObject {
            object: Box::new(profile),
        });
    }

    fn upsert_generic_string_object(
        &self,
        object: GenericCloudObject<GenericStringObjectId, CloudEnvVarCollectionModel>,
        ctx: &mut ModelContext<Self>,
    ) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(object.clone(), ctx);
        });
        self.save(ModelEvent::UpsertGenericStringObject {
            object: Box::new(object),
        });
    }

    fn upsert_templatable_mcp_server(
        &self,
        object: CloudTemplatableMCPServer,
        ctx: &mut ModelContext<Self>,
    ) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(object.clone(), ctx);
        });
        self.save(ModelEvent::UpsertGenericStringObject {
            object: Box::new(object),
        });
    }

    fn upsert_notebook(&self, notebook: CloudNotebook, ctx: &mut ModelContext<Self>) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(notebook.clone(), ctx);
        });
        self.save(ModelEvent::UpsertNotebook { notebook });
    }

    fn upsert_folder(&self, folder: CloudFolder, ctx: &mut ModelContext<Self>) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(folder.clone(), ctx);
        });
        self.save(ModelEvent::UpsertFolder { folder });
    }

    fn upsert_workflow(&self, workflow: CloudWorkflow, ctx: &mut ModelContext<Self>) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(workflow.clone(), ctx);
        });
        self.save(ModelEvent::UpsertWorkflow { workflow });
    }

    fn upsert_workflow_enum(&self, workflow_enum: CloudWorkflowEnum, ctx: &mut ModelContext<Self>) {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.upsert_local_object(workflow_enum.clone(), ctx);
        });
        self.save(ModelEvent::UpsertGenericStringObject {
            object: Box::new(workflow_enum),
        });
    }

    fn delete_local_object(
        &self,
        id: SyncId,
        object_type: ObjectIdType,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        CloudModel::handle(ctx).update(ctx, |cloud_model, ctx| {
            cloud_model.delete_object(id, ctx);
        });
        self.save(ModelEvent::DeleteObjects {
            ids: vec![(id, object_type)],
        });
        true
    }

    fn is_local_execution_profile(&self, profile: &LocalExecutionProfile) -> bool {
        profile.permissions().owner == local_owner()
            || self
                .legacy_profile_owner
                .is_some_and(|owner| profile.permissions().owner == owner)
    }

    fn save(&self, event: ModelEvent) {
        let Some(sender) = &self.model_event_sender else {
            return;
        };
        if let Err(error) = sender.send(event) {
            log::error!("Failed to save local object: {error}");
        }
    }
}

impl Entity for LocalObjectRepository {
    type Event = LocalObjectRepositoryEvent;
}

impl SingletonEntity for LocalObjectRepository {}

pub(crate) fn new_local_rule(id: SyncId, fact: AIFact) -> LocalRule {
    GenericCloudObject::new(
        id,
        CloudAIFactModel::new(fact),
        locally_saved_metadata(None),
        local_permissions(),
    )
}

pub(crate) fn new_local_notebook(
    id: SyncId,
    folder_id: Option<SyncId>,
    notebook: CloudNotebookModel,
) -> CloudNotebook {
    CloudNotebook::new(
        id,
        notebook,
        locally_saved_metadata(folder_id),
        local_permissions(),
    )
}

pub(crate) fn new_local_folder(
    id: SyncId,
    folder_id: Option<SyncId>,
    folder: CloudFolderModel,
) -> CloudFolder {
    CloudFolder::new(
        id,
        folder,
        locally_saved_metadata(folder_id),
        local_permissions(),
    )
}

pub(crate) fn new_local_workflow(
    id: SyncId,
    folder_id: Option<SyncId>,
    workflow: Workflow,
) -> CloudWorkflow {
    CloudWorkflow::new(
        id,
        CloudWorkflowModel::new(workflow),
        locally_saved_metadata(folder_id),
        local_permissions(),
    )
}

fn new_local_execution_profile(id: SyncId, profile: AIExecutionProfile) -> LocalExecutionProfile {
    GenericCloudObject::new(
        id,
        CloudAIExecutionProfileModel::new(profile),
        locally_saved_metadata(None),
        local_permissions(),
    )
}

pub(crate) fn local_owner() -> Owner {
    Owner::User {
        user_uid: UserUid::new(LOCAL_OWNER_ID),
    }
}

fn local_permissions() -> CloudObjectPermissions {
    CloudObjectPermissions {
        owner: local_owner(),
        permissions_last_updated_ts: None,
        anyone_with_link: None,
        guests: Vec::new(),
    }
}

fn locally_saved_metadata(folder_id: Option<SyncId>) -> CloudObjectMetadata {
    let now = ServerTimestamp::new(Utc::now());
    CloudObjectMetadata {
        revision: Some(Revision::from(now)),
        metadata_last_updated_ts: Some(now),
        current_editor_uid: None,
        pending_changes_statuses: locally_saved_statuses(),
        trashed_ts: None,
        folder_id,
        is_welcome_object: false,
        last_editor_uid: None,
        creator_uid: None,
        last_task_run_ts: None,
    }
}

fn set_locally_saved_metadata(metadata: &mut CloudObjectMetadata) {
    let now = ServerTimestamp::new(Utc::now());
    metadata.revision = Some(Revision::from(now));
    metadata.metadata_last_updated_ts = Some(now);
    metadata.pending_changes_statuses = locally_saved_statuses();
}

fn locally_saved_statuses() -> CloudObjectStatuses {
    CloudObjectStatuses {
        content_sync_status: CloudObjectSyncStatus::NoLocalChanges,
        has_pending_metadata_change: false,
        has_pending_permissions_change: false,
        pending_untrash: false,
        pending_delete: false,
    }
}

fn next_duplicate_name(original_name: &str, mut exists: impl FnMut(&str) -> bool) -> String {
    let mut copy_number = 1;
    loop {
        let candidate = format!("{original_name} ({copy_number})");
        if !exists(&candidate) {
            return candidate;
        }
        copy_number += 1;
    }
}

fn event_affects_rules(event: &CloudModelEvent) -> bool {
    match event {
        CloudModelEvent::ObjectMoved { type_and_id, .. }
        | CloudModelEvent::ObjectUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectTrashed { type_and_id, .. }
        | CloudModelEvent::ObjectUntrashed { type_and_id, .. }
        | CloudModelEvent::ObjectCreated { type_and_id }
        | CloudModelEvent::ObjectDeleted { type_and_id, .. }
        | CloudModelEvent::ObjectPermissionsUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectSynced { type_and_id, .. } => is_rule_type(type_and_id),
        CloudModelEvent::InitialLoadCompleted => true,
        CloudModelEvent::NotebookEditorChangedFromServer { .. }
        | CloudModelEvent::ObjectForceExpanded { .. } => false,
    }
}

fn execution_profile_change(event: &CloudModelEvent) -> Option<Option<SyncId>> {
    match event {
        CloudModelEvent::ObjectMoved { type_and_id, .. }
        | CloudModelEvent::ObjectUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectTrashed { type_and_id, .. }
        | CloudModelEvent::ObjectUntrashed { type_and_id, .. }
        | CloudModelEvent::ObjectCreated { type_and_id }
        | CloudModelEvent::ObjectDeleted { type_and_id, .. }
        | CloudModelEvent::ObjectPermissionsUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectSynced { type_and_id, .. } => match type_and_id {
            CloudObjectTypeAndId::GenericStringObject {
                object_type: GenericStringObjectFormat::Json(JsonObjectType::AIExecutionProfile),
                id,
            } => Some(Some(*id)),
            CloudObjectTypeAndId::Notebook(_)
            | CloudObjectTypeAndId::Workflow(_)
            | CloudObjectTypeAndId::Folder(_)
            | CloudObjectTypeAndId::GenericStringObject { .. } => None,
        },
        CloudModelEvent::InitialLoadCompleted => Some(None),
        CloudModelEvent::NotebookEditorChangedFromServer { .. }
        | CloudModelEvent::ObjectForceExpanded { .. } => None,
    }
}

fn notebook_change(event: &CloudModelEvent) -> Option<Option<SyncId>> {
    match event {
        CloudModelEvent::ObjectMoved { type_and_id, .. }
        | CloudModelEvent::ObjectUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectTrashed { type_and_id, .. }
        | CloudModelEvent::ObjectUntrashed { type_and_id, .. }
        | CloudModelEvent::ObjectCreated { type_and_id }
        | CloudModelEvent::ObjectDeleted { type_and_id, .. }
        | CloudModelEvent::ObjectPermissionsUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectSynced { type_and_id, .. } => match type_and_id {
            CloudObjectTypeAndId::Notebook(id) => Some(Some(*id)),
            CloudObjectTypeAndId::Workflow(_)
            | CloudObjectTypeAndId::Folder(_)
            | CloudObjectTypeAndId::GenericStringObject { .. } => None,
        },
        CloudModelEvent::InitialLoadCompleted => Some(None),
        CloudModelEvent::NotebookEditorChangedFromServer { .. }
        | CloudModelEvent::ObjectForceExpanded { .. } => None,
    }
}

fn workflow_change(event: &CloudModelEvent) -> Option<Option<SyncId>> {
    match event {
        CloudModelEvent::ObjectMoved { type_and_id, .. }
        | CloudModelEvent::ObjectUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectTrashed { type_and_id, .. }
        | CloudModelEvent::ObjectUntrashed { type_and_id, .. }
        | CloudModelEvent::ObjectCreated { type_and_id }
        | CloudModelEvent::ObjectDeleted { type_and_id, .. }
        | CloudModelEvent::ObjectPermissionsUpdated { type_and_id, .. }
        | CloudModelEvent::ObjectSynced { type_and_id, .. } => match type_and_id {
            CloudObjectTypeAndId::Workflow(id) => Some(Some(*id)),
            CloudObjectTypeAndId::Notebook(_)
            | CloudObjectTypeAndId::Folder(_)
            | CloudObjectTypeAndId::GenericStringObject { .. } => None,
        },
        CloudModelEvent::InitialLoadCompleted => Some(None),
        CloudModelEvent::NotebookEditorChangedFromServer { .. }
        | CloudModelEvent::ObjectForceExpanded { .. } => None,
    }
}

fn is_rule_type(type_and_id: &CloudObjectTypeAndId) -> bool {
    matches!(
        type_and_id,
        CloudObjectTypeAndId::GenericStringObject {
            object_type: GenericStringObjectFormat::Json(JsonObjectType::AIFact),
            ..
        }
    )
}

#[cfg(test)]
#[path = "local_object_repository_tests.rs"]
mod tests;
