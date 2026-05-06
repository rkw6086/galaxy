use std::sync::Arc;

use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput;
use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockStart, ConverseStreamOutput as StreamEvent,
    ReasoningContentBlockDelta, StopReason,
};
use futures::stream::BoxStream;
use uuid::Uuid;
use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api, ClientAction, ResponseEvent};

use crate::ai::agent::api::Event;
use crate::server::server_api::AIApiError;

pub fn bedrock_stream_to_response_events(
    mut output: ConverseStreamOutput,
    task_id: String,
) -> BoxStream<'static, Event> {
    let request_id = Uuid::new_v4().to_string();
    let conversation_id = Uuid::new_v4().to_string();

    let stream = async_stream::stream! {
        log::info!("[bedrock] Stream started: task_id={task_id}, request_id={request_id}");
        let init_event = build_stream_init(&request_id, &conversation_id);
        yield Ok(init_event);

        let mut current_text_message_id: Option<String> = None;
        let mut current_tool_use_id = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_input_json = String::new();
        let mut input_tokens: i32 = 0;
        let mut output_tokens: i32 = 0;
        let mut stop_reason = stream_finished::Reason::Done(stream_finished::Done {});

        loop {
            match output.stream.recv().await {
                Ok(Some(event)) => match event {
                    StreamEvent::MessageStart(_) => {}
                    StreamEvent::ContentBlockStart(block_start) => {
                        if let Some(start) = block_start.start() {
                            match start {
                                ContentBlockStart::ToolUse(tool_start) => {
                                    current_tool_use_id = tool_start.tool_use_id().to_string();
                                    current_tool_name = tool_start.name().to_string();
                                    current_tool_input_json.clear();
                                }
                                _ => {}
                            }
                        }
                    }
                    StreamEvent::ContentBlockDelta(delta) => {
                        if let Some(d) = delta.delta() {
                            match d {
                                ContentBlockDelta::Text(text) => {
                                    log::trace!("[bedrock] Text delta ({} chars): {:?}", text.len(), &text[..text.len().min(100)]);
                                    if current_text_message_id.is_none() {
                                        let msg_id = Uuid::new_v4().to_string();
                                        current_text_message_id = Some(msg_id.clone());
                                        log::debug!("[bedrock] First text chunk, creating message msg_id={msg_id}");
                                        let add_msg = build_add_agent_output_message(
                                            &task_id,
                                            &msg_id,
                                            text,
                                        );
                                        yield Ok(add_msg);
                                    } else {
                                        let msg_id = current_text_message_id.as_ref().unwrap();
                                        let append = build_append_text(
                                            &task_id,
                                            msg_id,
                                            text,
                                        );
                                        yield Ok(append);
                                    }
                                }
                                ContentBlockDelta::ReasoningContent(reasoning) => {
                                    if let ReasoningContentBlockDelta::Text(text) = reasoning {
                                        log::trace!("[bedrock] Reasoning delta ({} chars)", text.len());
                                        if current_text_message_id.is_none() {
                                            let msg_id = Uuid::new_v4().to_string();
                                            current_text_message_id = Some(msg_id.clone());
                                            log::debug!("[bedrock] First reasoning chunk, creating message msg_id={msg_id}");
                                            let add_msg = build_add_agent_output_message(
                                                &task_id,
                                                &msg_id,
                                                text,
                                            );
                                            yield Ok(add_msg);
                                        } else {
                                            let msg_id = current_text_message_id.as_ref().unwrap();
                                            let append = build_append_text(
                                                &task_id,
                                                msg_id,
                                                text,
                                            );
                                            yield Ok(append);
                                        }
                                    }
                                }
                                ContentBlockDelta::ToolUse(tool_delta) => {
                                    current_tool_input_json.push_str(tool_delta.input());
                                }
                                _ => {}
                            }
                        }
                    }
                    StreamEvent::ContentBlockStop(_) => {
                        if !current_tool_use_id.is_empty() {
                            log::debug!("[bedrock] Tool call complete: {} ({})", current_tool_name, current_tool_use_id);
                            let tool_msg = build_tool_call_message(
                                &task_id,
                                &current_tool_use_id,
                                &current_tool_name,
                                &current_tool_input_json,
                            );
                            yield Ok(tool_msg);
                            current_tool_use_id.clear();
                            current_tool_name.clear();
                            current_tool_input_json.clear();
                        }
                    }
                    StreamEvent::MessageStop(stop) => {
                        stop_reason = match stop.stop_reason() {
                            StopReason::EndTurn => {
                                stream_finished::Reason::Done(stream_finished::Done {})
                            }
                            StopReason::MaxTokens => {
                                stream_finished::Reason::MaxTokenLimit(
                                    stream_finished::ReachedMaxTokenLimit {},
                                )
                            }
                            StopReason::ToolUse => {
                                stream_finished::Reason::Done(stream_finished::Done {})
                            }
                            _ => stream_finished::Reason::Other(stream_finished::Other {}),
                        };
                    }
                    StreamEvent::Metadata(metadata) => {
                        if let Some(usage) = metadata.usage() {
                            input_tokens = usage.input_tokens();
                            output_tokens = usage.output_tokens();
                        }
                    }
                    _ => {}
                },
                Ok(None) => {
                    log::info!("[bedrock] Stream ended normally");
                    break;
                }
                Err(e) => {
                    log::error!("[bedrock] Stream error: {e}");
                    yield Err(Arc::new(AIApiError::Stream {
                        stream_type: "bedrock_converse",
                        source: anyhow::anyhow!("Bedrock stream error: {}", e),
                    }));
                    return;
                }
            }
        }

        log::info!("[bedrock] Stream finished: input_tokens={input_tokens}, output_tokens={output_tokens}");
        let finished_event = build_stream_finished(stop_reason, input_tokens, output_tokens);
        yield Ok(finished_event);
    };

    Box::pin(stream)
}

