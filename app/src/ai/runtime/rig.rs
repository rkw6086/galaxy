use std::collections::HashMap;
use std::sync::Arc;

use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use galaxy_agent_core::{
    turn_control, AgentError, AgentEvent, AgentRuntime, MessageContent, MessageRole, ToolCall,
    ToolCallDecision, ToolEvent, ToolPolicy, ToolResult, TurnCommand,
};
use galaxy_agent_rig::{OpenAICompatibleRuntime, OpenAICompatibleRuntimeConfig};
use uuid::Uuid;
use warp_multi_agent_api::ToolType;

use super::rig_request::{
    prepare_bedrock_rig_turn, prepare_rig_turn, MCPToolTarget, PreparedRigTurn,
};
use super::rig_tool::action_from_tool_call;
use crate::ai::agent::api::{Event, RequestParams, ResponseStream, StreamEvent};
use crate::ai::agent::AIAgentAction;
use crate::ai::bedrock::client::{BedrockClient, BedrockClientConfig};
use crate::ai::bedrock::external_config::ExternalBedrockConfig;
use crate::ai::bedrock::response_translator::build_add_agent_output_message;
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::provider::types::{ContentPart, ConversationMessage};
use crate::ai::runtime::{RuntimeResponseConfig, RuntimeResponseTranslator};
use crate::server::server_api::AIApiError;

pub(crate) fn rig_openai_response_stream(
    config: OpenAIClientConfig,
    params: RequestParams,
    supported_tools: Vec<ToolType>,
    supported_cli_agent_tools: Vec<ToolType>,
    cancellation_rx: oneshot::Receiver<()>,
) -> ResponseStream {
    let skill_path_origin = params.session_context.skill_path_origin();
    let prepared = prepare_rig_turn(&config, params, supported_tools, supported_cli_agent_tools);
    let model_id = prepared.request.model.as_str().to_string();
    let runtime = OpenAICompatibleRuntime::new(OpenAICompatibleRuntimeConfig {
        base_url: config.base_url,
        api_key: config.api_key,
        model: model_id.clone(),
        max_output_tokens: config.max_output_tokens.map(u64::from),
        supports_system_messages: config.supports_system_messages,
    });
    rig_response_stream(
        runtime,
        prepared,
        skill_path_origin,
        config.max_input_tokens,
        "rig_openai_compatible",
        cancellation_rx,
    )
}

pub(crate) async fn rig_bedrock_response_stream(
    config: BedrockClientConfig,
    params: RequestParams,
    supported_tools: Vec<ToolType>,
    supported_cli_agent_tools: Vec<ToolType>,
    cancellation_rx: oneshot::Receiver<()>,
) -> anyhow::Result<ResponseStream> {
    let skill_path_origin = params.session_context.skill_path_origin();
    let max_context_tokens = params.context_window_limit;
    let model = params.model.as_str().to_string();
    let max_output_tokens = Some(64_000);
    let cross_region_inference = config.cross_region_inference;
    let external_config = ExternalBedrockConfig::load();
    let prompt_caching = !external_config.disable_prompt_caching;
    let client = BedrockClient::from_config(config).await?;
    let runtime = client.rig_runtime(
        model.clone(),
        cross_region_inference,
        prompt_caching,
        max_output_tokens,
    )?;
    let prepared = prepare_bedrock_rig_turn(
        model,
        max_output_tokens,
        params,
        supported_tools,
        supported_cli_agent_tools,
    );

    Ok(rig_response_stream(
        runtime,
        prepared,
        skill_path_origin,
        max_context_tokens,
        "rig_bedrock",
        cancellation_rx,
    ))
}

