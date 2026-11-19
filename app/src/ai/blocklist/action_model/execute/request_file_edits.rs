mod apply_diff_model;
mod diff_application;
mod telemetry;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use ai::diff_validation::AIRequestedCodeDiff;
use apply_diff_model::ApplyDiffModel;
use diff_application::DiffApplicationError;
pub(crate) use diff_application::{apply_edits, FileReadResult};
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::FutureExt;
use galaxy_core::send_telemetry_from_ctx;
use itertools::Itertools;
pub(crate) use telemetry::MalformedFinalLineProxyEvent;
#[allow(unused_imports)]
pub use telemetry::{EditAcceptAndContinueClickedEvent, EditAcceptClickedEvent};
pub use telemetry::{
    EditReceivedEvent, EditResolvedEvent, EditStats, RequestFileEditsFormatKind,
    RequestFileEditsTelemetryEvent,
};
use uuid::Uuid;
use vec1::{vec1, Vec1};
use warp_util::file::FileSaveError;
use warpui::{Entity, EntityId, ModelContext, ModelHandle, SingletonEntity as _, ViewHandle};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResultType, AIAgentActionType,
    AIAgentOutputMessage, AIAgentOutputMessageType, AIIdentifiers, AnyFileContent, FileContext,
    FileEdit, FileLocations, RequestFileEditsResult, UpdatedFileContext,
};
use crate::ai::blocklist::inline_action::code_diff_view::{
    CodeDiffView, CodeDiffViewEvent, DiffSessionType, FileDiff,
};
use crate::ai::blocklist::{BlocklistAIPermissions, RequestedEditResolution};
use crate::ai::paths::host_native_absolute_path;
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model::session::SessionType;
use crate::{safe_warn, BlocklistAIHistoryModel};
const APPLY_DIFF_RESULT_CONTEXT_LINES: usize = 10;

type AppliedDiffs = (Vec<FileDiff>, DiffSessionType);
type FileEditExecutionKey = (AIConversationId, AIAgentActionId);

struct PreparedFileEdits {
    action: AIAgentAction,
    token: Uuid,
    working_directory: Option<String>,
    ready: bool,
    applied_diffs: Option<AppliedDiffs>,
}

/// Called on a worker before preprocessing or permission review. The queued action is replaced
/// with this exact proposal so the save-time guard and the reviewed fields cannot diverge.
pub(crate) fn normalize_local_creations(
    mut action: AIAgentAction,
    working_directory: Option<String>,
    native_local: bool,
) -> Result<AIAgentAction, &'static str> {
    if !native_local {
        return Err("Guarded creation requires a verified native local session.");
    }
    let directory = working_directory
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or("Guarded creation requires an absolute working directory.")?;
    let scope = directory
        .canonicalize()
        .map_err(|_| "The creation scope could not be resolved safely.")?;
    if !scope.is_dir() {
        return Err("The creation scope is not a directory.");
    }
    let AIAgentActionType::RequestFileEdits { file_edits, .. } = &mut action.action else {
        return Err("Guarded creation requires a file-edit action.");
    };
    for edit in file_edits {
        let FileEdit::Create {
            file,
            creation_scope,
            ..
        } = edit
        else {
            continue;
        };
        let path = file
            .as_deref()
            .filter(|path| !path.is_empty())
            .ok_or("Every guarded creation must name a file.")?;
        if path.trim() != path
            || path.starts_with('~')
            || path.chars().any(char::is_control)
            || Path::new(path)
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("Guarded creation requires a literal native project path.");
        }
        let path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            scope.join(path)
        };
        let existing_scope = match creation_scope.as_ref() {
            Some(existing) => existing
                .canonicalize()
                .map_err(|_| "The existing creation scope could not be resolved safely.")?,
            None => scope.clone(),
        };
        if !existing_scope.starts_with(&scope)
            || path == existing_scope
            || !path.starts_with(&existing_scope)
        {
            return Err(
                "The file and its existing creation scope must remain inside the active project.",
            );
        }
        *file = Some(
            path.to_str()
                .ok_or("The native creation path is not valid text.")?
                .to_owned(),
        );
        *creation_scope = Some(existing_scope);
    }
    Ok(action)
}

#[derive(Default)]
struct PendingAppliedDiffs {
    by_action: HashMap<FileEditExecutionKey, AppliedDiffs>,
}

impl PendingAppliedDiffs {
    fn buffer(
        &mut self,
        key: FileEditExecutionKey,
        diffs: Vec<FileDiff>,
        diff_session_type: DiffSessionType,
    ) {
        self.by_action.insert(key, (diffs, diff_session_type));
    }

