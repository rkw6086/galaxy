use std::sync::{Arc, Mutex};

use warp_multi_agent_api as api;

use super::client::{OpenAIClient, OpenAIClientConfig, OpenAIError};
use super::convert::build_openai_request;
use super::request_translator::sanitize_messages_for_openai;
use super::response_translator::{openai_stream_to_response_events, OpenAIStreamContext};
use crate::ai::agent::api::ResponseStream;
use crate::ai::bedrock::request_translator;
use crate::ai::provider::types::{ConversationMessage, MessageContent, MessageRole};

const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 64_000;

pub struct TranslatorRequest {
    pub config: OpenAIClientConfig,
    pub model_id: String,
    pub root_task_id: Option<String>,
    pub message_history: Vec<ConversationMessage>,
    pub tool_result_archive: Vec<ConversationMessage>,
    pub progressive_summary: Option<String>,
    pub messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
    /// Global rules (name, content) from the local CloudModel.
    pub global_rules: Vec<(String, String)>,
}

pub(crate) struct PreparedTurn {
    pub(crate) task_id: String,
    pub(crate) needs_create_task: bool,
    pub(crate) user_query: Option<String>,
    pub(crate) messages: Vec<ConversationMessage>,
    pub(crate) system_prompt: Option<String>,
    pub(crate) tools: Vec<crate::ai::provider::types::ToolDefinition>,
    pub(crate) model_id: String,
    pub(crate) persistent_message_count: usize,
}

pub(crate) fn prepare_turn(params: &TranslatorRequest, request: &mut api::Request) -> PreparedTurn {
    let task_id = params.root_task_id.clone().unwrap_or_else(|| {
        request
            .task_context
            .as_ref()
            .and_then(|tc| tc.tasks.first())
            .map(|task| task.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    });

    let needs_create_task = request
        .task_context
        .as_ref()
        .map(|task_context| task_context.tasks.is_empty())
        .unwrap_or(true);
    let model_id = if params.model_id.is_empty() || params.model_id == "auto" {
        params
            .config
            .model
            .clone()
            .unwrap_or_else(|| "anthropic/claude-sonnet-4-6".to_string())
    } else {
        params
            .config
            .model
            .clone()
            .unwrap_or_else(|| params.model_id.clone())
    };

    request_translator::inject_input_messages_into_task(request);
    let new_input_messages = request_translator::extract_new_input_messages(request);
    let persistent_message_count = params.message_history.len() + new_input_messages.len();
    let mut messages = Vec::new();

    if let Some(summary) = &params.progressive_summary {
        messages.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(format!(
                "<conversation-history-summary>\n{summary}\n</conversation-history-summary>\n\n\
                 The above summarizes earlier conversation history. The detailed messages below are the most recent exchanges."
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

    messages.extend(params.message_history.clone());
    messages.extend(new_input_messages);
    for message in &mut messages {
        message.truncate_tool_results_for_provider_request();
    }
    sanitize_messages_for_openai(&mut messages);

    PreparedTurn {
        task_id,
        needs_create_task,
        user_query: request_translator::extract_user_query_text(request),
        messages,
        system_prompt: request_translator::extract_system_prompt(request, &params.global_rules),
        tools: request_translator::extract_tools(request),
        model_id,
        persistent_message_count,
    }
}

pub async fn execute(
    params: TranslatorRequest,
    request: &mut api::Request,
) -> Result<ResponseStream, OpenAIError> {
    let client = OpenAIClient::from_config(params.config.clone());
    let PreparedTurn {
        task_id,
        needs_create_task,
        user_query,
        mut messages,
        system_prompt,
        tools,
        model_id,
        persistent_message_count,
    } = prepare_turn(&params, request);

    log::info!(
        "[openai] Translator: model={model_id}, task_id={task_id}, needs_create_task={needs_create_task}"
    );

    log::info!(
        "[openai] Sending {} messages, system_prompt={}, tools={}",
        messages.len(),
        system_prompt.is_some(),
        tools.len()
    );

    let max_output_tokens = params
        .config
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
        .min(i32::MAX as u32) as i32;

    let request_body = build_openai_request(
        messages.clone(),
        system_prompt,
        tools,
        max_output_tokens,
        None,
        &model_id,
    );

    let byte_stream = client.chat_completions_stream(request_body).await?;

    // Store the message history for the controller
    if let Ok(mut sent) = params.messages_sent.lock() {
        if persistent_message_count > 0 && messages.len() >= persistent_message_count {
            *sent = messages.split_off(messages.len() - persistent_message_count);
        } else {
            *sent = messages;
        }
    }

    let stream = openai_stream_to_response_events(
        byte_stream,
        OpenAIStreamContext {
            task_id,
            needs_create_task,
            user_query,
            messages_sent: params.messages_sent.clone(),
            model_id,
            max_context_tokens: params.config.max_input_tokens,
            tool_result_archive: params.tool_result_archive,
        },
    );

    Ok(stream)
}