fn rig_response_stream<R>(
    runtime: R,
    prepared: PreparedRigTurn,
    skill_path_origin: ai::skills::SkillPathOrigin,
    max_context_tokens: Option<u32>,
    stream_type: &'static str,
    cancellation_rx: oneshot::Receiver<()>,
) -> ResponseStream
where
    R: AgentRuntime + Send + Sync + 'static,
{
    let runtime_capabilities = runtime.descriptor().capabilities.clone();
    let PreparedRigTurn {
        task_id,
        needs_create_task,
        user_query,
        request: turn_request,
        persistent_messages,
        tool_result_archive,
        messages_sent,
        mcp_tool_aliases,
    } = prepared;
    store_messages_sent(&messages_sent, &persistent_messages);

    let conversation_id = turn_request.conversation_id.clone();
    let model_id = turn_request.model.as_str().to_string();
    let tool_policy = ToolPolicy::new(&turn_request.tools);
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
                        yield Err(agent_error(error, stream_type));
                        return;
                    }
                }
            }
            result = start_future => match result {
                Ok(stream) => stream,
                Err(error) => {
                    yield Err(agent_error(error, stream_type));
                    return;
                }
            },
        };

        let conversation_id = conversation_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut translator = RuntimeResponseTranslator::new(RuntimeResponseConfig {
            task_id: task_id.clone(),
            conversation_id,
            needs_create_task,
            user_query,
            model_id,
            max_context_tokens,
            capabilities: runtime_capabilities,
            empty_output_message: None,
        });
        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut reasoning_signature = None;
        let mut proposed_tools = Vec::new();
        let mut assistant_history_index = None;

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
                            yield Err(agent_error(error, stream_type));
                            return;
                        }
                    };

                    match event {
                        AgentEvent::Tool {
                            event: ToolEvent::Proposed { call },
                        } => {
                            proposed_tools.push(call.clone());
                            sync_assistant_turn(
                                &messages_sent,
                                &full_reasoning,
                                reasoning_signature.as_deref(),
                                &full_text,
                                &proposed_tools,
                                &mut assistant_history_index,
                            );
                            let history = messages_sent
                                .lock()
                                .map(|sent| sent.clone())
                                .unwrap_or_default();
                            match tool_policy.decide(&call, &history, &tool_result_archive) {
                                ToolCallDecision::Execute => {
                                    match build_tool_proposed(
                                        &task_id,
                                        &call,
                                        &skill_path_origin,
                                        &mcp_tool_aliases,
                                    ) {
                                        Ok(action) => yield Ok(StreamEvent::ToolProposed(action)),
                                        Err(message) => {
                                            yield Err(agent_error(AgentError::new(
                                                galaxy_agent_core::AgentErrorKind::Protocol,
                                                message,
                                            ), stream_type));
                                            return;
                                        }
                                    }
                                }
                                ToolCallDecision::Inline(result) => {
                                    append_tool_result(&messages_sent, result);
                                }
                                ToolCallDecision::Reject(result) => {
                                    log::warn!(
                                        "Rig model called unavailable tool '{}' (id={})",
                                        call.name,
                                        call.id
                                    );
                                    let error_display = format!(
                                        "Failed tool call: `{}`\n\n{}",
                                        call.name, result.content
                                    );
                                    append_tool_result(&messages_sent, result);
                                    let message_id = Uuid::new_v4().to_string();
                                    yield Ok(StreamEvent::Response(build_add_agent_output_message(
                                        &task_id,
                                        &message_id,
                                        &error_display,
                                    )));
                                }
                            }
                        }
                        AgentEvent::TurnStopped { reason } => {
                            sync_assistant_turn(
                                &messages_sent,
                                &full_reasoning,
                                reasoning_signature.as_deref(),
                                &full_text,
                                &proposed_tools,
                                &mut assistant_history_index,
                            );
                            let response_events = match translator
                                .translate(AgentEvent::TurnStopped { reason })
                            {
                                Ok(response_events) => response_events,
                                Err(message) => {
                                    yield Err(agent_error(AgentError::new(
                                        galaxy_agent_core::AgentErrorKind::Protocol,
                                        message,
                                    ), stream_type));
                                    return;
                                }
                            };
                            for response_event in response_events {
                                yield Ok(StreamEvent::Response(response_event));
                            }
                            return;
                        }
                        event => {
                            match &event {
                                AgentEvent::TextDelta { text } => full_text.push_str(text),
                                AgentEvent::ReasoningDelta { text } => {
                                    full_reasoning.push_str(text);
                                }
                                AgentEvent::ReasoningCompleted { text, signature } => {
                                    if !text.is_empty() {
                                        full_reasoning.clone_from(text);
                                    }
                                    reasoning_signature.clone_from(signature);
                                }
                                AgentEvent::TurnStarted { .. }
                                | AgentEvent::Tool { .. }
                                | AgentEvent::UsageUpdated { .. }
                                | AgentEvent::RuntimeActivityUpdated { .. }
                                | AgentEvent::ContextUsageUpdated { .. }
                                | AgentEvent::UserInputAccepted { .. }
                                | AgentEvent::RuntimeNotice { .. }
                                | AgentEvent::TurnStopped { .. } => {}
                            }
                            let response_events = match translator.translate(event) {
                                Ok(response_events) => response_events,
                                Err(message) => {
                                    yield Err(agent_error(AgentError::new(
                                        galaxy_agent_core::AgentErrorKind::Protocol,
                                        message,
                                    ), stream_type));
                                    return;
                                }
                            };
                            for response_event in response_events {
                                yield Ok(StreamEvent::Response(response_event));
                            }
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
) {
    let Ok(mut sent) = messages_sent.lock() else {
        return;
    };
    *sent = messages.to_vec();
}

fn append_tool_result(
    messages_sent: &std::sync::Arc<std::sync::Mutex<Vec<ConversationMessage>>>,
    result: ToolResult,
) {
    let is_error = result.is_error();
    let message = ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: result.call_id,
            content: result.content,
            is_error,
        },
    };
    if let Ok(mut sent) = messages_sent.lock() {
        sent.push(message);
    }
}

