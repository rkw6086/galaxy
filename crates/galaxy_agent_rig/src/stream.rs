use std::collections::HashMap;

use futures::{FutureExt, StreamExt};
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, StopReason, ToolCall, TurnCommand,
    TurnControl, Usage,
};
use rig_core::completion::{CompletionError, CompletionModel, CompletionRequest};
use rig_core::streaming::{StreamedAssistantContent, ToolCallDeltaContent};
use uuid::Uuid;

pub(crate) async fn start_model_turn<M>(
    model: M,
    completion_request: CompletionRequest,
    control: TurnControl,
    max_output_tokens: Option<u64>,
) -> Result<AgentEventStream, AgentError>
where
    M: CompletionModel + Send + Sync + 'static,
{
    let runtime_request_id = Uuid::new_v4().to_string();
    let stream_future = model.stream(completion_request).fuse();
    let initial_control = control.clone();
    let control_future = initial_control.receive().fuse();
    futures::pin_mut!(stream_future, control_future);

    let stream_result = futures::select_biased! {
        command = control_future => match command {
            Ok(TurnCommand::Cancel) => {
                return Ok(stopped_before_stream(runtime_request_id));
            }
            Ok(TurnCommand::Steer { .. }) | Err(_) => stream_future.await,
        },
        result = stream_future => result,
    };
    let mut rig_stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            if let Some(reason) = completion_error_stop_reason(&error) {
                return Ok(stopped_with_reason(runtime_request_id, reason));
            }
            return Err(map_completion_error(error));
        }
    };

    let events = async_stream::stream! {
        yield Ok(AgentEvent::TurnStarted {
            runtime_request_id,
        });

        let mut control_open = true;
        let mut last_output_tokens = 0;
        let mut tool_call_progress = HashMap::<String, ToolCallProgressState>::new();
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
                            // Steering is not advertised by provider runtimes yet.
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
                            yield Ok(AgentEvent::ReasoningCompleted {
                                text,
                                signature: reasoning.first_signature().map(str::to_string),
                            });
                        }
                        Ok(StreamedAssistantContent::ReasoningDelta { reasoning, .. }) => {
                            if !reasoning.is_empty() {
                                yield Ok(AgentEvent::ReasoningDelta { text: reasoning });
                            }
                        }
                        Ok(StreamedAssistantContent::ToolCall {
                            tool_call,
                            internal_call_id,
                        }) => {
                            tool_call_progress.remove(&internal_call_id);
                            yield Ok(AgentEvent::Tool {
                                event: galaxy_agent_core::ToolEvent::Proposed {
                                    call: domain_tool_call(tool_call),
                                },
                            });
                        }
                        Ok(StreamedAssistantContent::ToolCallDelta {
                            id,
                            internal_call_id,
                            content,
                        }) => {
                            let progress = tool_call_progress
                                .entry(internal_call_id.clone())
                                .or_insert_with(|| ToolCallProgressState {
                                    call_id: if id.is_empty() {
                                        internal_call_id
                                    } else {
                                        id
                                    },
                                    ..ToolCallProgressState::default()
                                });
                            match content {
                                ToolCallDeltaContent::Name(name) => progress.name = Some(name),
                                ToolCallDeltaContent::Delta(arguments) => {
                                    progress.arguments_bytes = progress
                                        .arguments_bytes
                                        .saturating_add(arguments.len() as u64);
                                }
                            }
                            yield Ok(AgentEvent::ToolCallProgress {
                                call_id: progress.call_id.clone(),
                                name: progress.name.clone(),
                                arguments_bytes: progress.arguments_bytes,
                            });
                        }
                        Ok(StreamedAssistantContent::Final(response)) => {
                            let mapped_usage = map_usage(response.usage);
                            last_output_tokens = mapped_usage.output_tokens;
                            yield Ok(AgentEvent::UsageUpdated {
                                usage: mapped_usage,
                            });
                        }
                        Ok(StreamedAssistantContent::Unknown(value)) => {
                            if is_keepalive_event(&value) {
                                yield Ok(AgentEvent::KeepAlive);
                            } else {
                                yield Err(AgentError::new(
                                    AgentErrorKind::Protocol,
                                    format!("Rig returned an unsupported provider event: {value}"),
                                ));
                                return;
                            }
                        }
                        Err(error) => {
                            if let Some(reason) = completion_error_stop_reason(&error) {
                                yield Ok(AgentEvent::TurnStopped { reason });
                                return;
                            }
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

#[derive(Default)]
struct ToolCallProgressState {
    call_id: String,
    name: Option<String>,
    arguments_bytes: u64,
}

pub(crate) async fn start_model_completion<M>(
    model: M,
    completion_request: CompletionRequest,
    control: TurnControl,
    max_output_tokens: Option<u64>,
) -> Result<AgentEventStream, AgentError>
where
    M: CompletionModel + Send + Sync + 'static,
{
    let runtime_request_id = Uuid::new_v4().to_string();
    let completion_future = model.completion(completion_request).fuse();
    let initial_control = control.clone();
    let control_future = initial_control.receive().fuse();
    futures::pin_mut!(completion_future, control_future);

    let completion_result = futures::select_biased! {
        command = control_future => match command {
            Ok(TurnCommand::Cancel) => {
                return Ok(stopped_before_stream(runtime_request_id));
            }
            Ok(TurnCommand::Steer { .. }) | Err(_) => completion_future.await,
        },
        result = completion_future => result,
    };
    let response = match completion_result {
        Ok(response) => response,
        Err(error) => {
            if let Some(reason) = completion_error_stop_reason(&error) {
                return Ok(stopped_with_reason(runtime_request_id, reason));
            }
            return Err(map_completion_error(error));
        }
    };

    let events = async_stream::stream! {
        yield Ok(AgentEvent::TurnStarted { runtime_request_id });

        for content in response.choice {
            match content {
                rig_core::completion::AssistantContent::Text(text) => {
                    if !text.text.is_empty() {
                        yield Ok(AgentEvent::TextDelta { text: text.text });
                    }
                }
                rig_core::completion::AssistantContent::Reasoning(reasoning) => {
                    yield Ok(AgentEvent::ReasoningCompleted {
                        text: reasoning.display_text(),
                        signature: reasoning.first_signature().map(str::to_string),
                    });
                }
                rig_core::completion::AssistantContent::ToolCall(tool_call) => {
                    yield Ok(AgentEvent::Tool {
                        event: galaxy_agent_core::ToolEvent::Proposed {
                            call: domain_tool_call(tool_call),
                        },
                    });
                }
                rig_core::completion::AssistantContent::Image(_) => {
                    yield Err(AgentError::new(
                        AgentErrorKind::Protocol,
                        "Rig returned an unsupported image completion content block",
                    ));
                    return;
                }
            }
        }

        let usage = map_usage(response.usage);
        let reached_max_tokens = max_output_tokens
            .is_some_and(|max| usage.output_tokens >= max);
        yield Ok(AgentEvent::UsageUpdated { usage });
        yield Ok(AgentEvent::TurnStopped {
            reason: if reached_max_tokens {
                StopReason::MaxTokens
            } else {
                StopReason::Completed
            },
        });
    };

    Ok(Box::pin(events))
}

fn domain_tool_call(tool_call: rig_core::message::ToolCall) -> ToolCall {
    ToolCall {
        // OpenAI Responses uses a separate `call_id` for function-call output
        // correlation. The domain model has one ID, so preserve that value when
        // it is available and fall back to the standard tool-call ID for other
        // OpenAI-compatible providers.
        id: tool_call.call_id.unwrap_or(tool_call.id),
        name: tool_call.function.name,
        arguments: tool_call.function.arguments,
    }
}

fn stopped_before_stream(runtime_request_id: String) -> AgentEventStream {
    stopped_with_reason(runtime_request_id, StopReason::Cancelled)
}

fn stopped_with_reason(runtime_request_id: String, reason: StopReason) -> AgentEventStream {
    Box::pin(futures::stream::iter([
        Ok(AgentEvent::TurnStarted { runtime_request_id }),
        Ok(AgentEvent::TurnStopped { reason }),
    ]))
}

pub(crate) fn map_usage(usage: rig_core::completion::Usage) -> Usage {
    // Rig preserves provider-native input semantics: OpenAI includes cached
    // tokens in `input_tokens`, while Anthropic reports cache reads/writes
    // separately. `total_tokens - output_tokens` gives the normalized prompt
    // size for both shapes, so store only the uncached portion in
    // `input_tokens` and keep cache reads/writes disjoint.
    let total_input_tokens = if usage.total_tokens > 0 && usage.total_tokens >= usage.output_tokens
    {
        usage.total_tokens - usage.output_tokens
    } else {
        usage
            .input_tokens
            .saturating_add(usage.cached_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens)
    };
    Usage {
        input_tokens: total_input_tokens
            .saturating_sub(usage.cached_input_tokens)
            .saturating_sub(usage.cache_creation_input_tokens),
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    }
}

pub(crate) fn completion_error_stop_reason(error: &CompletionError) -> Option<StopReason> {
    if completion_error_indicates_context_window_exceeded(error) {
        return Some(StopReason::ContextWindowExceeded);
    }

    match error {
        // rig-bedrock 0.40 currently surfaces Bedrock's MaxTokens stop as a
        // provider error. Normalize it here so the UI sees the same semantic
        // stop reason as every other Rig-backed provider.
        CompletionError::ProviderError(message) if message == "Exceeded max tokens" => {
            Some(StopReason::MaxTokens)
        }
        _ => None,
    }
}

fn completion_error_indicates_context_window_exceeded(error: &CompletionError) -> bool {
    if let Ok(Some(value)) = error.provider_response_json()
        && json_value_indicates_context_window_exceeded(&value)
    {
        return true;
    }

    error
        .provider_response_body()
        .is_some_and(text_indicates_context_window_exceeded)
        || text_indicates_context_window_exceeded(&error.to_string())
}

fn json_value_indicates_context_window_exceeded(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text_indicates_context_window_exceeded(text),
        serde_json::Value::Array(values) => values
            .iter()
            .any(json_value_indicates_context_window_exceeded),
        serde_json::Value::Object(map) => map
            .values()
            .any(json_value_indicates_context_window_exceeded),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

fn is_keepalive_event(value: &serde_json::Value) -> bool {
    value.get("type").and_then(serde_json::Value::as_str) == Some("keepalive")
}

fn text_indicates_context_window_exceeded(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("modelcontextwindowexceeded")
        || normalized.contains("context_length_exceeded")
        || normalized.contains("context window")
        || normalized.contains("context length")
        || normalized.contains("too many tokens")
        || normalized.contains("maximum context")
        || normalized.contains("input is too long")
        || (normalized.contains("input exceeds") && normalized.contains("context"))
        || normalized.contains("exceeds the context")
}

fn map_completion_error(error: CompletionError) -> AgentError {
    let is_context_window_exceeded = completion_error_indicates_context_window_exceeded(&error);
    let status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    let kind = if is_context_window_exceeded {
        AgentErrorKind::ContextWindowExceeded
    } else {
        match status {
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
        }
    };
    let mut mapped = AgentError::new(kind, error.to_string());
    mapped.recoverable = matches!(
        kind,
        AgentErrorKind::RateLimited | AgentErrorKind::Transport
    ) || status.is_some_and(|status| (500..=599).contains(&status));
    mapped
}

#[cfg(test)]
mod tests {
    use galaxy_agent_core::{AgentErrorKind, StopReason};
    use rig_core::completion::CompletionError;

    use super::{
        completion_error_stop_reason, domain_tool_call, is_keepalive_event, map_completion_error,
    };

    #[test]
    fn domain_tool_call_prefers_responses_call_id() {
        let tool_call = rig_core::message::ToolCall::new(
            "fc_item_123".to_string(),
            rig_core::message::ToolFunction {
                name: "read_files".to_string(),
                arguments: serde_json::json!({"files": ["Cargo.toml"]}),
            },
        )
        .with_call_id("call_123".to_string());

        let call = domain_tool_call(tool_call);

        assert_eq!(call.id, "call_123");
        assert_eq!(call.name, "read_files");
    }

    #[test]
    fn domain_tool_call_falls_back_to_wire_id_without_responses_call_id() {
        let tool_call = rig_core::message::ToolCall::new(
            "fc_item_123".to_string(),
            rig_core::message::ToolFunction {
                name: "read_files".to_string(),
                arguments: serde_json::json!({"files": ["Cargo.toml"]}),
            },
        );

        let call = domain_tool_call(tool_call);

        assert_eq!(call.id, "fc_item_123");
        assert_eq!(call.name, "read_files");
    }

    #[test]
    fn provider_context_window_error_maps_to_semantic_stop_reason_and_kind() {
        let status = rig_core::http_client::Response::builder()
            .status(400)
            .body(())
            .unwrap()
            .status();
        let error = CompletionError::from_http_response(
            status,
            r#"{"error":{"message":"Your input exceeds the context window of this model. Please adjust your input and try again.","code":"400"}}"#,
        );

        assert_eq!(
            completion_error_stop_reason(&error),
            Some(StopReason::ContextWindowExceeded)
        );
        let mapped = map_completion_error(error);
        assert_eq!(mapped.kind, AgentErrorKind::ContextWindowExceeded);
        assert!(!mapped.recoverable);
    }

    #[test]
    fn provider_context_length_string_maps_to_semantic_stop_reason() {
        let error = CompletionError::ProviderError(
            "context_length_exceeded: maximum context length is 128000 tokens".to_string(),
        );

        assert_eq!(
            completion_error_stop_reason(&error),
            Some(StopReason::ContextWindowExceeded)
        );
    }

    #[test]
    fn provider_keepalive_event_is_a_transport_heartbeat() {
        assert!(is_keepalive_event(&serde_json::json!({
            "type": "keepalive",
            "sequence_number": 3,
        })));
        assert!(!is_keepalive_event(&serde_json::json!({
            "type": "unsupported",
        })));
    }

    #[test]
    fn provider_server_error_is_recoverable() {
        let status = rig_core::http_client::Response::builder()
            .status(503)
            .body(())
            .unwrap()
            .status();
        let error = CompletionError::from_http_response(
            status,
            r#"{"error":{"message":"Service temporarily unavailable"}}"#,
        );

        let mapped = map_completion_error(error);
        assert_eq!(mapped.kind, AgentErrorKind::Provider);
        assert!(mapped.recoverable);
    }
}
