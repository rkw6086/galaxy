use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Local};
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::{select, FutureExt};
use futures_lite::pin;
use galaxy_core::command::ExitCode;
use galaxy_core::execution_mode::AppExecutionMode;
use galaxy_util::path::ShellFamily;
use galaxyui::r#async::{Spawnable, Timer};
use galaxyui::{Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};
use parking_lot::FairMutex;

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};
use crate::ai::agent::{
    AIAgentActionId, AIAgentActionResultType, AIAgentActionType, AIAgentPtyWriteMode,
    ReadShellCommandOutputResult, RequestCommandOutputResult, ShellCommandDelay, ShellCommandError,
    TransferShellCommandControlToUserResult, WriteToLongRunningShellCommandResult,
};
use crate::ai::blocklist::permissions::CommandExecutionPermission;
use crate::ai::blocklist::BlocklistAIPermissions;
use crate::ai::execution_profiles::WriteToPtyPermission;
use crate::terminal::event::BlockMetadataReceivedEvent;
use crate::terminal::model::block::{
    formatted_terminal_contents_for_input, Block, BlockId, CURSOR_MARKER,
};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model_events::{ModelEvent, ModelEventDispatcher};
use crate::terminal::shell::ShellType;
use crate::terminal::TerminalModel;
use crate::{send_telemetry_from_ctx, TelemetryEvent};

pub struct ShellCommandExecutor {
    active_session: ModelHandle<ActiveSession>,
    block_finished_senders: HashMap<BlockSelector, Vec<oneshot::Sender<()>>>,
    /// Senders used by `Check now` and the automatic monitor watchdog to force a long-running
    /// shell command's pending poll future to resolve immediately with a fresh snapshot,
    /// bypassing the agent-set timeout.
    force_refresh_senders: HashMap<BlockSelector, Vec<oneshot::Sender<()>>>,
    terminal_model: Arc<FairMutex<TerminalModel>>,
    terminal_view_id: EntityId,
    /// Sender to notify when user hands control back to agent after TransferShellCommandControlToUser.
    control_handback_sender: Option<oneshot::Sender<()>>,
}

impl ShellCommandExecutor {
    /// Command launch already passed the permission gate. Continuing that exact
    /// active command shares its authorization, not an unrelated terminal prompt.
    pub(crate) fn can_continue_authorized_command(
        &self,
        conversation_id: crate::AIConversationId,
        block_id: &BlockId,
        app: &galaxyui::AppContext,
    ) -> bool {
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        {
            let value = block_id.to_string();
            if let Ok(id) =
                uuid::Uuid::parse_str(value.strip_prefix("background-job:").unwrap_or(&value))
            {
                return crate::ai::background_jobs::BackgroundJobsModel::as_ref(app)
                    .job(id)
                    .is_some_and(|job| job.conversation_id == conversation_id);
            }
        }
        let model = self.terminal_model.lock();
        let Some(block) = model.block_list().block_with_id(block_id) else {
            return false;
        };
        block.ai_conversation_id() == Some(conversation_id)
            && (block.requested_command_action_id().is_some()
                || block.cli_subagent_task_id().is_some())
            // The executor returns the final snapshot instead of writing to a
            // finished block; no new permission is needed to retrieve its output.
            && (block.finished()
                || (model.block_list().active_block().id() == block_id
                    && block.is_active_and_long_running()
                    && block.is_agent_driving_command()))
    }

    pub const MAX_WAIT_DURATION: Duration = Duration::from_secs(2);
    /// Maximum delay we will honor for any agent-requested wait. Applies both  
    /// to finite `ShellCommandDelay::Duration` requests and to  
    /// `ShellCommandDelay::OnCompletion`, which would otherwise wait indefinitely.  
    pub const MAX_AGENT_DELAY_DURATION: Duration = Duration::from_secs(120);

    pub fn new(
        active_session: ModelHandle<ActiveSession>,
        terminal_model: Arc<FairMutex<TerminalModel>>,
        model_event_dispatcher: &ModelHandle<ModelEventDispatcher>,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(model_event_dispatcher, Self::handle_terminal_model_event);

        Self {
            active_session,
            terminal_model,
            block_finished_senders: HashMap::new(),
            force_refresh_senders: HashMap::new(),
            terminal_view_id,
            control_handback_sender: None,
        }
    }

