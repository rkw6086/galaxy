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

use super::diagnostic::BedrockDiagnosticLogger;

pub fn bedrock_stream_to_response_events(
    mut output: ConverseStreamOutput,
    task_id: String,
    needs_create_task: bool,
    diagnostic_logger: Option<Arc<BedrockDiagnosticLogger>>,
) -> BoxStream<'static, Event> {
    let request_id = Uuid::new_v4().to_string();
    let conversation_id = Uuid::new_v4().to_string();

    if let Some(ref logger) = diagnostic_logger {
        logger.set_ids(&conversation_id, &request_id);
    }

    let stream = async_stream::stream! {
        log::info!("[bedrock] Stream started: task_id={task_id}, request_id={request_id}, needs_create_task={needs_create_task}");

        if let Some(ref logger) = diagnostic_logger {
            logger.log_stream_event(&format!("StreamInit: request_id={request_id}, conversation_id={conversation_id}"));
        }

        let init_event = build_stream_init(&request_id, &conversation_id);
        yield Ok(init_event);

        if needs_create_task {
            log::debug!("[bedrock] Emitting CreateTask to upgrade optimistic root task");
            let create_task_event = build_create_task(&task_id);
            yield Ok(create_task_event);
        }

        let mut current_text_message_id: Option<String> = None;
        let mut buffered_text = String::new();
        let mut text_flushed = false;
        let mut current_tool_use_id = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_input_json = String::new();
        let mut _has_tool_calls = false;
        let mut input_tokens: i32 = 0;
        let mut output_tokens: i32 = 0;
        let mut stop_reason = stream_finished::Reason::Done(stream_finished::Done {});

        let mut event_count: u32 = 0;
        loop {
            match output.stream.recv().await {
                Ok(Some(event)) => {
                    event_count += 1;
                    match event {
                    StreamEvent::MessageStart(_) => {
                        log::info!("[bedrock-debug] Event #{event_count}: MessageStart");
                    }
                    StreamEvent::ContentBlockStart(block_start) => {
                        log::info!("[bedrock-debug] Event #{event_count}: ContentBlockStart");
                        if let Some(start) = block_start.start() {
                            match start {
                                ContentBlockStart::ToolUse(tool_start) => {
                                    _has_tool_calls = true;
                                    if !buffered_text.is_empty() {
                                        let msg_id = current_text_message_id
                                            .clone()
                                            .unwrap_or_else(|| Uuid::new_v4().to_string());
                                        if !text_flushed {
                                            current_text_message_id = Some(msg_id.clone());
                                            text_flushed = true;
                                            log::debug!("[bedrock] Flushing buffered text ({} chars) before tool call", buffered_text.len());
                                            let add_msg = build_add_agent_output_message(
                                                &task_id,
                                                &msg_id,
                                                &buffered_text,
                                            );
                                            yield Ok(add_msg);
                                        } else {
                                            log::debug!("[bedrock] Flushing remaining buffered text ({} chars) as append before tool call", buffered_text.len());
                                            let append = build_append_text(
                                                &task_id,
                                                &msg_id,
                                                &buffered_text,
                                            );
                                            yield Ok(append);
                                        }
                                        buffered_text.clear();
                                    }
                                    current_tool_use_id = tool_start.tool_use_id().to_string();
                                    current_tool_name = tool_start.name().to_string();
                                    current_tool_input_json.clear();
                                }
                                _ => {}
                            }
                        }
                    }
                    StreamEvent::ContentBlockDelta(delta) => {
                        log::info!("[bedrock-debug] Event #{event_count}: ContentBlockDelta");
                        if let Some(d) = delta.delta() {
                            match d {
                                ContentBlockDelta::Text(text) => {
                                    log::info!("[bedrock-debug] Event #{event_count}: TextDelta ({} chars): {:?}", text.len(), &text[..text.len().min(80)]);
                                    if text_flushed {
                                        let msg_id = current_text_message_id.as_ref().unwrap();
                                        let append = build_append_text(
                                            &task_id,
                                            msg_id,
                                            text,
                                        );
                                        yield Ok(append);
                                    } else {
                                        buffered_text.push_str(text);
                                        if buffered_text.len() >= 1 {
                                            let msg_id = Uuid::new_v4().to_string();
                                            current_text_message_id = Some(msg_id.clone());
                                            text_flushed = true;
                                            let add_msg = build_add_agent_output_message(
                                                &task_id,
                                                &msg_id,
                                                &buffered_text,
                                            );
                                            yield Ok(add_msg);
                                            buffered_text.clear();
                                        }
                                    }
                                }
                                ContentBlockDelta::ReasoningContent(reasoning) => {
                                    if let ReasoningContentBlockDelta::Text(text) = reasoning {
                                        log::trace!("[bedrock] Reasoning delta ({} chars) - not displayed to user", text.len());
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
                        log::info!("[bedrock-debug] Event #{event_count}: ContentBlockStop (tool_use_id={:?})", if current_tool_use_id.is_empty() { "none" } else { &current_tool_use_id });
                        if !current_tool_use_id.is_empty() {
                            log::debug!("[bedrock] Tool call complete: {} ({})", current_tool_name, current_tool_use_id);
                            if let Some(ref logger) = diagnostic_logger {
                                logger.log_stream_event(&format!(
                                    "ToolCall: name={}, id={}, input={}",
                                    current_tool_name, current_tool_use_id, current_tool_input_json
                                ));
                            }
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
                        log::info!("[bedrock-debug] Event #{event_count}: MessageStop (reason={:?})", stop.stop_reason());
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
                        log::info!("[bedrock-debug] Event #{event_count}: Metadata");
                        if let Some(usage) = metadata.usage() {
                            input_tokens = usage.input_tokens();
                            output_tokens = usage.output_tokens();
                        }
                    }
                    _ => {
                        log::info!("[bedrock-debug] Event #{event_count}: Unknown/Other event");
                    }
                }
                }
                Ok(None) => {
                    log::info!("[bedrock-debug] Stream ended normally after {event_count} events");
                    break;
                }
                Err(e) => {
                    log::error!("[bedrock-debug] Stream error after {event_count} events: {e}");
                    if let Some(ref logger) = diagnostic_logger {
                        logger.log_stream_error(&format!("{e}"));
                    }
                    if !buffered_text.is_empty() {
                        let msg_id = current_text_message_id
                            .clone()
                            .unwrap_or_else(|| Uuid::new_v4().to_string());
                        if !text_flushed {
                            let add_msg = build_add_agent_output_message(&task_id, &msg_id, &buffered_text);
                            yield Ok(add_msg);
                        } else {
                            let append = build_append_text(&task_id, &msg_id, &buffered_text);
                            yield Ok(append);
                        }
                        buffered_text.clear();
                    }
                    yield Err(Arc::new(AIApiError::Stream {
                        stream_type: "bedrock_converse",
                        source: anyhow::anyhow!("Bedrock stream error: {}", e),
                    }));
                    return;
                }
            }
        }

        if !buffered_text.is_empty() {
            let msg_id = current_text_message_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            log::debug!("[bedrock] Flushing remaining buffered text ({} chars) at stream end", buffered_text.len());
            if !text_flushed {
                let add_msg = build_add_agent_output_message(&task_id, &msg_id, &buffered_text);
                yield Ok(add_msg);
            } else {
                let append = build_append_text(&task_id, &msg_id, &buffered_text);
                yield Ok(append);
            }
        }

        log::info!("[bedrock] Stream finished: input_tokens={input_tokens}, output_tokens={output_tokens}");
        if let Some(ref logger) = diagnostic_logger {
            let stop_reason_str = match &stop_reason {
                stream_finished::Reason::Done(_) => "EndTurn",
                stream_finished::Reason::MaxTokenLimit(_) => "MaxTokens",
                _ => "Other",
            };
            logger.log_result_success(input_tokens, output_tokens, stop_reason_str);
        }
        let finished_event = build_stream_finished(stop_reason, input_tokens, output_tokens);
        yield Ok(finished_event);
    };

    Box::pin(stream)
}

pub(crate) fn build_create_task(task_id: &str) -> ResponseEvent {
    let task = api::Task {
        id: task_id.to_string(),
        description: String::new(),
        dependencies: None,
        messages: vec![],
        summary: String::new(),
        server_data: String::new(),
    };

    let action = ClientAction {
        action: Some(api::client_action::Action::CreateTask(
            api::client_action::CreateTask { task: Some(task) },
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
        paths: vec!["agent_output.text".to_string()],
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
    let input: serde_json::Value =
        serde_json::from_str(tool_input_json).unwrap_or(serde_json::json!({}));

    let tool = match tool_name {
        "run_shell_command" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(api::message::tool_call::Tool::RunShellCommand(
                api::message::tool_call::RunShellCommand {
                    command,
                    is_read_only: false,
                    uses_pager: false,
                    citations: vec![],
                    is_risky: false,
                    risk_category: 0,
                    wait_until_complete_value: None,
                },
            ))
        }
        "read_files" => {
            let files = input
                .get("files")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| f.as_str())
                        .map(|name| api::message::tool_call::read_files::File {
                            name: name.to_string(),
                            line_ranges: vec![],
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(api::message::tool_call::Tool::ReadFiles(
                api::message::tool_call::ReadFiles { files },
            ))
        }
        "apply_file_diffs" => {
            let diffs = input
                .get("diffs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            Some(api::message::tool_call::apply_file_diffs::FileDiff {
                                file_path: d.get("file_path")?.as_str()?.to_string(),
                                search: d
                                    .get("search")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                replace: d
                                    .get("replace")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(api::message::tool_call::Tool::ApplyFileDiffs(
                api::message::tool_call::ApplyFileDiffs {
                    summary: String::new(),
                    diffs,
                    new_files: vec![],
                    deleted_files: vec![],
                    v4a_updates: vec![],
                },
            ))
        }
        "grep" => {
            let queries = input
                .get("queries")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|q| q.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(api::message::tool_call::Tool::Grep(
                api::message::tool_call::Grep { queries, path },
            ))
        }
        "file_glob" => {
            let patterns = input
                .get("patterns")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            #[allow(deprecated)]
            Some(api::message::tool_call::Tool::FileGlob(
                api::message::tool_call::FileGlob {
                    patterns,
                    path: String::new(),
                },
            ))
        }
        _ => {
            log::warn!("[bedrock] Unknown tool name: {tool_name}, emitting as text");
            None
        }
    };

    let message = if let Some(tool_variant) = tool {
        api::Message {
            id: tool_use_id.to_string(),
            task_id: task_id.to_string(),
            request_id: String::new(),
            timestamp: None,
            server_message_data: String::new(),
            citations: vec![],
            message: Some(api::message::Message::ToolCall(api::message::ToolCall {
                tool_call_id: tool_use_id.to_string(),
                tool: Some(tool_variant),
            })),
        }
    } else {
        api::Message {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            request_id: String::new(),
            timestamp: None,
            server_message_data: String::new(),
            citations: vec![],
            message: Some(api::message::Message::AgentOutput(
                api::message::AgentOutput {
                    text: format!(
                        "[Tool call: {} ({})]\nInput: {}",
                        tool_name, tool_use_id, tool_input_json
                    ),
                },
            )),
        }
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