    fn take(&mut self, key: &FileEditExecutionKey) -> Option<AppliedDiffs> {
        self.by_action.remove(key)
    }
}

pub struct RequestFileEditsExecutor {
    active_session: ModelHandle<ActiveSession>,
    apply_diff_model: ModelHandle<ApplyDiffModel>,
    diff_views: HashMap<FileEditExecutionKey, ViewHandle<CodeDiffView>>,
    prepared_proposals: HashMap<FileEditExecutionKey, PreparedFileEdits>,
    /// Successfully applied diffs that completed before their view was registered.
    pending_applied_diffs: PendingAppliedDiffs,
    /// Set of action IDs where diff application failed.
    diff_application_failures: HashMap<FileEditExecutionKey, Vec1<DiffApplicationError>>,
    terminal_view_id: EntityId,
}

impl RequestFileEditsExecutor {
    pub fn new(
        active_session: ModelHandle<ActiveSession>,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let apply_diff_model = ctx.add_model(|_| ApplyDiffModel::new(active_session.clone()));
        Self {
            active_session,
            apply_diff_model,
            diff_views: HashMap::new(),
            prepared_proposals: HashMap::new(),
            pending_applied_diffs: PendingAppliedDiffs::default(),
            diff_application_failures: HashMap::new(),
            terminal_view_id,
        }
    }

    pub(super) fn should_autoexecute(
        &self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let ExecuteActionInput {
            action:
                AIAgentAction {
                    action: AIAgentActionType::RequestFileEdits { file_edits, .. },
                    ..
                },
            conversation_id,
        } = input
        else {
            return false;
        };

        let paths: Vec<PathBuf> = file_edits
            .iter()
            .filter_map(|edit| edit.file().map(PathBuf::from))
            .collect();

        // Don't allow autoexecution if the diff was generated passively.
        let Some(latest_exchange) = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&conversation_id)
            .and_then(|c| c.latest_exchange())
        else {
            return false;
        };
        if latest_exchange.has_passive_request() {
            return false;
        }

        // Allow "autoexecution" if the diff application failed so that we can continue execution.
        // This is a terrible hack--but allows us to continue execution and let the LLM potentially recover
        // from the LLM.
        // If we don't do this, a failed diff application will block execution of the entire AI conversation
        // without any possibility of recovery.
        if self
            .diff_application_failures
            .contains_key(&(input.conversation_id, input.action.id.clone()))
        {
            return true;
        }

