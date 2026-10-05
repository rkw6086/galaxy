//! Async executor for `AIAgentActionType::RunAgents`.
//!
//! Fans out per-child via [`super::start_agent::StartAgentExecutor::dispatch`]
//! and aggregates the outcomes into a single `RunAgentsResult`.
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ai::agent::action::{RunAgentsAgentRunConfig, RunAgentsExecutionMode, RunAgentsRequest};
use ai::agent::action_result::{
    RunAgentsAgentOutcome, RunAgentsAgentOutcomeKind, RunAgentsLaunchedExecutionMode,
    RunAgentsResult,
};
use ai::agent::orchestration_config::OrchestrationConfig;
use ai::skills::SkillReference;
use futures::future::{join_all, BoxFuture};
use futures::FutureExt;
use galaxy_core::execution_mode::AppExecutionMode;
use settings::Setting;
use warp_cli::agent::Harness;
use warpui::{Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};

use super::start_agent::{
    StartAgentDispatch, StartAgentExecutor, StartAgentExecutorEvent, StartAgentOutcome,
    StartAgentRequestId, StartAgentWaitPolicy,
};
use super::{
    child_agent_delegation_denial_reason, ActionExecution, AnyActionExecution, ExecuteActionInput,
    PreprocessActionInput,
};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResultType, AIAgentActionType, AIAgentInput,
    StartAgentExecutionMode,
};
use crate::ai::auth_secret_types::auth_secret_types_for_harness;
use crate::ai::blocklist::inline_action::orchestration_controls::OrchestrationEditState;
use crate::ai::blocklist::orchestration_events::OrchestrationEventService;
use crate::ai::blocklist::{BlocklistAIHistoryModel, BlocklistAIPermissions};
use crate::ai::cloud_agent_settings::CloudAgentSettings;
use crate::ai::document::plan_publication::{
    prepare_plan_publications, wait_for_plan_publications,
};
use crate::ai::local_harness_setup::local_harness_product_disabled_message;
#[cfg(not(target_family = "wasm"))]
use crate::ai::remote_logging::{self, RemoteLogLevel, RemoteLogRecord};

/// Per-child spawn timeout. If a child agent doesn't report back within
/// this window (e.g. binary not found, server error), the slot is failed
/// rather than hanging the "Spawning agents" UI indefinitely.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Snapshot of an in-flight dispatch, carried through
/// [`RunAgentsExecutorEvent::SpawningStarted`].
#[derive(Debug, Clone, Copy)]
pub struct RunAgentsSpawningSnapshot {
    pub agent_count: usize,
}

/// In-flight tracking per `RunAgents` action (idempotency guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRunAgents {
    Publishing,
    Spawning,
}
#[derive(Debug, Clone)]
struct ExistingLaunchedAgent {
    name: String,
    agent_id: String,
}

pub struct RunAgentsExecutor {
    pending: HashMap<(AIConversationId, AIAgentActionId), PendingRunAgents>,
    recovery_action_ids: HashSet<(AIConversationId, AIAgentActionId)>,
    launched_agents: HashMap<AIConversationId, HashMap<String, ExistingLaunchedAgent>>,
    start_agent_executor: ModelHandle<StartAgentExecutor>,
    terminal_view_id: EntityId,
}

/// Lifecycle events for in-flight dispatches.
pub enum RunAgentsExecutorEvent {
    SpawningStarted {
        conversation_id: AIConversationId,
        action_id: AIAgentActionId,
        snapshot: RunAgentsSpawningSnapshot,
    },
    SpawningFinished {
        conversation_id: AIConversationId,
        action_id: AIAgentActionId,
    },
    ChildConversationCreated {
        action_id: AIAgentActionId,
        agent_name: String,
        parent_conversation_id: AIConversationId,
        child_conversation_id: AIConversationId,
    },
}

impl Entity for RunAgentsExecutor {
    type Event = RunAgentsExecutorEvent;
}

impl RunAgentsExecutor {
    fn validate_profile_providers(&self, request: &RunAgentsRequest, ctx: &ModelContext<Self>) -> Result<(), String> {
        let profile = crate::ai::execution_profiles::profiles::AIExecutionProfilesModel::as_ref(ctx).active_profile(Some(self.terminal_view_id), ctx);
        if profile.data().orchestration_providers.is_none() { return Ok(()); }
        if !request.harness_type.trim().is_empty() && !matches!(Harness::parse_orchestration_harness(&request.harness_type), Some(Harness::Oz)) {
            return Err("This profile restricts orchestration to selected Galaxy model providers. External harnesses cannot bypass that selection.".to_string());
        }
        let prefs = crate::ai::llms::LLMPreferences::as_ref(ctx);
        for child in &request.agent_run_configs {
            let id = if !child.model_id.trim().is_empty() { child.model_id.as_str() } else if !request.model_id.trim().is_empty() { request.model_id.as_str() } else { prefs.get_active_base_model(ctx, Some(self.terminal_view_id)).id.as_str() };
            let model = prefs.get_llm_info(&crate::ai::llms::LLMId::from(id)).ok_or_else(|| format!("Unknown orchestration model: {id}"))?;
            if model.disable_reason.is_some() || !prefs.profile_allows_orchestration_model(model, Some(self.terminal_view_id), ctx) {
                return Err(format!("Model {id} is excluded by this profile's orchestration provider selection. Choose a model from the advertised provider list."));
            }
        }
        Ok(())
    }