    fn handle_terminal_model_event(
        &mut self,
        _: ModelHandle<ModelEventDispatcher>,
        event: &ModelEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        #[cfg(not(target_family = "wasm"))]
        if let ModelEvent::Exit { reason } = event {
            let context = {
                let model = self.terminal_model.lock();
                let block = model.block_list().active_block();
                serde_json::json!({
                    "event": "shell_terminal_exited",
                    "reason": format!("{reason:?}"),
                    "block_id": block.id().to_string(),
                    "conversation_id": block.ai_conversation_id().map(|id| id.to_string()),
                    "action_id": block.requested_command_action_id().map(|id| id.to_string()),
                    "completion_waiter_groups": self.block_finished_senders.len(),
                    "refresh_waiter_groups": self.force_refresh_senders.len(),
                })
            };
            crate::ai::remote_logging::log_model_event(
                ctx,
                crate::ai::remote_logging::RemoteLogRecord {
                    level: crate::ai::remote_logging::RemoteLogLevel::Info,
                    message: "Shell terminal exited".to_string(),
                    context,
                },
            );
        }
        // Log definitive terminal completion independently of tool-result delivery.
        #[cfg(not(target_family = "wasm"))]
        if let ModelEvent::BlockCompleted(completed) = event {
            let context = {
                let model = self.terminal_model.lock();
                model
                    .block_list()
                    .block_with_id(&completed.block_id)
                    .and_then(|block| {
                        block.requested_command_action_id().map(|action_id| serde_json::json!({
                        "event": "shell_command_terminal_completed",
                        "conversation_id": block.ai_conversation_id().map(|id| id.to_string()),
                        "action_id": action_id.to_string(),
                        "block_id": block.id().to_string(),
                        "exit_code": block.exit_code().value(),
                        "start_ts": block.start_ts(),
                        "completed_ts": block.completed_ts(),
                        "output_rows": block.output_grid().len_displayed(),
                    }))
                    })
            };
            if let Some(context) = context {
                crate::ai::remote_logging::log_model_event(
                    ctx,
                    crate::ai::remote_logging::RemoteLogRecord {
                        level: crate::ai::remote_logging::RemoteLogLevel::Info,
                        message: "Shell command terminal completed".to_string(),
                        context,
                    },
                );
            }
        }
        // Precmd provides fresh CWD metadata, while BlockCompleted is definitive completion
        // evidence for shells that never deliver a subsequent precmd.
        if matches!(
            event,
            ModelEvent::BlockMetadataReceived(BlockMetadataReceivedEvent { .. })
                | ModelEvent::BlockCompleted(_)
        ) {
            let model = self.terminal_model.lock();
            let block_finished_senders = self.block_finished_senders.drain().collect::<Vec<_>>();
            for (block_selector, block_finished_txs) in block_finished_senders {
                let completed_block = block_selector.get_block(&model).filter(|block| {
                    block.is_command_finished()
                        && match event {
                            ModelEvent::BlockCompleted(completed) => {
                                block.id() == &completed.block_id
                            }
                            ModelEvent::BlockMetadataReceived(_) => true,
                            _ => false,
                        }
                });
                if completed_block.is_some() {
                    for block_finished_tx in block_finished_txs {
                        if let Err(e) = block_finished_tx.send(()) {
                            log::warn!(
                                "Failed to notify block completion for running requested command: {e:?}"
                            )
                        }
                    }
                } else {
                    // The requested-command association may not exist yet. Keep all waiters until
                    // this selector resolves and its block actually completes, or it is cancelled.
                    self.block_finished_senders
                        .insert(block_selector, block_finished_txs);
                }
            }
        }
    }