        BlocklistAIPermissions::as_ref(ctx)
            .can_write_files(&conversation_id, &paths, Some(self.terminal_view_id), ctx)
            .is_allowed()
    }

    /// Registers a diff view to handle a RequestFileEdits action and applies any diffs that
    /// finished preprocessing before the UI observed the action.
    pub fn register_requested_edits(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
        view: &ViewHandle<CodeDiffView>,
        ctx: &mut ModelContext<Self>,
    ) {
        let key = (conversation_id, action_id.clone());
        if self
            .diff_views
            .get(&key)
            .is_some_and(|current| current.id() == view.id())
        {
            return;
        }
        // A late historical view must never replace the current proposal with stale edits.
        // Every new registration receives the exact current prepared snapshot, not whichever
        // data that view retained. Views already showing another proposal are left untouched.
        if !view.as_ref(ctx).is_pending_diffs_empty() {
            return;
        }
        let prepared_diffs = self
            .prepared_proposals
            .get(&key)
            .and_then(|prepared| prepared.applied_diffs.clone());
        if let Some((diffs, session_type)) = prepared_diffs {
            Self::apply_diffs_to_view(view, diffs, session_type, ctx);
            self.pending_applied_diffs.take(&key);
        } else if let Some((diffs, session_type)) = self.pending_applied_diffs.take(&key) {
            Self::apply_diffs_to_view(view, diffs, session_type, ctx);
        }
        self.diff_views.insert(key.clone(), view.clone());
    }

    pub(crate) fn clear_prepared_action(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) {
        let key = (conversation_id, action_id.clone());
        self.prepared_proposals.remove(&key);
        self.diff_views.remove(&key);
        self.pending_applied_diffs.take(&key);
        self.diff_application_failures.remove(&key);
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput {
            action:
                AIAgentAction {
                    id,
                    action: AIAgentActionType::RequestFileEdits { .. },
                    ..
                },
            ..
        } = input
        else {
            return ActionExecution::InvalidAction;
        };
        crate::ai::tool_diagnostics::tool_debug!(
            "RequestFileEditsExecutor::execute: action_id={:?}",
            id
        );

        let key = (input.conversation_id, id.clone());
        if BlocklistAIPermissions::as_ref(ctx).get_permission_mode(ctx, Some(self.terminal_view_id))
            == crate::ai::execution_profiles::PermissionMode::GalaxyDecides
        {
            let Some(prepared) = self.prepared_proposals.get(&key) else {
                return ActionExecution::NotReady;
            };
            if prepared.action != *input.action
                || prepared.working_directory
                    != self
                        .active_session
                        .as_ref(ctx)
                        .current_working_directory()
                        .cloned()
            {
                return ActionExecution::Sync(AIAgentActionResultType::RequestFileEdits(
                    RequestFileEditsResult::DiffApplicationFailed {
                        error: "The prepared file edits or working directory changed after permission review. Request a fresh file-edit action.".to_owned(),
                    },
                ));
            }
            if !prepared.ready {
                return ActionExecution::NotReady;
            }
        }
        let Some(diff_view) = self.diff_views.get(&key) else {
            crate::ai::tool_diagnostics::tool_debug!(
                "RequestFileEditsExecutor: no diff view found for action_id={:?}",
                id
            );
            return ActionExecution::NotReady;
        };

        // If diff application failed, early exit.
        if let Some(errors) = self.diff_application_failures.remove(&key) {
            return ActionExecution::Sync(AIAgentActionResultType::RequestFileEdits(
                RequestFileEditsResult::DiffApplicationFailed {
                    error: DiffApplicationError::error_for_conversation(&errors),
                },
            ));
        }

        let identifiers = self
            .generate_ai_identifiers(&input.conversation_id, id, ctx)
            .unwrap_or_else(|| AIIdentifiers {
                client_conversation_id: Some(input.conversation_id),
                ..Default::default()
            });

        let (result_tx, result_rx) = oneshot::channel();
        let mut result_tx = Some(result_tx);

        ctx.subscribe_to_view(diff_view, move |_me, _, event, ctx| match event {
            CodeDiffViewEvent::Rejected => {
                let Some(result_tx) = result_tx.take() else {
                    return;
                };
                let _ = result_tx.send(RequestFileEditsResult::Cancelled);
            }
            CodeDiffViewEvent::SavedAcceptedDiffs {
                diff,
                updated_files,
                file_contents,
                deleted_files,
                save_errors,
            } => {
                let Some(result_tx) = result_tx.take() else {
                    return;
                };

                // If saving any file failed, report it as an error to the LLM. Other files may
                // have saved successfully, but we're ignoring this edge case for now.
                if !save_errors.is_empty() {
                    let error = save_errors
                        .iter()
                        .filter_map(|err| match err.as_ref() {
                            FileSaveError::IOError { error, path } => {
                                Some(format!("Failed to save file {path:?}: {error}"))
                            }
                            _ => None,
                        })
                        .join("\n");

                    let _ = result_tx.send(RequestFileEditsResult::DiffApplicationFailed { error });
                    return;
                }

                let passive_diff = BlocklistAIHistoryModel::as_ref(ctx)
                    .is_entirely_passive_conversation(&input.conversation_id);
                send_telemetry_from_ctx!(
                    RequestFileEditsTelemetryEvent::EditResolved(EditResolvedEvent {
                        identifiers: identifiers.clone(),
                        response: RequestedEditResolution::Accept,
                        stats: EditStats {
                            files_edited: updated_files.len(),
                            lines_added: diff.lines_added,
                            lines_removed: diff.lines_removed,
                        },
                        passive_diff,
                    },),
                    ctx
                );

                // Build a map of file path → content from the editor buffers.
                // This avoids re-reading files from disk or the remote server.
                let content_map: HashMap<String, String> = file_contents.iter().cloned().collect();

                let _ = result_tx.send(RequestFileEditsResult::Success {
                    diff: diff.unified_diff.clone(),
                    updated_files: updated_file_contexts_from_editor_buffers(
                        updated_files,
                        &content_map,
                    ),
                    deleted_files: deleted_files.clone(),
                    lines_added: diff.lines_added,
                    lines_removed: diff.lines_removed,
                });
            }
            _ => (),
        });
        diff_view.update(ctx, |diff_view, ctx| {
            diff_view.accept_and_save(ctx);
        });

        ActionExecution::new_async(result_rx, |result, _ctx| match result {
            Ok(result) => AIAgentActionResultType::RequestFileEdits(result),
            Err(oneshot::Canceled) => {
                AIAgentActionResultType::RequestFileEdits(RequestFileEditsResult::Cancelled)
            }
        })
    }

    pub(super) fn preprocess_action(
        &mut self,
        input: PreprocessActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        let AIAgentAction {
            id,
            action: AIAgentActionType::RequestFileEdits { file_edits, .. },
            ..
        } = input.action
        else {
            return futures::future::ready(()).boxed();
        };

        let ai_identifiers = self
            .generate_ai_identifiers(&input.conversation_id, id, ctx)
            .unwrap_or_else(|| AIIdentifiers {
                client_conversation_id: Some(input.conversation_id),
                ..Default::default()
            });

        let passive_diff = BlocklistAIHistoryModel::as_ref(ctx)
            .is_entirely_passive_conversation(&input.conversation_id);

        send_telemetry_from_ctx!(
            RequestFileEditsTelemetryEvent::EditReceived(EditReceivedEvent {
                identifiers: ai_identifiers.clone(),
                unique_files: file_edits.iter().map(|file| file.file()).unique().count(),
                diffs: file_edits.len(),
                passive_diff,
            }),
            ctx
        );

        let (tx, rx) = oneshot::channel();
        let key = (input.conversation_id, id.clone());
        let token = Uuid::new_v4();
        let previous = self.prepared_proposals.insert(
            key.clone(),
            PreparedFileEdits {
                action: input.action.clone(),
                token,
                working_directory: self
                    .active_session
                    .as_ref(ctx)
                    .current_working_directory()
                    .cloned(),
                ready: false,
                applied_diffs: None,
            },
        );
        if previous.is_some() {
            self.diff_views.remove(&key);
            self.pending_applied_diffs.take(&key);
            self.diff_application_failures.remove(&key);
        }
        let files = file_edits.clone();
        let creation_scopes = files
            .iter()
            .filter_map(|edit| match edit {
                FileEdit::Create {
                    file: Some(path),
                    creation_scope: Some(scope),
                    ..
                } => Some((path.clone(), scope.clone())),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let apply_future = self.apply_diff_model.update(ctx, |model, ctx| {
            model.apply_diffs(files, &ai_identifiers, passive_diff, ctx)
        });

        ctx.spawn(
            async move {
                let applied_diffs = apply_future.await;
                (applied_diffs, tx)
            },
            move |me, (diffs, tx), ctx| {
                me.on_diffs_applied(diffs, key, token, tx, creation_scopes, ctx);
            },
        );

        async {
            rx.await.ok();
        }
        .boxed()
    }

    fn on_diffs_applied(
        &mut self,
        applied_diffs: Result<Vec<AIRequestedCodeDiff>, Vec1<DiffApplicationError>>,
        key: FileEditExecutionKey,
        token: Uuid,
        tx: oneshot::Sender<()>,
        creation_scopes: HashMap<String, PathBuf>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(prepared) = self
            .prepared_proposals
            .get_mut(&key)
            .filter(|prepared| prepared.token == token)
        else {
            let _ = tx.send(());
            return;
        };
        prepared.ready = true;
        let current_working_directory = prepared.working_directory.clone();
        match applied_diffs {
            Ok(applied_diffs) if !applied_diffs.is_empty() => {
                let shell_launch_data = self.active_session.as_ref(ctx).shell_launch_data(ctx);
                let diffs = applied_diffs
                    .into_iter()
                    .map(|diff| {
                        let path = if creation_scopes.contains_key(&diff.file_name) {
                            diff.file_name.clone()
                        } else {
                            host_native_absolute_path(
                                diff.file_name.as_str(),
                                &shell_launch_data,
                                &current_working_directory,
                            )
                        };
                        let mut file_diff =
                            FileDiff::new(diff.original_content, path, diff.diff_type);
                        file_diff.creation_scope = creation_scopes.get(&diff.file_name).cloned();
                        file_diff
                    })
                    .collect::<Vec<_>>();
                let diff_session_type = match self.active_session.as_ref(ctx).session_type(ctx) {
                    Some(SessionType::WormholedRemote {
                        host_id: Some(host_id),
                    }) => DiffSessionType::Remote(host_id.clone()),
                    _ => DiffSessionType::Local,
                };

                // Retain this snapshot until completion, including after the first view consumes
                // it. Late registration must bind to these exact edits and save-time guards.
                self.prepared_proposals.get_mut(&key).unwrap().applied_diffs =
                    Some((diffs.clone(), diff_session_type.clone()));

                if let Some(diff_view) = self
                    .diff_views
                    .get(&key)
                    .cloned()
                    .filter(|view| view.as_ref(ctx).is_pending_diffs_empty())
                {
                    Self::apply_diffs_to_view(&diff_view, diffs, diff_session_type, ctx);
                } else {
                    self.diff_views.remove(&key);
                    self.pending_applied_diffs
                        .buffer(key, diffs, diff_session_type);
                }
            }
            Ok(_) => {
                log::warn!("No diffs generated");
                self.diff_application_failures
                    .insert(key, vec1![DiffApplicationError::EmptyDiff]);
            }
            Err(err) => {
                safe_warn!(
                    safe: ("Failed to generate diffs"),
                    full: ("Failed to generate diffs {err:?}")
                );
                self.diff_application_failures.insert(key, err);
            }
        }

        tx.send(()).ok();
    }

    fn apply_diffs_to_view(
        diff_view: &ViewHandle<CodeDiffView>,
        diffs: Vec<FileDiff>,
        diff_session_type: DiffSessionType,
        ctx: &mut ModelContext<Self>,
    ) {
        diff_view.update(ctx, |diff_view, ctx| {
            diff_view.set_diff_session_type(diff_session_type);
            diff_view.set_candidate_diffs(diffs, ctx);
        });
    }

    fn generate_ai_identifiers(
        &self,
        conversation_id: &AIConversationId,
        action_id: &AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<AIIdentifiers> {
        let history_model = BlocklistAIHistoryModel::as_ref(ctx);
        let conversation = history_model.conversation(conversation_id)?;

        // Find the `AIAgentExchange` and its corresponding `AIAgentOutput` for this given action.
        let (exchange, output) = conversation.all_exchanges().into_iter().find_map(|exchange| {
            let output = exchange.output_status.output()?;
            let contains_action = output.get().messages.iter().any(|step| {
                matches!(step, AIAgentOutputMessage{ message: AIAgentOutputMessageType::Action(AIAgentAction { id, .. }), .. } if id == action_id)
            });

            contains_action.then_some((exchange, output))
        })?;

        let server_output_id = output.get().server_output_id.clone();
        let model_id = output.get().model_info.as_ref().map(|m| m.model_id.clone());
        Some(AIIdentifiers {
            client_conversation_id: Some(*conversation_id),
            client_exchange_id: Some(exchange.id),
            server_output_id,
            server_conversation_id: conversation
                .server_conversation_token()
                .cloned()
                .map(Into::into),
            model_id,
        })
    }
}

fn updated_file_contexts_from_editor_buffers(
    updated_files: &[(FileLocations, bool)],
    content_map: &HashMap<String, String>,
) -> Vec<UpdatedFileContext> {
    updated_files
        .iter()
        .flat_map(|(file_location, was_edited)| {
            let content = content_map
                .get(&file_location.name)
                .cloned()
                .unwrap_or_default();
            let line_count = content.lines().count();

            let mut file_location = file_location.clone();
            file_location.expand_surrounding_context(APPLY_DIFF_RESULT_CONTEXT_LINES);
            clamp_to_file_context_range_start(&mut file_location);

            if file_location.lines.is_empty() {
                return vec![UpdatedFileContext {
                    was_edited_by_user: *was_edited,
                    file_context: FileContext {
                        file_name: file_location.name,
                        content: AnyFileContent::StringContent(content),
                        line_range: None,
                        last_modified: None,
                        line_count,
                    },
                }];
            }

            let lines = content.lines().collect_vec();
            file_location
                .lines
                .into_iter()
                .map(|range| {
                    let start = range.start.saturating_sub(1).min(lines.len());
                    let end = range.end.saturating_sub(1).min(lines.len());
                    let fragment = if start >= end {
                        String::new()
                    } else {
                        lines[start..end].join("\n")
                    };

                    UpdatedFileContext {
                        was_edited_by_user: *was_edited,
                        file_context: FileContext {
                            file_name: file_location.name.clone(),
                            content: AnyFileContent::StringContent(fragment),
                            line_range: Some(range),
                            last_modified: None,
                            line_count,
                        },
                    }
                })
                .collect_vec()
        })
        .collect()
}

fn clamp_to_file_context_range_start(file_location: &mut FileLocations) {
    for range in &mut file_location.lines {
        range.start = range.start.max(1);
        range.end = range.end.max(range.start);
    }
}

impl Entity for RequestFileEditsExecutor {
    type Event = ();
}

#[cfg(test)]
#[path = "request_file_edits_tests.rs"]
mod tests;
