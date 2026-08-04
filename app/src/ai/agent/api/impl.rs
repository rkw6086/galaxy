use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use galaxy_core::features::FeatureFlag;
use warp_multi_agent_api as api;

use super::convert_to::convert_input;
use super::{ConvertToAPITypeError, RequestParams, ResponseStream, StreamEvent};
use crate::ai::agent::redaction;
use crate::ai::openai::translator as openai_translator;
use crate::ai::provider::ProviderConfig;
use crate::server::server_api::AIApiError;
use crate::terminal::model::session::SessionType;

pub async fn generate_multi_agent_output(
    provider_config: ProviderConfig,
    mut params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let supported_tools_override = params.supported_tools_override.take();
    let supported_tools = supported_tools_override
        .clone()
        .unwrap_or_else(|| get_supported_tools(&params));
    let supported_cli_agent_tools =
        supported_tools_override.unwrap_or_else(|| get_supported_cli_agent_tools(&params));
    if params.should_redact_secrets {
        redaction::redact_inputs(&mut params.input);
    }

    if let ProviderConfig::OpenAI(config) = &provider_config {
        if config.use_rig {
            return Ok(crate::ai::runtime::rig_openai_response_stream(
                config.clone(),
                params,
                supported_tools,
                supported_cli_agent_tools,
                cancellation_rx,
            ));
        }
    }

    let mut logging_metadata = HashMap::new();
    if let Some(ref metadata) = params.metadata {
        logging_metadata.insert(
            "is_autodetected_user_query".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::BoolValue(
                    metadata.is_autodetected_user_query,
                )),
            },
        );
        logging_metadata.insert(
            "entrypoint".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue(
                    metadata.entrypoint.entrypoint(),
                )),
            },
        );
        logging_metadata.insert(
            "is_auto_resume_after_error".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::BoolValue(
                    metadata.is_auto_resume_after_error,
                )),
            },
        );
    }

    let mut request = api::Request {
        task_context: Some(api::request::TaskContext {
            tasks: params.tasks,
        }),
        input: Some(convert_input(params.input)?),
        settings: Some(api::request::Settings {
            model_config: Some(api::request::settings::ModelConfig {
                base: params.model.clone().into(),
                cli_agent: params.cli_agent_model.clone().into(),
                computer_use_agent: params.computer_use_model.clone().into(),
                base_model_context_window_limit: params.context_window_limit.unwrap_or(0),
                ..Default::default()
            }),
            rules_enabled: params.is_memory_enabled,
            warp_drive_context_enabled: params.warp_drive_context_enabled,
            web_context_retrieval_enabled: true,
            supports_parallel_tool_calls: true,
            use_anthropic_text_editor_tools: false,
            planning_enabled: params.planning_enabled,
            supports_create_files: true,
            supported_tools: supported_tools.into_iter().map(Into::into).collect(),
            supports_long_running_commands: true,
            should_preserve_file_content_in_history: true,
            supports_todos_ui: true,
            supports_linked_code_blocks: FeatureFlag::LinkedCodeBlocks.is_enabled(),
            supports_started_child_task_message: true,
            // Galaxy's direct providers only receive tools with local schemas and
            // executors. Hosted-only suggestion/orchestration capability bits must
            // remain false so models do not plan around unavailable Warp services.
            supports_suggest_prompt: false,
            supports_read_image_files: FeatureFlag::ReadImageFiles.is_enabled(),
            supports_reasoning_message: true,
            api_keys: params.api_keys,
            autonomy_level: params.autonomy_level.into(),
            isolation_level: params.isolation_level.into(),
            web_search_enabled: params.web_search_enabled,
            supported_cli_agent_tools: supported_cli_agent_tools
                .into_iter()
                .map(Into::into)
                .collect(),
            supports_v4a_file_diffs: FeatureFlag::V4AFileDiffs.is_enabled(),
            supports_summarization_via_message_replacement:
                FeatureFlag::SummarizationViaMessageReplacement.is_enabled(),
            supports_bundled_skills: FeatureFlag::BundledSkills.is_enabled(),
            supports_research_agent: params.research_agent_enabled,
            supports_orchestration_v2: false,
            supports_background_computer_use: FeatureFlag::BackgroundComputerUse.is_enabled()
                && computer_use::background_supported(),
            custom_model_providers: params.custom_model_providers,
            custom_model_routers: params.custom_model_routers,
        }),
        metadata: Some(api::request::Metadata {
            logging: logging_metadata,
            conversation_id: params
                .conversation_token
                .as_ref()
                .map(|token| token.as_str().to_string())
                .unwrap_or_default(),
            ambient_agent_task_id: params
                .ambient_agent_task_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            forked_from_conversation_id: if params.conversation_token.is_none() {
                // We only include this param on our initial request to the server
                // (when the forked conversation has not been assigned a new id yet).
                params
                    .forked_from_conversation_token
                    .map(|token| token.as_str().to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            },
            parent_agent_id: params.parent_agent_id.unwrap_or_default(),
            agent_name: params.agent_name.unwrap_or_default(),
        }),
        existing_suggestions: params
            .existing_suggestions
            .map(|suggestions| suggestions.into()),
        mcp_context: params.mcp_context.map(Into::into),
    };

    match provider_config {
        ProviderConfig::OpenAI(config) => {
            let translator_request = openai_translator::TranslatorRequest {
                config,
                model_id: params.model.as_str().to_string(),
                root_task_id: params.root_task_id.clone(),
                message_history: params.message_history.clone(),
                tool_result_archive: params.tool_result_archive.clone(),
                progressive_summary: params.progressive_summary.clone(),
                messages_sent: params.messages_sent.clone(),
                global_rules: params.global_rules.clone(),
            };

            match openai_translator::execute(translator_request, &mut request).await {
                Ok(stream) => {
                    let output_stream = stream
                        .map(|event| event.map(StreamEvent::Response))
                        .take_until(cancellation_rx);
                    Ok(Box::pin(output_stream))
                }
                Err(e) => {
                    log::error!("[openai] Translator error: {e}");
                    let err = Arc::new(
                        crate::server::server_api::AIApiError::Stream {
                            stream_type: "openai_chat_completions",
                            source: anyhow::anyhow!("{e}"),
                        }
                        .into_quota_limit_if_provider_budget_exhausted(),
                    );
                    let (tx, rx) = async_channel::unbounded();
                    let _ = tx.send(Err(err)).await;
                    Ok(Box::pin(rx))
                }
            }
        }
        ProviderConfig::Bedrock(config) => {
            let translator_request = crate::ai::bedrock::translator::TranslatorRequest {
                config,
                model_id: params.model.as_str().to_string(),
                root_task_id: params.root_task_id.clone(),
                bedrock_message_history: params.message_history.clone(),
                bedrock_tool_result_archive: params.tool_result_archive.clone(),
                bedrock_progressive_summary: params.progressive_summary.clone(),
                bedrock_messages_sent: params.messages_sent.clone(),
                global_rules: params.global_rules.clone(),
            };

            match crate::ai::bedrock::translator::execute(translator_request, &mut request).await {
                Ok(stream) => {
                    let output_stream = stream
                        .map(|event| event.map(StreamEvent::Response))
                        .take_until(cancellation_rx);
                    Ok(Box::pin(output_stream))
                }
                Err(e) => {
                    log::error!("[bedrock] Translator error: {e}");
                    let err = Arc::new(crate::server::server_api::AIApiError::Stream {
                        stream_type: "bedrock",
                        source: anyhow::anyhow!("{e}"),
                    });
                    let (tx, rx) = async_channel::unbounded();
                    let _ = tx.send(Err(err)).await;
                    Ok(Box::pin(rx))
                }
            }
        }
        ProviderConfig::None => {
            // No provider configured — do not fall back to Warp's cloud API.
            let err = Arc::new(crate::server::server_api::AIApiError::Stream {
                stream_type: "none",
                source: anyhow::anyhow!(
                    "No AI provider configured. Enable Bedrock or OpenAI/LiteLLM in settings."
                ),
            });
            let (tx, rx) = async_channel::unbounded();
            let _ = tx.send(Err(err)).await;
            Ok(Box::pin(rx))
        }
    }
}