    pub(super) fn should_autoexecute(
        &self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let blocklist_permissions = BlocklistAIPermissions::as_ref(ctx);
        match &input.action.action {
            AIAgentActionType::RequestCommandOutput {
                command,
                is_read_only,
                is_risky,
                ..
            } => {
                let Some(escape_char) = self
                    .active_session
                    .as_ref(ctx)
                    .shell_type(ctx)
                    .map(|s| ShellFamily::from(s).escape_char())
                else {
                    return false;
                };
                let autoexecution_permission = blocklist_permissions.can_autoexecute_command(
                    &input.conversation_id,
                    command,
                    escape_char,
                    is_read_only.unwrap_or(false),
                    *is_risky,
                    Some(self.terminal_view_id),
                    ctx,
                );
                if let CommandExecutionPermission::Allowed(reason) = autoexecution_permission {
                    send_telemetry_from_ctx!(
                        TelemetryEvent::AutoexecutedAgentModeRequestedCommand { reason },
                        ctx
                    );
                } else if let CommandExecutionPermission::Denied(reason) = autoexecution_permission
                {
                    if AppExecutionMode::as_ref(ctx).is_autonomous() {
                        log::warn!(
                            "Command denied during autonomous execution, reason: {reason:?}"
                        );
                    }
                }
                autoexecution_permission.is_allowed()
            }
            AIAgentActionType::WriteToLongRunningShellCommand { block_id, .. } => {
                let terminal_model = self.terminal_model.lock();
                let block = terminal_model.block_list().block_with_id(block_id);

                if block.is_none_or(|block| block.finished()) {
                    // If the block is already finished, allow auto-execution - the finished output
                    // will be returned.
                    true
                } else {
                    let should_autoexecute = match blocklist_permissions.can_write_to_pty(
                        &input.conversation_id,
                        Some(self.terminal_view_id),
                        ctx,
                    ) {
                        WriteToPtyPermission::AlwaysAllow => true,
                        WriteToPtyPermission::AskOnFirstWrite => terminal_model
                            .block_list()
                            .active_block()
                            .has_agent_written_to_block(),
                        _ => false,
                    };

                    if should_autoexecute {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::CLISubagentActionExecuted {
                                conversation_id: input.conversation_id,
                                block_id: block_id.clone(),
                                is_autoexecuted: true,
                            },
                            ctx
                        );
                    }

                    should_autoexecute
                }
            }
            AIAgentActionType::ReadShellCommandOutput { .. } => true,
            AIAgentActionType::TransferShellCommandControlToUser { .. } => false,
            _ => false,
        }
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> AnyActionExecution {
        match &input.action.action {
            AIAgentActionType::RequestCommandOutput { command, .. } => {
                self.start_background_command(input, command, ctx)
            }
            AIAgentActionType::WriteToLongRunningShellCommand {
                block_id,
                input: bytes,
                mode,
            } => {
                let result = if *mode == AIAgentPtyWriteMode::Raw && bytes.as_ref() == [3] {
                    self.cancel_background_job(input.conversation_id, block_id, ctx)
                } else {
                    WriteToLongRunningShellCommandResult::Error(
                        ShellCommandError::NonInteractiveOnly,
                    )
                };
                AnyActionExecution::Sync(AIAgentActionResultType::WriteToLongRunningShellCommand(
                    result,
                ))
            }
            AIAgentActionType::ReadShellCommandOutput { .. } => {
                AnyActionExecution::Sync(AIAgentActionResultType::ReadShellCommandOutput(
                    ReadShellCommandOutputResult::Error(ShellCommandError::NonInteractiveOnly),
                ))
            }
            AIAgentActionType::TransferShellCommandControlToUser { .. } => {
                AnyActionExecution::Sync(
                    AIAgentActionResultType::TransferShellCommandControlToUser(
                        TransferShellCommandControlToUserResult::Error(
                            ShellCommandError::NonInteractiveOnly,
                        ),
                    ),
                )
            }
            _ => AnyActionExecution::InvalidAction,
        }
    }

