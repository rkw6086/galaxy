use galaxyui::{Entity, ModelContext, SingletonEntity};

use super::CloudEnvVarCollectionModel;
use crate::cloud_object::breadcrumbs::ContainingObject;
use crate::cloud_object::model::persistence::CloudModelEvent;
use crate::cloud_object::{CloudObject, Owner, Revision, Space};
use crate::env_vars::CloudEnvVarCollection;
use crate::server::ids::{ClientId, SyncId};
use crate::{AppContext, CloudModel};

#[derive(Default, Clone)]
pub enum ActiveEnvVarCollection {
    #[default]
    None,
    // An EnvVarCollection already stored in CloudModel, all relevant data should be queried
    // from CloudModel directly
    CommittedEnvVarCollection(SyncId),
    // An EnvVarCollection that has been created and displayed in the view, but is not yet
    // committed to CloudModel
    NewEnvVarCollection(Box<CloudEnvVarCollection>),
}

#[derive(Default, PartialEq, Debug)]
pub enum SavingStatus {
    #[default]
    Saved,
    Unsaved,
    New,
}

#[derive(Default)]
pub struct ActiveEnvVarCollectionData {
    pub saving_status: SavingStatus,
    pub active_env_var_collection: ActiveEnvVarCollection,
    pub revision_ts: Option<Revision>,
}

impl ActiveEnvVarCollectionData {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let cloud_model = CloudModel::handle(ctx);

        ctx.subscribe_to_model(&cloud_model, |me, _, event, ctx| {
            me.handle_cloud_model_event(event, ctx);
        });

        Self {
            ..Default::default()
        }
    }

    fn handle_cloud_model_event(&mut self, event: &CloudModelEvent, ctx: &mut ModelContext<Self>) {
        if let CloudModelEvent::ObjectMoved { type_and_id, .. } = event {
            if let Some(env_var_collection_id) = type_and_id.as_generic_string_object_id() {
                if self.is_active_env_var_collection(env_var_collection_id) {
                    ctx.emit(ActiveEnvVarCollectionDataEvent::BreadcrumbsChanged)
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.active_env_var_collection = ActiveEnvVarCollection::None;
    }

    pub fn open_new(
        &mut self,
        owner: Owner,
        initial_folder_id: Option<SyncId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.reset();

        let new_id = ClientId::default();

        // Set the active env var collection to be an uncommitted collection
        self.active_env_var_collection = ActiveEnvVarCollection::NewEnvVarCollection(Box::new(
            CloudEnvVarCollection::new_local(
                CloudEnvVarCollectionModel::default(),
                owner,
                initial_folder_id,
                new_id,
            ),
        ));

        ctx.emit(ActiveEnvVarCollectionDataEvent::BreadcrumbsChanged);
        ctx.notify();
    }

    pub fn open_existing(&mut self, env_var_collection_id: SyncId, ctx: &mut ModelContext<Self>) {
        self.reset();
        self.saving_status = SavingStatus::Saved;
        self.active_env_var_collection =
            ActiveEnvVarCollection::CommittedEnvVarCollection(env_var_collection_id);

        ctx.emit(ActiveEnvVarCollectionDataEvent::BreadcrumbsChanged);
        ctx.notify();
    }

    pub fn id(&self) -> Option<SyncId> {
        match &self.active_env_var_collection {
            ActiveEnvVarCollection::None => None,
            ActiveEnvVarCollection::CommittedEnvVarCollection(id) => Some(*id),
            ActiveEnvVarCollection::NewEnvVarCollection(env_var_collection) => {
                Some(env_var_collection.id)
            }
        }
    }

    /// The current user's access level on this env var collection.
    pub fn access_level(&self, _app: &AppContext) -> crate::drive::sharing::SharingAccessLevel {
        crate::drive::sharing::SharingAccessLevel::Full
    }

    pub fn editability(&self, _app: &AppContext) -> crate::drive::sharing::ContentEditability {
        crate::drive::sharing::ContentEditability::Editable
    }

    /// The space that this env var collection is in.
    pub fn space(&self, app: &AppContext) -> Option<Space> {
        match &self.active_env_var_collection {
            ActiveEnvVarCollection::None => None,
            ActiveEnvVarCollection::CommittedEnvVarCollection(sync_id) => CloudModel::as_ref(app)
                .get_env_var_collection(sync_id)
                .map(|collection| collection.space(app)),
            ActiveEnvVarCollection::NewEnvVarCollection(env_var_collection) => {
                Some(env_var_collection.space(app))
            }
        }
    }

    pub fn active_env_var_collection(&self) -> ActiveEnvVarCollection {
        self.active_env_var_collection.clone()
    }

    /// Whether or not the EVC has been persisted locally.
    pub fn is_on_server(&self) -> bool {
        matches!(
            &self.active_env_var_collection,
            ActiveEnvVarCollection::CommittedEnvVarCollection(_)
        )
    }

    pub fn is_active_env_var_collection(&self, env_var_collection_id: SyncId) -> bool {
        self.id() == Some(env_var_collection_id)
    }

    pub fn breadcrumbs(&self, ctx: &AppContext) -> Option<Vec<ContainingObject>> {
        let cloud_env_var_collection = match &self.active_env_var_collection {
            ActiveEnvVarCollection::None => None,
            ActiveEnvVarCollection::CommittedEnvVarCollection(id) => {
                CloudModel::as_ref(ctx).get_env_var_collection(id)
            }
            ActiveEnvVarCollection::NewEnvVarCollection(env_var_collection) => {
                Some(env_var_collection.as_ref())
            }
        };

        cloud_env_var_collection
            .map(|env_var_collection| env_var_collection.containing_objects_path(ctx))
    }

    pub fn trash_status(&self, ctx: &AppContext) -> TrashStatus {
        match &self.active_env_var_collection {
            ActiveEnvVarCollection::None | ActiveEnvVarCollection::NewEnvVarCollection(_) => {
                TrashStatus::Active
            }
            ActiveEnvVarCollection::CommittedEnvVarCollection(id) => {
                let cloud_model = CloudModel::as_ref(ctx);
                match cloud_model.get_env_var_collection(id) {
                    Some(env_var_collection) => {
                        if env_var_collection.is_trashed(cloud_model) {
                            TrashStatus::Trashed
                        } else {
                            TrashStatus::Active
                        }
                    }
                    None => TrashStatus::Deleted,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrashStatus {
    Active,
    Trashed,
    Deleted,
}

pub enum ActiveEnvVarCollectionDataEvent {
    /// The EVC's breadcrumbs were updated.
    BreadcrumbsChanged,
}

impl Entity for ActiveEnvVarCollectionData {
    type Event = ActiveEnvVarCollectionDataEvent;
}
