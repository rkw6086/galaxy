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
use galaxy_agent_core::RuntimeCapabilities;
#[cfg(not(target_family = "wasm"))]
use galaxy_agent_core::TurnCommand;
#[cfg(not(target_family = "wasm"))]
use galaxy_core::features::FeatureFlag;
use galaxyui::{AppContext, Entity, ModelContext, SingletonEntity};
use settings::Setting;
use uuid::Uuid;
use warp_multi_agent_api::response_event;

#[cfg(not(target_family = "wasm"))]
use crate::ai::acp::{
    acp_output_stream, acp_startup_error_stream, galaxy_mcp_server, resolve_acp_launch,
    resolve_acp_permissions, validate_acp_dispatch, validate_acp_launch_identity, AcpRuntimeModel,
    AcpSessionMetadata, AcpTurnControlSlot, GalaxyMcpTarget,
};
use crate::ai::agent::api::{self, ConvertToAPITypeError};
use crate::ai::agent::conversation::AIConversationId;
#[cfg(not(target_family = "wasm"))]
use crate::ai::agent::AIAgentInput;
use crate::ai::agent::{AIIdentifiers, CancellationReason};
#[cfg(not(target_family = "wasm"))]
use crate::ai::blocklist::BlocklistAIPermissions;
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::provider::client::BedrockClientConfig;
use crate::ai::provider::ProviderConfig;
#[cfg(not(target_family = "wasm"))]
use crate::ai::remote_logging::{self, RemoteLogLevel, RemoteLogRecord};
#[cfg(not(target_family = "wasm"))]
use crate::pane_group::PaneGroup;
use crate::persistence::model::AgentBackend;
use crate::server::server_api::AIApiError;
#[cfg(not(target_family = "wasm"))]
use crate::settings::LocalControlSettings;
use crate::{report_error, AISettings, BlocklistAIHistoryModel};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResponseStreamId(String);

impl ResponseStreamId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_persisted(value: String) -> Self {
        Self(value)
    }

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
    turn_control: AcpTurnControlSlot,
}

/// Projects an ACP response stream or durable provider-run output into conversation history.
///
/// Direct-provider lifecycle and retry ownership lives in `ProviderRun`; this model owns only ACP
/// transport plus the common UI/history projection boundary.
pub struct ResponseStream {
    id: ResponseStreamId,
    runtime_capabilities: RuntimeCapabilities,
    #[cfg(not(target_family = "wasm"))]
    acp_session_metadata: Arc<Mutex<AcpSessionMetadata>>,
    #[cfg(not(target_family = "wasm"))]
    acp_turn_control: AcpTurnControlSlot,
    params: Option<api::RequestParams>,
    projection_model: LLMId,
    projection_messages_sent:
        Arc<std::sync::Mutex<Vec<crate::ai::provider::types::ConversationMessage>>>,
    start_time: DateTime<Local>,
    time_to_latest_event: TimeDelta,
    cancellation_tx: Option<oneshot::Sender<()>>,
    /// Whether the ACP stream emitted client actions, retained for failure diagnostics.
    has_received_client_actions: bool,
    /// AI identifiers for telemetry emission
    ai_identifiers: AIIdentifiers,
    #[cfg(not(target_family = "wasm"))]
    remote_log_backend: String,
    #[cfg(not(target_family = "wasm"))]
    remote_log_provider: String,

    /// Whether a `StreamFinished` event was received for the current request. A
    /// stream that completes without one was truncated in transit.
    stream_finished_received: bool,

    /// Whether a terminal error event has already been emitted for the current
    /// request, so stream completion doesn't synthesize a second failure for it.
    error_event_emitted: bool,

