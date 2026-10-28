use std::collections::HashMap;

use aws_sdk_bedrockruntime::types::{
    CachePointBlock, CachePointType, CacheTtl, ContentBlock, ConversationRole, ImageBlock,
    ImageFormat, ImageSource, InferenceConfiguration, Message as BedrockMessage,
    ReasoningContentBlock, ReasoningTextBlock, SystemContentBlock, Tool, ToolConfiguration,
    ToolInputSchema, ToolResultBlock, ToolResultContentBlock, ToolResultStatus, ToolSpecification,
    ToolUseBlock,
};
use aws_smithy_types::{Blob, Document};
use galaxy_agent_rig::prepare_bedrock_history;
use serde_json::Value as JsonValue;

use super::external_config::ExternalBedrockConfig;
// Re-export shared provider types so existing imports from bedrock::convert continue to work.
pub use crate::ai::provider::types::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
};

#[derive(Clone, Debug)]
pub struct CachingConfig {
    pub enabled: bool,
    pub extended_ttl_requested: bool,
}

impl Default for CachingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            extended_ttl_requested: false,
        }
    }
}

impl CachingConfig {
    pub fn from_external_config(external: &ExternalBedrockConfig) -> Self {
        Self {
            enabled: !external.disable_prompt_caching,
            extended_ttl_requested: external.enable_prompt_caching_1h,
        }
    }

    pub fn for_model(mut self, model_id: &str) -> Self {
        let model = galaxy_bedrock_model_catalog::model_metadata(model_id);
        let known_claude_4 = model.is_some_and(|metadata| {
            metadata.provider_name == "Anthropic"
                && metadata.supports_agent_runtime() == Some(true)
                && (metadata.model_id.starts_with("anthropic.claude-opus-4")
                    || metadata.model_id.starts_with("anthropic.claude-sonnet-4")
                    || metadata.model_id.starts_with("anthropic.claude-haiku-4"))
        });
        self.enabled &= known_claude_4;
        self.extended_ttl_requested &= self.enabled && model.is_some_and(|metadata| {
            ["4-5", "4-6", "4-7", "4-8"].iter().any(|version| metadata.model_id.contains(version))
        });
        self
    }
}

pub struct ConvertedRequest {
    pub messages: Vec<BedrockMessage>,
    pub system: Vec<SystemContentBlock>,
    pub inference_config: InferenceConfiguration,
    pub tool_config: Option<ToolConfiguration>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_converse_request(
    model_id: &str,
    messages: Vec<ConversationMessage>,
    system_prompt: Option<String>,
    compact_summary: Option<String>,
    tools: Vec<ToolDefinition>,
    max_tokens: i32,
    temperature: Option<f32>,
    top_p: Option<f32>,
    stop_sequences: Option<Vec<String>>,
    caching_config: CachingConfig,
) -> ConvertedRequest {
    let messages = prepare_bedrock_history(model_id, messages);
    let bedrock_messages = convert_messages(messages, &caching_config);
    let system = convert_system_prompt(system_prompt, compact_summary, &caching_config);
    let inference_config = build_inference_config(max_tokens, temperature, top_p, stop_sequences);
    let tool_config = build_tool_config(tools, &caching_config);

    ConvertedRequest {
        messages: bedrock_messages,
        system,
        inference_config,
        tool_config,
    }
}

fn json_to_document(value: JsonValue) -> Document {
    match value {
        JsonValue::Null => Document::Null,
        JsonValue::Bool(b) => Document::Bool(b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Document::Number(aws_smithy_types::Number::PosInt(i as u64))
            } else if let Some(f) = n.as_f64() {
                Document::Number(aws_smithy_types::Number::Float(f))
            } else {
                Document::Null
            }
        }
        JsonValue::String(s) => Document::String(s),
        JsonValue::Array(arr) => Document::Array(arr.into_iter().map(json_to_document).collect()),
        JsonValue::Object(obj) => {
            let map: HashMap<String, Document> = obj
                .into_iter()
                .map(|(k, v)| (k, json_to_document(v)))
                .collect();
            Document::Object(map)
        }
    }
}

