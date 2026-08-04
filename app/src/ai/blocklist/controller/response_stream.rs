#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(target_family = "wasm"))]
use std::sync::Mutex;

#[cfg(not(target_family = "wasm"))]
use ::local_control::remote_command::is_potential_remote_ssh_command;
use anyhow::anyhow;
use chrono::{DateTime, Local, TimeDelta};
use futures::channel::oneshot;
#[cfg(not(target_family = "wasm"))]
use galaxy_core::features::FeatureFlag;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use settings::Setting;
use uuid::Uuid;
use warp_multi_agent_api::response_event;

#[cfg(not(target_family = "wasm"))]
use crate::ai::acp::{
    acp_output_stream, acp_startup_error_stream, galaxy_mcp_server, resolve_acp_launch,
    resolve_acp_permissions, validate_acp_dispatch, validate_acp_launch_identity, AcpRuntimeModel,
    AcpSessionHandleSlot, AcpSessionMetadata, AcpSteeringRequest, GalaxyMcpTarget,
};
use crate::ai::agent::api::{self, ConvertToAPITypeError};
use crate::ai::agent::conversation::AIConversationId;
#[cfg(not(target_family = "wasm"))]
use crate::ai::agent::AIAgentInput;
use crate::ai::agent::{AIIdentifiers, CancellationReason};
use crate::ai::bedrock::client::BedrockClientConfig;
#[cfg(not(target_family = "wasm"))]
use crate::ai::blocklist::BlocklistAIPermissions;
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::provider::ProviderConfig;
use crate::ai::runtime::ProviderRuntime;
use crate::network::NetworkStatus;
#[cfg(not(target_family = "wasm"))]
use crate::pane_group::PaneGroup;
use crate::persistence::model::AgentBackend;
use crate::server::server_api::AIApiError;
#[cfg(not(target_family = "wasm"))]
use crate::settings::LocalControlSettings;
use crate::{report_error, send_telemetry_from_ctx, AISettings, BlocklistAIHistoryModel};

/// Maximum number of times a single MAA request is re-sent before the failure is
/// surfaced.
const MAX_RETRIES: usize = 3;

/// What to do about a failed or truncated MAA response attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    /// Re-send the same request immediately.
    RetryNow,
    /// Re-send the same request once connectivity returns.
    RetryWhenOnline,
    /// Resume the conversation with a fresh request after the stream completes.
    Resume,
    /// Surface the error; the conversation ends in error.
    Fail,
}

