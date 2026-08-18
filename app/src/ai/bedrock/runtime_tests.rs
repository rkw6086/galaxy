use aws_sdk_bedrockruntime::types::{
    CacheTtl, ContentBlock, ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart,
    ContentBlockStartEvent, ContentBlockStopEvent, ConverseStreamMetadataEvent,
    ConverseStreamOutput as AwsStreamEvent, MessageStopEvent, ReasoningContentBlockDelta,
    StopReason as AwsStopReason, SystemContentBlock, TokenUsage, Tool, ToolUseBlockDelta,
    ToolUseBlockStart,
};
use galaxy_agent_core::{
    AgentErrorKind, AgentEvent, ConversationMessage, MessageContent, MessageRole, StopReason,
    ToolDefinition, ToolEvent, TurnRequest, Usage,
};
use serde_json::json;

use super::*;

fn turn_request() -> TurnRequest {
    let mut request = TurnRequest::new(
        "anthropic.claude-test",
        vec![
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text("first".to_string()),
            },
            ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::Text("response".to_string()),
            },
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text("continue".to_string()),
            },
        ],
    );
    request.system_prompt = Some("system".to_string());
    request.tools = vec![ToolDefinition {
        name: "read_files".to_string(),
        description: "Read files".to_string(),
        input_schema: json!({"type": "object"}),
    }];
    request
}

fn cache_ttls(converted: &ConvertedRequest) -> Vec<Option<CacheTtl>> {
    let mut ttls = Vec::new();
    for message in &converted.messages {
        for block in message.content() {
            if let ContentBlock::CachePoint(point) = block {
                ttls.push(point.ttl().cloned());
            }
        }
    }
    for block in &converted.system {
        if let SystemContentBlock::CachePoint(point) = block {
            ttls.push(point.ttl().cloned());
        }
    }
    if let Some(tool_config) = &converted.tool_config {
        for tool in tool_config.tools() {
            if let Tool::CachePoint(point) = tool {
                ttls.push(point.ttl().cloned());
            }
        }
    }
    ttls
}

#[test]
fn one_turn_transport_preserves_disabled_default_and_one_hour_cache_modes() {
    let disabled = convert_turn_request(
        turn_request(),
        Some(4096),
        CachingConfig {
            enabled: false,
            extended_ttl_requested: false,
        },
    );
    assert!(cache_ttls(&disabled).is_empty());

    let default = convert_turn_request(turn_request(), Some(4096), CachingConfig::default());
    assert_eq!(cache_ttls(&default), vec![None, None, None]);

    let one_hour = convert_turn_request(
        turn_request(),
        Some(4096),
        CachingConfig {
            enabled: true,
            extended_ttl_requested: true,
        },
    );
    assert_eq!(
        cache_ttls(&one_hour),
        vec![
            Some(CacheTtl::OneHour),
            Some(CacheTtl::OneHour),
            Some(CacheTtl::OneHour),
        ]
    );
}

#[test]
fn one_turn_transport_prefers_request_output_limit() {
    let mut request = turn_request();
    request.max_output_tokens = Some(8192);
    let converted = convert_turn_request(request, Some(4096), CachingConfig::default());
    assert_eq!(converted.inference_config.max_tokens(), Some(8192));
}

fn tool_start(index: i32, id: &str, name: &str) -> AwsStreamEvent {
    AwsStreamEvent::ContentBlockStart(
        ContentBlockStartEvent::builder()
            .content_block_index(index)
            .start(ContentBlockStart::ToolUse(
                ToolUseBlockStart::builder()
                    .tool_use_id(id)
                    .name(name)
                    .build()
                    .unwrap(),
            ))
            .build()
            .unwrap(),
    )
}

fn ordinary_start(index: i32) -> AwsStreamEvent {
    AwsStreamEvent::ContentBlockStart(
        ContentBlockStartEvent::builder()
            .content_block_index(index)
            .build()
            .unwrap(),
    )
}

fn content_delta(index: i32, delta: ContentBlockDelta) -> AwsStreamEvent {
    AwsStreamEvent::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .content_block_index(index)
            .delta(delta)
            .build()
            .unwrap(),
    )
}

fn content_stop(index: i32) -> AwsStreamEvent {
    AwsStreamEvent::ContentBlockStop(
        ContentBlockStopEvent::builder()
            .content_block_index(index)
            .build()
            .unwrap(),
    )
}

fn message_stop(reason: AwsStopReason) -> AwsStreamEvent {
    AwsStreamEvent::MessageStop(
        MessageStopEvent::builder()
            .stop_reason(reason)
            .build()
            .unwrap(),
    )
}

fn metadata(usage: Usage) -> AwsStreamEvent {
    AwsStreamEvent::Metadata(
        ConverseStreamMetadataEvent::builder()
            .usage(
                TokenUsage::builder()
                    .input_tokens(usage.input_tokens as i32)
                    .output_tokens(usage.output_tokens as i32)
                    .total_tokens((usage.input_tokens + usage.output_tokens) as i32)
                    .cache_read_input_tokens(usage.cached_input_tokens as i32)
                    .cache_write_input_tokens(usage.cache_creation_input_tokens as i32)
                    .build()
                    .unwrap(),
            )
            .build(),
    )
}

