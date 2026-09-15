use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use galaxy_cli::agent::Harness;
use galaxyui::{Entity, ModelContext, ModelHandle, SingletonEntity};
use shell_words::split as split_shell_words;

use super::{
    child_agent_delegation_denial_reason, compose_leaf_agent_prompt, ActionExecution,
    AnyActionExecution, ExecuteActionInput, PreprocessActionInput,
};
use crate::ai::agent::conversation::{AIConversation, AIConversationId, ConversationStatus};
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResultType, AIAgentActionType, LifecycleEventType,
    StartAgentExecutionMode, StartAgentResult,
};
use crate::ai::blocklist::orchestration_event_streamer::OrchestrationEventStreamer;
use crate::ai::blocklist::orchestration_events::OrchestrationEventService;
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::local_harness_setup::local_harness_product_disabled_message;

/// Per-request outcome of a StartAgent dispatch.
#[derive(Debug, Clone)]
pub enum StartAgentOutcome {
    Started {
        agent_id: String,
    },
    /// A direct-provider child completed and returned its output inline.
    Completed {
        agent_id: String,
        output: String,
    },
    /// An error occurred while starting or running the agent.
    Error(String),
}

/// Determines whether a dispatch receiver acknowledges startup or waits for a
/// direct-provider child to reach a terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAgentWaitPolicy {
    Startup,
    /// A local child has been linked; its persistent group owns completion.
    LocalStartup,
    Completion,
}

fn wait_policy_for_execution_mode(mode: &StartAgentExecutionMode) -> StartAgentWaitPolicy {
    match mode {
        StartAgentExecutionMode::Local { .. } => StartAgentWaitPolicy::Completion,
        StartAgentExecutionMode::Remote { .. } => StartAgentWaitPolicy::Startup,
    }
}

pub struct StartAgentDispatch {
    pub request_id: StartAgentRequestId,
    pub receiver: async_channel::Receiver<StartAgentOutcome>,
    pub wait_policy: StartAgentWaitPolicy,
    pub(super) detached: Arc<AtomicBool>,
}

impl StartAgentDispatch {
    pub(super) fn mark_detached(&self) {
        self.detached.store(true, Ordering::Release);
    }
}

fn invalid_local_child_harness_error(harness_type: &str) -> String {
    let harness_name = harness_type.trim();
    if harness_name.is_empty() {
        "Local child harness type is missing.".to_string()
    } else {
        format!("Unsupported local child harness '{harness_name}'.")
    }
}

/// Handles local child launch requests produced by older agents, where the
/// prompt encoded the target CLI command and `execution_mode.harness_type` was
/// still unset. Normalizing here keeps those requests routed through the Codex
/// local harness path instead of launching them as Oz child prompts.
fn parse_legacy_local_child_harness_command(command: &str) -> Option<(String, String)> {
    let args = split_shell_words(command.trim()).ok()?;
    match args.as_slice() {
        [binary, flag, child_prompt]
            if binary == "codex"
                && flag == "--dangerously-bypass-approvals-and-sandbox"
                && !child_prompt.trim().is_empty() =>
        {
            Some(("codex".to_string(), child_prompt.clone()))
        }
        _ => None,
    }
}

fn normalize_legacy_local_child_harness_command(
    prompt: String,
    execution_mode: StartAgentExecutionMode,
) -> (String, StartAgentExecutionMode) {
    match execution_mode {
        StartAgentExecutionMode::Local {
            harness_type: None,
            model_id,
        } => {
            if let Some((harness_type, child_prompt)) =
                parse_legacy_local_child_harness_command(&prompt)
            {
                (
                    child_prompt,
                    StartAgentExecutionMode::Local {
                        harness_type: Some(harness_type),
                        model_id,
                    },
                )
            } else {
                (
                    prompt,
                    StartAgentExecutionMode::Local {
                        harness_type: None,
                        model_id,
                    },
                )
            }
        }
        execution_mode => (prompt, execution_mode),
    }
}