/// Decides how to recover from a failed response-stream attempt.
///
/// Before any client actions have been received, the request can be re-sent verbatim
/// (immediately, or once connectivity returns). After actions have streamed,
/// re-sending is unsafe, so recovery uses a fresh `ResumeConversation` request.
fn recovery_action(
    has_received_client_actions: bool,
    is_recoverable: bool,
    has_retry_budget: bool,
    can_attempt_resume_on_error: bool,
    is_online: bool,
) -> RecoveryAction {
    if !has_received_client_actions && is_recoverable && has_retry_budget {
        if is_online {
            RecoveryAction::RetryNow
        } else {
            RecoveryAction::RetryWhenOnline
        }
    } else if has_received_client_actions && is_recoverable && can_attempt_resume_on_error {
        RecoveryAction::Resume
    } else {
        RecoveryAction::Fail
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResponseStreamId(String);

impl ResponseStreamId {
    pub fn for_shared_session(init_event: &response_event::StreamInit) -> Self {
        // Make the stream ID unique per viewing by appending a local UUID
        // This prevents collisions when replaying the same conversation multiple times
        // (either on close-and-reopen or when viewing the same shared session from multiple terminals)
        Self(format!("{}-{}", init_event.request_id, Uuid::new_v4()))
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

#[cfg(not(target_family = "wasm"))]
struct AcpRequestControl {
    cancellation_rx: oneshot::Receiver<()>,
    session_metadata: Arc<Mutex<AcpSessionMetadata>>,
    session_handle: AcpSessionHandleSlot,
    steering_rx: async_channel::Receiver<AcpSteeringRequest>,
}

/// Model wrapping an agent API response stream.
///
/// Emits events when the output corresponding to the stream is updated, typically after receiving
/// each response chunk.
///
/// Handles retries internally - retries are only attempted if no ClientActions events have been
/// received yet, ensuring we don't retry after the AI has started executing actions.
pub struct ResponseStream {
    id: ResponseStreamId,
    agent_backend: AgentBackend,
    #[cfg(not(target_family = "wasm"))]
    acp_session_metadata: Arc<Mutex<AcpSessionMetadata>>,
    #[cfg(not(target_family = "wasm"))]
    acp_session_handle: AcpSessionHandleSlot,
    #[cfg(not(target_family = "wasm"))]
    acp_steering_tx: async_channel::Sender<AcpSteeringRequest>,
    params: api::RequestParams,
    retry_count: usize,
    /// One-time fallback from the profile's thinking model to its coding model.
    coding_model_fallback_attempted: bool,
    start_time: DateTime<Local>,
    time_to_latest_event: TimeDelta,
    cancellation_tx: Option<oneshot::Sender<()>>,
    /// Store the original error for telemetry when retries succeed
    original_error: Option<String>,
    /// Track whether we've received any client actions
    /// If true, we cannot retry on subsequent errors since actions may have been executed
    has_received_client_actions: bool,
    /// AI identifiers for telemetry emission
    ai_identifiers: AIIdentifiers,

    /// Whether this request can attempt to resume the conversation on error.
    /// This is true for all requests except those that are themselves the result of a resume
    /// triggered by a previous error.
    can_attempt_resume_on_error: bool,

    /// Whether we should attempt to resume the conversation after the stream finishes.
    ///
    /// This is set when a transient network/server failure occurs after client actions
    /// have been received (so an in-request retry is unsafe) and
    /// `can_attempt_resume_on_error` is true.
    should_resume_conversation_after_stream_finished: bool,

    /// Whether a `StreamFinished` event was received for the current request. A
    /// stream that completes without one was truncated in transit.
    stream_finished_received: bool,

    /// Whether a terminal error event has already been emitted for the current
    /// request, so stream completion doesn't synthesize a second failure for it.
    error_event_emitted: bool,

    /// Whether a retry is parked waiting for connectivity. While set, completion of
    /// the failed attempt's underlying stream is ignored.
    deferred_retry_pending: bool,

    /// Unique, internal id for the current request.
    ///
    /// This ensures that the model never emits events for a request that was already cancelled (or
    /// retried) and is still receiving lagging events.
    ///
    /// Note this is unique compared to `id`; this is unique across retry requests while the response
    /// stream id remains stable.
    current_request_id: Option<Uuid>,
}

impl ResponseStream {
    /// Emits a synthetic successful response event through the normal controller subscription.
    #[cfg(test)]
    pub fn emit_response_event_for_test(
        &mut self,
        event: warp_multi_agent_api::ResponseEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(Ok(
            event,
        ))));
    }
    #[cfg(test)]
    pub fn new_for_test(id: ResponseStreamId) -> Self {
        let (cancellation_tx, _rx) = oneshot::channel();
        Self {
            id,
            agent_backend: AgentBackend::Provider,
            #[cfg(not(target_family = "wasm"))]
            acp_session_metadata: Arc::new(Mutex::new(AcpSessionMetadata::default())),
            #[cfg(not(target_family = "wasm"))]
            acp_session_handle: Arc::new(Mutex::new(None)),
            #[cfg(not(target_family = "wasm"))]
            acp_steering_tx: async_channel::unbounded().0,
            params: api::RequestParams::new_for_test(),
            retry_count: 0,
            coding_model_fallback_attempted: false,
            start_time: Local::now(),
            time_to_latest_event: TimeDelta::seconds(0),
            cancellation_tx: Some(cancellation_tx),
            original_error: None,
            has_received_client_actions: false,
            ai_identifiers: AIIdentifiers::default(),
            can_attempt_resume_on_error: false,
            should_resume_conversation_after_stream_finished: false,
            stream_finished_received: false,
            error_event_emitted: false,
            deferred_retry_pending: false,
            current_request_id: Some(Uuid::new_v4()),
        }
    }

    fn resolve_provider_config(model_id: &str, ctx: &ModelContext<Self>) -> ProviderConfig {
        let settings = AISettings::as_ref(ctx);

        // Check if this specific model has an OpenAI-compatible routing entry.
        // This allows OpenAI/LiteLLM models to coexist with Bedrock models —
        // only models fetched from the OpenAI endpoint route through it.
        if *settings.openai_enabled.value() {
            let llm_prefs = LLMPreferences::as_ref(ctx);
            if let Some(client_config) = llm_prefs.openai_client_config_for_model(model_id) {
                return ProviderConfig::OpenAI(OpenAIClientConfig {
                    base_url: client_config.base_url.clone(),
                    api_key: client_config.api_key.clone(),
                    model: Some(model_id.to_string()),
                    max_input_tokens: client_config.max_input_tokens,
                    max_output_tokens: client_config.max_output_tokens,
                    use_rig: client_config.use_rig,
                    supports_system_messages: client_config.supports_system_messages,
                });
            }
        }

        // Fall back to Bedrock
        if *settings.bedrock_enabled.value() {
            let auth_method = *settings.bedrock_auth_method.value();
            let api_key_manager = ::ai::api_keys::ApiKeyManager::as_ref(ctx);
            let mut config = BedrockClientConfig {
                auth_method,
                profile: settings.bedrock_profile.value().clone(),
                region: settings.bedrock_region.value().clone(),
                access_key_id: settings.bedrock_access_key_id.value().clone(),
                secret_access_key: settings.bedrock_secret_access_key.value().clone(),
                session_token: None,
                cross_region_inference: *settings.bedrock_cross_region_inference.value(),
            };

            if let ::ai::api_keys::AwsCredentialsState::Loaded { credentials, .. } =
                api_key_manager.aws_credentials_state()
            {
                config.auth_method = crate::settings::BedrockAuthMethod::StaticKeys;
                config.access_key_id = credentials.access_key().to_string();
                config.secret_access_key = credentials.secret_key().to_string();
                config.session_token = credentials.session_token().map(|s| s.to_string());
            }

            return ProviderConfig::Bedrock(config.with_external_fallbacks());
        }

        ProviderConfig::None
    }

    #[cfg(not(target_family = "wasm"))]
    fn resolve_acp_manager(
        backend: &crate::persistence::model::AcpConversationData,
        ctx: &mut ModelContext<Self>,
    ) -> Result<galaxy_acp::AcpSessionManager, String> {
        use galaxy_acp::AcpManagerConfig;

        let settings = AISettings::as_ref(ctx);
        let configured_agent_id = settings.acp_agent_id.value().trim();
        let configured_agent_id = if configured_agent_id.is_empty() {
            "codex"
        } else {
            configured_agent_id
        };
        let launch = resolve_acp_launch(
            configured_agent_id,
            settings.acp_agent_command.value(),
            settings.acp_agent_args.value(),
        )?;
        validate_acp_launch_identity(
            backend,
            configured_agent_id,
            settings.acp_agent_command.value(),
            &launch,
        )?;
        let config = AcpManagerConfig::new(launch);
        AcpRuntimeModel::handle(ctx).update(ctx, |runtime, _| runtime.manager(config))
    }

    #[cfg(not(target_family = "wasm"))]
    fn spawn_acp_request(
        backend: crate::persistence::model::AcpConversationData,
        params: api::RequestParams,
        conversation_key: String,
        request_id: Uuid,
        control: AcpRequestControl,
        ctx: &mut ModelContext<Self>,
    ) {
        let AcpRequestControl {
            cancellation_rx,
            session_metadata,
            session_handle,
            steering_rx,
        } = control;
        let profile = BlocklistAIPermissions::as_ref(ctx)
            .active_permissions_profile(ctx, params.terminal_view_id);
        let permissions = resolve_acp_permissions(&profile);
        let targets_remote_terminal = params.session_context.is_remote()
            || params.input.iter().any(|input| {
                let AIAgentInput::UserQuery {
                    running_command: Some(command),
                    ..
                } = input
                else {
                    return false;
                };
                is_interactive_remote_command(&command.command)
            });
        let manager = validate_acp_dispatch(
            FeatureFlag::AgentClientProtocol.is_enabled(),
            *AISettings::as_ref(ctx).acp_enabled.value(),
            targets_remote_terminal,
        )
        .and_then(|()| Self::resolve_acp_manager(&backend, ctx));
        let galaxy_mcp_server = if cfg!(unix)
            && manager.is_ok()
            && permissions.expose_galaxy_tools
            && FeatureFlag::GalaxyControlCli.is_enabled()
            && LocalControlSettings::as_ref(ctx).is_enabled()
        {
            match params
                .terminal_view_id
                .and_then(|terminal_view_id| terminal_pane_id(terminal_view_id, ctx))
            {
                Some(target) => match galaxy_mcp_server(
                    &target,
                    permissions.allow_terminal_execute,
                    permissions.allow_terminal_interrupt,
                ) {
                    Ok(server) => Some(server),
                    Err(error) => {
                        log::warn!("Galaxy MCP tools are unavailable for ACP: {error}");
                        None
                    }
                },
                None => {
                    log::warn!(
                        "Galaxy MCP tools are unavailable for ACP because the originating terminal pane could not be resolved"
                    );
                    None
                }
            }
        } else {
            None
        };
        let galaxy_terminal_interrupt_available =
            galaxy_mcp_server.is_some() && permissions.allow_terminal_interrupt;
        let _ = ctx.spawn(
            async move {
                let stream = match manager {
                    Ok(manager) => {
                        acp_output_stream(
                            manager,
                            params,
                            conversation_key,
                            backend,
                            galaxy_mcp_server,
                            galaxy_terminal_interrupt_available,
                            permissions.policy,
                            permissions.auto_approve_protocol_requests,
                            session_metadata,
                            session_handle,
                            steering_rx,
                            cancellation_rx,
                        )
                        .await
                    }
                    Err(message) => acp_startup_error_stream(&params, &backend, &message),
                };
                Ok::<_, ConvertToAPITypeError>(stream)
            },
            move |me, stream, ctx| {
                me.handle_response_stream_result(request_id, stream, ctx);
            },
        );
    }

    fn spawn_provider_request(
        params: api::RequestParams,
        provider_config: ProviderConfig,
        request_id: Uuid,
        cancellation_rx: oneshot::Receiver<()>,
        ctx: &mut ModelContext<Self>,
    ) {
        let _ = ctx.spawn(
            async move {
                ProviderRuntime::new(provider_config)
                    .start_turn(params, cancellation_rx)
                    .await
            },
            move |me, stream, ctx| {
                me.handle_response_stream_result(request_id, stream, ctx);
            },
        );
    }

    pub fn new(
        params: api::RequestParams,
        ai_identifiers: AIIdentifiers,
        agent_backend: AgentBackend,
        can_attempt_resume_on_error: bool,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let (cancellation_tx, cancellation_rx) = oneshot::channel();
        let start_time = Local::now();

        let request_id = Uuid::new_v4();
        #[cfg(not(target_family = "wasm"))]
        let acp_session_metadata = Arc::new(Mutex::new(AcpSessionMetadata::default()));
        #[cfg(not(target_family = "wasm"))]
        let acp_session_handle = Arc::new(Mutex::new(None));
        #[cfg(not(target_family = "wasm"))]
        let (acp_steering_tx, acp_steering_rx) = async_channel::unbounded();
        match &agent_backend {
            AgentBackend::Provider => {
                let provider_config = Self::resolve_provider_config(params.model.as_str(), ctx);
                Self::spawn_provider_request(
                    params.clone(),
                    provider_config,
                    request_id,
                    cancellation_rx,
                    ctx,
                );
            }
            AgentBackend::Acp(backend) => {
                #[cfg(not(target_family = "wasm"))]
                Self::spawn_acp_request(
                    backend.clone(),
                    params.clone(),
                    ai_identifiers
                        .client_conversation_id
                        .map(|id| format!("{id:?}"))
                        .unwrap_or_else(|| Uuid::new_v4().to_string()),
                    request_id,
                    AcpRequestControl {
                        cancellation_rx,
                        session_metadata: acp_session_metadata.clone(),
                        session_handle: acp_session_handle.clone(),
                        steering_rx: acp_steering_rx,
                    },
                    ctx,
                );
                #[cfg(target_family = "wasm")]
                {
                    let error = Arc::new(AIApiError::Stream {
                        stream_type: "acp",
                        source: anyhow!("ACP is unavailable in the web client"),
                    });
                    let stream = Box::pin(futures::stream::once(async move { Err(error) }));
                    let _ = ctx.spawn(
                        async move { Ok::<_, ConvertToAPITypeError>(stream) },
                        move |me, stream, ctx| {
                            me.handle_response_stream_result(request_id, stream, ctx);
                        },
                    );
                }
            }
        }
        Self {
            id: ResponseStreamId(Uuid::new_v4().to_string()),
            agent_backend,
            #[cfg(not(target_family = "wasm"))]
            acp_session_metadata,
            #[cfg(not(target_family = "wasm"))]
            acp_session_handle,
            #[cfg(not(target_family = "wasm"))]
            acp_steering_tx,
            params: params.clone(),
            start_time,
            time_to_latest_event: TimeDelta::seconds(0),
            cancellation_tx: Some(cancellation_tx),
            retry_count: 0,
            coding_model_fallback_attempted: false,
            original_error: None,
            has_received_client_actions: false,
            ai_identifiers,
            can_attempt_resume_on_error,
            should_resume_conversation_after_stream_finished: false,
            stream_finished_received: false,
            error_event_emitted: false,
            deferred_retry_pending: false,
            current_request_id: Some(request_id),
        }
    }

    pub fn id(&self) -> &ResponseStreamId {
        &self.id
    }

    pub fn is_acp(&self) -> bool {
        matches!(self.agent_backend, AgentBackend::Acp(_))
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn acp_session_metadata(&self) -> Option<AcpSessionMetadata> {
        self.is_acp()
            .then(|| {
                self.acp_session_metadata
                    .lock()
                    .ok()
                    .map(|state| state.clone())
            })
            .flatten()
    }

    pub(super) fn try_steer_acp(&self, display_text: String) -> bool {
        #[cfg(not(target_family = "wasm"))]
        {
            if !self.is_acp()
                || self.current_request_id.is_none()
                || !self
                    .acp_session_metadata()
                    .is_some_and(|metadata| metadata.can_steer)
                || !self
                    .acp_session_handle
                    .lock()
                    .is_ok_and(|session| session.is_some())
            {
                return false;
            }
            let mut model_text = display_text.clone();
            self.params.redact_text_for_model(&mut model_text);
            self.acp_steering_tx
                .try_send(AcpSteeringRequest::text(display_text, model_text))
                .is_ok()
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = display_text;
            false
        }
    }

    pub fn bedrock_messages_sent(
        &self,
    ) -> &std::sync::Arc<std::sync::Mutex<Vec<crate::ai::bedrock::convert::ConversationMessage>>>
    {
        &self.params.bedrock_messages_sent
    }

    /// Returns the model ID associated with this response stream's request.
    pub fn model_id(&self) -> &str {
        self.params.model.as_str()
    }

    pub(super) fn llm_id(&self) -> &LLMId {
        &self.params.model
    }

    /// Returns true if we should attempt to resume the conversation after the stream finishes.
    pub fn should_resume_conversation_after_stream_finished(&self) -> bool {
        self.should_resume_conversation_after_stream_finished
    }

    /// Helper function to emit AgentModeError telemetry for error that is retryable (not user visible).
    fn emit_retryable_agent_mode_error_telemetry(
        &self,
        error: String,
        ctx: &mut ModelContext<Self>,
    ) {
        send_telemetry_from_ctx!(
            crate::TelemetryEvent::AgentModeError {
                identifiers: self.ai_identifiers.clone(),
                error,
                is_user_visible: false,
                will_attempt_to_resume: false,
            },
            ctx
        );
    }

    fn retry(&mut self, ctx: &mut ModelContext<Self>) {
        self.retry_count += 1;
        // Reset per-attempt state for the new attempt.
        self.has_received_client_actions = false;
        self.stream_finished_received = false;
        self.error_event_emitted = false;
        self.deferred_retry_pending = false;

        let (cancellation_tx, cancellation_rx) = oneshot::channel();
        if let Some(old_cancellation_tx) = self.cancellation_tx.take() {
            let _ = old_cancellation_tx.send(());
        }
        self.cancellation_tx = Some(cancellation_tx);

        let request_id = Uuid::new_v4();
        self.current_request_id = Some(request_id);
        let params = self.params.clone();
        let provider_config = Self::resolve_provider_config(params.model.as_str(), ctx);
        let _ = ctx.spawn(
            async move {
                ProviderRuntime::new(provider_config)
                    .start_turn(params, cancellation_rx)
                    .await
            },
            move |me, stream, ctx| {
                me.handle_response_stream_result(request_id, stream, ctx);
            },
        );
    }

    fn should_fallback_to_coding_model(
        &self,
        error: &Arc<crate::server::server_api::AIApiError>,
    ) -> bool {
        if self.is_acp() || self.coding_model_fallback_attempted || self.has_received_client_actions
        {
            return false;
        }
        let coding_model = self.params.coding_model.as_str();
        !coding_model.is_empty()
            && coding_model != self.params.model.as_str()
            && matches!(
                error.as_ref(),
                crate::server::server_api::AIApiError::QuotaLimit { .. }
            )
    }

    fn retry_with_coding_model(&mut self, ctx: &mut ModelContext<Self>) {
        self.coding_model_fallback_attempted = true;
        self.params.model = self.params.coding_model.clone();
        self.retry(ctx);
    }

    /// Cancels the stream. The conversation_id is preserved in the emitted event for async handling.
    pub(super) fn cancel(
        &mut self,
        reason: CancellationReason,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.current_request_id = None;
        let Some(cancellation_tx) = self.cancellation_tx.take() else {
            return;
        };
        let _ = cancellation_tx.send(());
        ctx.emit(ResponseStreamEvent::AfterStreamFinished {
            cancellation: Some(StreamCancellation {
                reason,
                conversation_id,
            }),
        });
    }

    fn handle_response_stream_result(
        &mut self,
        request_id: Uuid,
        stream_result: Result<api::ResponseStream, ConvertToAPITypeError>,
        ctx: &mut ModelContext<Self>,
    ) {
        match stream_result {
            Ok(stream) => {
                ctx.spawn_stream_local(
                    stream,
                    move |me, event, ctx| {
                        me.handle_response_stream_event(request_id, event, ctx);
                    },
                    move |me, ctx| {
                        me.on_response_stream_complete(request_id, ctx);
                    },
                );
            }
            Err(e) => {
                log::error!("Failed to send request to multi-agent API: {e:?}");
                if self.current_request_id.is_none_or(|id| id != request_id) {
                    return;
                }
                // A request-conversion failure is a deterministic client-side error and
                // no stream was ever created: retrying would fail identically, and
                // letting completion synthesize `UnexpectedEof` would misreport it as
                // a transient network failure. Surface the original error and finish
                // terminally. (HTTP send failures don't take this path — they arrive as
                // in-stream error events.)
                let error = Arc::new(AIApiError::Other(anyhow!(e)));
                self.error_event_emitted = true;
                self.report_request_failure(&error, NetworkStatus::as_ref(ctx).is_online());
                ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(Err(
                    error,
                ))));
                self.on_response_stream_complete(request_id, ctx);
            }
        }
    }

    fn handle_response_stream_event(
        &mut self,
        request_id: Uuid,
        event: api::Event,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.current_request_id.is_none_or(|id| id != request_id) {
            log::info!(
                "[bedrock-debug] handle_response_stream_event: stale request_id, dropping event"
            );
            return;
        }
        self.time_to_latest_event = Local::now().signed_duration_since(self.start_time);

        match &event {
            Ok(response_event) => {
                let event_type_name = match &response_event.r#type {
                    Some(warp_multi_agent_api::response_event::Type::Init(_)) => "Init",
                    Some(warp_multi_agent_api::response_event::Type::ClientActions(a)) => {
                        log::debug!(
                            "[bedrock] ResponseStream received ClientActions with {} actions",
                            a.actions.len()
                        );
                        "ClientActions"
                    }
                    Some(warp_multi_agent_api::response_event::Type::Finished(f)) => {
                        log::info!(
                            "[bedrock-debug] ResponseStream received Finished (reason={:?})",
                            f.reason
                                .as_ref()
                                .map(|r| format!("{r:?}"))
                                .unwrap_or("None".into())
                        );
                        "Finished"
                    }
                    None => "None",
                };
                log::info!("[bedrock-debug] ResponseStream emitting event type={event_type_name}");
                if let Some(event_type) = &response_event.r#type {
                    match event_type {
                        warp_multi_agent_api::response_event::Type::Init(init_event) => {
                            // Capture server_output_id from StreamInit event
                            self.ai_identifiers.server_output_id =
                                Some(crate::ai::agent::ServerOutputId::new(
                                    init_event.request_id.clone(),
                                ));
                        }
                        warp_multi_agent_api::response_event::Type::ClientActions(_) => {
                            // Mark that we've received client actions
                            self.has_received_client_actions = true;
                        }
                        warp_multi_agent_api::response_event::Type::Finished(finished_event) => {
                            self.stream_finished_received = true;
                            // Emit retry success telemetry on successful completion
                            if matches!(
                                finished_event.reason,
                                Some(warp_multi_agent_api::response_event::stream_finished::Reason::Done(_)) | None
                            ) {
                                // Emit retry success telemetry if this was a successful completion after retries
                                if self.retry_count > 0 {
                                    if let Some(original_error) = &self.original_error {
                                        send_telemetry_from_ctx!(
                                            crate::TelemetryEvent::AgentModeRequestRetrySucceeded {
                                                identifiers: self.ai_identifiers.clone(),
                                                retry_count: self.retry_count,
                                                original_error: original_error.clone(),
                                            },
                                            ctx
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(event)));
            }
            Err(e) => {
                log::info!("[bedrock-debug] ResponseStream received ERROR: {e:?}");
                // Store original error if this is the first error
                if self.retry_count == 0 {
                    self.original_error = Some(format!("{e:?}"));
                }

                if self.should_fallback_to_coding_model(e) {
                    log::warn!(
                        "Thinking model rate-limited; retrying with the profile coding model"
                    );
                    self.retry_with_coding_model(ctx);
                    return;
                }

                let is_online = NetworkStatus::as_ref(ctx).is_online();
                match recovery_action(
                    self.has_received_client_actions,
                    e.is_recoverable() && !self.is_acp(),
                    self.retry_count < MAX_RETRIES,
                    self.can_attempt_resume_on_error,
                    is_online,
                ) {
                    RecoveryAction::RetryNow => {
                        log::warn!(
                            "MultiAgent request failed, retrying (attempt {}/{}) - Error: {e:?}",
                            self.retry_count + 1,
                            MAX_RETRIES
                        );
                        // Only emit error telemetry here if we're retrying.
                        // Final errors that aren't being retried are emitted elsewhere.
                        self.emit_retryable_agent_mode_error_telemetry(format!("{e:?}"), ctx);
                        self.retry(ctx);
                        // Don't emit the error event, we're retrying
                        return;
                    }
                    RecoveryAction::RetryWhenOnline => {
                        log::warn!(
                            "MultiAgent request failed while offline; retrying (attempt {}/{}) once connectivity returns - Error: {e:?}",
                            self.retry_count + 1,
                            MAX_RETRIES
                        );
                        self.emit_retryable_agent_mode_error_telemetry(format!("{e:?}"), ctx);
                        self.defer_retry_until_online(ctx);
                        return;
                    }
                    RecoveryAction::Resume => {
                        // Recoverable failure after client actions: we'll resume the
                        // conversation once the stream finishes rather than surface the
                        // error, so the UI suppresses the banner. Log it so the
                        // auto-recovery isn't completely silent.
                        log::warn!(
                            "MultiAgent request failed after client actions; resuming conversation after stream finishes - Error: {e:?}"
                        );
                        // The resume spawn itself waits for connectivity.
                        self.should_resume_conversation_after_stream_finished = true;
                    }
                    RecoveryAction::Fail => {}
                }
                self.error_event_emitted = true;

                self.report_request_failure(e, is_online);

                ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(event)));
            }
        }
    }

    fn on_response_stream_complete(&mut self, request_id: Uuid, ctx: &mut ModelContext<Self>) {
        log::info!("[bedrock-debug] on_response_stream_complete called (request_id={request_id})");
        if self.current_request_id.is_none_or(|id| id != request_id) {
            log::info!("[bedrock-debug] on_response_stream_complete: stale request_id, ignoring");
            return;
        }
        // A retry is parked waiting for connectivity; the request is logically still
        // active, so don't complete the stream for the failed attempt.
        if self.deferred_retry_pending {
            return;
        }

        // The server always sends a StreamFinished event before ending the response,
        // but a transport cut between chunks surfaces as a clean EOF. Synthesize the
        // failure and recover like any transient error.
        if !self.stream_finished_received && !self.error_event_emitted {
            log::warn!(
                "generate_multi_agent_output stream ended without emitting StreamFinished event."
            );
            let unexpected_eof = Arc::new(AIApiError::UnexpectedEof);
            let is_online = NetworkStatus::as_ref(ctx).is_online();
            match recovery_action(
                self.has_received_client_actions,
                unexpected_eof.is_recoverable() && !self.is_acp(),
                self.retry_count < MAX_RETRIES,
                self.can_attempt_resume_on_error,
                is_online,
            ) {
                RecoveryAction::RetryNow => {
                    log::warn!(
                        "MultiAgent request failed, retrying (attempt {}/{}) - Error: {unexpected_eof:?}",
                        self.retry_count + 1,
                        MAX_RETRIES
                    );
                    self.emit_retryable_agent_mode_error_telemetry(
                        format!("{unexpected_eof:?}"),
                        ctx,
                    );
                    self.retry(ctx);
                    return;
                }
                RecoveryAction::RetryWhenOnline => {
                    log::warn!(
                        "MultiAgent request failed while offline; retrying (attempt {}/{}) once connectivity returns - Error: {unexpected_eof:?}",
                        self.retry_count + 1,
                        MAX_RETRIES
                    );
                    self.emit_retryable_agent_mode_error_telemetry(
                        format!("{unexpected_eof:?}"),
                        ctx,
                    );
                    self.defer_retry_until_online(ctx);
                    return;
                }
                RecoveryAction::Resume => {
                    // Recoverable truncation after client actions: we'll resume the
                    // conversation once the stream finishes rather than surface the
                    // error, so the UI suppresses the banner. Log it so the
                    // auto-recovery isn't completely silent.
                    log::warn!(
                        "MultiAgent request truncated after client actions; resuming conversation after stream finishes - Error: {unexpected_eof:?}"
                    );
                    self.should_resume_conversation_after_stream_finished = true;
                    self.error_event_emitted = true;
                    self.report_request_failure(&unexpected_eof, is_online);
                    ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(Err(
                        unexpected_eof,
                    ))));
                }
                RecoveryAction::Fail => {
                    self.error_event_emitted = true;
                    self.report_request_failure(&unexpected_eof, is_online);
                    ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(Err(
                        unexpected_eof,
                    ))));
                }
            }
        }

        ctx.emit(ResponseStreamEvent::AfterStreamFinished { cancellation: None });
        self.cancellation_tx = None;
    }

    /// Reports a non-retried request failure to crash reporting with classification
    /// tags.
    #[cfg_attr(not(feature = "crash_reporting"), expect(unused_variables))]
    fn report_request_failure(&self, error: &Arc<AIApiError>, is_online: bool) {
        #[cfg(feature = "crash_reporting")]
        sentry::with_scope(
            |scope| {
                scope.set_tag(
                    "has_received_client_actions",
                    self.has_received_client_actions,
                );
                scope.set_tag("error", format!("{error:?}"));
                scope.set_tag("is_recoverable", error.is_recoverable());
                scope.set_tag(
                    "will_attempt_resume",
                    self.should_resume_conversation_after_stream_finished,
                );
                scope.set_tag("is_online", is_online);
                scope.set_tag("retry_count", self.retry_count);
            },
            || {
                report_error!(anyhow!(error.clone()).context(format!(
                    "MultiAgent request failed after {} retries",
                    self.retry_count
                )));
            },
        );
        #[cfg(not(feature = "crash_reporting"))]
        {
            report_error!(anyhow!(error.clone()).context(format!(
                "MultiAgent request failed after {} retries",
                self.retry_count
            )));
        }
    }

    /// Parks a retry until connectivity returns; cancellation invalidates the parked
    /// retry through `current_request_id`.
    fn defer_retry_until_online(&mut self, ctx: &mut ModelContext<Self>) {
        self.deferred_retry_pending = true;
        ctx.emit(ResponseStreamEvent::WaitingForNetwork { waiting: true });
        let request_id_at_defer = self.current_request_id;
        let wait_for_online = NetworkStatus::as_ref(ctx).wait_until_online();
        let _ = ctx.spawn(wait_for_online, move |me, _, ctx| {
            // Cancelled or superseded while waiting — drop the parked retry.
            if request_id_at_defer.is_none() || me.current_request_id != request_id_at_defer {
                return;
            }
            ctx.emit(ResponseStreamEvent::WaitingForNetwork { waiting: false });
            me.retry(ctx);
        });
    }
}