#[test]
fn stream_translator_accepts_ordinary_content_block_starts() {
    let mut translator = BedrockStreamTranslator::default();
    assert!(translator.translate(ordinary_start(0)).unwrap().is_empty());
    assert_eq!(
        translator
            .translate(content_delta(
                0,
                ContentBlockDelta::Text("response".to_string()),
            ))
            .unwrap(),
        vec![AgentEvent::TextDelta {
            text: "response".to_string(),
        }]
    );
    assert!(translator.translate(content_stop(0)).unwrap().is_empty());
}

#[test]
fn stream_translator_correlates_tools_by_content_index() {
    let mut translator = BedrockStreamTranslator::default();
    translator
        .translate(tool_start(2, "call-2", "grep"))
        .unwrap();
    translator
        .translate(tool_start(1, "call-1", "read_files"))
        .unwrap();
    translator
        .translate(content_delta(
            1,
            ContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder()
                    .input("{\"files\":[\"Cargo.toml\"]}")
                    .build()
                    .unwrap(),
            ),
        ))
        .unwrap();
    translator
        .translate(content_delta(
            2,
            ContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder()
                    .input("{\"query\":\"ProviderRun\"}")
                    .build()
                    .unwrap(),
            ),
        ))
        .unwrap();

    let first = translator.translate(content_stop(1)).unwrap();
    let second = translator.translate(content_stop(2)).unwrap();
    assert!(matches!(
        first.as_slice(),
        [AgentEvent::Tool {
            event: ToolEvent::Proposed { call }
        }] if call.id == "call-1"
            && call.name == "read_files"
            && call.arguments == json!({"files": ["Cargo.toml"]})
    ));
    assert!(matches!(
        second.as_slice(),
        [AgentEvent::Tool {
            event: ToolEvent::Proposed { call }
        }] if call.id == "call-2"
            && call.name == "grep"
            && call.arguments == json!({"query": "ProviderRun"})
    ));
}

#[test]
fn stream_translator_defers_stop_until_usage_metadata_arrives() {
    let mut translator = BedrockStreamTranslator::default();
    assert!(translator
        .translate(message_stop(AwsStopReason::EndTurn))
        .unwrap()
        .is_empty());

    let expected_usage = Usage {
        input_tokens: 10,
        output_tokens: 4,
        cached_input_tokens: 7,
        cache_creation_input_tokens: 3,
    };
    assert_eq!(
        translator
            .translate(metadata(expected_usage.clone()))
            .unwrap(),
        vec![AgentEvent::UsageUpdated {
            usage: expected_usage,
        }]
    );
    assert_eq!(
        translator.finish().unwrap(),
        vec![AgentEvent::TurnStopped {
            reason: StopReason::Completed,
        }]
    );
}

#[test]
fn stream_translator_preserves_reasoning_text_and_signature() {
    let mut translator = BedrockStreamTranslator::default();
    assert_eq!(
        translator
            .translate(content_delta(
                0,
                ContentBlockDelta::ReasoningContent(ReasoningContentBlockDelta::Text(
                    "inspect".to_string(),
                )),
            ))
            .unwrap(),
        vec![AgentEvent::ReasoningDelta {
            text: "inspect".to_string(),
        }]
    );
    translator
        .translate(content_delta(
            0,
            ContentBlockDelta::ReasoningContent(ReasoningContentBlockDelta::Signature(
                "signature".to_string(),
            )),
        ))
        .unwrap();
    assert_eq!(
        translator.translate(content_stop(0)).unwrap(),
        vec![AgentEvent::ReasoningCompleted {
            text: "inspect".to_string(),
            signature: Some("signature".to_string()),
        }]
    );
}

#[test]
fn stream_translator_rejects_invalid_tool_json() {
    let mut translator = BedrockStreamTranslator::default();
    translator
        .translate(tool_start(0, "call", "read_files"))
        .unwrap();
    translator
        .translate(content_delta(
            0,
            ContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder()
                    .input("not-json")
                    .build()
                    .unwrap(),
            ),
        ))
        .unwrap();
    let error = translator.translate(content_stop(0)).unwrap_err();
    assert_eq!(error.kind, AgentErrorKind::Protocol);
}

#[test]
fn bedrock_stop_reasons_map_to_domain_reasons() {
    assert_eq!(
        map_stop_reason(&AwsStopReason::ToolUse),
        StopReason::Completed
    );
    assert_eq!(
        map_stop_reason(&AwsStopReason::MaxTokens),
        StopReason::MaxTokens
    );
    assert_eq!(
        map_stop_reason(&AwsStopReason::ModelContextWindowExceeded),
        StopReason::ContextWindowExceeded
    );
    assert_eq!(
        map_stop_reason(&AwsStopReason::GuardrailIntervened),
        StopReason::Refusal
    );
}