fn convert_messages(
    messages: Vec<ConversationMessage>,
    caching_config: &CachingConfig,
) -> Vec<BedrockMessage> {
    let mut result = Vec::new();

    for msg in messages {
        let role = match msg.role {
            MessageRole::User => ConversationRole::User,
            MessageRole::Assistant => ConversationRole::Assistant,
        };

        let content_blocks = match msg.content {
            MessageContent::Text(text) => vec![ContentBlock::Text(text)],
            MessageContent::ToolUse {
                tool_use_id,
                name,
                input,
            } => {
                let input_doc = json_to_document(input);
                vec![ContentBlock::ToolUse(
                    ToolUseBlock::builder()
                        .tool_use_id(tool_use_id)
                        .name(name)
                        .input(input_doc)
                        .build()
                        .expect("valid tool use block"),
                )]
            }
            MessageContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let status = if is_error {
                    ToolResultStatus::Error
                } else {
                    ToolResultStatus::Success
                };
                vec![ContentBlock::ToolResult(
                    ToolResultBlock::builder()
                        .tool_use_id(tool_use_id)
                        .status(status)
                        .content(ToolResultContentBlock::Text(content))
                        .build()
                        .expect("valid tool result block"),
                )]
            }
            MessageContent::MultiPart(parts) => parts
                .into_iter()
                .map(|part| match part {
                    ContentPart::Text(text) => ContentBlock::Text(text),
                    ContentPart::Reasoning { text, signature } => {
                        ContentBlock::ReasoningContent(ReasoningContentBlock::ReasoningText(
                            ReasoningTextBlock::builder()
                                .text(text)
                                .set_signature(signature)
                                .build()
                                .expect("valid reasoning text block"),
                        ))
                    }
                    ContentPart::Image { data, mime_type } => image_content_block(data, &mime_type),
                    ContentPart::ToolUse {
                        tool_use_id,
                        name,
                        input,
                    } => {
                        let input_doc = json_to_document(input);
                        ContentBlock::ToolUse(
                            ToolUseBlock::builder()
                                .tool_use_id(tool_use_id)
                                .name(name)
                                .input(input_doc)
                                .build()
                                .expect("valid tool use block"),
                        )
                    }
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let status = if is_error {
                            ToolResultStatus::Error
                        } else {
                            ToolResultStatus::Success
                        };
                        ContentBlock::ToolResult(
                            ToolResultBlock::builder()
                                .tool_use_id(tool_use_id)
                                .status(status)
                                .content(ToolResultContentBlock::Text(content))
                                .build()
                                .expect("valid tool result block"),
                        )
                    }
                })
                .collect(),
        };

        let message = BedrockMessage::builder()
            .role(role)
            .set_content(Some(content_blocks))
            .build()
            .expect("valid message");

        result.push(message);
    }

    let mut messages = coalesce_consecutive_roles(result);

    // Add a cache point to the second-to-last message (the conversation prefix
    // that is stable between requests). This allows Bedrock to cache all prior
    // context and only process the latest message as new input tokens.
    if caching_config.enabled && messages.len() >= 2 {
        let cache_idx = messages.len() - 2;
        let msg = messages.remove(cache_idx);
        let mut content = msg.content().to_vec();

        let mut builder = CachePointBlock::builder().r#type(CachePointType::Default);
        if caching_config.extended_ttl_requested {
            builder = builder.ttl(CacheTtl::OneHour);
            log::info!("[bedrock] Using 1-hour cache TTL (ENABLE_PROMPT_CACHING_1H=1)");
        }

        content.push(ContentBlock::CachePoint(
            builder.build().expect("valid cache point"),
        ));
        let cached_msg = BedrockMessage::builder()
            .role(msg.role().clone())
            .set_content(Some(content))
            .build()
            .expect("valid message with cache point");
        messages.insert(cache_idx, cached_msg);
    }

    messages
}

