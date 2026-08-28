use std::collections::HashMap;
use std::sync::Arc;

use galaxyui::{Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};
use instant::Instant;
use parking_lot::FairMutex;
use serde::{Deserialize, Serialize};

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentActionId, AIAgentActionResultType, AIAgentContext, CancellationReason,
    ReadShellCommandOutputResult, RequestCommandOutputResult, RunningCommand,
    TransferShellCommandControlToUserResult, WriteToLongRunningShellCommandResult,
};
use crate::ai::blocklist::agent_view::{AgentViewController, AgentViewEntryOrigin};
use crate::ai::blocklist::context_model::block_context_from_terminal_model;
use crate::ai::blocklist::controller::PendingProviderCommandCompletion;
use crate::ai::blocklist::{
    BlocklistAIActionEvent, BlocklistAIActionModel, BlocklistAIController,
    BlocklistAIControllerEvent, BlocklistAIHistoryEvent,
};
use crate::server::telemetry::{CLISubagentControlState, TelemetryEvent};
use crate::terminal::event::BlockType;
use crate::terminal::model::block::BlockId;
use crate::terminal::model_events::{ModelEvent, ModelEventDispatcher};
use crate::terminal::TerminalModel;
use crate::BlocklistAIHistoryModel;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UserTakeOverReason {
    Manual,
    Stop,
    /// The agent explicitly transferred control to the user via the
    /// TransferShellCommandControlToUser tool call.
    TransferFromAgent {
        /// The reason the agent gave for transferring control.
        reason: String,
    },
}

#[derive(Debug, Clone, Default)]
struct ActiveCLISubagentState {
    initial_requested_command_conversation_id: Option<AIConversationId>,
    initial_requested_command_action_id: Option<AIAgentActionId>,
    task_id: Option<TaskId>,
    last_snapshot_at: Option<Instant>,
    /// Prevents a monitor turn that ended with prose and no tool call from recursively
    /// generating nudges. A real snapshot/action result resets this so the next turn can be
    /// nudged again if it stalls in the same way.
    monitor_nudge_sent: bool,
    completion: Option<PendingCommandCompletion>,
}

#[derive(Debug, Clone)]
struct PendingCommandCompletion {
    conversation_id: AIConversationId,
    initial_requested_command_action_id: Option<AIAgentActionId>,
    prompt: String,
    completed_command: RunningCommand,
    exit_code: i32,
    final_turn_started: bool,
}

impl UserTakeOverReason {
    pub fn is_stop(&self) -> bool {
        matches!(self, Self::Stop)
    }

    pub fn is_transfer_from_agent(&self) -> bool {
        matches!(self, Self::TransferFromAgent { .. })
    }

    pub fn transfer_reason(&self) -> Option<&str> {
        match self {
            Self::TransferFromAgent { reason } => Some(reason.as_str()),
            Self::Manual | Self::Stop => None,
        }
    }
}

/// Represents which party is in control of the active long running command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LongRunningCommandControlState {
    /// The agent is in control.
    ///
    /// When the agent has control, the user cannot submit input to the command.
    Agent {
        /// `true` if the agent is blocked on approval from the user for submitting input.
        is_blocked: bool,
        /// `true` if agent responses should be hidden in the UI.
        should_hide_responses: bool,
    },
    /// The user is in control.
    User { reason: UserTakeOverReason },
}

impl LongRunningCommandControlState {
    pub fn is_agent_in_control(&self) -> bool {
        matches!(self, Self::Agent { .. })
    }

    pub fn is_agent_blocked(&self) -> bool {
        matches!(
            self,
            Self::Agent {
                is_blocked: true,
                ..
            }
        )
    }

    pub fn is_user_in_control(&self) -> bool {
        matches!(self, Self::User { .. })
    }

    pub fn should_hide_responses(&self) -> bool {
        matches!(
            self,
            Self::Agent {
                should_hide_responses: true,
                ..
            }
        )
    }

    pub fn user_take_over_reason(&self) -> Option<&UserTakeOverReason> {
        match &self {
            LongRunningCommandControlState::Agent { .. } => None,
            LongRunningCommandControlState::User { reason } => Some(reason),
        }
    }
}

/// Responsible for managing 'control' (e.g. write permissions) for the active long running
/// agent-requested command.
///
/// Control state is canonically stored on the relevant command `Block` owned by terminal model,
/// but wrapping update APIs in this controller ensures consistent update semantics and makes
/// control state updates subscribable.
pub struct CLISubagentController {
    controller: ModelHandle<BlocklistAIController>,
    action_model: ModelHandle<BlocklistAIActionModel>,
    agent_view_controller: Option<ModelHandle<AgentViewController>>,
    terminal_model: Arc<FairMutex<TerminalModel>>,
    terminal_view_id: EntityId,
    // Active or recently-active CLI subagent state, keyed by the associated block.
    active_subagents_by_block: HashMap<BlockId, ActiveCLISubagentState>,
}

