use std::collections::BTreeMap;

use async_trait::async_trait;
use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput;
use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockStart, ConverseStreamOutput as AwsStreamEvent,
    ReasoningContentBlockDelta, StopReason as AwsStopReason,
};
use aws_sdk_bedrockruntime::Client as AwsBedrockClient;
use futures::{FutureExt, StreamExt};
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, AgentRuntime, RuntimeCapabilities,
    RuntimeDescriptor, RuntimeKind, StopReason, ToolCall, ToolEvent, TurnCommand, TurnControl,
    TurnRequest, Usage,
};
use uuid::Uuid;

use super::convert::{build_converse_request, CachingConfig, ConvertedRequest};

const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 64_000;

#[derive(Clone)]
pub(crate) struct BedrockAgentRuntime {
    client: AwsBedrockClient,
    resolved_model: String,
    max_output_tokens: Option<u64>,
    caching_config: CachingConfig,
    descriptor: RuntimeDescriptor,
}

impl BedrockAgentRuntime {
    pub(crate) fn new(
        client: AwsBedrockClient,
        configured_model: String,
        region: String,
        cross_region_inference: bool,
        max_output_tokens: Option<u64>,
        caching_config: CachingConfig,
    ) -> Result<Self, AgentError> {
        let resolved_model = galaxy_agent_rig::resolve_bedrock_model_id(
            &configured_model,
            &region,
            cross_region_inference,
        )?;
        let descriptor = RuntimeDescriptor {
            id: format!("bedrock:{resolved_model}"),
            display_name: format!("Bedrock / {resolved_model}"),
            kind: RuntimeKind::Provider,
            capabilities: RuntimeCapabilities::provider(),
        };
        Ok(Self {
            client,
            resolved_model,
            max_output_tokens,
            caching_config,
            descriptor,
        })
    }
}

#[async_trait]
impl AgentRuntime for BedrockAgentRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let converted =
            convert_turn_request(request, self.max_output_tokens, self.caching_config.clone());
        let mut request = self
            .client
            .converse_stream()
            .model_id(&self.resolved_model)
            .set_system(Some(converted.system))
            .set_messages(Some(converted.messages))
            .inference_config(converted.inference_config);
        if let Some(tool_config) = converted.tool_config {
            request = request.tool_config(tool_config);
        }

        let runtime_request_id = Uuid::new_v4().to_string();
        let send_future = request.send().fuse();
        let initial_control = control.clone();
        let control_future = initial_control.receive().fuse();
        futures::pin_mut!(send_future, control_future);
        let output = futures::select_biased! {
            command = control_future => match command {
                Ok(TurnCommand::Cancel) => {
                    return Ok(stopped_before_stream(runtime_request_id));
                }
                Ok(TurnCommand::Steer { .. }) | Err(_) => {
                    send_future.await.map_err(map_bedrock_error)?
                }
            },
            result = send_future => result.map_err(map_bedrock_error)?,
        };

        Ok(translate_bedrock_stream(
            output,
            runtime_request_id,
            control,
        ))
    }
}

fn convert_turn_request(
    request: TurnRequest,
    configured_max_output_tokens: Option<u64>,
    caching_config: CachingConfig,
) -> ConvertedRequest {
    let max_output_tokens = request
        .max_output_tokens
        .or(configured_max_output_tokens)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
        .min(i32::MAX as u64) as i32;
    build_converse_request(
        request.messages,
        request.system_prompt,
        None,
        request.tools,
        max_output_tokens,
        None,
        None,
        None,
        caching_config,
    )
}

fn translate_bedrock_stream(
    mut output: ConverseStreamOutput,
    runtime_request_id: String,
    control: TurnControl,
) -> AgentEventStream {
    let events = async_stream::stream! {
        yield Ok(AgentEvent::TurnStarted {
            runtime_request_id,
        });

        let mut translator = BedrockStreamTranslator::default();
        let mut control_open = true;
        loop {
            let next_event = output.stream.recv().fuse();
            let next_command = if control_open {
                futures::future::Either::Left(control.receive())
            } else {
                futures::future::Either::Right(futures::future::pending())
            }
            .fuse();
            futures::pin_mut!(next_event, next_command);

            let event = futures::select_biased! {
                command = next_command => {
                    match command {
                        Ok(TurnCommand::Cancel) => {
                            yield Ok(AgentEvent::TurnStopped {
                                reason: StopReason::Cancelled,
                            });
                            return;
                        }
                        Ok(TurnCommand::Steer { .. }) => continue,
                        Err(_) => {
                            control_open = false;
                            continue;
                        }
                    }
                }
                event = next_event => event,
            };

            match event {
                Ok(Some(event)) => match translator.translate(event) {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                },
                Ok(None) => match translator.finish() {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                        return;
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                },
                Err(error) => {
                    yield Err(map_bedrock_error(error));
                    return;
                }
            }
        }
    };
    Box::pin(events)
}

