use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::Stream;
use serde_json::Value as JsonValue;
use uuid::Uuid;
use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api, ClientAction, ResponseEvent};

use crate::ai::agent::api::Event;
use crate::ai::bedrock::response_translator::{
    build_create_task, build_stream_init, context_window_for_model,
};
use crate::ai::provider::types::{ContentPart, ConversationMessage, MessageContent, MessageRole};
use crate::server::server_api::AIApiError;

struct ToolCallAccumulator {
    #[allow(dead_code)]
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

pub fn openai_stream_to_response_events(
    byte_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    task_id: String,
    needs_create_task: bool,
    user_query: Option<String>,
    messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
    model_id: String,
    _tool_result_archive: Vec<ConversationMessage>,
) -> BoxStream<'static, Event> {
    use futures::StreamExt;

    let request_id = Uuid::new_v4().to_string();
    let conversation_id = Uuid::new_v4().to_string();

    let stream = async_stream::stream! {
        log::info!("[openai] Stream started: task_id={task_id}, request_id={request_id}");

        let init_event = build_stream_init(&request_id, &conversation_id);
        yield Ok(init_event);

        if needs_create_task {
            let create_task_event = build_create_task(&task_id);
            yield Ok(create_task_event);
        }

        if let Some(ref query_text) = user_query {
            let user_query_msg = build_user_query_message(&task_id, query_text);
            yield Ok(user_query_msg);
        }

        let mut current_text_message_id: Option<String> = None;
        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCallAccumulator> = Vec::new();
        let mut input_tokens: i32 = 0;
        let mut output_tokens: i32 = 0;
        let mut stop_reason = stream_finished::Reason::Done(api::response_event::stream_finished::Done {});
        let mut line_buffer = String::new();

        futures::pin_mut!(byte_stream);

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = match chunk_result {
                Ok(bytes) => bytes,
                Err(e) => {
                    log::error!("[openai] Stream chunk error: {e}");
                    yield Err(Arc::new(AIApiError::Stream {
                        stream_type: "openai_chat_completions",
                        source: anyhow::anyhow!("{e}"),
                    }));
                    return;
                }
            };

            let chunk_str = String::from_utf8_lossy(&chunk);
            line_buffer.push_str(&chunk_str);

            // Process complete SSE lines
            while let Some(line_end) = line_buffer.find('\n') {
                let line = line_buffer[..line_end].trim_end_matches('\r').to_string();
                line_buffer = line_buffer[line_end + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                if line == "data: [DONE]" {
                    log::info!("[openai] Stream complete: [DONE]");
                    break;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    let parsed: JsonValue = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("[openai] Failed to parse SSE data: {e}");
                            continue;
                        }
                    };

                    // Extract usage from the chunk (may appear in any chunk or final one)
                    if let Some(usage) = parsed.get("usage") {
                        if let Some(prompt) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                            input_tokens = prompt as i32;
                        }
                        if let Some(completion) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                            output_tokens = completion as i32;
                        }
                    }

                    // Process choices
                    let choices = match parsed.get("choices").and_then(|v| v.as_array()) {
                        Some(c) => c,
                        None => continue,
                    };

                    for choice in choices {
                        // Check finish_reason
                        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                            match reason {
                                "stop" => {
                                    stop_reason = stream_finished::Reason::Done(
                                        api::response_event::stream_finished::Done {},
                                    );
                                }
                                "tool_calls" => {
                                    stop_reason = stream_finished::Reason::Done(
                                        api::response_event::stream_finished::Done {},
                                    );
                                }
                                "length" => {
                                    stop_reason = stream_finished::Reason::MaxTokenLimit(
                                        stream_finished::ReachedMaxTokenLimit {},
                                    );
                                }
                                _ => {}
                            }
                        }

                        let delta = match choice.get("delta") {
                            Some(d) => d,
                            None => continue,
                        };

