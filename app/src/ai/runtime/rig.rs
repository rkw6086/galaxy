use std::sync::Arc;

use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use galaxy_agent_core::{
    turn_control, AgentError, AgentEvent, AgentRuntime, MessageContent, MessageRole, StopReason,
    TurnCommand, TurnRequest, Usage,
};
use galaxy_agent_rig::{OpenAICompatibleRuntime, OpenAICompatibleRuntimeConfig};
use uuid::Uuid;
use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api, ClientAction, ResponseEvent};

use crate::ai::agent::api::{Event, RequestParams, ResponseStream};
use crate::ai::bedrock::response_translator::{
    build_add_agent_output_message, build_append_text, build_create_task, build_stream_init,
    build_user_query_message,
};
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::openai::response_translator::{build_stream_finished, StreamUsage};
use crate::ai::openai::translator::{prepare_turn, PreparedTurn, TranslatorRequest};
use crate::ai::provider::types::ConversationMessage;
use crate::server::server_api::AIApiError;

pub(crate) fn rig_openai_response_stream(
    config: OpenAIClientConfig,
    params: RequestParams,
    request: &mut api::Request,
    cancellation_rx: oneshot::Receiver<()>,
) -> ResponseStream {
    let translator_request = TranslatorRequest {
        config: config.clone(),
        model_id: params.model.as_str().to_string(),
        root_task_id: params.root_task_id,
        message_history: params.bedrock_message_history,
        tool_result_archive: params.bedrock_tool_result_archive,
        progressive_summary: params.bedrock_progressive_summary,
        messages_sent: params.bedrock_messages_sent,
        global_rules: params.global_rules,
    };
    let PreparedTurn {
        task_id,
        needs_create_task,
        user_query,
        messages,
        system_prompt,
        tools: _,
        model_id,
        persistent_message_count,
    } = prepare_turn(&translator_request, request);

    store_messages_sent(
        &translator_request.messages_sent,
        &messages,
        persistent_message_count,
    );

    let conversation_id = request
        .metadata
        .as_ref()
        .map(|metadata| metadata.conversation_id.clone())
        .filter(|id| !id.is_empty());
    let mut turn_request = TurnRequest::new(model_id.clone(), messages);
    turn_request.conversation_id = conversation_id.clone();
    turn_request.system_prompt = system_prompt;
    // Phase 2 deliberately validates the model streaming seam. Galaxy tool
    // execution moves behind AgentRuntime in Phase 3; exposing the legacy tool
    // list here would split ownership across both systems.
    turn_request.tools = Vec::new();
    turn_request.max_output_tokens = config.max_output_tokens.map(u64::from);

    let runtime = OpenAICompatibleRuntime::new(OpenAICompatibleRuntimeConfig {
        base_url: config.base_url,
        api_key: config.api_key,
        model: model_id.clone(),
        max_output_tokens: config.max_output_tokens.map(u64::from),
        supports_system_messages: config.supports_system_messages,
    });
    let messages_sent = translator_request.messages_sent;
    let max_context_tokens = config.max_input_tokens;
    let stream = async_stream::stream! {
        let (control_sender, control) = turn_control();
        let start_future = runtime.start_turn(turn_request, control).fuse();
        let cancel_future = cancellation_rx.fuse();
        futures::pin_mut!(start_future, cancel_future);

        let mut agent_events = futures::select_biased! {
            _ = cancel_future => {
                let _ = control_sender.try_send(TurnCommand::Cancel);
                match start_future.await {
                    Ok(stream) => stream,
                    Err(error) => {
                        yield Err(agent_error(error));
                        return;
                    }
                }
            }
            result = start_future => match result {
                Ok(stream) => stream,
                Err(error) => {
                    yield Err(agent_error(error));
                    return;
                }
            },
        };

        let request_id = Uuid::new_v4().to_string();
        let conversation_id = conversation_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut initialized = false;
        let mut current_text_message_id: Option<String> = None;
        let mut current_reasoning_message_id: Option<String> = None;
        let mut full_text = String::new();
        let mut usage = Usage::default();

        loop {
            let next_event = agent_events.next().fuse();
            futures::pin_mut!(next_event);
            futures::select_biased! {
                _ = cancel_future => {
                    let _ = control_sender.try_send(TurnCommand::Cancel);
                }
                event = next_event => {
                    let Some(event) = event else {
                        yield Err(Arc::new(AIApiError::UnexpectedEof));
                        return;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            yield Err(agent_error(error));
                            return;
                        }
                    };

                    match event {
                        AgentEvent::TurnStarted { .. } => {
                            initialized = true;
                            yield Ok(build_stream_init(&request_id, &conversation_id));
                            if needs_create_task {
                                yield Ok(build_create_task(&task_id));
                            }
                            if let Some(user_query) = &user_query {
                                yield Ok(build_user_query_message(&task_id, user_query));
                            }
                        }
                        AgentEvent::TextDelta { text } => {
                            full_text.push_str(&text);
                            if let Some(message_id) = &current_text_message_id {
                                yield Ok(build_append_text(&task_id, message_id, &text));
                            } else {
                                let message_id = Uuid::new_v4().to_string();
                                yield Ok(build_add_agent_output_message(&task_id, &message_id, &text));
                                current_text_message_id = Some(message_id);
                            }
                        }
                        AgentEvent::ReasoningDelta { text } => {
                            if let Some(message_id) = &current_reasoning_message_id {
                                yield Ok(build_append_reasoning(&task_id, message_id, &text));
                            } else {
                                let message_id = Uuid::new_v4().to_string();
                                yield Ok(build_add_reasoning(&task_id, &message_id, &text));
                                current_reasoning_message_id = Some(message_id);
                            }
                        }
                        AgentEvent::UsageUpdated { usage: updated } => usage = updated,
                        AgentEvent::TurnStopped { reason } => {
                            if !initialized {
                                yield Ok(build_stream_init(&request_id, &conversation_id));
                            }
                            store_assistant_text(&messages_sent, full_text);
                            yield Ok(build_stream_finished(
                                map_stop_reason(reason),
                                StreamUsage {
                                    input_tokens: saturating_i32(usage.input_tokens),
                                    output_tokens: saturating_i32(usage.output_tokens),
                                    cache_read_tokens: saturating_i32(usage.cached_input_tokens),
                                    cache_write_tokens: saturating_i32(
                                        usage.cache_creation_input_tokens,
                                    ),
                                    cost_in_cents: 0.0,
                                    model_id,
                                    max_context_tokens,
                                },
                            ));
                            return;
                        }
                        AgentEvent::ToolProposed { .. }
                        | AgentEvent::PermissionRequested { .. }
                        | AgentEvent::ToolStarted { .. }
                        | AgentEvent::ToolCompleted { .. } => {
                            yield Err(agent_error(AgentError::new(
                                galaxy_agent_core::AgentErrorKind::Protocol,
                                "the Phase 2 Rig runtime emitted a tool event while tools are disabled",
                            )));
                            return;
                        }
                    }
                }
            }
        }
    };

    Box::pin(stream)
}

