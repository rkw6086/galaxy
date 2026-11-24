use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{FutureExt as _, StreamExt};
use galaxy_agent_core::{
    turn_control, AgentError, AgentErrorKind, AgentEvent, AgentRuntime, CompletedModelTurn,
    ContentPart, ConversationMessage, ExternalWorkId, MessageContent, MessageRole,
    ModelFailureDisposition, PendingToolBatch, ProviderModelCall, ProviderRequestProfile,
    ProviderRun, ProviderRunFailureKind, ProviderRunId, ProviderRunLimits, ProviderRunOutcome,
    ProviderRunPhase, ProviderRunProtocolError, ProviderRunState, ProviderRunStep, RunEpoch,
    RuntimeKind, StopReason, TaskListSnapshot, ToolEvent, TurnCommand, TurnControl, TurnRequest,
    Usage,
};
use instant::Instant;
use warpui::r#async::Timer;

#[cfg(not(target_family = "wasm"))]
use super::model_failure_diagnostics::ModelFailureDiagnostics;
use super::rig_request::{add_orchestration_model_options, OrchestrationModelOption};
use super::task_progress::{add_task_list_context, initial_task_list, task_list_from_plan_batch};
#[cfg(not(target_family = "wasm"))]
use crate::ai::activity;
use crate::ai::agent::conversation::AIConversationId;
#[cfg(not(target_family = "wasm"))]
use crate::ai::model_output_logging;

pub(crate) const BASE_PROVIDER_PROFILE: &str = "base";
pub(crate) const CLI_MONITOR_PROVIDER_PROFILE: &str = "cli-monitor";
pub(crate) const COMPUTER_USE_PROVIDER_PROFILE: &str = "computer-use";
pub(crate) const PROGRESSIVE_SUMMARY_TRIGGER_USAGE: f32 = 0.70;
const PROVIDER_MODEL_START_TIMEOUT: Duration = Duration::from_secs(120);
const PROVIDER_MODEL_EVENT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const PROVIDER_MODEL_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(5),
];

/// Estimated occupancy of the same bounded request projection sent to a provider.
/// The durable transcript remains complete; this snapshot contains no message content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProviderContextProfileUsage {
    pub(crate) profile: String,
    pub(crate) model_id: String,
    pub(crate) context_limit: u32,
    pub(crate) estimated_input_tokens: u32,
    pub(crate) fixed_input_tokens: u32,
    pub(crate) reserved_output_tokens: u32,
    pub(crate) history_tokens: u32,
    pub(crate) history_capacity_tokens: u32,
}

impl ProviderContextProfileUsage {
    pub(crate) fn input_usage(&self) -> f32 {
        self.estimated_input_tokens as f32 / self.context_limit.max(1) as f32
    }

    pub(crate) fn total_reserved_tokens(&self) -> u32 {
        self.estimated_input_tokens
            .saturating_add(self.reserved_output_tokens)
    }

