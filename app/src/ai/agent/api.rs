pub(crate) mod convert_conversation;
mod convert_from;
mod convert_to;
mod r#impl;

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

pub use ai::agent::convert::ConvertToAPITypeError;
use ai::api_keys::ApiKeyManager;
pub use convert_from::{
    user_inputs_from_messages, ConversionParams, ConvertAPIMessageToClientOutputMessage,
    MaybeAIAgentOutputMessage, MessageToAIAgentOutputMessageError,
};
use futures_lite::Stream;
use galaxy_agent_core::ToolResult;
use galaxy_core::channel::ChannelState;
use galaxy_core::execution_mode::AppExecutionMode;
use galaxy_core::features::FeatureFlag;
use galaxy_core::user_preferences::GetUserPreferences;
use galaxyui::{AppContext, EntityId, SingletonEntity as _};
use mcp::TemplatableMCPServerInfo;
pub(crate) use r#impl::prepare_direct_provider_params;
use serde::Serialize;

use super::{AIAgentInput, MCPContext, MCPServer, RequestMetadata, Suggestions};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::blocklist::{BlocklistAIPermissions, RequestInput, SessionContext};
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::execution_profiles::AIExecutionProfileAppExt;
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::server::server_api::AIApiError;
use crate::settings::AISettings;
use crate::terminal::safe_mode_settings::get_secret_obfuscation_mode;
use crate::workspaces::user_workspaces::UserWorkspaces;

const INTERNAL_COMMAND_COMPLETION_ASSESSMENT_MESSAGE_DATA: &str =
    "galaxy:internal-command-completion-assessment:v1";

pub(crate) fn mark_internal_command_completion_assessment(
    message: &mut warp_multi_agent_api::Message,
) {
    message.server_message_data = INTERNAL_COMMAND_COMPLETION_ASSESSMENT_MESSAGE_DATA.to_string();
}

pub(crate) fn is_internal_command_completion_assessment(
    message: &warp_multi_agent_api::Message,
) -> bool {
    message.server_message_data == INTERNAL_COMMAND_COMPLETION_ASSESSMENT_MESSAGE_DATA
}

/// Unique, server-generated conversation-scoped token to be roundtripped to the API when sending
/// requests that follow-up within a given conversation.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerConversationToken(String);

impl ServerConversationToken {
    pub fn new(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn debug_link(&self) -> String {
        format!(
            "{}/debug/maa/{}",
            ChannelState::server_root_url(),
            self.as_str()
        )
    }

    pub fn conversation_link(&self) -> String {
        format!(
            "{}/conversation/{}",
            ChannelState::server_root_url(),
            self.as_str()
        )
    }
}

impl From<ServerConversationToken> for String {
    fn from(value: ServerConversationToken) -> Self {
        value.0
    }
}

// Conversions between AI ServerConversationToken and protocol ServerConversationToken
impl From<session_sharing_protocol::common::ServerConversationToken> for ServerConversationToken {
    fn from(token: session_sharing_protocol::common::ServerConversationToken) -> Self {
        Self(token.to_string())
    }
}

impl TryFrom<ServerConversationToken>
    for session_sharing_protocol::common::ServerConversationToken
{
    type Error = uuid::Error;

    fn try_from(token: ServerConversationToken) -> Result<Self, Self::Error> {
        token.as_str().parse()
    }
}

#[derive(Debug, Clone)]
pub struct RequestParams {
    /// Galaxy terminal view that originated the request. This is retained
    /// locally so ACP-provided Galaxy tools can be pinned to the exact pane.
    pub terminal_view_id: Option<EntityId>,
    pub input: Vec<AIAgentInput>,
    /// Normalized action results appended to direct-provider run history.
    pub tool_results: Vec<ToolResult>,
    pub conversation_token: Option<ServerConversationToken>,
    pub forked_from_conversation_token: Option<ServerConversationToken>,
    pub ambient_agent_task_id: Option<AmbientAgentTaskId>,
    pub tasks: Vec<warp_multi_agent_api::Task>,
    pub existing_suggestions: Option<Suggestions>,
    pub metadata: Option<RequestMetadata>,
    pub session_context: SessionContext,
    pub model: LLMId,
    #[allow(unused)]
    pub coding_model: LLMId,
    pub cli_agent_model: LLMId,
    pub computer_use_model: LLMId,
    pub is_memory_enabled: bool,
    pub warp_drive_context_enabled: bool,
    pub context_window_limit: Option<u32>,
    pub mcp_context: Option<MCPContext>,
    pub planning_enabled: bool,
    should_redact_secrets: bool,

