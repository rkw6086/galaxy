use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use futures::future::{BoxFuture, Fuse, FusedFuture as _};
use futures::stream::FusedStream as _;
use futures::{FutureExt as _, StreamExt as _};
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, AgentRuntime, ContentPart,
    MessageContent, RuntimeActivity, RuntimeActivityStatus, RuntimeCapabilities, RuntimeDescriptor,
    RuntimeKind, StopReason as AgentStopReason, TurnCommand, TurnControl, TurnRequest,
};

use crate::{
    AcpEvent, AcpPermissionPolicy, AcpRuntimeError, AcpSessionHandle, AcpSessionManager,
    AcpSteeringOutcome, AcpTurnRequest, ContentBlock, ImageContent, McpServer, SessionConfigOption,
    SessionConfigOptionValue, SessionId, StopReason, TextContent, ToolCallId, ToolCallStatus,
};

/// Session state learned from the connected ACP agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AcpRuntimeState {
    pub session_id: Option<String>,
    pub can_load: bool,
    pub can_steer: bool,
    pub config_options: Vec<SessionConfigOption>,
}

pub type AcpRuntimeStateHandle = Arc<Mutex<AcpRuntimeState>>;

/// ACP-specific session inputs that are stable for one Galaxy turn.
#[derive(Clone, Debug)]
pub struct AcpAgentRuntimeConfig {
    pub runtime_id: String,
    pub display_name: String,
    pub config_values: BTreeMap<String, SessionConfigOptionValue>,
    pub session_id: Option<SessionId>,
    pub cwd: PathBuf,
    pub additional_directories: Vec<PathBuf>,
    pub mcp_servers: Vec<McpServer>,
    pub auto_approve_permissions: bool,
    pub permission_policy: AcpPermissionPolicy,
}

impl AcpAgentRuntimeConfig {
    #[must_use]
    pub fn new(
        runtime_id: impl Into<String>,
        display_name: impl Into<String>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runtime_id: runtime_id.into(),
            display_name: display_name.into(),
            config_values: BTreeMap::new(),
            session_id: None,
            cwd: cwd.into(),
            additional_directories: Vec::new(),
            mcp_servers: Vec::new(),
            auto_approve_permissions: false,
            permission_policy: AcpPermissionPolicy::default(),
        }
    }
}

/// Adapts an official ACP client session to Galaxy's provider-neutral runtime contract.
pub struct AcpAgentRuntime {
    manager: AcpSessionManager,
    config: AcpAgentRuntimeConfig,
    descriptor: RuntimeDescriptor,
    state: AcpRuntimeStateHandle,
}

impl AcpAgentRuntime {
    #[must_use]
    pub const fn capabilities() -> RuntimeCapabilities {
        RuntimeCapabilities::session_runtime()
    }

    #[must_use]
    pub fn new(manager: AcpSessionManager, config: AcpAgentRuntimeConfig) -> Self {
        let descriptor = RuntimeDescriptor {
            id: config.runtime_id.clone(),
            display_name: config.display_name.clone(),
            kind: RuntimeKind::Acp,
            capabilities: Self::capabilities(),
        };
        Self {
            manager,
            config,
            descriptor,
            state: Arc::new(Mutex::new(AcpRuntimeState::default())),
        }
    }

    #[must_use]
    pub fn with_state(mut self, state: AcpRuntimeStateHandle) -> Self {
        self.state = state;
        self
    }

    #[must_use]
    pub fn state(&self) -> AcpRuntimeStateHandle {
        Arc::clone(&self.state)
    }