impl CLISubagentController {
    pub fn new(
        controller: &ModelHandle<BlocklistAIController>,
        action_model: &ModelHandle<BlocklistAIActionModel>,
        agent_view_controller: Option<ModelHandle<AgentViewController>>,
        terminal_model: Arc<FairMutex<TerminalModel>>,
        model_event_dispatcher: &ModelHandle<ModelEventDispatcher>,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, Self::handle_history_model_event);
        ctx.subscribe_to_model(controller, |me, _, event, ctx| {
            let BlocklistAIControllerEvent::FinishedReceivingOutput {
                conversation_id, ..
            } = event
            else {
                return;
            };
            me.advance_completed_subagents(*conversation_id, ctx);
            me.ensure_monitor_continues(*conversation_id, ctx);
        });

        ctx.subscribe_to_model(action_model, |me, _, event, ctx| match event {
            BlocklistAIActionEvent::ActionBlockedOnUserConfirmation {
                action_id,
                conversation_id,
                ..
            } => {
                let mut terminal_model = me.terminal_model.lock();
                let active_block = terminal_model.block_list_mut().active_block_mut();
                if !matches_active_requested_command(
                    *conversation_id,
                    action_id,
                    active_block.ai_conversation_id(),
                    active_block.requested_command_action_id(),
                ) {
                    return;
                }
                active_block.update_is_agent_blocked(true);

                let action_id = active_block.requested_command_action_id().cloned();
                ctx.emit(CLISubagentEvent::UpdatedControl {
                    block_id: active_block.id().clone(),
                    requested_command_action_id: action_id,
                    agent_has_control: active_block.is_agent_in_control(),
                });
            }
            BlocklistAIActionEvent::ExecutingAction {
                action_id,
                conversation_id,
                ..
            } => {
                let mut terminal_model = me.terminal_model.lock();
                let active_block = terminal_model.block_list_mut().active_block_mut();
                if !matches_active_requested_command(
                    *conversation_id,
                    action_id,
                    active_block.ai_conversation_id(),
                    active_block.requested_command_action_id(),
                ) {
                    return;
                }
                active_block.update_is_agent_blocked(false);

                let action_id = active_block.requested_command_action_id().cloned();
                ctx.emit(CLISubagentEvent::UpdatedControl {
                    block_id: active_block.id().clone(),
                    requested_command_action_id: action_id,
                    agent_has_control: active_block.is_agent_in_control(),
                });
            }
            BlocklistAIActionEvent::FinishedAction {
                action_id: finished_action_id,
                conversation_id,
                ..
            } => {
                let action_result = me
                    .action_model
                    .as_ref(ctx)
                    .get_action_result(*conversation_id, finished_action_id);
                let initial_command_finished_without_snapshot =
                    action_result.is_some_and(|result| {
                        matches!(
                            &result.result,
                            AIAgentActionResultType::RequestCommandOutput(
                                RequestCommandOutputResult::Completed { .. }
                                    | RequestCommandOutputResult::CancelledBeforeExecution
                                    | RequestCommandOutputResult::Denylisted { .. }
                            )
                        )
                    });
                let snapshot_block_id = action_result
                    .and_then(|result| snapshot_block_id_for_action_result(&result.result))
                    .cloned();
                let command_finished_block_id = action_result
                    .and_then(|result| command_finished_block_id(&result.result))
                    .cloned();
                let mut terminal_model = me.terminal_model.lock();
                let active_block = terminal_model.block_list_mut().active_block_mut();
                if matches_active_requested_command(
                    *conversation_id,
                    finished_action_id,
                    active_block.ai_conversation_id(),
                    active_block.requested_command_action_id(),
                ) {
                    active_block.update_is_agent_blocked(false);

                    let active_command_action_id =
                        active_block.requested_command_action_id().cloned();
                    ctx.emit(CLISubagentEvent::UpdatedControl {
                        block_id: active_block.id().clone(),
                        requested_command_action_id: active_command_action_id,
                        agent_has_control: active_block.is_agent_in_control(),
                    });
                }

                // Updates the last snapshot timestamp for the active block after the agent has read the block output.
                if let Some(snapshot_block_id) = snapshot_block_id {
                    let state = me
                        .active_subagents_by_block
                        .entry(snapshot_block_id.clone())
                        .or_default();
                    state.last_snapshot_at = Some(Instant::now());
                    state.monitor_nudge_sent = false;
                    ctx.emit(CLISubagentEvent::UpdatedLastSnapshot);
                }
                if initial_command_finished_without_snapshot {
                    me.active_subagents_by_block.retain(|_, state| {
                        state.task_id.is_some()
                            || !matches_requested_command_identity(
                                *conversation_id,
                                finished_action_id,
                                state.initial_requested_command_conversation_id,
                                state.initial_requested_command_action_id.as_ref(),
                            )
                    });
                }
                drop(terminal_model);
                if let Some(block_id) = command_finished_block_id {
                    me.advance_completed_subagent(&block_id, ctx);
                }
            }
            _ => (),
        });