/// Opaque, monotonically increasing request identifier.
/// Disambiguates parallel in-flight StartAgent requests.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Default)]
pub struct StartAgentRequestId(u64);

impl StartAgentRequestId {
    #[cfg(test)]
    pub const fn from_raw_for_test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone)]
pub struct StartAgentRequest {
    pub id: StartAgentRequestId,
    pub name: String,
    pub prompt: String,
    pub execution_mode: StartAgentExecutionMode,
    pub lifecycle_subscription: Option<Vec<LifecycleEventType>>,
    pub parent_conversation_id: AIConversationId,
    pub parent_run_id: Option<String>,
}

struct PendingStartAgent {
    action_id: AIAgentActionId,
    /// Present when RunAgents owns this dispatch. Standalone StartAgent calls
    /// use the action id only for their one-to-one inline child panel.
    run_agents_child_name: Option<String>,
    parent_conversation_id: AIConversationId,
    /// Set once the child conversation is synchronously created.
    child_conversation_id: Option<AIConversationId>,
    sender: async_channel::Sender<StartAgentOutcome>,
    detached: Arc<AtomicBool>,
    /// Direct Bedrock/OpenAI parents do not have a server run id or an
    /// orchestration event stream. Keep the tool call open until their local
    /// child finishes, then return the child's output inline.
    wait_policy: StartAgentWaitPolicy,
}

pub struct StartAgentExecutor {
    pending: HashMap<StartAgentRequestId, PendingStartAgent>,
    next_request_id: u64,
}