    fn acp_turn_request(&self, request: TurnRequest) -> Result<AcpTurnRequest, AgentError> {
        if request.model.as_str() != self.descriptor.id {
            return Err(invalid_request(format!(
                "ACP runtime '{}' cannot serve selection '{}'",
                self.descriptor.id,
                request.model.as_str()
            )));
        }
        if !request.messages.is_empty() {
            return Err(invalid_request(
                "ACP sessions own conversation history; only the explicit current prompt is accepted",
            ));
        }
        if request.max_output_tokens.is_some() {
            return Err(invalid_request(
                "ACP agents own their output limit; Galaxy cannot apply a provider token limit",
            ));
        }
        let conversation_key = request
            .conversation_id
            .ok_or_else(|| invalid_request("ACP turns require a stable Galaxy conversation ID"))?;
        let prompt = request
            .prompt
            .ok_or_else(|| invalid_request("ACP turns require an explicit current prompt"))?;
        if request.system_prompt.is_some() {
            return Err(invalid_request(
                "ACP agents own their system prompt; Galaxy cannot send a provider system message",
            ));
        }
        if !request.tools.is_empty() {
            return Err(invalid_request(
                "ACP tools must be attached as MCP servers, not provider tool definitions",
            ));
        }

        Ok(AcpTurnRequest {
            config_values: self.config.config_values.clone(),
            conversation_key,
            session_id: self.config.session_id.clone(),
            cwd: self.config.cwd.clone(),
            additional_directories: self.config.additional_directories.clone(),
            prompt: prompt_content(prompt)?,
            mcp_servers: self.config.mcp_servers.clone(),
            auto_approve_permissions: self.config.auto_approve_permissions,
            permission_policy: self.config.permission_policy,
        })
    }
}

#[async_trait]
impl AgentRuntime for AcpAgentRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let request = self.acp_turn_request(request)?;
        let (session, events) = self
            .manager
            .run_turn(request)
            .map_err(agent_error_from_runtime)?;
        Ok(acp_event_stream(
            session,
            events,
            control,
            Arc::clone(&self.state),
        ))
    }
}

type SteeringResult = Result<AcpSteeringOutcome, AcpRuntimeError>;
type PendingSteering = Fuse<BoxFuture<'static, SteeringResult>>;

fn pending_steering(session: AcpSessionHandle, model_text: String) -> PendingSteering {
    async move {
        session
            .steer(vec![ContentBlock::Text(TextContent::new(model_text))])
            .await
    }
    .boxed()
    .fuse()
}

