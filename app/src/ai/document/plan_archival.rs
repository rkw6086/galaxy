//! Retain completed plans in a recoverable Drive folder after one week.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use galaxy_core::user_preferences::GetUserPreferences as _;
use galaxyui::r#async::Timer;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use serde::{Deserialize, Serialize};

use super::ai_document_model::{
    AIDocumentId, AIDocumentModel, AIDocumentModelEvent, AIDocumentUpdateSource,
    AIDocumentUserEditStatus,
};
use crate::ai::agent::conversation::{AIConversation, AIConversationId, ConversationStatus};
use crate::ai::artifacts::Artifact;
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::cloud_object::model::persistence::{CloudModel, CloudModelEvent};
use crate::cloud_object::{CloudObject, CloudObjectLocation, Space};
use crate::drive::CloudObjectTypeAndId;
use crate::local_object_repository::{
    local_owner, LocalObjectRepository, LocalObjectRepositoryEvent,
};
use crate::server::ids::{ClientId, SyncId};

const PLAN_ARCHIVAL_KEY: &str = "CompletedPlanArchival";
const PLAN_FOLDER_NAME: &str = "Plans";
const ARCHIVE_FOLDER_NAME: &str = "Archive";
const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const ARCHIVE_AFTER_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CompletedPlan {
    conversation_id: AIConversationId,
    completed_at: DateTime<Utc>,
    #[serde(default)]
    archived_to: Option<SyncId>,
    #[serde(default)]
    original_folder: Option<SyncId>,
}

impl CompletedPlan {
    fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.archived_to.is_none()
            && now.signed_duration_since(self.completed_at)
                >= chrono::Duration::days(ARCHIVE_AFTER_DAYS)
    }
}

/// Completion records are local preferences; notebook contents and folder
/// membership continue to use the normal SQLite-backed Drive repository.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanArchivalModel {
    #[serde(default)]
    completed_plans: HashMap<String, CompletedPlan>,
    #[serde(default)]
    reopened_plans: HashMap<String, ReopenedPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReopenedPlan {
    conversation_id: AIConversationId,
    /// The edited plan must reach a new agent exchange before the old task
    /// list can be used as evidence of completion again.
    saw_new_exchange: bool,
}