                        // Handle text content
                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                            if !content.is_empty() {
                                full_text.push_str(content);

                                if let Some(ref msg_id) = current_text_message_id {
                                    let event = build_append_text(&task_id, msg_id, content);
                                    yield Ok(event);
                                } else {
                                    let msg_id = Uuid::new_v4().to_string();
                                    let event = build_add_agent_output_message(&task_id, &msg_id, content);
                                    current_text_message_id = Some(msg_id);
                                    yield Ok(event);
                                }
                            }
                        }

                        // Handle tool calls
                        if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc in tc_array {
                                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                                // Extend tool_calls vector if needed
                                while tool_calls.len() <= index {
                                    tool_calls.push(ToolCallAccumulator {
                                        index: tool_calls.len(),
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                }

                                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                    tool_calls[index].id = id.to_string();
                                }
                                if let Some(function) = tc.get("function") {
                                    if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                                        tool_calls[index].name = name.to_string();
                                    }
                                    if let Some(args) = function.get("arguments").and_then(|v| v.as_str()) {
                                        tool_calls[index].arguments.push_str(args);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Emit tool call messages for completed tool calls
        let mut assistant_parts: Vec<ContentPart> = Vec::new();
        if !full_text.is_empty() {
            assistant_parts.push(ContentPart::Text(full_text.clone()));
        }

        for tc in &tool_calls {
            if tc.id.is_empty() || tc.name.is_empty() {
                continue;
            }

            let event = build_tool_call_message(&task_id, &tc.id, &tc.name, &tc.arguments);
            yield Ok(event);

            let input: JsonValue = serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
            assistant_parts.push(ContentPart::ToolUse {
                tool_use_id: tc.id.clone(),
                name: tc.name.clone(),
                input,
            });
        }

        // Store the complete assistant message in messages_sent
        if !assistant_parts.is_empty() {
            let assistant_msg = if assistant_parts.len() == 1 {
                match assistant_parts.into_iter().next().unwrap() {
                    ContentPart::Text(text) => ConversationMessage {
                        role: MessageRole::Assistant,
                        content: MessageContent::Text(text),
                    },
                    ContentPart::ToolUse { tool_use_id, name, input } => ConversationMessage {
                        role: MessageRole::Assistant,
                        content: MessageContent::ToolUse { tool_use_id, name, input },
                    },
                    _ => unreachable!(),
                }
            } else {
                ConversationMessage {
                    role: MessageRole::Assistant,
                    content: MessageContent::MultiPart(assistant_parts),
                }
            };

            if let Ok(mut sent) = messages_sent.lock() {
                sent.push(assistant_msg);
            }
        }

        // Emit hallucinated tool error results (tools the model called that aren't known)
        for tc in &tool_calls {
            if tc.id.is_empty() || tc.name.is_empty() {
                continue;
            }
            if !is_known_tool(&tc.name) {
                log::warn!("[openai] Model called unknown tool: {}", tc.name);
                let error_result = ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: format!(
                            "Error: '{}' is not a valid tool. Please use one of the available tools.",
                            tc.name
                        ),
                        is_error: true,
                    },
                };
                if let Ok(mut sent) = messages_sent.lock() {
                    sent.push(error_result);
                }
            }
        }

        let cost = estimate_cost_cents(input_tokens as u32, output_tokens as u32, &model_id);
        let finished_event = build_stream_finished(stop_reason, input_tokens, output_tokens, cost, &model_id);
        yield Ok(finished_event);

        log::info!("[openai] Stream finished: input_tokens={input_tokens}, output_tokens={output_tokens}");
    };

    Box::pin(stream)
}

fn build_user_query_message(task_id: &str, query_text: &str) -> ResponseEvent {
    let message = api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::UserQuery(api::message::UserQuery {
            query: query_text.to_string(),
            ..Default::default()
        })),
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
    // Reuse the Bedrock tool call message builder since the proto output is identical
    crate::ai::bedrock::response_translator::build_tool_call_message(
        task_id,
        tool_use_id,
        tool_name,
        tool_input_json,
    )
}

fn build_stream_finished(
    reason: stream_finished::Reason,
    input_tokens: i32,
    output_tokens: i32,
    cost_in_cents: f32,
    model_id: &str,
) -> ResponseEvent {
    let total_tokens = (input_tokens + output_tokens) as u32;

    let mut byok_token_usage = std::collections::HashMap::new();
    if total_tokens > 0 {
        #[allow(deprecated)]
        byok_token_usage.insert(
            "openai".to_string(),
            stream_finished::ModelTokenUsage {
                model_id: String::new(),
                total_tokens,
                token_usage_by_category: std::collections::HashMap::new(),
            },
        );
    }

    let token_usage = vec![stream_finished::TokenUsage {
        model_id: "openai".to_string(),
        total_input: input_tokens as u32,
        output: output_tokens as u32,
        input_cache_read: 0,
        input_cache_write: 0,
        cost_in_cents,
    }];

    let max_context_tokens = context_window_for_model(model_id);
    let context_usage = if max_context_tokens > 0 {
        input_tokens as f32 / max_context_tokens as f32
    } else {
        0.0
    };

    #[allow(deprecated)]
    let conversation_usage_metadata = Some(stream_finished::ConversationUsageMetadata {
        context_window_usage: context_usage,
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
                token_usage,
                should_refresh_model_config: false,
                request_cost: None,
                conversation_usage_metadata,
            },
        )),
    }
}

/// LiteLLM proxies to various backends — estimate cost based on model name.
/// These are rough estimates; actual billing comes from LiteLLM.
fn estimate_cost_cents(input_tokens: u32, output_tokens: u32, model_id: &str) -> f32 {
    let lower = model_id.to_lowercase();

    let (input_rate, output_rate) = if lower.contains("opus") {
        (15.0, 75.0)
    } else if lower.contains("haiku") {
        (0.80, 4.0)
    } else if lower.contains("sonnet") {
        (3.0, 15.0)
    } else if lower.contains("gpt-4o") {
        (2.50, 10.0)
    } else if lower.contains("gpt-4") {
        (30.0, 60.0)
    } else if lower.contains("gpt-3.5") {
        (0.50, 1.50)
    } else {
        (3.0, 15.0) // Default to Sonnet-tier pricing
    };

    let input_cost = input_tokens as f64 * input_rate * 100.0 / 1_000_000.0;
    let output_cost = output_tokens as f64 * output_rate * 100.0 / 1_000_000.0;
    (input_cost + output_cost) as f32
}

const KNOWN_TOOLS: &[&str] = &[
    "run_shell_command",
    "read_files",
    "apply_file_diffs",
    "grep",
    "file_glob",
    "search_codebase",
    "write_to_long_running_shell_command",
    "read_shell_command_output",
    "read_mcp_resource",
    "read_plan",
    "create_plan",
    "edit_plan",
    "read_notebook",
    "create_notebook",
    "edit_notebook",
    // Legacy aliases — accept old names so in-flight conversations don't break.
    "read_documents",
    "create_documents",
    "edit_documents",
    "start_agent",
    "send_message_to_agent",
    "ask_user_question",
    "suggest_next_prompt",
    "read_skill",
    "fetch_conversation",
    "recall_tool_history",
];

fn is_known_tool(name: &str) -> bool {
    KNOWN_TOOLS.contains(&name) || name.starts_with("mcp__")
}