    fn start_background_command(
        &self,
        input: ExecuteActionInput,
        command: &str,
        ctx: &mut ModelContext<Self>,
    ) -> AnyActionExecution {
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        {
            use std::path::PathBuf;

            use crate::ai::background_jobs::{BackgroundJobLaunch, BackgroundJobsModel};
            use crate::ai::blocklist::SessionContext;
            let session = self.active_session.as_ref(ctx);
            let context = SessionContext::from_session(session, ctx);
            let Some(cwd) = context.current_working_directory().as_deref() else {
                return background_launch_error(
                    command,
                    "The session working directory is unavailable".to_owned(),
                );
            };
            let directory = if context.is_remote() {
                cwd.to_owned()
            } else {
                ai::paths::host_native_absolute_path(
                    cwd,
                    context.shell(),
                    context.current_working_directory(),
                )
            };
            let session_id = session.session(ctx).map(|session| session.id());
            let launch = BackgroundJobLaunch {
                conversation_id: input.conversation_id,
                action_id: input.action.id.clone(),
                command: command.to_owned(),
                working_directory: PathBuf::from(directory),
                shell: context.shell().clone(),
                environment: session.environment_variables(ctx),
                host_id: context.host_id().cloned(),
            };
            if context.is_remote() {
                let Some(host_id) = context.host_id() else {
                    return background_launch_error(
                        command,
                        "Connect to the command's remote host before running it".to_owned(),
                    );
                };
                let Some(session_id) = session_id else {
                    return background_launch_error(
                        command,
                        "The remote shell session is unavailable".to_owned(),
                    );
                };
                let Some(client) = remote_server::manager::RemoteServerManager::as_ref(ctx)
                    .client_for_host(host_id)
                    .cloned()
                else {
                    return background_launch_error(
                        command,
                        "The command's remote host is disconnected".to_owned(),
                    );
                };
                if !client.supports_background_jobs() {
                    return background_launch_error(
                        command,
                        "Update Galaxy on the remote host to run agent background jobs".to_owned(),
                    );
                }
                let transport = Arc::new(
                    remote_server::manager::RemoteServerManager::as_ref(ctx)
                        .host_request_handle(host_id),
                );
                let result = BackgroundJobsModel::handle(ctx).update(ctx, |jobs, ctx| {
                    crate::ai::background_jobs::remote::start(
                        transport, launch, session_id, jobs, ctx,
                    )
                });
                return AnyActionExecution::Sync(AIAgentActionResultType::RequestCommandOutput(
                    match result {
                        Ok(id) => RequestCommandOutputResult::BackgroundJobStarted {
                            job_id: id.to_string(),
                            command: command.to_owned(),
                        },
                        Err(message) => RequestCommandOutputResult::ExecutionError {
                            command: command.to_owned(),
                            message,
                        },
                    },
                ));
            }
            let result =
                BackgroundJobsModel::handle(ctx).update(ctx, |jobs, ctx| jobs.start(launch, ctx));
            AnyActionExecution::Sync(AIAgentActionResultType::RequestCommandOutput(
                match result {
                    Ok(id) => RequestCommandOutputResult::BackgroundJobStarted {
                        job_id: id.to_string(),
                        command: command.to_owned(),
                    },
                    Err(message) => RequestCommandOutputResult::ExecutionError {
                        command: command.to_owned(),
                        message,
                    },
                },
            ))
        }
        #[cfg(not(all(feature = "local_fs", not(target_family = "wasm"))))]
        {
            let _ = (input, ctx);
            background_launch_error(
                command,
                "Background commands are unavailable in this session".to_owned(),
            )
        }
    }

    fn cancel_background_job(
        &self,
        conversation_id: crate::AIConversationId,
        block_id: &BlockId,
        ctx: &mut ModelContext<Self>,
    ) -> WriteToLongRunningShellCommandResult {
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        {
            use crate::ai::background_jobs::BackgroundJobsModel;
            let value = block_id.to_string();
            let parsed =
                uuid::Uuid::parse_str(value.strip_prefix("background-job:").unwrap_or(&value));
            let Ok(id) = parsed else {
                return WriteToLongRunningShellCommandResult::Error(
                    ShellCommandError::NonInteractiveOnly,
                );
            };
            if !BackgroundJobsModel::as_ref(ctx)
                .job(id)
                .is_some_and(|job| job.conversation_id == conversation_id)
            {
                return WriteToLongRunningShellCommandResult::Error(
                    ShellCommandError::BlockNotFound,
                );
            }
            return match BackgroundJobsModel::handle(ctx)
                .update(ctx, |jobs, ctx| jobs.cancel(id, ctx))
            {
                Ok(()) => {
                    WriteToLongRunningShellCommandResult::BackgroundJobCancellationRequested {
                        job_id: id.to_string(),
                    }
                }
                Err(message) => WriteToLongRunningShellCommandResult::Error(
                    ShellCommandError::BackgroundJobError(message),
                ),
            };
        }
        #[cfg(not(all(feature = "local_fs", not(target_family = "wasm"))))]
        {
            let _ = (conversation_id, block_id, ctx);
            WriteToLongRunningShellCommandResult::Error(ShellCommandError::NonInteractiveOnly)
        }
    }