fn store_messages_sent(
    messages_sent: &std::sync::Arc<std::sync::Mutex<Vec<ConversationMessage>>>,
    messages: &[ConversationMessage],
    persistent_message_count: usize,
) {
    let Ok(mut sent) = messages_sent.lock() else {
        return;
    };
    if persistent_message_count > 0 && messages.len() >= persistent_message_count {
        *sent = messages[messages.len() - persistent_message_count..].to_vec();
    } else {
        *sent = messages.to_vec();
    }
}

fn store_assistant_text(
    messages_sent: &std::sync::Arc<std::sync::Mutex<Vec<ConversationMessage>>>,
    text: String,
) {
    if text.is_empty() {
        return;
    }
    if let Ok(mut sent) = messages_sent.lock() {
        sent.push(ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text(text),
        });
    }
}

fn build_add_reasoning(task_id: &str, message_id: &str, text: &str) -> ResponseEvent {
    reasoning_action(task_id, message_id, text, false)
}

fn build_append_reasoning(task_id: &str, message_id: &str, text: &str) -> ResponseEvent {
    reasoning_action(task_id, message_id, text, true)
}

fn reasoning_action(task_id: &str, message_id: &str, text: &str, append: bool) -> ResponseEvent {
    let message = api::Message {
        id: message_id.to_string(),
        task_id: task_id.to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: Vec::new(),
        fetched_memories: Vec::new(),
        message: Some(api::message::Message::AgentReasoning(
            api::message::AgentReasoning {
                reasoning: text.to_string(),
                finished_duration: None,
            },
        )),
    };
    let action = if append {
        api::client_action::Action::AppendToMessageContent(
            api::client_action::AppendToMessageContent {
                task_id: task_id.to_string(),
                message: Some(message),
                mask: Some(prost_types::FieldMask {
                    paths: vec!["agent_reasoning.reasoning".to_string()],
                }),
            },
        )
    } else {
        api::client_action::Action::AddMessagesToTask(api::client_action::AddMessagesToTask {
            task_id: task_id.to_string(),
            messages: vec![message],
        })
    };
    ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![ClientAction {
                    action: Some(action),
                }],
            },
        )),
    }
}

fn map_stop_reason(reason: StopReason) -> stream_finished::Reason {
    match reason {
        StopReason::Completed => stream_finished::Reason::Done(stream_finished::Done {}),
        StopReason::MaxTokens => {
            stream_finished::Reason::MaxTokenLimit(stream_finished::ReachedMaxTokenLimit {})
        }
        StopReason::ContextWindowExceeded => stream_finished::Reason::ContextWindowExceeded(
            stream_finished::ContextWindowExceeded {},
        ),
        StopReason::Cancelled
        | StopReason::Refusal
        | StopReason::ToolLoopLimit
        | StopReason::Other(_) => stream_finished::Reason::Other(stream_finished::Other {}),
    }
}

fn saturating_i32(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn agent_error(error: AgentError) -> Arc<AIApiError> {
    Arc::new(
        AIApiError::Stream {
            stream_type: "rig_openai_compatible",
            source: anyhow::anyhow!(error),
        }
        .into_quota_limit_if_provider_budget_exhausted(),
    )
}

#[cfg(test)]
#[path = "rig_tests.rs"]
mod tests;
