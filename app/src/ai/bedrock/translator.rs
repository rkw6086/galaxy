use std::sync::{Arc, Mutex};

use warp_multi_agent_api as api;

use crate::ai::agent::api::ResponseStream;
use crate::ai::bedrock::client::{BedrockClient, BedrockClientConfig, BedrockError};
use crate::ai::bedrock::convert::ConversationMessage;
use crate::ai::bedrock::diagnostic::BedrockDiagnosticLogger;
use crate::ai::bedrock::request_translator;

pub struct TranslatorRequest {
    pub config: BedrockClientConfig,
    pub model_id: String,
    pub root_task_id: Option<String>,
    pub bedrock_message_history: Vec<ConversationMessage>,
    pub bedrock_compact_summary: Option<String>,
    pub bedrock_messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
    pub is_summarization: bool,
}

pub async fn execute(
    params: TranslatorRequest,
    request: &mut api::Request,
) -> Result<ResponseStream, BedrockError> {
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

    let mut model_id = params.model_id;
    if model_id.is_empty() || model_id == "auto" {
        model_id = "us.anthropic.claude-opus-4-6".to_string();
    }

    log::info!("[bedrock] Translator: model={model_id}, task_id={task_id}, needs_create_task={needs_create_task}");

    let diagnostic_logger =
        BedrockDiagnosticLogger::try_new(&model_id, "", "", &task_id).map(Arc::new);

    if let Some(ref logger) = diagnostic_logger {
        logger.log_protobuf_input(request);
    }

    request_translator::inject_input_messages_into_task(request);

    let new_input_messages = request_translator::extract_new_input_messages(request);

    let mut messages = params.bedrock_message_history;
    if !new_input_messages.is_empty() {
        log::info!(
            "[bedrock] Appending {} new input messages to history of {}",
            new_input_messages.len(),
            messages.len()
        );
        messages.extend(new_input_messages);
    }

    request_translator::sanitize_messages_for_bedrock(&mut messages);

    let system_prompt = request_translator::extract_system_prompt(request);
    let compact_summary = params.bedrock_compact_summary;
    let tools = request_translator::extract_tools(request);

    log::info!(
        "[bedrock] Sending {} messages, system_prompt={}, compact_summary={}, tools={}",
        messages.len(),
        system_prompt.is_some(),
        compact_summary.is_some(),
        tools.len()
    );

    for (i, msg) in messages.iter().enumerate() {
        let content_desc = describe_message_content(&msg.content);
        log::info!("[bedrock] msg[{}]: role={:?}, content={}", i, msg.role, content_desc);
    }

    let user_query_text = request_translator::extract_user_query_text(request);

    let stream = bedrock
        .converse_stream(
            &model_id,
            &task_id,
            needs_create_task,
            messages.clone(),
            system_prompt,
            compact_summary,
            tools,
            64000,
            None,
            cross_region_inference,
            user_query_text,
            diagnostic_logger,
            params.bedrock_messages_sent.clone(),
            params.is_summarization,
        )
        .await?;

    if let Ok(mut sent) = params.bedrock_messages_sent.lock() {
        *sent = messages;
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