    pub fn new(
        start_agent_executor: ModelHandle<StartAgentExecutor>,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&start_agent_executor, |_, _, event, ctx| {
            if let StartAgentExecutorEvent::RunAgentsChildConversationCreated {
                action_id,
                agent_name,
                parent_conversation_id,
                child_conversation_id,
            } = event
            {
                ctx.emit(RunAgentsExecutorEvent::ChildConversationCreated {
                    action_id: action_id.clone(),
                    agent_name: agent_name.clone(),
                    parent_conversation_id: *parent_conversation_id,
                    child_conversation_id: *child_conversation_id,
                });
            }
        });
        Self {
            pending: HashMap::new(),
            recovery_action_ids: HashSet::new(),
            launched_agents: HashMap::new(),
            start_agent_executor,
            terminal_view_id,
        }
    }

    pub fn is_pending(
        &self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> bool {
        self.pending
            .contains_key(&(conversation_id, action_id.clone()))
    }

    pub fn mark_recovery_actions(
        &mut self,
        conversation_id: AIConversationId,
        action_ids: &HashSet<AIAgentActionId>,
    ) {
        self.recovery_action_ids.extend(
            action_ids
                .iter()
                .cloned()
                .map(|action_id| (conversation_id, action_id)),
        );
    }

    pub(crate) fn terminal_view_id(&self) -> EntityId {
        self.terminal_view_id
    }

    /// Removes all per-conversation state for a conversation that has been removed.
    pub(super) fn cleanup_conversation(&mut self, conversation_id: AIConversationId) {
        self.launched_agents.remove(&conversation_id);
        self.pending
            .retain(|(conv_id, _), _| *conv_id != conversation_id);
        self.recovery_action_ids
            .retain(|(conv_id, _)| *conv_id != conversation_id);
    }

    /// Cancels the parent tool wait without cancelling independently-running children.
    pub(super) fn cancel_execution(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) {
        let action_key = (conversation_id, action_id.clone());
        self.recovery_action_ids.remove(&action_key);
        let detached_dispatches = self.start_agent_executor.update(ctx, |executor, _| {
            executor.cancel_dispatches_for_action(conversation_id, action_id)
        });
        crate::ai::tool_diagnostics::tool_debug!(
            "RunAgents cancellation detached {detached_dispatches} pending child dispatch(es) for action {action_id}"
        );
        if self.pending.remove(&action_key).is_some() {
            OrchestrationEventService::handle(ctx).update(ctx, |service, ctx| {
                service.seal_local_group(conversation_id, &action_id.to_string(), Vec::new(), ctx);
            });
            ctx.emit(RunAgentsExecutorEvent::SpawningFinished {
                conversation_id,
                action_id: action_id.clone(),
            });
        }
    }

    fn record_launched_agents(
        &mut self,
        conversation_id: AIConversationId,
        agents: &[RunAgentsAgentOutcome],
    ) {
        for agent in agents {
            let RunAgentsAgentOutcomeKind::Launched { agent_id } = &agent.kind else {
                let RunAgentsAgentOutcomeKind::Completed { agent_id, .. } = &agent.kind else {
                    continue;
                };
                let Some(normalized_name) = normalize_agent_name(&agent.name) else {
                    continue;
                };
                self.launched_agents
                    .entry(conversation_id)
                    .or_default()
                    .insert(
                        normalized_name,
                        ExistingLaunchedAgent {
                            name: agent.name.clone(),
                            agent_id: agent_id.clone(),
                        },
                    );
                continue;
            };
            let Some(normalized_name) = normalize_agent_name(&agent.name) else {
                continue;
            };
            self.launched_agents
                .entry(conversation_id)
                .or_default()
                .insert(
                    normalized_name,
                    ExistingLaunchedAgent {
                        name: agent.name.clone(),
                        agent_id: agent_id.clone(),
                    },
                );
        }
    }

    fn duplicate_launched_agents_reason(
        &self,
        request: &RunAgentsRequest,
        parent_conversation_id: AIConversationId,
        ctx: &ModelContext<Self>,
    ) -> Option<String> {
        duplicate_launched_agents_reason(
            request,
            parent_conversation_id,
            &self.launched_agents,
            ctx,
        )
    }

    /// Publishes parent plans and dispatches children after a bounded best-effort wait.
    fn dispatch_prepared_run_agents(
        &mut self,
        action_id: AIAgentActionId,
        request: RunAgentsRequest,
        parent_conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) -> async_channel::Receiver<RunAgentsResult> {
        let (sender, receiver) = async_channel::bounded(1);

        let action_key = (parent_conversation_id, action_id.clone());
        if self.pending.contains_key(&action_key) {
            log::warn!("RunAgentsExecutor: dispatch reentered for {action_id:?}; rejecting");
            #[cfg(not(target_family = "wasm"))]
            log_run_agents_event(
                ctx,
                RemoteLogLevel::Warn,
                "RunAgents dispatch rejected",
                serde_json::json!({
                    "event": "run_agents_dispatch_rejected",
                    "reason": "reentered_pending_action",
                    "action_id": action_id.to_string(),
                    "parent_conversation_id": parent_conversation_id.to_string(),
                }),
            );
            let _ = sender.try_send(RunAgentsResult::Cancelled);
            return receiver;
        }

        if let Err(error) = validate_request(&request).and_then(|()| self.validate_profile_providers(&request, ctx)) {
            log::warn!("RunAgentsExecutor: validation failure: {error}");
            #[cfg(not(target_family = "wasm"))]
            log_run_agents_event(
                ctx,
                RemoteLogLevel::Warn,
                "RunAgents validation failed",
                serde_json::json!({
                    "event": "run_agents_validation_failed",
                    "action_id": action_id.to_string(),
                    "parent_conversation_id": parent_conversation_id.to_string(),
                    "error": remote_logging::sanitize_error(&error),
                }),
            );
            let _ = sender.try_send(RunAgentsResult::Failure { error });
            return receiver;
        }
        let pending_plan_publications = prepare_plan_publications(parent_conversation_id, ctx);

        let snapshot = RunAgentsSpawningSnapshot {
            agent_count: request.agent_run_configs.len(),
        };
        self.pending
            .insert(action_key, PendingRunAgents::Publishing);
        #[cfg(not(target_family = "wasm"))]
        log_run_agents_event(
            ctx,
            RemoteLogLevel::Info,
            "RunAgents plan publication wait started",
            serde_json::json!({
                "event": "run_agents_plan_publication_wait_started",
                "action_id": action_id.to_string(),
                "parent_conversation_id": parent_conversation_id.to_string(),
                "agent_count": snapshot.agent_count,
                "plan_id_present": !request.plan_id.trim().is_empty(),
            }),
        );
        ctx.emit(RunAgentsExecutorEvent::SpawningStarted {
            conversation_id: parent_conversation_id,
            action_id: action_id.clone(),
            snapshot,
        });

        let action_id_for_wait = action_id.clone();
        ctx.spawn(
            async move {
                // Wait briefly for each plan to become server-backed without blocking
                // launch on a failed or slow publication. Resolves immediately when
                // there is nothing to wait on.
                wait_for_plan_publications(pending_plan_publications).await;
                request
            },
            move |me, request, ctx| {
                if !me.is_pending(parent_conversation_id, &action_id_for_wait) {
                    return;
                }
                me.dispatch_children_for_prepared_request(
                    action_id_for_wait.clone(),
                    request,
                    parent_conversation_id,
                    HashMap::new(),
                    sender,
                    ctx,
                )
            },
        );

        receiver
    }

    fn dispatch_recovered_run_agents(
        &mut self,
        action_id: AIAgentActionId,
        request: RunAgentsRequest,
        parent_conversation_id: AIConversationId,
        recovery_children: HashMap<String, AIConversationId>,
        ctx: &mut ModelContext<Self>,
    ) -> async_channel::Receiver<RunAgentsResult> {
        let (sender, receiver) = async_channel::bounded(1);
        if self.is_pending(parent_conversation_id, &action_id) {
            let _ = sender.try_send(RunAgentsResult::Cancelled);
            return receiver;
        }
        if let Err(error) = validate_request(&request).and_then(|()| self.validate_profile_providers(&request, ctx)) {
            let _ = sender.try_send(RunAgentsResult::Failure { error });
            return receiver;
        }

        let snapshot = RunAgentsSpawningSnapshot {
            agent_count: request.agent_run_configs.len(),
        };
        ctx.emit(RunAgentsExecutorEvent::SpawningStarted {
            conversation_id: parent_conversation_id,
            action_id: action_id.clone(),
            snapshot,
        });
        self.dispatch_children_for_prepared_request(
            action_id,
            request,
            parent_conversation_id,
            recovery_children,
            sender,
            ctx,
        );
        receiver
    }

    fn dispatch_children_for_prepared_request(
        &mut self,
        action_id: AIAgentActionId,
        request: RunAgentsRequest,
        parent_conversation_id: AIConversationId,
        mut recovery_children: HashMap<String, AIConversationId>,
        sender: async_channel::Sender<RunAgentsResult>,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Err(error) = self.validate_profile_providers(&request, ctx) {
            self.pending.remove(&(parent_conversation_id, action_id.clone()));
            let _ = sender.try_send(RunAgentsResult::Failure { error });
            return;
        }
        self.pending.insert(
            (parent_conversation_id, action_id.clone()),
            PendingRunAgents::Spawning,
        );
        let parent_run_id = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&parent_conversation_id)
            .and_then(|c| c.run_id());

        let RunAgentsRequest {
            execution_mode: run_execution_mode,
            harness_type,
            model_id,
            skills,
            agent_run_configs,
            base_prompt,
            harness_auth_secret_name,
            ..
        } = request;

        #[cfg(not(target_family = "wasm"))]
        log_run_agents_event(
            ctx,
            RemoteLogLevel::Info,
            "RunAgents child dispatch started",
            serde_json::json!({
                "event": "run_agents_child_dispatch_started",
                "action_id": action_id.to_string(),
                "parent_conversation_id": parent_conversation_id.to_string(),
                "agent_count": agent_run_configs.len(),
                "execution_mode": run_agents_execution_mode_label(&run_execution_mode),
                "harness_type": harness_type.as_str(),
                "model_id_present": !model_id.trim().is_empty(),
                "parent_run_id_present": parent_run_id.is_some(),
            }),
        );

        if matches!(run_execution_mode, RunAgentsExecutionMode::Local) {
            OrchestrationEventService::handle(ctx).update(ctx, |service, ctx| {
                service.begin_local_group(parent_conversation_id, &action_id.to_string(), ctx);
            });
        }
        let mut slots: Vec<ChildSlot> = Vec::with_capacity(agent_run_configs.len());
        let wait_policy = match &run_execution_mode {
            RunAgentsExecutionMode::Local => StartAgentWaitPolicy::LocalStartup,
            RunAgentsExecutionMode::Remote { .. } => StartAgentWaitPolicy::Startup,
        };
        for cfg in &agent_run_configs {
            let normalized_name = normalize_agent_name(&cfg.name)
                .expect("validated RunAgents requests have non-empty agent names");
            if let Some(child_conversation_id) = recovery_children.remove(&normalized_name) {
                if matches!(run_execution_mode, RunAgentsExecutionMode::Local) {
                    OrchestrationEventService::handle(ctx).update(ctx, |service, ctx| {
                        service.register_local_group_child(
                            parent_conversation_id,
                            &action_id.to_string(),
                            child_conversation_id,
                            ctx,
                        );
                    });
                }
                let dispatch = self.start_agent_executor.update(ctx, |executor, exec_ctx| {
                    executor.reattach(
                        action_id.clone(),
                        cfg.name.clone(),
                        parent_conversation_id,
                        child_conversation_id,
                        wait_policy,
                        exec_ctx,
                    )
                });
                slots.push(ChildSlot::Pending(dispatch));
                continue;
            }

            let prompt = compose_run_agents_child_prompt(&base_prompt, &cfg.prompt);
            let mode = match run_agents_to_start_agent_mode(
                &run_execution_mode,
                &harness_type,
                &model_id,
                &skills,
                harness_auth_secret_name.as_deref(),
                cfg,
            ) {
                Ok(mode) => mode,
                Err(err) => {
                    #[cfg(not(target_family = "wasm"))]
                    log_run_agents_event(
                        ctx,
                        RemoteLogLevel::Warn,
                        "RunAgents child dispatch failed before launch",
                        serde_json::json!({
                            "event": "run_agents_child_dispatch_prelaunch_failed",
                            "action_id": action_id.to_string(),
                            "parent_conversation_id": parent_conversation_id.to_string(),
                            "agent_name": cfg.name.as_str(),
                            "error": remote_logging::sanitize_error(&err),
                        }),
                    );
                    slots.push(ChildSlot::Failed(err));
                    continue;
                }
            };
            if matches!(run_execution_mode, RunAgentsExecutionMode::Remote { .. })
                && parent_run_id.is_none()
            {
                #[cfg(not(target_family = "wasm"))]
                log_run_agents_event(
                    ctx,
                    RemoteLogLevel::Warn,
                    "RunAgents remote child dispatch missing parent run_id",
                    serde_json::json!({
                        "event": "run_agents_child_dispatch_prelaunch_failed",
                        "action_id": action_id.to_string(),
                        "parent_conversation_id": parent_conversation_id.to_string(),
                        "agent_name": cfg.name.as_str(),
                        "error": "Remote child agents require the parent run_id to be available.",
                    }),
                );
                slots.push(ChildSlot::Failed(
                    "Remote child agents require the parent run_id to be available.".to_string(),
                ));
                continue;
            }
            #[cfg(not(target_family = "wasm"))]
            log_run_agents_event(
                ctx,
                RemoteLogLevel::Info,
                "RunAgents child dispatch queued",
                serde_json::json!({
                    "event": "run_agents_child_dispatch_queued",
                    "action_id": action_id.to_string(),
                    "parent_conversation_id": parent_conversation_id.to_string(),
                    "agent_name": cfg.name.as_str(),
                    "execution_mode": start_agent_execution_mode_label(&mode),
                }),
            );
            let dispatch = self.start_agent_executor.update(ctx, |executor, exec_ctx| {
                let mut dispatch = executor.dispatch(
                    action_id.clone(),
                    cfg.name.clone(),
                    prompt,
                    mode,
                    None, /* lifecycle_subscription */
                    parent_conversation_id,
                    parent_run_id.clone(),
                    exec_ctx,
                );
                if matches!(run_execution_mode, RunAgentsExecutionMode::Local) {
                    executor.yield_after_local_start(&mut dispatch);
                }
                dispatch
            });
            slots.push(ChildSlot::Pending(dispatch));
        }

        let agent_run_configs_for_result = agent_run_configs.clone();
        let action_id_for_aggr = action_id.clone();
        let run_model_id = model_id.clone();
        let run_harness_type = harness_type.clone();
        let run_execution_mode_for_aggr = run_execution_mode.clone();
        let parent_conversation_id_for_result = parent_conversation_id;
        #[cfg(not(target_family = "wasm"))]
        let action_id_for_async_log = action_id.clone();
        #[cfg(not(target_family = "wasm"))]
        let parent_conversation_id_for_async_log = parent_conversation_id;
        #[cfg(not(target_family = "wasm"))]
        let agent_names_for_async_log = agent_run_configs
            .iter()
            .map(|cfg| cfg.name.clone())
            .collect::<Vec<_>>();

        ctx.spawn(
            async move {
                let resolved_slots = join_all(slots.into_iter().map(resolve_child_slot)).await;
                #[cfg(not(target_family = "wasm"))]
                for (slot_index, resolved) in resolved_slots.iter().enumerate() {
                    log::info!(
                        "RunAgents child launch outcome action_id={} parent_conversation_id={} \
                         agent_name={} slot_index={} outcome={}",
                        action_id_for_async_log,
                        parent_conversation_id_for_async_log,
                        agent_names_for_async_log
                            .get(slot_index)
                            .map(String::as_str)
                            .unwrap_or("<unknown>"),
                        slot_index,
                        run_agents_agent_outcome_kind_label(&resolved.outcome)
                    );
                }
                resolved_slots
            },
            move |me, resolved_slots, ctx| {
                if !me.is_pending(parent_conversation_id_for_result, &action_id_for_aggr) {
                    return;
                }
                let timed_out_request_ids = resolved_slots
                    .iter()
                    .filter_map(|resolved| resolved.timed_out_request_id)
                    .collect::<Vec<_>>();
                if !timed_out_request_ids.is_empty() {
                    me.start_agent_executor.update(ctx, |executor, _| {
                        for request_id in timed_out_request_ids {
                            executor.detach_dispatch(request_id);
                        }
                    });
                }
                let agents: Vec<RunAgentsAgentOutcome> = agent_run_configs_for_result
                    .iter()
                    .zip(resolved_slots)
                    .map(|(cfg, resolved)| RunAgentsAgentOutcome {
                        name: cfg.name.clone(),
                        kind: resolved.outcome,
                    })
                    .collect();
                me.record_launched_agents(parent_conversation_id_for_result, &agents);
                if matches!(run_execution_mode_for_aggr, RunAgentsExecutionMode::Local) {
                    let errors = agents.iter().filter_map(|agent| match &agent.kind {
                        RunAgentsAgentOutcomeKind::Failed { error } => Some(format!("{}: {error}", agent.name)),
                        RunAgentsAgentOutcomeKind::Launched { .. } | RunAgentsAgentOutcomeKind::Completed { .. } => None,
                    }).collect();
                    OrchestrationEventService::handle(ctx).update(ctx, |service, ctx| {
                        service.seal_local_group(parent_conversation_id_for_result, &action_id_for_aggr.to_string(), errors, ctx);
                    });
                }

                #[cfg(not(target_family = "wasm"))]
                log_run_agents_event(
                    ctx,
                    RemoteLogLevel::Info,
                    "RunAgents launch outcomes resolved",
                    serde_json::json!({
                        "event": "run_agents_launch_outcomes_resolved",
                        "action_id": action_id_for_aggr.to_string(),
                        "parent_conversation_id": parent_conversation_id_for_result.to_string(),
                        "agent_count": agents.len(),
                        "launched_count": agents.iter().filter(|agent| matches!(agent.kind, RunAgentsAgentOutcomeKind::Launched { .. } | RunAgentsAgentOutcomeKind::Completed { .. })).count(),
                        "failed_count": agents.iter().filter(|agent| matches!(agent.kind, RunAgentsAgentOutcomeKind::Failed { .. })).count(),
                        "agents": agents
                            .iter()
                            .map(|agent| match &agent.kind {
                                RunAgentsAgentOutcomeKind::Launched { agent_id } => serde_json::json!({
                                    "name": agent.name.as_str(),
                                    "status": "launched",
                                    "agent_id": agent_id.as_str(),
                                }),
                                RunAgentsAgentOutcomeKind::Completed { agent_id, output } => serde_json::json!({
                                    "name": agent.name.as_str(),
                                    "status": "completed",
                                    "agent_id": agent_id.as_str(),
                                    "output": output,
                                }),
                                RunAgentsAgentOutcomeKind::Failed { error } => serde_json::json!({
                                    "name": agent.name.as_str(),
                                    "status": "failed",
                                    "error": remote_logging::sanitize_error(error),
                                }),
                            })
                            .collect::<Vec<_>>(),
                    }),
                );
                let launched_mode = match &run_execution_mode_for_aggr {
                    RunAgentsExecutionMode::Local => RunAgentsLaunchedExecutionMode::Local,
                    RunAgentsExecutionMode::Remote {
                        environment_id,
                        worker_host,
                        computer_use_enabled,
                    } => RunAgentsLaunchedExecutionMode::Remote {
                        environment_id: environment_id.clone(),
                        worker_host: worker_host.clone(),
                        computer_use_enabled: *computer_use_enabled,
                    },
                };
                let result = RunAgentsResult::Launched {
                    model_id: run_model_id,
                    harness_type: run_harness_type,
                    execution_mode: launched_mode,
                    agents,
                };
                me.pending.remove(&(
                    parent_conversation_id_for_result,
                    action_id_for_aggr.clone(),
                ));
                ctx.emit(RunAgentsExecutorEvent::SpawningFinished {
                    conversation_id: parent_conversation_id_for_result,
                    action_id: action_id_for_aggr,
                });
                let _ = sender.try_send(result);
            },
        );
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let AIAgentAction { action, id, .. } = input.action;
        let AIAgentActionType::RunAgents(request) = action else {
            return ActionExecution::InvalidAction;
        };
        let mut request = request.clone();
        let action_id = id.clone();
        let parent_conversation_id = input.conversation_id;
        let is_recovery = self
            .recovery_action_ids
            .remove(&(parent_conversation_id, action_id.clone()));

        let recovery_children = if is_recovery {
            prepare_recovery_request_for_execution(&mut request, parent_conversation_id, ctx);
            match recovery_children_by_name(parent_conversation_id, ctx) {
                Ok(children) => children,
                Err(error) => {
                    return ActionExecution::Sync(AIAgentActionResultType::RunAgents(
                        RunAgentsResult::Failure { error },
                    ));
                }
            }
        } else {
            if let Some(reason) = prepare_request_for_execution(
                &mut request,
                parent_conversation_id,
                self.terminal_view_id,
                &self.launched_agents,
                ctx,
            ) {
                #[cfg(not(target_family = "wasm"))]
                log_run_agents_event(
                    ctx,
                    RemoteLogLevel::Warn,
                    "RunAgents execution denied",
                    serde_json::json!({
                        "event": "run_agents_execution_denied",
                        "action_id": action_id.to_string(),
                        "parent_conversation_id": parent_conversation_id.to_string(),
                        "reason": remote_logging::sanitize_error(&reason),
                    }),
                );
                return ActionExecution::Sync(AIAgentActionResultType::RunAgents(
                    RunAgentsResult::Denied { reason },
                ));
            }
            HashMap::new()
        };

        let receiver = if is_recovery {
            self.dispatch_recovered_run_agents(
                action_id,
                request,
                parent_conversation_id,
                recovery_children,
                ctx,
            )
        } else {
            self.dispatch_prepared_run_agents(action_id, request, parent_conversation_id, ctx)
        };

        ActionExecution::new_async(
            async move { receiver.recv().await },
            |result, _| match result {
                Ok(r) => AIAgentActionResultType::RunAgents(r),
                Err(_) => AIAgentActionResultType::RunAgents(RunAgentsResult::Cancelled),
            },
        )
    }

    pub(super) fn should_autoexecute(
        &self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let AIAgentActionType::RunAgents(request) = &input.action.action else {
            return false;
        };
        if child_agent_delegation_denial_reason(input.conversation_id, ctx).is_some() {
            return true;
        }
        if AppExecutionMode::as_ref(ctx).is_autonomous() {
            return true;
        }
        let mut resolved_request = request.clone();
        resolve_request_from_approved_config(&mut resolved_request, input.conversation_id, ctx);
        populate_default_auth_secret_for_execution(&mut resolved_request, ctx);
        if self
            .duplicate_launched_agents_reason(&resolved_request, input.conversation_id, ctx)
            .is_some()
        {
            return true;
        }
        approved_orchestration_config_can_autoexecute(request, input.conversation_id, ctx)
            || BlocklistAIPermissions::as_ref(ctx)
                .get_run_agents_setting(ctx, Some(self.terminal_view_id))
                .is_always_allow()
    }

    pub(super) fn preprocess_action(
        &mut self,
        _action: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

#[cfg(test)]
#[path = "run_agents_tests.rs"]
mod tests;

#[cfg(not(target_family = "wasm"))]
fn log_run_agents_event(
    ctx: &mut ModelContext<RunAgentsExecutor>,
    level: RemoteLogLevel,
    message: impl Into<String>,
    context: serde_json::Value,
) {
    remote_logging::log_model_event(
        ctx,
        RemoteLogRecord {
            level,
            message: message.into(),
            context,
        },
    );
}

#[cfg(not(target_family = "wasm"))]
fn run_agents_execution_mode_label(mode: &RunAgentsExecutionMode) -> &'static str {
    match mode {
        RunAgentsExecutionMode::Local => "local",
        RunAgentsExecutionMode::Remote { .. } => "remote",
    }
}

