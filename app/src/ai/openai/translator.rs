use std::sync::{Arc, Mutex};

use warp_multi_agent_api as api;

use super::client::{OpenAIClient, OpenAIClientConfig, OpenAIError};
use super::convert::build_openai_request;
use super::request_translator::sanitize_messages_for_openai;
use super::response_translator::openai_stream_to_response_events;
use crate::ai::agent::api::ResponseStream;
use crate::ai::bedrock::request_translator;
use crate::ai::provider::types::{ConversationMessage, MessageContent, MessageRole};

pub struct TranslatorRequest {
    pub config: OpenAIClientConfig,
    pub model_id: String,
    pub root_task_id: Option<String>,
    pub message_history: Vec<ConversationMessage>,
    pub tool_result_archive: Vec<ConversationMessage>,
    pub progressive_summary: Option<String>,
    pub messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
}

pub async fn execute(
    params: TranslatorRequest,
    request: &mut api::Request,
) -> Result<ResponseStream, OpenAIError> {
    let client = OpenAIClient::from_config(params.config.clone());

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

    let model_id = if params.model_id.is_empty() || params.model_id == "auto" {
        params
            .config
            .model
            .clone()
            .unwrap_or_else(|| "anthropic/claude-sonnet-4-6".to_string())
    } else {
        // If a model override is configured in settings, use it
        params
            .config
            .model
            .clone()
            .unwrap_or_else(|| params.model_id.clone())
    };

    log::info!(
        "[openai] Translator: model={model_id}, task_id={task_id}, needs_create_task={needs_create_task}"
    );

    request_translator::inject_input_messages_into_task(request);

    let new_input_messages = request_translator::extract_new_input_messages(request);
    let new_input_count = new_input_messages.len();

    let mut messages = Vec::new();

    // Prepend progressive summary as first message pair if present
    if let Some(ref summary) = params.progressive_summary {
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

    let history_len = params.message_history.len();
    messages.extend(params.message_history);

    if !new_input_messages.is_empty() {
        log::info!(
            "[openai] Appending {} new input messages to history of {}",
            new_input_messages.len(),
            history_len
        );
        messages.extend(new_input_messages);
    }

    sanitize_messages_for_openai(&mut messages);

    let system_prompt = request_translator::extract_system_prompt(request);
    let tools = request_translator::extract_tools(request);

    log::info!(
        "[openai] Sending {} messages, system_prompt={}, tools={}",
        messages.len(),
        system_prompt.is_some(),
        tools.len()
    );

    let user_query_text = request_translator::extract_user_query_text(request);

    let request_body = build_openai_request(
        messages.clone(),
        system_prompt,
        tools,
        64000,
        None,
        &model_id,
    );

    let byte_stream = client.chat_completions_stream(request_body).await?;

    // Store the message history for the controller
    if let Ok(mut sent) = params.messages_sent.lock() {
        let persistent_count = history_len + new_input_count;
        if persistent_count > 0 && messages.len() >= persistent_count {
            *sent = messages.split_off(messages.len() - persistent_count);
        } else {
            *sent = messages;
        }
    }

    let stream = openai_stream_to_response_events(
        byte_stream,
        task_id,
        needs_create_task,
        user_query_text,
        params.messages_sent.clone(),
        model_id,
        params.tool_result_archive,
    );

    Ok(stream)
}