pub(super) fn build_stream_init(request_id: &str, conversation_id: &str) -> ResponseEvent {
    ResponseEvent {
        r#type: Some(api::response_event::Type::Init(
            api::response_event::StreamInit {
                conversation_id: conversation_id.to_string(),
                request_id: request_id.to_string(),
                run_id: String::new(),
            },
        )),
    }
}

pub(super) fn build_stream_finished(
    reason: stream_finished::Reason,
    input_tokens: i32,
    output_tokens: i32,
) -> ResponseEvent {
    let total_tokens = (input_tokens + output_tokens) as u32;

    let mut byok_token_usage = std::collections::HashMap::new();
    if total_tokens > 0 {
        #[allow(deprecated)]
        byok_token_usage.insert(
            "bedrock".to_string(),
            stream_finished::ModelTokenUsage {
                model_id: String::new(),
                total_tokens,
                token_usage_by_category: std::collections::HashMap::new(),
            },
        );
    }

    #[allow(deprecated)]
    let conversation_usage_metadata = Some(stream_finished::ConversationUsageMetadata {
        context_window_usage: 0.0,
        summarized: false,
        credits_spent: 0.0,
        token_usage: vec![],
        tool_usage_metadata: None,
        warp_token_usage: std::collections::HashMap::new(),
        byok_token_usage,
    });

    ResponseEvent {
        r#type: Some(api::response_event::Type::Finished(
            api::response_event::StreamFinished {
                reason: Some(reason),
                token_usage: vec![],
                should_refresh_model_config: false,
                request_cost: None,
                conversation_usage_metadata,
            },
        )),
    }
}

fn build_add_agent_output_message(
    task_id: &str,
    message_id: &str,
    initial_text: &str,
) -> ResponseEvent {
    let message = api::Message {
        id: message_id.to_string(),
        task_id: task_id.to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: initial_text.to_string(),
            },
        )),
    };

    let action = ClientAction {
        action: Some(api::client_action::Action::AddMessagesToTask(
            api::client_action::AddMessagesToTask {
                task_id: task_id.to_string(),
                messages: vec![message],
            },
        )),
    };

    ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![action],
            },
        )),
    }
}

fn build_append_text(task_id: &str, message_id: &str, text_delta: &str) -> ResponseEvent {
    let message = api::Message {
        id: message_id.to_string(),
        task_id: task_id.to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: text_delta.to_string(),
            },
        )),
    };

    let mask = prost_types::FieldMask {
        paths: vec!["message.agent_output.text".to_string()],
    };

    let action = ClientAction {
        action: Some(api::client_action::Action::AppendToMessageContent(
            api::client_action::AppendToMessageContent {
                task_id: task_id.to_string(),
                message: Some(message),
                mask: Some(mask),
            },
        )),
    };

    ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![action],
            },
        )),
    }
}

fn build_tool_call_message(
    task_id: &str,
    tool_use_id: &str,
    tool_name: &str,
    tool_input_json: &str,
) -> ResponseEvent {
    let _tool_use_id = tool_use_id.to_string();
    let _tool_name = tool_name.to_string();
    let _tool_input_json = tool_input_json.to_string();

    let message = api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: format!("[Tool call: {} ({})]", _tool_name, _tool_use_id),
            },
        )),
    };

    let action = ClientAction {
        action: Some(api::client_action::Action::AddMessagesToTask(
            api::client_action::AddMessagesToTask {
                task_id: task_id.to_string(),
                messages: vec![message],
            },
        )),
    };

    ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![action],
            },
        )),
    }
}