#[derive(Default)]
struct BedrockStreamTranslator {
    content_blocks: BTreeMap<i32, PendingContentBlock>,
    stop_reason: Option<StopReason>,
}

impl BedrockStreamTranslator {
    fn translate(&mut self, event: AwsStreamEvent) -> Result<Vec<AgentEvent>, AgentError> {
        match event {
            AwsStreamEvent::MessageStart(_) => Ok(Vec::new()),
            AwsStreamEvent::ContentBlockStart(start) => {
                let Some(block_start) = start.start() else {
                    return Ok(Vec::new());
                };
                let ContentBlockStart::ToolUse(tool) = block_start else {
                    return Err(protocol_error(
                        "Bedrock started an unsupported output content block",
                    ));
                };
                let index = start.content_block_index();
                if self
                    .content_blocks
                    .insert(
                        index,
                        PendingContentBlock::Tool {
                            id: tool.tool_use_id().to_string(),
                            name: tool.name().to_string(),
                            input: String::new(),
                        },
                    )
                    .is_some()
                {
                    return Err(protocol_error(format!(
                        "Bedrock started content block {index} more than once"
                    )));
                }
                Ok(Vec::new())
            }
            AwsStreamEvent::ContentBlockDelta(delta) => {
                let Some(delta_value) = delta.delta() else {
                    return Err(protocol_error("Bedrock emitted an empty content delta"));
                };
                let index = delta.content_block_index();
                match delta_value {
                    ContentBlockDelta::Text(text) => {
                        Ok(vec![AgentEvent::TextDelta { text: text.clone() }])
                    }
                    ContentBlockDelta::ReasoningContent(reasoning) => {
                        let block = self.content_blocks.entry(index).or_insert_with(|| {
                            PendingContentBlock::Reasoning {
                                text: String::new(),
                                signature: None,
                            }
                        });
                        let PendingContentBlock::Reasoning { text, signature } = block else {
                            return Err(protocol_error(format!(
                                "Bedrock mixed reasoning and tool data in content block {index}"
                            )));
                        };
                        match reasoning {
                            ReasoningContentBlockDelta::Text(delta) => {
                                text.push_str(delta);
                                Ok(vec![AgentEvent::ReasoningDelta {
                                    text: delta.clone(),
                                }])
                            }
                            ReasoningContentBlockDelta::Signature(delta) => {
                                signature.get_or_insert_with(String::new).push_str(delta);
                                Ok(Vec::new())
                            }
                            ReasoningContentBlockDelta::RedactedContent(_) => Ok(Vec::new()),
                            _ => Err(protocol_error("Bedrock emitted an unknown reasoning delta")),
                        }
                    }
                    ContentBlockDelta::ToolUse(tool_delta) => {
                        let Some(PendingContentBlock::Tool { input, .. }) =
                            self.content_blocks.get_mut(&index)
                        else {
                            return Err(protocol_error(format!(
                                "Bedrock emitted tool input before starting content block {index}"
                            )));
                        };
                        input.push_str(tool_delta.input());
                        Ok(Vec::new())
                    }
                    ContentBlockDelta::Citation(_) => Ok(Vec::new()),
                    ContentBlockDelta::Image(_) => {
                        Err(protocol_error("Bedrock emitted unsupported image output"))
                    }
                    ContentBlockDelta::ToolResult(_) => Err(protocol_error(
                        "Bedrock emitted an unexpected tool-result delta",
                    )),
                    _ => Err(protocol_error("Bedrock emitted an unknown content delta")),
                }
            }
            AwsStreamEvent::ContentBlockStop(stop) => {
                let index = stop.content_block_index();
                let Some(block) = self.content_blocks.remove(&index) else {
                    return Ok(Vec::new());
                };
                match block {
                    PendingContentBlock::Tool { id, name, input } => {
                        let arguments = serde_json::from_str(&input).map_err(|error| {
                            protocol_error(format!(
                                "Bedrock returned invalid JSON for tool '{name}' ({id}): {error}"
                            ))
                        })?;
                        Ok(vec![AgentEvent::Tool {
                            event: ToolEvent::Proposed {
                                call: ToolCall {
                                    id,
                                    name,
                                    arguments,
                                },
                            },
                        }])
                    }
                    PendingContentBlock::Reasoning { text, signature } => {
                        Ok(vec![AgentEvent::ReasoningCompleted { text, signature }])
                    }
                }
            }
            AwsStreamEvent::MessageStop(stop) => {
                if self.stop_reason.is_some() {
                    return Err(protocol_error(
                        "Bedrock emitted more than one message-stop event",
                    ));
                }
                self.stop_reason = Some(map_stop_reason(stop.stop_reason()));
                Ok(Vec::new())
            }
            AwsStreamEvent::Metadata(metadata) => {
                let Some(usage) = metadata.usage() else {
                    return Ok(Vec::new());
                };
                Ok(vec![AgentEvent::UsageUpdated {
                    usage: Usage {
                        input_tokens: nonnegative_tokens(usage.input_tokens()),
                        output_tokens: nonnegative_tokens(usage.output_tokens()),
                        cached_input_tokens: nonnegative_tokens(
                            usage.cache_read_input_tokens().unwrap_or(0),
                        ),
                        cache_creation_input_tokens: nonnegative_tokens(
                            usage.cache_write_input_tokens().unwrap_or(0),
                        ),
                    },
                }])
            }
            _ => Err(protocol_error("Bedrock emitted an unknown stream event")),
        }
    }