fn get_supported_tools(params: &RequestParams) -> Vec<api::ToolType> {
    let mut supported_tools = vec![
        api::ToolType::Grep,
        api::ToolType::FileGlob,
        api::ToolType::FileGlobV2,
        api::ToolType::ReadMcpResource,
        api::ToolType::CallMcpTool,
        api::ToolType::RunShellCommand,
        api::ToolType::Subagent,
        api::ToolType::WriteToLongRunningShellCommand,
        api::ToolType::ReadShellCommandOutput,
        api::ToolType::ReadDocuments,
        api::ToolType::CreateDocuments,
        api::ToolType::EditDocuments,
    ];

    if FeatureFlag::ConversationsAsContext.is_enabled() {
        supported_tools.push(api::ToolType::FetchConversation);
    }

    match params.session_context.session_type() {
        None | Some(SessionType::Local) => {
            supported_tools.extend(&[
                api::ToolType::ReadFiles,
                api::ToolType::ApplyFileDiffs,
                api::ToolType::SearchCodebase,
            ]);
        }
        Some(SessionType::WarpifiedRemote { host_id: Some(_) }) => {
            // Remote session with a known host — enable tools that route
            // through RemoteServerClient. The host_id is only populated
            // after a successful connection handshake, so its presence is a
            // sufficient proxy for client availability.
            supported_tools.extend(&[api::ToolType::ReadFiles, api::ToolType::ApplyFileDiffs]);
            if FeatureFlag::RemoteCodebaseIndexing.is_enabled() {
                supported_tools.push(api::ToolType::SearchCodebase);
            }
        }
        Some(SessionType::WarpifiedRemote { host_id: None }) => {}
    }

    if FeatureFlag::ListSkills.is_enabled() {
        supported_tools.push(api::ToolType::ReadSkill);
    }

    if FeatureFlag::AskUserQuestion.is_enabled() && params.ask_user_question_enabled {
        supported_tools.push(api::ToolType::AskUserQuestion);
    }

    supported_tools
}

fn get_supported_cli_agent_tools(params: &RequestParams) -> Vec<api::ToolType> {
    let mut supported_cli_agent_tools = vec![
        api::ToolType::WriteToLongRunningShellCommand,
        api::ToolType::ReadShellCommandOutput,
        api::ToolType::Grep,
        api::ToolType::FileGlob,
        api::ToolType::FileGlobV2,
    ];

    if FeatureFlag::TransferControlTool.is_enabled() {
        supported_cli_agent_tools.push(api::ToolType::TransferShellCommandControlToUser);
    }

    match params.session_context.session_type() {
        None | Some(SessionType::Local) => {
            supported_cli_agent_tools
                .extend(&[api::ToolType::ReadFiles, api::ToolType::SearchCodebase]);
        }
        Some(SessionType::WarpifiedRemote { host_id: Some(_) }) => {
            supported_cli_agent_tools.push(api::ToolType::ReadFiles);
            if FeatureFlag::RemoteCodebaseIndexing.is_enabled() {
                supported_cli_agent_tools.push(api::ToolType::SearchCodebase);
            }
        }
        Some(SessionType::WarpifiedRemote { host_id: None }) => {}
    }

    supported_cli_agent_tools
}

#[cfg(test)]
#[path = "impl_tests.rs"]
mod tests;
