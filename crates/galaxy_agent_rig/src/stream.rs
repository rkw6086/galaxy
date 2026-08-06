use futures::{FutureExt, StreamExt};
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, StopReason, ToolCall, TurnCommand,
    TurnControl, Usage,
};
use rig_core::completion::{CompletionError, CompletionModel, CompletionRequest};
use rig_core::streaming::StreamedAssistantContent;
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
                        Ok(StreamedAssistantContent::ToolCall { tool_call, .. }) => {
                            yield Ok(AgentEvent::Tool {
                                event: galaxy_agent_core::ToolEvent::Proposed {
                                    call: domain_tool_call(tool_call),
                                },
                            });
                        }
                        Ok(StreamedAssistantContent::ToolCallDelta { .. }) => {
                            // Rig emits a complete ToolCall after its deltas, which
                            // is the canonical event Galaxy consumes.
                        }
                        Ok(StreamedAssistantContent::Final(response)) => {
                            let mapped_usage = map_usage(response.usage);
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

    let response = futures::select_biased! {
        command = control_future => match command {
            Ok(TurnCommand::Cancel) => {
                return Ok(stopped_before_stream(runtime_request_id));
            }
            Ok(TurnCommand::Steer { .. }) | Err(_) => {
                completion_future.await.map_err(map_completion_error)?
            }
        },
        result = completion_future => result.map_err(map_completion_error)?,
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
    Box::pin(futures::stream::iter([
        Ok(AgentEvent::TurnStarted { runtime_request_id }),
        Ok(AgentEvent::TurnStopped {
            reason: StopReason::Cancelled,
        }),
    ]))
}

pub(crate) fn map_usage(usage: rig_core::completion::Usage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    }
}

pub(crate) fn completion_error_stop_reason(error: &CompletionError) -> Option<StopReason> {
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
mod tests {
    use super::domain_tool_call;

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
}