        ctx.subscribe_to_model(model_event_dispatcher, |me, _, event, ctx| {
            if let ModelEvent::BlockCompleted(block_completed_event) = event {
                let terminal_model = me.terminal_model.lock();
                let Some(block) = terminal_model
                    .block_list()
                    .block_with_id(&block_completed_event.block_id)
                else {
                    return;
                };

                let block_id = block.id().clone();
                let conversation_id = block.ai_conversation_id();
                let requested_command_action_id = block.requested_command_action_id().cloned();
                let should_skip_completion_assessment =
                    !should_request_completion_assessment(block.long_running_control_state());
                let completion = match (&block_completed_event.block_type, conversation_id) {
                    (BlockType::User(completed), Some(conversation_id)) => {
                        let command = if completed.command_with_obfuscated_secrets.is_empty() {
                            completed.command.clone()
                        } else {
                            completed.command_with_obfuscated_secrets.clone()
                        };
                        let output = completed
                            .output_truncated_with_obfuscated_secrets
                            .clone();
                        let exit_code = completed.serialized_block.exit_code.value();
                        Some(PendingCommandCompletion {
                            conversation_id,
                            initial_requested_command_action_id: requested_command_action_id
                                .clone(),
                            prompt: format!(
                                "The monitored command has finished with exit code {exit_code}. \
                                 Give the user a concise final assessment grounded in the final \
                                 output below. Do not call another shell tool or restart the \
                                 command.\n\nCommand:\n```sh\n{command}\n```\n\nFinal output:\n```text\n{output}\n```"
                            ),
                            completed_command: RunningCommand {
                                command,
                                block_id: block_id.clone(),
                                grid_contents: output,
                                cursor: String::new(),
                                requested_command_id: requested_command_action_id.clone(),
                                is_alt_screen_active: false,
                            },
                            exit_code,
                            final_turn_started: false,
                        })
                    }
                    (
                        BlockType::BootstrapHidden
                        | BlockType::BootstrapVisible(_)
                        | BlockType::Restored
                        | BlockType::InBandCommand
                        | BlockType::Background(_)
                        | BlockType::Static,
                        _,
                    )
                    | (BlockType::User(_), None) => None,
                };
                drop(terminal_model);

                let provider_accepted_completion = completion.as_ref().is_some_and(|completion| {
                    let provider_completion = PendingProviderCommandCompletion::new(
                        completion.completed_command.block_id.clone(),
                        completion.initial_requested_command_action_id.clone(),
                        completion.completed_command.command.clone(),
                        completion.completed_command.grid_contents.clone(),
                        completion.exit_code,
                    );
                    me.controller.update(ctx, |controller, ctx| {
                        controller.offer_provider_command_completion(
                            completion.conversation_id,
                            provider_completion,
                            ctx,
                        )
                    })
                });
                let has_last_snapshot = me
                    .active_subagents_by_block
                    .get(&block_id)
                    .is_some_and(|state| state.last_snapshot_at.is_some());
                if has_last_snapshot {
                    ctx.emit(CLISubagentEvent::UpdatedLastSnapshot);
                }
                if provider_accepted_completion {
                    // The provider controller owns deactivation after it applies the queued
                    // completion at a safe run boundary.
                    return;
                }
                if !me.active_subagents_by_block.contains_key(&block_id) {
                    return;
                }

                // A Stop takeover intentionally cancels the subagent. The command may still
                // finish later, but that completion must not start a new assessment turn. Also
                // clean up the in-memory monitor state so the stopped subagent cannot linger in
                // the UI or intercept later refreshes.
                if should_skip_completion_assessment {
                    me.finish_subagent(
                        &block_id,
                        conversation_id,
                        requested_command_action_id,
                        ctx,
                    );
                    return;
                }

                let has_completion = {
                    let Some(subagent_state) = me.active_subagents_by_block.get_mut(&block_id)
                    else {
                        return;
                    };
                    subagent_state.completion = completion;
                    subagent_state.completion.is_some()
                };
                if !has_completion {
                    log::warn!(
                        "CLI monitor block {block_id:?} completed without final command metadata"
                    );
                    me.finish_subagent(
                        &block_id,
                        conversation_id,
                        requested_command_action_id,
                        ctx,
                    );
                    return;
                }
                me.advance_completed_subagent(&block_id, ctx);
            }
        });

