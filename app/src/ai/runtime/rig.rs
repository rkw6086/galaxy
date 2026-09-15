use std::collections::HashMap;
use std::sync::Arc;

use galaxy_agent_core::{AgentRuntime, ToolCall, TurnRequest};
use galaxy_agent_rig::{
    AnthropicRuntime, AnthropicRuntimeConfig, BedrockRigConfig, BedrockRuntime,
    ChatGPTSubscriptionRuntime, ChatGPTSubscriptionRuntimeConfig, GeminiRuntime,
    GeminiRuntimeConfig, OpenAICompatibleRuntime, OpenAICompatibleRuntimeConfig, VertexAiRuntime,
    VertexAiRuntimeConfig,
};
use galaxy_bedrock_model_catalog::model_metadata;
use uuid::Uuid;
use warp_multi_agent_api::ToolType;

use super::rig_request::{
    add_orchestration_model_options, prepare_bedrock_rig_turn_for_mode, prepare_rig_turn,
    prepare_rig_turn_for_mode, MCPToolTarget, OrchestrationModelOption, PreparedRigTurn,
    RigRequestMode,
};
use super::rig_tool::action_from_tool_call;
use super::ProviderRunProfile;
use crate::ai::agent::api::RequestParams;
use crate::ai::agent::AIAgentAction;
use crate::ai::provider::client::BedrockClient;
use crate::ai::provider::convert::CachingConfig;
use crate::ai::provider::external_config::ExternalBedrockConfig;
use crate::ai::provider::types::ConversationMessage;
use crate::ai::runtime::RuntimeResponseConfig;
use crate::settings::OpenAIProviderKind;

pub(crate) struct PreparedProviderRun {
    pub(crate) base_profile: ProviderRunProfile,
    pub(crate) cli_monitor_profile: Option<ProviderRunProfile>,
    pub(crate) tool_result_archive: Vec<ConversationMessage>,
    pub(crate) messages_sent: Arc<std::sync::Mutex<Vec<ConversationMessage>>>,
    pub(crate) persistence_offset: usize,
    pub(crate) response_config: RuntimeResponseConfig,
    pub(crate) action_context: ProviderActionContext,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProviderActionContext {
    task_id: String,
    skill_path_origin: ai::skills::SkillPathOrigin,
    mcp_tool_aliases: HashMap<String, MCPToolTarget>,
}

impl ProviderActionContext {
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) fn set_task_id(&mut self, task_id: impl Into<String>) {
        self.task_id = task_id.into();
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            skill_path_origin: ai::skills::SkillPathOrigin::Local,
            mcp_tool_aliases: HashMap::new(),
        }
    }

    pub(crate) fn tool_call_title(&self, call: &ToolCall) -> Option<String> {
        let parameter = match self.mcp_tool_aliases.get(&call.name) {
            Some(target) => target.title_parameter.as_deref()?,
            None => super::rig_request::TOOL_TITLE_PARAMETER,
        };
        call.arguments
            .get(parameter)
            .and_then(serde_json::Value::as_str)
            .map(|title| {
                title
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(120)
                    .collect::<String>()
            })
            .filter(|title| !title.is_empty())
    }

    pub(crate) fn action_from_tool_call(&self, call: &ToolCall) -> Result<AIAgentAction, String> {
        action_from_tool_call(
            &self.task_id,
            call,
            &self.skill_path_origin,
            &self.mcp_tool_aliases,
        )
    }
}