    fn with_transcript(&self, messages: &[ConversationMessage]) -> Self {
        let messages = project_messages_for_context(messages, Some(self.context_limit));
        let text_bytes = messages
            .iter()
            .map(estimate_conversation_message_bytes)
            .sum::<usize>();
        let image_tokens = messages
            .iter()
            .map(|message| count_images_in_content(&message.content))
            .sum::<u64>()
            .saturating_mul(ESTIMATED_IMAGE_TOKENS);
        let history_tokens = (text_bytes as u64)
            .div_ceil(ESTIMATED_BYTES_PER_TOKEN)
            .saturating_add(image_tokens);
        let mut usage = self.clone();
        usage.history_tokens = u32::try_from(history_tokens).unwrap_or(u32::MAX);
        // Without retaining the fixed prompt itself, rounding its token estimate
        // separately from history can differ from the next full estimate by one token.
        usage.estimated_input_tokens = usage
            .fixed_input_tokens
            .saturating_add(usage.history_tokens);
        usage
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProviderContextUsage {
    pub(crate) active: ProviderContextProfileUsage,
    pub(crate) compaction: ProviderContextProfileUsage,
    #[serde(default)]
    pub(crate) monitor: Option<ProviderContextProfileUsage>,
}

impl ProviderContextUsage {
    pub(crate) fn compaction_usage(&self) -> f32 {
        self.compaction.history_tokens as f32
            / self.compaction.history_capacity_tokens.max(1) as f32
    }

    pub(crate) fn with_transcript(&self, messages: &[ConversationMessage]) -> Self {
        let active = self.active.with_transcript(messages);
        let monitor = self
            .monitor
            .as_ref()
            .map(|usage| usage.with_transcript(messages));
        let compaction = controlling_context_profile(&active, monitor.as_ref());
        Self {
            active,
            compaction,
            monitor,
        }
    }
}

fn controlling_context_profile(
    active: &ProviderContextProfileUsage,
    monitor: Option<&ProviderContextProfileUsage>,
) -> ProviderContextProfileUsage {
    monitor
        .filter(|usage| {
            // Profiles bound individual outputs differently. Compare actual
            // projected pressure rather than only the available capacity.
            u64::from(usage.history_tokens) * u64::from(active.history_capacity_tokens.max(1))
                > u64::from(active.history_tokens) * u64::from(usage.history_capacity_tokens.max(1))
        })
        .unwrap_or(active)
        .clone()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderRunProjection {
    ModelTurnRequested {
        work_id: ExternalWorkId,
        profile: ProviderRequestProfile,
        runtime_id: String,
        model_id: String,
        retry_attempt: u32,
        context_usage: Option<ProviderContextUsage>,
    },
    ModelTurnStarted {
        work_id: ExternalWorkId,
        profile: ProviderRequestProfile,
        runtime_id: String,
        model_id: String,
        runtime_request_id: String,
        retry_attempt: u32,
        elapsed_ms: u64,
    },
    ModelTurnFinished {
        work_id: ExternalWorkId,
        profile: ProviderRequestProfile,
        runtime_id: String,
        model_id: String,
        stop_reason: StopReason,
        retry_attempt: u32,
        elapsed_ms: u64,
        tool_call_count: usize,
    },
    ModelEvent {
        work_id: ExternalWorkId,
        event: AgentEvent,
    },
    ModelRetry {
        work_id: ExternalWorkId,
        profile: ProviderRequestProfile,
        runtime_id: String,
        model_id: String,
        retry_attempt: u32,
        max_retries: u32,
        elapsed_ms: u64,
        error: AgentError,
    },
    ToolBatchReady {
        batch: PendingToolBatch,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderRunBlock {
    Tools(PendingToolBatch),
    ReadyToCallModel,
    ContextWindowExceeded,
    AwaitingDriver {
        work_id: ExternalWorkId,
        stop_reason: StopReason,
    },
    Done(ProviderRunOutcome),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ProviderToolExecutionRef {
    pub(crate) conversation_id: AIConversationId,
    pub(crate) run_id: ProviderRunId,
    pub(crate) epoch: RunEpoch,
    pub(crate) call_id: String,
}

impl ProviderToolExecutionRef {
    pub(crate) fn new(
        conversation_id: AIConversationId,
        work_id: &ExternalWorkId,
        call_id: impl Into<String>,
    ) -> Self {
        Self {
            conversation_id,
            run_id: work_id.run_id.clone(),
            epoch: work_id.epoch,
            call_id: call_id.into(),
        }
    }

    pub(crate) fn work_id(&self) -> ExternalWorkId {
        ExternalWorkId {
            run_id: self.run_id.clone(),
            epoch: self.epoch,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderToolLifecycleOutcome {
    Pending,
    BatchCommitted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProviderRunCoordinatorError {
    InvalidRuntime(String),
    InvalidToolLifecycle(String),
    Core(ProviderRunProtocolError),
}

impl fmt::Display for ProviderRunCoordinatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRuntime(message) | Self::InvalidToolLifecycle(message) => {
                f.write_str(message)
            }
            Self::Core(error) => error.fmt(f),
        }
    }
}

impl Error for ProviderRunCoordinatorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRuntime(_) | Self::InvalidToolLifecycle(_) => None,
            Self::Core(error) => Some(error),
        }
    }
}

impl From<ProviderRunProtocolError> for ProviderRunCoordinatorError {
    fn from(value: ProviderRunProtocolError) -> Self {
        Self::Core(value)
    }
}

#[derive(Clone)]
pub(crate) struct ProviderRunProfile {
    pub(crate) runtime: Arc<dyn AgentRuntime>,
    pub(crate) request: TurnRequest,
    pub(crate) max_context_tokens: Option<u32>,
    // Capture capability before a temporarily empty allowed-model catalog hides
    // delegation. Never synthesize this tool for leaf or focused CLI profiles.
    original_orchestration_tool: Option<galaxy_agent_core::ToolDefinition>,
}

impl ProviderRunProfile {
    pub(crate) fn new(runtime: Arc<dyn AgentRuntime>, request: TurnRequest) -> Self {
        let original_orchestration_tool = request
            .tools
            .iter()
            .find(|tool| tool.name == "run_agents")
            .cloned();
        Self {
            runtime,
            request,
            max_context_tokens: None,
            original_orchestration_tool,
        }
    }

    pub(crate) fn with_max_context_tokens(mut self, max_context_tokens: Option<u32>) -> Self {
        self.max_context_tokens = max_context_tokens;
        self
    }

    pub(crate) fn refresh_orchestration_models(&mut self, models: &[OrchestrationModelOption]) {
        const AVAILABLE: &str = "\n## Current Orchestration Availability\nFor child-agent launches, use only run_agents and the model IDs in its current tool schema. That schema is authoritative over older history or generic tool-name lists. start_agent and implicit/default model selection are unavailable. Choose an allowed model explicitly for every child; do not invent another provider or model.\n";
        const UNAVAILABLE: &str = "\n## Current Orchestration Availability\nChild-agent launches are unavailable because this profile has no advertised allowed launch models. Do not call run_agents or start_agent, including with implicit/default models. This current restriction overrides older history or generic tool-name lists. Existing child conversations may still receive messages or deliver results through their separately advertised tools.\n";
        if !models.is_empty()
            && !self
                .request
                .tools
                .iter()
                .any(|tool| tool.name == "run_agents")
        {
            if let Some(tool) = &self.original_orchestration_tool {
                self.request.tools.push(tool.clone());
            }
        }
        add_orchestration_model_options(&mut self.request.tools, models);

        // The original prompt can include a generic tool list from before this
        // catalog was applied. Replace only these exact Galaxy-owned paragraphs
        // instead of rewriting user rules or accumulating per-turn appendices.
        if self.original_orchestration_tool.is_some()
            || self
                .request
                .system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains(AVAILABLE) || prompt.contains(UNAVAILABLE))
        {
            let prompt = self.request.system_prompt.get_or_insert_with(String::new);
            *prompt = prompt.replace(AVAILABLE, "").replace(UNAVAILABLE, "");
            prompt.push_str(
                if self
                    .request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "run_agents")
                {
                    AVAILABLE
                } else {
                    UNAVAILABLE
                },
            );
        }
    }
}

pub(crate) struct ProviderRunCoordinator {
    run: ProviderRun,
    profiles: BTreeMap<String, ProviderRunProfile>,
    model_start_timeout: Duration,
    model_event_idle_timeout: Duration,
    context_compaction_requested: bool,
    max_context_tokens: Option<u32>,
    last_orchestration_models: Option<Vec<OrchestrationModelOption>>,
    #[cfg(not(target_family = "wasm"))]
    activity_session_id: Option<String>,
    #[cfg(not(target_family = "wasm"))]
    model_failure_diagnostics: ModelFailureDiagnostics,
}

impl ProviderRunCoordinator {
    pub(crate) fn from_request(
        run_id: impl Into<galaxy_agent_core::ProviderRunId>,
        base_profile: ProviderRunProfile,
        tool_result_archive: Vec<galaxy_agent_core::ConversationMessage>,
        limits: ProviderRunLimits,
    ) -> Result<Self, ProviderRunCoordinatorError> {
        let mut run = ProviderRun::new(
            run_id,
            base_profile.request.messages.clone(),
            BASE_PROVIDER_PROFILE,
            limits,
        );
        run.replace_tool_result_archive(tool_result_archive);
        run.set_task_list(initial_task_list(&base_profile.request))?;
        let mut profiles = BTreeMap::new();
        profiles.insert(BASE_PROVIDER_PROFILE.to_string(), base_profile);
        Self::new(run, profiles)
    }

    pub(crate) fn new(
        run: ProviderRun,
        profiles: BTreeMap<String, ProviderRunProfile>,
    ) -> Result<Self, ProviderRunCoordinatorError> {
        if !profiles.contains_key(run.profile().as_str()) {
            return Err(ProviderRunCoordinatorError::InvalidRuntime(format!(
                "provider request profile '{}' is not configured",
                run.profile().as_str()
            )));
        }
        for (profile, config) in &profiles {
            validate_profile_runtime(profile, config.runtime.as_ref())?;
        }
        Ok(Self {
            run,
            profiles,
            model_start_timeout: PROVIDER_MODEL_START_TIMEOUT,
            model_event_idle_timeout: PROVIDER_MODEL_EVENT_IDLE_TIMEOUT,
            context_compaction_requested: false,
            max_context_tokens: None,
            last_orchestration_models: None,
            #[cfg(not(target_family = "wasm"))]
            activity_session_id: None,
            #[cfg(not(target_family = "wasm"))]
            model_failure_diagnostics: ModelFailureDiagnostics::default(),
        })
    }

    #[cfg(test)]
    fn set_model_timeouts(&mut self, start: Duration, event_idle: Duration) {
        self.model_start_timeout = start;
        self.model_event_idle_timeout = event_idle;
    }

    #[cfg(test)]
    fn model_start_timeout(&self) -> Duration {
        self.model_start_timeout
    }

    /// Override the model startup timeout for this run. `None` keeps the
    /// compile-time default (`PROVIDER_MODEL_START_TIMEOUT`). The event-idle
    /// timeout is never changed by this call.
    pub(crate) fn set_model_start_timeout(&mut self, timeout_secs: Option<u32>) {
        if let Some(secs) = timeout_secs {
            if secs > 0 {
                self.model_start_timeout = Duration::from_secs(u64::from(secs));
            }
        }
    }

    /// Local inspection identity only; never changes provider requests or checkpoints.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn set_activity_session_id(&mut self, conversation_id: AIConversationId) {
        self.activity_session_id = Some(conversation_id.to_string());
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn model_failure_diagnostics(&self) -> serde_json::Value {
        self.model_failure_diagnostics.snapshot()
    }

    #[cfg(not(target_family = "wasm"))]
    fn activity_context(
        &self,
        call: &ProviderModelCall,
        profile: &ProviderRunProfile,
    ) -> serde_json::Value {
        serde_json::json!({
            "conversation_id": self.activity_session_id,
            "provider_run_id": call.work_id.run_id.as_str(),
            "provider_epoch": call.work_id.epoch.get(),
            "profile": call.profile.as_str(),
            "runtime_id": profile.runtime.descriptor().id,
            "model_id": profile.request.model.as_str(),
            "retry_attempt": call.retry_attempt,
            "snapshot_kind": "provider_neutral",
        })
    }

    pub(crate) fn run(&self) -> &ProviderRun {
        &self.run
    }

    pub(crate) fn run_mut(&mut self) -> &mut ProviderRun {
        &mut self.run
    }

    pub(crate) fn profile_request(&self, profile: &str) -> Option<&TurnRequest> {
        self.profiles.get(profile).map(|profile| &profile.request)
    }

    pub(crate) fn has_orchestration_tools(&self) -> bool {
        self.profiles.iter().any(|(name, profile)| {
            name != CLI_MONITOR_PROVIDER_PROFILE && profile.original_orchestration_tool.is_some()
        })
    }

    /// Reconcile advertised model choices only between model calls. Permission
    /// checks remain authoritative for actions already emitted by an older call.
    pub(crate) fn refresh_orchestration_models(
        &mut self,
        models: &[OrchestrationModelOption],
    ) -> bool {
        if !matches!(self.run.state(), ProviderRunState::ReadyToCallModel)
            || self.last_orchestration_models.as_deref() == Some(models)
        {
            return false;
        }
        for (name, profile) in &mut self.profiles {
            if name != CLI_MONITOR_PROVIDER_PROFILE {
                profile.refresh_orchestration_models(models);
            }
        }
        self.last_orchestration_models = Some(models.to_vec());
        true
    }

    /// The pending model profile owns the context budget, including after a
    /// preflight overflow has returned the run to its compaction boundary.
    pub(crate) fn current_context_limit(&self) -> Option<u32> {
        self.profiles
            .get(self.run.profile().as_str())
            .and_then(|profile| profile.max_context_tokens)
            .or(self.max_context_tokens)
    }

    /// Leave enough history space for a command-monitor turn before switching
    /// profiles, accounting for each profile's own fixed prompt and output budget.
    pub(crate) fn progressive_summary_context_budget(&self) -> Option<(u32, u32)> {
        [self.run.profile().as_str(), CLI_MONITOR_PROVIDER_PROFILE]
            .into_iter()
            .filter_map(|name| self.context_profile_usage(name))
            .min_by_key(|usage| usage.history_capacity_tokens)
            .map(|usage| {
                (
                    usage.context_limit,
                    usage
                        .fixed_input_tokens
                        .saturating_add(usage.reserved_output_tokens),
                )
            })
    }

    /// Shared by compaction decisions and the context display. Each profile is
    /// projected with its own output bounds before measuring its usable history.
    pub(crate) fn context_usage_snapshot(&self) -> Option<ProviderContextUsage> {
        let active = self.context_profile_usage(self.run.profile().as_str())?;
        let monitor = self.context_profile_usage(CLI_MONITOR_PROVIDER_PROFILE);
        let compaction = controlling_context_profile(&active, monitor.as_ref());
        Some(ProviderContextUsage {
            active,
            compaction,
            monitor,
        })
    }

    /// Retention can use these projected message sizes while preserving the same
    /// indices into the complete durable transcript for prefix validation.
    pub(crate) fn projected_transcript_for_context(
        &self,
        profile: &str,
    ) -> Option<Vec<ConversationMessage>> {
        Some(self.projected_request_for_context(profile)?.messages)
    }

    fn projected_request_for_context(&self, name: &str) -> Option<TurnRequest> {
        let profile = self.profiles.get(name)?;
        Some(project_request_for_messages(
            profile.request.clone(),
            self.run.transcript(),
            profile.max_context_tokens.or(self.max_context_tokens),
            self.run.task_list(),
        ))
    }

    pub(crate) fn context_profile_usage(&self, name: &str) -> Option<ProviderContextProfileUsage> {
        let profile = self.profiles.get(name)?;
        let limit = profile.max_context_tokens.or(self.max_context_tokens)?;
        let request = self.projected_request_for_context(name)?;
        Some(context_profile_usage(name, limit, &request))
    }

    /// Includes the current transcript, fixed request context, tools, and output reservation.
    /// Read at the model boundary so newly committed tool output is included.
    pub(crate) fn estimated_context_tokens(&self) -> Option<(u32, u32)> {
        let request = self.projected_request_for_context(self.run.profile().as_str())?;
        let mut fixed_request = request.clone();
        fixed_request.messages.clear();
        Some((
            u32::try_from(estimate_turn_request_tokens(&request)).unwrap_or(u32::MAX),
            u32::try_from(estimate_turn_request_tokens(&fixed_request)).unwrap_or(u32::MAX),
        ))
    }

    pub(crate) fn set_max_context_tokens(&mut self, max_context_tokens: Option<u32>) {
        self.max_context_tokens = max_context_tokens;
    }

    pub(crate) fn insert_profile(
        &mut self,
        profile: impl Into<String>,
        config: ProviderRunProfile,
    ) -> Result<(), ProviderRunCoordinatorError> {
        let profile = profile.into();
        validate_profile_runtime(&profile, config.runtime.as_ref())?;
        self.profiles.insert(profile, config);
        self.last_orchestration_models = None;
        Ok(())
    }

    pub(crate) fn apply_tool_lifecycle(
        &mut self,
        execution_ref: &ProviderToolExecutionRef,
        event: &ToolEvent,
    ) -> Result<ProviderToolLifecycleOutcome, ProviderRunCoordinatorError> {
        let event_call_id = tool_event_call_id(event)?;
        if event_call_id != execution_ref.call_id {
            return Err(ProviderRunCoordinatorError::InvalidToolLifecycle(format!(
                "provider tool lifecycle call ID mismatch: expected '{}', received '{}'",
                execution_ref.call_id, event_call_id
            )));
        }

        let work_id = execution_ref.work_id();
        match event {
            ToolEvent::Proposed { .. } => {
                return Err(ProviderRunCoordinatorError::InvalidToolLifecycle(
                    "tool proposals must be committed by the model turn before action execution"
                        .to_string(),
                ));
            }
            ToolEvent::PermissionRequested { request } => {
                self.run
                    .request_tool_permission(&work_id, request.clone())?;
            }
            ToolEvent::PermissionResolved {
                request_id,
                call_id,
                decision,
            } => {
                self.run.resolve_tool_permission(
                    &work_id,
                    call_id,
                    request_id,
                    decision.clone(),
                )?;
            }
            ToolEvent::Started { call_id } => {
                self.run.start_tool(&work_id, call_id)?;
            }
            ToolEvent::Completed { result } => {
                self.run.complete_tool(&work_id, result.clone())?;
            }
        }

        let batch_is_complete = match self.run.state() {
            ProviderRunState::AwaitingTools { batch } => batch.is_complete(),
            ProviderRunState::ReadyToCallModel
            | ProviderRunState::AwaitingModel { .. }
            | ProviderRunState::ResolvingModel { .. }
            | ProviderRunState::AwaitingDriver { .. }
            | ProviderRunState::Done { .. }
            | ProviderRunState::Failed { .. }
            | ProviderRunState::Cancelled { .. } => false,
        };
        if batch_is_complete {
            self.run.commit_tool_batch(&work_id)?;
            Ok(ProviderToolLifecycleOutcome::BatchCommitted)
        } else {
            Ok(ProviderToolLifecycleOutcome::Pending)
        }
    }

    pub(crate) async fn drive_until_blocked<F>(
        &mut self,
        control: TurnControl,
        project: F,
    ) -> Result<ProviderRunBlock, ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> Result<(), String>,
    {
        self.drive_until_blocked_with_checkpoint(control, project, |_| Box::pin(async { Ok(()) }))
            .await
    }

    pub(crate) async fn drive_until_blocked_with_checkpoint<F, C>(
        &mut self,
        control: TurnControl,
        mut project: F,
        checkpoint: C,
    ) -> Result<ProviderRunBlock, ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> Result<(), String>,
        C: FnMut(ProviderRun) -> BoxFuture<'static, Result<(), String>>,
    {
        self.drive_until_blocked_with_acknowledgements(
            control,
            move |projection| {
                let result = project(projection);
                Box::pin(async move { result })
            },
            checkpoint,
        )
        .await
    }

    pub(crate) async fn drive_until_blocked_with_acknowledgements<F, C>(
        &mut self,
        control: TurnControl,
        mut project: F,
        mut checkpoint: C,
    ) -> Result<ProviderRunBlock, ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> BoxFuture<'static, Result<(), String>>,
        C: FnMut(ProviderRun) -> BoxFuture<'static, Result<(), String>>,
    {
        loop {
            match self.run.next_step()? {
                Some(ProviderRunStep::CallModel(call)) => {
                    if !self.checkpoint_or_fail(&mut checkpoint).await? {
                        return self.terminal_block();
                    }
                    if self.request_needs_context_compaction(&call) {
                        self.run.prepare_context_compaction(&call.work_id)?;
                        return Ok(ProviderRunBlock::ContextWindowExceeded);
                    }
                    self.drive_model_call_acknowledged(call, control.clone(), &mut project)
                        .await?;
                    if self.context_compaction_requested {
                        self.context_compaction_requested = false;
                        return Ok(ProviderRunBlock::ContextWindowExceeded);
                    }
                }
                Some(ProviderRunStep::DispatchTools(batch)) => {
                    if self.run.task_list().is_none() {
                        self.run.set_task_list(task_list_from_plan_batch(&batch))?;
                    }
                    if !self.checkpoint_or_fail(&mut checkpoint).await? {
                        return self.terminal_block();
                    }
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ToolBatchReady {
                                batch: batch.clone(),
                            },
                            &mut project,
                        )
                        .await?
                    {
                        continue;
                    }
                    if batch.is_complete() {
                        self.run.commit_tool_batch(&batch.work_id)?;
                        return Ok(ProviderRunBlock::ReadyToCallModel);
                    }
                    return Ok(ProviderRunBlock::Tools(batch));
                }
                Some(ProviderRunStep::Done(outcome)) => {
                    if !self.checkpoint_or_fail(&mut checkpoint).await? {
                        return self.terminal_block();
                    }
                    return Ok(ProviderRunBlock::Done(outcome));
                }
                None => match self.run.state() {
                    ProviderRunState::AwaitingDriver {
                        work_id,
                        stop_reason,
                    } => {
                        let work_id = work_id.clone();
                        let stop_reason = stop_reason.clone();
                        if !self.checkpoint_or_fail(&mut checkpoint).await? {
                            return self.terminal_block();
                        }
                        return Ok(ProviderRunBlock::AwaitingDriver {
                            work_id,
                            stop_reason,
                        });
                    }
                    ProviderRunState::ReadyToCallModel
                    | ProviderRunState::AwaitingModel { .. }
                    | ProviderRunState::ResolvingModel { .. }
                    | ProviderRunState::AwaitingTools { .. }
                    | ProviderRunState::Done { .. }
                    | ProviderRunState::Failed { .. }
                    | ProviderRunState::Cancelled { .. } => {
                        return Err(ProviderRunCoordinatorError::Core(
                            ProviderRunProtocolError::UnexpectedState {
                                expected: ProviderRunPhase::AwaitingDriver,
                                actual: self.run.state().phase(),
                            },
                        ));
                    }
                },
            }
        }
    }

    fn request_needs_context_compaction(&self, call: &ProviderModelCall) -> bool {
        let Some(profile) = self.profiles.get(call.profile.as_str()) else {
            return false;
        };
        let Some(max_context_tokens) = profile.max_context_tokens.or(self.max_context_tokens)
        else {
            return false;
        };
        let request = request_for_model_call(
            profile.request.clone(),
            call,
            Some(max_context_tokens),
            self.run.task_list(),
        );
        // Provider tokenizers and request envelopes differ. Compact before the
        // estimate reaches the advertised limit, leaving room for that uncertainty.
        let safe_context_tokens = u64::from(max_context_tokens) * 9 / 10;
        u64::from(
            context_profile_usage(call.profile.as_str(), max_context_tokens, &request)
                .total_reserved_tokens(),
        ) >= safe_context_tokens
    }

    async fn checkpoint_or_fail<C>(
        &mut self,
        checkpoint: &mut C,
    ) -> Result<bool, ProviderRunCoordinatorError>
    where
        C: FnMut(ProviderRun) -> BoxFuture<'static, Result<(), String>>,
    {
        match checkpoint(self.run.clone()).await {
            Ok(()) => Ok(true),
            Err(message) => {
                self.run.fail(
                    ProviderRunFailureKind::ExternalWork,
                    format!("provider run checkpoint failed: {message}"),
                )?;
                Ok(false)
            }
        }
    }

    fn terminal_block(&mut self) -> Result<ProviderRunBlock, ProviderRunCoordinatorError> {
        match self.run.next_step()? {
            Some(ProviderRunStep::Done(outcome)) => Ok(ProviderRunBlock::Done(outcome)),
            Some(ProviderRunStep::CallModel(_) | ProviderRunStep::DispatchTools(_)) | None => Err(
                ProviderRunCoordinatorError::Core(ProviderRunProtocolError::UnexpectedState {
                    expected: ProviderRunPhase::Failed,
                    actual: self.run.state().phase(),
                }),
            ),
        }
    }

    async fn drive_model_call_acknowledged<F>(
        &mut self,
        call: ProviderModelCall,
        control: TurnControl,
        project: &mut F,
    ) -> Result<(), ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> BoxFuture<'static, Result<(), String>>,
    {
        let Some(profile) = self.profiles.get(call.profile.as_str()).cloned() else {
            self.run.fail(
                ProviderRunFailureKind::ExternalWork,
                format!(
                    "provider request profile '{}' is not configured",
                    call.profile.as_str()
                ),
            )?;
            return Ok(());
        };
        let advertised_tools = profile
            .request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<BTreeSet<_>>();
        let runtime_id = profile.runtime.descriptor().id.clone();
        let model_id = profile.request.model.as_str().to_string();
        if !self
            .project_or_fail_acknowledged(
                ProviderRunProjection::ModelTurnRequested {
                    work_id: call.work_id.clone(),
                    profile: call.profile.clone(),
                    runtime_id: runtime_id.clone(),
                    model_id: model_id.clone(),
                    retry_attempt: call.retry_attempt,
                    context_usage: self.context_usage_snapshot(),
                },
                project,
            )
            .await?
        {
            return Ok(());
        }
        let request = request_for_model_call(
            profile.request.clone(),
            &call,
            profile.max_context_tokens.or(self.max_context_tokens),
            self.run.task_list(),
        );
        #[cfg(not(target_family = "wasm"))]
        if activity::is_recording() {
            activity::record_serializable(
                "model_request",
                "Model request",
                self.activity_context(&call, &profile),
                &request,
            );
        }
        let started_at = Instant::now();
        let (attempt_sender, attempt_control) = turn_control();
        let start_future = profile.runtime.start_turn(request, attempt_control).fuse();
        let timeout = futures::FutureExt::fuse(Timer::after(self.model_start_timeout));
        futures::pin_mut!(start_future, timeout);
        let mut control_open = true;
        let start_result = loop {
            let command = if control_open {
                futures::future::Either::Left(control.receive())
            } else {
                futures::future::Either::Right(futures::future::pending())
            }
            .fuse();
            futures::pin_mut!(command);
            futures::select_biased! {
                result = start_future => break Some(result),
                _ = timeout => {
                    let _ = attempt_sender.send(TurnCommand::Cancel).await;
                    break None;
                }
                command = command => match command {
                    Ok(command) => {
                        let _ = attempt_sender.send(command).await;
                    }
                    Err(_) => control_open = false,
                },
            }
        };
        let stream = match start_result {
            Some(Ok(stream)) => stream,
            Some(Err(error)) => {
                self.handle_model_failure_acknowledged(&call, &profile, started_at, error, project)
                    .await?;
                return Ok(());
            }
            None => {
                self.handle_model_failure_acknowledged(
                    &call,
                    &profile,
                    started_at,
                    provider_timeout_error("start", self.model_start_timeout),
                    project,
                )
                .await?;
                return Ok(());
            }
        };
        futures::pin_mut!(stream);
        let mut buffer = ModelTurnBuffer::default();
        #[cfg(not(target_family = "wasm"))]
        let mut usage_reported = false;

        loop {
            let next_event = stream.next().fuse();
            let timeout = futures::FutureExt::fuse(Timer::after(self.model_event_idle_timeout));
            futures::pin_mut!(next_event, timeout);
            let event_result = loop {
                let command = if control_open {
                    futures::future::Either::Left(control.receive())
                } else {
                    futures::future::Either::Right(futures::future::pending())
                }
                .fuse();
                futures::pin_mut!(command);
                futures::select_biased! {
                    event = next_event => break Some(event),
                    _ = timeout => {
                        let _ = attempt_sender.send(TurnCommand::Cancel).await;
                        break None;
                    }
                    command = command => match command {
                        Ok(command) => {
                            let _ = attempt_sender.send(command).await;
                        }
                        Err(_) => control_open = false,
                    },
                }
            };
            let event = match event_result {
                Some(Some(Ok(event))) => event,
                Some(Some(Err(error))) => {
                    self.handle_model_failure_acknowledged(
                        &call, &profile, started_at, error, project,
                    )
                    .await?;
                    return Ok(());
                }
                Some(None) => break,
                None => {
                    self.handle_model_failure_acknowledged(
                        &call,
                        &profile,
                        started_at,
                        provider_timeout_error("event", self.model_event_idle_timeout),
                        project,
                    )
                    .await?;
                    return Ok(());
                }
            };
            #[cfg(not(target_family = "wasm"))]
            model_output_logging::log(serde_json::json!({
                "event": "model_output",
                "source": "direct_provider",
                "provider_run_id": call.work_id.run_id.as_str(),
                "provider_epoch": call.work_id.epoch.get(),
                "profile": call.profile.as_str(),
                "runtime_id": runtime_id,
                "model_id": model_id,
                "retry_attempt": call.retry_attempt,
                "payload": &event,
            }));
            match event {
                AgentEvent::TurnStarted { runtime_request_id } => {
                    if buffer.started {
                        self.handle_model_failure_acknowledged(
                            &call,
                            &profile,
                            started_at,
                            protocol_error("provider emitted more than one TurnStarted event"),
                            project,
                        )
                        .await?;
                        return Ok(());
                    }
                    if runtime_request_id.is_empty() {
                        self.handle_model_failure_acknowledged(
                            &call,
                            &profile,
                            started_at,
                            protocol_error("provider emitted an empty runtime request ID"),
                            project,
                        )
                        .await?;
                        return Ok(());
                    }
                    buffer.started = true;
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelTurnStarted {
                                work_id: call.work_id.clone(),
                                profile: call.profile.clone(),
                                runtime_id: runtime_id.clone(),
                                model_id: model_id.clone(),
                                runtime_request_id,
                                retry_attempt: call.retry_attempt,
                                elapsed_ms: elapsed_millis(started_at),
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                }
                AgentEvent::KeepAlive => {}
                AgentEvent::ToolCallProgress {
                    call_id,
                    name,
                    arguments_bytes,
                } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelEvent {
                                work_id: call.work_id.clone(),
                                event: AgentEvent::ToolCallProgress {
                                    call_id,
                                    name,
                                    arguments_bytes,
                                },
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                }
                AgentEvent::TextDelta { text } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    buffer.text.push_str(&text);
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelEvent {
                                work_id: call.work_id.clone(),
                                event: AgentEvent::TextDelta { text },
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                }
                AgentEvent::ReasoningDelta { text } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    buffer.reasoning.push_str(&text);
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelEvent {
                                work_id: call.work_id.clone(),
                                event: AgentEvent::ReasoningDelta { text },
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                }
                AgentEvent::ReasoningCompleted { text, signature }
                    if text.is_empty() && signature.is_none() =>
                {
                    // Ignore provider heartbeat-shaped empty reasoning events. They are not
                    // meaningful model progress and must not keep an otherwise idle turn alive.
                    continue;
                }
                AgentEvent::ReasoningCompleted { text, signature } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    buffer.reasoning.clone_from(&text);
                    buffer.reasoning_signature.clone_from(&signature);
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelEvent {
                                work_id: call.work_id.clone(),
                                event: AgentEvent::ReasoningCompleted { text, signature },
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                }
                AgentEvent::Tool {
                    event: ToolEvent::Proposed { call: tool_call },
                } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    buffer.tool_calls.push(tool_call);
                }
                AgentEvent::UsageUpdated { usage } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    buffer.usage.clone_from(&usage);
                    #[cfg(not(target_family = "wasm"))]
                    {
                        usage_reported = true;
                    }
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelEvent {
                                work_id: call.work_id.clone(),
                                event: AgentEvent::UsageUpdated { usage },
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                }
                AgentEvent::TurnStopped { reason } => {
                    if !self
                        .ensure_model_started_acknowledged(
                            &call, &profile, started_at, &buffer, project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    // Output is projected as it arrives. The buffer is only for canonical
                    // history; replaying it here would duplicate the streamed response.
                    if !self
                        .project_or_fail_acknowledged(
                            ProviderRunProjection::ModelTurnFinished {
                                work_id: call.work_id.clone(),
                                profile: call.profile.clone(),
                                runtime_id: runtime_id.clone(),
                                model_id: model_id.clone(),
                                stop_reason: reason.clone(),
                                retry_attempt: call.retry_attempt,
                                elapsed_ms: elapsed_millis(started_at),
                                tool_call_count: buffer.tool_calls.len(),
                            },
                            project,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    #[cfg(not(target_family = "wasm"))]
                    if activity::is_recording() {
                        let mut context = self.activity_context(&call, &profile);
                        context["elapsed_ms"] = serde_json::json!(elapsed_millis(started_at));
                        context["stop_reason"] = serde_json::json!(&reason);
                        context["usage_reported"] = serde_json::json!(usage_reported);
                        activity::record_serializable(
                            "model_response",
                            "Model response",
                            context,
                            &buffer,
                        );
                    }
                    if reason == StopReason::Cancelled {
                        self.run.cancel("provider model call was cancelled")?;
                        return Ok(());
                    }
                    if reason == StopReason::ContextWindowExceeded {
                        self.run.prepare_context_compaction(&call.work_id)?;
                        self.context_compaction_requested = true;
                        return Ok(());
                    }
                    let turn = buffer.complete(reason, advertised_tools);
                    if let Err(error) = self.run.accept_model_turn(&call.work_id, turn) {
                        self.run.fail(
                            ProviderRunFailureKind::Protocol,
                            format!("provider returned an invalid completed turn: {error}"),
                        )?;
                    }
                    return Ok(());
                }
                AgentEvent::Tool {
                    event:
                        ToolEvent::PermissionRequested { .. }
                        | ToolEvent::PermissionResolved { .. }
                        | ToolEvent::Started { .. }
                        | ToolEvent::Completed { .. },
                }
                | AgentEvent::RuntimeActivityUpdated { .. }
                | AgentEvent::ContextUsageUpdated { .. }
                | AgentEvent::UserInputAccepted { .. }
                | AgentEvent::RuntimeNotice { .. } => {
                    self.handle_model_failure_acknowledged(
                        &call,
                        &profile,
                        started_at,
                        protocol_error(
                            "direct-provider transport emitted a non-model lifecycle event",
                        ),
                        project,
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        self.handle_model_failure_acknowledged(
            &call,
            &profile,
            started_at,
            protocol_error("provider stream ended before TurnStopped"),
            project,
        )
        .await?;
        Ok(())
    }

    async fn ensure_model_started_acknowledged<F>(
        &mut self,
        call: &ProviderModelCall,
        profile: &ProviderRunProfile,
        started_at: Instant,
        buffer: &ModelTurnBuffer,
        project: &mut F,
    ) -> Result<bool, ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> BoxFuture<'static, Result<(), String>>,
    {
        if buffer.started {
            return Ok(true);
        }
        self.handle_model_failure_acknowledged(
            call,
            profile,
            started_at,
            protocol_error("provider emitted model output before TurnStarted"),
            project,
        )
        .await?;
        Ok(false)
    }

    async fn handle_model_failure_acknowledged<F>(
        &mut self,
        call: &ProviderModelCall,
        profile: &ProviderRunProfile,
        started_at: Instant,
        error: AgentError,
        project: &mut F,
    ) -> Result<(), ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> BoxFuture<'static, Result<(), String>>,
    {
        #[cfg(not(target_family = "wasm"))]
        {
            let mut identity = self.activity_context(call, profile);
            identity["elapsed_ms"] = serde_json::json!(elapsed_millis(started_at));
            self.model_failure_diagnostics.record(identity, &error);
        }
        #[cfg(not(target_family = "wasm"))]
        model_output_logging::log(serde_json::json!({
            "event": "model_error",
            "source": "direct_provider",
            "provider_run_id": call.work_id.run_id.as_str(),
            "provider_epoch": call.work_id.epoch.get(),
            "profile": call.profile.as_str(),
            "runtime_id": profile.runtime.descriptor().id,
            "model_id": profile.request.model.as_str(),
            "retry_attempt": call.retry_attempt,
            "payload": &error,
        }));
        #[cfg(not(target_family = "wasm"))]
        if activity::is_recording() {
            let mut context = self.activity_context(call, profile);
            context["elapsed_ms"] = serde_json::json!(elapsed_millis(started_at));
            context["error_kind"] = serde_json::json!(&error.kind);
            context["error"] =
                serde_json::json!(crate::ai::remote_logging::sanitize_error(&error.message));
            activity::record("model_error", "Model request failed", context);
        }
        let error_message = error.message.clone();
        if error.kind == AgentErrorKind::ContextWindowExceeded {
            self.run.prepare_context_compaction(&call.work_id)?;
            self.context_compaction_requested = true;
            return Ok(());
        }
        let disposition = self
            .run
            .register_model_failure(&call.work_id, error.clone())?;
        if disposition == ModelFailureDisposition::RetryScheduled {
            let retry_attempt = match self.run.state() {
                ProviderRunState::AwaitingModel { call } => call.retry_attempt,
                ProviderRunState::ReadyToCallModel
                | ProviderRunState::ResolvingModel { .. }
                | ProviderRunState::AwaitingTools { .. }
                | ProviderRunState::AwaitingDriver { .. }
                | ProviderRunState::Done { .. }
                | ProviderRunState::Failed { .. }
                | ProviderRunState::Cancelled { .. } => {
                    return Err(ProviderRunCoordinatorError::Core(
                        ProviderRunProtocolError::UnexpectedState {
                            expected: ProviderRunPhase::AwaitingModel,
                            actual: self.run.state().phase(),
                        },
                    ));
                }
            };
            self.project_or_fail_acknowledged(
                ProviderRunProjection::ModelRetry {
                    work_id: call.work_id.clone(),
                    profile: call.profile.clone(),
                    runtime_id: profile.runtime.descriptor().id.clone(),
                    model_id: profile.request.model.as_str().to_string(),
                    retry_attempt,
                    max_retries: self.run.max_model_retries_per_turn(),
                    elapsed_ms: elapsed_millis(started_at),
                    error,
                },
                project,
            )
            .await?;
            let delay = PROVIDER_MODEL_RETRY_DELAYS
                .get(retry_attempt.saturating_sub(1) as usize)
                .copied()
                .unwrap_or_default();
            log::warn!(
                "rig model call failed; retrying attempt {retry_attempt} after {}s: {}",
                delay.as_secs(),
                error_message
            );
            if !delay.is_zero() {
                Timer::after(delay).await;
            }
            return Ok(());
        }
        log::error!(
            "rig model call failed permanently after {} retries: {}",
            call.retry_attempt,
            error_message
        );
        Ok(())
    }

    async fn project_or_fail_acknowledged<F>(
        &mut self,
        event: ProviderRunProjection,
        project: &mut F,
    ) -> Result<bool, ProviderRunCoordinatorError>
    where
        F: FnMut(ProviderRunProjection) -> BoxFuture<'static, Result<(), String>>,
    {
        match project(event).await {
            Ok(()) => Ok(true),
            Err(message) => {
                self.run.fail(
                    ProviderRunFailureKind::Projection,
                    format!("provider run projection failed: {message}"),
                )?;
                Ok(false)
            }
        }
    }
}

#[derive(Default, serde::Serialize)]
struct ModelTurnBuffer {
    started: bool,
    text: String,
    reasoning: String,
    reasoning_signature: Option<String>,
    tool_calls: Vec<galaxy_agent_core::ToolCall>,
    usage: Usage,
}

impl ModelTurnBuffer {
    fn complete(
        self,
        stop_reason: StopReason,
        advertised_tools: BTreeSet<String>,
    ) -> CompletedModelTurn {
        let mut assistant_content = Vec::new();
        if !self.reasoning.is_empty() || self.reasoning_signature.is_some() {
            assistant_content.push(ContentPart::Reasoning {
                text: self.reasoning,
                signature: self.reasoning_signature,
            });
        }
        if !self.text.is_empty() {
            assistant_content.push(ContentPart::Text(self.text));
        }
        CompletedModelTurn {
            assistant_content,
            tool_calls: self.tool_calls,
            usage: self.usage,
            stop_reason,
            advertised_tools,
        }
    }
}

fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn provider_timeout_error(stage: &str, timeout: Duration) -> AgentError {
    let mut error = AgentError::new(
        AgentErrorKind::Transport,
        format!(
            "provider model {stage} timed out after {} seconds",
            timeout.as_secs()
        ),
    );
    error.recoverable = true;
    error
}

fn validate_profile_runtime(
    profile: &str,
    runtime: &dyn AgentRuntime,
) -> Result<(), ProviderRunCoordinatorError> {
    let descriptor = runtime.descriptor();
    if descriptor.kind != RuntimeKind::Provider {
        return Err(ProviderRunCoordinatorError::InvalidRuntime(format!(
            "runtime '{}' for provider request profile '{profile}' is not a direct-provider transport",
            descriptor.id
        )));
    }
    if !descriptor.capabilities.host_managed_history || !descriptor.capabilities.host_tool_execution
    {
        return Err(ProviderRunCoordinatorError::InvalidRuntime(format!(
            "runtime '{}' for provider request profile '{profile}' does not expose Galaxy-owned history and tools",
            descriptor.id
        )));
    }
    Ok(())
}

fn request_for_model_call(
    template: TurnRequest,
    call: &ProviderModelCall,
    max_context_tokens: Option<u32>,
    task_list: Option<&TaskListSnapshot>,
) -> TurnRequest {
    project_request_for_messages(template, &call.messages, max_context_tokens, task_list)
}

fn project_request_for_messages(
    mut template: TurnRequest,
    messages: &[ConversationMessage],
    max_context_tokens: Option<u32>,
    task_list: Option<&TaskListSnapshot>,
) -> TurnRequest {
    template.prompt = None;
    template.messages = project_messages_for_context(messages, max_context_tokens);
    add_task_list_context(&mut template, task_list);
    add_exploration_reminder(&mut template);
    template
}

fn project_messages_for_context(
    messages: &[ConversationMessage],
    max_context_tokens: Option<u32>,
) -> Vec<ConversationMessage> {
    let mut messages = messages.to_vec();
    for message in &mut messages {
        message.truncate_tool_results_for_provider_request_with_context(max_context_tokens);
        message
            .truncate_tagged_terminal_output_for_provider_request_with_context(max_context_tokens);
    }
    messages
}

fn context_profile_usage(
    profile: &str,
    context_limit: u32,
    request: &TurnRequest,
) -> ProviderContextProfileUsage {
    let reserved_output_tokens = request.max_output_tokens.unwrap_or_default();
    let estimated_input_tokens =
        estimate_turn_request_tokens(request).saturating_sub(reserved_output_tokens);
    let mut fixed_request = request.clone();
    fixed_request.messages.clear();
    let fixed_input_tokens =
        estimate_turn_request_tokens(&fixed_request).saturating_sub(reserved_output_tokens);
    ProviderContextProfileUsage {
        profile: profile.to_owned(),
        model_id: request.model.as_str().to_owned(),
        context_limit,
        estimated_input_tokens: u32::try_from(estimated_input_tokens).unwrap_or(u32::MAX),
        fixed_input_tokens: u32::try_from(fixed_input_tokens).unwrap_or(u32::MAX),
        reserved_output_tokens: u32::try_from(reserved_output_tokens).unwrap_or(u32::MAX),
        history_tokens: u32::try_from(estimated_input_tokens.saturating_sub(fixed_input_tokens))
            .unwrap_or(u32::MAX),
        history_capacity_tokens: u32::try_from(
            u64::from(context_limit)
                .saturating_sub(fixed_input_tokens)
                .saturating_sub(reserved_output_tokens),
        )
        .unwrap_or(u32::MAX),
    }
}

/// A transient prompt reminder, never a limit or a synthetic tool result. It is
/// derived from the transcript so retries/restores do not need another counter.
fn add_exploration_reminder(request: &mut TurnRequest) {
    let Some(prompt) = request.system_prompt.as_mut() else {
        return;
    };
    if !prompt.contains(crate::ai::provider::request_translator::IMPLEMENTATION_PROGRESS_GUIDANCE) {
        return;
    }
    let batches = consecutive_discovery_batches(&request.messages);
    if batches >= 4 && batches.is_multiple_of(4) {
        prompt.push_str("\nImplementation progress checkpoint: several consecutive tool batches have only gathered context. If the user requested implementation and the next change is understood, make that smallest coherent change now and validate it. Otherwise identify the specific missing fact that prevents that change and resolve only that gap. Do not edit for research-only or review tasks, skip required instructions, or claim progress without evidence. This is guidance, not a tool restriction or a turn limit.\n");
    }
}

fn consecutive_discovery_batches(messages: &[ConversationMessage]) -> usize {
    let mut batches = 0;
    for message in messages.iter().rev() {
        let mut names = Vec::new();
        let has_user_input = match &message.content {
            MessageContent::Text(_) => true,
            MessageContent::ToolUse { name, .. } => {
                names.push(name.as_str());
                false
            }
            MessageContent::ToolResult { .. } => false,
            MessageContent::MultiPart(parts) => {
                let mut input = false;
                for part in parts {
                    match part {
                        ContentPart::ToolUse { name, .. } => names.push(name.as_str()),
                        ContentPart::Text(_) | ContentPart::Image { .. } => input = true,
                        ContentPart::ToolResult { .. } | ContentPart::Reasoning { .. } => {}
                    }
                }
                input
            }
        };
        if message.role == MessageRole::User && has_user_input {
            break;
        }
        if names.is_empty() {
            continue;
        }
        // Unknown tools and shell commands may mutate state: never infer that
        // they are discovery. A mixed batch also ends the discovery streak.
        if names.iter().any(|name| {
            !matches!(
                *name,
                "read_files"
                    | "search_codebase"
                    | "file_glob"
                    | "grep"
                    | "read_documents"
                    | "read_skill"
                    | "recall_tool_history"
            )
        }) {
            break;
        }
        batches += 1;
    }
    batches
}

const ESTIMATED_BYTES_PER_TOKEN: u64 = 3;
const ESTIMATED_REQUEST_OVERHEAD_TOKENS: u64 = 512;
const ESTIMATED_IMAGE_TOKENS: u64 = 1_600;

/// Per-message history estimate from the same bounded projection used by requests.
/// Rounding each message separately is conservative by at most one token per message.
pub(crate) fn estimated_context_message_tokens(message: &ConversationMessage) -> u32 {
    let tokens = (estimate_conversation_message_bytes(message) as u64)
        .div_ceil(ESTIMATED_BYTES_PER_TOKEN)
        .saturating_add(
            count_images_in_content(&message.content).saturating_mul(ESTIMATED_IMAGE_TOKENS),
        );
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

/// Heuristic estimate, not an exact tokenizer count or provider-specific bound.
/// UTF-8 bytes account for non-ASCII content better than character counts; raw
/// image payload bytes are excluded from text accounting and charged separately.
fn estimate_turn_request_tokens(request: &TurnRequest) -> u64 {
    let mut text_bytes = request
        .system_prompt
        .as_deref()
        .map_or(0, |text| text.len());
    text_bytes += request
        .prompt
        .as_ref()
        .map_or(0, estimate_message_content_bytes);
    text_bytes += request
        .messages
        .iter()
        .map(estimate_conversation_message_bytes)
        .sum::<usize>();
    text_bytes += request
        .tools
        .iter()
        .map(|tool| serde_json::to_string(tool).unwrap_or_default().len())
        .sum::<usize>();

    let image_tokens = request
        .prompt
        .as_ref()
        .map_or(0, count_images_in_content)
        .saturating_add(
            request
                .messages
                .iter()
                .map(|message| count_images_in_content(&message.content))
                .sum::<u64>(),
        )
        .saturating_mul(ESTIMATED_IMAGE_TOKENS);
    let input_tokens = (text_bytes as u64).div_ceil(ESTIMATED_BYTES_PER_TOKEN);
    input_tokens
        .saturating_add(image_tokens)
        .saturating_add(ESTIMATED_REQUEST_OVERHEAD_TOKENS)
        .saturating_add(request.max_output_tokens.unwrap_or_default())
}

fn estimate_conversation_message_bytes(message: &ConversationMessage) -> usize {
    let mut sanitized_message = message.clone();
    remove_image_payloads_from_content(&mut sanitized_message.content);
    serde_json::to_string(&sanitized_message)
        .unwrap_or_default()
        .len()
}

fn estimate_message_content_bytes(content: &MessageContent) -> usize {
    let mut sanitized_content = content.clone();
    remove_image_payloads_from_content(&mut sanitized_content);
    serde_json::to_string(&sanitized_content)
        .unwrap_or_default()
        .len()
}

fn remove_image_payloads_from_content(content: &mut MessageContent) {
    if let MessageContent::MultiPart(parts) = content {
        for part in parts {
            match part {
                ContentPart::Image { data, .. } => data.clear(),
                ContentPart::Text(_)
                | ContentPart::Reasoning { .. }
                | ContentPart::ToolUse { .. }
                | ContentPart::ToolResult { .. } => {}
            }
        }
    }
}

fn count_images_in_content(content: &MessageContent) -> u64 {
    match content {
        MessageContent::Text(_)
        | MessageContent::ToolUse { .. }
        | MessageContent::ToolResult { .. } => 0,
        MessageContent::MultiPart(parts) => parts
            .iter()
            .filter(|part| matches!(part, ContentPart::Image { .. }))
            .count() as u64,
    }
}

fn tool_event_call_id(event: &ToolEvent) -> Result<&str, ProviderRunCoordinatorError> {
    match event {
        ToolEvent::Proposed { call } => Ok(&call.id),
        ToolEvent::PermissionRequested { request } => Ok(&request.call_id),
        ToolEvent::PermissionResolved { call_id, .. } | ToolEvent::Started { call_id } => {
            Ok(call_id)
        }
        ToolEvent::Completed { result } => Ok(&result.call_id),
    }
}

fn protocol_error(message: impl Into<String>) -> AgentError {
    AgentError::new(AgentErrorKind::Protocol, message)
}

#[cfg(test)]
#[path = "provider_run_coordinator_tests.rs"]
mod tests;
