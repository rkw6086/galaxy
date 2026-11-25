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
use sha2::{Digest, Sha256};
use uuid::Uuid;
use warp_multi_agent_api::ToolType;

use super::rig_request::{
    prepare_bedrock_rig_turn_for_mode, prepare_rig_turn, prepare_rig_turn_for_mode, MCPToolTarget,
    OrchestrationModelOption, PreparedRigTurn, RigRequestMode,
};
use super::rig_tool::action_from_tool_call;
use super::skill_creation::SkillCreationContext;
use super::ProviderRunProfile;
use crate::ai::agent::api::RequestParams;
use crate::ai::agent::{AIAgentAction, AIAgentActionType, FileEdit};
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::provider::client::BedrockClient;
use crate::ai::provider::convert::CachingConfig;
use crate::ai::provider::external_config::ExternalBedrockConfig;
use crate::ai::provider::types::ConversationMessage;
use crate::ai::runtime::RuntimeResponseConfig;
use crate::settings::OpenAIProviderKind;

const PROVIDER_SELECTION_METADATA: &str = "galaxy_provider_selection";

/// Keep routing identity separate from the wire model name. This metadata is
/// checkpointed with each request, but is never included in provider payloads.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedProviderSelection {
    model_id: String,
    connection_fingerprint: String,
    reasoning: PersistedReasoningSelection,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedReasoningSelection {
    ProviderDefault,
    Explicit(String),
}

fn provider_connection_fingerprint(config: &OpenAIClientConfig) -> String {
    // Credentials are resolved afresh on restore. Hash only connection metadata
    // so endpoint URLs do not appear in checkpoints either.
    let identity = serde_json::json!([
        config.connection_identity,
        format!("{:?}", config.kind),
        config.base_url,
        config.project_id,
        config.location,
    ]);
    format!("{:x}", Sha256::digest(identity.to_string().as_bytes()))
}

fn remember_provider_selection(
    request: &mut TurnRequest,
    model_id: String,
    provider_config: &crate::ai::provider::ProviderConfig,
) -> anyhow::Result<()> {
    if let crate::ai::provider::ProviderConfig::OpenAI(config) = provider_config {
        request.metadata.insert(
            PROVIDER_SELECTION_METADATA.to_string(),
            serde_json::to_value(PersistedProviderSelection {
                model_id,
                connection_fingerprint: provider_connection_fingerprint(config),
                reasoning: config.reasoning_effort.clone().map_or(
                    PersistedReasoningSelection::ProviderDefault,
                    PersistedReasoningSelection::Explicit,
                ),
            })?,
        );
    }
    Ok(())
}

pub(crate) fn restore_provider_config(
    request: &TurnRequest,
    resolve: impl FnOnce(&str) -> crate::ai::provider::ProviderConfig,
) -> anyhow::Result<crate::ai::provider::ProviderConfig> {
    let Some(selection) = request.metadata.get(PROVIDER_SELECTION_METADATA) else {
        // Older checkpoints only contain the wire model ID.
        return Ok(resolve(request.model.as_str()));
    };
    let selection: PersistedProviderSelection = serde_json::from_value(selection.clone())
        .map_err(|_| anyhow::anyhow!("invalid persisted provider selection"))?;
    anyhow::ensure!(
        !selection.model_id.trim().is_empty(),
        "empty persisted provider selection"
    );
    let crate::ai::provider::ProviderConfig::OpenAI(mut config) = resolve(&selection.model_id)
    else {
        anyhow::bail!("the saved provider model is no longer available");
    };
    anyhow::ensure!(
        provider_connection_fingerprint(&config) == selection.connection_fingerprint,
        "the saved provider connection has changed"
    );
    let wire_model = config
        .model
        .as_deref()
        .filter(|model| !model.is_empty() && *model != "auto")
        .unwrap_or(&selection.model_id);
    anyhow::ensure!(
        wire_model == request.model.as_str(),
        "the saved provider model no longer matches its request"
    );
    config.reasoning_effort = match selection.reasoning {
        PersistedReasoningSelection::ProviderDefault => None,
        PersistedReasoningSelection::Explicit(effort) => Some(effort),
    };
    Ok(crate::ai::provider::ProviderConfig::OpenAI(config))
}

