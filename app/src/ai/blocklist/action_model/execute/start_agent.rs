use futures::{future::BoxFuture, FutureExt};
use galaxyui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent::conversation::{AIConversation, AIConversationId, ConversationStatus};
use crate::ai::agent::{
    AIAgentAction, AIAgentActionResultType, AIAgentActionType, LifecycleEventType,
    StartAgentExecutionMode, StartAgentResult,
};
use crate::ai::blocklist::orchestration_event_streamer::OrchestrationEventStreamer;
use crate::ai::blocklist::orchestration_events::OrchestrationEventService;
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use galaxy_cli::agent::Harness;
use galaxy_core::features::FeatureFlag;

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};

/// The result sent back to the executor after observing the child agent's lifecycle.
enum StartAgentDecision {
    /// The child conversation was created successfully.
    Started { agent_id: String },
    /// The child agent completed and here is its output (local/blocking mode).
    Completed { agent_id: String, output: String },
    /// An error occurred while starting the agent.
    Error(String),
}

fn invalid_local_child_harness_error(harness_type: &str) -> String {
    let harness_name = harness_type.trim();
    if harness_name.is_empty() {
        "Local child harness type is missing.".to_string()
    } else {
        format!("Unsupported local child harness '{harness_name}'.")
    }
}

/// Groups the data for a single StartAgent invocation as it flows from the
/// executor through the terminal view and pane group into the controller.
#[derive(Clone)]
pub struct StartAgentRequest {
    pub name: String,
    pub prompt: String,
    pub execution_mode: StartAgentExecutionMode,
    pub lifecycle_subscription: Option<Vec<LifecycleEventType>>,
    pub parent_conversation_id: AIConversationId,
    pub parent_run_id: Option<String>,
}

/// Tracks a single in-flight StartAgent action.
struct PendingStartAgent {
    parent_conversation_id: AIConversationId,
    /// Set when `StartedNewConversation` fires for a conversation whose
    /// `parent_conversation_id` matches.
    child_conversation_id: Option<AIConversationId>,
    sender: async_channel::Sender<StartAgentDecision>,
    /// When true, the executor blocks until the child agent finishes and
    /// returns its output as the tool result (local Bedrock mode).
    wait_for_completion: bool,
}

pub struct StartAgentExecutor {
    /// All in-flight StartAgent actions. Multiple agents can be spawned
    /// concurrently from the same parent.
    pending: Vec<PendingStartAgent>,
}

