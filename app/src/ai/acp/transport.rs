use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;
use futures::future::{BoxFuture, Fuse, FusedFuture as _};
use futures::stream::FusedStream as _;
use futures::{FutureExt as _, StreamExt as _};
use galaxy_acp::{
    AcpEvent, AcpPermissionPolicy, AcpRuntimeError, AcpSessionHandle, AcpSessionManager,
    AcpSteeringOutcome, AcpTurnRequest, ContentBlock, McpServer, McpServerStdio,
    SessionConfigOptionValue, SessionId, TextContent,
};

use super::launch::acp_selection_identity;
use super::prompt::{prompt_content, GalaxyTerminalTools};
use super::response_translator::AcpResponseTranslator;
use crate::ai::agent::api::{self, RequestParams};
use crate::ai::agent::EntrypointType;
use crate::persistence::model::AcpConversationData;
use crate::server::server_api::AIApiError;

#[derive(Clone, Debug, Default)]
pub(crate) struct AcpSessionMetadata {
    pub(crate) session_id: Option<String>,
    pub(crate) can_load: bool,
    pub(crate) can_steer: bool,
    pub(crate) config_options: Vec<galaxy_acp::SessionConfigOption>,
}

#[derive(Clone, Debug)]
pub(crate) struct AcpSteeringRequest {
    display_text: String,
    model_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GalaxyMcpTarget {
    pub(crate) window_id: String,
    pub(crate) tab_id: String,
    pub(crate) pane_id: String,
}

impl AcpSteeringRequest {
    pub(crate) fn text(display_text: String, model_text: String) -> Self {
        Self {
            display_text,
            model_text,
        }
    }
}

type SteeringResult = Result<AcpSteeringOutcome, AcpRuntimeError>;
type PendingSteering = Fuse<BoxFuture<'static, SteeringResult>>;

fn pending_steering(session: AcpSessionHandle, steering: AcpSteeringRequest) -> PendingSteering {
    async move {
        let content = ContentBlock::Text(TextContent::new(steering.model_text));
        session.steer(vec![content]).await
    }
    .boxed()
    .fuse()
}

pub(crate) type AcpSessionHandleSlot = Arc<Mutex<Option<AcpSessionHandle>>>;

struct AcpSessionHandleGuard {
    slot: AcpSessionHandleSlot,
}

impl AcpSessionHandleGuard {
    fn new(slot: AcpSessionHandleSlot, session: AcpSessionHandle) -> Self {
        if let Ok(mut active_session) = slot.lock() {
            *active_session = Some(session);
        }
        Self { slot }
    }
}

impl Drop for AcpSessionHandleGuard {
    fn drop(&mut self) {
        if let Ok(mut active_session) = self.slot.lock() {
            *active_session = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn acp_output_stream(
    manager: AcpSessionManager,
    params: RequestParams,
    conversation_id: String,
    backend: AcpConversationData,
    galaxy_mcp_server: Option<McpServer>,
    galaxy_terminal_interrupt_available: bool,
    permission_policy: AcpPermissionPolicy,
    auto_approve_permissions: bool,
    session_metadata: Arc<Mutex<AcpSessionMetadata>>,
    session_handle: AcpSessionHandleSlot,
    steering_rx: async_channel::Receiver<AcpSteeringRequest>,
    cancellation_rx: oneshot::Receiver<()>,
) -> api::ResponseStream {
    let mut translator = response_translator(&params, &backend);
    let terminal_tools = GalaxyTerminalTools {
        status: galaxy_mcp_server.is_some(),
        interrupt: galaxy_mcp_server.is_some() && galaxy_terminal_interrupt_available,
    };
    let prompt = match prompt_content(&params, terminal_tools) {
        Ok(prompt) => prompt,
        Err(error) => return translated_startup_error_stream(translator, &error),
    };
    let cwd = params
        .session_context
        .current_working_directory()
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut mcp_servers = Vec::new();
    if let Some(server) = galaxy_mcp_server {
        mcp_servers.push(server);
    }
    let request = AcpTurnRequest {
        config_values: backend
            .config_values
            .into_iter()
            .filter_map(|(key, value)| {
                serde_json::from_value::<SessionConfigOptionValue>(value)
                    .ok()
                    .map(|value| (key, value))
            })
            .collect(),
        conversation_key: conversation_id,
        session_id: backend.session_id.map(SessionId::from),
        cwd,
        additional_directories: Vec::new(),
        prompt,
        mcp_servers,
        auto_approve_permissions,
        permission_policy,
        prompt_capabilities: Default::default(),
    };
    let (session, events) = match manager.run_turn(request) {
        Ok(turn) => turn,
        Err(error) => return translated_startup_error_stream(translator, &error.to_string()),
    };
    let session_handle_guard = AcpSessionHandleGuard::new(session_handle, session.clone());

    let stream = async_stream::stream! {
        let _session_handle_guard = session_handle_guard;
        let mut cancellation_rx = cancellation_rx.fuse();
        let mut events = Box::pin(events.fuse());
        let mut steering_rx = Box::pin(steering_rx.fuse());
        let mut steering_queue = VecDeque::new();
        let mut steering_result: PendingSteering = Fuse::terminated();
        loop {
            futures::select_biased! {
                _ = cancellation_rx => {
                    if let Err(error) = session.cancel().await {
                        log::warn!("Failed to cancel ACP turn cleanly: {error}");
                    }
                    break;
                }
                steering = steering_rx.next() => {
                    let Some(steering) = steering else {
                        continue;
                    };
                    let content = ContentBlock::Text(TextContent::new(
                        steering.display_text.clone(),
                    ));
                    match translator.translate_steered_user_content(content) {
                        Ok(response_events) => {
                            for response_event in response_events {
                                yield Ok(response_event);
                            }
                        }
                        Err(message) => {
                            yield Err(Arc::new(AIApiError::Stream {
                                stream_type: "acp",
                                source: anyhow::anyhow!(message),
                            }));
                            break;
                        }
                    }
                    if steering_result.is_terminated() {
                        steering_result = pending_steering(session.clone(), steering);
                    } else {
                        steering_queue.push_back(steering);
                    }
                }
                steering = steering_result => {
                    match steering {
                        Ok(AcpSteeringOutcome::Injected) => {
                            // The user message was rendered as soon as Galaxy
                            // accepted it; keep consuming agent events without
                            // holding the transcript behind the steering RPC.
                        }
                        Ok(AcpSteeringOutcome::StartedNewTurn) => {
                            for response_event in translator.steering_started_new_turn() {
                                yield Ok(response_event);
                            }
                        }
                        Ok(AcpSteeringOutcome::Failed) => {
                            for response_event in translator.steering_failed(
                                "the ACP agent could not inject it into the active turn",
                            ) {
                                yield Ok(response_event);
                            }
                        }
                        Err(error) => {
                            log::warn!("ACP live steering failed: {error}");
                            for response_event in translator.steering_failed(&error.to_string()) {
                                yield Ok(response_event);
                            }
                        }
                    }
                    steering_result = Fuse::terminated();
                    if let Some(steering) = steering_queue.pop_front() {
                        steering_result = pending_steering(session.clone(), steering);
                    } else if events.is_terminated() {
                        break;
                    }
                }
                event = events.next() => {
                    let Some(event) = event else {
                        if steering_result.is_terminated() && steering_queue.is_empty() {
                            break;
                        }
                        continue;
                    };
                    if let AcpEvent::SessionStarted {
                        session_id,
                        can_load,
                        can_steer,
                        ..
                    } = &event
                    {
                        if let Ok(mut metadata) = session_metadata.lock() {
                            metadata.session_id = Some(session_id.to_string());
                            metadata.can_load = *can_load;
                            metadata.can_steer = *can_steer;
                        }
                    }
                    if let AcpEvent::ConfigOptions { options } = &event {
                        if let Ok(mut metadata) = session_metadata.lock() {
                            metadata.config_options = options.clone();
                        }
                    }
                    match translator.translate(event) {
                        Ok(response_events) => {
                            for response_event in response_events {
                                yield Ok(response_event);
                            }
                        }
                        Err(message) => {
                            yield Err(Arc::new(AIApiError::Stream {
                                stream_type: "acp",
                                source: anyhow::anyhow!(message),
                            }));
                            break;
                        }
                    }
                }
            }
        }
    };
    Box::pin(stream)
}

pub(crate) fn acp_startup_error_stream(
    params: &RequestParams,
    backend: &AcpConversationData,
    message: &str,
) -> api::ResponseStream {
    translated_startup_error_stream(response_translator(params, backend), message)
}

fn response_translator(
    params: &RequestParams,
    backend: &AcpConversationData,
) -> AcpResponseTranslator {
    let task_id = params
        .root_task_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let user_query = request_user_query(params);
    AcpResponseTranslator::new(
        task_id,
        params.tasks.is_empty(),
        user_query,
        acp_selection_identity(&backend.agent_id, &backend.config_values),
    )
}

fn request_user_query(params: &RequestParams) -> Option<String> {
    let should_display = params.metadata.as_ref().is_none_or(|metadata| {
        !metadata.is_auto_resume_after_error
            && matches!(
                metadata.entrypoint,
                EntrypointType::PromptSuggestion { .. }
                    | EntrypointType::ZeroStateAgentModePromptSuggestion
                    | EntrypointType::UserInitiated
                    | EntrypointType::SharedSession
                    | EntrypointType::CloneRepository
            )
    });
    if !should_display {
        return None;
    }
    params
        .input
        .iter()
        .rev()
        .find_map(crate::ai::agent::AIAgentInput::display_query)
}

pub(crate) fn galaxy_mcp_server(
    target: &GalaxyMcpTarget,
    allow_terminal_execute: bool,
    allow_terminal_interrupt: bool,
) -> Result<McpServer, String> {
    if !cfg!(unix) {
        return Err("ACP Galaxy terminal tools are currently available only on Unix".to_owned());
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate the Galaxy executable: {error}"))?;
    let args = galaxy_mcp_args(target, allow_terminal_execute, allow_terminal_interrupt);
    Ok(McpServer::Stdio(
        McpServerStdio::new("Galaxy", executable).args(args),
    ))
}

fn galaxy_mcp_args(
    target: &GalaxyMcpTarget,
    allow_terminal_execute: bool,
    allow_terminal_interrupt: bool,
) -> Vec<String> {
    let mut args = vec![
        "--galaxyctrl".to_owned(),
        "mcp".to_owned(),
        "--pid".to_owned(),
        std::process::id().to_string(),
        "--window".to_owned(),
        target.window_id.clone(),
        "--tab".to_owned(),
        target.tab_id.clone(),
        "--pane".to_owned(),
        target.pane_id.clone(),
        "--agent-safe".to_owned(),
    ];
    if allow_terminal_execute {
        args.push("--allow-terminal-execute".to_owned());
    }
    if allow_terminal_interrupt {
        args.push("--allow-terminal-interrupt".to_owned());
    }
    args
}

fn translated_startup_error_stream(
    mut translator: AcpResponseTranslator,
    message: &str,
) -> api::ResponseStream {
    let events = translator.startup_error(message);
    Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