    /// Unique internal ID for the active ACP request. Clearing it on cancellation causes late
    /// transport events to be discarded while the stable response-stream ID remains a projection ID.
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
            api::StreamEvent::Response(event),
        ))));
    }
    #[cfg(test)]
    pub fn new_for_test(id: ResponseStreamId) -> Self {
        let (cancellation_tx, _rx) = oneshot::channel();
        Self {
            id,
            runtime_capabilities: RuntimeCapabilities::provider(),
            #[cfg(not(target_family = "wasm"))]
            acp_session_metadata: Arc::new(Mutex::new(AcpSessionMetadata::default())),
            #[cfg(not(target_family = "wasm"))]
            acp_turn_control: Arc::new(Mutex::new(None)),
            params: Some(api::RequestParams::new_for_test()),
            projection_model: LLMId::from("test-model"),
            projection_messages_sent: Arc::new(std::sync::Mutex::new(Vec::new())),
            start_time: Local::now(),
            time_to_latest_event: TimeDelta::seconds(0),
            cancellation_tx: Some(cancellation_tx),
            has_received_client_actions: false,
            ai_identifiers: AIIdentifiers::default(),
            #[cfg(not(target_family = "wasm"))]
            remote_log_backend: "provider".to_string(),
            #[cfg(not(target_family = "wasm"))]
            remote_log_provider: "test".to_string(),
            stream_finished_received: false,
            error_event_emitted: false,
            current_request_id: Some(Uuid::new_v4()),
        }
    }

    pub(super) fn resolve_provider_config(model_id: &str, ctx: &AppContext) -> ProviderConfig {
        let settings = AISettings::as_ref(ctx);

        // Check if this specific model has an OpenAI-compatible routing entry.
        // This allows OpenAI/LiteLLM models to coexist with Bedrock models —
        // only models fetched from the OpenAI endpoint route through it.
        let llm_prefs = LLMPreferences::as_ref(ctx);
        if let Some(client_config) = llm_prefs.openai_client_config_for_model(model_id) {
            return ProviderConfig::OpenAI(OpenAIClientConfig {
                kind: client_config.kind,
                base_url: client_config.base_url.clone(),
                api_key: client_config.api_key.clone(),
                project_id: client_config.project_id.clone(),
                location: client_config.location.clone(),
                model: client_config
                    .model
                    .clone()
                    .or_else(|| Some(model_id.to_string())),
                reasoning_effort: client_config.reasoning_effort.clone(),
                max_input_tokens: client_config.max_input_tokens,
                max_output_tokens: client_config.max_output_tokens,
                supports_system_messages: client_config.supports_system_messages,
            });
        }

        // Fall back to Bedrock
        if *settings.bedrock_enabled.value() {
            let auth_method = *settings.bedrock_auth_method.value();
            let region = settings.bedrock_region.value().clone();
            let cross_region_inference = *settings.bedrock_cross_region_inference.value();
            let api_key_manager = ::ai::api_keys::ApiKeyManager::as_ref(ctx);
            let mut config = BedrockClientConfig {
                auth_method,
                profile: settings.bedrock_profile.value().clone(),
                region,
                access_key_id: settings.bedrock_access_key_id.value().clone(),
                secret_access_key: settings.bedrock_secret_access_key.value().clone(),
                session_token: None,
                cross_region_inference,
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
    fn remote_log_provider_for_config(provider_config: &ProviderConfig) -> String {
        match provider_config {
            ProviderConfig::Bedrock(config) => {
                let region = if config.region.trim().is_empty() {
                    "auto"
                } else {
                    config.region.as_str()
                };
                format!("bedrock:{:?}:region={region}", config.auth_method)
            }
            ProviderConfig::OpenAI(config) => {
                format!("openai:{:?}", config.kind)
            }
            ProviderConfig::None => "none".to_string(),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn common_remote_log_context(
        &self,
        event: &str,
        request_id: Uuid,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut context = serde_json::Map::new();
        context.insert("event".to_string(), serde_json::json!(event));
        context.insert("stream_id".to_string(), serde_json::json!(self.id.0));
        context.insert(
            "request_id".to_string(),
            serde_json::json!(request_id.to_string()),
        );
        context.insert(
            "model_id".to_string(),
            serde_json::json!(self.projection_model.as_str()),
        );
        context.insert(
            "backend".to_string(),
            serde_json::json!(self.remote_log_backend),
        );
        context.insert(
            "provider".to_string(),
            serde_json::json!(self.remote_log_provider),
        );
        context.insert(
            "has_received_client_actions".to_string(),
            serde_json::json!(self.has_received_client_actions),
        );
        context.insert(
            "identifiers".to_string(),
            serde_json::to_value(&self.ai_identifiers).unwrap_or_else(|_| serde_json::json!({})),
        );
        context
    }

    #[cfg(not(target_family = "wasm"))]
    #[allow(clippy::too_many_arguments)]
    fn log_llm_request_started(
        ctx: &mut ModelContext<Self>,
        stream_id: &ResponseStreamId,
        request_id: Uuid,
        params: &api::RequestParams,
        ai_identifiers: &AIIdentifiers,
        backend: &str,
        provider: &str,
    ) {
        remote_logging::log_model_event(
            ctx,
            RemoteLogRecord {
                level: RemoteLogLevel::Info,
                message: "LLM request started".to_string(),
                context: serde_json::json!({
                    "event": "llm_request_started",
                    "stream_id": stream_id.0,
                    "request_id": request_id.to_string(),
                    "model_id": params.model.as_str(),
                    "coding_model_id": params.coding_model.as_str(),
                    "backend": backend,
                    "provider": provider,
                    "input_count": params.input.len(),
                    "tool_result_count": params.tool_results.len(),
                    "task_count": params.tasks.len(),
                    "message_history_count": params.message_history.len(),
                    "has_progressive_summary": params.progressive_summary.is_some(),
                    "memory_enabled": params.is_memory_enabled,
                    "warp_drive_context_enabled": params.warp_drive_context_enabled,
                    "planning_enabled": params.planning_enabled,
                    "web_search_enabled": params.web_search_enabled,
                    "computer_use_enabled": params.computer_use_enabled,
                    "ask_user_question_enabled": params.ask_user_question_enabled,
                    "orchestration_enabled": params.orchestration_enabled,
                    "is_remote_session": params.session_context.is_remote(),
                    "llm_finished": false,
                    "response_stream_terminal": false,
                    "identifiers": serde_json::to_value(ai_identifiers).unwrap_or_else(|_| serde_json::json!({})),
                }),
            },
        );
        if let Some(raw_payload) =
            remote_logging::raw_model_payload_context(ctx, raw_model_request_payload(params))
        {
            remote_logging::log_model_event(
                ctx,
                RemoteLogRecord {
                    level: RemoteLogLevel::Info,
                    message: "Raw model request".to_string(),
                    context: serde_json::json!({
                        "event": "raw_model_request",
                        "stream_id": stream_id.0,
                        "request_id": request_id.to_string(),
                        "model_id": params.model.as_str(),
                        "backend": backend,
                        "provider": provider,
                        "raw_payload": raw_payload,
                    }),
                },
            );
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn log_galaxy_decision(
        &self,
        request_id: Uuid,
        decision: &str,
        details: serde_json::Value,
        ctx: &mut ModelContext<Self>,
    ) {
        let mut context = self.common_remote_log_context("galaxy_decision", request_id);
        context.insert("decision".to_string(), serde_json::json!(decision));
        context.insert("details".to_string(), details);
        remote_logging::log_model_event(
            ctx,
            RemoteLogRecord {
                level: RemoteLogLevel::Info,
                message: format!("Galaxy decision: {decision}"),
                context: serde_json::Value::Object(context),
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn log_llm_response_finished(
        &self,
        request_id: Uuid,
        finished_event: &warp_multi_agent_api::response_event::StreamFinished,
        ctx: &mut ModelContext<Self>,
    ) {
        let mut context = self.common_remote_log_context("llm_response_finished", request_id);
        context.insert(
            "reason".to_string(),
            serde_json::json!(stream_finished_reason_name(&finished_event.reason)),
        );
        context.insert(
            "llm_finished".to_string(),
            serde_json::json!(stream_finished_llm_finished(&finished_event.reason)),
        );
        context.insert(
            "response_stream_terminal".to_string(),
            serde_json::json!(true),
        );
        context.insert(
            "elapsed_ms".to_string(),
            serde_json::json!(self.time_to_latest_event.num_milliseconds()),
        );
        context.insert(
            "should_refresh_model_config".to_string(),
            serde_json::json!(finished_event.should_refresh_model_config),
        );
        context.insert(
            "token_usage".to_string(),
            token_usage_context(&finished_event.token_usage),
        );
        if let Some(cost) = finished_event.request_cost.as_ref() {
            context.insert(
                "request_cost".to_string(),
                serde_json::json!({
                    "exact": cost.exact,
                    "platform_credits": cost.platform_credits,
                }),
            );
        }
        remote_logging::log_model_event(
            ctx,
            RemoteLogRecord {
                level: RemoteLogLevel::Info,
                message: "LLM response finished".to_string(),
                context: serde_json::Value::Object(context),
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn log_llm_request_error(
        &self,
        request_id: Uuid,
        error: impl std::fmt::Display,
        recovery: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        let mut context = self.common_remote_log_context("llm_request_error", request_id);
        context.insert(
            "elapsed_ms".to_string(),
            serde_json::json!(self.time_to_latest_event.num_milliseconds()),
        );
        context.insert("recovery".to_string(), serde_json::json!(recovery));
        context.insert("llm_finished".to_string(), serde_json::json!(false));
        context.insert(
            "response_stream_terminal".to_string(),
            serde_json::json!(true),
        );
        context.insert(
            "error".to_string(),
            serde_json::json!(remote_logging::sanitize_error(error)),
        );
        remote_logging::log_model_event(
            ctx,
            RemoteLogRecord {
                level: RemoteLogLevel::Warn,
                message: format!("LLM request error: {recovery}"),
                context: serde_json::Value::Object(context),
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn log_raw_model_response(
        &self,
        request_id: Uuid,
        payload_kind: &str,
        raw_payload: impl AsRef<str>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(raw_payload) =
            remote_logging::raw_model_payload_context(ctx, raw_payload.as_ref())
        else {
            return;
        };
        let mut context = self.common_remote_log_context("raw_model_response", request_id);
        context.insert("payload_kind".to_string(), serde_json::json!(payload_kind));
        context.insert("raw_payload".to_string(), raw_payload);
        remote_logging::log_model_event(
            ctx,
            RemoteLogRecord {
                level: RemoteLogLevel::Info,
                message: "Raw model response".to_string(),
                context: serde_json::Value::Object(context),
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn resolve_acp_manager(
        backend: &crate::persistence::model::AcpConversationData,
        ctx: &mut ModelContext<Self>,
    ) -> Result<galaxy_acp::AcpSessionManager, String> {
        use galaxy_acp::AcpManagerConfig;

        let settings = AISettings::as_ref(ctx);
        let provider = if backend.provider_id.is_empty() {
            settings.legacy_acp_provider()
        } else {
            settings.enabled_acp_provider_by_id(&backend.provider_id)
        }
        .ok_or_else(|| {
            "The ACP connection for this conversation is no longer configured in Settings. Add it again or start a new ACP conversation."
                .to_string()
        })?;
        let configured_agent_id = provider.agent_id.trim();
        let configured_agent_id = if configured_agent_id.is_empty() {
            "codex"
        } else {
            configured_agent_id
        };
        let launch = resolve_acp_launch(configured_agent_id, &provider.command, &provider.args)?;
        validate_acp_launch_identity(backend, configured_agent_id, &provider.command, &launch)?;
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
            turn_control,
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
                            turn_control,
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

    pub(super) fn new_provider_projection(
        params: api::RequestParams,
        ai_identifiers: AIIdentifiers,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let start_time = Local::now();
        let request_id = Uuid::new_v4();
        let response_stream_id = ResponseStreamId(Uuid::new_v4().to_string());
        #[cfg(not(target_family = "wasm"))]
        let provider_config = Self::resolve_provider_config(params.model.as_str(), ctx);
        #[cfg(not(target_family = "wasm"))]
        let remote_log_backend = "provider".to_string();
        #[cfg(not(target_family = "wasm"))]
        let remote_log_provider = Self::remote_log_provider_for_config(&provider_config);
        #[cfg(not(target_family = "wasm"))]
        Self::log_llm_request_started(
            ctx,
            &response_stream_id,
            request_id,
            &params,
            &ai_identifiers,
            &remote_log_backend,
            &remote_log_provider,
        );

        let projection_model = params.model.clone();
        let projection_messages_sent = params.messages_sent.clone();
        // For direct-provider projections, drop the heavy history fields from params
        // after logging. Only `tool_results` is needed post-init (for `has_error_tool_results`
        // and `tool_result_count`). This prevents retaining the full conversation history
        // for the lifetime of the response stream.
        let mut params = params;
        params.message_history = Vec::new();
        params.tool_result_archive = Vec::new();
        params.input = Vec::new();
        params.global_rules = Vec::new();
        params.mcp_context = None;
        Self {
            id: response_stream_id,
            runtime_capabilities: RuntimeCapabilities::provider(),
            #[cfg(not(target_family = "wasm"))]
            acp_session_metadata: Arc::new(Mutex::new(AcpSessionMetadata::default())),
            #[cfg(not(target_family = "wasm"))]
            acp_turn_control: Arc::new(Mutex::new(None)),
            params: Some(params),
            projection_model,
            projection_messages_sent,
            start_time,
            time_to_latest_event: TimeDelta::seconds(0),
            cancellation_tx: None,
            has_received_client_actions: false,
            ai_identifiers,
            #[cfg(not(target_family = "wasm"))]
            remote_log_backend,
            #[cfg(not(target_family = "wasm"))]
            remote_log_provider,
            stream_finished_received: false,
            error_event_emitted: false,
            current_request_id: None,
        }
    }

    pub(super) fn new_restored_provider_projection(
        model: LLMId,
        messages_sent: Arc<std::sync::Mutex<Vec<crate::ai::provider::types::ConversationMessage>>>,
        ai_identifiers: AIIdentifiers,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        #[cfg(not(target_family = "wasm"))]
        let provider_config = Self::resolve_provider_config(model.as_str(), ctx);
        Self {
            id: ResponseStreamId(Uuid::new_v4().to_string()),
            runtime_capabilities: RuntimeCapabilities::provider(),
            #[cfg(not(target_family = "wasm"))]
            acp_session_metadata: Arc::new(Mutex::new(AcpSessionMetadata::default())),
            #[cfg(not(target_family = "wasm"))]
            acp_turn_control: Arc::new(Mutex::new(None)),
            params: None,
            projection_model: model,
            projection_messages_sent: messages_sent,
            start_time: Local::now(),
            time_to_latest_event: TimeDelta::seconds(0),
            cancellation_tx: None,
            has_received_client_actions: false,
            ai_identifiers,
            #[cfg(not(target_family = "wasm"))]
            remote_log_backend: "provider".to_string(),
            #[cfg(not(target_family = "wasm"))]
            remote_log_provider: Self::remote_log_provider_for_config(&provider_config),
            stream_finished_received: false,
            error_event_emitted: false,
            current_request_id: None,
        }
    }

    pub(super) fn projected_event(
        event: warp_multi_agent_api::ResponseEvent,
    ) -> ResponseStreamEvent {
        ResponseStreamEvent::ReceivedEvent(Consumable::new(Ok(api::StreamEvent::Response(event))))
    }

    pub fn new(
        params: api::RequestParams,
        ai_identifiers: AIIdentifiers,
        agent_backend: AgentBackend,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let AgentBackend::Acp(backend) = agent_backend else {
            unreachable!("direct providers must use the durable provider-run projection");
        };
        let (cancellation_tx, cancellation_rx) = oneshot::channel();
        let start_time = Local::now();
        let request_id = Uuid::new_v4();
        let response_stream_id = ResponseStreamId(Uuid::new_v4().to_string());
        let runtime_capabilities = RuntimeCapabilities::session_runtime();
        #[cfg(not(target_family = "wasm"))]
        let acp_session_metadata = Arc::new(Mutex::new(AcpSessionMetadata::default()));
        #[cfg(not(target_family = "wasm"))]
        let acp_turn_control = Arc::new(Mutex::new(None));
        #[cfg(not(target_family = "wasm"))]
        let remote_log_backend = "acp".to_string();
        #[cfg(not(target_family = "wasm"))]
        let remote_log_provider = if backend.agent_id.is_empty() {
            "acp".to_string()
        } else {
            format!("acp:{}", backend.agent_id)
        };
        #[cfg(not(target_family = "wasm"))]
        {
            Self::log_llm_request_started(
                ctx,
                &response_stream_id,
                request_id,
                &params,
                &ai_identifiers,
                &remote_log_backend,
                &remote_log_provider,
            );
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
                    turn_control: acp_turn_control.clone(),
                },
                ctx,
            );
        }
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
        let projection_model = params.model.clone();
        let projection_messages_sent = params.messages_sent.clone();
        Self {
            id: response_stream_id,
            runtime_capabilities,
            #[cfg(not(target_family = "wasm"))]
            acp_session_metadata,
            #[cfg(not(target_family = "wasm"))]
            acp_turn_control,
            params: Some(params),
            projection_model,
            projection_messages_sent,
            start_time,
            time_to_latest_event: TimeDelta::seconds(0),
            cancellation_tx: Some(cancellation_tx),
            has_received_client_actions: false,
            ai_identifiers,
            #[cfg(not(target_family = "wasm"))]
            remote_log_backend,
            #[cfg(not(target_family = "wasm"))]
            remote_log_provider,
            stream_finished_received: false,
            error_event_emitted: false,
            current_request_id: Some(request_id),
        }
    }

    pub fn id(&self) -> &ResponseStreamId {
        &self.id
    }

    pub fn supports_shared_session_sync(&self) -> bool {
        self.runtime_capabilities.shared_session_sync
    }

    pub fn host_manages_history(&self) -> bool {
        self.runtime_capabilities.host_managed_history
    }

    pub(super) fn has_error_tool_results(&self) -> bool {
        self.params.as_ref().is_some_and(|params| {
            params
                .tool_results
                .iter()
                .any(galaxy_agent_core::ToolResult::is_error)
        })
    }

    pub(super) fn tool_result_count(&self) -> usize {
        self.params
            .as_ref()
            .map_or(0, |params| params.tool_results.len())
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn acp_session_metadata(&self) -> Option<AcpSessionMetadata> {
        self.runtime_capabilities
            .session_resume
            .then(|| {
                self.acp_session_metadata
                    .lock()
                    .ok()
                    .map(|state| state.clone())
            })
            .flatten()
    }

    pub(super) fn try_steer_runtime(&self, display_text: String) -> bool {
        #[cfg(not(target_family = "wasm"))]
        {
            if !self.runtime_capabilities.steering
                || self.current_request_id.is_none()
                || !self
                    .acp_session_metadata()
                    .is_some_and(|metadata| metadata.can_steer)
            {
                return false;
            }
            let Some(params) = self.params.as_ref() else {
                return false;
            };
            let mut model_text = display_text.clone();
            params.redact_text_for_model(&mut model_text);
            self.acp_turn_control
                .lock()
                .ok()
                .and_then(|control| control.clone())
                .is_some_and(|control| {
                    control
                        .try_send(TurnCommand::Steer {
                            display_text,
                            model_text,
                        })
                        .is_ok()
                })
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = display_text;
            false
        }
    }

    pub fn messages_sent(
        &self,
    ) -> &std::sync::Arc<std::sync::Mutex<Vec<crate::ai::provider::types::ConversationMessage>>>
    {
        &self.projection_messages_sent
    }

    /// Returns the model ID associated with this response stream's request.
    pub fn model_id(&self) -> &str {
        self.projection_model.as_str()
    }

    pub(super) fn llm_id(&self) -> &LLMId {
        &self.projection_model
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
                #[cfg(not(target_family = "wasm"))]
                self.log_llm_request_error(
                    request_id,
                    error.as_ref(),
                    "stream_creation_failed",
                    ctx,
                );
                self.error_event_emitted = true;
                self.report_request_failure(&error);
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
            log::debug!("Dropping event for stale ACP request {request_id}");
            return;
        }
        self.time_to_latest_event = Local::now().signed_duration_since(self.start_time);

        match &event {
            Ok(api::StreamEvent::Response(response_event)) => {
                let event_type_name = match &response_event.r#type {
                    Some(warp_multi_agent_api::response_event::Type::Init(_)) => "Init",
                    Some(warp_multi_agent_api::response_event::Type::ClientActions(_)) => {
                        "ClientActions"
                    }
                    Some(warp_multi_agent_api::response_event::Type::Finished(_)) => "Finished",
                    None => "None",
                };
                #[cfg(not(target_family = "wasm"))]
                self.log_raw_model_response(
                    request_id,
                    event_type_name,
                    format!("{response_event:#?}"),
                    ctx,
                );
                if let Some(event_type) = &response_event.r#type {
                    match event_type {
                        warp_multi_agent_api::response_event::Type::Init(init_event) => {
                            // Capture server_output_id from StreamInit event
                            self.ai_identifiers.server_output_id =
                                Some(crate::ai::agent::ServerOutputId::new(
                                    init_event.request_id.clone(),
                                ));
                        }
                        warp_multi_agent_api::response_event::Type::ClientActions(
                            client_actions,
                        ) => {
                            // Mark that we've received client actions
                            self.has_received_client_actions = true;
                            #[cfg(not(target_family = "wasm"))]
                            {
                                let mut context = self.common_remote_log_context(
                                    "llm_client_actions_received",
                                    request_id,
                                );
                                context.insert(
                                    "action_count".to_string(),
                                    serde_json::json!(client_actions.actions.len()),
                                );
                                remote_logging::log_model_event(
                                    ctx,
                                    RemoteLogRecord {
                                        level: RemoteLogLevel::Info,
                                        message: "LLM client actions received".to_string(),
                                        context: serde_json::Value::Object(context),
                                    },
                                );
                            }
                        }
                        warp_multi_agent_api::response_event::Type::Finished(finished_event) => {
                            self.stream_finished_received = true;
                            #[cfg(not(target_family = "wasm"))]
                            self.log_llm_response_finished(request_id, finished_event, ctx);
                        }
                    }
                }
                ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(event)));
            }
            Err(e) => {
                log::info!("ResponseStream received error: {e:?}");
                #[cfg(not(target_family = "wasm"))]
                self.log_llm_request_error(request_id, e.as_ref(), "fail", ctx);
                self.error_event_emitted = true;
                self.report_request_failure(e);
                ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(event)));
            }
        }
    }

    fn on_response_stream_complete(&mut self, request_id: Uuid, ctx: &mut ModelContext<Self>) {
        if self.current_request_id.is_none_or(|id| id != request_id) {
            log::debug!("Ignoring completion for stale ACP request {request_id}");
            return;
        }
        // ACP sends StreamFinished before closing. A clean EOF without it is a
        // truncated protocol response and cannot be retried safely by this projection.
        if !self.stream_finished_received && !self.error_event_emitted {
            log::warn!("ACP response stream ended without emitting StreamFinished");
            let unexpected_eof = Arc::new(AIApiError::UnexpectedEof);
            #[cfg(not(target_family = "wasm"))]
            self.log_llm_request_error(request_id, unexpected_eof.as_ref(), "fail", ctx);
            self.error_event_emitted = true;
            self.report_request_failure(&unexpected_eof);
            ctx.emit(ResponseStreamEvent::ReceivedEvent(Consumable::new(Err(
                unexpected_eof,
            ))));
        }

        ctx.emit(ResponseStreamEvent::AfterStreamFinished { cancellation: None });
        self.cancellation_tx = None;
    }

    /// Reports a terminal response-stream failure with classification tags.
    fn report_request_failure(&self, error: &Arc<AIApiError>) {
        #[cfg(feature = "crash_reporting")]
        sentry::with_scope(
            |scope| {
                scope.set_tag(
                    "has_received_client_actions",
                    self.has_received_client_actions,
                );
                scope.set_tag("error", format!("{error:?}"));
                scope.set_tag("is_recoverable", error.is_recoverable());
            },
            || {
                report_error!(anyhow!(error.clone()).context("ACP response stream failed"));
            },
        );
        #[cfg(not(feature = "crash_reporting"))]
        report_error!(anyhow!(error.clone()).context("ACP response stream failed"));
    }
}

#[cfg(not(target_family = "wasm"))]
fn raw_model_request_payload(params: &api::RequestParams) -> String {
    let payload = serde_json::json!({
        "model_id": params.model.as_str(),
        "coding_model_id": params.coding_model.as_str(),
        "cli_agent_model_id": params.cli_agent_model.as_str(),
        "computer_use_model_id": params.computer_use_model.as_str(),
        "input": format!("{:#?}", params.input),
        "tool_results": format!("{:#?}", params.tool_results),
        "tasks": format!("{:#?}", params.tasks),
        "message_history": serde_json::to_value(&params.message_history)
            .unwrap_or_else(|_| serde_json::json!(format!("{:#?}", params.message_history))),
        "progressive_summary": &params.progressive_summary,
        "tool_result_archive": serde_json::to_value(&params.tool_result_archive)
            .unwrap_or_else(|_| serde_json::json!(format!("{:#?}", params.tool_result_archive))),
        "global_rules": &params.global_rules,
        "mcp_context": format!("{:#?}", params.mcp_context),
        "session": {
            "is_remote": params.session_context.is_remote(),
        },
        "features": {
            "memory_enabled": params.is_memory_enabled,
            "warp_drive_context_enabled": params.warp_drive_context_enabled,
            "planning_enabled": params.planning_enabled,
            "web_search_enabled": params.web_search_enabled,
            "computer_use_enabled": params.computer_use_enabled,
            "ask_user_question_enabled": params.ask_user_question_enabled,
            "research_agent_enabled": params.research_agent_enabled,
            "orchestration_enabled": params.orchestration_enabled,
        },
        "autonomy_level": format!("{:?}", params.autonomy_level),
        "isolation_level": format!("{:?}", params.isolation_level),
        "supported_tools_override": format!("{:#?}", params.supported_tools_override),
        "context_window_limit": params.context_window_limit,
        "omitted_sensitive_fields": [
            "api_keys",
            "custom_model_providers",
            "custom_model_routers",
        ],
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| format!("{payload:#?}"))
}

#[cfg(not(target_family = "wasm"))]
fn stream_finished_reason_name(
    reason: &Option<response_event::stream_finished::Reason>,
) -> &'static str {
    match reason {
        None => "done",
        Some(response_event::stream_finished::Reason::Done(_)) => "done",
        Some(response_event::stream_finished::Reason::MaxTokenLimit(_)) => "max_token_limit",
        Some(response_event::stream_finished::Reason::Other(_)) => "other",
        Some(response_event::stream_finished::Reason::ContextWindowExceeded(_)) => {
            "context_window_exceeded"
        }
        Some(response_event::stream_finished::Reason::QuotaLimit(_)) => "quota_limit",
        Some(response_event::stream_finished::Reason::LlmUnavailable(_)) => "llm_unavailable",
        Some(response_event::stream_finished::Reason::InvalidApiKey(_)) => "invalid_api_key",
        Some(response_event::stream_finished::Reason::InternalError(_)) => "internal_error",
    }
}

#[cfg(not(target_family = "wasm"))]
fn stream_finished_llm_finished(reason: &Option<response_event::stream_finished::Reason>) -> bool {
    matches!(
        reason,
        None | Some(response_event::stream_finished::Reason::Done(_))
            | Some(response_event::stream_finished::Reason::MaxTokenLimit(_))
    )
}

#[cfg(not(target_family = "wasm"))]
fn token_usage_context(
    token_usage: &[response_event::stream_finished::TokenUsage],
) -> serde_json::Value {
    let total_input: u64 = token_usage
        .iter()
        .map(|usage| u64::from(usage.total_input))
        .sum();
    let output: u64 = token_usage
        .iter()
        .map(|usage| u64::from(usage.output))
        .sum();
    let input_cache_read: u64 = token_usage
        .iter()
        .map(|usage| u64::from(usage.input_cache_read))
        .sum();
    let input_cache_write: u64 = token_usage
        .iter()
        .map(|usage| u64::from(usage.input_cache_write))
        .sum();
    let cost_in_cents: f32 = token_usage.iter().map(|usage| usage.cost_in_cents).sum();
    serde_json::json!({
        "total_input": total_input,
        "output": output,
        "input_cache_read": input_cache_read,
        "input_cache_write": input_cache_write,
        "cost_in_cents": cost_in_cents,
        "models": token_usage.iter().map(|usage| {
            serde_json::json!({
                "model_id": usage.model_id,
                "total_input": usage.total_input,
                "output": usage.output,
                "input_cache_read": usage.input_cache_read,
                "input_cache_write": usage.input_cache_write,
                "cost_in_cents": usage.cost_in_cents,
            })
        }).collect::<Vec<_>>(),
    })
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