impl StartAgentExecutor {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, Self::handle_history_event);

        Self { pending: Vec::new() }
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            BlocklistAIHistoryEvent::StartedNewConversation {
                new_conversation_id,
                ..
            } => {
                if self.pending.is_empty() {
                    return;
                }
                let history = BlocklistAIHistoryModel::as_ref(ctx);
                let Some(conversation) = history.conversation(new_conversation_id) else {
                    return;
                };
                let parent_id = conversation.parent_conversation_id();
                // Find the first pending entry that matches this parent and hasn't been assigned a child yet.
                if let Some(pending) = self.pending.iter_mut().find(|p| {
                    p.child_conversation_id.is_none()
                        && parent_id == Some(p.parent_conversation_id)
                }) {
                    pending.child_conversation_id = Some(*new_conversation_id);
                }
            }
            BlocklistAIHistoryEvent::ConversationServerTokenAssigned {
                conversation_id, ..
            } => {
                let Some(idx) = self.pending.iter().position(|p| {
                    p.child_conversation_id.as_ref() == Some(conversation_id)
                }) else {
                    return;
                };
                // Don't remove yet if we're waiting for completion — we need
                // the entry to stay so UpdatedConversationStatus can find it.
                if self.pending[idx].wait_for_completion {
                    // Just log and continue — we'll resolve on Success status.
                    let conversation = BlocklistAIHistoryModel::as_ref(ctx)
                        .conversation(conversation_id);
                    let agent_id = conversation
                        .and_then(|c| c.orchestration_agent_id())
                        .or_else(|| {
                            conversation.and_then(|c| {
                                c.server_conversation_token()
                                    .map(|t| t.as_str().to_string())
                            })
                        })
                        .unwrap_or_else(|| conversation_id.to_string());
                    log::info!(
                        "[start_agent] Child agent started: conversation_id={:?}, agent_id={}",
                        conversation_id,
                        agent_id
                    );
                    log::info!(
                        "[start_agent] Local mode: waiting for child {:?} to complete",
                        conversation_id
                    );
                    return;
                }
                let pending = self.pending.remove(idx);
                let conversation = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation(conversation_id);
                // orchestration_agent_id() uses run_id in v2 mode, which won't
                // exist for locally-spawned Bedrock child agents. Fall back to
                // the server conversation token (set by the stream Init event)
                // or the conversation ID itself as the agent identifier.
                let agent_id = conversation
                    .and_then(|c| c.orchestration_agent_id())
                    .or_else(|| {
                        conversation.and_then(|c| {
                            c.server_conversation_token()
                                .map(|t| t.as_str().to_string())
                        })
                    })
                    .unwrap_or_else(|| conversation_id.to_string());

                log::info!(
                    "[start_agent] Child agent started: conversation_id={:?}, agent_id={}",
                    conversation_id,
                    agent_id
                );

                // Only register with the orchestration streamer if the parent
                // has a run_id (server-assigned). Local Bedrock child agents
                // don't have server-side orchestration — attempting to open an
                // SSE stream to a non-existent server freezes the app.
                let parent_has_run_id = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation(&pending.parent_conversation_id)
                    .and_then(|c| c.run_id())
                    .is_some();

                if pending.wait_for_completion {
                    // Local mode: don't resolve yet — wait for the child to
                    // finish (UpdatedConversationStatus → Success) so we can
                    // return its output as the tool result.
                    log::info!(
                        "[start_agent] Local mode: waiting for child {:?} to complete",
                        conversation_id
                    );
                } else {
                    let _ = pending.sender.try_send(StartAgentDecision::Started {
                        agent_id: agent_id.clone(),
                    });
                    if parent_has_run_id {
                        if FeatureFlag::OrchestrationV2.is_enabled() {
                            OrchestrationEventStreamer::handle(ctx).update(ctx, |streamer, ctx| {
                                streamer.register_watched_run_id(
                                    pending.parent_conversation_id,
                                    agent_id,
                                    ctx,
                                );
                            });
                        } else {
                            OrchestrationEventService::handle(ctx).update(ctx, |svc, ctx| {
                                svc.emit_child_startup_started(*conversation_id, ctx);
                            });
                        }
                    }
                }
            }
            BlocklistAIHistoryEvent::UpdatedConversationStatus {
                conversation_id, ..
            } => {
                let Some(idx) = self.pending.iter().position(|p| {
                    p.child_conversation_id.as_ref() == Some(conversation_id)
                }) else {
                    return;
                };
                let history = BlocklistAIHistoryModel::as_ref(ctx);
                let Some(conversation) = history.conversation(conversation_id) else {
                    return;
                };

                match conversation.status() {
                    ConversationStatus::Success => {
                        if !self.pending[idx].wait_for_completion {
                            // Non-blocking mode — already resolved on token assignment.
                            return;
                        }
                        let pending = self.pending.remove(idx);
                        // Extract the child's text output from its last exchange.
                        let output = extract_child_output(conversation);
                        log::info!(
                            "[start_agent] Child agent {:?} completed with {} chars of output",
                            conversation_id,
                            output.len()
                        );
                        let agent_id = conversation
                            .orchestration_agent_id()
                            .or_else(|| {
                                conversation
                                    .server_conversation_token()
                                    .map(|t| t.as_str().to_string())
                            })
                            .unwrap_or_else(|| conversation_id.to_string());
                        let _ = pending.sender.try_send(StartAgentDecision::Completed {
                            agent_id,
                            output,
                        });
                    }
                    status => {
                        let error_msg = start_agent_error_message_for_status(
                            status,
                            conversation.status_error_message(),
                        );
                        if let Some(error_msg) = error_msg {
                            let pending = self.pending.remove(idx);
                            let _ = pending
                                .sender
                                .try_send(StartAgentDecision::Error(error_msg.clone()));
                            if !FeatureFlag::OrchestrationV2.is_enabled() {
                                OrchestrationEventService::handle(ctx).update(ctx, |svc, ctx| {
                                    svc.emit_child_startup_errored(
                                        *conversation_id,
                                        "conversation_status".to_string(),
                                        error_msg,
                                        ctx,
                                    );
                                });
                            }
                        }
                    }
                }
            }
            BlocklistAIHistoryEvent::CreatedSubtask { .. }
            | BlocklistAIHistoryEvent::UpgradedTask { .. }
            | BlocklistAIHistoryEvent::AppendedExchange { .. }
            | BlocklistAIHistoryEvent::ReassignedExchange { .. }
            | BlocklistAIHistoryEvent::UpdatedStreamingExchange { .. }
            | BlocklistAIHistoryEvent::SetActiveConversation { .. }
            | BlocklistAIHistoryEvent::ClearedActiveConversation { .. }
            | BlocklistAIHistoryEvent::ClearedConversationsInTerminalView { .. }
            | BlocklistAIHistoryEvent::UpdatedTodoList { .. }
            | BlocklistAIHistoryEvent::UpdatedAutoexecuteOverride { .. }
            | BlocklistAIHistoryEvent::SplitConversation { .. }
            | BlocklistAIHistoryEvent::RemoveConversation { .. }
            | BlocklistAIHistoryEvent::DeletedConversation { .. }
            | BlocklistAIHistoryEvent::RestoredConversations { .. }
            | BlocklistAIHistoryEvent::UpdatedConversationMetadata { .. }
            | BlocklistAIHistoryEvent::UpdatedConversationArtifacts { .. } => {}
        }
    }

    pub(super) fn should_autoexecute(
        &self,
        _input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> bool {
        // TODO(QUALITY-342): this should be a setting
        true
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let AIAgentAction {
            action:
                AIAgentActionType::StartAgent {
                    version,
                    name,
                    prompt,
                    execution_mode,
                    lifecycle_subscription,
                },
            ..
        } = input.action
        else {
            return ActionExecution::InvalidAction;
        };

        let prompt = prompt.clone();
        let version = *version;
        let parent_conversation_id = input.conversation_id;
        let (execution_mode, parent_run_id) = match execution_mode.clone() {
            StartAgentExecutionMode::Local { harness_type: None } => {
                // Legacy local Oz child agents do not use
                // StartAgentRequest.parent_run_id. Instead, the child
                // conversation is linked back to its parent on the first
                // request via Request.metadata.parent_agent_id, sourced
                // from the conversation's versioned orchestration_agent_id()
                // (run_id in v2, server conversation token in v1). Remote
                // child agents and local third-party harness children need
                // parent_run_id here because their run is spawned before that
                // first child request exists.
                (StartAgentExecutionMode::Local { harness_type: None }, None)
            }
            StartAgentExecutionMode::Local {
                harness_type: Some(harness_type),
            } => {
                let Some(harness) = Harness::parse_local_child_harness(&harness_type) else {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: invalid_local_child_harness_error(&harness_type),
                            version,
                        },
                    ));
                };

                if !FeatureFlag::OrchestrationV2.is_enabled() {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: "Local harness child agents require orchestration v2."
                                .to_string(),
                            version,
                        },
                    ));
                }

                let parent_run_id = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation(&parent_conversation_id)
                    .and_then(|conversation| conversation.run_id());
                let Some(parent_run_id) = parent_run_id else {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error:
                                "Local harness child agents require the parent run_id to be available."
                                    .to_string(),
                            version,
                        },
                    ));
                };

                (
                    StartAgentExecutionMode::Local {
                        harness_type: Some(harness.to_string()),
                    },
                    Some(parent_run_id),
                )
            }
            StartAgentExecutionMode::Remote {
                environment_id,
                skill_references,
                model_id,
                computer_use_enabled,
                worker_host,
                harness_type,
                title,
            } => {
                if !FeatureFlag::OrchestrationV2.is_enabled() {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: "Remote child agents require orchestration v2.".to_string(),
                            version,
                        },
                    ));
                }

                let harness_type = Harness::parse_orchestration_harness(&harness_type)
                    .map(|harness| harness.to_string())
                    .unwrap_or(harness_type);
                if Harness::parse_orchestration_harness(&harness_type) == Some(Harness::OpenCode) {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: "Remote child agents do not support the opencode harness yet."
                                .to_string(),
                            version,
                        },
                    ));
                }

                // An empty environment_id is allowed and means the child will be spawned with an
                // empty environment (no preconfigured repositories, secrets, or integrations).
                // Callers are discouraged from relying on this, but we intentionally do not reject
                // it here so that agent authors can opt into running without an environment.
                if environment_id.trim().is_empty() {
                    log::warn!(
                        "Starting remote child agent with empty environment_id; the child will run \
                         with an empty environment."
                    );
                }

                let parent_run_id = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation(&parent_conversation_id)
                    .and_then(|conversation| conversation.run_id());
                let Some(parent_run_id) = parent_run_id else {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: "Remote child agents require the parent run_id to be available."
                                .to_string(),
                            version,
                        },
                    ));
                };

                (
                    StartAgentExecutionMode::Remote {
                        environment_id,
                        skill_references,
                        model_id,
                        computer_use_enabled,
                        worker_host,
                        harness_type,
                        title,
                    },
                    Some(parent_run_id),
                )
            }
        };

        // In local mode (no parent_run_id), block until the child finishes
        // so the parent model receives the child's output as the tool result.
        let wait_for_completion = parent_run_id.is_none();

        let (sender, receiver) = async_channel::bounded(1);
        self.pending.push(PendingStartAgent {
            parent_conversation_id,
            child_conversation_id: None,
            sender,
            wait_for_completion,
        });

        ctx.emit(StartAgentExecutorEvent::CreateAgent(StartAgentRequest {
            name: name.clone(),
            prompt,
            execution_mode,
            lifecycle_subscription: lifecycle_subscription.clone(),
            parent_conversation_id,
            parent_run_id,
        }));

        ActionExecution::new_async(async move { receiver.recv().await }, move |result, _ctx| {
            match result {
                Ok(StartAgentDecision::Started { agent_id }) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Success {
                        agent_id,
                        version,
                    })
                }
                Ok(StartAgentDecision::Completed { agent_id, output }) => {
                    // Return the child's output as a "success" with the output
                    // embedded in the agent_id field. The Display impl on
                    // StartAgentResult will show this to the model.
                    AIAgentActionResultType::StartAgent(StartAgentResult::Success {
                        agent_id: format!("{agent_id}\n\nAgent output:\n{output}"),
                        version,
                    })
                }
                Ok(StartAgentDecision::Error(error)) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                }
                Err(_) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Cancelled { version })
                }
            }
        })
    }

    pub(super) fn preprocess_action(
        &mut self,
        _action: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

/// Extracts the text output from a child agent's conversation.
/// Collects text from all exchanges in the conversation.
fn extract_child_output(conversation: &AIConversation) -> String {
    let mut output_parts = Vec::new();
    for exchange in conversation.all_exchanges() {
        if let Some(output) = exchange.output_status.output() {
            let text = output.get().format_for_copy(None);
            if !text.is_empty() {
                output_parts.push(text);
            }
        }
    }
    if output_parts.is_empty() {
        "Agent completed but produced no text output.".to_string()
    } else {
        output_parts.join("\n\n")
    }
}

fn start_agent_error_message_for_status(
    status: &ConversationStatus,
    error_message: Option<&str>,
) -> Option<String> {
    match status {
        ConversationStatus::Error => Some(
            error_message
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("Child agent failed to initialize")
                .to_string(),
        ),
        ConversationStatus::Cancelled => {
            Some("Child agent was cancelled before initialization".to_string())
        }
        ConversationStatus::Blocked { blocked_action } => {
            let blocked_action = blocked_action.trim();
            Some(if blocked_action.is_empty() {
                "Child agent startup was blocked before initialization".to_string()
            } else {
                blocked_action.to_string()
            })
        }
        ConversationStatus::InProgress | ConversationStatus::Success => None,
    }
}

impl Entity for StartAgentExecutor {
    type Event = StartAgentExecutorEvent;
}

pub enum StartAgentExecutorEvent {
    CreateAgent(StartAgentRequest),
}

#[cfg(test)]
#[path = "start_agent_tests.rs"]
mod tests;
