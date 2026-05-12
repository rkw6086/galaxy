use std::{collections::HashMap, sync::Arc};

use crate::{ai::agent::redaction, terminal::model::session::SessionType};
use futures_util::StreamExt;
use galaxy_core::features::FeatureFlag;
use warp_multi_agent_api as api;

use crate::ai::bedrock::client::{BedrockClient, BedrockClientConfig};
use crate::ai::bedrock::diagnostic::BedrockDiagnosticLogger;
use crate::server::server_api::ServerApi;

use super::{convert_to::convert_input, ConvertToAPITypeError, RequestParams, ResponseStream};

pub async fn generate_multi_agent_output(
    _server_api: Arc<ServerApi>,
    bedrock_config: Option<BedrockClientConfig>,
    mut params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let supported_tools = params
        .supported_tools_override
        .take()
        .unwrap_or_else(|| get_supported_tools(&params));
    let supported_cli_agent_tools = get_supported_cli_agent_tools(&params);
    let mut logging_metadata = HashMap::new();
    if let Some(metadata) = params.metadata {
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

    if params.should_redact_secrets {
        redaction::redact_inputs(&mut params.input);
    }

    let mut api_keys = params.api_keys;
    if let Some(api_keys) = &mut api_keys {
        api_keys.allow_use_of_warp_credits = params.allow_use_of_warp_credits_with_byok;
    }

    let request = api::Request {
        task_context: Some(api::request::TaskContext {
            tasks: params.tasks,
        }),
        input: Some(convert_input(params.input)?),
        settings: Some(api::request::Settings {
            model_config: Some(api::request::settings::ModelConfig {
                base: params.model.into(),
                cli_agent: params.cli_agent_model.into(),
                computer_use_agent: params.computer_use_model.into(),
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
            supports_suggest_prompt: true,
            supports_read_image_files: FeatureFlag::ReadImageFiles.is_enabled(),
            supports_reasoning_message: true,
            api_keys,
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
            supports_orchestration_v2: FeatureFlag::OrchestrationV2.is_enabled(),
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
                // (when the forked conversation has not been asigned a new id yet).
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

    if let Some(config) = bedrock_config {
        let model_id_for_fallback_check = request
            .settings
            .as_ref()
            .and_then(|s| s.model_config.as_ref())
            .map(|mc| mc.base.clone())
            .unwrap_or_default();
        let is_arn = model_id_for_fallback_check.starts_with("arn:");
        let fallback_to_warp = config.fallback_to_warp && !is_arn;
        if is_arn && config.fallback_to_warp {
            log::info!(
                "[bedrock] Fallback disabled for ARN-based model (not available on Warp server)"
            );
        }
        match BedrockClient::from_config(config).await {
            Ok(bedrock) => {
                let task_id = params.root_task_id.clone().unwrap_or_else(|| {
                    request
                        .task_context
                        .as_ref()
                        .and_then(|tc| tc.tasks.first())
                        .map(|t| t.id.clone())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
                });

                log::info!("[bedrock] Starting stream with task_id={task_id}");

                let needs_create_task = request
                    .task_context
                    .as_ref()
                    .map(|tc| tc.tasks.is_empty())
                    .unwrap_or(true);

                log::info!("[bedrock] needs_create_task={needs_create_task}");

                let mut model_id = request
                    .settings
                    .as_ref()
                    .and_then(|s| s.model_config.as_ref())
                    .map(|mc| mc.base.clone())
                    .unwrap_or_default();

                if model_id.is_empty() || model_id == "auto" {
                    model_id = "us.anthropic.claude-sonnet-4-20250514-v1:0".to_string();
                }

                log::info!("[bedrock] Model: {model_id}");

                let diagnostic_logger =
                    BedrockDiagnosticLogger::try_new(&model_id, "", "", &task_id).map(Arc::new);

                if let Some(ref logger) = diagnostic_logger {
                    logger.log_protobuf_input(&request);
                }

                // Build message list from bedrock_message_history + new input messages.
                // The history contains all prior messages. We extract only NEW messages
                // from the current request input and append them.
                let new_input_messages =
                    crate::ai::bedrock::convert_request::extract_new_input_messages(&request);

                let mut messages = params.bedrock_message_history.clone();
                if !new_input_messages.is_empty() {
                    log::info!(
                        "[bedrock] Appending {} new input messages to history of {}",
                        new_input_messages.len(),
                        messages.len()
                    );
                    messages.extend(new_input_messages);
                }

                let system_prompt =
                    crate::ai::bedrock::convert_request::extract_system_prompt(&request);
                let tools = crate::ai::bedrock::convert_request::extract_tools(&request);

                log::info!(
                    "[bedrock] Sending {} messages, system_prompt={}, tools={}",
                    messages.len(),
                    system_prompt.is_some(),
                    tools.len()
                );

                for (i, msg) in messages.iter().enumerate() {
                    let content_desc = match &msg.content {
                        crate::ai::bedrock::convert::MessageContent::Text(t) => {
                            format!("Text({}chars)", t.len())
                        }
                        crate::ai::bedrock::convert::MessageContent::ToolUse {
                            tool_use_id,
                            name,
                            ..
                        } => {
                            format!("ToolUse(name={}, id={})", name, tool_use_id)
                        }
                        crate::ai::bedrock::convert::MessageContent::ToolResult {
                            tool_use_id,
                            is_error,
                            ..
                        } => {
                            format!("ToolResult(id={}, is_error={})", tool_use_id, is_error)
                        }
                        crate::ai::bedrock::convert::MessageContent::MultiPart(parts) => {
                            let part_descs: Vec<String> = parts
                                .iter()
                                .map(|p| match p {
                                    crate::ai::bedrock::convert::ContentPart::Text(t) => {
                                        format!("Text({})", t.len())
                                    }
                                    crate::ai::bedrock::convert::ContentPart::ToolUse {
                                        name,
                                        tool_use_id,
                                        ..
                                    } => format!("ToolUse({},{})", name, tool_use_id),
                                    crate::ai::bedrock::convert::ContentPart::ToolResult {
                                        tool_use_id,
                                        ..
                                    } => format!("ToolResult({})", tool_use_id),
                                })
                                .collect();
                            format!("MultiPart[{}]", part_descs.join(", "))
                        }
                    };
                    log::debug!(
                        "[bedrock] msg[{}]: role={:?}, content={}",
                        i,
                        msg.role,
                        content_desc
                    );
                }

                match bedrock
                    .converse_stream(
                        &model_id,
                        &task_id,
                        needs_create_task,
                        messages.clone(),
                        system_prompt,
                        tools,
                        64000,
                        None,
                        true,
                        diagnostic_logger,
                        params.bedrock_messages_sent.clone(),
                    )
                    .await
                {
                    Ok(stream) => {
                        // Store the input messages we sent so the controller can
                        // persist them. The stream will append the assistant response.
                        if let Ok(mut sent) = params.bedrock_messages_sent.lock() {
                            *sent = messages;
                        }
                        let output_stream = stream.take_until(cancellation_rx);
                        return Ok(Box::pin(output_stream));
                    }
                    Err(e) => {
                        if fallback_to_warp {
                            log::warn!("Bedrock stream failed, falling back to server: {e}");
                        } else {
                            let err = Arc::new(crate::server::server_api::AIApiError::Stream {
                                stream_type: "bedrock_converse",
                                source: anyhow::anyhow!("{e}"),
                            });
                            let (tx, rx) = async_channel::unbounded();
                            let _ = tx.send(Err(err)).await;
                            return Ok(Box::pin(rx));
                        }
                    }
                }
            }
            Err(e) => {
                if fallback_to_warp {
                    log::warn!("Bedrock client creation failed, falling back to server: {e}");
                } else {
                    let err = Arc::new(crate::server::server_api::AIApiError::Stream {
                        stream_type: "bedrock_converse",
                        source: anyhow::anyhow!("{e}"),
                    });
                    let (tx, rx) = async_channel::unbounded();
                    let _ = tx.send(Err(err)).await;
                    return Ok(Box::pin(rx));
                }
            }
        }
    }

    log::error!("[bedrock] No Bedrock config available and server fallback is disabled. Cannot process request.");
    let err = Arc::new(crate::server::server_api::AIApiError::Stream {
        stream_type: "bedrock_converse",
        source: anyhow::anyhow!("No AI backend available. Please configure Bedrock credentials in Settings > AI."),
    });
    let (tx, rx) = async_channel::unbounded();
    let _ = tx.send(Err(err)).await;
    Ok(Box::pin(rx))
}

fn get_supported_tools(params: &RequestParams) -> Vec<api::ToolType> {
    let mut supported_tools = vec![
        api::ToolType::Grep,
        api::ToolType::FileGlob,
        api::ToolType::FileGlobV2,
        api::ToolType::ReadMcpResource,
        api::ToolType::CallMcpTool,
        api::ToolType::InitProject,
        api::ToolType::OpenCodeReview,
        api::ToolType::RunShellCommand,
        api::ToolType::SuggestNewConversation,
        api::ToolType::Subagent,
        api::ToolType::WriteToLongRunningShellCommand,
        api::ToolType::ReadShellCommandOutput,
        api::ToolType::ReadDocuments,
        api::ToolType::CreateDocuments,
        api::ToolType::EditDocuments,
        api::ToolType::SuggestPrompt,
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

            if FeatureFlag::ArtifactCommand.is_enabled() {
                supported_tools.push(api::ToolType::UploadFileArtifact);
            }
        }
        Some(SessionType::WarpifiedRemote { host_id: Some(_) }) => {
            // Remote session with a known host — enable tools that route
            // through RemoteServerClient. The host_id is only populated
            // after a successful connection handshake, so its presence is a
            // sufficient proxy for client availability.
            // SearchCodebase remains disabled (follow-up work).
            supported_tools.extend(&[api::ToolType::ReadFiles, api::ToolType::ApplyFileDiffs]);
        }
        Some(SessionType::WarpifiedRemote { host_id: None }) => {
            // Feature flag off or not yet connected — no remote tools.
        }
    }

    if FeatureFlag::AgentModeComputerUse.is_enabled() && params.computer_use_enabled {
        supported_tools.extend(&[api::ToolType::UseComputer]);
        supported_tools.extend(&[api::ToolType::RequestComputerUse])
    }

    if FeatureFlag::PRCommentsSlashCommand.is_enabled() {
        supported_tools.push(api::ToolType::InsertReviewComments);
    }

    if FeatureFlag::ListSkills.is_enabled() {
        supported_tools.push(api::ToolType::ReadSkill);
    }

    if params.orchestration_enabled {
        supported_tools.push(if FeatureFlag::OrchestrationV2.is_enabled() {
            api::ToolType::StartAgentV2
        } else {
            api::ToolType::StartAgent
        });
        supported_tools.push(api::ToolType::SendMessageToAgent);
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
        }
        Some(SessionType::WarpifiedRemote { host_id: None }) => {}
    }

    supported_cli_agent_tools
}

#[cfg(test)]
#[path = "impl_tests.rs"]
mod tests;