        Self {
            controller: controller.clone(),
            action_model: action_model.clone(),
            agent_view_controller,
            terminal_model,
            terminal_view_id,
            active_subagents_by_block: HashMap::new(),
        }
    }

    fn advance_completed_subagents(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let block_ids = self
            .active_subagents_by_block
            .iter()
            .filter_map(|(block_id, state)| {
                state
                    .completion
                    .as_ref()
                    .is_some_and(|completion| completion.conversation_id == conversation_id)
                    .then_some(block_id.clone())
            })
            .collect::<Vec<_>>();
        for block_id in block_ids {
            self.advance_completed_subagent(&block_id, ctx);
        }
    }

    fn advance_completed_subagent(&mut self, block_id: &BlockId, ctx: &mut ModelContext<Self>) {
        let Some(completion) = self
            .active_subagents_by_block
            .get(block_id)
            .and_then(|state| state.completion.as_ref().cloned())
        else {
            return;
        };

        let has_active_stream = self
            .controller
            .as_ref(ctx)
            .has_active_stream_for_conversation(completion.conversation_id, ctx);
        let has_unfinished_action = self
            .action_model
            .as_ref(ctx)
            .has_unfinished_actions_for_conversation(completion.conversation_id);
        if has_active_stream || has_unfinished_action {
            return;
        }

        if completion.final_turn_started {
            self.finish_subagent(
                block_id,
                Some(completion.conversation_id),
                completion.initial_requested_command_action_id,
                ctx,
            );
            return;
        }

        let sent = self.controller.update(ctx, |controller, ctx| {
            controller.send_command_completion_assessment(
                completion.conversation_id,
                completion.prompt,
                completion.completed_command,
                ctx,
            )
        });
        if sent {
            if let Some(completion) = self
                .active_subagents_by_block
                .get_mut(block_id)
                .and_then(|state| state.completion.as_mut())
            {
                completion.final_turn_started = true;
            }
        }
    }

    /// A monitor turn that returns only prose has no action result to trigger the normal
    /// action-follow-up path. Nudge that monitor once with the live command context so a model
    /// that acknowledged the first snapshot without polling gets another chance to inspect it.
    fn ensure_monitor_continues(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(block_id) = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&conversation_id)
            .and_then(|conversation| {
                conversation.all_tasks().find_map(|task| {
                    let block_id = task.cli_subagent_block_id()?;
                    let state = self.active_subagents_by_block.get(&block_id)?;
                    if state.task_id.as_ref() != Some(task.id()) || state.completion.is_some() {
                        return None;
                    }
                    let last_exchange_has_action = task.last_exchange().is_some_and(|exchange| {
                        exchange
                            .output_status
                            .output()
                            .is_some_and(|output| output.get().actions().next().is_some())
                    });
                    should_nudge_monitor_turn(last_exchange_has_action, state.monitor_nudge_sent)
                        .then_some(block_id)
                })
            })
        else {
            return;
        };

        if self
            .controller
            .as_ref(ctx)
            .has_active_provider_run(conversation_id)
            || self
                .controller
                .as_ref(ctx)
                .has_active_stream_for_conversation(conversation_id, ctx)
            || self
                .action_model
                .as_ref(ctx)
                .has_unfinished_actions_for_conversation(conversation_id)
        {
            return;
        }

        let command_is_still_agent_controlled = {
            let terminal_model = self.terminal_model.lock();
            terminal_model
                .block_list()
                .block_with_id(&block_id)
                .is_some_and(|block| {
                    block.is_active_and_long_running()
                        && block.is_agent_in_control()
                        && block.ai_conversation_id() == Some(conversation_id)
                })
        };
        if !command_is_still_agent_controlled {
            return;
        }

        if let Some(state) = self.active_subagents_by_block.get_mut(&block_id) {
            state.monitor_nudge_sent = true;
        }
        self.controller.update(ctx, |controller, ctx| {
            controller.send_cli_monitor_nudge(conversation_id, ctx);
        });
    }

    fn finish_subagent(
        &mut self,
        block_id: &BlockId,
        conversation_id: Option<AIConversationId>,
        initial_requested_command_action_id: Option<AIAgentActionId>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(state) = self.active_subagents_by_block.remove(block_id) else {
            return;
        };
        let conversation_id = conversation_id.or_else(|| {
            state
                .completion
                .as_ref()
                .map(|completion| completion.conversation_id)
        });
        let initial_requested_command_action_id = initial_requested_command_action_id
            .or_else(|| {
                state
                    .completion
                    .as_ref()
                    .and_then(|completion| completion.initial_requested_command_action_id.clone())
            })
            .or(state.initial_requested_command_action_id);

        if let Some(conversation_id) = conversation_id {
            let deactivate_result =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, _| {
                    history_model
                        .deactivate_cli_subagent_task_for_conversation(block_id, conversation_id)
                });
            if let Err(error) = deactivate_result {
                log::error!("Failed to deactivate CLI monitor for block {block_id:?}: {error:?}");
            }
        }

        ctx.emit(CLISubagentEvent::FinishedSubagent {
            block_id: block_id.clone(),
            conversation_id,
            initial_requested_command_action_id,
        });

        if let (Some(agent_view_controller), Some(conversation_id)) =
            (&self.agent_view_controller, conversation_id)
        {
            agent_view_controller.update(ctx, |controller, ctx| {
                let is_this_inline_conversation = controller.is_inline()
                    && controller.agent_view_state().active_conversation_id()
                        == Some(conversation_id);
                if is_this_inline_conversation {
                    controller.exit_agent_view(ctx);
                }
            });
        }
    }

    pub fn is_agent_in_control(&self) -> bool {
        let terminal_model = self.terminal_model.lock();
        terminal_model
            .block_list()
            .active_block()
            .is_agent_in_control()
    }

    pub(crate) fn is_agent_in_control_or_tagged_in(&self) -> bool {
        let terminal_model = self.terminal_model.lock();
        terminal_model
            .block_list()
            .active_block()
            .is_agent_in_control_or_tagged_in()
    }

    pub fn last_snapshot_at(&self, block_id: &BlockId) -> Option<Instant> {
        self.active_subagents_by_block
            .get(block_id)
            .and_then(|state| state.last_snapshot_at)
    }

    /// Begins tracking an agent-requested command before its shell event is dispatched.
    ///
    /// The placeholder lets command completion and action-result events arrive in either order
    /// without losing the completion that a subsequently-created CLI monitor needs.
    pub fn track_requested_command(
        &mut self,
        block_id: &BlockId,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) {
        let state = self
            .active_subagents_by_block
            .entry(block_id.clone())
            .or_default();
        state.initial_requested_command_conversation_id = Some(conversation_id);
        state.initial_requested_command_action_id = Some(action_id.clone());
    }

    /// Force the currently in-flight poll for the given long-running command block to
    /// resolve immediately with a fresh snapshot, bypassing the agent-set timeout.
    /// Backs the `Check now` affordance surfaced next to the `Last seen by agent ...`
    /// indicator in the command status footer. Returns whether a matching poll was refreshed.
    pub fn request_force_refresh(
        &mut self,
        block_id: &BlockId,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let executor_handle = self.action_model.as_ref(ctx).shell_command_executor(ctx);
        let block_id = block_id.clone();
        let refreshed =
            executor_handle.update(ctx, |executor, _| executor.force_refresh_block(&block_id));
        if refreshed {
            self.active_subagents_by_block.entry(block_id).or_default();
        }
        refreshed
    }

    pub fn switch_control_to_user(&self, reason: UserTakeOverReason, ctx: &mut ModelContext<Self>) {
        let should_cancel_conversation = !reason.is_transfer_from_agent();
        let mut terminal_model = self.terminal_model.lock();

        let active_block = terminal_model.block_list_mut().active_block_mut();
        let block_id = active_block.id().clone();
        let interaction_mode_debug = format!("{:?}", active_block.interaction_mode());
        let lrc_state_debug = format!("{:?}", active_block.long_running_control_state());
        if let Err(e) = active_block.take_over_control_for_user(reason.clone()) {
            log::error!(
                "Failed to take control for user: {e:?}, reason={reason:?}, \
                 block_id={block_id:?}, interaction_mode={interaction_mode_debug}, \
                 lrc_state={lrc_state_debug}"
            );
            return;
        }

        let action_id = active_block.requested_command_action_id().cloned();
        let conversation_id = active_block.ai_conversation_id();
        let agent_has_control = active_block.is_agent_in_control();
        if let Some(action_id) = &action_id {
            terminal_model
                .block_list_mut()
                .set_visibility_of_block_for_ai_action(action_id, true);
        }
        // Conversation cancellation potentially takes a lock on terminal model if the
        // cancelled action is a shell command action, so we have to drop the terminal
        // model lock before actually cancelling the conversation.
        drop(terminal_model);

        // Cancel the in-flight stream to stop the CLI subagent monitoring loop.
        // When the user manually takes over, we use CLISubagentUserTakeover so the
        // conversation status stays InProgress — the agent will resume once the command
        // finishes or the user hands control back. We do NOT use ManuallyCancelled here
        // because that would mark the conversation (and ambient task) as cancelled,
        // which is incorrect since the conversation is still proceeding.
        if should_cancel_conversation {
            if let Some(conversation_id) = conversation_id {
                self.controller.update(ctx, |controller, ctx| {
                    controller.cancel_conversation_progress(
                        conversation_id,
                        CancellationReason::CLISubagentUserTakeover,
                        ctx,
                    );
                });
            }
        }

        ctx.emit(CLISubagentEvent::UpdatedControl {
            block_id: block_id.clone(),
            requested_command_action_id: action_id,
            agent_has_control,
        });

        send_telemetry_from_ctx!(
            TelemetryEvent::CLISubagentControlStateChanged {
                conversation_id,
                block_id,
                control_state: CLISubagentControlState::UserInControl,
            },
            ctx
        );
    }

    pub fn handoff_active_command_control_to_agent(&self, ctx: &mut ModelContext<Self>) {
        let mut terminal_model = self.terminal_model.lock();

        let active_block = terminal_model.block_list_mut().active_block_mut();
        let conversation_id = active_block.ai_conversation_id();
        let block_id = active_block.id().clone();
        let lrc_state_debug = format!("{:?}", active_block.long_running_control_state());
        // Check if control was transferred from agent before handoff.
        let was_transfer_from_agent = active_block
            .long_running_control_state()
            .and_then(|state| state.user_take_over_reason())
            .is_some_and(|reason| reason.is_transfer_from_agent());
        if let Err(e) = active_block.handoff_control_to_agent() {
            log::error!(
                "Failed to handoff control to agent: {e:?}, \
                 block_id={block_id:?}, lrc_state={lrc_state_debug}"
            );
            return;
        }
        log::info!(
            "handoff_active_command_control_to_agent: block_id={block_id:?}, \
             conversation_id={conversation_id:?}, lrc_state={lrc_state_debug}"
        );
        let action_id = active_block.requested_command_action_id().cloned();
        let agent_has_control = active_block.is_agent_in_control();
        if let Some(action_id) = &action_id {
            terminal_model
                .block_list_mut()
                .set_visibility_of_block_for_ai_action(action_id, false);
        }
        drop(terminal_model);
        if let Some(agent_view_controller) = &self.agent_view_controller {
            agent_view_controller.update(ctx, |controller, ctx| {
                if !controller.is_inline() {
                    if let Err(e) = controller.try_enter_inline_agent_view(
                        conversation_id,
                        AgentViewEntryOrigin::LongRunningCommand,
                        ctx,
                    ) {
                        log::error!("Failed to enter inline agent view for LRC handoff: {e}");
                    }
                }
            });
        }

        // Trigger an auto-resume of the conversation when handing control to the agent.
        if let Some(conversation_id) = conversation_id {
            let is_viewing_shared_session = BlocklistAIHistoryModel::as_ref(ctx)
                .conversation(&conversation_id)
                .is_some_and(|conversation| conversation.is_viewing_shared_session());
            if !is_viewing_shared_session {
                let resume_context = {
                    let terminal_model = self.terminal_model.lock();
                    block_context_from_terminal_model(&terminal_model, &block_id, false)
                        .map(Box::new)
                        .map(AIAgentContext::Block)
                        .into_iter()
                        .collect()
                };
                self.controller.update(ctx, |controller, ctx| {
                    controller.resume_conversation(conversation_id, resume_context, ctx);
                });
            }
        }

        ctx.emit(CLISubagentEvent::UpdatedControl {
            block_id: block_id.clone(),
            requested_command_action_id: action_id,
            agent_has_control,
        });

        // Emit a special event if control was transferred from agent, so the executor can be notified.
        if was_transfer_from_agent {
            ctx.emit(CLISubagentEvent::ControlHandedBackAfterTransfer);
        }

        send_telemetry_from_ctx!(
            TelemetryEvent::CLISubagentControlStateChanged {
                conversation_id,
                block_id,
                control_state: CLISubagentControlState::AgentInControl,
            },
            ctx
        );
    }

    pub fn toggle_hide_responses(&self, ctx: &mut ModelContext<Self>) {
        let mut terminal_model = self.terminal_model.lock();
        let active_block = terminal_model.block_list_mut().active_block_mut();

        if active_block.toggle_subagent_response_visibility() {
            let conversation_id = active_block.ai_conversation_id();
            let block_id = active_block.id().clone();
            let is_hidden = active_block.should_hide_responses();

            ctx.emit(CLISubagentEvent::ToggledHideResponses);

            if let Some(conversation_id) = conversation_id {
                send_telemetry_from_ctx!(
                    TelemetryEvent::CLISubagentResponsesToggled {
                        conversation_id,
                        block_id,
                        is_hidden,
                    },
                    ctx
                );
            }
        }
    }

    fn spawn_cli_subagent_for_task_if_ready(
        &mut self,
        conversation_id: AIConversationId,
        task_id: &TaskId,
        ctx: &mut ModelContext<Self>,
    ) {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        let Some(conversation) = history_model.as_ref(ctx).conversation(&conversation_id) else {
            return;
        };
        let Some(task) = conversation.get_task(task_id) else {
            return;
        };
        let Some(cli_subagent_block_id) = task.cli_subagent_block_id() else {
            return;
        };

        // The direct-provider action-result path creates the optimistic task before appending its
        // first exchange. Depending on event delivery order, CreatedSubtask can therefore arrive
        // before the view model is constructible. AppendedExchange retries this same idempotent
        // path.
        if task.last_exchange().is_none()
            || conversation
                .is_subagent_task_finished(task_id)
                .unwrap_or(true)
            || self
                .active_subagents_by_block
                .get(&cli_subagent_block_id)
                .and_then(|state| state.task_id.as_ref())
                == Some(task_id)
        {
            return;
        }

        let mut terminal_model = self.terminal_model.lock();
        let Some(block) = terminal_model
            .block_list_mut()
            .mut_block_from_id(&cli_subagent_block_id)
        else {
            return;
        };
        let block_id = block.id().clone();
        if let Err(e) =
            block.set_agent_interaction_mode_for_agent_monitored_command(task_id, conversation_id)
        {
            log::error!("Could not update interaction mode to agent-monitored: {e:?}",);
            return;
        };

        let action_id = block.requested_command_action_id().cloned();
        let agent_has_control = block.is_agent_in_control();
        drop(terminal_model);

        // When the CLI subagent is first created for a long running command,
        // the agent now has control. Emit an UpdatedControl event so that
        // shared-session state can reflect this initial control state.
        ctx.emit(CLISubagentEvent::UpdatedControl {
            block_id: block_id.clone(),
            requested_command_action_id: action_id.clone(),
            agent_has_control,
        });
        self.active_subagents_by_block
            .entry(block_id.clone())
            .or_default()
            .initial_requested_command_conversation_id = Some(conversation_id);
        self.active_subagents_by_block
            .entry(block_id.clone())
            .or_default()
            .task_id = Some(task_id.clone());

        ctx.emit(CLISubagentEvent::SpawnedSubagent {
            task_id: task_id.clone(),
            conversation_id,
            block_id,
            initial_requested_command_action_id: action_id,
        });
        self.advance_completed_subagent(&cli_subagent_block_id, ctx);
    }

    fn handle_history_model_event(
        &mut self,
        _: ModelHandle<BlocklistAIHistoryModel>,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if event
            .terminal_surface_id()
            .is_some_and(|id| id != self.terminal_view_id)
        {
            return;
        }
        match event {
            BlocklistAIHistoryEvent::CreatedSubtask {
                task_id,
                conversation_id,
                ..
            }
            | BlocklistAIHistoryEvent::AppendedExchange {
                task_id,
                conversation_id,
                ..
            } => self.spawn_cli_subagent_for_task_if_ready(*conversation_id, task_id, ctx),
            BlocklistAIHistoryEvent::UpgradedTask {
                optimistic_id: old_id,
                server_id: new_id,
                ..
            } => {
                let block_id =
                    self.active_subagents_by_block
                        .iter()
                        .find_map(|(block_id, state)| {
                            (state.task_id.as_ref() == Some(old_id)).then_some(block_id.clone())
                        });
                if let Some(block_id) = block_id {
                    let mut terminal_model = self.terminal_model.lock();
                    if let Some(block) =
                        terminal_model.block_list_mut().mut_block_from_id(&block_id)
                    {
                        match block.upgrade_cli_subagent_task_id(new_id.clone()) {
                            Ok(()) => {
                                if let Some(state) =
                                    self.active_subagents_by_block.get_mut(&block_id)
                                {
                                    state.task_id = Some(new_id.clone());
                                }
                            }
                            Err(e) => {
                                log::error!(
                                    "Tried to upgrade CLISubagent task ID for non-existent block: {e:?}"
                                );
                            }
                        }
                    }
                }
            }
            _ => (),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CLISubagentEvent {
    // Emitted when a CLI subagent is spawned for a running command block.
    SpawnedSubagent {
        task_id: TaskId,
        block_id: BlockId,
        conversation_id: AIConversationId,

        /// The ID of the requested command for which this subagent was spawned, if any.
        ///
        /// None if the subagent was spawned by entering agent mode during a user-executed command,
        /// rather than a requested command.
        initial_requested_command_action_id: Option<AIAgentActionId>,
    },
    // Emitted when a CLI subagent's execution ends.
    FinishedSubagent {
        block_id: BlockId,
        conversation_id: Option<AIConversationId>,
        initial_requested_command_action_id: Option<AIAgentActionId>,
    },
    UpdatedControl {
        block_id: BlockId,
        requested_command_action_id: Option<AIAgentActionId>,
        agent_has_control: bool,
    },
    UpdatedLastSnapshot,
    ToggledHideResponses,
    /// Emitted when the user hands control back to the agent after a
    /// TransferShellCommandControlToUser action.
    ControlHandedBackAfterTransfer,
}

impl Entity for CLISubagentController {
    type Event = CLISubagentEvent;
}

fn snapshot_block_id_for_action_result(result: &AIAgentActionResultType) -> Option<&BlockId> {
    // Enumerates all possible action result types that read a command output.
    match result {
        AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::LongRunningCommandSnapshot { block_id, .. },
        ) => Some(block_id),
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Snapshot { block_id, .. },
        ) => Some(block_id),
        AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::LongRunningCommandSnapshot { block_id, .. },
        ) => Some(block_id),
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Snapshot { block_id, .. },
        ) => Some(block_id),
        _ => None,
    }
}