pub(crate) async fn prepare_provider_run(
    base_provider_config: crate::ai::provider::ProviderConfig,
    cli_provider_config: crate::ai::provider::ProviderConfig,
    mut params: RequestParams,
    orchestration_models: Vec<OrchestrationModelOption>,
) -> anyhow::Result<PreparedProviderRun> {
    let (supported_tools, supported_cli_agent_tools) =
        crate::ai::agent::api::prepare_direct_provider_params(&mut params);
    let skill_path_origin = params.session_context.skill_path_origin();
    let max_context_tokens = provider_context_window_tokens(
        &base_provider_config,
        params.model.as_str(),
        params.context_window_limit,
    );
    let mut cli_params = params.clone();
    let cli_model_is_placeholder = params.cli_agent_model.as_str().trim().is_empty()
        || params
            .cli_agent_model
            .as_str()
            .eq_ignore_ascii_case("placeholder");
    let cli_provider_config = match cli_provider_config {
        crate::ai::provider::ProviderConfig::None => {
            // The CLI model can be absent from a model-specific provider routing table even when
            // the base model is usable. Keep monitoring available through the base provider/model.
            cli_params.model = params.model.clone();
            base_provider_config.clone()
        }
        provider_config if cli_model_is_placeholder => {
            // A placeholder CLI model is used while preferences are still
            // loading. Never send it to a provider: fall back to the working
            // base model so command monitoring cannot terminate the run with
            // a provider-side invalid-model error.
            cli_params.model = params.model.clone();
            base_provider_config.clone()
        }
        provider_config => {
            cli_params.model = params.cli_agent_model.clone();
            provider_config
        }
    };

    let (base_runtime, mut prepared) = prepare_provider_profile(
        base_provider_config,
        params,
        supported_tools.clone(),
        supported_cli_agent_tools.clone(),
        None,
    )
    .await?;
    add_orchestration_model_options(&mut prepared.request.tools, &orchestration_models);
    let (cli_runtime, cli_prepared) = prepare_provider_profile(
        cli_provider_config,
        cli_params,
        supported_tools,
        supported_cli_agent_tools,
        Some(RigRequestMode::Cli),
    )
    .await?;
    let cli_monitor_profile = Some(ProviderRunProfile::new(cli_runtime, cli_prepared.request));

    let PreparedRigTurn {
        task_id,
        needs_create_task,
        user_query,
        request,
        persistent_messages,
        tool_result_archive,
        messages_sent,
        mcp_tool_aliases,
    } = prepared;
    let persistence_offset = request
        .messages
        .len()
        .saturating_sub(persistent_messages.len());
    let response_config = RuntimeResponseConfig {
        task_id: task_id.clone(),
        conversation_id: request
            .conversation_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        needs_create_task,
        user_query,
        model_id: request.model.as_str().to_string(),
        max_context_tokens,
        capabilities: base_runtime.descriptor().capabilities.clone(),
        empty_output_message: None,
    };
    Ok(PreparedProviderRun {
        base_profile: ProviderRunProfile::new(base_runtime, request),
        cli_monitor_profile,
        tool_result_archive,
        messages_sent,
        persistence_offset,
        response_config,
        action_context: ProviderActionContext {
            task_id,
            skill_path_origin,
            mcp_tool_aliases,
        },
    })
}

async fn prepare_provider_profile(
    provider_config: crate::ai::provider::ProviderConfig,
    params: RequestParams,
    supported_tools: Vec<ToolType>,
    supported_cli_agent_tools: Vec<ToolType>,
    mode: Option<RigRequestMode>,
) -> anyhow::Result<(Arc<dyn AgentRuntime>, PreparedRigTurn)> {
    let model = params.model.as_str().to_string();
    let prepared = match &provider_config {
        crate::ai::provider::ProviderConfig::OpenAI(config) => match mode {
            Some(mode) => prepare_rig_turn_for_mode(
                config,
                params,
                supported_tools,
                supported_cli_agent_tools,
                mode,
            ),
            None => prepare_rig_turn(config, params, supported_tools, supported_cli_agent_tools),
        },
        crate::ai::provider::ProviderConfig::Bedrock(_) => {
            let max_output_tokens = Some(bedrock_max_output_tokens(&model));
            prepare_bedrock_rig_turn_for_mode(
                model,
                max_output_tokens,
                params,
                supported_tools,
                supported_cli_agent_tools,
                mode,
            )
        }
        crate::ai::provider::ProviderConfig::None => {
            anyhow::bail!(
                "No AI runtime configured. Enable an agent runtime or model provider in settings."
            );
        }
    };
    let runtime = provider_runtime_for_request(provider_config, &prepared.request).await?;
    Ok((runtime, prepared))
}