    /// User-provided API keys for AI providers, currently AWS Bedrock credentials
    /// and/or Gemini Enterprise (GEAP) credentials.
    pub api_keys: Option<warp_multi_agent_api::request::settings::ApiKeys>,
    /// User-provided custom model providers (BYOK endpoints).
    pub custom_model_providers:
        Option<warp_multi_agent_api::request::settings::CustomModelProviders>,
    /// User-defined custom model routers referenced by the current selection. Mirrors
    /// `custom_model_providers`: the selected model's `config_key` indexes into this
    /// registry. `None` when no custom router is selected.
    pub custom_model_routers: Option<warp_multi_agent_api::request::settings::CustomModelRouters>,
    pub permission_mode: crate::ai::execution_profiles::PermissionMode,
    pub command_allowlist: Vec<String>,
    pub autonomy_level: warp_multi_agent_api::AutonomyLevel,
    pub isolation_level: warp_multi_agent_api::IsolationLevel,
    pub web_search_enabled: bool,
    pub computer_use_enabled: bool,
    pub ask_user_question_enabled: bool,
    pub research_agent_enabled: bool,
    pub orchestration_enabled: bool,
    pub supported_tools_override: Option<Vec<warp_multi_agent_api::ToolType>>,
    /// The root task ID used to anchor direct-provider projection when optimistic tasks are not
    /// present in the proto task context.
    pub root_task_id: Option<String>,
    /// The conversation ID of the parent agent that spawned this child agent, if any.
    pub parent_agent_id: Option<String>,
    /// The display name for this agent (e.g. "Agent 1"), assigned by the orchestrator.
    pub agent_name: Option<String>,
    /// Provider-neutral conversation history for direct model calls.
    pub message_history: Vec<crate::ai::provider::types::ConversationMessage>,
    /// Progressive summary of older conversation history. Prepended as the first
    /// message pair in the messages array sent to the model.
    pub progressive_summary: Option<String>,
    /// Archived tool_use/tool_result pairs from previous summarization drains.
    /// Kept separately so `recall_tool_history` can search archived results even after
    /// they've been summarized away from live history.
    pub tool_result_archive: Vec<crate::ai::provider::types::ConversationMessage>,
    /// Populated while preparing a direct-provider run with the durable transcript that the
    /// controller persists for restoration and future turns.
    pub messages_sent:
        std::sync::Arc<std::sync::Mutex<Vec<crate::ai::provider::types::ConversationMessage>>>,
    /// Global rules (name, content) from the local CloudModel (AIFact/AIMemory).
    /// Injected into the system prompt when `is_memory_enabled` is true.
    pub global_rules: Vec<(String, String)>,
}

/// Response event projected into the local conversation controller.
#[derive(Debug)]
pub enum StreamEvent {
    Response(warp_multi_agent_api::ResponseEvent),
}

pub type Event = Result<StreamEvent, Arc<AIApiError>>;
pub type LegacyEvent = Result<warp_multi_agent_api::ResponseEvent, Arc<AIApiError>>;

#[cfg(not(target_family = "wasm"))]
pub type ResponseStream = Pin<Box<dyn Stream<Item = Event> + Send + 'static>>;

#[cfg(not(target_family = "wasm"))]
pub type LegacyResponseStream = Pin<Box<dyn Stream<Item = LegacyEvent> + Send + 'static>>;

// The WASM version of this type has no bound on `Send`, which is an unnecessary bound when
// targeting wasm because the browser is single-threaded (and we don't leverage WebWorkers for async
// execution in WoW).
#[cfg(target_family = "wasm")]
pub type ResponseStream = Pin<Box<dyn Stream<Item = Event>>>;

#[cfg(target_family = "wasm")]
pub type LegacyResponseStream = Pin<Box<dyn Stream<Item = LegacyEvent>>>;

#[derive(Debug, Clone)]
pub struct ConversationData {
    pub id: AIConversationId,
    pub tasks: Vec<warp_multi_agent_api::Task>,
    pub server_conversation_token: Option<ServerConversationToken>,
    pub forked_from_conversation_token: Option<ServerConversationToken>,
    pub ambient_agent_task_id: Option<AmbientAgentTaskId>,
    pub existing_suggestions: Option<Suggestions>,
}

impl RequestParams {
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self {
            terminal_view_id: None,
            input: vec![],
            tool_results: vec![],
            conversation_token: None,
            forked_from_conversation_token: None,
            ambient_agent_task_id: None,
            tasks: vec![],
            existing_suggestions: None,
            metadata: None,
            session_context: SessionContext::new_for_test(),
            model: LLMId::from("test-model"),
            coding_model: LLMId::from("test-model"),
            cli_agent_model: LLMId::from("test-model"),
            computer_use_model: LLMId::from("test-model"),
            is_memory_enabled: false,
            warp_drive_context_enabled: false,
            context_window_limit: None,
            mcp_context: None,
            planning_enabled: false,
            should_redact_secrets: false,
            api_keys: None,
            custom_model_providers: None,
            custom_model_routers: None,
            permission_mode: Default::default(),
            command_allowlist: Vec::new(),
            autonomy_level: Default::default(),
            isolation_level: Default::default(),
            web_search_enabled: false,
            computer_use_enabled: false,
            ask_user_question_enabled: false,
            research_agent_enabled: false,
            orchestration_enabled: false,
            supported_tools_override: None,
            parent_agent_id: None,
            agent_name: None,
            root_task_id: None,
            message_history: vec![],
            progressive_summary: None,
            tool_result_archive: vec![],
            messages_sent: std::sync::Arc::new(std::sync::Mutex::new(vec![])),
            global_rules: vec![],
        }
    }