fn command_finished_block_id(result: &AIAgentActionResultType) -> Option<&BlockId> {
    match result {
        AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
            block_id,
            ..
        })
        | AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::CommandFinished { block_id, .. },
        )
        | AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::CommandFinished { block_id, .. },
        )
        | AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::CommandFinished { block_id, .. },
        ) => Some(block_id),
        AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::LongRunningCommandSnapshot { .. }
            | RequestCommandOutputResult::CancelledBeforeExecution
            | RequestCommandOutputResult::ExecutionError { .. }
            | RequestCommandOutputResult::Denylisted { .. },
        )
        | AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Snapshot { .. }
            | WriteToLongRunningShellCommandResult::Cancelled
            | WriteToLongRunningShellCommandResult::Error(_),
        )
        | AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::LongRunningCommandSnapshot { .. }
            | ReadShellCommandOutputResult::Cancelled
            | ReadShellCommandOutputResult::Error(_),
        )
        | AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Snapshot { .. }
            | TransferShellCommandControlToUserResult::Cancelled
            | TransferShellCommandControlToUserResult::Error(_),
        )
        | AIAgentActionResultType::RequestFileEdits(_)
        | AIAgentActionResultType::ReadFiles(_)
        | AIAgentActionResultType::UploadArtifact(_)
        | AIAgentActionResultType::SearchCodebase(_)
        | AIAgentActionResultType::Grep(_)
        | AIAgentActionResultType::FileGlob(_)
        | AIAgentActionResultType::FileGlobV2(_)
        | AIAgentActionResultType::ReadMCPResource(_)
        | AIAgentActionResultType::CallMCPTool(_)
        | AIAgentActionResultType::ReadSkill(_)
        | AIAgentActionResultType::SuggestNewConversation(_)
        | AIAgentActionResultType::SuggestPrompt(_)
        | AIAgentActionResultType::OpenCodeReview
        | AIAgentActionResultType::InitProject
        | AIAgentActionResultType::ReadDocuments(_)
        | AIAgentActionResultType::EditDocuments(_)
        | AIAgentActionResultType::CreateDocuments(_)
        | AIAgentActionResultType::UseComputer(_)
        | AIAgentActionResultType::InsertReviewComments(_)
        | AIAgentActionResultType::RequestComputerUse(_)
        | AIAgentActionResultType::FetchConversation(_)
        | AIAgentActionResultType::StartAgent(_)
        | AIAgentActionResultType::SendMessageToAgent(_)
        | AIAgentActionResultType::AskUserQuestion(_)
        | AIAgentActionResultType::RunAgents(_)
        | AIAgentActionResultType::WaitForEvents(_) => None,
    }
}

