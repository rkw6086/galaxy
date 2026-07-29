use base64::engine::general_purpose;
use base64::Engine as _;
use serde_json::{json, Value as JsonValue};

use crate::ai::provider::types::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
};

/// Sanitizes a tool_use_id to match Bedrock's required pattern `^[a-zA-Z0-9_-]+$`.
/// LiteLLM may proxy to Bedrock which rejects IDs with characters outside this set.
/// Replaces any invalid character with an underscore.
fn sanitize_tool_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn build_openai_request(
    messages: Vec<ConversationMessage>,
    system_prompt: Option<String>,
    tools: Vec<ToolDefinition>,
    max_tokens: i32,
    temperature: Option<f32>,
    model: &str,
) -> JsonValue {
    let mut openai_messages: Vec<JsonValue> = Vec::new();

    if let Some(prompt) = system_prompt {
        if !prompt.is_empty() {
            openai_messages.push(json!({
                "role": "system",
                "content": prompt,
            }));
        }
    }

    for msg in messages {
        match convert_message(msg) {
            ConvertedMessages::Single(m) => openai_messages.push(m),
            ConvertedMessages::Multiple(ms) => openai_messages.extend(ms),
        }
    }

    let mut request = json!({
        "model": model,
        "messages": openai_messages,
        "max_tokens": max_tokens,
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    if let Some(temp) = temperature {
        request["temperature"] = json!(temp);
    }

    if !tools.is_empty() {
        let tool_defs: Vec<JsonValue> = tools.into_iter().map(convert_tool_definition).collect();
        request["tools"] = json!(tool_defs);
    }

    request
}

enum ConvertedMessages {
    Single(JsonValue),
    Multiple(Vec<JsonValue>),
}

enum UserContentPart {
    Text(String),
    Image { data: Vec<u8>, mime_type: String },
}

fn convert_message(msg: ConversationMessage) -> ConvertedMessages {
    match msg.role {
        MessageRole::User => convert_user_message(msg.content),
        MessageRole::Assistant => convert_assistant_message(msg.content),
    }
}

fn convert_user_message(content: MessageContent) -> ConvertedMessages {
    match content {
        MessageContent::Text(text) => ConvertedMessages::Single(json!({
            "role": "user",
            "content": text,
        })),
        MessageContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut msg = json!({
                "role": "tool",
                "tool_call_id": sanitize_tool_id(&tool_use_id),
                "content": content,
            });
            if is_error {
                msg["content"] = json!(format!("[ERROR] {content}"));
            }
            ConvertedMessages::Single(msg)
        }
        MessageContent::ToolUse { .. } => {
            // User messages shouldn't contain tool_use, but handle gracefully
            ConvertedMessages::Single(json!({
                "role": "user",
                "content": "[unexpected tool_use in user message]",
            }))
        }
        MessageContent::MultiPart(parts) => {
            let mut messages = Vec::new();
            let mut user_content_parts = Vec::new();

            for part in parts {
                match part {
                    ContentPart::Text(text) => {
                        user_content_parts.push(UserContentPart::Text(text));
                    }
                    ContentPart::Image { data, mime_type } => {
                        user_content_parts.push(UserContentPart::Image { data, mime_type });
                    }
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        flush_user_content(&mut messages, &mut user_content_parts);
                        let result_content = if is_error {
                            format!("[ERROR] {content}")
                        } else {
                            content
                        };
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": sanitize_tool_id(&tool_use_id),
                            "content": result_content,
                        }));
                    }
                    ContentPart::ToolUse { .. } => {
                        user_content_parts.push(UserContentPart::Text(
                            "[unexpected tool_use in user message]".to_string(),
                        ));
                    }
                }
            }

            flush_user_content(&mut messages, &mut user_content_parts);

            if messages.len() == 1 {
                ConvertedMessages::Single(messages.into_iter().next().unwrap())
            } else {
                ConvertedMessages::Multiple(messages)
            }
        }
    }
}

fn convert_assistant_message(content: MessageContent) -> ConvertedMessages {
    match content {
        MessageContent::Text(text) => ConvertedMessages::Single(json!({
            "role": "assistant",
            "content": text,
        })),
        MessageContent::ToolUse {
            tool_use_id,
            name,
            input,
        } => ConvertedMessages::Single(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": sanitize_tool_id(&tool_use_id),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": input.to_string(),
                }
            }]
        })),
        MessageContent::ToolResult { .. } => {
            // Assistant messages shouldn't contain tool_result
            ConvertedMessages::Single(json!({
                "role": "assistant",
                "content": "[unexpected tool_result in assistant message]",
            }))
        }
        MessageContent::MultiPart(parts) => {
            let mut text_content = String::new();
            let mut tool_calls: Vec<JsonValue> = Vec::new();

            for part in parts {
                match part {
                    ContentPart::Text(text) => {
                        if !text_content.is_empty() {
                            text_content.push('\n');
                        }
                        text_content.push_str(&text);
                    }
                    ContentPart::ToolUse {
                        tool_use_id,
                        name,
                        input,
                    } => {
                        tool_calls.push(json!({
                            "id": sanitize_tool_id(&tool_use_id),
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": input.to_string(),
                            }
                        }));
                    }
                    ContentPart::ToolResult { .. } => {}
                    ContentPart::Image { .. } => {
                        if !text_content.is_empty() {
                            text_content.push('\n');
                        }
                        text_content.push_str("[unexpected image in assistant message]");
                    }
                }
            }

            let mut msg = json!({ "role": "assistant" });
            if !text_content.is_empty() {
                msg["content"] = json!(text_content);
            } else {
                msg["content"] = JsonValue::Null;
            }
            if !tool_calls.is_empty() {
                msg["tool_calls"] = json!(tool_calls);
            }

            ConvertedMessages::Single(msg)
        }
    }
}

fn flush_user_content(messages: &mut Vec<JsonValue>, content_parts: &mut Vec<UserContentPart>) {
    if content_parts.is_empty() {
        return;
    }

    let has_image = content_parts
        .iter()
        .any(|part| matches!(part, UserContentPart::Image { .. }));
    let content = if has_image {
        JsonValue::Array(
            std::mem::take(content_parts)
                .into_iter()
                .map(|part| match part {
                    UserContentPart::Text(text) => json!({
                        "type": "text",
                        "text": text,
                    }),
                    UserContentPart::Image { data, mime_type } => {
                        let data = general_purpose::STANDARD.encode(data);
                        json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime_type};base64,{data}"),
                            },
                        })
                    }
                })
                .collect(),
        )
    } else {
        JsonValue::String(
            std::mem::take(content_parts)
                .into_iter()
                .map(|part| match part {
                    UserContentPart::Text(text) => text,
                    UserContentPart::Image { .. } => unreachable!(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    messages.push(json!({
        "role": "user",
        "content": content,
    }));
}

fn convert_tool_definition(tool: ToolDefinition) -> JsonValue {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}