pub(crate) struct PreparedProviderRun {
    pub(crate) base_profile: ProviderRunProfile,
    pub(crate) cli_monitor_profile: Option<ProviderRunProfile>,
    pub(crate) computer_use_profile: Option<ProviderRunProfile>,
    pub(crate) tool_result_archive: Vec<ConversationMessage>,
    pub(crate) messages_sent: Arc<std::sync::Mutex<Vec<ConversationMessage>>>,
    pub(crate) persistence_offset: usize,
    pub(crate) response_config: RuntimeResponseConfig,
    pub(crate) action_context: ProviderActionContext,
    /// Per-profile startup timeout carried from the execution-profile setting.
    /// `None` means use the coordinator's compile-time default (120 s).
    pub(crate) model_start_timeout_secs: Option<u32>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProviderActionContext {
    task_id: String,
    skill_path_origin: ai::skills::SkillPathOrigin,
    mcp_tool_aliases: HashMap<String, MCPToolTarget>,
    #[serde(default)]
    skill_creation: SkillCreationContext,
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
            skill_creation: SkillCreationContext::default(),
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

    pub(crate) fn history_compatibility_call(&self, call: &ToolCall) -> ToolCall {
        if call.name == "create_skill" {
            if let Ok((compatible, _)) =
                self.skill_creation
                    .lower_call(call, &self.skill_path_origin, false)
            {
                return compatible;
            }
        }
        let mut compatible = call.clone();
        if let Some(target) = self.mcp_tool_aliases.get(&call.name) {
            let server = target
                .server_id
                .map(|id| id.to_string())
                .unwrap_or_default();
            compatible.name = format!("mcp__{server}__{}", target.name);
            if let (Some(arguments), Some(parameter)) = (
                compatible.arguments.as_object_mut(),
                target.title_parameter.as_ref(),
            ) {
                arguments.remove(parameter);
            }
        }
        compatible
    }

    pub(crate) fn restored_action_from_tool_call(
        &self,
        call: &ToolCall,
    ) -> Result<AIAgentAction, String> {
        let mut context = self.clone();
        context.skill_path_origin = ai::skills::SkillPathOrigin::RestoredDisplayOnly;
        context.action_from_tool_call(call)
    }

    pub(crate) fn action_from_tool_call(&self, call: &ToolCall) -> Result<AIAgentAction, String> {
        if call.name == "create_skill" {
            let (lowered, scope) = self.skill_creation.lower_call(
                call,
                &self.skill_path_origin,
                !matches!(
                    self.skill_path_origin,
                    ai::skills::SkillPathOrigin::RestoredDisplayOnly
                ),
            )?;
            let mut action = action_from_tool_call(
                &self.task_id,
                &lowered,
                &self.skill_path_origin,
                &self.mcp_tool_aliases,
            )?;
            if let AIAgentActionType::RequestFileEdits { file_edits, .. } = &mut action.action {
                for edit in file_edits {
                    if let FileEdit::Create { creation_scope, .. } = edit {
                        *creation_scope = Some(scope.clone());
                    }
                }
            }
            return Ok(action);
        }
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
    computer_use_provider_config: crate::ai::provider::ProviderConfig,
    mut params: RequestParams,
    orchestration_models: Vec<OrchestrationModelOption>,
) -> anyhow::Result<PreparedProviderRun> {
    let (supported_tools, supported_cli_agent_tools) =
        crate::ai::agent::api::prepare_direct_provider_params(&mut params);
    let skill_path_origin = params.session_context.skill_path_origin();
    let skill_creation = SkillCreationContext::from_session(&params.session_context);
    let model_start_timeout_secs = params.model_start_timeout_secs;
    let mut cli_params = params.clone();
    let computer_use_model = params.computer_use_model.as_str();
    let computer_use_profile = if supported_tools.contains(&ToolType::RequestComputerUse) {
        let mut computer_params = params.clone();
        let computer_provider = if computer_use_model.is_empty()
            || computer_use_model == "computer-use-agent-auto"
            || computer_use_model == params.model.as_str()
            || matches!(
                &computer_use_provider_config,
                crate::ai::provider::ProviderConfig::None
            ) {
            base_provider_config.clone()
        } else {
            computer_params.model = params.computer_use_model.clone();
            computer_use_provider_config
        };
        let mut computer_tools = supported_tools.clone();
        computer_tools.retain(|tool| *tool != ToolType::RequestComputerUse);
        computer_tools.push(ToolType::UseComputer);
        let (runtime, prepared) = prepare_provider_profile(
            computer_provider,
            computer_params,
            computer_tools,
            supported_cli_agent_tools.clone(),
            None,
        )
        .await?;
        let mut profile = ProviderRunProfile::new(runtime, prepared.request)
            .with_max_context_tokens(prepared.max_context_tokens);
        profile.refresh_orchestration_models(&orchestration_models);
        Some(profile)
    } else {
        None
    };
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

    let (base_runtime, prepared) = prepare_provider_profile(
        base_provider_config,
        params,
        supported_tools.clone(),
        supported_cli_agent_tools.clone(),
        None,
    )
    .await?;
    let (cli_runtime, cli_prepared) = prepare_provider_profile(
        cli_provider_config,
        cli_params,
        supported_tools,
        supported_cli_agent_tools,
        Some(RigRequestMode::Cli),
    )
    .await?;
    let cli_monitor_profile = Some(
        ProviderRunProfile::new(cli_runtime, cli_prepared.request)
            .with_max_context_tokens(cli_prepared.max_context_tokens),
    );

    let PreparedRigTurn {
        max_context_tokens: prepared_context_tokens,
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
        max_context_tokens: prepared_context_tokens,
        capabilities: base_runtime.descriptor().capabilities.clone(),
        empty_output_message: None,
    };
    let mut base_profile = ProviderRunProfile::new(base_runtime, request)
        .with_max_context_tokens(prepared_context_tokens);
    base_profile.refresh_orchestration_models(&orchestration_models);
    Ok(PreparedProviderRun {
        base_profile,
        cli_monitor_profile,
        computer_use_profile,
        tool_result_archive,
        messages_sent,
        persistence_offset,
        response_config,
        action_context: ProviderActionContext {
            task_id,
            skill_path_origin,
            mcp_tool_aliases,
            skill_creation,
        },
        model_start_timeout_secs,
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
    let mut prepared = match &provider_config {
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
                model.clone(),
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
    remember_provider_selection(&mut prepared.request, model, &provider_config)?;
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
    // Keep transport fallbacks bound to the saved request. A later catalog change
    // must not add an output reservation that the restored run cannot account for.
    // The coordinator's projected request can select a smaller explicit budget.
    let max_output_tokens = request.max_output_tokens;
    let runtime: Arc<dyn AgentRuntime> = match provider_config {
        crate::ai::provider::ProviderConfig::OpenAI(config) => match config.kind {
            OpenAIProviderKind::OpenAI | OpenAIProviderKind::LiteLLM => Arc::new(
                OpenAICompatibleRuntime::new(OpenAICompatibleRuntimeConfig {
                    base_url: config.base_url,
                    api_key: config.api_key,
                    model,
                    max_output_tokens,
                    supports_system_messages: config.supports_system_messages,
                })
                .with_reasoning_effort(config.reasoning_effort),
            ),
            OpenAIProviderKind::ChatGPTSubscription => Arc::new(
                ChatGPTSubscriptionRuntime::new(ChatGPTSubscriptionRuntimeConfig {
                    model,
                    reasoning_effort: config.reasoning_effort,
                    max_output_tokens,
                })
                .with_credential_resolver(crate::ai::chatgpt_auth::credential_resolver()),
            ),
            OpenAIProviderKind::Anthropic => {
                let credential = config.api_key.unwrap_or_default();
                let stored = crate::ai::claude_subscription::is_stored_credential(&credential);
                let mut runtime = AnthropicRuntime::new(AnthropicRuntimeConfig {
                    api_key: credential.clone(),
                    model,
                    max_output_tokens,
                })
                .with_reasoning_effort(config.reasoning_effort);
                if stored {
                    runtime = runtime.with_credential_resolver(Arc::new(move || {
                        Box::pin(crate::ai::claude_oauth::load_credential(credential.clone()))
                    }));
                }
                Arc::new(runtime)
            }
            OpenAIProviderKind::Gemini => Arc::new(
                GeminiRuntime::new(GeminiRuntimeConfig {
                    api_key: config.api_key.unwrap_or_default(),
                    model,
                    max_output_tokens,
                })
                .with_reasoning_effort(config.reasoning_effort),
            ),
            OpenAIProviderKind::VertexAI => Arc::new(
                VertexAiRuntime::new(VertexAiRuntimeConfig {
                    project_id: config.project_id.unwrap_or_default(),
                    location: config.location.unwrap_or_else(|| "global".to_string()),
                    model,
                    max_output_tokens,
                })
                .with_reasoning_effort(config.reasoning_effort),
            ),
        },
        crate::ai::provider::ProviderConfig::Bedrock(config) => {
            let cross_region_inference =
                config.invocation_model_id.is_none() && config.cross_region_inference;
            let caching_config =
                CachingConfig::from_external_config(&ExternalBedrockConfig::load())
                    .for_model(&model);
            let invocation_model = config
                .invocation_model_id
                .clone()
                .unwrap_or_else(|| model.clone());
            let client = BedrockClient::from_config(config).await?;
            Arc::new(
                BedrockRuntime::from_aws_client(
                    client.runtime_client(),
                    BedrockRigConfig {
                        model: invocation_model,
                        region: client.region().to_string(),
                        cross_region_inference,
                        prompt_caching: caching_config.enabled,
                        max_output_tokens,
                    },
                )?
                .with_reasoning_model(model),
            )
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