#[cfg(not(target_family = "wasm"))]
fn is_interactive_remote_command(command: &str) -> bool {
    is_potential_remote_ssh_command(command)
}

#[cfg(not(target_family = "wasm"))]
fn terminal_pane_id(
    terminal_view_id: galaxyui::EntityId,
    ctx: &ModelContext<ResponseStream>,
) -> Option<GalaxyMcpTarget> {
    let window_ids = ctx.window_ids().collect::<Vec<_>>();
    for window_id in window_ids {
        let Some(pane_groups) = ctx.views_of_type::<PaneGroup>(window_id) else {
            continue;
        };
        for pane_group in pane_groups {
            let tab_id = pane_group.id().to_string();
            if let Some(pane_id) = pane_group
                .as_ref(ctx)
                .find_pane_id_for_terminal_view(terminal_view_id, ctx)
            {
                return Some(GalaxyMcpTarget {
                    window_id: window_id.to_string(),
                    tab_id,
                    pane_id: pane_id.to_string(),
                });
            }
        }
    }
    None
}

#[derive(Debug)]
pub struct Consumable<T> {
    value: Rc<RefCell<Option<T>>>,
}

impl<T> Consumable<T> {
    fn new(value: T) -> Self {
        Consumable {
            value: Rc::new(RefCell::new(Some(value))),
        }
    }