impl PlanArchivalModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        let model = ctx
            .private_user_preferences()
            .read_value(PLAN_ARCHIVAL_KEY)
            .unwrap_or_default()
            .and_then(|json| Self::from_persisted(&json))
            .unwrap_or_default();

        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |model, _, event, ctx| match event {
                BlocklistAIHistoryEvent::AppendedExchange { conversation_id, .. } => {
                    // Clear completion before a successor turn can start. A
                    // previously archived plan becomes visible again as well.
                    model.begin_exchange(*conversation_id, ctx);
                }
                BlocklistAIHistoryEvent::UpdatedConversationStatus { .. }
                | BlocklistAIHistoryEvent::RestoredConversations { .. } => {
                    model.sweep(ctx);
                }
                _ => {}
            },
        );
        ctx.subscribe_to_model(&AIDocumentModel::handle(ctx), |model, _, event, ctx| {
            match event {
                AIDocumentModelEvent::DocumentUpdated {
                    document_id,
                    source: AIDocumentUpdateSource::User,
                    ..
                }
                | AIDocumentModelEvent::DocumentUserEditStatusUpdated {
                    document_id,
                    status: AIDocumentUserEditStatus::Dirty,
                } => model.reopen_document(*document_id, ctx),
                AIDocumentModelEvent::DocumentUserEditStatusUpdated {
                    document_id,
                    status: AIDocumentUserEditStatus::UpToDate,
                } => model.accept_document_edits(*document_id, ctx),
                _ => {}
            }
        });
        ctx.subscribe_to_model(&CloudModel::handle(ctx), |model, _, event, ctx| {
            if matches!(event, CloudModelEvent::InitialLoadCompleted) {
                model.sweep(ctx);
            }
        });
        ctx.subscribe_to_model(&LocalObjectRepository::handle(ctx), |model, _, event, ctx| {
            let LocalObjectRepositoryEvent::NotebookContentEdited { id } = event else {
                return;
            };
            let document_id = CloudModel::as_ref(ctx)
                .get_notebook(id)
                .and_then(|notebook| notebook.model().ai_document_id);
            if let Some(document_id) = document_id {
                model.reopen_document(document_id, ctx);
            }
        });

        // The first callback runs after singleton registration and session
        // restoration. Periodic checks also catch provider cleanup occurring
        // just after a successful status event, without delaying that cleanup.
        ctx.spawn(async {}, |model, _, ctx| {
            model.sweep(ctx);
            model.schedule_sweep(ctx);
        });

        model
    }

    fn from_persisted(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok().or_else(|| {
            // Preserve completion records written before reopened-plan state
            // was added to the preference payload.
            serde_json::from_str(json).ok().map(|completed_plans| Self {
                completed_plans,
                ..Self::default()
            })
        })
    }

    fn schedule_sweep(&self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async { Timer::after(SWEEP_INTERVAL).await },
            |model, _, ctx| {
                model.sweep(ctx);
                model.schedule_sweep(ctx);
            },
        );
    }

    fn sweep(&mut self, ctx: &mut ModelContext<Self>) {
        let before = self.completed_plans.clone();
        let reopened_before = self.reopened_plans.clone();
        self.reopened_plans.retain(|document_id, record| {
            !record.saw_new_exchange
                || AIDocumentId::try_from(document_id.as_str())
                    .ok()
                    .and_then(|id| AIDocumentModel::as_ref(ctx).get_current_document(&id))
                    .is_none_or(|document| document.user_edit_status.is_dirty())
        });
        let observations = {
            let history = BlocklistAIHistoryModel::as_ref(ctx);
            let documents = AIDocumentModel::as_ref(ctx);
            let mut seen = HashSet::new();
            history
                .all_live_conversations()
                .into_iter()
                .chain(history.all_cleared_conversations())
                .filter(|(_, conversation)| seen.insert(conversation.id()))
                .map(|(_, conversation)| {
                    let mut document_ids = documents
                        .get_all_documents_for_conversation(conversation.id())
                        .into_iter()
                        .map(|(id, _)| id.to_string())
                        .collect::<HashSet<_>>();
                    document_ids.extend(conversation.artifacts().iter().filter_map(
                        |artifact| {
                            if let Artifact::Plan { document_uid, .. } = artifact {
                                Some(document_uid.clone())
                            } else {
                                None
                            }
                        },
                    ));
                    (
                        conversation.id(),
                        document_ids,
                        completed_work_time(conversation),
                    )
                })
                .collect::<Vec<_>>()
        };

        for (conversation_id, document_ids, completed_at) in observations {
            match completed_at {
                Some(completed_at) => {
                    for document_id in document_ids {
                        self.observe_completion(document_id, conversation_id, completed_at);
                    }
                }
                None => self.resume_conversation(conversation_id, ctx),
            }
        }

        // Persisted completion records also cover unloaded conversations.
        // AppendedExchange clears them before later work starts. Do not use
        // metadata.last_modified_at as work identity: checkpoint and title
        // saves update that timestamp after the exchange has already finished.

        let now = Utc::now();
        let due = self
            .completed_plans
            .iter()
            .filter(|(_, record)| record.is_due(now))
            .map(|(document_id, _)| document_id.clone())
            .collect::<Vec<_>>();
        for document_id in due {
            self.archive_plan(&document_id, ctx);
        }

        if before != self.completed_plans || reopened_before != self.reopened_plans {
            self.persist(ctx);
        }
    }

    fn observe_completion(
        &mut self,
        document_id: String,
        conversation_id: AIConversationId,
        completed_at: DateTime<Utc>,
    ) {
        if self.reopened_plans.contains_key(&document_id) {
            return;
        }
        if self
            .completed_plans
            .get(&document_id)
            .is_some_and(|record| record.conversation_id == conversation_id)
        {
            return;
        }
        self.completed_plans.insert(
            document_id,
            CompletedPlan {
                conversation_id,
                completed_at,
                archived_to: None,
                original_folder: None,
            },
        );
    }

    fn archive_plan(&mut self, document_id: &str, ctx: &mut ModelContext<Self>) {
        if self.reopened_plans.contains_key(document_id) {
            return;
        }
        if let Some(id) = AIDocumentId::try_from(document_id).ok().filter(|id| {
            AIDocumentModel::as_ref(ctx)
                .get_current_document(id)
                .is_some_and(|document| document.user_edit_status.is_dirty())
        }) {
            self.reopen_document(id, ctx);
            return;
        }
        let notebook = CloudModel::as_ref(ctx)
            .get_all_active_notebooks()
            .find(|notebook| {
                notebook.permissions.owner == local_owner()
                    && notebook
                        .model()
                        .ai_document_id
                        .is_some_and(|id| id.to_string() == document_id)
            })
            .cloned();
        let Some(notebook) = notebook else {
            return;
        };

        let plans_folder = get_or_create_folder(PLAN_FOLDER_NAME, None, ctx);
        let archive_folder = get_or_create_folder(ARCHIVE_FOLDER_NAME, Some(plans_folder), ctx);
        let original_folder = notebook.metadata.folder_id;
        if original_folder != Some(archive_folder)
            && !LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
                repository.move_object_to_location(
                    CloudObjectTypeAndId::Notebook(notebook.id),
                    CloudObjectLocation::Folder(archive_folder),
                    ctx,
                )
            })
        {
            return;
        }
        if let Some(record) = self.completed_plans.get_mut(document_id) {
            record.original_folder = original_folder.filter(|id| *id != archive_folder);
            record.archived_to = Some(archive_folder);
        }
    }

    fn resume_conversation(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let document_ids = self
            .completed_plans
            .iter()
            .filter(|(_, record)| record.conversation_id == conversation_id)
            .map(|(document_id, _)| document_id.clone())
            .collect::<Vec<_>>();
        if document_ids.is_empty() {
            return;
        }
        for document_id in document_ids {
            let Some(record) = self.completed_plans.remove(&document_id) else {
                continue;
            };
            restore_archived_plan(&document_id, record, ctx);
        }
        self.persist(ctx);
    }

    fn begin_exchange(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.resume_conversation(conversation_id, ctx);
        let mut changed = false;
        self.reopened_plans.retain(|document_id, record| {
            if record.conversation_id != conversation_id {
                return true;
            }
            record.saw_new_exchange = true;
            changed = true;
            let document = AIDocumentId::try_from(document_id.as_str())
                .ok()
                .and_then(|id| AIDocumentModel::as_ref(ctx).get_current_document(&id));
            document.is_none_or(|document| document.user_edit_status.is_dirty())
        });
        if changed {
            self.persist(ctx);
        }
    }

    fn reopen_document(&mut self, document_id: AIDocumentId, ctx: &mut ModelContext<Self>) {
        let key = document_id.to_string();
        let completed = self.completed_plans.remove(&key);
        // Restored free-floating document views can have a synthetic owner.
        // Prefer the real owner retained in the completion/reopening record.
        let conversation_id = completed
            .as_ref()
            .map(|record| record.conversation_id)
            .or_else(|| {
                self.reopened_plans
                    .get(&key)
                    .map(|record| record.conversation_id)
            })
            .or_else(|| {
                AIDocumentModel::as_ref(ctx).get_conversation_id_for_document_id(&document_id)
            });
        if let Some(conversation_id) = conversation_id {
            self.reopened_plans.insert(
                key.clone(),
                ReopenedPlan {
                    conversation_id,
                    saw_new_exchange: false,
                },
            );
        }
        if let Some(record) = completed {
            restore_archived_plan(&key, record, ctx);
        }
        self.persist(ctx);
    }

    fn accept_document_edits(&mut self, document_id: AIDocumentId, ctx: &mut ModelContext<Self>) {
        let key = document_id.to_string();
        if self
            .reopened_plans
            .get(&key)
            .is_some_and(|record| record.saw_new_exchange)
        {
            self.reopened_plans.remove(&key);
            self.persist(ctx);
        }
    }

    fn persist(&self, ctx: &mut ModelContext<Self>) {
        match serde_json::to_string(self) {
            Ok(json) => {
                if let Err(error) = ctx
                    .private_user_preferences()
                    .write_value(PLAN_ARCHIVAL_KEY, json)
                {
                    log::error!("Failed to persist completed plan retention: {error}");
                }
            }
            Err(error) => log::error!("Failed to serialize completed plan retention: {error}"),
        }
    }
}

