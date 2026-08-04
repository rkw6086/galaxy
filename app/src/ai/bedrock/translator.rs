#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use warp_multi_agent_api as api;

use crate::ai::agent::api::LegacyResponseStream;
use crate::ai::bedrock::client::{BedrockClient, BedrockClientConfig, BedrockError};
use crate::ai::bedrock::convert::ConversationMessage;
use crate::ai::bedrock::diagnostic::BedrockDiagnosticLogger;
use crate::ai::bedrock::request_translator;

pub struct TranslatorRequest {
    pub config: BedrockClientConfig,
    pub model_id: String,
    pub root_task_id: Option<String>,
    pub bedrock_message_history: Vec<ConversationMessage>,
    pub bedrock_tool_result_archive: Vec<ConversationMessage>,
    pub bedrock_progressive_summary: Option<String>,
    pub bedrock_messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
    /// Global rules (name, content) from the local CloudModel.
    pub global_rules: Vec<(String, String)>,
}

pub async fn execute(
    params: TranslatorRequest,
    request: &mut api::Request,
) -> Result<LegacyResponseStream, BedrockError> {
    let config = params.config.with_external_fallbacks();
    let cross_region_inference = config.cross_region_inference;
    let bedrock = BedrockClient::from_config(config).await?;

    let task_id = params.root_task_id.unwrap_or_else(|| {
        request
            .task_context
            .as_ref()
            .and_then(|tc| tc.tasks.first())
            .map(|t| t.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    });

    let needs_create_task = request
        .task_context
        .as_ref()
        .map(|tc| tc.tasks.is_empty())
        .unwrap_or(true);

    // Use the model from params (selected in UI or defaulted from ANTHROPIC_MODEL)
    let mut model_id = params.model_id;
    if model_id.is_empty() || model_id == "auto" {
        // Fall back to default if nothing is set
        model_id = "us.anthropic.claude-opus-4-6[1m]".to_string();
    }

    log::info!("[bedrock] Translator: model={model_id}, task_id={task_id}, needs_create_task={needs_create_task}");

    let diagnostic_logger =
        BedrockDiagnosticLogger::try_new(&model_id, "", "", &task_id).map(Arc::new);

    if let Some(ref logger) = diagnostic_logger {
        logger.log_protobuf_input(request);
    }

    request_translator::inject_input_messages_into_task(request);

    let new_input_messages = request_translator::extract_new_input_messages(request);
    let new_input_count = new_input_messages.len();

    let mut messages = Vec::new();

    // Prepend progressive summary as the first message pair if present
    if let Some(ref summary) = params.bedrock_progressive_summary {
        use crate::ai::bedrock::convert::{MessageContent, MessageRole};
        messages.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(format!(
                "<conversation-history-summary>\n{}\n</conversation-history-summary>\n\n\
                 The above summarizes earlier conversation history. The detailed messages below are the most recent exchanges.",
                summary
            )),
        });
        messages.push(ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text(
                "Understood, I have the prior context. Continuing with the recent conversation."
                    .to_string(),
            ),
        });
    }

    let history_len = params.bedrock_message_history.len();
    messages.extend(params.bedrock_message_history);

    if !new_input_messages.is_empty() {
        log::info!(
            "[bedrock] Appending {} new input messages to history of {}",
            new_input_messages.len(),
            history_len
        );
        messages.extend(new_input_messages);
    }

    for message in &mut messages {
        message.truncate_tool_results_for_provider_request();
    }

    request_translator::sanitize_messages_for_bedrock(&mut messages);

    let system_prompt = request_translator::extract_system_prompt(request, &params.global_rules);
    let tools = request_translator::extract_tools(request);

    log::info!(
        "[bedrock] Sending {} messages, system_prompt={}, progressive_summary={}, tools={}",
        messages.len(),
        system_prompt.is_some(),
        params.bedrock_progressive_summary.is_some(),
        tools.len()
    );

    for (i, msg) in messages.iter().enumerate() {
        let content_desc = describe_message_content(&msg.content);
        log::info!(
            "[bedrock] msg[{}]: role={:?}, content={}",
            i,
            msg.role,
            content_desc
        );
    }

    let user_query_text = request_translator::extract_user_query_text(request);

    let stream = bedrock
        .converse_stream(
            &model_id,
            &task_id,
            needs_create_task,
            messages.clone(),
            system_prompt,
            None, // progressive summary is in messages array, not system prompt
            tools,
            64000,
            None,
            cross_region_inference,
            user_query_text,
            diagnostic_logger,
            params.bedrock_messages_sent.clone(),
            params.bedrock_tool_result_archive,
        )
        .await?;

    if let Ok(mut sent) = params.bedrock_messages_sent.lock() {
        // Only persist the actual conversation history (history + new inputs), not the
        // ephemeral prepended summary pair, so we don't duplicate the summary on every
        // subsequent write-back. The summary is prepended at request time each turn.
        let persistent_count = history_len + new_input_count;
        if persistent_count > 0 && messages.len() >= persistent_count {
            *sent = messages.split_off(messages.len() - persistent_count);
        } else {
            *sent = messages;
        }
    }

    Ok(stream)
}

fn describe_message_content(content: &crate::ai::bedrock::convert::MessageContent) -> String {
    use crate::ai::bedrock::convert::{ContentPart, MessageContent};
    match content {
        MessageContent::Text(t) => format!("Text({}chars)", t.len()),
        MessageContent::ToolUse {
            tool_use_id, name, ..
        } => format!("ToolUse(name={}, id={})", name, tool_use_id),
        MessageContent::ToolResult {
            tool_use_id,
            is_error,
            ..
        } => format!("ToolResult(id={}, is_error={})", tool_use_id, is_error),
        MessageContent::MultiPart(parts) => {
            let part_descs: Vec<String> = parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text(t) => format!("Text({})", t.len()),
                    ContentPart::Reasoning { text, signature } => {
                        format!(
                            "Reasoning({}chars,signed={})",
                            text.len(),
                            signature.is_some()
                        )
                    }
                    ContentPart::Image { data, mime_type } => {
                        format!("Image({mime_type},{}bytes)", data.len())
                    }
                    ContentPart::ToolUse {
                        name, tool_use_id, ..
                    } => format!("ToolUse({},{})", name, tool_use_id),
                    ContentPart::ToolResult { tool_use_id, .. } => {
                        format!("ToolResult({})", tool_use_id)
                    }
                })
                .collect();
            format!("MultiPart[{}]", part_descs.join(", "))
        }
    }
}