#[cfg(not(target_family = "wasm"))]
fn start_agent_execution_mode_label(mode: &StartAgentExecutionMode) -> &'static str {
    match mode {
        StartAgentExecutionMode::Local { .. } => "local",
        StartAgentExecutionMode::Remote { .. } => "remote",
    }
}

#[cfg(not(target_family = "wasm"))]
fn run_agents_agent_outcome_kind_label(kind: &RunAgentsAgentOutcomeKind) -> &'static str {
    match kind {
        RunAgentsAgentOutcomeKind::Launched { .. } => "launched",
        RunAgentsAgentOutcomeKind::Completed { .. } => "completed",
        RunAgentsAgentOutcomeKind::Failed { .. } => "failed",
    }
}

enum ChildSlot {
    Failed(String),
    Pending(StartAgentDispatch),
}

#[derive(Debug)]
struct ResolvedChildSlot {
    outcome: RunAgentsAgentOutcomeKind,
    timed_out_request_id: Option<StartAgentRequestId>,
}

async fn resolve_child_slot(slot: ChildSlot) -> ResolvedChildSlot {
    resolve_child_slot_with_timeout(slot, SPAWN_TIMEOUT).await
}

async fn resolve_child_slot_with_timeout(
    slot: ChildSlot,
    spawn_timeout: Duration,
) -> ResolvedChildSlot {
    let dispatch = match slot {
        ChildSlot::Failed(error) => {
            return ResolvedChildSlot {
                outcome: RunAgentsAgentOutcomeKind::Failed { error },
                timed_out_request_id: None,
            };
        }
        ChildSlot::Pending(dispatch) => dispatch,
    };
    let request_id = dispatch.request_id;

    let outcome = match dispatch.wait_policy {
        StartAgentWaitPolicy::Completion | StartAgentWaitPolicy::LocalStartup => {
            dispatch.receiver.recv().await.ok()
        }
        StartAgentWaitPolicy::Startup => {
            let timeout = warpui::r#async::Timer::after(spawn_timeout);
            match futures::future::select(Box::pin(dispatch.receiver.recv()), Box::pin(timeout))
                .await
            {
                futures::future::Either::Left((outcome, _)) => outcome.ok(),
                futures::future::Either::Right((_, _)) => {
                    dispatch.mark_detached();
                    log::warn!(
                        "Agent spawn timed out after {} seconds",
                        spawn_timeout.as_secs()
                    );
                    return ResolvedChildSlot {
                        outcome: RunAgentsAgentOutcomeKind::Failed {
                            error: format!(
                                "Agent failed to start within {} seconds. \
                                 The harness binary may not be installed.",
                                spawn_timeout.as_secs()
                            ),
                        },
                        timed_out_request_id: Some(request_id),
                    };
                }
            }
        }
    };

    let outcome = match outcome {
        Some(StartAgentOutcome::Started { agent_id }) => {
            RunAgentsAgentOutcomeKind::Launched { agent_id }
        }
        Some(StartAgentOutcome::Completed { agent_id, output }) => {
            RunAgentsAgentOutcomeKind::Completed { agent_id, output }
        }
        Some(StartAgentOutcome::Error(error)) => RunAgentsAgentOutcomeKind::Failed { error },
        None => RunAgentsAgentOutcomeKind::Failed {
            error: "Child agent was cancelled before completion".to_string(),
        },
    };
    ResolvedChildSlot {
        outcome,
        timed_out_request_id: None,
    }
}