fn sync_assistant_turn(
    messages_sent: &std::sync::Arc<std::sync::Mutex<Vec<ConversationMessage>>>,
    reasoning_text: &str,
    reasoning_signature: Option<&str>,
    text: &str,
    tool_calls: &[ToolCall],
    history_index: &mut Option<usize>,
) {
    let has_reasoning = !reasoning_text.is_empty() || reasoning_signature.is_some();
    let mut parts = Vec::with_capacity(
        usize::from(has_reasoning) + usize::from(!text.is_empty()) + tool_calls.len(),
    );
    if has_reasoning {
        parts.push(ContentPart::Reasoning {
            text: reasoning_text.to_string(),
            signature: reasoning_signature.map(str::to_string),
        });
    }
    if !text.is_empty() {
        parts.push(ContentPart::Text(text.to_string()));
    }
    parts.extend(tool_calls.iter().map(|call| ContentPart::ToolUse {
        tool_use_id: call.id.clone(),
        name: call.name.clone(),
        input: call.arguments.clone(),
    }));
    if parts.is_empty() {
        return;
    }

    let content = if parts.len() == 1 {
        match parts.pop().unwrap() {
            ContentPart::Text(text) => MessageContent::Text(text),
            ContentPart::ToolUse {
                tool_use_id,
                name,
                input,
            } => MessageContent::ToolUse {
                tool_use_id,
                name,
                input,
            },
            reasoning @ ContentPart::Reasoning { .. } => MessageContent::MultiPart(vec![reasoning]),
            ContentPart::Image { .. } | ContentPart::ToolResult { .. } => unreachable!(),
        }
    } else {
        MessageContent::MultiPart(parts)
    };
    let message = ConversationMessage {
        role: MessageRole::Assistant,
        content,
    };

    let Ok(mut sent) = messages_sent.lock() else {
        return;
    };
    if let Some(index) = *history_index {
        if index < sent.len() {
            sent[index] = message;
            return;
        }
    }
    *history_index = Some(sent.len());
    sent.push(message);
}

fn build_tool_proposed(
    task_id: &str,
    call: &ToolCall,
    skill_path_origin: &ai::skills::SkillPathOrigin,
    mcp_tool_aliases: &HashMap<String, MCPToolTarget>,
) -> Result<AIAgentAction, String> {
    action_from_tool_call(task_id, call, skill_path_origin, mcp_tool_aliases)
}

fn agent_error(error: AgentError, stream_type: &'static str) -> Arc<AIApiError> {
    Arc::new(
        AIApiError::Stream {
            stream_type,
            source: anyhow::anyhow!(error),
        }
        .into_quota_limit_if_provider_budget_exhausted(),
    )
}

#[cfg(test)]
#[path = "rig_tests.rs"]
mod tests;
