use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{FutureExt as _, StreamExt};
use galaxy_agent_core::{
    turn_control, AgentError, AgentErrorKind, AgentEvent, AgentRuntime, CompletedModelTurn,
    ContentPart, ExternalWorkId, ModelFailureDisposition, PendingToolBatch, ProviderModelCall,
    ProviderRequestProfile, ProviderRun, ProviderRunFailureKind, ProviderRunId, ProviderRunLimits,
    ProviderRunOutcome, ProviderRunPhase, ProviderRunProtocolError, ProviderRunState,
    ProviderRunStep, RunEpoch, RuntimeKind, StopReason, ToolEvent, TurnCommand, TurnControl,
    TurnRequest, Usage,
};
use instant::Instant;
use warpui::r#async::Timer;

use crate::ai::agent::conversation::AIConversationId;
#[cfg(not(target_family = "wasm"))]
use crate::ai::model_output_logging;

pub(crate) const BASE_PROVIDER_PROFILE: &str = "base";
pub(crate) const CLI_MONITOR_PROVIDER_PROFILE: &str = "cli-monitor";
const PROVIDER_MODEL_START_TIMEOUT: Duration = Duration::from_secs(120);
const PROVIDER_MODEL_EVENT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const PROVIDER_MODEL_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(5),
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderRunProjection {
    ModelTurnRequested {
        work_id: ExternalWorkId,
        profile: ProviderRequestProfile,
        runtime_id: String,
        model_id: String,
        retry_attempt: u32,
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
}

impl ProviderRunProfile {
    pub(crate) fn new(runtime: Arc<dyn AgentRuntime>, request: TurnRequest) -> Self {
        Self { runtime, request }
    }
}

pub(crate) struct ProviderRunCoordinator {
    run: ProviderRun,
    profiles: BTreeMap<String, ProviderRunProfile>,
    model_start_timeout: Duration,
    model_event_idle_timeout: Duration,
}

impl ProviderRunCoordinator {
    pub(crate) fn from_request(
        run_id: impl Into<galaxy_agent_core::ProviderRunId>,
        runtime: Arc<dyn AgentRuntime>,
        request: TurnRequest,
        tool_result_archive: Vec<galaxy_agent_core::ConversationMessage>,
        limits: ProviderRunLimits,
    ) -> Result<Self, ProviderRunCoordinatorError> {
        let mut run = ProviderRun::new(
            run_id,
            request.messages.clone(),
            BASE_PROVIDER_PROFILE,
            limits,
        );
        run.replace_tool_result_archive(tool_result_archive);
        let mut profiles = BTreeMap::new();
        profiles.insert(
            BASE_PROVIDER_PROFILE.to_string(),
            ProviderRunProfile::new(runtime, request),
        );
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
        })
    }

    #[cfg(test)]
    fn set_model_timeouts(&mut self, start: Duration, event_idle: Duration) {
        self.model_start_timeout = start;
        self.model_event_idle_timeout = event_idle;
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

    pub(crate) fn insert_profile(
        &mut self,
        profile: impl Into<String>,
        runtime: Arc<dyn AgentRuntime>,
        request: TurnRequest,
    ) -> Result<(), ProviderRunCoordinatorError> {
        let profile = profile.into();
        validate_profile_runtime(&profile, runtime.as_ref())?;
        self.profiles
            .insert(profile, ProviderRunProfile::new(runtime, request));
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
                    self.drive_model_call_acknowledged(call, control.clone(), &mut project)
                        .await?;
                }
                Some(ProviderRunStep::DispatchTools(batch)) => {
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
                        continue;
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
                },
                project,
            )
            .await?
        {
            return Ok(());
        }
        let request = request_for_model_call(profile.request.clone(), &call);
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
                    if reason == StopReason::Cancelled {
                        self.run.cancel("provider model call was cancelled")?;
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
        let error_message = error.message.clone();
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

#[derive(Default)]
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

fn request_for_model_call(mut template: TurnRequest, call: &ProviderModelCall) -> TurnRequest {
    template.prompt = None;
    template.messages = call.messages.clone();
    template
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