    /// Applies the request's configured secret-redaction policy to text immediately
    /// before it is handed to an external model or agent.
    pub(crate) fn redact_text_for_model(&self, text: &mut String) {
        if self.should_redact_secrets {
            super::redaction::redact_secrets(text);
        }
    }

    #[cfg(test)]
    pub(crate) fn enable_secret_redaction_for_test(&mut self) {
        self.should_redact_secrets = true;
    }

    pub fn new(
        terminal_view_id: Option<EntityId>,
        session_context: SessionContext,
        request_input: &RequestInput,
        conversation: ConversationData,
        metadata: Option<RequestMetadata>,
        app: &AppContext,
    ) -> Self {
        let ai_settings = AISettings::as_ref(app);
        let is_memory_enabled = ai_settings.is_memory_enabled(app);
        let warp_drive_context_enabled = ai_settings.is_warp_drive_context_enabled(app);

        // Build MCP context - either grouped by server or flat lists based on feature flag
        let mcp_context = if FeatureFlag::MCPGroupedServerContext.is_enabled() {
            // Group MCP tools and resources by server
            let templatable_manager = TemplatableMCPServerManager::as_ref(app);

            let mut active_servers: Vec<&TemplatableMCPServerInfo> = templatable_manager
                .get_active_templatable_servers()
                .values()
                .copied()
                .collect();

            // If file-based MCP servers are enabled, add active servers in scope of
            // the user's current working directory
            if let Some(cwd) = session_context.current_working_directory() {
                active_servers.extend(
                    templatable_manager
                        .get_active_file_based_servers(Path::new(cwd), app)
                        .values(),
                );
            }

            // Include any ephemeral MCP servers started via the Oz CLI.
            active_servers.extend(
                templatable_manager
                    .get_active_cli_spawned_servers()
                    .values(),
            );

            let servers: Vec<MCPServer> = active_servers
                .into_iter()
                .map(|server| MCPServer {
                    name: server.name().to_string(),
                    description: server.description().unwrap_or_default().to_string(),
                    id: server.installation_id().to_string(),
                    resources: server.resources().to_vec(),
                    tools: server.tools().to_vec(),
                })
                .collect();

            if servers.is_empty() {
                None
            } else {
                #[allow(deprecated)]
                Some(MCPContext {
                    resources: vec![],
                    tools: vec![],
                    servers,
                })
            }
        } else {
            // Flat lists of resources and tools
            let templatable_mcp_manager = TemplatableMCPServerManager::as_ref(app);
            let resources = templatable_mcp_manager
                .resources()
                .cloned()
                .collect::<Vec<_>>();
            let tools = templatable_mcp_manager.tools().cloned().collect::<Vec<_>>();

            #[allow(deprecated)]
            (!resources.is_empty() || !tools.is_empty()).then_some(MCPContext {
                resources,
                tools,
                servers: vec![],
            })
        };

        let should_redact_secrets = get_secret_obfuscation_mode(app).should_redact_secret();

        let user_workspaces = UserWorkspaces::as_ref(app);
        let api_key_manager = ApiKeyManager::as_ref(app);
        #[cfg(not(target_family = "wasm"))]
        let geap_binding = crate::ai::geap_credentials::current_geap_policy(app).mint_binding();
        #[cfg(target_family = "wasm")]
        let geap_binding: Option<::ai::api_keys::GeapMintBinding> = None;
        let api_keys = api_key_manager
            .api_keys_for_request(user_workspaces.is_bedrock_enabled(app), geap_binding);
        let custom_model_providers = None;
        let custom_model_routers = FeatureFlag::CustomModelRouters.is_enabled().then(|| {
            LLMPreferences::as_ref(app).custom_model_routers_for_request(
                &request_input.model_id,
                &request_input.coding_model_id,
            )
        });

        let app_execution_mode = AppExecutionMode::as_ref(app);
        let autonomy_level = if app_execution_mode.is_autonomous() {
            warp_multi_agent_api::AutonomyLevel::Unsupervised
        } else {
            warp_multi_agent_api::AutonomyLevel::Supervised
        };

        let isolation_level = if app_execution_mode.is_sandboxed() {
            warp_multi_agent_api::IsolationLevel::Sandbox
        } else {
            warp_multi_agent_api::IsolationLevel::None
        };

        let web_search_enabled =
            BlocklistAIPermissions::as_ref(app).get_web_search_enabled(app, terminal_view_id);
        let research_agent_enabled = app
            .private_user_preferences()
            .read_value("ResearchAgentEnabled")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let is_ambient_agent = conversation.ambient_agent_task_id.is_some();
        let computer_use_enabled = FeatureFlag::AgentModeComputerUse.is_enabled()
            && BlocklistAIPermissions::as_ref(app)
                .get_computer_use_setting(app, terminal_view_id)
                .is_enabled()
            && computer_use::is_supported_on_current_platform()
            && (FeatureFlag::LocalComputerUse.is_enabled() || is_ambient_agent);
        let ask_user_question_enabled = BlocklistAIPermissions::as_ref(app)
            .get_ask_user_question_setting(app, terminal_view_id)
            != crate::ai::execution_profiles::AskUserQuestionPermission::Never;

        let orchestration_enabled = ai_settings.is_orchestration_enabled(app)
            && BlocklistAIPermissions::as_ref(app)
                .get_run_agents_setting(app, terminal_view_id)
                .is_enabled()
            && session_context
                .session_type()
                .as_ref()
                .is_none_or(|t| matches!(t, crate::terminal::model::session::SessionType::Local));

        // Reconcile the persisted override against the active base model's
        // current `LLMContextWindow` instead of trusting whatever was stored
        // last. If the active model isn't configurable or has been removed
        // server-side, drop the override; otherwise clamp it to the model's
        // current `[min, max]` range. This closes the window between an
        // in-flight model metadata refresh and the next request.
        let llm_preferences = LLMPreferences::as_ref(app);
        let selected_model = llm_preferences
            .get_llm_info(&request_input.model_id)
            .unwrap_or_else(|| llm_preferences.get_active_base_model(app, terminal_view_id));
        let context_window_limit = AIExecutionProfilesModel::as_ref(app)
            .active_profile(terminal_view_id, app)
            .data()
            .context_window_limit_for_model_request(selected_model, app);

        Self {
            terminal_view_id,
            input: request_input.all_inputs().cloned().collect(),
            tool_results: Vec::new(),
            conversation_token: conversation.server_conversation_token,
            forked_from_conversation_token: conversation.forked_from_conversation_token,
            ambient_agent_task_id: conversation.ambient_agent_task_id,
            tasks: conversation.tasks,
            existing_suggestions: conversation.existing_suggestions,
            context_window_limit,
            metadata,
            session_context,
            model: request_input.model_id.clone(),
            coding_model: request_input.coding_model_id.clone(),
            cli_agent_model: request_input.cli_agent_model_id.clone(),
            computer_use_model: request_input.computer_use_model_id.clone(),
            is_memory_enabled,
            warp_drive_context_enabled,
            mcp_context,
            planning_enabled: true,
            should_redact_secrets,
            api_keys,
            custom_model_providers,
            custom_model_routers,
            command_allowlist:
                crate::ai::execution_profiles::profiles::AIExecutionProfilesModel::as_ref(app)
                    .active_profile(terminal_view_id, app)
                    .data()
                    .command_allowlist
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            permission_mode: BlocklistAIPermissions::as_ref(app)
                .get_permission_mode(app, terminal_view_id),
            autonomy_level,
            isolation_level,
            web_search_enabled,
            computer_use_enabled,
            ask_user_question_enabled,
            research_agent_enabled,
            orchestration_enabled,
            supported_tools_override: request_input.supported_tools_override.clone(),
            root_task_id: request_input
                .input_messages
                .keys()
                .next()
                .map(|id| id.to_string()),
            parent_agent_id: None,
            agent_name: None,
            message_history: Vec::new(),
            progressive_summary: None,
            tool_result_archive: Vec::new(),
            messages_sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            global_rules: if is_memory_enabled {
                Self::load_global_rules(app)
            } else {
                vec![]
            },
        }
    }

    /// Load global rules (AIFact/AIMemory) from the local CloudModel.
    fn load_global_rules(app: &AppContext) -> Vec<(String, String)> {
        use crate::ai::facts::{AIFact, AIMemory, CloudAIFactModel};
        use crate::cloud_object::model::generic_string_model::GenericStringObjectId;
        use crate::cloud_object::model::persistence::CloudModel;

        CloudModel::as_ref(app)
            .get_all_objects_of_type::<GenericStringObjectId, CloudAIFactModel>()
            .filter_map(|fact| {
                let AIFact::Memory(AIMemory { name, content, .. }) =
                    fact.model().string_model.clone();
                if content.is_empty() {
                    None
                } else {
                    Some((name.unwrap_or_default(), content))
                }
            })
            .collect()
    }
}
