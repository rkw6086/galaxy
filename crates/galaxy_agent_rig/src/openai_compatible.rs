use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, AgentRuntime, ContentPart,
    ConversationMessage, MessageContent, MessageRole, RuntimeCapabilities, RuntimeDescriptor,
    RuntimeKind, StopReason, ToolCall, TurnCommand, TurnControl, TurnRequest, Usage,
};
use rig_core::OneOrMany;
use rig_core::client::CompletionClient;
use rig_core::completion::{
    AssistantContent, CompletionError, CompletionModel, CompletionRequest, GetTokenUsage, Message,
    ToolDefinition,
};
use rig_core::message::{
    DocumentSourceKind, Image, ImageMediaType, MimeType, ToolResultContent, UserContent,
};
use rig_core::providers::openai;
use rig_core::streaming::StreamedAssistantContent;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAICompatibleRuntimeConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub max_output_tokens: Option<u64>,
    pub supports_system_messages: bool,
}

#[derive(Clone, Debug)]
pub struct OpenAICompatibleRuntime {
    config: OpenAICompatibleRuntimeConfig,
    descriptor: RuntimeDescriptor,
}

impl OpenAICompatibleRuntime {
    pub fn new(config: OpenAICompatibleRuntimeConfig) -> Self {
        let descriptor = RuntimeDescriptor {
            id: format!("rig-openai-compatible:{}", config.model),
            display_name: format!("Rig / {}", config.model),
            kind: RuntimeKind::Provider,
            capabilities: RuntimeCapabilities {
                model_selection: true,
                session_resume: false,
                steering: false,
                tool_permissions: false,
            },
        };
        Self { config, descriptor }
    }
}

#[async_trait]
impl AgentRuntime for OpenAICompatibleRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let client = openai::CompletionsClient::builder()
            // Rig 0.40 requires an API-key builder value. An empty key preserves
            // compatibility with unauthenticated local OpenAI-compatible servers.
            .api_key(self.config.api_key.as_deref().unwrap_or_default())
            .base_url(&self.config.base_url)
            .build()
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let model = client.completion_model(&self.config.model);
        start_model_turn(
            model,
            request,
            control,
            self.config.max_output_tokens,
            self.config.supports_system_messages,
        )
        .await
    }
}