fn image_content_block(data: Vec<u8>, mime_type: &str) -> ContentBlock {
    let format = match mime_type.to_ascii_lowercase().as_str() {
        "image/gif" | "gif" => ImageFormat::Gif,
        "image/jpeg" | "image/jpg" | "jpeg" | "jpg" => ImageFormat::Jpeg,
        "image/png" | "png" => ImageFormat::Png,
        "image/webp" | "webp" => ImageFormat::Webp,
        _ => {
            log::warn!(
                "[bedrock] Omitting image attachment with unsupported MIME type: {mime_type}"
            );
            return ContentBlock::Text(
                "[Image attachment omitted because its format is unsupported.]".to_string(),
            );
        }
    };

    ContentBlock::Image(
        ImageBlock::builder()
            .format(format)
            .source(ImageSource::Bytes(Blob::new(data)))
            .build()
            .expect("valid image block"),
    )
}

fn coalesce_consecutive_roles(messages: Vec<BedrockMessage>) -> Vec<BedrockMessage> {
    if messages.is_empty() {
        return messages;
    }

    let mut result: Vec<BedrockMessage> = Vec::new();

    for msg in messages {
        let should_merge = result
            .last()
            .map(|last| last.role() == msg.role())
            .unwrap_or(false);

        if should_merge {
            let last = result.pop().unwrap();
            let mut combined_content: Vec<ContentBlock> = last.content().to_vec();
            combined_content.extend(msg.content().to_vec());
            let merged = BedrockMessage::builder()
                .role(last.role().clone())
                .set_content(Some(combined_content))
                .build()
                .expect("valid merged message");
            result.push(merged);
        } else {
            result.push(msg);
        }
    }

    result
}

fn convert_system_prompt(
    system_prompt: Option<String>,
    _compact_summary: Option<String>,
    caching_config: &CachingConfig,
) -> Vec<SystemContentBlock> {
    let mut blocks = Vec::new();

    if let Some(prompt) = system_prompt {
        if !prompt.is_empty() {
            blocks.push(SystemContentBlock::Text(prompt));
        }
    }

    // Progressive summary is now prepended to the messages array instead of
    // being injected here. The compact_summary parameter is kept for API compat
    // but ignored.

    if !blocks.is_empty() && caching_config.enabled {
        let mut builder = CachePointBlock::builder().r#type(CachePointType::Default);
        if caching_config.extended_ttl_requested {
            builder = builder.ttl(CacheTtl::OneHour);
        }
        blocks.push(SystemContentBlock::CachePoint(
            builder.build().expect("valid cache point"),
        ));
    }

    blocks
}

fn build_inference_config(
    max_tokens: i32,
    temperature: Option<f32>,
    top_p: Option<f32>,
    stop_sequences: Option<Vec<String>>,
) -> InferenceConfiguration {
    let mut builder = InferenceConfiguration::builder().max_tokens(max_tokens);

    if let Some(temp) = temperature {
        builder = builder.temperature(temp);
    }
    if let Some(p) = top_p {
        builder = builder.top_p(p);
    }
    if let Some(stops) = stop_sequences {
        builder = builder.set_stop_sequences(Some(stops));
    }

    builder.build()
}

fn build_tool_config(
    tools: Vec<ToolDefinition>,
    caching_config: &CachingConfig,
) -> Option<ToolConfiguration> {
    if tools.is_empty() {
        return None;
    }

    let mut tool_specs: Vec<Tool> = tools
        .into_iter()
        .map(|tool| {
            let input_schema_doc = json_to_document(tool.input_schema);
            Tool::ToolSpec(
                ToolSpecification::builder()
                    .name(tool.name)
                    .description(tool.description)
                    .input_schema(ToolInputSchema::Json(input_schema_doc))
                    .build()
                    .expect("valid tool spec"),
            )
        })
        .collect();

    if caching_config.enabled {
        let mut builder = CachePointBlock::builder().r#type(CachePointType::Default);
        if caching_config.extended_ttl_requested {
            builder = builder.ttl(CacheTtl::OneHour);
        }
        tool_specs.push(Tool::CachePoint(
            builder.build().expect("valid cache point"),
        ));
    }

    Some(
        ToolConfiguration::builder()
            .set_tools(Some(tool_specs))
            .build()
            .expect("valid tool config"),
    )
}