    pub(super) fn consume(&self) -> Option<T> {
        self.value.borrow_mut().take()
    }
}

impl<T> Clone for Consumable<T> {
    fn clone(&self) -> Self {
        Consumable {
            value: Rc::clone(&self.value),
        }
    }
}

/// Cancellation context preserved for async event handling.
/// Includes conversation_id because truncation can remove exchange mappings before the event is processed.
#[derive(Debug, Clone)]
pub struct StreamCancellation {
    pub reason: CancellationReason,
    pub conversation_id: AIConversationId,
}

#[derive(Debug, Clone)]
pub enum ResponseStreamEvent {
    ReceivedEvent(Consumable<api::Event>),
    /// A retry is parked until connectivity returns (`waiting: true`) or has just
    /// fired (`waiting: false`). The controller mirrors this on the conversation
    /// status (`TransientError` ↔ `InProgress`).
    ///
    /// Only emitted from `defer_retry_until_online`, i.e. always after a recoverable
    /// request failure while offline — never speculatively before an attempt. Consumers
    /// can therefore treat `waiting: true` as a transient-error (reconnecting) state.
    WaitingForNetwork {
        waiting: bool,
    },
    AfterStreamFinished {
        /// Some for cancellation (with context), None for natural completion (uses dynamic lookup).
        cancellation: Option<StreamCancellation>,
    },
}

impl Entity for ResponseStream {
    type Event = ResponseStreamEvent;
}

#[cfg(test)]
#[path = "response_stream_tests.rs"]
mod tests;
