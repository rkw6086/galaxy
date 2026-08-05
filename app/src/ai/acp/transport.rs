use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _};
use galaxy_acp::{
    AcpAgentRuntime, AcpAgentRuntimeConfig, AcpPermissionPolicy, AcpRuntimeState,
    AcpRuntimeStateHandle, AcpSessionManager, McpServer, McpServerStdio, SessionConfigOptionValue,
    SessionId,
};
use galaxy_agent_core::{
    turn_control, AgentRuntime as _, RuntimeCapabilities, TurnCommand, TurnCommandSender,
    TurnRequest,
};

use super::launch::acp_selection_identity;
use super::prompt::{prompt_content, GalaxyTerminalTools};
use crate::ai::agent::api::{self, RequestParams};
use crate::ai::agent::EntrypointType;
use crate::ai::runtime::{RuntimeResponseConfig, RuntimeResponseTranslator};
use crate::persistence::model::AcpConversationData;
use crate::server::server_api::AIApiError;

pub(crate) type AcpSessionMetadata = AcpRuntimeState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GalaxyMcpTarget {
    pub(crate) window_id: String,
    pub(crate) tab_id: String,
    pub(crate) pane_id: String,
}

pub(crate) type AcpTurnControlSlot = Arc<Mutex<Option<TurnCommandSender>>>;

struct AcpTurnControlGuard {
    slot: AcpTurnControlSlot,
}

impl AcpTurnControlGuard {
    fn new(slot: AcpTurnControlSlot, control: TurnCommandSender) -> Self {
        if let Ok(mut active_control) = slot.lock() {
            *active_control = Some(control);
        }
        Self { slot }
    }
}

impl Drop for AcpTurnControlGuard {
    fn drop(&mut self) {
        if let Ok(mut active_control) = self.slot.lock() {
            *active_control = None;
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
    session_metadata: AcpRuntimeStateHandle,
    turn_control_slot: AcpTurnControlSlot,
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
    let runtime_id = acp_selection_identity(&backend.agent_id, &backend.config_values);
    let mut runtime_config =
        AcpAgentRuntimeConfig::new(runtime_id.clone(), backend.agent_id.clone(), cwd);
    runtime_config.config_values = backend
        .config_values
        .iter()
        .filter_map(|(key, value)| {
            serde_json::from_value::<SessionConfigOptionValue>(value.clone())
                .ok()
                .map(|value| (key.clone(), value))
        })
        .collect();
    runtime_config.session_id = backend.session_id.clone().map(SessionId::from);
    runtime_config.mcp_servers = mcp_servers;
    runtime_config.auto_approve_permissions = auto_approve_permissions;
    runtime_config.permission_policy = permission_policy;
    let runtime = AcpAgentRuntime::new(manager, runtime_config).with_state(session_metadata);
    let mut request = TurnRequest::new(runtime_id, Vec::new()).with_prompt(prompt);
    request.conversation_id = Some(conversation_id);
    let (control_sender, control) = turn_control();
    let events = match runtime.start_turn(request, control).await {
        Ok(events) => events,
        Err(error) => return translated_startup_error_stream(translator, &error.to_string()),
    };
    let turn_control_guard = AcpTurnControlGuard::new(turn_control_slot, control_sender.clone());

    let stream = async_stream::stream! {
        let _turn_control_guard = turn_control_guard;
        let mut cancellation_rx = cancellation_rx.fuse();
        let mut events = events.fuse();
        loop {
            futures::select_biased! {
                _ = cancellation_rx => {
                    if let Err(error) = control_sender.try_send(TurnCommand::Cancel) {
                        log::warn!("Failed to queue ACP cancellation: {error}");
                    }
                }
                event = events.next() => {
                    let Some(event) = event else {
                        break;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            yield Err(Arc::new(AIApiError::Stream {
                                stream_type: "acp",
                                source: anyhow::anyhow!(error),
                            }));
                            break;
                        }
                    };
                    match translator.translate(event) {
                        Ok(response_events) => {
                            for response_event in response_events {
                                yield Ok(api::StreamEvent::Response(response_event));
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
) -> RuntimeResponseTranslator {
    let task_id = params
        .root_task_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let user_query = request_user_query(params);
    RuntimeResponseTranslator::new(RuntimeResponseConfig {
        task_id,
        // ACP owns its session identifier. Keeping this empty prevents the
        // compatibility Init event from entering Galaxy cloud-token paths.
        conversation_id: String::new(),
        needs_create_task: params.tasks.is_empty(),
        user_query,
        model_id: acp_selection_identity(&backend.agent_id, &backend.config_values),
        max_context_tokens: None,
        capabilities: RuntimeCapabilities::session_runtime(),
        empty_output_message: Some("> ACP agent completed without a text response.".to_owned()),
    })
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
    mut translator: RuntimeResponseTranslator,
    message: &str,
) -> api::ResponseStream {
    let events =
        translator.startup_error(&format!("Galaxy couldn't start the ACP agent: {message}"));
    Box::pin(futures::stream::iter(
        events
            .into_iter()
            .map(|event| Ok(api::StreamEvent::Response(event))),
    ))
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