async fn start_model_turn<M>(
    model: M,
    request: TurnRequest,
    control: TurnControl,
    configured_max_output_tokens: Option<u64>,
    supports_system_messages: bool,
) -> Result<AgentEventStream, AgentError>
where
    M: CompletionModel + Send + Sync + 'static,
    M::StreamingResponse: Send + Sync + 'static,
{
    let runtime_request_id = Uuid::new_v4().to_string();
    let max_output_tokens = request.max_output_tokens.or(configured_max_output_tokens);
    let completion_request = build_completion_request(
        request,
        configured_max_output_tokens,
        supports_system_messages,
    )?;
    let stream_future = model.stream(completion_request).fuse();
    let initial_control = control.clone();
    let control_future = initial_control.receive().fuse();
    futures::pin_mut!(stream_future, control_future);

    let mut rig_stream = futures::select_biased! {
        command = control_future => match command {
            Ok(TurnCommand::Cancel) => {
                return Ok(stopped_before_stream(runtime_request_id));
            }
            Ok(TurnCommand::Steer { .. }) | Err(_) => {
                stream_future.await.map_err(map_completion_error)?
            }
        },
        result = stream_future => result.map_err(map_completion_error)?,
    };

    let events = async_stream::stream! {
        yield Ok(AgentEvent::TurnStarted {
            runtime_request_id,
        });

        let mut control_open = true;
        let mut last_output_tokens = 0;
        loop {
            let next_item = rig_stream.next().fuse();
            let next_command = if control_open {
                futures::future::Either::Left(control.receive())
            } else {
                futures::future::Either::Right(futures::future::pending())
            }
            .fuse();
            futures::pin_mut!(next_item, next_command);

            futures::select_biased! {
                command = next_command => {
                    match command {
                        Ok(TurnCommand::Cancel) => {
                            rig_stream.cancel();
                            yield Ok(AgentEvent::TurnStopped {
                                reason: StopReason::Cancelled,
                            });
                            return;
                        }
                        Ok(TurnCommand::Steer { .. }) => {
                            // Steering is not advertised by this runtime yet.
                        }
                        Err(_) => control_open = false,
                    }
                }
                item = next_item => {
                    let Some(item) = item else {
                        yield Ok(AgentEvent::TurnStopped {
                            reason: if max_output_tokens.is_some_and(|max| {
                                last_output_tokens >= max
                            }) {
                                StopReason::MaxTokens
                            } else {
                                StopReason::Completed
                            },
                        });
                        return;
                    };

                    match item {
                        Ok(StreamedAssistantContent::Text(text)) => {
                            if !text.text.is_empty() {
                                yield Ok(AgentEvent::TextDelta { text: text.text });
                            }
                        }
                        Ok(StreamedAssistantContent::Reasoning(reasoning)) => {
                            let text = reasoning.display_text();
                            if !text.is_empty() {
                                yield Ok(AgentEvent::ReasoningDelta { text });
                            }
                        }
                        Ok(StreamedAssistantContent::ReasoningDelta { reasoning, .. }) => {
                            if !reasoning.is_empty() {
                                yield Ok(AgentEvent::ReasoningDelta { text: reasoning });
                            }
                        }
                        Ok(StreamedAssistantContent::ToolCall { tool_call, .. }) => {
                            yield Ok(AgentEvent::Tool {
                                event: galaxy_agent_core::ToolEvent::Proposed {
                                    call: ToolCall {
                                    id: tool_call.id,
                                    name: tool_call.function.name,
                                    arguments: tool_call.function.arguments,
                                },
                                },
                            });
                        }
                        Ok(StreamedAssistantContent::ToolCallDelta { .. }) => {
                            // Rig emits a complete ToolCall after its deltas, which
                            // is the canonical event Galaxy consumes.
                        }
                        Ok(StreamedAssistantContent::Final(response)) => {
                            let mapped_usage = map_usage(response.token_usage());
                            last_output_tokens = mapped_usage.output_tokens;
                            yield Ok(AgentEvent::UsageUpdated {
                                usage: mapped_usage,
                            });
                        }
                        Ok(StreamedAssistantContent::Unknown(value)) => {
                            yield Err(AgentError::new(
                                AgentErrorKind::Protocol,
                                format!("Rig returned an unsupported provider event: {value}"),
                            ));
                            return;
                        }
                        Err(error) => {
                            yield Err(map_completion_error(error));
                            return;
                        }
                    }
                }
            }
        }
    };

    Ok(Box::pin(events))
}

fn stopped_before_stream(runtime_request_id: String) -> AgentEventStream {
    Box::pin(futures::stream::iter([
        Ok(AgentEvent::TurnStarted { runtime_request_id }),
        Ok(AgentEvent::TurnStopped {
            reason: StopReason::Cancelled,
        }),
    ]))
}

fn build_completion_request(
    request: TurnRequest,
    configured_max_output_tokens: Option<u64>,
    supports_system_messages: bool,
) -> Result<CompletionRequest, AgentError> {
    let mut messages = Vec::new();
    if let Some(system_prompt) = request.system_prompt {
        if supports_system_messages {
            messages.push(Message::System {
                content: system_prompt,
            });
        } else {
            messages.push(Message::User {
                content: OneOrMany::one(UserContent::text(system_prompt)),
            });
        }
    }
    for message in request.messages {
        messages.push(convert_message(message)?);
    }

    let chat_history = OneOrMany::many(messages).map_err(|_| {
        AgentError::new(
            AgentErrorKind::InvalidRequest,
            "a Rig turn requires at least one conversation message",
        )
    })?;

    Ok(CompletionRequest {
        model: Some(request.model.as_str().to_string()),
        preamble: None,
        chat_history,
        documents: Vec::new(),
        tools: request
            .tools
            .into_iter()
            .map(|tool| ToolDefinition {
                name: tool.name,
                description: tool.description,
                parameters: tool.input_schema,
            })
            .collect(),
        temperature: None,
        max_tokens: request.max_output_tokens.or(configured_max_output_tokens),
        tool_choice: None,
        additional_params: Some(serde_json::json!({
            "stream_options": { "include_usage": true }
        })),
        output_schema: None,
    })
}

fn convert_message(message: ConversationMessage) -> Result<Message, AgentError> {
    match message.role {
        MessageRole::User => Ok(Message::User {
            content: user_content(message.content)?,
        }),
        MessageRole::Assistant => Ok(Message::Assistant {
            id: None,
            content: assistant_content(message.content)?,
        }),
    }
}