    /// Called when user hands control back to agent after TransferShellCommandControlToUser.
    pub fn notify_control_handed_back(&mut self) {
        if let Some(sender) = self.control_handback_sender.take() {
            let _ = sender.send(());
        }
    }

    pub(super) fn cancel_execution(
        &mut self,
        id: &AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let terminal_model = self.terminal_model.lock();
        let requested_selector = BlockSelector::RequestedCommandId(id.clone());
        let requested_block_is_running = requested_selector
            .get_block(&terminal_model)
            .is_some_and(|block| block.is_active_and_long_running() && !block.finished());
        let selector = if requested_block_is_running {
            requested_selector
        } else {
            BlockSelector::Id(terminal_model.active_block_id().clone())
        };
        // Cancelling the wait future alone would report cancellation while the process keeps
        // running. Terminate the exact requested command before resolving the action as cancelled.
        ctx.emit(ShellCommandExecutorEvent::CancelExecution {
            action_id: id.clone(),
        });
        if !requested_block_is_running {
            self.block_finished_senders.remove(&selector);
            self.force_refresh_senders.remove(&selector);
        }
        requested_block_is_running
    }

    fn prune_closed_senders(&mut self, selector: &BlockSelector) {
        Self::prune_closed_sender_group(&mut self.block_finished_senders, selector);
        Self::prune_closed_sender_group(&mut self.force_refresh_senders, selector);
    }

    fn prune_closed_sender_group(
        senders: &mut HashMap<BlockSelector, Vec<oneshot::Sender<()>>>,
        selector: &BlockSelector,
    ) {
        if let Some(selector_senders) = senders.get_mut(selector) {
            selector_senders.retain(|sender| !sender.is_canceled());
            if selector_senders.is_empty() {
                senders.remove(selector);
            }
        }
    }