fn approved_orchestration_config_can_autoexecute(
    request: &RunAgentsRequest,
    parent_conversation_id: AIConversationId,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> bool {
    let mut resolved_request = request.clone();
    resolve_request_from_approved_config(&mut resolved_request, parent_conversation_id, ctx)
        .is_some_and(|status| status.is_approved())
        && can_execute_with_auth_secret(&resolved_request, ctx)
}

fn resolve_request_from_approved_config(
    request: &mut RunAgentsRequest,
    parent_conversation_id: AIConversationId,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> Option<ai::agent::orchestration_config::OrchestrationConfigStatus> {
    let conversation =
        BlocklistAIHistoryModel::as_ref(ctx).conversation(&parent_conversation_id)?;
    let (config, status) = conversation.orchestration_config_for_plan(&request.plan_id)?;
    if status.is_approved() {
        resolve_request_from_config(request, config);
    }
    Some(status)
}

/// Normalizes the request and returns a denial reason when launch is blocked.
///
/// Root autonomous agents bypass interactive policy denials because they cannot
/// present a confirmation card. Child-agent delegation is rejected before that
/// bypass, while allowed root calls still inherit approved config and auth fields.
fn prepare_request_for_execution(
    request: &mut RunAgentsRequest,
    parent_conversation_id: AIConversationId,
    terminal_view_id: EntityId,
    launched_agents: &HashMap<AIConversationId, HashMap<String, ExistingLaunchedAgent>>,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> Option<String> {
    if let Some(reason) = child_agent_delegation_denial_reason(parent_conversation_id, ctx) {
        return Some(reason);
    }

    normalize_request_for_local_execution(request);
    let status = resolve_request_from_approved_config(request, parent_conversation_id, ctx);
    populate_default_auth_secret_for_execution(request, ctx);
    if let Some(reason) =
        duplicate_launched_agents_reason(request, parent_conversation_id, launched_agents, ctx)
    {
        return Some(reason);
    }

    if AppExecutionMode::as_ref(ctx).is_autonomous() {
        return None;
    }

    if status.is_some_and(|status| status.is_disapproved()) {
        return Some("Orchestration config was disapproved".to_string());
    }

    if BlocklistAIPermissions::as_ref(ctx)
        .get_run_agents_setting(ctx, Some(terminal_view_id))
        .is_never_allow()
    {
        return Some(
            "Running child agents is disabled by the active execution profile.".to_string(),
        );
    }

    if !can_execute_with_auth_secret(request, ctx) {
        return Some(
            "Cloud child agents using this harness require an API key before they can run."
                .to_string(),
        );
    }

    None
}

fn prepare_recovery_request_for_execution(
    request: &mut RunAgentsRequest,
    parent_conversation_id: AIConversationId,
    ctx: &ModelContext<RunAgentsExecutor>,
) {
    normalize_request_for_local_execution(request);
    resolve_request_from_approved_config(request, parent_conversation_id, ctx);
    populate_default_auth_secret_for_execution(request, ctx);
}

fn recovery_children_by_name(
    parent_conversation_id: AIConversationId,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> Result<HashMap<String, AIConversationId>, String> {
    let mut children_by_name = HashMap::new();
    for conversation in
        BlocklistAIHistoryModel::as_ref(ctx).child_conversations_of(parent_conversation_id)
    {
        let Some(name) = conversation.agent_name() else {
            continue;
        };
        let Some(normalized_name) = normalize_agent_name(name) else {
            continue;
        };
        if children_by_name
            .insert(normalized_name.clone(), conversation.id())
            .is_some()
        {
            return Err(format!(
                "Cannot recover child agent '{name}': multiple persisted child conversations have the same name."
            ));
        }
    }
    Ok(children_by_name)
}

fn duplicate_launched_agents_reason(
    request: &RunAgentsRequest,
    parent_conversation_id: AIConversationId,
    launched_agents: &HashMap<AIConversationId, HashMap<String, ExistingLaunchedAgent>>,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> Option<String> {
    let requested_agents = request
        .agent_run_configs
        .iter()
        .map(|cfg| normalize_agent_name(&cfg.name).map(|name| (name, cfg.name.clone())))
        .collect::<Option<Vec<_>>>()?;
    if requested_agents.is_empty() {
        return None;
    }

    let existing_agents =
        existing_launched_agents_for_conversation(parent_conversation_id, launched_agents, ctx);
    if existing_agents.is_empty() {
        return None;
    }

    let duplicates = requested_agents
        .iter()
        .filter_map(|(normalized_name, _)| existing_agents.get(normalized_name))
        .collect::<Vec<_>>();
    if duplicates.is_empty() {
        return None;
    }
    let duplicate_list = duplicates
        .iter()
        .map(|agent| format!("{} ({})", agent.name, agent.agent_id))
        .collect::<Vec<_>>()
        .join(", ");
    let addresses = duplicates
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    Some(format!(
        "Requested agent(s) have already been launched: {duplicate_list}. \
         Do not start duplicate child agents; send any follow-up with send_message_to_agent \
         using the existing agent id(s): {addresses}."
    ))
}

fn existing_launched_agents_for_conversation(
    parent_conversation_id: AIConversationId,
    launched_agents: &HashMap<AIConversationId, HashMap<String, ExistingLaunchedAgent>>,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> HashMap<String, ExistingLaunchedAgent> {
    let mut existing_agents = launched_agents
        .get(&parent_conversation_id)
        .cloned()
        .unwrap_or_default();

    if let Some(conversation) =
        BlocklistAIHistoryModel::as_ref(ctx).conversation(&parent_conversation_id)
    {
        for exchange in conversation.all_exchanges() {
            for input in &exchange.input {
                let AIAgentInput::ActionResult { result, .. } = input else {
                    continue;
                };
                let AIAgentActionResultType::RunAgents(RunAgentsResult::Launched {
                    agents, ..
                }) = &result.result
                else {
                    continue;
                };
                for agent in agents {
                    let RunAgentsAgentOutcomeKind::Launched { agent_id } = &agent.kind else {
                        let RunAgentsAgentOutcomeKind::Completed { agent_id, .. } = &agent.kind
                        else {
                            continue;
                        };
                        let Some(normalized_name) = normalize_agent_name(&agent.name) else {
                            continue;
                        };
                        existing_agents.entry(normalized_name).or_insert_with(|| {
                            ExistingLaunchedAgent {
                                name: agent.name.clone(),
                                agent_id: agent_id.clone(),
                            }
                        });
                        continue;
                    };
                    let Some(normalized_name) = normalize_agent_name(&agent.name) else {
                        continue;
                    };
                    existing_agents.entry(normalized_name).or_insert_with(|| {
                        ExistingLaunchedAgent {
                            name: agent.name.clone(),
                            agent_id: agent_id.clone(),
                        }
                    });
                }
            }
        }
    }

    existing_agents
}

fn normalize_agent_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn requires_default_auth_secret_for_execution(request: &RunAgentsRequest) -> bool {
    if !request.execution_mode.is_remote() {
        return false;
    }
    let Some(harness) = Harness::parse_orchestration_harness(&request.harness_type) else {
        return false;
    };
    harness != Harness::Oz && !auth_secret_types_for_harness(harness).is_empty()
}

fn can_execute_with_auth_secret(
    request: &RunAgentsRequest,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> bool {
    if !requires_default_auth_secret_for_execution(request) {
        return true;
    }
    if request
        .harness_auth_secret_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
    {
        return true;
    }
    default_auth_secret_name_for_harness(&request.harness_type, ctx).is_some()
}

fn default_auth_secret_name_for_harness(
    harness_type: &str,
    ctx: &ModelContext<RunAgentsExecutor>,
) -> Option<String> {
    let harness = Harness::parse_orchestration_harness(harness_type)?;
    if harness == Harness::Oz {
        return None;
    }
    CloudAgentSettings::as_ref(ctx)
        .last_selected_auth_secret
        .value()
        .get(harness.config_name())
        .cloned()
        .filter(|name| !name.trim().is_empty())
}

fn populate_default_auth_secret_for_execution(
    request: &mut RunAgentsRequest,
    ctx: &ModelContext<RunAgentsExecutor>,
) {
    if !requires_default_auth_secret_for_execution(request)
        || request
            .harness_auth_secret_name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
    {
        return;
    }
    request.harness_auth_secret_name =
        default_auth_secret_name_for_harness(&request.harness_type, ctx);
}

fn normalize_request_for_local_execution(request: &mut RunAgentsRequest) {
    let edit_state = OrchestrationEditState::from_run_agents_fields(
        &request.model_id,
        &request.harness_type,
        &request.execution_mode,
    );
    request.model_id = edit_state.model_id;
    request.harness_type = edit_state.harness_type;
    request.execution_mode = RunAgentsExecutionMode::Local;
    request.harness_auth_secret_name = None;
}

/// Unconditionally overrides run-wide fields on a `RunAgentsRequest`
/// from the approved orchestration config, delegating to
/// `OrchestrationEditState::override_from_approved_config`.
fn resolve_request_from_config(request: &mut RunAgentsRequest, config: &OrchestrationConfig) {
    // The approved plan config is the source of truth for these run-wide fields,
    // so callers pass a mutable request and continue with the normalized value.
    let mut edit_state = OrchestrationEditState::from_run_agents_fields(
        &request.model_id,
        &request.harness_type,
        &request.execution_mode,
    );
    edit_state.override_from_approved_config(config);
    request.model_id = edit_state.model_id;
    request.harness_type = edit_state.harness_type;
    request.execution_mode = edit_state.execution_mode;
}

/// Defence-in-depth validation; mirrors the card view's
/// `accept_disabled_reason` check.
fn validate_request(request: &RunAgentsRequest) -> Result<(), String> {
    if request.agent_run_configs.is_empty() {
        return Err("orchestrate: empty agent_run_configs".to_string());
    }
    if request.execution_mode.is_remote() {
        return Err("Galaxy only supports local child-agent orchestration.".to_string());
    }

    let mut normalized_names = HashSet::new();
    for config in &request.agent_run_configs {
        let Some(normalized_name) = normalize_agent_name(&config.name) else {
            return Err("orchestrate: agent names must not be empty".to_string());
        };
        if !normalized_names.insert(normalized_name) {
            return Err(format!(
                "orchestrate: duplicate agent name '{}' in the same batch",
                config.name.trim()
            ));
        }
    }

    if matches!(request.execution_mode, RunAgentsExecutionMode::Local) {
        if let Some(harness) = Harness::parse_local_child_harness(&request.harness_type) {
            if let Some(message) = local_harness_product_disabled_message(harness) {
                return Err(message.to_string());
            }
        }
    }
    if matches!(
        request.execution_mode,
        RunAgentsExecutionMode::Remote { .. }
    ) && request.harness_type.eq_ignore_ascii_case("opencode")
    {
        return Err("Remote child agents do not support the opencode harness yet.".to_string());
    }
    Ok(())
}

/// Joins `base_prompt` and a per-agent prompt with `"\n\n"`,
/// falling back to whichever is non-empty.
pub fn compose_run_agents_child_prompt(base_prompt: &str, per_agent_prompt: &str) -> String {
    let base_trimmed = base_prompt.trim();
    let per_agent_trimmed = per_agent_prompt.trim();
    match (base_trimmed.is_empty(), per_agent_trimmed.is_empty()) {
        (false, false) => format!("{base_prompt}\n\n{per_agent_prompt}"),
        (false, true) => base_prompt.to_string(),
        (true, false) => per_agent_prompt.to_string(),
        (true, true) => String::new(),
    }
}

/// Translates shared config and the child's model selection into a per-child
/// [`StartAgentExecutionMode`]. Returns `Err` for rejected
/// combinations (e.g. OpenCode+Remote).
///
/// `run_auth_secret_name` is the managed-secret name the orchestration UI
/// resolved for the run-wide harness; only Remote mode currently consumes
/// it (Local children inherit auth from the user's shell environment).
pub fn run_agents_to_start_agent_mode(
    run_execution_mode: &RunAgentsExecutionMode,
    run_harness_type: &str,
    run_model_id: &str,
    run_skills: &[SkillReference],
    run_auth_secret_name: Option<&str>,
    cfg: &RunAgentsAgentRunConfig,
) -> Result<StartAgentExecutionMode, String> {
    let child_model_id = if cfg.model_id.trim().is_empty() {
        run_model_id.trim()
    } else {
        cfg.model_id.trim()
    };
    match run_execution_mode {
        RunAgentsExecutionMode::Local => {
            let trimmed = run_harness_type.trim();
            let model_id = (!child_model_id.is_empty()).then(|| child_model_id.to_string());
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("oz") {
                Ok(StartAgentExecutionMode::Local {
                    harness_type: None,
                    model_id,
                })
            } else {
                if let Some(harness) = Harness::parse_local_child_harness(trimmed) {
                    if let Some(message) = local_harness_product_disabled_message(harness) {
                        return Err(message.to_string());
                    }
                }
                Ok(StartAgentExecutionMode::Local {
                    harness_type: Some(trimmed.to_string()),
                    model_id,
                })
            }
        }
        RunAgentsExecutionMode::Remote {
            environment_id,
            worker_host,
            computer_use_enabled,
        } => {
            // OpenCode is unsupported on Remote.
            if run_harness_type.eq_ignore_ascii_case("opencode") {
                return Err(
                    "Remote child agents do not support the opencode harness yet.".to_string(),
                );
            }
            Ok(StartAgentExecutionMode::Remote {
                environment_id: environment_id.clone(),
                skill_references: run_skills.to_vec(),
                model_id: child_model_id.to_string(),
                computer_use_enabled: *computer_use_enabled,
                worker_host: worker_host.clone(),
                harness_type: run_harness_type.to_string(),
                title: cfg.title.clone(),
                auth_secret_name: run_auth_secret_name
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty()),
            })
        }
    }
}