/// Rebuilds a one-turn provider transport from current settings and a persisted request.
/// Credentials remain in the live provider config and never enter the run snapshot.
pub(crate) async fn provider_runtime_for_request(
    provider_config: crate::ai::provider::ProviderConfig,
    request: &TurnRequest,
) -> anyhow::Result<Arc<dyn AgentRuntime>> {
    let model = request.model.as_str().to_string();
    let runtime: Arc<dyn AgentRuntime> = match provider_config {
        crate::ai::provider::ProviderConfig::OpenAI(config) => match config.kind {
            OpenAIProviderKind::OpenAI | OpenAIProviderKind::LiteLLM => Arc::new(
                OpenAICompatibleRuntime::new(OpenAICompatibleRuntimeConfig {
                    base_url: config.base_url,
                    api_key: config.api_key,
                    model,
                    max_output_tokens: config.max_output_tokens.map(u64::from),
                    supports_system_messages: config.supports_system_messages,
                }),
            ),
            OpenAIProviderKind::ChatGPTSubscription => Arc::new(ChatGPTSubscriptionRuntime::new(
                ChatGPTSubscriptionRuntimeConfig {
                    model,
                    reasoning_effort: config.reasoning_effort,
                    max_output_tokens: config.max_output_tokens.map(u64::from),
                    auth_file: None,
                },
            )),
            OpenAIProviderKind::Anthropic => {
                Arc::new(AnthropicRuntime::new(AnthropicRuntimeConfig {
                    api_key: config.api_key.unwrap_or_default(),
                    model,
                    max_output_tokens: config.max_output_tokens.map(u64::from),
                }))
            }
            OpenAIProviderKind::Gemini => Arc::new(GeminiRuntime::new(GeminiRuntimeConfig {
                api_key: config.api_key.unwrap_or_default(),
                model,
                max_output_tokens: config.max_output_tokens.map(u64::from),
            })),
            OpenAIProviderKind::VertexAI => Arc::new(VertexAiRuntime::new(VertexAiRuntimeConfig {
                project_id: config.project_id.unwrap_or_default(),
                location: config.location.unwrap_or_else(|| "global".to_string()),
                model,
                max_output_tokens: config.max_output_tokens.map(u64::from),
            })),
        },
        crate::ai::provider::ProviderConfig::Bedrock(config) => {
            let max_output_tokens = Some(bedrock_max_output_tokens(&model));
            let cross_region_inference = config.cross_region_inference;
            let caching_config =
                CachingConfig::from_external_config(&ExternalBedrockConfig::load());
            let client = BedrockClient::from_config(config).await?;
            Arc::new(BedrockRuntime::from_aws_client(
                client.runtime_client(),
                BedrockRigConfig {
                    model,
                    region: client.region().to_string(),
                    cross_region_inference,
                    prompt_caching: caching_config.enabled,
                    max_output_tokens,
                },
            )?)
        }
        crate::ai::provider::ProviderConfig::None => {
            anyhow::bail!(
                "No AI runtime configured. Enable an agent runtime or model provider in settings."
            );
        }
    };
    Ok(runtime)
}

fn bedrock_max_output_tokens(model: &str) -> u64 {
    model_metadata(model)
        .and_then(|metadata| metadata.max_output_tokens)
        .map(u64::from)
        .unwrap_or(64_000)
}

pub(crate) fn provider_context_window_tokens(
    provider_config: &crate::ai::provider::ProviderConfig,
    model: &str,
    configured_limit: Option<u32>,
) -> Option<u32> {
    let provider_limit = match provider_config {
        crate::ai::provider::ProviderConfig::Bedrock(_) => {
            model_metadata(model).and_then(|metadata| metadata.context_window_tokens)
        }
        crate::ai::provider::ProviderConfig::OpenAI(config) => config.max_input_tokens,
        crate::ai::provider::ProviderConfig::None => None,
    };
    match (configured_limit, provider_limit) {
        (Some(configured), Some(provider)) => Some(configured.min(provider)),
        (configured, provider) => configured.or(provider),
    }
}

#[cfg(test)]
#[path = "rig_tests.rs"]
mod tests;