    /// Force any in-flight poll for the given long-running command block to resolve
    /// immediately with a fresh snapshot, bypassing the agent-set timeout.
    ///
    /// Called by the `Check now` affordance and automatic monitor watchdog. No-ops if there is no
    /// matching in-flight poll (e.g. because the block already finished or the agent transferred
    /// control to the user). Returns whether a matching poll was successfully refreshed.
    pub fn force_refresh_block(&mut self, block_id: &BlockId) -> bool {
        let terminal_model = self.terminal_model.lock();
        // Find every pending poll whose selector resolves to this block. Multiple provider polls
        // may legitimately wait on the same command and must be refreshed together.
        let matching_selectors = self
            .force_refresh_senders
            .keys()
            .filter(|selector| {
                selector.get_block(&terminal_model).is_some_and(|block| {
                    block.id() == block_id
                        && block.is_active_and_long_running()
                        && !block.finished()
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        drop(terminal_model);

        let mut refreshed = false;
        for selector in matching_selectors {
            if let Some(senders) = self.force_refresh_senders.remove(&selector) {
                for sender in senders {
                    refreshed |= sender.send(()).is_ok();
                }
            }
        }
        refreshed
    }

    pub(super) fn preprocess_action(
        &mut self,
        _action: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

fn log_shell_wait_result(
    conversation_id: crate::AIConversationId,
    action_id: &AIAgentActionId,
    result: &ActionResult,
    ctx: &mut ModelContext<ShellCommandExecutor>,
) {
    crate::ai::remote_logging::log_model_event(
        ctx,
        crate::ai::remote_logging::RemoteLogRecord {
            level: crate::ai::remote_logging::RemoteLogLevel::Info,
            message: "Shell command wait resolved".to_string(),
            context: serde_json::json!({
                "event": "shell_command_wait_resolved",
                "conversation_id": conversation_id.to_string(),
                "action_id": action_id.to_string(),
                "result": shell_result_diagnostics(result),
            }),
        },
    );
}

fn shell_result_diagnostics(result: &ActionResult) -> serde_json::Value {
    match result {
        ActionResult::CommandFinished {
            block_id,
            output,
            exit_code,
            start_ts,
            completed_ts,
        } => serde_json::json!({
            "state": "finished", "block_id": block_id.to_string(),
            "exit_code": exit_code.value(), "output_bytes": output.len(),
            "start_ts": start_ts, "completed_ts": completed_ts,
        }),
        ActionResult::LongRunningCommandSnapshot {
            block_id,
            grid_contents,
            is_alt_screen_active,
            is_preempted,
            wake_reason,
            ..
        } => serde_json::json!({
            "state": "running", "block_id": block_id.to_string(),
            "snapshot_bytes": grid_contents.len(), "alt_screen": is_alt_screen_active,
            "preempted": is_preempted, "wake_reason": wake_reason,
        }),
        ActionResult::Cancelled => serde_json::json!({"state": "cancelled"}),
        ActionResult::BlockNotFound => serde_json::json!({"state": "block_not_found"}),
    }
}

fn command_for_execution(
    command: &str,
    shell_type: Option<ShellType>,
    wait_until_completion: bool,
) -> String {
    if !wait_until_completion {
        return command.to_string();
    }

    match shell_type {
        // Pager environment variables preserve the command's output and exit status, unlike piping
        // through `cat`. Tool-specific variables override user configuration for common pagers.
        Some(ShellType::Zsh) | Some(ShellType::Bash) => format!(
            "(export PAGER=cat GIT_PAGER=cat GH_PAGER=cat AWS_PAGER=cat SYSTEMD_PAGER=cat; {command})"
        ),
        Some(ShellType::Fish) => format!(
            "begin; set -lx PAGER cat; set -lx GIT_PAGER cat; set -lx GH_PAGER cat; set -lx AWS_PAGER cat; set -lx SYSTEMD_PAGER cat; {command}; end"
        ),
        // PowerShell's pipeline host suppresses paging for commands that honor the host stream.
        Some(ShellType::PowerShell) => format!("({command}) | \\Out-Host"),
        None => command.to_string(),
    }
}

fn terminal_busy_execution_error(command: &str, running_command: &str) -> AIAgentActionResultType {
    AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::ExecutionError {
        command: command.to_string(),
        message: format!("terminal is busy running command '{running_command}'"),
    })
}

fn background_launch_error(command: &str, message: String) -> AnyActionExecution {
    AnyActionExecution::Sync(AIAgentActionResultType::RequestCommandOutput(
        RequestCommandOutputResult::ExecutionError {
            command: command.to_owned(),
            message,
        },
    ))
}

fn selected_block_owns_alt_screen(
    is_alt_screen_active: bool,
    active_block_id: &BlockId,
    selected_block_id: &BlockId,
) -> bool {
    is_alt_screen_active && active_block_id == selected_block_id
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum BlockSelector {
    Id(BlockId),
    RequestedCommandId(AIAgentActionId),
}

impl BlockSelector {
    fn get_block<'a>(&self, model: &'a TerminalModel) -> Option<&'a Block> {
        match self {
            BlockSelector::Id(block_id) => model.block_list().block_with_id(block_id),
            BlockSelector::RequestedCommandId(requested_command_id) => model
                .block_list()
                .block_for_ai_action_id(requested_command_id),
        }
    }
}

/// Returns the result from executing a requested command.
fn action_result_for_requested_command(
    command: String,
    result: ActionResult,
) -> AIAgentActionResultType {
    match result {
        ActionResult::CommandFinished {
            block_id,
            output,
            exit_code,
            start_ts,
            completed_ts,
        } => AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
            command,
            block_id,
            output,
            exit_code,
            start_ts,
            completed_ts,
        }),
        ActionResult::LongRunningCommandSnapshot {
            block_id,
            grid_contents,
            cursor,
            is_alt_screen_active,
            ..
        } => AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::LongRunningCommandSnapshot {
                command,
                block_id,
                grid_contents,
                cursor: cursor.to_owned(),
                is_alt_screen_active,
            },
        ),
        ActionResult::BlockNotFound | ActionResult::Cancelled => {
            AIAgentActionResultType::RequestCommandOutput(
                RequestCommandOutputResult::CancelledBeforeExecution,
            )
        }
    }
}

/// Returns the result from writing to a long-running shell command.
fn action_result_for_write_to_long_running_shell_command(
    result: ActionResult,
) -> AIAgentActionResultType {
    match result {
        ActionResult::CommandFinished {
            block_id,
            output,
            exit_code,
            start_ts,
            completed_ts,
        } => AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::CommandFinished {
                block_id,
                output,
                exit_code,
                start_ts,
                completed_ts,
            },
        ),
        ActionResult::LongRunningCommandSnapshot {
            block_id,
            grid_contents,
            cursor,
            is_alt_screen_active,
            is_preempted,
            ..
        } => AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Snapshot {
                block_id,
                grid_contents,
                cursor: cursor.to_owned(),
                is_alt_screen_active,
                is_preempted,
            },
        ),
        ActionResult::Cancelled => AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Cancelled,
        ),
        ActionResult::BlockNotFound => AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Error(ShellCommandError::BlockNotFound),
        ),
    }
}