fn restore_archived_plan(
    document_id: &str,
    record: CompletedPlan,
    ctx: &mut ModelContext<PlanArchivalModel>,
) {
    let Some(archive_folder) = record.archived_to else {
        return;
    };
    let notebook = CloudModel::as_ref(ctx)
        .get_all_active_notebooks()
        .find(|notebook| {
            notebook.permissions.owner == local_owner()
                && notebook.metadata.folder_id == Some(archive_folder)
                && notebook
                    .model()
                    .ai_document_id
                    .is_some_and(|id| id.to_string() == document_id)
        })
        .cloned();
    let Some(notebook) = notebook else {
        return;
    };
    let original_folder = record.original_folder.filter(|id| {
        let cloud_model = CloudModel::as_ref(ctx);
        cloud_model.get_folder(id).is_some_and(|folder| {
            folder.permissions.owner == local_owner() && !folder.is_trashed(cloud_model)
        })
    });
    let destination = original_folder
        .map(CloudObjectLocation::Folder)
        .unwrap_or(CloudObjectLocation::Space(Space::Personal));
    LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
        repository.move_object_to_location(
            CloudObjectTypeAndId::Notebook(notebook.id),
            destination,
            ctx,
        );
    });
}

fn completed_work_time(conversation: &AIConversation) -> Option<DateTime<Utc>> {
    if !matches!(conversation.status(), ConversationStatus::Success)
        || conversation.active_provider_run_json().is_some()
        || conversation.has_active_subagent()
        || !conversation
            .active_todo_list()
            .is_some_and(|list| list.is_finished())
    {
        return None;
    }
    conversation
        .latest_exchange()
        .and_then(|exchange| exchange.finish_time)
        .map(|time| time.with_timezone(&Utc))
}

fn get_or_create_folder(
    name: &str,
    parent: Option<SyncId>,
    ctx: &mut ModelContext<PlanArchivalModel>,
) -> SyncId {
    if let Some(id) = CloudModel::as_ref(ctx)
        .get_all_active_folders()
        .find(|folder| {
            folder.permissions.owner == local_owner()
                && folder.model().name == name
                && folder.metadata.folder_id == parent
        })
        .map(|folder| folder.id)
    {
        return id;
    }
    let id = SyncId::ClientId(ClientId::new());
    LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
        repository.create_folder_with_id(id, name.to_string(), parent, ctx);
    });
    id
}

impl Entity for PlanArchivalModel {
    type Event = ();
}

impl SingletonEntity for PlanArchivalModel {}

#[cfg(test)]
#[path = "plan_archival_tests.rs"]
mod tests;
