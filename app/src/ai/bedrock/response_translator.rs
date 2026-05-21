use std::sync::{Arc, Mutex};

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

use super::convert::{ContentPart, ConversationMessage, MessageContent, MessageRole};
use super::diagnostic::BedrockDiagnosticLogger;

fn json_to_prost_struct(value: &serde_json::Value) -> prost_types::Struct {
    let fields = match value.as_object() {
        Some(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
            .collect(),
        None => std::collections::BTreeMap::new(),
    };
    prost_types::Struct { fields }
}

fn json_to_prost_value(value: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match value {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(arr) => Kind::ListValue(prost_types::ListValue {
            values: arr.iter().map(json_to_prost_value).collect(),
        }),
        serde_json::Value::Object(_) => Kind::StructValue(json_to_prost_struct(value)),
    };
    prost_types::Value { kind: Some(kind) }
}

/// Returns the context window size (in tokens) for a given model ID.
/// Models with "[1m]" in their identifier support 1M token context.
pub fn context_window_for_model(model_id: &str) -> u32 {
    let lower = model_id.to_lowercase();
    if lower.contains("[1m]") {
        1_000_000
    } else if lower.contains("nova") {
        300_000
    } else if lower.contains("deepseek") {
        128_000
    } else {
        200_000
    }
}

pub fn bedrock_stream_to_response_events(
    mut output: ConverseStreamOutput,
    task_id: String,
    needs_create_task: bool,
    user_query: Option<String>,
    diagnostic_logger: Option<Arc<BedrockDiagnosticLogger>>,
    messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
    model_id: String,
    is_summarization: bool,
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

        // Emit the user's query as a proto message in the task so it persists
        // across sessions and can be used for the conversation title.
        if let Some(ref query_text) = user_query {
            let user_query_msg = build_user_query_message(&task_id, query_text);
            yield Ok(user_query_msg);
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
        let mut cache_read_input_tokens: i32 = 0;
        let mut cache_write_input_tokens: i32 = 0;
        let mut stop_reason = stream_finished::Reason::Done(stream_finished::Done {});

        // Track full assistant text and tool calls for bedrock_message_history
        let mut history_text = String::new();
        let mut history_tool_calls: Vec<ContentPart> = Vec::new();
        // Synthetic tool_results for unknown/hallucinated tools — these get
        // paired with their tool_use in history so the next request is valid.
        let mut synthetic_tool_results: Vec<ContentPart> = Vec::new();

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
                                    history_text.push_str(text);
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
                            // Skip suggest_next_prompt — its executor hangs forever
                            // waiting for UI interaction that doesn't exist in the
                            // Bedrock path.
                            if current_tool_name == "suggest_next_prompt" {
                                log::info!("[bedrock] Skipping suggest_next_prompt tool call");
                                current_tool_use_id.clear();
                                current_tool_name.clear();
                                current_tool_input_json.clear();
                            } else if !is_known_tool(&current_tool_name) {
                                // Unknown/hallucinated tool: record it in history
                                // with a paired error result so the conversation
                                // doesn't deadlock waiting for a tool_result that
                                // will never come.
                                log::warn!(
                                    "[bedrock] Model called unknown tool '{}' (id={}), synthesizing error result",
                                    current_tool_name, current_tool_use_id
                                );
                                let input_json: serde_json::Value = serde_json::from_str(&current_tool_input_json)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                                history_tool_calls.push(ContentPart::ToolUse {
                                    tool_use_id: current_tool_use_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input_json,
                                });
                                // Immediately pair with a synthetic error result
                                // so ensure_tool_results_paired doesn't need to
                                // fix it up later (and so the executor doesn't hang).
                                synthetic_tool_results.push(ContentPart::ToolResult {
                                    tool_use_id: current_tool_use_id.clone(),
                                    content: format!(
                                        "Error: '{}' is not a valid tool. Available tools are: run_shell_command, read_files, apply_file_diffs, grep, file_glob. Please use one of these tools instead.",
                                        current_tool_name
                                    ),
                                    is_error: true,
                                });
                                if let Some(ref logger) = diagnostic_logger {
                                    logger.log_stream_event(&format!(
                                        "UnknownToolCall: name={}, id={} — synthesized error result",
                                        current_tool_name, current_tool_use_id
                                    ));
                                }
                                current_tool_use_id.clear();
                                current_tool_name.clear();
                                current_tool_input_json.clear();
                            } else {
                                log::debug!("[bedrock] Tool call complete: {} ({})", current_tool_name, current_tool_use_id);
                                // Track for bedrock_message_history
                                let input_json: serde_json::Value = serde_json::from_str(&current_tool_input_json)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                                history_tool_calls.push(ContentPart::ToolUse {
                                    tool_use_id: current_tool_use_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input_json,
                                });
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
                        if let Some(usage) = metadata.usage() {
                            input_tokens = usage.input_tokens();
                            output_tokens = usage.output_tokens();
                            cache_read_input_tokens = usage.cache_read_input_tokens().unwrap_or(0);
                            cache_write_input_tokens = usage.cache_write_input_tokens().unwrap_or(0);
                            log::info!(
                                "[bedrock-debug] Event #{event_count}: Metadata (input={}, output={}, cache_read={}, cache_write={})",
                                input_tokens, output_tokens, cache_read_input_tokens, cache_write_input_tokens
                            );
                        } else {
                            log::warn!("[bedrock-debug] Event #{event_count}: Metadata with NO usage data");
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
                        let error_msg = format!("{e}");
                        let debug_error = format!("{e:?}");
                        logger.log_stream_error(&error_msg);
                        logger.log_result_fail(&error_msg);
                        if let Some(path) = logger.dump_error_snapshot(&error_msg, &debug_error) {
                            log::error!(
                                "[bedrock] Wrote Bedrock failure snapshot to {}",
                                path.display()
                            );
                        }
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

        let cost = estimate_cost_cents(
            input_tokens as u32,
            output_tokens as u32,
            cache_read_input_tokens as u32,
            cache_write_input_tokens as u32,
            &model_id,
        );
        log::info!(
            "[bedrock] Stream finished: model={model_id}, input_tokens={input_tokens}, output_tokens={output_tokens}, cache_read={cache_read_input_tokens}, cache_write={cache_write_input_tokens}, cost_cents={cost:.4}"
        );

        // Build and store the assistant message into bedrock_messages_sent
        // so the controller can persist it as part of conversation history.
        {
            let mut parts: Vec<ContentPart> = Vec::new();
            if !history_text.is_empty() {
                parts.push(ContentPart::Text(history_text));
            }
            parts.extend(history_tool_calls);

            if !parts.is_empty() {
                let assistant_msg = if parts.len() == 1 {
                    match parts.remove(0) {
                        ContentPart::Text(t) => ConversationMessage {
                            role: MessageRole::Assistant,
                            content: MessageContent::Text(t),
                        },
                        ContentPart::ToolUse { tool_use_id, name, input } => ConversationMessage {
                            role: MessageRole::Assistant,
                            content: MessageContent::ToolUse { tool_use_id, name, input },
                        },
                        other => ConversationMessage {
                            role: MessageRole::Assistant,
                            content: MessageContent::MultiPart(vec![other]),
                        },
                    }
                } else {
                    ConversationMessage {
                        role: MessageRole::Assistant,
                        content: MessageContent::MultiPart(parts),
                    }
                };

                if let Ok(mut sent) = messages_sent.lock() {
                    sent.push(assistant_msg);

                    // If the model hallucinated unknown tools, append a user
                    // message with synthetic error results so the history is
                    // valid for the next Bedrock request (every tool_use must
                    // be followed by a tool_result).
                    if !synthetic_tool_results.is_empty() {
                        let result_msg = if synthetic_tool_results.len() == 1 {
                            match synthetic_tool_results.remove(0) {
                                ContentPart::ToolResult { tool_use_id, content, is_error } => {
                                    ConversationMessage {
                                        role: MessageRole::User,
                                        content: MessageContent::ToolResult { tool_use_id, content, is_error },
                                    }
                                }
                                _ => unreachable!(),
                            }
                        } else {
                            ConversationMessage {
                                role: MessageRole::User,
                                content: MessageContent::MultiPart(synthetic_tool_results),
                            }
                        };
                        sent.push(result_msg);
                        log::info!(
                            "[bedrock] Stored synthetic tool error results in history. Total messages: {}",
                            sent.len()
                        );
                    }

                    log::info!(
                        "[bedrock] Stored assistant message in history. Total messages: {}",
                        sent.len()
                    );
                }
            }
        }

        if let Some(ref logger) = diagnostic_logger {
            let stop_reason_str = match &stop_reason {
                stream_finished::Reason::Done(_) => "EndTurn",
                stream_finished::Reason::MaxTokenLimit(_) => "MaxTokens",
                _ => "Other",
            };
            logger.log_result_success(input_tokens, output_tokens, stop_reason_str);
        }
        let finished_event = build_stream_finished(
            stop_reason,
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            &model_id,
            is_summarization,
        );
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

fn build_user_query_message(task_id: &str, query_text: &str) -> ResponseEvent {
    let message = api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::UserQuery(
            api::message::UserQuery {
                query: query_text.to_string(),
                ..Default::default()
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
    cache_read_input_tokens: i32,
    cache_write_input_tokens: i32,
    model_id: &str,
    is_summarization: bool,
) -> ResponseEvent {
    let total_tokens =
        (input_tokens + output_tokens + cache_read_input_tokens + cache_write_input_tokens) as u32;

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

    let token_usage = vec![stream_finished::TokenUsage {
        model_id: "bedrock".to_string(),
        total_input: input_tokens as u32,
        output: output_tokens as u32,
        input_cache_read: cache_read_input_tokens as u32,
        input_cache_write: cache_write_input_tokens as u32,
        cost_in_cents: estimate_cost_cents(
            input_tokens as u32,
            output_tokens as u32,
            cache_read_input_tokens as u32,
            cache_write_input_tokens as u32,
            model_id,
        ),
    }];

    let max_context_tokens = context_window_for_model(model_id);
    let context_usage = if max_context_tokens > 0 {
        (input_tokens as f32 + cache_read_input_tokens as f32 + cache_write_input_tokens as f32)
            / max_context_tokens as f32
    } else {
        0.0
    };

    #[allow(deprecated)]
    let conversation_usage_metadata = Some(stream_finished::ConversationUsageMetadata {
        context_window_usage: context_usage,
        summarized: is_summarization,
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

/// Estimates cost in cents based on Bedrock model pricing.
/// Pricing varies by model (per 1M tokens):
///   Opus 4.6/4.7: input $15, output $75, cache_read $1.50, cache_write $18.75
///   Sonnet 4/4.6: input $3, output $15, cache_read $0.30, cache_write $3.75
///   Haiku 4.5:    input $0.80, output $4, cache_read $0.08, cache_write $1.00
///   Nova Pro:     input $0.80, output $3.20
///   Nova Lite:    input $0.06, output $0.24
///   Nova Micro:   input $0.035, output $0.14
///   DeepSeek R1:  input $1.35, output $5.40
fn estimate_cost_cents(
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    model_id: &str,
) -> f32 {
    let lower = model_id.to_lowercase();

    // (input_per_1m, output_per_1m, cache_read_per_1m, cache_write_per_1m) in dollars
    let (input_rate, output_rate, cache_read_rate, cache_write_rate) =
        if lower.contains("opus") {
            (15.0, 75.0, 1.50, 18.75)
        } else if lower.contains("haiku") {
            (0.80, 4.0, 0.08, 1.0)
        } else if lower.contains("nova-pro") {
            (0.80, 3.20, 0.0, 0.0)
        } else if lower.contains("nova-lite") {
            (0.06, 0.24, 0.0, 0.0)
        } else if lower.contains("nova-micro") {
            (0.035, 0.14, 0.0, 0.0)
        } else if lower.contains("deepseek") {
            (1.35, 5.40, 0.0, 0.0)
        } else {
            // Default to Sonnet pricing
            (3.0, 15.0, 0.30, 3.75)
        };

    // Convert from dollars per 1M tokens to cents per token
    let input_cost = input_tokens as f64 * input_rate * 100.0 / 1_000_000.0;
    let output_cost = output_tokens as f64 * output_rate * 100.0 / 1_000_000.0;
    let cache_read_cost = cache_read_tokens as f64 * cache_read_rate * 100.0 / 1_000_000.0;
    let cache_write_cost = cache_write_tokens as f64 * cache_write_rate * 100.0 / 1_000_000.0;
    (input_cost + output_cost + cache_read_cost + cache_write_cost) as f32
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
                    uses_pager: true,
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
        "search_codebase" => {
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let codebase_path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(api::message::tool_call::Tool::SearchCodebase(
                api::message::tool_call::SearchCodebase {
                    query,
                    path_filters: vec![],
                    codebase_path,
                },
            ))
        }
        "write_to_long_running_shell_command" => {
            let text_input = input
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(api::message::tool_call::Tool::WriteToLongRunningShellCommand(
                api::message::tool_call::WriteToLongRunningShellCommand {
                    input: text_input.into_bytes(),
                    mode: None,
                    command_id: String::new(),
                },
            ))
        }
        "read_shell_command_output" => {
            Some(api::message::tool_call::Tool::ReadShellCommandOutput(
                api::message::tool_call::ReadShellCommandOutput {
                    command_id: String::new(),
                    delay: None,
                },
            ))
        }
        "read_mcp_resource" => {
            let server_id = input
                .get("server_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let uri = input
                .get("uri")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(api::message::tool_call::Tool::ReadMcpResource(
                api::message::tool_call::ReadMcpResource {
                    uri,
                    server_id,
                },
            ))
        }
        "read_documents" => {
            let documents = input
                .get("document_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| d.as_str())
                        .map(|id| api::message::tool_call::read_documents::Document {
                            document_id: id.to_string(),
                            line_ranges: vec![],
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(api::message::tool_call::Tool::ReadDocuments(
                api::message::tool_call::ReadDocuments { documents },
            ))
        }
        "create_documents" => {
            let new_documents = input
                .get("documents")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            Some(api::message::tool_call::create_documents::NewDocument {
                                title: d.get("title")?.as_str()?.to_string(),
                                content: d.get("content")?.as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(api::message::tool_call::Tool::CreateDocuments(
                api::message::tool_call::CreateDocuments { new_documents },
            ))
        }
        "edit_documents" => {
            let diffs = input
                .get("diffs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            Some(api::message::tool_call::edit_documents::DocumentDiff {
                                document_id: d.get("document_id")?.as_str()?.to_string(),
                                search: d.get("search").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                replace: d.get("replace").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(api::message::tool_call::Tool::EditDocuments(
                api::message::tool_call::EditDocuments { diffs },
            ))
        }
        "start_agent" => {
            let _name = input.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(api::message::tool_call::Tool::Subagent(
                api::message::tool_call::Subagent {
                    task_id: String::new(),
                    payload: prompt,
                    metadata: None,
                },
            ))
        }
        "send_message_to_agent" => {
            let agent_id = input.get("agent_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let message = input.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(api::message::tool_call::Tool::SendMessageToAgent(
                api::SendMessageToAgent {
                    addresses: vec![agent_id],
                    subject: String::new(),
                    message,
                },
            ))
        }
        "ask_user_question" => {
            let question_text = input.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let options: Vec<api::ask_user_question::Option> = input
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| o.as_str())
                        .map(|label| api::ask_user_question::Option { label: label.to_string() })
                        .collect()
                })
                .unwrap_or_default();
            let question = api::ask_user_question::Question {
                question_id: Uuid::new_v4().to_string(),
                question: question_text,
                question_type: Some(api::ask_user_question::question::QuestionType::MultipleChoice(
                    api::ask_user_question::MultipleChoice {
                        options,
                        is_multiselect: false,
                        supports_other: true,
                        recommended_option_index: 0,
                    },
                )),
            };
            Some(api::message::tool_call::Tool::AskUserQuestion(
                api::AskUserQuestion {
                    questions: vec![question],
                },
            ))
        }
        "read_skill" => {
            let skill = input.get("skill").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(api::message::tool_call::Tool::ReadSkill(
                api::message::tool_call::ReadSkill {
                    name: skill.clone(),
                    skill_reference: Some(
                        api::message::tool_call::read_skill::SkillReference::SkillPath(skill),
                    ),
                },
            ))
        }
        "fetch_conversation" => {
            let conversation_id = input.get("conversation_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(api::message::tool_call::Tool::FetchConversation(
                api::message::tool_call::FetchConversation { conversation_id },
            ))
        }
        "suggest_next_prompt" => {
            let prompt = input
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let label = input
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(api::message::tool_call::Tool::SuggestPrompt(
                api::message::tool_call::SuggestPrompt {
                    is_trigger_irrelevant: false,
                    display_mode: Some(
                        api::message::tool_call::suggest_prompt::DisplayMode::PromptChip(
                            api::message::tool_call::suggest_prompt::PromptChip { prompt, label },
                        ),
                    ),
                },
            ))
        }
        name if name.starts_with("mcp__") => {
            // MCP tool call: parse server and tool name from "mcp__{server}__{tool}"
            let parts: Vec<&str> = name.splitn(3, "__").collect();
            let (server_name, mcp_tool_name) = if parts.len() == 3 {
                (parts[1].to_string(), parts[2].to_string())
            } else {
                (String::new(), name.strip_prefix("mcp__").unwrap_or(name).to_string())
            };
            let args = json_to_prost_struct(&input);
            Some(api::message::tool_call::Tool::CallMcpTool(
                api::message::tool_call::CallMcpTool {
                    name: mcp_tool_name,
                    args: Some(args),
                    server_id: server_name,
                },
            ))
        }
        _ => {
            log::error!("[bedrock] build_tool_call_message called with unknown tool: {tool_name}");
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
        // Fallback: emit as agent output text so the stream doesn't break,
        // but this should not happen in normal operation.
        log::error!("[bedrock] Emitting unknown tool as text (should have been caught earlier): {tool_name}");
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
                        "Error: Model attempted to use unknown tool '{}'. This tool does not exist.",
                        tool_name
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

/// Built-in tools that Galaxy knows how to execute directly.
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
    "read_documents",
    "create_documents",
    "edit_documents",
    "start_agent",
    "send_message_to_agent",
    "ask_user_question",
    "suggest_next_prompt",
    "read_skill",
    "fetch_conversation",
];

fn is_known_tool(name: &str) -> bool {
    KNOWN_TOOLS.contains(&name) || name.starts_with("mcp__")
}