    fn finish(self) -> Result<Vec<AgentEvent>, AgentError> {
        if !self.content_blocks.is_empty() {
            return Err(protocol_error(
                "Bedrock stream ended with incomplete content blocks",
            ));
        }
        let reason = self
            .stop_reason
            .ok_or_else(|| protocol_error("Bedrock stream ended before the message-stop event"))?;
        Ok(vec![AgentEvent::TurnStopped { reason }])
    }
}

#[derive(Debug)]
enum PendingContentBlock {
    Tool {
        id: String,
        name: String,
        input: String,
    },
    Reasoning {
        text: String,
        signature: Option<String>,
    },
}

fn map_stop_reason(reason: &AwsStopReason) -> StopReason {
    match reason {
        AwsStopReason::EndTurn | AwsStopReason::StopSequence | AwsStopReason::ToolUse => {
            StopReason::Completed
        }
        AwsStopReason::MaxTokens => StopReason::MaxTokens,
        AwsStopReason::ModelContextWindowExceeded => StopReason::ContextWindowExceeded,
        AwsStopReason::ContentFiltered | AwsStopReason::GuardrailIntervened => StopReason::Refusal,
        AwsStopReason::MalformedModelOutput | AwsStopReason::MalformedToolUse => {
            StopReason::Other(reason.as_str().to_string())
        }
        other => StopReason::Other(other.as_str().to_string()),
    }
}

fn nonnegative_tokens(value: i32) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

fn stopped_before_stream(runtime_request_id: String) -> AgentEventStream {
    Box::pin(futures::stream::iter([
        Ok(AgentEvent::TurnStarted { runtime_request_id }),
        Ok(AgentEvent::TurnStopped {
            reason: StopReason::Cancelled,
        }),
    ]))
}

fn map_bedrock_error(error: impl std::fmt::Display + std::fmt::Debug) -> AgentError {
    let display = error.to_string();
    let debug = format!("{error:?}");
    let message = if debug.len() > display.len() {
        debug
    } else {
        display
    };
    let normalized = message.to_ascii_lowercase();
    let kind = if normalized.contains("accessdenied")
        || normalized.contains("access denied")
        || normalized.contains("unauthorized")
        || normalized.contains("credential")
    {
        AgentErrorKind::Authentication
    } else if normalized.contains("throttl") || normalized.contains("rate limit") {
        AgentErrorKind::RateLimited
    } else if normalized.contains("context window")
        || normalized.contains("too many tokens")
        || normalized.contains("modelcontextwindowexceeded")
    {
        AgentErrorKind::ContextWindowExceeded
    } else if normalized.contains("validation")
        || normalized.contains("resource not found")
        || normalized.contains("resourcenotfound")
    {
        AgentErrorKind::InvalidRequest
    } else if normalized.contains("timeout")
        || normalized.contains("dispatchfailure")
        || normalized.contains("connection")
    {
        AgentErrorKind::Transport
    } else {
        AgentErrorKind::Provider
    };
    let mut error = AgentError::new(kind, message);
    error.recoverable = matches!(
        kind,
        AgentErrorKind::RateLimited | AgentErrorKind::Transport
    );
    error
}

fn protocol_error(message: impl Into<String>) -> AgentError {
    AgentError::new(AgentErrorKind::Protocol, message)
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