fn user_content(content: MessageContent) -> Result<OneOrMany<UserContent>, AgentError> {
    let parts = match content {
        MessageContent::Text(text) => vec![UserContent::text(text)],
        MessageContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => vec![UserContent::tool_result(
            tool_use_id,
            OneOrMany::one(ToolResultContent::text(tool_result_text(content, is_error))),
        )],
        MessageContent::MultiPart(parts) => parts
            .into_iter()
            .map(convert_user_part)
            .collect::<Result<Vec<_>, _>>()?,
        MessageContent::ToolUse { .. } => {
            return Err(invalid_role("tool use", "user"));
        }
    };
    one_or_many(parts, "user")
}

fn assistant_content(content: MessageContent) -> Result<OneOrMany<AssistantContent>, AgentError> {
    let parts = match content {
        MessageContent::Text(text) => vec![AssistantContent::text(text)],
        MessageContent::ToolUse {
            tool_use_id,
            name,
            input,
        } => vec![AssistantContent::tool_call(tool_use_id, name, input)],
        MessageContent::MultiPart(parts) => parts
            .into_iter()
            .map(convert_assistant_part)
            .collect::<Result<Vec<_>, _>>()?,
        MessageContent::ToolResult { .. } => {
            return Err(invalid_role("tool result", "assistant"));
        }
    };
    one_or_many(parts, "assistant")
}

fn convert_user_part(part: ContentPart) -> Result<UserContent, AgentError> {
    match part {
        ContentPart::Text(text) => Ok(UserContent::text(text)),
        ContentPart::Image { data, mime_type } => Ok(UserContent::image_raw(
            data,
            ImageMediaType::from_mime_type(&mime_type),
            None,
        )),
        ContentPart::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Ok(UserContent::tool_result(
            tool_use_id,
            OneOrMany::one(ToolResultContent::text(tool_result_text(content, is_error))),
        )),
        ContentPart::ToolUse { .. } => Err(invalid_role("tool use", "user")),
    }
}

fn convert_assistant_part(part: ContentPart) -> Result<AssistantContent, AgentError> {
    match part {
        ContentPart::Text(text) => Ok(AssistantContent::text(text)),
        ContentPart::Image { data, mime_type } => Ok(AssistantContent::Image(Image {
            data: DocumentSourceKind::Raw(data),
            media_type: ImageMediaType::from_mime_type(&mime_type),
            detail: None,
            additional_params: None,
        })),
        ContentPart::ToolUse {
            tool_use_id,
            name,
            input,
        } => Ok(AssistantContent::tool_call(tool_use_id, name, input)),
        ContentPart::ToolResult { .. } => Err(invalid_role("tool result", "assistant")),
    }
}

fn tool_result_text(content: String, is_error: bool) -> String {
    if is_error {
        format!("[ERROR] {content}")
    } else {
        content
    }
}

fn one_or_many<T: Clone>(parts: Vec<T>, role: &str) -> Result<OneOrMany<T>, AgentError> {
    OneOrMany::many(parts).map_err(|_| {
        AgentError::new(
            AgentErrorKind::InvalidRequest,
            format!("{role} message has no content"),
        )
    })
}

fn invalid_role(content: &str, role: &str) -> AgentError {
    AgentError::new(
        AgentErrorKind::InvalidRequest,
        format!("{content} content cannot appear in a {role} message"),
    )
}

fn map_usage(usage: rig_core::completion::Usage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    }
}

fn map_completion_error(error: CompletionError) -> AgentError {
    let status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    let kind = match status {
        Some(401 | 403) => AgentErrorKind::Authentication,
        Some(429) => AgentErrorKind::RateLimited,
        Some(400 | 404 | 413 | 422) => AgentErrorKind::InvalidRequest,
        Some(500..=599) => AgentErrorKind::Provider,
        Some(_) => AgentErrorKind::Provider,
        None => match &error {
            CompletionError::HttpError(_)
            | CompletionError::UrlError(_)
            | CompletionError::RequestError(_) => AgentErrorKind::Transport,
            CompletionError::JsonError(_) | CompletionError::ResponseError(_) => {
                AgentErrorKind::Protocol
            }
            CompletionError::ProviderError(_) | CompletionError::ProviderResponse(_) => {
                AgentErrorKind::Provider
            }
            _ => AgentErrorKind::Provider,
        },
    };
    let mut mapped = AgentError::new(kind, error.to_string());
    mapped.recoverable = matches!(
        kind,
        AgentErrorKind::RateLimited | AgentErrorKind::Transport
    );
    mapped
}

#[cfg(test)]
#[path = "openai_compatible_tests.rs"]
mod tests;