fn acp_event_stream(
    session: AcpSessionHandle,
    events: async_channel::Receiver<AcpEvent>,
    control: TurnControl,
    state: AcpRuntimeStateHandle,
) -> AgentEventStream {
    let stream = async_stream::stream! {
        let mut events = Box::pin(events.fuse());
        let mut control_open = true;
        let mut steering_queue = VecDeque::new();
        let mut steering_result: PendingSteering = Fuse::terminated();
        let mut tool_titles = HashMap::new();

        loop {
            let command = if control_open {
                control.receive().boxed().fuse()
            } else {
                Fuse::terminated()
            };
            futures::pin_mut!(command);
            futures::select_biased! {
                command = command => match command {
                    Ok(TurnCommand::Cancel) => {
                        if let Err(error) = session.cancel().await {
                            log::warn!("Failed to cancel ACP turn cleanly: {error}");
                        }
                        yield Ok(AgentEvent::TurnStopped {
                            reason: AgentStopReason::Cancelled,
                        });
                        return;
                    }
                    Ok(TurnCommand::Steer { display_text, model_text }) => {
                        yield Ok(AgentEvent::UserInputAccepted { text: display_text });
                        if steering_result.is_terminated() {
                            steering_result = pending_steering(session.clone(), model_text);
                        } else {
                            steering_queue.push_back(model_text);
                        }
                    }
                    Err(_) => control_open = false,
                },
                steering = steering_result => {
                    match steering {
                        Ok(AcpSteeringOutcome::Injected) => {}
                        Ok(AcpSteeringOutcome::StartedNewTurn) => {
                            yield Ok(AgentEvent::RuntimeNotice {
                                message: "The ACP adapter started that steering message as a separate turn instead of injecting it into the active one. Galaxy terminated the adapter process immediately, but the turn may have begun acting; check the current terminal and file state before retrying.".to_owned(),
                            });
                        }
                        Ok(AcpSteeringOutcome::Failed) => {
                            yield Ok(AgentEvent::RuntimeNotice {
                                message: steering_failure_notice(
                                    "the ACP agent could not inject it into the active turn",
                                ),
                            });
                        }
                        Err(error) => {
                            log::warn!("ACP live steering failed: {error}");
                            yield Ok(AgentEvent::RuntimeNotice {
                                message: steering_failure_notice(&error.to_string()),
                            });
                        }
                    }
                    steering_result = Fuse::terminated();
                    if let Some(model_text) = steering_queue.pop_front() {
                        steering_result = pending_steering(session.clone(), model_text);
                    } else if events.is_terminated() {
                        yield Err(AgentError::new(
                            AgentErrorKind::Protocol,
                            "ACP event stream closed before the turn reported completion",
                        ));
                        return;
                    }
                },
                event = events.next() => {
                    let Some(event) = event else {
                        if steering_result.is_terminated() && steering_queue.is_empty() {
                            yield Err(AgentError::new(
                                AgentErrorKind::Protocol,
                                "ACP event stream closed before the turn reported completion",
                            ));
                            return;
                        }
                        continue;
                    };
                    match map_event(event, &state, &mut tool_titles) {
                        Ok(Some(event)) => {
                            let stopped = matches!(event, AgentEvent::TurnStopped { .. });
                            yield Ok(event);
                            if stopped {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
            }
        }
    };
    Box::pin(stream)
}

fn map_event(
    event: AcpEvent,
    state: &AcpRuntimeStateHandle,
    tool_titles: &mut HashMap<ToolCallId, String>,
) -> Result<Option<AgentEvent>, AgentError> {
    let event = match event {
        AcpEvent::SessionStarted {
            session_id,
            can_load,
            can_steer,
            ..
        } => {
            if let Ok(mut state) = state.lock() {
                state.session_id = Some(session_id.to_string());
                state.can_load = can_load;
                state.can_steer = can_steer;
            }
            Some(AgentEvent::TurnStarted {
                runtime_request_id: session_id.to_string(),
            })
        }
        AcpEvent::AgentText { text } => Some(AgentEvent::TextDelta { text }),
        AcpEvent::AgentThought { text } => Some(AgentEvent::ReasoningDelta { text }),
        AcpEvent::AgentContent { content, thought } => map_content(content, thought),
        // Initial prompt echoes can contain hidden Galaxy context. Steering is
        // emitted from the accepted command above, so no ACP echo is trusted.
        AcpEvent::UserContent { .. } => None,
        AcpEvent::ToolCall {
            id,
            title,
            status,
            output,
        } => {
            tool_titles.insert(id.clone(), title.clone());
            Some(AgentEvent::RuntimeActivityUpdated {
                activity: RuntimeActivity {
                    id: id.to_string(),
                    title,
                    status: Some(activity_status(status)),
                    output,
                },
            })
        }
        AcpEvent::ToolCallUpdate {
            id,
            title,
            status,
            output,
        } => {
            let title = title
                .or_else(|| tool_titles.get(&id).cloned())
                .unwrap_or_else(|| "tool".to_owned());
            tool_titles.insert(id.clone(), title.clone());
            Some(AgentEvent::RuntimeActivityUpdated {
                activity: RuntimeActivity {
                    id: id.to_string(),
                    title,
                    status: status.map(activity_status),
                    output,
                },
            })
        }
        AcpEvent::ConfigOptions { options } => {
            if let Ok(mut state) = state.lock() {
                state.config_options = options;
            }
            None
        }
        AcpEvent::Usage { used, size, .. } => Some(AgentEvent::ContextUsageUpdated {
            used_tokens: used,
            context_size: size,
        }),
        AcpEvent::PermissionRequested { request } => Some(AgentEvent::RuntimeNotice {
            message: format!(
                "\n\n> Permission requested for: {}\n",
                request.tool_call.fields.title.as_deref().unwrap_or("tool")
            ),
        }),
        AcpEvent::PermissionResolved { decision, .. } => Some(AgentEvent::RuntimeNotice {
            message: format!("\n\n> Permission decision: {decision:?}\n"),
        }),
        AcpEvent::Finished { stop_reason } => Some(AgentEvent::TurnStopped {
            reason: stop_reason_from_acp(stop_reason),
        }),
        AcpEvent::Error { message } => {
            return Err(AgentError::new(AgentErrorKind::Protocol, message));
        }
    };
    Ok(event)
}

fn map_content(content: ContentBlock, thought: bool) -> Option<AgentEvent> {
    let text = match content {
        ContentBlock::Text(text) => text.text,
        ContentBlock::Image(_) => "[Agent returned an image.]".to_owned(),
        ContentBlock::Audio(_) => "[Agent returned audio.]".to_owned(),
        ContentBlock::ResourceLink(resource) => format!("[Agent referenced {}.]", resource.name),
        ContentBlock::Resource(_) => "[Agent returned embedded resource content.]".to_owned(),
        _ => "[Agent returned unsupported content.]".to_owned(),
    };
    if thought {
        Some(AgentEvent::ReasoningDelta { text })
    } else {
        Some(AgentEvent::TextDelta { text })
    }
}

fn activity_status(status: ToolCallStatus) -> RuntimeActivityStatus {
    match status {
        ToolCallStatus::Pending => RuntimeActivityStatus::Pending,
        ToolCallStatus::InProgress => RuntimeActivityStatus::InProgress,
        ToolCallStatus::Completed => RuntimeActivityStatus::Completed,
        ToolCallStatus::Failed => RuntimeActivityStatus::Failed,
        _ => RuntimeActivityStatus::Other("updated".to_owned()),
    }
}

fn stop_reason_from_acp(reason: StopReason) -> AgentStopReason {
    match reason {
        StopReason::EndTurn => AgentStopReason::Completed,
        StopReason::Cancelled => AgentStopReason::Cancelled,
        StopReason::MaxTokens | StopReason::MaxTurnRequests => AgentStopReason::MaxTokens,
        StopReason::Refusal => AgentStopReason::Refusal,
        _ => AgentStopReason::Other(format!("{reason:?}")),
    }
}

fn prompt_content(content: MessageContent) -> Result<Vec<ContentBlock>, AgentError> {
    match content {
        MessageContent::Text(text) => Ok(vec![ContentBlock::Text(TextContent::new(text))]),
        MessageContent::MultiPart(parts) => parts.into_iter().map(prompt_part).collect(),
        MessageContent::ToolUse { .. } | MessageContent::ToolResult { .. } => Err(invalid_request(
            "ACP prompts cannot contain provider tool-use history",
        )),
    }
}

fn prompt_part(part: ContentPart) -> Result<ContentBlock, AgentError> {
    match part {
        ContentPart::Text(text) => Ok(ContentBlock::Text(TextContent::new(text))),
        ContentPart::Image { data, mime_type } => {
            let image = ImageContent::new(
                base64::engine::general_purpose::STANDARD.encode(data),
                mime_type,
            );
            Ok(ContentBlock::Image(image))
        }
        ContentPart::Reasoning { .. }
        | ContentPart::ToolUse { .. }
        | ContentPart::ToolResult { .. } => Err(invalid_request(
            "ACP prompts support only user text and image content",
        )),
    }
}

fn steering_failure_notice(error: &str) -> String {
    format!(
        "Galaxy couldn't confirm that live steering message: {error}. The agent may not have received it; check the current terminal and file state before retrying."
    )
}

fn invalid_request(message: impl Into<String>) -> AgentError {
    AgentError::new(AgentErrorKind::InvalidRequest, message)
}

fn agent_error_from_runtime(error: AcpRuntimeError) -> AgentError {
    let kind = match error {
        AcpRuntimeError::InvalidTurn(_) => AgentErrorKind::InvalidRequest,
        AcpRuntimeError::AuthenticationTimeout(_) => AgentErrorKind::Authentication,
        AcpRuntimeError::WorkerSpawn(_)
        | AcpRuntimeError::RuntimeClosed(_)
        | AcpRuntimeError::InitializationTimeout(_)
        | AcpRuntimeError::DiscoveryTimeout(_) => AgentErrorKind::Transport,
        AcpRuntimeError::ProcessTreeTeardownUnsupported => AgentErrorKind::Configuration,
        AcpRuntimeError::TurnNotActive
        | AcpRuntimeError::SteeringUnsupported
        | AcpRuntimeError::SteeringTimeout(_)
        | AcpRuntimeError::Protocol(_) => AgentErrorKind::Protocol,
    };
    AgentError::new(kind, error.to_string())
}

#[cfg(test)]
#[path = "agent_runtime_tests.rs"]
mod tests;