impl StartAgentExecutor {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, Self::handle_history_event);

        Self {
            pending: HashMap::new(),
            next_request_id: 0,
        }
    }

    fn next_request_id(&mut self) -> StartAgentRequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        StartAgentRequestId(id)
    }

    /// Links a pending request to its freshly-created child
    /// conversation so subsequent history events can find it.
    fn record_child_conversation(
        &mut self,
        request_id: StartAgentRequestId,
        child_conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let child_link_event = {
            let Some(pending) = self.pending.get(&request_id) else {
                return;
            };
            if pending.detached.load(Ordering::Acquire) {
                self.pending.remove(&request_id);
                return;
            }
            let pending = self
                .pending
                .get_mut(&request_id)
                .expect("pending request was checked above");
            pending.child_conversation_id = Some(child_conversation_id);
            if let Some(agent_name) = pending.run_agents_child_name.clone() {
                Some(StartAgentExecutorEvent::RunAgentsChildConversationCreated {
                    action_id: pending.action_id.clone(),
                    agent_name,
                    parent_conversation_id: pending.parent_conversation_id,
                    child_conversation_id,
                })
            } else if matches!(pending.wait_policy, StartAgentWaitPolicy::Completion) {
                Some(
                    StartAgentExecutorEvent::DirectProviderChildConversationCreated {
                        action_id: pending.action_id.clone(),
                        parent_conversation_id: pending.parent_conversation_id,
                        child_conversation_id,
                    },
                )
            } else {
                None
            }
        };

        if let Some(pending) = self.pending.get(&request_id) {
            if matches!(pending.wait_policy, StartAgentWaitPolicy::LocalStartup) {
                let parent = pending.parent_conversation_id;
                let group_id = pending.action_id.to_string();
                OrchestrationEventService::handle(ctx).update(ctx, |service, ctx| {
                    service.register_local_group_child(
                        parent,
                        &group_id,
                        child_conversation_id,
                        ctx,
                    );
                });
            }
        }
        if let Some(event) = child_link_event {
            ctx.emit(event);
        }
        self.maybe_complete_pending_for_child_state(request_id, child_conversation_id, ctx);
    }

    fn find_pending_by_child(
        &self,
        child_conversation_id: &AIConversationId,
    ) -> Option<StartAgentRequestId> {
        self.pending.iter().find_map(|(id, pending)| {
            (pending.child_conversation_id.as_ref() == Some(child_conversation_id)).then_some(*id)
        })
    }

    fn complete_pending_as_started(
        &mut self,
        request_id: StartAgentRequestId,
        child_conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(pending) = self.pending.remove(&request_id) else {
            return;
        };
        let agent_id = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&child_conversation_id)
            .and_then(|conversation| conversation.orchestration_agent_id());
        match agent_id {
            Some(id) => {
                let _ = pending.sender.try_send(StartAgentOutcome::Started {
                    agent_id: id.clone(),
                });
                OrchestrationEventStreamer::handle(ctx).update(ctx, |streamer, ctx| {
                    streamer.register_watched_run_id(pending.parent_conversation_id, id, ctx);
                });
            }
            None => {
                log::error!(
                    "No agent identifier found for child conversation {child_conversation_id:?}"
                );
                let _ = pending.sender.try_send(StartAgentOutcome::Error(
                    "Server did not assign an agent identifier".to_string(),
                ));
            }
        }
    }

    fn complete_pending_as_completed(
        &mut self,
        request_id: StartAgentRequestId,
        child_conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&child_conversation_id)
        else {
            return;
        };
        let agent_id = conversation
            .orchestration_agent_id()
            .or_else(|| {
                conversation
                    .server_conversation_token()
                    .map(|token| token.as_str().to_string())
            })
            .unwrap_or_else(|| child_conversation_id.to_string());
        let output = extract_child_output(conversation);

        let Some(pending) = self.pending.remove(&request_id) else {
            return;
        };
        let _ = pending
            .sender
            .try_send(StartAgentOutcome::Completed { agent_id, output });
    }

    fn complete_pending_as_error(
        &mut self,
        request_id: StartAgentRequestId,
        child_conversation_id: AIConversationId,
        error_msg: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(pending) = self.pending.remove(&request_id) else {
            return;
        };
        let error_msg = if matches!(pending.wait_policy, StartAgentWaitPolicy::Completion) {
            format!("Child agent {child_conversation_id} failed: {error_msg}. For a recoverable error (never a user cancellation), use send_message_to_agent with this agent_id and a corrective instruction to continue the same child; do not create a duplicate.")
        } else {
            error_msg
        };
        let _ = pending.sender.try_send(StartAgentOutcome::Error(error_msg));
        // Only startup acknowledgements may clean up a conversation that never
        // initialized. Direct-provider completion waits preserve the terminal
        // child so its transcript and failure remain inspectable.
        let should_cleanup = matches!(pending.wait_policy, StartAgentWaitPolicy::Startup)
            && BlocklistAIHistoryModel::as_ref(ctx)
                .conversation(&child_conversation_id)
                .is_some_and(|conversation| {
                    should_cleanup_failed_child_launch(conversation.status())
                });
        if should_cleanup {
            ctx.emit(StartAgentExecutorEvent::CleanupFailedChildLaunch {
                conversation_id: child_conversation_id,
            });
        }
    }

    fn maybe_complete_pending_for_child_state(
        &mut self,
        request_id: StartAgentRequestId,
        child_conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&child_conversation_id)
        else {
            return;
        };
        let wait_policy = self
            .pending
            .get(&request_id)
            .map(|pending| pending.wait_policy);
        match wait_policy {
            Some(StartAgentWaitPolicy::Completion) => match conversation.status() {
                ConversationStatus::Success => {
                    self.complete_pending_as_completed(request_id, child_conversation_id, ctx);
                }
                ConversationStatus::Error | ConversationStatus::Cancelled => {
                    let error_msg = direct_child_error_message_for_status(
                        conversation.status(),
                        conversation.status_error_message().as_deref(),
                    )
                    .expect("terminal direct child status should produce an error");
                    self.complete_pending_as_error(
                        request_id,
                        child_conversation_id,
                        error_msg,
                        ctx,
                    );
                }
                ConversationStatus::InProgress
                | ConversationStatus::TransientError
                | ConversationStatus::Blocked { .. }
                | ConversationStatus::WaitingForEvents => {}
            },
            Some(StartAgentWaitPolicy::LocalStartup) => {
                // Membership is durable before launch acknowledgement reaches the parent.
                if let Some(pending) = self.pending.remove(&request_id) {
                    let _ = pending.sender.try_send(StartAgentOutcome::Started {
                        agent_id: child_conversation_id.to_string(),
                    });
                }
            }
            Some(StartAgentWaitPolicy::Startup) => {
                if let Some(error_msg) = start_agent_startup_error_message_for_status(
                    conversation.status(),
                    conversation.status_error_message().as_deref(),
                ) {
                    self.complete_pending_as_error(
                        request_id,
                        child_conversation_id,
                        error_msg,
                        ctx,
                    );
                } else if conversation.orchestration_agent_id().is_some() {
                    self.complete_pending_as_started(request_id, child_conversation_id, ctx);
                }
            }
            None => {}
        }
    }

    fn handle_history_event(
        &mut self,
        _: ModelHandle<BlocklistAIHistoryModel>,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            BlocklistAIHistoryEvent::ConversationServerTokenAssigned {
                conversation_id, ..
            } => {
                let Some(request_id) = self.find_pending_by_child(conversation_id) else {
                    return;
                };
                self.maybe_complete_pending_for_child_state(request_id, *conversation_id, ctx);
            }
            BlocklistAIHistoryEvent::UpdatedConversationStatus {
                conversation_id, ..
            } => {
                let Some(request_id) = self.find_pending_by_child(conversation_id) else {
                    return;
                };
                self.maybe_complete_pending_for_child_state(request_id, *conversation_id, ctx);
            }
            BlocklistAIHistoryEvent::NewConversationRequestComplete {
                request_id,
                conversation_id,
            } => {
                self.record_child_conversation(*request_id, *conversation_id, ctx);
            }
            BlocklistAIHistoryEvent::RemoveConversation {
                conversation_id, ..
            }
            | BlocklistAIHistoryEvent::DeletedConversation {
                conversation_id, ..
            } => {
                let Some(request_id) = self.find_pending_by_child(conversation_id) else {
                    return;
                };
                let Some(pending) = self.pending.remove(&request_id) else {
                    return;
                };
                let _ = pending.sender.try_send(StartAgentOutcome::Error(
                    "Child agent conversation was removed by the user.".to_string(),
                ));
            }
            BlocklistAIHistoryEvent::StartedNewConversation { .. }
            | BlocklistAIHistoryEvent::CreatedSubtask { .. }
            | BlocklistAIHistoryEvent::UpgradedTask { .. }
            | BlocklistAIHistoryEvent::AppendedExchange { .. }
            | BlocklistAIHistoryEvent::ReassignedExchange { .. }
            | BlocklistAIHistoryEvent::UpdatedStreamingExchange { .. }
            | BlocklistAIHistoryEvent::SetActiveConversation { .. }
            | BlocklistAIHistoryEvent::ClearedActiveConversation { .. }
            | BlocklistAIHistoryEvent::ClearedConversationsForTerminalSurface { .. }
            | BlocklistAIHistoryEvent::UpdatedTodoList { .. }
            | BlocklistAIHistoryEvent::UpdatedAutoexecuteOverride { .. }
            | BlocklistAIHistoryEvent::SplitConversation { .. }
            | BlocklistAIHistoryEvent::RestoredConversations { .. }
            | BlocklistAIHistoryEvent::UpdatedConversationTitle { .. }
            | BlocklistAIHistoryEvent::UpdatedConversationMetadata { .. }
            | BlocklistAIHistoryEvent::UpdatedConversationArtifacts { .. }
            | BlocklistAIHistoryEvent::ConversationTransferredBetweenTerminalSurfaces { .. } => {}
            BlocklistAIHistoryEvent::OrchestrationConfigUpdated { .. }
            | BlocklistAIHistoryEvent::ConversationUsageMetadataUpdated { .. }
            | BlocklistAIHistoryEvent::LocalSharedSessionEstablished { .. } => {}
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

        let version = *version;
        let parent_conversation_id = input.conversation_id;
        if let Some(error) = child_agent_delegation_denial_reason(parent_conversation_id, ctx) {
            return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                StartAgentResult::Error { error, version },
            ));
        }

        let prompt = prompt.clone();
        let action_id = input.action.id.clone();
        let (prompt, execution_mode) =
            normalize_legacy_local_child_harness_command(prompt, execution_mode.clone());
        let prompt = compose_leaf_agent_prompt(&prompt);
        let (execution_mode, parent_run_id) = match execution_mode {
            StartAgentExecutionMode::Local {
                harness_type: None,
                model_id,
            } => {
                // Server-backed parents launch an Oz child and return its run
                // id immediately. Direct Bedrock/OpenAI parents have no run
                // id; terminal_pane creates a local hidden child instead and
                // this executor waits for its final output.
                let parent_run_id = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation(&parent_conversation_id)
                    .and_then(|conversation| conversation.run_id());
                (
                    StartAgentExecutionMode::Local {
                        harness_type: None,
                        model_id,
                    },
                    parent_run_id,
                )
            }
            StartAgentExecutionMode::Local {
                harness_type: Some(harness_type),
                model_id,
            } => {
                let Some(harness) = Harness::parse_local_child_harness(&harness_type) else {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: invalid_local_child_harness_error(&harness_type),
                            version,
                        },
                    ));
                };
                if let Some(message) = local_harness_product_disabled_message(harness) {
                    return ActionExecution::Sync(AIAgentActionResultType::StartAgent(
                        StartAgentResult::Error {
                            error: message.to_string(),
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
                        model_id,
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
                auth_secret_name,
            } => {
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
                        auth_secret_name,
                    },
                    Some(parent_run_id),
                )
            }
        };

        // Local children return their completed work; remote children acknowledge startup and
        // continue through the hosted orchestration lifecycle.
        let wait_policy = wait_policy_for_execution_mode(&execution_mode);

        let (sender, receiver) = async_channel::bounded(1);
        let request_id = self.next_request_id();
        let detached = Arc::new(AtomicBool::new(false));
        self.pending.insert(
            request_id,
            PendingStartAgent {
                action_id,
                run_agents_child_name: None,
                parent_conversation_id,
                child_conversation_id: None,
                sender,
                detached,
                wait_policy,
            },
        );

        ctx.emit(StartAgentExecutorEvent::CreateAgent(Box::new(
            StartAgentRequest {
                id: request_id,
                name: name.clone(),
                prompt,
                execution_mode,
                lifecycle_subscription: lifecycle_subscription.clone(),
                parent_conversation_id,
                parent_run_id,
            },
        )));

        ActionExecution::new_async(async move { receiver.recv().await }, move |result, _ctx| {
            match result {
                Ok(StartAgentOutcome::Started { agent_id }) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Success {
                        agent_id,
                        version,
                    })
                }
                Ok(StartAgentOutcome::Completed { agent_id, output }) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Success {
                        agent_id: format!("{agent_id}\n\nAgent output:\n{output}"),
                        version,
                    })
                }
                Ok(StartAgentOutcome::Error(error)) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                }
                Err(_) => {
                    AIAgentActionResultType::StartAgent(StartAgentResult::Cancelled { version })
                }
            }
        })
    }

    pub(super) fn yield_after_local_start(&mut self, dispatch: &mut StartAgentDispatch) {
        if matches!(dispatch.wait_policy, StartAgentWaitPolicy::Completion) {
            dispatch.wait_policy = StartAgentWaitPolicy::LocalStartup;
            if let Some(pending) = self.pending.get_mut(&dispatch.request_id) {
                pending.wait_policy = StartAgentWaitPolicy::LocalStartup;
            }
        }
    }

    /// Dispatch a pre-validated StartAgent request. Returns a receiver
    /// for the resulting [`StartAgentOutcome`].
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &mut self,
        action_id: AIAgentActionId,
        name: String,
        prompt: String,
        execution_mode: StartAgentExecutionMode,
        lifecycle_subscription: Option<Vec<LifecycleEventType>>,
        parent_conversation_id: AIConversationId,
        parent_run_id: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) -> StartAgentDispatch {
        let wait_policy = wait_policy_for_execution_mode(&execution_mode);
        let (sender, receiver) = async_channel::bounded(1);
        let request_id = self.next_request_id();
        let detached = Arc::new(AtomicBool::new(false));
        if let Some(error) = child_agent_delegation_denial_reason(parent_conversation_id, ctx) {
            let _ = sender.try_send(StartAgentOutcome::Error(error));
            return StartAgentDispatch {
                request_id,
                receiver,
                wait_policy,
                detached,
            };
        }

        let (prompt, execution_mode) =
            normalize_legacy_local_child_harness_command(prompt, execution_mode);
        if parent_run_id.is_none() {
            if let StartAgentExecutionMode::Local {
                harness_type: None,
                model_id: Some(model_id),
            } = &execution_mode
            {
                if ctx
                    .try_get_singleton_model_as_ref::<LLMPreferences>()
                    .is_some_and(|preferences| {
                        preferences
                            .get_llm_info(&LLMId::from(model_id.as_str()))
                            .is_none()
                    })
                {
                    let _ = sender.try_send(StartAgentOutcome::Error(format!(
                        "Model '{model_id}' is no longer available. Refresh the provider models and select an available model before launching this agent."
                    )));
                    return StartAgentDispatch {
                        request_id,
                        receiver,
                        wait_policy,
                        detached,
                    };
                }
            }
        }
        let prompt = compose_leaf_agent_prompt(&prompt);
        self.pending.insert(
            request_id,
            PendingStartAgent {
                action_id,
                run_agents_child_name: Some(name.clone()),
                parent_conversation_id,
                child_conversation_id: None,
                sender,
                detached: detached.clone(),
                wait_policy,
            },
        );
        ctx.emit(StartAgentExecutorEvent::CreateAgent(Box::new(
            StartAgentRequest {
                id: request_id,
                name,
                prompt,
                execution_mode,
                lifecycle_subscription,
                parent_conversation_id,
                parent_run_id,
            },
        )));
        StartAgentDispatch {
            request_id,
            receiver,
            wait_policy,
            detached,
        }
    }

    pub fn reattach(
        &mut self,
        action_id: AIAgentActionId,
        name: String,
        parent_conversation_id: AIConversationId,
        child_conversation_id: AIConversationId,
        wait_policy: StartAgentWaitPolicy,
        ctx: &mut ModelContext<Self>,
    ) -> StartAgentDispatch {
        let (sender, receiver) = async_channel::bounded(1);
        let request_id = self.next_request_id();
        let detached = Arc::new(AtomicBool::new(false));
        self.pending.insert(
            request_id,
            PendingStartAgent {
                action_id,
                run_agents_child_name: Some(name),
                parent_conversation_id,
                child_conversation_id: Some(child_conversation_id),
                sender,
                detached: detached.clone(),
                wait_policy,
            },
        );
        self.record_child_conversation(request_id, child_conversation_id, ctx);
        StartAgentDispatch {
            request_id,
            receiver,
            wait_policy,
            detached,
        }
    }

    /// Detaches one exact dispatch. If its launch callback is already queued,
    /// the shared marker prevents that callback from linking a late child.
    pub fn detach_dispatch(&mut self, request_id: StartAgentRequestId) -> bool {
        let Some(pending) = self.pending.remove(&request_id) else {
            return false;
        };
        pending.detached.store(true, Ordering::Release);
        true
    }

    /// Test-only lookup for request ownership without exposing executor internals.
    #[cfg(test)]
    pub fn has_pending_dispatch_for_test(&self, request_id: StartAgentRequestId) -> bool {
        self.pending.contains_key(&request_id)
    }

    pub fn cancel_dispatches_for_action(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> usize {
        let request_ids = self
            .pending
            .iter()
            .filter_map(|(request_id, pending)| {
                (pending.parent_conversation_id == conversation_id
                    && &pending.action_id == action_id)
                    .then_some(*request_id)
            })
            .collect::<Vec<_>>();
        let detached_count = request_ids.len();
        for request_id in request_ids {
            self.detach_dispatch(request_id);
        }
        detached_count
    }

    /// Cancels only the caller's pending tool wait. A child that was already created keeps
    /// running independently and remains available in conversation history.
    pub(super) fn cancel_execution(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) {
        self.cancel_dispatches_for_action(conversation_id, action_id);
    }

    pub(super) fn preprocess_action(
        &mut self,
        _action: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

/// Extracts the text output from every completed exchange in a local child.
fn extract_child_output(conversation: &AIConversation) -> String {
    let output_parts = conversation
        .all_exchanges()
        .into_iter()
        .filter_map(|exchange| exchange.output_status.output())
        .map(|output| output.get().format_for_copy(None))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if output_parts.is_empty() {
        "Agent completed but produced no text output.".to_string()
    } else {
        output_parts.join("\n\n")
    }
}

/// Whether a child that failed before launch should have its hidden pane and
/// conversation cleaned up. Only terminal launch failures qualify; recoverable
/// `Blocked` startup states (e.g. awaiting GitHub auth) and non-terminal
/// `TransientError` (a recovery is in flight) keep their chip so the user can
/// resolve them or let the retry complete.
fn should_cleanup_failed_child_launch(status: &ConversationStatus) -> bool {
    match status {
        ConversationStatus::Error | ConversationStatus::Cancelled => true,
        ConversationStatus::Blocked { .. }
        | ConversationStatus::InProgress
        | ConversationStatus::TransientError
        | ConversationStatus::Success
        | ConversationStatus::WaitingForEvents => false,
    }
}

fn start_agent_startup_error_message_for_status(
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
        // `WaitingForEvents` is treated like `InProgress`/`Success` here:
        // a child that's actively waiting for events has, by definition,
        // already initialized successfully and is not an error case.
        // TransientError is likewise non-terminal: a recovery is in flight,
        // so keep waiting. The agent run is still in flight in all of these
        // cases, so we don't surface an error message for the start path.
        ConversationStatus::InProgress
        | ConversationStatus::TransientError
        | ConversationStatus::Success
        | ConversationStatus::WaitingForEvents => None,
    }
}

fn direct_child_error_message_for_status(
    status: &ConversationStatus,
    error_message: Option<&str>,
) -> Option<String> {
    match status {
        ConversationStatus::Error => Some(
            error_message
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("Child agent failed")
                .to_string(),
        ),
        ConversationStatus::Cancelled => Some("Child agent was cancelled by the user.".to_string()),
        ConversationStatus::InProgress
        | ConversationStatus::TransientError
        | ConversationStatus::Success
        | ConversationStatus::Blocked { .. }
        | ConversationStatus::WaitingForEvents => None,
    }
}

impl Entity for StartAgentExecutor {
    type Event = StartAgentExecutorEvent;
}

pub enum StartAgentExecutorEvent {
    CreateAgent(Box<StartAgentRequest>),
    /// A direct-provider child conversation is available while its StartAgent
    /// tool call remains open waiting for completion. This lets the parent
    /// action render the live child panel before the tool result exists.
    DirectProviderChildConversationCreated {
        action_id: AIAgentActionId,
        parent_conversation_id: AIConversationId,
        child_conversation_id: AIConversationId,
    },
    /// A RunAgents child conversation is available for live status and
    /// navigation in the owning action card.
    RunAgentsChildConversationCreated {
        action_id: AIAgentActionId,
        agent_name: String,
        parent_conversation_id: AIConversationId,
        child_conversation_id: AIConversationId,
    },
    /// A child agent failed at the launch stage (never started a server-side
    /// run). The owning terminal view removes its hidden pane and conversation
    /// so the orchestration pill bar does not retain a dead chip.
    CleanupFailedChildLaunch {
        conversation_id: AIConversationId,
    },
}

#[cfg(test)]
#[path = "start_agent_tests.rs"]
mod tests;