fn should_request_completion_assessment(
    control_state: Option<&LongRunningCommandControlState>,
) -> bool {
    !control_state
        .and_then(LongRunningCommandControlState::user_take_over_reason)
        .is_some_and(UserTakeOverReason::is_stop)
}

fn should_nudge_monitor_turn(last_exchange_has_action: bool, monitor_nudge_sent: bool) -> bool {
    !last_exchange_has_action && !monitor_nudge_sent
}

fn matches_active_requested_command(
    event_conversation_id: AIConversationId,
    event_action_id: &AIAgentActionId,
    active_conversation_id: Option<AIConversationId>,
    active_requested_command_id: Option<&AIAgentActionId>,
) -> bool {
    active_conversation_id == Some(event_conversation_id)
        && active_requested_command_id == Some(event_action_id)
}

fn matches_requested_command_identity(
    event_conversation_id: AIConversationId,
    event_action_id: &AIAgentActionId,
    requested_command_conversation_id: Option<AIConversationId>,
    requested_command_action_id: Option<&AIAgentActionId>,
) -> bool {
    requested_command_conversation_id == Some(event_conversation_id)
        && requested_command_action_id == Some(event_action_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_takeover_does_not_request_a_completion_assessment() {
        let state = LongRunningCommandControlState::User {
            reason: UserTakeOverReason::Stop,
        };

        assert!(!should_request_completion_assessment(Some(&state)));
    }

    #[test]
    fn non_stop_control_states_can_request_a_completion_assessment() {
        let agent_state = LongRunningCommandControlState::Agent {
            is_blocked: false,
            should_hide_responses: false,
        };
        let transfer_state = LongRunningCommandControlState::User {
            reason: UserTakeOverReason::TransferFromAgent {
                reason: "needs user input".to_owned(),
            },
        };

        assert!(should_request_completion_assessment(None));
        assert!(should_request_completion_assessment(Some(&agent_state)));
        assert!(should_request_completion_assessment(Some(&transfer_state)));
    }

    #[test]
    fn prose_monitor_turn_is_nudged_once_until_a_tool_action_runs() {
        assert!(should_nudge_monitor_turn(false, false));
        assert!(!should_nudge_monitor_turn(false, true));
        assert!(!should_nudge_monitor_turn(true, false));
    }

    #[test]
    fn shell_control_event_must_match_conversation_and_requested_command() {
        let active_conversation_id = AIConversationId::new();
        let other_conversation_id = AIConversationId::new();
        let active_action_id = AIAgentActionId::from("same-action".to_owned());
        let other_action_id = AIAgentActionId::from("other-action".to_owned());

        assert!(matches_active_requested_command(
            active_conversation_id,
            &active_action_id,
            Some(active_conversation_id),
            Some(&active_action_id),
        ));
        assert!(!matches_active_requested_command(
            other_conversation_id,
            &active_action_id,
            Some(active_conversation_id),
            Some(&active_action_id),
        ));
        assert!(!matches_active_requested_command(
            active_conversation_id,
            &other_action_id,
            Some(active_conversation_id),
            Some(&active_action_id),
        ));
    }

    #[test]
    fn requested_command_identity_rejects_duplicate_id_from_another_conversation() {
        let active_conversation_id = AIConversationId::new();
        let other_conversation_id = AIConversationId::new();
        let duplicate_action_id = AIAgentActionId::from("duplicate-action".to_owned());

        assert!(matches_requested_command_identity(
            active_conversation_id,
            &duplicate_action_id,
            Some(active_conversation_id),
            Some(&duplicate_action_id),
        ));
        assert!(!matches_requested_command_identity(
            other_conversation_id,
            &duplicate_action_id,
            Some(active_conversation_id),
            Some(&duplicate_action_id),
        ));
    }
}