/// Returns the result from reading shell command output.
fn action_result_for_read_shell_command_output(
    command: String,
    result: ActionResult,
) -> AIAgentActionResultType {
    match result {
        ActionResult::CommandFinished {
            output,
            exit_code,
            block_id,
            start_ts,
            completed_ts,
        } => AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::CommandFinished {
                command,
                block_id,
                output,
                exit_code,
                start_ts,
                completed_ts,
            },
        ),
        ActionResult::LongRunningCommandSnapshot {
            block_id,
            grid_contents,
            cursor,
            is_alt_screen_active,
            is_preempted,
            ..
        } => AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::LongRunningCommandSnapshot {
                command,
                block_id,
                grid_contents,
                cursor: cursor.to_owned(),
                is_alt_screen_active,
                is_preempted,
            },
        ),
        ActionResult::Cancelled => {
            AIAgentActionResultType::ReadShellCommandOutput(ReadShellCommandOutputResult::Cancelled)
        }
        ActionResult::BlockNotFound => AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::Error(ShellCommandError::BlockNotFound),
        ),
    }
}

/// Returns the result from transferring shell command control to user.
fn action_result_for_transfer_shell_command_control_to_user(
    result: ActionResult,
) -> AIAgentActionResultType {
    match result {
        ActionResult::CommandFinished {
            block_id,
            output,
            exit_code,
            start_ts,
            completed_ts,
        } => AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::CommandFinished {
                block_id,
                output,
                exit_code,
                start_ts,
                completed_ts,
            },
        ),
        ActionResult::LongRunningCommandSnapshot {
            block_id,
            grid_contents,
            cursor,
            is_alt_screen_active,
            is_preempted,
            ..
        } => AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Snapshot {
                block_id,
                grid_contents,
                cursor: cursor.to_owned(),
                is_alt_screen_active,
                is_preempted,
            },
        ),
        ActionResult::Cancelled => AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Cancelled,
        ),
        ActionResult::BlockNotFound => AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Error(ShellCommandError::BlockNotFound),
        ),
    }
}

#[derive(Debug, Clone)]
pub enum ShellCommandExecutorEvent {
    ExecuteCommand {
        action_id: AIAgentActionId,
        command: String,
    },
    WriteToPty {
        input: Bytes,
        mode: AIAgentPtyWriteMode,
    },
    CancelExecution {
        action_id: AIAgentActionId,
    },
    /// Emitted when the agent requests to transfer control of a long-running command to the user.
    TransferControlToUser {
        action_id: AIAgentActionId,
        reason: String,
    },
}

impl Entity for ShellCommandExecutor {
    type Event = ShellCommandExecutorEvent;
}

/// Result from waiting for control transfer.
#[derive(Debug, Clone)]
enum TransferControlResult {
    ControlHandedBack,
    BlockFinished,
    Cancelled,
}

/// The possible results of taking an action.
#[derive(Debug, Clone)]
enum ActionResult {
    CommandFinished {
        block_id: BlockId,
        output: String,
        exit_code: ExitCode,
        start_ts: Option<DateTime<Local>>,
        completed_ts: Option<DateTime<Local>>,
    },
    LongRunningCommandSnapshot {
        block_id: BlockId,
        grid_contents: String,
        cursor: &'static str,
        is_alt_screen_active: bool,
        is_preempted: bool,
        wake_reason: &'static str,
    },
    Cancelled,
    BlockNotFound,
}

#[cfg(test)]
#[path = "shell_command_tests.rs"]
mod tests;
