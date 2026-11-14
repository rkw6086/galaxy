#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ai::api_keys::ApiKeyManager;
pub use ai::LLMId;
#[cfg(not(target_family = "wasm"))]
use galaxy_agent_rig::{
    discover_anthropic_models, discover_gemini_models, validate_vertex_ai_credentials,
    vertex_ai_model_catalog, RigModelInfo,
};
#[cfg(not(target_family = "wasm"))]
use galaxy_bedrock_model_catalog::model_metadata;
use galaxy_core::features::FeatureFlag;
use galaxy_core::ui::icons::Icon;
use galaxy_core::user_preferences::GetUserPreferences;
use galaxyui::r#async::Timer;
use galaxyui::windowing::WindowManager;
use galaxyui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity};
use parking_lot::FairMutex;
use serde::{de, Deserialize, Serialize};
use settings::Setting;
use warp_multi_agent_api as api;

use super::custom_model_routers::{self, CustomModelRouter, ModelConfigError};
use super::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::acp::{acp_launch_fingerprint, acp_provider_selection_identity};
#[cfg(not(target_family = "wasm"))]
use crate::ai::provider::external_config::ExternalBedrockConfig;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::AuthStateProvider;
use crate::network::{NetworkStatus, NetworkStatusEvent, NetworkStatusKind};
#[cfg(not(target_family = "wasm"))]
use crate::persistence::model::{AcpConversationData, AgentBackend};
use crate::server::server_api::ServerApiProvider;
use crate::settings::{
    AcpConfigValueSettings, AcpProviderConfig, BedrockModelConfig, OpenAIModelConfig,
    OpenAIProviderConfig, OpenAIProviderKind,
};
use crate::user_config::{WarpConfig, WarpConfigUpdateEvent};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;
use crate::workspaces::user_workspaces::{UserWorkspaces, UserWorkspacesEvent};
use crate::{report_error, AISettings};

/// Checks if a user's' API key is being used for the given provider.
/// AWS Bedrock is the only supported provider in Galaxy; there is no
/// user-pasted BYO key path for other providers.
pub fn is_using_api_key_for_provider(_provider: &LLMProvider, _app: &AppContext) -> bool {
    false
}

pub fn should_show_bedrock_icon_for_model(llm: &LLMInfo, _app: &AppContext) -> bool {
    llm.host_configs
        .get(&LLMModelHost::AwsBedrock)
        .is_some_and(|config| config.enabled)
}

/// Key for cached LLM metadata in user preferences.
///
/// Note: this key used to store a single [`AvailableLLMs`]
/// but was migrated to store a full [`ModelsByFeature`].
pub const MODELS_BY_FEATURE_CACHE_KEY: &str = "AvailableLLMs";
const CUSTOM_ENDPOINT_USAGE_FALLBACK_LABEL: &str = "Custom endpoint";
const CHATGPT_CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
const CODEX_LATEST_RELEASE_URL: &str = "https://api.github.com/repos/openai/codex/releases/latest";
const CHATGPT_SUBSCRIPTION_MODELS_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24);
const DEFAULT_DISCOVERED_MODEL_CONTEXT_SIZE: u32 = 200_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LLMUsageMetadata {
    pub request_multiplier: usize,
    pub credit_multiplier: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisableReason {
    AdminDisabled,
    OutOfRequests,
    ProviderOutage,
    RequiresUpgrade,
    Unavailable,
}

impl DisableReason {
    /// Returns a user-facing tooltip explaining why the model is disabled.
    pub fn tooltip_text(&self) -> &'static str {
        match self {
            DisableReason::AdminDisabled => "This model has been disabled by your team admin.",
            DisableReason::OutOfRequests => "Please upgrade your plan to make more requests.",
            DisableReason::ProviderOutage => {
                "This model is temporarily unavailable due to a provider outage."
            }
            DisableReason::RequiresUpgrade => "Please upgrade your plan to access this model.",
            DisableReason::Unavailable => "This model is unavailable.",
        }
    }

    /// Returns `true` when this disable reason means the user cannot use the model
    /// and we should clear their stored preference.
    ///
    /// `RequiresUpgrade` is BYOK-aware: if the user has a BYO API key for the
    /// model's provider (`has_byok_key = true`), the server will still accept
    /// the request, so we keep the selection.
    ///
    /// `OutOfRequests` and `ProviderOutage` are transient and expected to
    /// resolve without user action, so we preserve the selection.
    fn should_clear_preference(&self, has_byok_key: bool) -> bool {
        match self {
            DisableReason::AdminDisabled | DisableReason::Unavailable => true,
            DisableReason::RequiresUpgrade => !has_byok_key,
            DisableReason::OutOfRequests | DisableReason::ProviderOutage => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LLMSpec {
    pub cost: f32,
    pub quality: f32,
    pub speed: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LLMProvider {
    OpenAI,
    Anthropic,
    Google,
    Xai,
    Bedrock,
    /// Models served through an OpenAI-compatible proxy (e.g. LiteLLM).
    LiteLLM,
    /// Models selected and executed by an Agent Client Protocol runtime.
    Acp,
    Unknown,
}

impl LLMProvider {
    /// Maps an LLMProvider to its corresponding icon.
    pub fn icon(&self) -> Option<Icon> {
        match self {
            LLMProvider::OpenAI => Some(Icon::OpenAILogo),
            LLMProvider::Anthropic => Some(Icon::ClaudeLogo),
            LLMProvider::Google => Some(Icon::GeminiLogo),
            LLMProvider::Bedrock => Some(Icon::BedrockLogo),
            LLMProvider::LiteLLM => Some(Icon::OpenAILogo),
            LLMProvider::Acp => Some(Icon::Terminal),
            LLMProvider::Xai => None,
            LLMProvider::Unknown => None,
        }
    }

    /// Human-readable provider name for user-facing copy.
    pub fn display_name(&self) -> &'static str {
        match self {
            LLMProvider::OpenAI => "OpenAI",
            LLMProvider::Anthropic => "Anthropic",
            LLMProvider::Google => "Google",
            LLMProvider::Xai => "xAI",
            LLMProvider::Bedrock => "AWS Bedrock",
            LLMProvider::LiteLLM => "LiteLLM",
            LLMProvider::Acp => "ACP",
            LLMProvider::Unknown => "this provider",
        }
    }
}

/// The host where an LLM can be routed to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LLMModelHost {
    DirectApi,
    AwsBedrock,
    CustomEndpoint,
    GeminiEnterprise,
    #[serde(other)]
    Unknown,
}

/// Configuration for routing an LLM to a specific host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutingHostConfig {
    pub enabled: bool,
    pub model_routing_host: LLMModelHost,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LLMContextWindow {
    #[serde(default)]
    pub is_configurable: bool,
    #[serde(default)]
    pub min: u32,
    #[serde(default)]
    pub max: u32,
    #[serde(default)]
    pub default_max: u32,
}

/// Metadata about an LLM.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LLMInfo {
    pub display_name: String,
    pub base_model_name: String,
    pub id: LLMId,
    pub reasoning_level: Option<String>,
    /// Exact provider model identity for grouping locally discovered reasoning variants.
    /// Older catalogs and ACP retain their existing display-name grouping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_model_id: Option<String>,
    pub usage_metadata: LLMUsageMetadata,
    pub description: Option<String>,
    pub disable_reason: Option<DisableReason>,
    pub vision_supported: bool,
    pub spec: Option<LLMSpec>,
    pub provider: LLMProvider,
    pub host_configs: HashMap<LLMModelHost, RoutingHostConfig>,
    pub discount_percentage: Option<f32>,
    pub context_window: LLMContextWindow,
}

impl<'de> Deserialize<'de> for LLMInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        /// Helper type that can deserialize host_configs from either:
        /// - A Vec (wire format from server)
        /// - A HashMap (cached format after commit a8a82421c3)
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum HostConfigsWire {
            Vec(Vec<RoutingHostConfig>),
            Map(HashMap<LLMModelHost, RoutingHostConfig>),
        }

        impl Default for HostConfigsWire {
            fn default() -> Self {
                HostConfigsWire::Vec(Vec::new())
            }
        }

        #[derive(Deserialize)]
        struct WireLLMInfo {
            display_name: String,
            #[serde(default)]
            base_model_name: Option<String>,
            id: LLMId,
            #[serde(default)]
            reasoning_level: Option<String>,
            #[serde(default)]
            reasoning_model_id: Option<String>,
            usage_metadata: LLMUsageMetadata,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            disable_reason: Option<DisableReason>,
            #[serde(default)]
            vision_supported: bool,
            #[serde(default)]
            spec: Option<LLMSpec>,
            provider: LLMProvider,
            #[serde(default)]
            host_configs: HostConfigsWire,
            #[serde(default)]
            discount_percentage: Option<f32>,
            #[serde(default)]
            context_window: LLMContextWindow,
        }

        let wire = WireLLMInfo::deserialize(deserializer)?;
        let host_configs = match wire.host_configs {
            HostConfigsWire::Map(map) => map,
            HostConfigsWire::Vec(vec) => {
                let mut map = HashMap::new();
                for config in vec {
                    let host = config.model_routing_host.clone();
                    if map.insert(host.clone(), config).is_some() {
                        log::warn!(
                            "Duplicate LLMModelHost entry for {:?}, using latest value",
                            host
                        );
                    }
                }
                map
            }
        };
        Ok(Self {
            base_model_name: wire
                .base_model_name
                .unwrap_or_else(|| wire.display_name.clone()),
            vision_supported: wire.vision_supported,
            provider: wire.provider,
            display_name: wire.display_name,
            id: wire.id,
            reasoning_level: wire.reasoning_level,
            reasoning_model_id: wire.reasoning_model_id,
            usage_metadata: wire.usage_metadata,
            description: wire.description,
            disable_reason: wire.disable_reason,
            spec: wire.spec,
            host_configs,
            discount_percentage: wire.discount_percentage,
            context_window: wire.context_window,
        })
    }
}

/// Deduplicates a list of LLMInfo choices by base_model_name and returns an alphabetically sorted
/// list of display names.
pub fn dedupe_model_display_names<'a>(
    choices: impl IntoIterator<Item = &'a LLMInfo>,
) -> Vec<String> {
    let names: HashSet<String> = choices
        .into_iter()
        .map(|choice| choice.base_model_name.clone())
        .collect();
    let mut sorted: Vec<String> = names.into_iter().collect();
    sorted.sort();
    sorted
}

impl LLMInfo {
    /// Returns the display name for the LLM, to be used in the LLM selector menu.
    pub fn menu_display_name(&self) -> String {
        // Custom model routers carry a routing/source description that belongs in
        // the sidecar detail panel, not inline in the chip label. Appending it
        // here would produce a redundant "(Routes by … · …)" suffix.
        if custom_model_routers::is_custom_router_id(self.id.as_str()) {
            return self.display_name.clone();
        }
        self.label_with_provider(&self.display_name)
    }

    pub fn label_with_provider(&self, label: &str) -> String {
        if custom_model_routers::is_custom_router_id(self.id.as_str()) {
            return label.to_string();
        }
        match self.description.as_deref().filter(|name| !name.trim().is_empty()) {
            Some(name) => format!("{label} ({name})"),
            None => label.to_string(),
        }
    }

    /// Reasoning menus must never merge models belonging to different connections.
    pub fn reasoning_group_key(&self) -> String {
        let connection = self
            .id
            .as_str()
            .rsplit_once("::connection::")
            .map(|(_, connection)| connection)
            .unwrap_or_default();
        let model_identity = self
            .reasoning_model_id
            .as_deref()
            .unwrap_or(&self.base_model_name);
        format!(
            "{model_identity}\u{1f}{}\u{1f}{:?}\u{1f}{connection}",
            self.description.as_deref().unwrap_or_default(),
            self.provider
        )
    }

    /// Returns the given model's base name.
    /// For non-reasoning models, this is the same as the display name.
    /// E.g. gpt-5.1 (low reasoning) -> gpt-5.1
    pub fn base_model_name(&self) -> &str {
        &self.base_model_name
    }

    /// Returns true if this model has a reasoning level configured.
    pub fn has_reasoning_level(&self) -> bool {
        self.reasoning_level.is_some()
    }

    /// Returns the reasoning level label formatted for display.
    pub fn reasoning_level(&self) -> Option<String> {
        self.reasoning_level.clone()
    }

    #[cfg(feature = "integration_tests")]
    fn new_for_test(llm_name: &str) -> Self {
        Self {
            display_name: llm_name.to_string(),
            base_model_name: llm_name.to_string(),
            id: llm_name.into(),
            reasoning_level: None,
            reasoning_model_id: None,
            usage_metadata: LLMUsageMetadata {
                request_multiplier: 1,
                credit_multiplier: None,
            },
            description: None,
            disable_reason: None,
            vision_supported: false, // Default to false for tests
            spec: None,
            provider: LLMProvider::Unknown,
            host_configs: HashMap::new(),
            discount_percentage: None,
            context_window: LLMContextWindow::default(),
        }
    }
}

/// The set of LLMs available for a feature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AvailableLLMs {
    /// The Warp "default" LLM.
    default_id: LLMId,
    choices: Vec<LLMInfo>,
    #[serde(skip)]
    legacy_reasoning_aliases: HashMap<LLMId, LLMId>,

    #[serde(default)]
    preferred_codex_model_id: Option<LLMId>,
}

impl AvailableLLMs {
    /// Constructs an `AvailableLLMs` instance from the given default ID and choices.
    ///
    /// If choices is empty, returns an error.
    ///
    /// If default_id is not a valid ID present in `choices`, takes the first choice in `choices
    /// and uses it as the default.
    pub fn new<T: Into<LLMInfo>>(
        mut default_id: LLMId,
        choices: impl IntoIterator<Item = T>,
        preferred_codex_model_id: Option<LLMId>,
    ) -> Result<Self, anyhow::Error> {
        let choices: Vec<LLMInfo> = choices.into_iter().map(Into::into).collect();
        if choices.is_empty() {
            return Err(anyhow::anyhow!(
                "Tried to create AvailableLLMs with empty`choices`.",
            ));
        } else if !choices.iter().any(|info| info.id == default_id) {
            let fallback_default = choices
                .first()
                .ok_or_else(|| anyhow::anyhow!("Choices should not be empty"))?;
            log::error!(
                "Default LLM ID {} not present in choices, falling back to first choice {}",
                default_id,
                fallback_default.display_name
            );
            default_id = fallback_default.id.clone();
        }

        Ok(Self {
            default_id,
            choices: choices.into_iter().collect(),
            legacy_reasoning_aliases: HashMap::new(),
            preferred_codex_model_id,
        })
    }

    fn info_for_id(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.choices.iter().find(|info| info.id == *id).or_else(|| {
            let resolved = self.legacy_reasoning_aliases.get(id)?;
            self.choices.iter().find(|info| info.id == *resolved)
        })
    }

    /// Returns the info for the given id only if the model is usable (present
    /// and not effectively disabled for the current user).
    fn usable_info_for_id(&self, id: &LLMId, app: &AppContext) -> Option<&LLMInfo> {
        self.info_for_id(id).filter(|info| {
            let has_byok_key = is_using_api_key_for_provider(&info.provider, app);
            info.disable_reason
                .as_ref()
                .is_none_or(|reason| !reason.should_clear_preference(has_byok_key))
        })
    }

    fn default_llm_info(&self) -> &LLMInfo {
        static NO_PROVIDER_FALLBACK: std::sync::LazyLock<LLMInfo> =
            std::sync::LazyLock::new(|| LLMInfo {
                display_name: "No models configured".to_owned(),
                base_model_name: "No models configured".to_owned(),
                id: "none".to_owned().into(),
                reasoning_level: None,
                reasoning_model_id: None,
                usage_metadata: LLMUsageMetadata {
                    request_multiplier: 1,
                    credit_multiplier: None,
                },
                description: Some("Enable an AI runtime in Models settings".to_string()),
                disable_reason: Some(DisableReason::Unavailable),
                vision_supported: false,
                spec: None,
                provider: LLMProvider::Unknown,
                host_configs: HashMap::new(),
                discount_percentage: None,
                context_window: LLMContextWindow::default(),
            });

        self.info_for_id(&self.default_id)
            .or_else(|| self.choices.first())
            .unwrap_or(&NO_PROVIDER_FALLBACK)
    }

    #[cfg(feature = "integration_tests")]
    pub fn new_for_test(llm_name: &str) -> Self {
        Self {
            default_id: llm_name.into(),
            choices: vec![LLMInfo::new_for_test(llm_name)],
            preferred_codex_model_id: None,
        }
    }
}

/// The set of models available to the client, grouped by the feature they support.
/// This is fetched from the server and cached.
///
/// Currently, if a model is available for multiple features,
/// it will appear denormalized in each of the feature's
/// [`AvailableLLMs`]. While this denormalization doesn't add much value today,
/// it eventually lets us add feature-specific properties to an [`LLMInfo`].
///
/// NOTE: This used to include a `planning` field; this was removed after planning via subagent was
/// deprecated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelsByFeature {
    pub agent_mode: AvailableLLMs,
    pub coding: AvailableLLMs,
    /// The set of LLMs available for CLI agent.
    /// This field is optional during deserialization, as older clients might not have this field.
    #[serde(default)]
    pub cli_agent: Option<AvailableLLMs>,
    /// The set of LLMs available for computer use agent.
    /// This field is optional during deserialization, as older clients might not have this field.
    #[serde(default)]
    pub computer_use: Option<AvailableLLMs>,
}

impl ModelsByFeature {
    /// Returns the info about the LLM identified by `id`, if we have it.
    ///
    /// For models that are available across multiple features,
    /// any one of the metadata will be returned.
    fn info_for_id(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.agent_mode.info_for_id(id)
    }
}

/// Returns the default AvailableLLMs for computer use.
/// Used both in `ModelsByFeature::default()` and as a fallback in `get_computer_use_available()`.
fn default_computer_use_llms() -> AvailableLLMs {
    AvailableLLMs {
        legacy_reasoning_aliases: HashMap::new(),
        default_id: "computer-use-agent-auto".to_owned().into(),
        choices: vec![LLMInfo {
            display_name: "auto".to_owned(),
            base_model_name: "auto".to_owned(),
            id: "computer-use-agent-auto".to_owned().into(),
            reasoning_level: None,
            reasoning_model_id: None,
            usage_metadata: LLMUsageMetadata {
                request_multiplier: 1,
                credit_multiplier: None,
            },
            description: None,
            disable_reason: None,
            vision_supported: false,
            spec: None,
            provider: LLMProvider::Unknown,
            host_configs: HashMap::new(),
            discount_percentage: None,
            context_window: LLMContextWindow::default(),
        }],
        preferred_codex_model_id: None,
    }
}

impl Default for ModelsByFeature {
    /// Returns a minimal placeholder. The real model list is populated exclusively
    /// by `inject_bedrock_models` and `inject_openai_models` based on local settings.
    /// The placeholder entry uses `LLMProvider::Unknown` so it gets stripped by
    /// `inject_bedrock_models` once real models are loaded.
    fn default() -> Self {
        let placeholder = || AvailableLLMs {
            legacy_reasoning_aliases: HashMap::new(),
            default_id: "placeholder".to_owned().into(),
            choices: vec![LLMInfo {
                display_name: "No models configured".to_owned(),
                base_model_name: "No models configured".to_owned(),
                id: "placeholder".to_owned().into(),
                reasoning_level: None,
                reasoning_model_id: None,
                usage_metadata: LLMUsageMetadata {
                    request_multiplier: 1,
                    credit_multiplier: None,
                },
                description: Some("Enable an AI runtime in Models settings".to_string()),
                disable_reason: None,
                vision_supported: false,
                spec: None,
                provider: LLMProvider::Unknown,
                host_configs: HashMap::new(),
                discount_percentage: None,
                context_window: LLMContextWindow::default(),
            }],
            preferred_codex_model_id: None,
        };
        Self {
            agent_mode: placeholder(),
            coding: placeholder(),
            cli_agent: Some(placeholder()),
            computer_use: Some(default_computer_use_llms()),
        }
    }
}

enum UpdatePopupVisibilityState {
    WaitingToBeShown,
    Visible(EntityId),
    Hidden,
}

struct AvailableLLMsUpdate {
    new_choices: Vec<LLMInfo>,
    popup_visibility_state: Arc<FairMutex<UpdatePopupVisibilityState>>,
}

/// Singleton model holding user/workspace LLM preferences, including the set of LLMs available for
/// use as well as the user's preferred LLM for Agent Mode.
pub struct LLMPreferences {
    models_by_feature: ModelsByFeature,
    last_update: Option<AvailableLLMsUpdate>,
    base_llm_for_terminal_view: HashMap<EntityId, LLMId>,
    /// Synthetic `LLMInfo` entries built from the user's `ApiKeyManager.custom_endpoints` so
    /// custom models surface in the model picker and resolve through `info_for_id` lookups.
    /// Each entry's `id` is the model's `config_key` (UUID), which is also what flows out to
    /// `Request.Settings.custom_model_providers.providers[*].models[*].config_key`.
    ///
    /// Rebuilt from scratch on every `ApiKeyManagerEvent::KeysUpdated`, so adds, edits, and
    /// removals all immediately propagate to the picker.
    custom_llms: Vec<LLMInfo>,
    /// All custom model routers, including both local and cloud-backed.
    custom_model_routers: Vec<CustomModelRouter>,
    #[cfg(not(target_family = "wasm"))]
    openai_provider_routing: HashMap<String, super::openai::client::OpenAIClientConfig>,
    /// Models fetched from the OpenAI-compatible /models endpoint at runtime.
    /// Used as a short-lived fallback while the fetched list is persisted to settings.
    #[cfg(not(target_family = "wasm"))]
    fetched_openai_models: Vec<OpenAIModelConfig>,
    #[cfg(not(target_family = "wasm"))]
    catalog_connections: Vec<OpenAIProviderConfig>,
    #[cfg(not(target_family = "wasm"))]
    catalog_refresh_generations: HashMap<Option<usize>, u64>,
    #[cfg(not(target_family = "wasm"))]
    chatgpt_subscription_models_refresh_in_flight: bool,
    bedrock_models_refresh_in_flight: bool,
    bedrock_verification_generation: u64,
    #[cfg(not(target_family = "wasm"))]
    bedrock_verified_config: Option<super::provider::client::BedrockClientConfig>,
    #[cfg(not(target_family = "wasm"))]
    bedrock_verified_model_ids: HashSet<String>,
    /// External defaults are read once at startup, never during settings updates.
    #[cfg(not(target_family = "wasm"))]
    bedrock_default_model_id: Option<String>,
    #[cfg(not(target_family = "wasm"))]
    acp_selections: HashMap<LLMId, AcpModelSelection>,
}

#[cfg(not(target_family = "wasm"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AcpModelSelection {
    pub(crate) provider_id: String,
    pub(crate) agent_id: String,
    pub(crate) launch_fingerprint: String,
    pub(crate) config_values: BTreeMap<String, serde_json::Value>,
}

impl LLMPreferences {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let models_by_feature = get_cached_models(ctx).unwrap_or_default();

        ctx.subscribe_to_model(&NetworkStatus::handle(ctx), |me, _, event, ctx| {
            if let NetworkStatusEvent::NetworkStatusChanged {
                new_status: NetworkStatusKind::Online,
            } = event
            {
                me.refresh_authed_models(ctx);
                me.refresh_chatgpt_subscription_models(ctx);
            }
        });

        // TODO: Instead of querying this ad-hoc upon a successful log in, we should add the
        // available LLMs query to the general workspace metadata query which is polled
        // and hooked up to workspace changes. For that to work, each user would need to
        // have a personal workspace. This is a stop-gap.
        ctx.subscribe_to_model(&AuthManager::handle(ctx), |me, _, event, ctx| {
            if let AuthManagerEvent::AuthComplete = event {
                me.refresh_authed_models(ctx);
                me.refresh_chatgpt_subscription_models(ctx);
            }
        });

        #[cfg(not(target_family = "wasm"))]
        if ctx
            .try_get_singleton_model_as_ref::<super::chatgpt_auth::ChatGPTAuthModel>()
            .is_some()
        {
            ctx.subscribe_to_model(
                &super::chatgpt_auth::ChatGPTAuthModel::handle(ctx),
                |me, _, event, ctx| {
                    if matches!(
                        event,
                        super::chatgpt_auth::ChatGPTAuthModelEvent::StateChanged
                    ) && matches!(
                        super::chatgpt_auth::ChatGPTAuthModel::as_ref(ctx).state(),
                        super::chatgpt_auth::ChatGPTAuthState::Connected
                    ) {
                        me.refresh_chatgpt_subscription_models(ctx);
                    }
                },
            );
        }

        ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), |me, _, event, ctx| {
            if let UserWorkspacesEvent::TeamsChanged = event {
                me.sanitize_disabled_custom_model_preferences(ctx);
                me.refresh_authed_models(ctx);
            }
        });

        // Re-reconcile disabled model preferences when BYOK keys change, since
        // RequiresUpgrade models may become usable or unusable.
        // Also rebuild `custom_llms` so adds/edits/removals to the user's custom endpoints
        // immediately flow through to the model picker.
        ctx.subscribe_to_model(&ApiKeyManager::handle(ctx), |me, _, _event, ctx| {
            me.reconcile_disabled_model_preferences(ctx);
            ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
        });

        // Rebuild custom model routers whenever the local `model_configs/` directory
        // changes, and reconcile any now-stale local selection.
        if FeatureFlag::CustomModelRouters.is_enabled() {
            ctx.subscribe_to_model(&WarpConfig::handle(ctx), |me, _, event, ctx| {
                if matches!(event, WarpConfigUpdateEvent::ModelConfigs) {
                    me.rebuild_custom_model_routers(ctx);
                    me.reconcile_stale_custom_router_selection(ctx);
                }
            });
        }

        // Rebuild only the provider affected by a settings change.
        #[cfg(not(target_family = "wasm"))]
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            use crate::settings::AISettingsChangedEvent;
            let bedrock_changed = matches!(
                event,
                AISettingsChangedEvent::BedrockEnabled { .. }
                    | AISettingsChangedEvent::BedrockModels { .. }
                    | AISettingsChangedEvent::BedrockAuthMethod { .. }
                    | AISettingsChangedEvent::BedrockProfile { .. }
                    | AISettingsChangedEvent::BedrockRegion { .. }
                    | AISettingsChangedEvent::BedrockAccessKeyId { .. }
                    | AISettingsChangedEvent::BedrockSecretAccessKey { .. }
                    | AISettingsChangedEvent::BedrockCrossRegionInference { .. }
            );
            let other_provider_changed = matches!(
                event,
                AISettingsChangedEvent::OpenAIEnabled { .. }
                    | AISettingsChangedEvent::OpenAIBaseUrl { .. }
                    | AISettingsChangedEvent::OpenAIApiKey { .. }
                    | AISettingsChangedEvent::OpenAIModels { .. }
                    | AISettingsChangedEvent::OpenAIProviders { .. }
                    | AISettingsChangedEvent::AcpEnabled { .. }
                    | AISettingsChangedEvent::AcpProviders { .. }
                    | AISettingsChangedEvent::AcpAgents { .. }
                    | AISettingsChangedEvent::AcpAgentId { .. }
            );
            if bedrock_changed || other_provider_changed {
                let previous_ids: Vec<_> = me
                    .models_by_feature
                    .agent_mode
                    .choices
                    .iter()
                    .filter(|model| {
                        matches!(model.provider, LLMProvider::LiteLLM | LLMProvider::Bedrock)
                    })
                    .map(|model| model.id.as_str().to_owned())
                    .collect();
                if bedrock_changed {
                    me.inject_bedrock_models(ctx);
                }
                if other_provider_changed {
                    me.inject_openai_models(ctx);
                } else if matches!(event, AISettingsChangedEvent::BedrockEnabled { .. }) {
                    // ACP can expose Bedrock models, gated by the same enable switch.
                    me.inject_acp_models(ctx);
                }
                if matches!(
                    event,
                    AISettingsChangedEvent::OpenAIEnabled { .. }
                        | AISettingsChangedEvent::OpenAIBaseUrl { .. }
                        | AISettingsChangedEvent::OpenAIApiKey { .. }
                ) {
                    me.fetch_openai_models_from_endpoint(ctx);
                }
                if other_provider_changed {
                    me.refresh_changed_catalog_connections(ctx);
                }
                // Safety: ensure the default model is still present in choices.
                // If all provider models were removed, the default_id would dangle.
                me.ensure_default_model_present();
                let removed: HashSet<_> = previous_ids
                    .into_iter()
                    .map(LLMId::from)
                    .filter(|id| me.get_llm_info(id).is_none())
                    .collect();
                if !removed.is_empty() {
                    me.base_llm_for_terminal_view
                        .retain(|_, id| !removed.contains(id));
                    if ctx
                        .try_get_singleton_model_as_ref::<AIExecutionProfilesModel>()
                        .is_some()
                    {
                        me.reconcile_disabled_model_preferences(ctx);
                    }
                    me.trigger_snapshot_save(ctx);
                    ctx.emit(LLMPreferencesEvent::UpdatedActiveAgentModeLLM);
                    ctx.emit(LLMPreferencesEvent::UpdatedActiveCodingLLM);
                }
                ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
            }
        });

        let base_llm_for_terminal_view = HashMap::new();
        let custom_llms = Vec::new();

        let mut me = Self {
            models_by_feature,
            last_update: None,
            base_llm_for_terminal_view,
            custom_llms,
            custom_model_routers: Vec::new(),
            #[cfg(not(target_family = "wasm"))]
            openai_provider_routing: HashMap::new(),
            #[cfg(not(target_family = "wasm"))]
            fetched_openai_models: Vec::new(),
            #[cfg(not(target_family = "wasm"))]
            catalog_connections: Vec::new(),
            #[cfg(not(target_family = "wasm"))]
            catalog_refresh_generations: HashMap::new(),
            #[cfg(not(target_family = "wasm"))]
            chatgpt_subscription_models_refresh_in_flight: false,
            bedrock_models_refresh_in_flight: false,
            bedrock_verification_generation: 0,
            #[cfg(not(target_family = "wasm"))]
            bedrock_verified_config: None,
            #[cfg(not(target_family = "wasm"))]
            bedrock_verified_model_ids: HashSet::new(),
            #[cfg(not(target_family = "wasm"))]
            bedrock_default_model_id: ExternalBedrockConfig::load().anthropic_model,
            #[cfg(not(target_family = "wasm"))]
            acp_selections: HashMap::new(),
        };

        // Seed from any already-loaded local config (the async load emits
        // `ModelConfigs` shortly after startup to populate fully).
        if FeatureFlag::CustomModelRouters.is_enabled() {
            me.rebuild_custom_model_routers(ctx);
        }

        // In agent mode eval builds, eagerly kick off a fetch of the model list from the server
        // so that it's available by the time test steps like `set_preferred_agent_mode_llm` run.
        // In production, this is handled reactively (on auth complete, network online, etc.)
        // to avoid duplicate requests at startup.
        #[cfg(feature = "agent_mode_evals")]
        me.refresh_available_models(ctx);

        #[cfg(not(target_family = "wasm"))]
        {
            // Saved toggle preferences survive startup, but access must be
            // verified against AWS before a Bedrock model becomes selectable.
            me.inject_bedrock_models(ctx);
            me.refresh_bedrock_models(ctx);
            me.inject_openai_models(ctx);
            me.ensure_default_model_present();
            me.fetch_openai_models_from_endpoint(ctx);
            me.refresh_changed_catalog_connections(ctx);
            me.refresh_chatgpt_subscription_models(ctx);
            me.schedule_chatgpt_subscription_model_refresh(ctx);
        }

        me
    }

    #[cfg(not(target_family = "wasm"))]
    fn refresh_bedrock_models(&mut self, ctx: &mut ModelContext<Self>) {
        if self.bedrock_models_refresh_in_flight {
            return;
        }
        let settings = AISettings::as_ref(ctx);
        if !*settings.bedrock_enabled.value() {
            return;
        }
        self.bedrock_models_refresh_in_flight = true;
        let generation = self.begin_bedrock_verification();
        let config = bedrock_discovery_config(settings);
        let revision = super::provider::authentication::connection_revision(ctx);
        let requested_config = config.clone();
        self.bedrock_verified_config = None;
        self.bedrock_verified_model_ids.clear();
        self.inject_bedrock_models(ctx);

        let _ = ctx.spawn(
            async move { crate::ai::provider::discovery::discover_available_models(config).await },
            move |me, result, ctx| {
                let result = result.map(|discovery| {
                    for record in discovery.records {
                        crate::ai::remote_logging::log_model_event(ctx, record);
                    }
                    discovery.models
                });
                me.bedrock_models_refresh_in_flight = false;
                if me.bedrock_verification_generation != generation
                    || super::provider::authentication::connection_revision(ctx) != revision
                {
                    return;
                }
                if !*AISettings::as_ref(ctx).bedrock_enabled.value() {
                    return;
                }
                if bedrock_discovery_config(AISettings::as_ref(ctx)) != requested_config {
                    return;
                }
                match result {
                    Ok(models) => {
                        // Verification belongs to this live connection, not serialized settings.
                        me.bedrock_verified_model_ids =
                            models.iter().map(|model| model.model_id.clone()).collect();
                        me.bedrock_verified_config = Some(requested_config);
                        AISettings::handle(ctx).update(ctx, |settings, ctx| {
                            let models = super::provider::discovery::preserve_model_toggles(
                                settings.bedrock_models.value(),
                                models,
                            );
                            if let Err(error) = settings.bedrock_models.set_value(models, ctx) {
                                log::warn!(
                                    "[bedrock] Failed to persist discovered models: {error}"
                                );
                            }
                        });
                        me.inject_bedrock_models(ctx);
                        me.ensure_default_model_present();
                        ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
                    }
                    Err(error) => {
                        log::warn!("[bedrock] Startup model discovery unavailable: {error}");
                    }
                }
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn begin_bedrock_verification(&mut self) -> u64 {
        self.bedrock_verification_generation += 1;
        self.bedrock_verification_generation
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn record_bedrock_verification(
        &mut self,
        generation: u64,
        config: super::provider::client::BedrockClientConfig,
        models: &[BedrockModelConfig],
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        if self.bedrock_verification_generation != generation {
            return false;
        }
        self.bedrock_verified_model_ids =
            models.iter().map(|model| model.model_id.clone()).collect();
        self.bedrock_verified_config = Some(config);
        self.inject_bedrock_models(ctx);
        self.ensure_default_model_present();
        ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
        true
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn bedrock_model_is_verified(&self, model_id: &str, ctx: &AppContext) -> bool {
        self.bedrock_verified_config.as_ref()
            == Some(&bedrock_discovery_config(AISettings::as_ref(ctx)))
            && self.bedrock_verified_model_ids.contains(model_id)
    }

    #[cfg(not(target_family = "wasm"))]
    fn inject_bedrock_models(&mut self, ctx: &AppContext) {
        // Remove only previously injected Bedrock entries. Other enabled provider
        // sections (OpenAI/LiteLLM, ChatGPT subscription, ACP) must remain
        // available in the shared model picker.
        self.models_by_feature
            .agent_mode
            .choices
            .retain(|m| m.provider != LLMProvider::Bedrock);
        self.models_by_feature
            .coding
            .choices
            .retain(|m| m.provider != LLMProvider::Bedrock);
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            cli.choices.retain(|m| m.provider != LLMProvider::Bedrock);
        }

        let settings = AISettings::as_ref(ctx);
        if !*settings.bedrock_enabled.value() {
            return;
        }

        // Bedrock models are populated only by the control-plane discovery
        // flow. Never fall back to a static catalog or external config here.
        let discovered_models = settings.bedrock_models.value();
        let region = settings.bedrock_region.value();
        let cross_region = *settings.bedrock_cross_region_inference.value();
        let connection_is_verified = self.bedrock_verified_config.as_ref()
            == Some(&bedrock_discovery_config(settings));

        for model in discovered_models {
            if !model.enabled
                || !connection_is_verified
                || !self.bedrock_verified_model_ids.contains(&model.model_id)
            {
                continue;
            }
            let metadata = model_metadata(&model.model_id);
            if model.runtime_supported == Some(false)
                || metadata.is_some_and(|metadata| metadata.supports_agent_runtime() == Some(false))
            {
                log::debug!(
                    "[bedrock] Excluding {} from Agent Mode because its catalog metadata marks it as incompatible with streaming Converse",
                    model.model_id
                );
                continue;
            }

            let model_id = if cross_region && !region.is_empty() {
                super::provider::models::apply_cross_region_prefix(&model.model_id, region)
            } else {
                model.model_id.clone()
            };
            let context_window = metadata
                .and_then(|metadata| metadata.context_window_tokens)
                .map(|tokens| LLMContextWindow {
                    is_configurable: false,
                    min: tokens,
                    max: tokens,
                    default_max: tokens,
                })
                .unwrap_or_default();

            let llm_info = LLMInfo {
                id: LLMId::from(model_id.as_str()),
                display_name: model.display_name.clone(),
                base_model_name: model.display_name.clone(),
                reasoning_level: None,
                reasoning_model_id: Some(model_id.clone()),
                usage_metadata: LLMUsageMetadata {
                    request_multiplier: 1,
                    credit_multiplier: None,
                },
                description: Some("AWS Bedrock".to_string()),
                disable_reason: None,
                vision_supported: model.vision_supported
                    || metadata.is_some_and(|metadata| metadata.supports_vision_input()),
                spec: None,
                provider: LLMProvider::Bedrock,
                host_configs: HashMap::from([(
                    LLMModelHost::AwsBedrock,
                    RoutingHostConfig {
                        enabled: true,
                        model_routing_host: LLMModelHost::AwsBedrock,
                    },
                )]),
                discount_percentage: None,
                context_window,
            };
            self.models_by_feature
                .agent_mode
                .choices
                .push(llm_info.clone());
            self.models_by_feature.coding.choices.push(llm_info.clone());
            if let Some(ref mut cli) = self.models_by_feature.cli_agent {
                cli.choices.push(llm_info);
            }
        }

        // Default agent mode to ANTHROPIC_MODEL from external config if set,
        // otherwise Claude Opus 4.6, falling back to the first available model.
        if let Some(id) = self
            .bedrock_default_model_id
            .as_ref()
            .and_then(|model_id| {
                // Match by model_id (with or without cross-region prefix)
                self.models_by_feature
                    .agent_mode
                    .choices
                    .iter()
                    .find(|m| {
                        m.id.as_str() == model_id
                            || m.id.as_str().ends_with(model_id)
                            || model_id.ends_with(m.id.as_str())
                    })
                    .map(|m| {
                        log::info!(
                            "[bedrock] Setting default agent mode model from ANTHROPIC_MODEL: {} -> {}",
                            model_id,
                            m.display_name
                        );
                        m.id.clone()
                    })
            })
            .or_else(|| {
                self.models_by_feature
                    .agent_mode
                    .choices
                    .iter()
                    .find(|m| m.display_name.contains("Opus 4.6"))
                    .or_else(|| self.models_by_feature.agent_mode.choices.first())
                    .map(|m| m.id.clone())
            })
        {
            if !self.models_by_feature.agent_mode.choices.iter().any(|model| {
                model.id == self.models_by_feature.agent_mode.default_id
            }) {
                self.models_by_feature.agent_mode.default_id = id;
            }
        }

        // Default coding to Claude Sonnet 4.6, falling back to the first available model.
        if let Some(id) = self
            .models_by_feature
            .coding
            .choices
            .iter()
            .find(|m| m.display_name.contains("Sonnet 4.6"))
            .or_else(|| self.models_by_feature.coding.choices.first())
            .map(|m| m.id.clone())
        {
            if !self.models_by_feature.coding.choices.iter().any(|model| {
                model.id == self.models_by_feature.coding.default_id
            }) {
                self.models_by_feature.coding.default_id = id;
            }
        }

        // Default CLI agent to the first available model.
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            if let Some(id) = cli.choices.first().filter(|_| {
                !cli.choices.iter().any(|model| model.id == cli.default_id)
            }).map(|m| m.id.clone()) {
                cli.default_id = id;
            }
        }
    }

    /// Injects models from OpenAI-compatible providers into the available model lists.
    ///
    /// Supports two configuration paths:
    /// 1. Legacy single-provider: `ai.openai.{base_url, api_key, models}`
    /// 2. Multi-provider: `ai.providers[]` (each with name, base_url, api_key, models)
    ///
    /// Also populates `openai_provider_routing` so that `resolve_provider_config` can
    /// dispatch requests to the correct endpoint per model.
    #[cfg(not(target_family = "wasm"))]
    fn inject_openai_models(&mut self, ctx: &AppContext) {
        use super::openai::client::OpenAIClientConfig;

        // Remove any previously injected LiteLLM models
        self.models_by_feature
            .agent_mode
            .choices
            .retain(|m| m.provider != LLMProvider::LiteLLM);
        self.models_by_feature
            .coding
            .choices
            .retain(|m| m.provider != LLMProvider::LiteLLM);
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            cli.choices.retain(|m| m.provider != LLMProvider::LiteLLM);
        }
        self.openai_provider_routing.clear();
        self.models_by_feature.agent_mode.legacy_reasoning_aliases.clear();
        self.models_by_feature.coding.legacy_reasoning_aliases.clear();
        if let Some(cli) = &mut self.models_by_feature.cli_agent {
            cli.legacy_reasoning_aliases.clear();
        }

        let settings = AISettings::as_ref(ctx);
        self.inject_acp_models(ctx);
        if !settings.is_openai_provider_enabled() {
            return;
        }

        type OpenAIProviderEntry = (
            String,
            OpenAIProviderKind,
            bool,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Vec<OpenAIModelConfig>,
        );
        let mut provider_entries: Vec<OpenAIProviderEntry> = Vec::new();

        let configured_models = settings.openai_models.value().clone();
        let single_provider_models = if configured_models.is_empty() {
            self.fetched_openai_models.clone()
        } else {
            configured_models
        };

        // Only a registry entry for the same endpoint replaces the legacy
        // connection. Adding an unrelated subscription must not hide LiteLLM.
        let include_legacy =
            settings.legacy_openai_provider_enabled() && !single_provider_models.is_empty();
        if include_legacy {
            let base_url = settings.openai_base_url.value().clone();
            let api_key = {
                let key = settings.openai_api_key.value().clone();
                if key.is_empty() {
                    None
                } else {
                    Some(key)
                }
            };
            let name = if base_url.contains("localhost") || base_url.contains("127.0.0.1") {
                "LiteLLM (local)".to_string()
            } else {
                "LiteLLM".to_string()
            };
            provider_entries.push((
                name,
                OpenAIProviderKind::LiteLLM,
                true,
                base_url,
                api_key,
                None,
                None,
                single_provider_models,
            ));
        }

        provider_entries.extend(
            settings
                .openai_providers
                .value()
                .iter()
                .filter_map(|provider| {
                    let missing_credentials = match provider.kind {
                        OpenAIProviderKind::OpenAI => {
                            provider.base_url.trim().is_empty()
                                || provider
                                    .api_key
                                    .as_deref()
                                    .is_none_or(|key| key.trim().is_empty())
                        }
                        OpenAIProviderKind::LiteLLM => provider.base_url.trim().is_empty(),
                        OpenAIProviderKind::Anthropic | OpenAIProviderKind::Gemini => provider
                            .api_key
                            .as_deref()
                            .is_none_or(|key| key.trim().is_empty()),
                        OpenAIProviderKind::VertexAI => provider
                            .project_id
                            .as_deref()
                            .is_none_or(|project| project.trim().is_empty()),
                        OpenAIProviderKind::ChatGPTSubscription => false,
                    };
                    if missing_credentials || provider.models.is_empty() {
                        return None;
                    }
                    Some((
                        provider.name.clone(),
                        provider.kind,
                        provider.enabled,
                        provider.base_url.clone(),
                        provider.api_key.clone(),
                        provider.project_id.clone(),
                        provider.location.clone(),
                        provider.models.clone(),
                    ))
                }),
        );
        // Keep existing raw-model selections routed to registry connections.
        // The legacy connection remains selectable under a qualified key when
        // it serves the same model as a registered provider.
        if include_legacy && provider_entries.len() > 1 {
            provider_entries.rotate_left(1);
        }

        if provider_entries.is_empty() {
            return;
        }

        let mut total_injected = 0;
        let mut seen_model_ids: HashSet<String> = HashSet::new();
        let mut legacy_reserved_model_ids = HashSet::new();
        let mut legacy_reasoning_aliases = HashMap::new();
        let mut seen_connections = HashSet::new();
        for (
            provider_name,
            provider_kind,
            provider_enabled,
            base_url,
            api_key,
            provider_project_id,
            provider_location,
            models,
        ) in provider_entries
        {
            let connection_identity = format!("{provider_kind:?}|{provider_name}|{base_url}|{provider_project_id:?}|{provider_location:?}");
            if !seen_connections.insert(connection_identity.clone()) {
                continue;
            }
            let mut connection_model_ids = HashSet::new();
            for model in &models {
                if !connection_model_ids.insert(model.model_id.clone()) {
                    continue;
                }

                // All providers may advertise discrete effort choices. Keep the
                // default model key stable and route explicit choices separately.
                let reasoning_efforts = std::iter::once(None)
                    .chain(model.reasoning_efforts.iter().map(Some));

                // Choose a namespace once for the whole family. Otherwise a
                // colliding base with unique efforts would split one connection
                // into multiple groups in the model/reasoning selectors.
                let connection_suffix = if seen_model_ids.contains(&model.model_id) {
                    use sha2::{Digest, Sha256};
                    Some(format!("{:x}", Sha256::digest(connection_identity.as_bytes())))
                } else {
                    None
                };
                for reasoning_effort in reasoning_efforts {
                    let reasoning_effort = reasoning_effort.cloned();
                    let mut model_key = reasoning_effort.as_deref().map_or_else(
                        || model.model_id.clone(),
                        |effort| openai_model_variant_id(&model.model_id, effort),
                    );
                    let legacy_key = model_key.clone();
                    let legacy_unclaimed = legacy_reserved_model_ids.insert(legacy_key.clone());
                    if let Some(suffix) = &connection_suffix {
                        model_key = format!("{model_key}::connection::{suffix}");
                    }
                    if !seen_model_ids.insert(model_key.clone()) {
                        continue;
                    }
                    // Disabled entries still reserve their selection keys so
                    // toggling a connection cannot silently reroute another
                    // connection's saved model selection.
                    if !provider_enabled || !model.enabled {
                        continue;
                    }

                    if legacy_unclaimed && reasoning_effort.is_some() && legacy_key != model_key {
                        legacy_reasoning_aliases.insert(LLMId::from(legacy_key), LLMId::from(model_key.clone()));
                    }
                    // Register the routing entry. Reasoning variants keep the provider's
                    // actual model ID while using their synthetic key only for selection.
                    let client_config = OpenAIClientConfig {
                        kind: provider_kind,
                        connection_identity: Some(connection_identity.clone()),
                        base_url: base_url.clone(),
                        api_key: api_key.clone(),
                        project_id: provider_project_id.clone(),
                        location: provider_location.clone(),
                        model: Some(model.model_id.clone()),
                        reasoning_effort: reasoning_effort.clone(),
                        max_input_tokens: Some(openai_model_context_size(model, provider_kind)),
                        max_output_tokens: model.max_output_tokens,
                        supports_system_messages: model.supports_system_messages(),
                    };
                    self.openai_provider_routing
                        .insert(model_key.clone(), client_config);

                    let display_name = reasoning_effort.as_deref().map_or_else(
                        || model.display_name.clone(),
                        |effort| format!("{} ({effort})", model.display_name),
                    );
                    let llm_info = LLMInfo {
                        id: LLMId::from(model_key.as_str()),
                        display_name,
                        base_model_name: model.display_name.clone(),
                        reasoning_level: reasoning_effort,
                        reasoning_model_id: Some(model.model_id.clone()),
                        usage_metadata: LLMUsageMetadata {
                            request_multiplier: 1,
                            credit_multiplier: None,
                        },
                        description: Some(provider_name.clone()),
                        disable_reason: None,
                        vision_supported: model.effective_vision_supported(),
                        spec: None,
                        provider: LLMProvider::LiteLLM,
                        host_configs: HashMap::from([(
                            LLMModelHost::DirectApi,
                            RoutingHostConfig {
                                enabled: true,
                                model_routing_host: LLMModelHost::DirectApi,
                            },
                        )]),
                        discount_percentage: None,
                        context_window: openai_model_context_window(model, provider_kind),
                    };
                    self.models_by_feature
                        .agent_mode
                        .choices
                        .push(llm_info.clone());
                    self.models_by_feature.coding.choices.push(llm_info.clone());
                    if let Some(ref mut cli) = self.models_by_feature.cli_agent {
                        cli.choices.push(llm_info);
                    }
                    total_injected += 1;
                }
            }
        }

        // An exact key, including one reserved by a disabled model, always wins.
        legacy_reasoning_aliases.retain(|legacy_id, _| !seen_model_ids.contains(legacy_id.as_str()));
        for (legacy_id, canonical_id) in &legacy_reasoning_aliases {
            if let Some(config) = self.openai_provider_routing.get(canonical_id.as_str()).cloned() {
                self.openai_provider_routing.entry(legacy_id.to_string()).or_insert(config);
            }
        }
        self.models_by_feature.agent_mode.legacy_reasoning_aliases = legacy_reasoning_aliases.clone();
        self.models_by_feature.coding.legacy_reasoning_aliases = legacy_reasoning_aliases.clone();
        if let Some(cli) = &mut self.models_by_feature.cli_agent {
            cli.legacy_reasoning_aliases = legacy_reasoning_aliases;
        }
        log::info!("[openai/litellm] Injected {total_injected} model(s) into available choices");
    }

    #[cfg(not(target_family = "wasm"))]
    fn inject_acp_models(&mut self, ctx: &AppContext) {
        let previous_acp_model_ids = self.acp_selections.keys().cloned().collect::<HashSet<_>>();
        let remove_previous_acp_models = |choices: &mut Vec<LLMInfo>| {
            choices.retain(|model| !previous_acp_model_ids.contains(&model.id));
        };
        remove_previous_acp_models(&mut self.models_by_feature.agent_mode.choices);
        remove_previous_acp_models(&mut self.models_by_feature.coding.choices);
        if let Some(cli) = &mut self.models_by_feature.cli_agent {
            remove_previous_acp_models(&mut cli.choices);
        }
        self.acp_selections.clear();
        let settings = AISettings::as_ref(ctx);
        let providers = settings.enabled_acp_providers();
        if providers.is_empty() {
            return;
        }
        let bedrock_enabled = *settings.bedrock_enabled.value();
        for provider in providers {
            self.inject_acp_provider_models(&provider, bedrock_enabled);
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn inject_acp_provider_models(&mut self, provider: &AcpProviderConfig, bedrock_enabled: bool) {
        let agent_id = provider.agent_id.trim();
        let agent_id = if agent_id.is_empty() {
            "codex"
        } else {
            agent_id
        };
        let agent_name = acp_agent_display_name(agent_id);
        if provider.config_options.is_empty() {
            self.push_acp_model(provider, &agent_name, &agent_name, BTreeMap::new(), None);
            return;
        }

        let model_option = provider
            .config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("model"));
        let Some(model_option) = model_option else {
            let selection =
                crate::ai::acp::AcpRuntimeModel::current_config_values(&provider.config_options);
            self.push_acp_model(provider, &agent_name, &agent_name, selection, None);
            return;
        };
        let reasoning_option = provider
            .config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("thought_level"));
        for value in model_option
            .options
            .iter()
            .filter(|value| acp_model_is_enabled(&value.value, bedrock_enabled))
        {
            let mut selection =
                crate::ai::acp::AcpRuntimeModel::current_config_values(&provider.config_options);
            selection.insert(model_option.id.clone(), value.value.clone());
            if let Some(reasoning_option) =
                reasoning_option.filter(|option| !option.options.is_empty())
            {
                for reasoning in &reasoning_option.options {
                    let mut selection = selection.clone();
                    selection.insert(reasoning_option.id.clone(), reasoning.value.clone());
                    self.push_acp_model(
                        provider,
                        &value.name,
                        &value.name,
                        selection,
                        Some(reasoning),
                    );
                }
            } else {
                self.push_acp_model(provider, &value.name, &value.name, selection, None);
            }
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn push_acp_model(
        &mut self,
        provider: &AcpProviderConfig,
        display_name: &str,
        base_model_name: &str,
        selection: BTreeMap<String, serde_json::Value>,
        reasoning: Option<&AcpConfigValueSettings>,
    ) {
        let display_name = reasoning.map_or_else(
            || display_name.to_owned(),
            |reasoning| format!("{display_name} ({})", reasoning.name),
        );
        let provider_name = provider.display_name();
        let agent_id = provider.agent_id.trim();
        let agent_id = if agent_id.is_empty() {
            "codex"
        } else {
            agent_id
        };
        let id = acp_provider_selection_identity(&provider.id, agent_id, &selection);
        let llm_id = LLMId::from(id.as_str());
        self.acp_selections.insert(
            llm_id.clone(),
            AcpModelSelection {
                provider_id: provider.id.clone(),
                agent_id: agent_id.to_owned(),
                launch_fingerprint: acp_launch_fingerprint(
                    agent_id,
                    &provider.command,
                    &provider.args,
                ),
                config_values: selection,
            },
        );
        let info = LLMInfo {
            id: llm_id,
            display_name,
            base_model_name: base_model_name.to_owned(),
            reasoning_level: reasoning.map(|reasoning| reasoning.name.clone()),
            reasoning_model_id: None,
            usage_metadata: LLMUsageMetadata {
                request_multiplier: 1,
                credit_multiplier: None,
            },
            description: Some(provider_name),
            disable_reason: None,
            vision_supported: false,
            spec: None,
            provider: LLMProvider::Acp,
            host_configs: HashMap::new(),
            discount_percentage: None,
            context_window: LLMContextWindow::default(),
        };
        self.models_by_feature.agent_mode.choices.push(info.clone());
        self.models_by_feature.coding.choices.push(info.clone());
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            cli.choices.push(info);
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn acp_runtime_selection_for_model(
        &self,
        model_id: &LLMId,
    ) -> Option<&AcpModelSelection> {
        self.acp_selections.get(model_id)
    }

    /// Resolves the runtime that owns the active model for a terminal surface.
    ///
    /// ACP is an execution backend, not a global lock on Agent Mode. Selecting
    /// an ACP-advertised model routes the conversation to that ACP agent, while
    /// selecting a Rig/provider model routes it through Galaxy's provider path.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn agent_backend_for_active_model(
        &self,
        terminal_view_id: Option<EntityId>,
        ctx: &AppContext,
    ) -> AgentBackend {
        if !cfg!(unix) || !FeatureFlag::AgentClientProtocol.is_enabled() {
            return AgentBackend::Provider;
        }

        let settings = AISettings::as_ref(ctx);
        if !*settings.acp_enabled.value() {
            return AgentBackend::Provider;
        }

        let active_model = self.get_active_base_model(ctx, terminal_view_id);
        if let Some(selection) = self.acp_runtime_selection_for_model(&active_model.id) {
            return AgentBackend::Acp(AcpConversationData {
                provider_id: selection.provider_id.clone(),
                agent_id: selection.agent_id.clone(),
                launch_fingerprint: selection.launch_fingerprint.clone(),
                session_id: None,
                config_values: selection.config_values.clone(),
            });
        }

        if active_model.id.as_str() != "none" {
            return AgentBackend::Provider;
        }

        // ACP remains a valid runtime even before discovery has returned a
        // model option (and for agents that do not expose model selection at
        // all). A discovered model catalog with no enabled entries must not
        // fall back to its disabled current model, though.
        let providers = settings.enabled_acp_providers();
        let [provider] = providers.as_slice() else {
            return AgentBackend::Provider;
        };
        if provider
            .config_options
            .iter()
            .any(|option| option.category.as_deref() == Some("model"))
        {
            return AgentBackend::Provider;
        }
        let agent_id = provider.agent_id.trim();
        let agent_id = if agent_id.is_empty() {
            "codex"
        } else {
            agent_id
        };

        AgentBackend::Acp(AcpConversationData {
            provider_id: provider.id.clone(),
            agent_id: agent_id.to_owned(),
            launch_fingerprint: acp_launch_fingerprint(agent_id, &provider.command, &provider.args),
            session_id: None,
            config_values: crate::ai::acp::AcpRuntimeModel::current_config_values(
                &provider.config_options,
            ),
        })
    }

    /// Ensures the default model ID in each feature's choices still points to
    /// an existing entry. If the default was removed (e.g. provider disabled),
    /// switch to the first remaining choice.
    #[cfg(not(target_family = "wasm"))]
    fn ensure_default_model_present(&mut self) {
        fn fix_default(feature: &mut AvailableLLMs) {
            if feature.choices.is_empty() {
                return;
            }
            let default_exists = feature.choices.iter().any(|m| m.id == feature.default_id);
            if !default_exists {
                let new_default = feature.choices[0].id.clone();
                log::info!(
                    "[llm] Default model {:?} no longer available, switching to {:?}",
                    feature.default_id,
                    new_default
                );
                feature.default_id = new_default;
            }
        }
        fix_default(&mut self.models_by_feature.agent_mode);
        fix_default(&mut self.models_by_feature.coding);
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            fix_default(cli);
        }
    }

    /// Returns the OpenAI client config for a given model ID, if it was injected
    /// from an OpenAI-compatible provider.
    #[cfg(not(target_family = "wasm"))]
    pub fn openai_client_config_for_model(
        &self,
        model_id: &str,
    ) -> Option<&super::openai::client::OpenAIClientConfig> {
        self.openai_provider_routing.get(model_id)
    }

    /// Fetches available models from the configured OpenAI-compatible endpoint.
    ///
    /// Tries LiteLLM's `/model/info` first (which returns rich metadata including
    /// accurate `max_input_tokens`, `max_output_tokens`, `supports_vision`, and
    /// `supports_function_calling`). Falls back to the standard OpenAI `/models`
    /// endpoint if `/model/info` is unavailable.
    #[cfg(not(target_family = "wasm"))]
    pub fn fetch_openai_models_from_endpoint(&mut self, ctx: &mut ModelContext<Self>) {
        let settings = AISettings::as_ref(ctx);
        if !settings.is_openai_provider_enabled() || !settings.openai_providers.value().is_empty() {
            return;
        }

        let base_url = settings.openai_base_url.value().clone();
        if base_url.is_empty() {
            return;
        }

        let api_key = {
            let key = settings.openai_api_key.value().clone();
            if key.is_empty() {
                None
            } else {
                Some(key)
            }
        };

        let generation = self.catalog_refresh_generations.entry(None).or_default();
        *generation += 1;
        let generation = *generation;
        let requested_base_url = base_url.clone();
        let requested_api_key = settings.openai_api_key.value().clone();
        let _ = ctx.spawn(
            async move {
                let base = base_url.trim_end_matches('/');
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default();

                fetch_litellm_catalog(base, api_key.as_deref(), &client).await
            },
            move |me, result, ctx| {
                if me.catalog_refresh_generations.get(&None) != Some(&generation) {
                    return;
                }
                let settings = AISettings::as_ref(ctx);
                if settings.openai_base_url.value() != &requested_base_url
                    || settings.openai_api_key.value() != &requested_api_key
                    || !settings.openai_providers.value().is_empty()
                {
                    return;
                }
                match result {
                    Ok(models) => {
                        let models =
                            replace_litellm_catalog(settings.openai_models.value(), models);
                        me.fetched_openai_models = models.clone();
                        AISettings::handle(ctx).update(ctx, |settings, ctx| {
                            if let Err(err) = settings.openai_models.set_value(models, ctx) {
                                report_error!(
                                    err.context("Failed to persist fetched OpenAI models")
                                );
                            }
                        });
                    }
                    Err(error) => Self::report_catalog_refresh_error(error, ctx),
                }
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn report_catalog_refresh_error(error: String, ctx: &mut ModelContext<Self>) {
        log::warn!("Model refresh failed; keeping cached catalog: {error}");
        if let Some(window_id) = WindowManager::as_ref(ctx).active_window() {
            ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                stack.add_ephemeral_toast(
                    DismissibleToast::error(format!(
                        "Model refresh failed. Keeping saved models. {error}"
                    )),
                    window_id,
                    ctx,
                );
            });
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn refresh_changed_catalog_connections(&mut self, ctx: &mut ModelContext<Self>) {
        let settings = AISettings::as_ref(ctx);
        if !settings.is_openai_provider_enabled() {
            self.catalog_connections.clear();
            return;
        }
        let connections: Vec<_> = settings
            .openai_providers
            .value()
            .iter()
            .cloned()
            .map(|mut provider| {
                provider.models.clear();
                provider
            })
            .collect();
        let changed: Vec<_> = connections
            .iter()
            .enumerate()
            .filter_map(|(index, provider)| {
                (provider.enabled
                    && matches!(provider.kind, OpenAIProviderKind::LiteLLM | OpenAIProviderKind::Anthropic)
                    && self.catalog_connections.get(index) != Some(provider))
                .then_some(index)
            })
            .collect();
        self.catalog_connections = connections;
        for index in changed {
            self.fetch_openai_provider_models(index, ctx);
        }
    }

    /// Explicitly refreshes the models for one entry in the OpenAI-compatible
    /// provider registry. LiteLLM and Anthropic also refresh on startup and connector changes.
    #[cfg(not(target_family = "wasm"))]
    pub fn fetch_openai_provider_models(
        &mut self,
        provider_index: usize,
        ctx: &mut ModelContext<Self>,
    ) {
        let settings = AISettings::as_ref(ctx);
        if !settings.is_openai_provider_enabled() {
            return;
        }

        let Some(provider) = settings
            .openai_providers
            .value()
            .get(provider_index)
            .cloned()
        else {
            return;
        };
        if provider.kind != OpenAIProviderKind::ChatGPTSubscription
            && provider.base_url.trim().is_empty()
        {
            return;
        }

        let generation = self
            .catalog_refresh_generations
            .entry(Some(provider_index))
            .or_default();
        *generation += 1;
        let generation = *generation;
        let requested_provider = provider.clone();
        let _ = ctx.spawn(
            async move { Self::discover_openai_provider_models(provider).await },
            move |me, result, ctx| {
                if me.catalog_refresh_generations.get(&Some(provider_index)) != Some(&generation) {
                    return;
                }
                let discovered_models = match result {
                    Ok(models) => models,
                    Err(error) => {
                        Self::report_catalog_refresh_error(error, ctx);
                        return;
                    }
                };
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut providers = settings.openai_providers.value().clone();
                    let Some(provider) = providers.get_mut(provider_index) else {
                        return;
                    };
                    let mut current_connection = provider.clone();
                    current_connection.models.clear();
                    let mut requested_connection = requested_provider.clone();
                    requested_connection.models.clear();
                    if current_connection != requested_connection {
                        return;
                    }
                    provider.models = match provider.kind {
                        OpenAIProviderKind::ChatGPTSubscription => {
                            merge_discovered_chatgpt_subscription_models(
                                &provider.models,
                                discovered_models,
                            )
                        }
                        OpenAIProviderKind::LiteLLM => {
                            replace_litellm_catalog(&provider.models, discovered_models)
                        }
                        OpenAIProviderKind::OpenAI
                        | OpenAIProviderKind::Anthropic
                        | OpenAIProviderKind::Gemini
                        | OpenAIProviderKind::VertexAI => {
                            merge_discovered_provider_models(&provider.models, discovered_models)
                        }
                    };
                    if let Err(err) = settings.openai_providers.set_value(providers, ctx) {
                        report_error!(err.context("Failed to persist discovered provider models"));
                    }
                });
            },
        );
    }

    /// Discovers models for a provider draft without persisting or injecting it.
    ///
    /// The provider setup view uses this to keep configuration changes atomic
    /// until the user clicks Save.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) async fn discover_openai_provider_models(
        provider: OpenAIProviderConfig,
    ) -> Result<Vec<OpenAIModelConfig>, String> {
        if provider.kind == OpenAIProviderKind::ChatGPTSubscription {
            return Self::discover_chatgpt_subscription_models().await;
        }

        let native_models = match provider.kind {
            OpenAIProviderKind::Anthropic => {
                let api_key = provider
                    .api_key
                    .as_deref()
                    .filter(|key| !key.trim().is_empty())
                    .ok_or_else(|| {
                        "Enter an Anthropic API key or Claude subscription token before testing the connection.".to_string()
                    })?;
                let api_key = crate::ai::claude_oauth::load_credential(api_key.to_string()).await?;
                Some(discover_anthropic_models(&api_key).await?)
            }
            OpenAIProviderKind::Gemini => {
                let api_key = provider
                    .api_key
                    .as_deref()
                    .filter(|key| !key.trim().is_empty())
                    .ok_or_else(|| {
                        "Enter a Gemini API key before testing the connection.".to_string()
                    })?;
                Some(discover_gemini_models(api_key).await?)
            }
            OpenAIProviderKind::VertexAI => {
                if provider
                    .project_id
                    .as_deref()
                    .is_none_or(|project| project.trim().is_empty())
                {
                    return Err(
                        "Enter a Google Cloud project ID before testing the connection."
                            .to_string(),
                    );
                }
                validate_vertex_ai_credentials(
                    provider.project_id.as_deref().unwrap_or_default(),
                    provider.location.as_deref().unwrap_or("global"),
                )?;
                Some(vertex_ai_model_catalog())
            }
            OpenAIProviderKind::OpenAI | OpenAIProviderKind::LiteLLM => None,
            OpenAIProviderKind::ChatGPTSubscription => {
                unreachable!("ChatGPT subscription discovery is handled before native discovery")
            }
        };

        if let Some(models) = native_models {
            if models.is_empty() {
                return Err("The provider responded, but no models were found.".to_string());
            }
            return Ok(Self::rig_models_to_openai_models(models));
        }

        if provider.base_url.trim().is_empty() {
            return Err("Enter a provider URL before testing the connection.".to_string());
        }

        let base_url = provider.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| format!("Could not create the provider client: {error}"))?;
        let api_key = provider.api_key.as_deref().filter(|key| !key.is_empty());

        if provider.kind == OpenAIProviderKind::LiteLLM {
            fetch_litellm_catalog(&base_url, api_key, &client).await
        } else {
            fetch_from_openai_models(&base_url, api_key, &client).await
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn schedule_chatgpt_subscription_model_refresh(&self, ctx: &mut ModelContext<Self>) {
        let _ = ctx.spawn(
            async move {
                Timer::after(CHATGPT_SUBSCRIPTION_MODELS_REFRESH_INTERVAL).await;
            },
            |me, _, ctx| {
                me.refresh_chatgpt_subscription_models(ctx);
                me.schedule_chatgpt_subscription_model_refresh(ctx);
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn refresh_chatgpt_subscription_models(&mut self, ctx: &mut ModelContext<Self>) {
        if self.chatgpt_subscription_models_refresh_in_flight {
            return;
        }

        let settings = AISettings::as_ref(ctx);
        if !settings.is_openai_provider_enabled()
            || !settings.openai_providers.value().iter().any(|provider| {
                provider.enabled && provider.kind == OpenAIProviderKind::ChatGPTSubscription
            })
        {
            return;
        }

        self.chatgpt_subscription_models_refresh_in_flight = true;
        let _ = ctx.spawn(
            async { Self::discover_chatgpt_subscription_models().await },
            |me, result, ctx| {
                me.chatgpt_subscription_models_refresh_in_flight = false;
                let discovered_models = match result {
                    Ok(models) => models,
                    Err(error) => {
                        log::warn!(
                            "[chatgpt/models] Failed to refresh ChatGPT subscription models: {error}"
                        );
                        return;
                    }
                };

                if discovered_models.is_empty() {
                    return;
                }

                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut providers = settings.openai_providers.value().clone();
                    let mut changed = false;
                    for provider in &mut providers {
                        if provider.kind != OpenAIProviderKind::ChatGPTSubscription {
                            continue;
                        }
                        provider.models = merge_discovered_chatgpt_subscription_models(
                            &provider.models,
                            discovered_models.clone(),
                        );
                        changed = true;
                    }
                    if changed {
                        if let Err(err) = settings.openai_providers.set_value(providers, ctx) {
                            report_error!(
                                err.context("Failed to persist ChatGPT subscription models")
                            );
                        }
                    }
                });

                me.inject_openai_models(ctx);
                me.ensure_default_model_present();
                ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    async fn discover_chatgpt_subscription_models() -> Result<Vec<OpenAIModelConfig>, String> {
        let credentials = crate::ai::chatgpt_auth::load_or_refresh_auth_credentials().await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("Could not create the ChatGPT model client: {error}"))?;
        let client_version = fetch_latest_codex_client_version(&client).await?;
        fetch_from_chatgpt_codex_models(&client_version, credentials, &client).await
    }

    #[cfg(not(target_family = "wasm"))]
    fn rig_models_to_openai_models(models: Vec<RigModelInfo>) -> Vec<OpenAIModelConfig> {
        models
            .into_iter()
            .map(|model| OpenAIModelConfig {
                model_id: model.id,
                display_name: model.display_name,
                vision_supported: true,
                context_size: model.context_size.unwrap_or(128_000),
                max_input_tokens: model.context_size,
                default_context_size: None,
                max_output_tokens: model.max_output_tokens,
                provider: None,
                supports_system_messages: Some(true),
                capability_overrides: std::collections::HashMap::new(),
                reasoning_efforts: model.reasoning_efforts,
                enabled: true,
            })
            .collect()
    }

    /// Returns the `LLMInfo` for the base LLM to be used for an Agent Mode request.
    pub fn get_active_base_model<'a>(
        &'a self,
        app: &'a AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &'a LLMInfo {
        self.get_preferred_base_model(app, terminal_view_id)
    }

    /// Returns `LLMInfo` for the currently selected LLM to be used for Agent Mode.
    fn get_preferred_base_model(
        &self,
        app: &AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &LLMInfo {
        if let Some(terminal_view_id) = terminal_view_id {
            let raw_override = self.base_llm_for_terminal_view.get(&terminal_view_id);
            if let Some(llm_id) = raw_override {
                if let Some(llm_info) = Self::server_info_for_id_router_gated(
                    &self.models_by_feature.agent_mode,
                    llm_id,
                )
                .or_else(|| self.custom_llm_info_for_id_if_enabled(llm_id, app))
                .or_else(|| self.custom_router_llm_info_for_id_if_enabled(llm_id))
                {
                    return llm_info;
                }
            }
        }

        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);

        profile
            .data()
            .base_model
            .clone()
            .and_then(|id| {
                Self::server_info_for_id_router_gated(&self.models_by_feature.agent_mode, &id)
                    .or_else(|| self.custom_llm_info_for_id_if_enabled(&id, app))
                    .or_else(|| self.custom_router_llm_info_for_id_if_enabled(&id))
            })
            .unwrap_or_else(|| self.models_by_feature.agent_mode.default_llm_info())
    }

    pub fn get_active_coding_model<'a>(
        &'a self,
        app: &'a AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &'a LLMInfo {
        self.get_preferred_coding_model(app, terminal_view_id)
    }

    /// Returns `LLMInfo` for user's preferred coding model.
    fn get_preferred_coding_model(
        &self,
        app: &AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &LLMInfo {
        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);

        profile
            .data()
            .coding_model
            .clone()
            .and_then(|id| {
                Self::server_info_for_id_router_gated(&self.models_by_feature.coding, &id)
                    .or_else(|| self.custom_llm_info_for_id_if_enabled(&id, app))
                    .or_else(|| self.custom_router_llm_info_for_id_if_enabled(&id))
            })
            .unwrap_or_else(|| self.models_by_feature.coding.default_llm_info())
    }

    /// Resolves `id` against a server-provided model list, but hides cloud/team
    /// custom routers when the custom-router feature flag is off. Mirrors the
    /// gating applied to local routers (see
    /// [`Self::custom_router_llm_info_for_id_if_enabled`]) so the whole
    /// custom-router feature is controlled by a single client flag.
    fn server_info_for_id_router_gated<'a>(
        available: &'a AvailableLLMs,
        id: &LLMId,
    ) -> Option<&'a LLMInfo> {
        let info = available.info_for_id(id)?;
        if !FeatureFlag::CustomModelRouters.is_enabled()
            && custom_model_routers::is_cloud_custom_router_id(info.id.as_str())
        {
            return None;
        }
        Some(info)
    }

    /// Returns the set of LLMs available for Agent Mode use.
    pub fn get_base_llm_choices_for_agent_mode(
        &self,
        app: &AppContext,
    ) -> impl Iterator<Item = &LLMInfo> {
        // Don't show admin-disabled models in the dropdown
        let routers_enabled = FeatureFlag::CustomModelRouters.is_enabled();
        self.models_by_feature
            .agent_mode
            .choices
            .iter()
            .filter(|llm| !matches!(llm.disable_reason, Some(DisableReason::AdminDisabled)))
            // Gate cloud/team routers behind the same flag as local routers so
            // the entire custom-router feature is controlled by one flag.
            .filter(move |llm| {
                routers_enabled || !custom_model_routers::is_cloud_custom_router_id(llm.id.as_str())
            })
            .chain(self.custom_llm_choices(app))
            .chain(self.custom_router_choices())
    }

    pub fn orchestration_provider_name<'a>(&self, model: &'a LLMInfo) -> &'a str {
        model.description.as_deref().unwrap_or_else(|| model.provider.display_name())
    }

    pub fn profile_allows_orchestration_model(&self, model: &LLMInfo, terminal: Option<EntityId>, app: &AppContext) -> bool {
        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal, app);
        // Routers can choose another provider, so they cannot bypass a restricted profile.
        if profile.data().orchestration_providers.is_some() && custom_model_routers::is_custom_router_id(model.id.as_str()) {
            return false;
        }
        profile.data().includes_orchestration_provider(self.orchestration_provider_name(model))
    }

    /// Returns the set of LLMs available for coding.
    pub fn get_coding_llm_choices(&self, app: &AppContext) -> impl Iterator<Item = &LLMInfo> {
        // Don't show admin-disabled models in the dropdown
        let routers_enabled = FeatureFlag::CustomModelRouters.is_enabled();
        self.models_by_feature
            .coding
            .choices
            .iter()
            .filter(|llm| !matches!(llm.disable_reason, Some(DisableReason::AdminDisabled)))
            // Gate cloud/team routers behind the same flag as local routers.
            .filter(move |llm| {
                routers_enabled || !custom_model_routers::is_cloud_custom_router_id(llm.id.as_str())
            })
            .chain(self.custom_llm_choices(app))
            .chain(self.custom_router_choices())
    }

    /// Returns the set of LLMs available for CLI agent.
    pub fn get_cli_agent_llm_choices(&self, app: &AppContext) -> impl Iterator<Item = &LLMInfo> {
        self.get_cli_agent_available()
            .choices
            .iter()
            .chain(self.custom_llm_choices(app))
    }

    /// Returns the `LLMInfo` for the CLI agent model.
    pub fn get_active_cli_agent_model<'a>(
        &'a self,
        app: &'a AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &'a LLMInfo {
        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);

        let available = self.get_cli_agent_available();
        profile
            .data()
            .cli_agent_model
            .clone()
            .and_then(|id| {
                available
                    .info_for_id(&id)
                    .or_else(|| self.custom_llm_info_for_id_if_enabled(&id, app))
            })
            .unwrap_or_else(|| available.default_llm_info())
    }

    /// Returns the default CLI agent model as a fallback.
    pub fn get_default_cli_agent_model(&self) -> &LLMInfo {
        self.get_cli_agent_available().default_llm_info()
    }

    /// Helper to get the AvailableLLMs for cli_agent, falling back to agent_mode.
    fn get_cli_agent_available(&self) -> &AvailableLLMs {
        self.models_by_feature
            .cli_agent
            .as_ref()
            .unwrap_or(&self.models_by_feature.agent_mode)
    }

    /// Returns computer-use models plus configured models that can inspect screenshots.
    pub fn get_computer_use_llm_choices(&self, app: &AppContext) -> impl Iterator<Item = &LLMInfo> {
        let available = self.get_computer_use_available();
        available.choices.iter().chain(
            self.get_base_llm_choices_for_agent_mode(app)
                .filter(move |info| {
                    info.vision_supported && available.info_for_id(&info.id).is_none()
                }),
        )
    }

    /// Returns the `LLMInfo` for the computer use agent model.
    pub fn get_active_computer_use_model<'a>(
        &'a self,
        app: &'a AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &'a LLMInfo {
        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);

        profile
            .data()
            .computer_use_model
            .clone()
            .and_then(|id| self.computer_use_llm_info_for_id(&id, app))
            .unwrap_or_else(|| self.get_default_computer_use_model())
    }

    /// Resolve migrated selection IDs before checking computer-use eligibility.
    /// Looking up the canonical ID in these choices preserves the vision filter
    /// and any computer-use-specific metadata, including disable reasons.
    fn computer_use_llm_info_for_id<'a>(
        &'a self,
        id: &LLMId,
        app: &'a AppContext,
    ) -> Option<&'a LLMInfo> {
        let canonical_id = self.get_llm_info(id).map_or(id, |info| &info.id);
        self.get_computer_use_llm_choices(app)
            .find(|info| &info.id == canonical_id)
    }

    /// Returns the default computer use model as a fallback.
    pub fn get_default_computer_use_model(&self) -> &LLMInfo {
        self.get_computer_use_available().default_llm_info()
    }

    /// Helper to get the AvailableLLMs for computer_use.
    /// Falls back to a computer-use-specific default if None.
    fn get_computer_use_available(&self) -> &AvailableLLMs {
        static DEFAULT: OnceLock<AvailableLLMs> = OnceLock::new();
        self.models_by_feature
            .computer_use
            .as_ref()
            .unwrap_or_else(|| DEFAULT.get_or_init(default_computer_use_llms))
    }

    /// Returns metadata about an LLM, if the client knows about it.
    /// Falls back to the user's custom-endpoint LLMs when the id isn't a server-known model
    /// id (e.g. when it's a `config_key` UUID).
    pub fn get_llm_info(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.models_by_feature
            .info_for_id(id)
            .or_else(|| self.custom_llm_info_for_id(id))
            .or_else(|| self.custom_router_llm_info_for_id(id))
    }

    /// Resolves an `LLMId` against the user's custom-endpoint LLMs.
    /// Returns `None` if the id isn't a known custom model `config_key`.
    pub fn custom_llm_info_for_id(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.custom_llms.iter().find(|info| info.id == *id)
    }

    /// Footer label for custom endpoint usage keyed by the request config_key.
    /// The synthetic custom LLMInfo already owns alias-or-name display semantics.
    pub fn custom_endpoint_usage_display_label(&self, config_key: &str) -> String {
        let config_key = LLMId::from(config_key);
        self.custom_llm_info_for_id(&config_key)
            .map(|info| info.display_name.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| CUSTOM_ENDPOINT_USAGE_FALLBACK_LABEL.to_string())
    }

    fn custom_llm_info_for_id_if_enabled(&self, id: &LLMId, app: &AppContext) -> Option<&LLMInfo> {
        Self::custom_inference_enabled(app)
            .then(|| self.custom_llm_info_for_id(id))
            .flatten()
    }

    /// Iterator over the user's custom-endpoint LLMs, gated on the feature flag and entitlement.
    pub fn custom_llm_choices(&self, app: &AppContext) -> std::slice::Iter<'_, LLMInfo> {
        if Self::custom_inference_enabled(app) {
            self.custom_llms.iter()
        } else {
            // Empty slice with a matching element type so the return type stays consistent
            // across both branches.
            (&[] as &[LLMInfo]).iter()
        }
    }

    fn custom_inference_enabled(app: &AppContext) -> bool {
        let _ = app;
        false
    }

    /// Resolves a custom model router by its `config_key`/`LLMId`.
    pub fn custom_model_router_for_id(&self, id: &LLMId) -> Option<&CustomModelRouter> {
        self.custom_model_routers.iter().find(|m| m.llm_id() == *id)
    }

    fn custom_router_llm_info_for_id(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.custom_model_routers
            .iter()
            .find(|m| m.info.id == *id)
            .map(|m| &m.info)
    }

    fn custom_router_llm_info_for_id_if_enabled(&self, id: &LLMId) -> Option<&LLMInfo> {
        FeatureFlag::CustomModelRouters
            .is_enabled()
            .then(|| self.custom_router_llm_info_for_id(id))
            .flatten()
    }

    /// Iterator over the custom router picker entries, gated on the feature flag.
    /// Mirrors [`Self::custom_llm_choices`].
    pub fn custom_router_choices(&self) -> impl Iterator<Item = &LLMInfo> {
        let enabled = FeatureFlag::CustomModelRouters.is_enabled();
        self.custom_model_routers
            .iter()
            .filter(move |_| enabled)
            .map(|m| &m.info)
    }

    /// Builds the custom_model_routers registry for an outbound request.
    pub fn custom_model_routers_for_request(
        &self,
        base_id: &LLMId,
        coding_id: &LLMId,
    ) -> api::request::settings::CustomModelRouters {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for id in [base_id, coding_id] {
            if let Some(entry) = self.custom_router_proto_entry(id) {
                if seen.insert(entry.config_key.clone()) {
                    models.push(entry);
                }
            }
        }
        api::request::settings::CustomModelRouters { routers: models }
    }

    /// Returns the proto registry entry for a local custom-router id, or `None`
    /// if `id` is not a known local router.
    fn custom_router_proto_entry(
        &self,
        id: &LLMId,
    ) -> Option<api::request::settings::custom_model_routers::CustomModelRouter> {
        self.custom_model_router_for_id(id).map(|m| m.to_proto())
    }

    /// Rebuilds `custom_model_routers` from the `model_configs/` directory,
    /// then notifies subscribers.
    ///
    /// Routers whose targets include an unknown model are excluded and a
    /// warning is logged. The check uses the currently loaded model list
    /// (server-fetched + cached), so it is best-effort at startup before
    /// the server responds.
    fn rebuild_custom_model_routers(&mut self, ctx: &mut ModelContext<Self>) {
        let local = WarpConfig::as_ref(ctx).custom_model_routers().clone();

        let mut deduped = Vec::with_capacity(local.len());
        let mut seen = HashSet::new();
        for model in local {
            if seen.insert(model.config_key()) {
                deduped.push(model);
            }
        }
        let mut validation_errors: Vec<ModelConfigError> = Vec::new();
        deduped.retain(|router| {
            let unknown: Vec<&str> = router
                .all_targets()
                .into_iter()
                .filter(|id| self.get_llm_info(&LLMId::from(*id)).is_none())
                .collect();
            if unknown.is_empty() {
                return true;
            }
            let error_message = format!("unknown target model(s): {}", unknown.join(", "));
            log::warn!(
                "Custom model router '{}': {} — excluding from picker",
                router.info.display_name,
                error_message,
            );
            validation_errors.push(ModelConfigError {
                file_name: router
                    .source_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or(router.info.display_name.as_str())
                    .to_owned(),
                file_path: router.source_path.clone().unwrap_or_default(),
                error_message,
            });
            false
        });
        if !validation_errors.is_empty() {
            WarpConfig::handle(ctx).update(ctx, |_, ctx| {
                ctx.emit(WarpConfigUpdateEvent::ModelConfigErrors(validation_errors));
            });
        }

        // vision is supported only when every concrete target model supports it.
        for router in &mut deduped {
            router.info.vision_supported = router.all_targets().iter().all(|id| {
                self.get_llm_info(&LLMId::from(*id))
                    .is_some_and(|info| info.vision_supported)
            });
        }

        self.custom_model_routers = deduped;
        ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
    }

    /// Resets any persisted *local* custom-router selection that no longer resolves
    /// to a loaded definition, so a deleted/invalid local config falls back to the
    /// default model and the visible selection updates. Scoped to local
    /// ids so a cloud selection isn't reset by a local reload.
    fn reconcile_stale_custom_router_selection(&mut self, ctx: &mut ModelContext<Self>) {
        let valid_local: HashSet<LLMId> = self
            .custom_model_routers
            .iter()
            .map(|m| m.llm_id())
            .collect();

        let mut updated_agent_mode = false;
        let mut updated_coding = false;

        self.base_llm_for_terminal_view.retain(|_, id| {
            let stale = custom_model_routers::is_local_custom_router_id(id.as_str())
                && !valid_local.contains(&*id);
            updated_agent_mode |= stale;
            !stale
        });

        AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
            for profile_id in profiles.get_all_profile_ids() {
                let Some(profile) = profiles.get_profile_by_id(profile_id, ctx) else {
                    continue;
                };
                let profile_data = profile.data();
                let base_stale = profile_data.base_model.as_ref().is_some_and(|id| {
                    custom_model_routers::is_local_custom_router_id(id.as_str())
                        && !valid_local.contains(id)
                });
                if base_stale {
                    profiles.set_base_model(profile_id, None, ctx);
                    profiles.set_context_window_limit(profile_id, None, ctx);
                    updated_agent_mode = true;
                }
                let coding_stale = profile_data.coding_model.as_ref().is_some_and(|id| {
                    custom_model_routers::is_local_custom_router_id(id.as_str())
                        && !valid_local.contains(id)
                });
                if coding_stale {
                    profiles.set_coding_model(profile_id, None, ctx);
                    updated_coding = true;
                }
            }
        });

        if updated_agent_mode {
            self.trigger_snapshot_save(ctx);
            ctx.emit(LLMPreferencesEvent::UpdatedActiveAgentModeLLM);
        }
        if updated_coding {
            ctx.emit(LLMPreferencesEvent::UpdatedActiveCodingLLM);
        }
    }

    /// Reads the user's current `ApiKeyManager.custom_endpoints` and replaces `custom_llms`
    /// with synthetic `LLMInfo`s. Called on every `ApiKeyManagerEvent::KeysUpdated`, so adds,
    /// edits, and removals all propagate immediately.
    fn sanitize_disabled_custom_model_preferences(&mut self, _ctx: &mut ModelContext<Self>) {}

    /// Returns the default base model as a fallback.
    /// Returns `true` if at least one real AI provider model is configured and available.
    /// When this returns `false`, agent mode should be disabled to avoid null references
    /// or attempts to call unconfigured providers.
    pub fn has_any_provider_models(&self) -> bool {
        self.models_by_feature
            .agent_mode
            .choices
            .iter()
            .any(|m| m.provider != LLMProvider::Unknown)
    }

    pub fn get_default_base_model(&self) -> &LLMInfo {
        self.models_by_feature.agent_mode.default_llm_info()
    }

    /// Returns the default coding model as a fallback.
    pub fn get_default_coding_model(&self) -> &LLMInfo {
        self.models_by_feature.coding.default_llm_info()
    }

    /// Returns the preferred Codex model, if set by the server.
    pub fn get_preferred_codex_model(&self) -> Option<&LLMInfo> {
        self.models_by_feature
            .agent_mode
            .preferred_codex_model_id
            .as_ref()
            .and_then(|id| self.models_by_feature.agent_mode.info_for_id(id))
    }

    #[cfg(feature = "integration_tests")]
    pub fn is_available_agent_mode_llm(&self, id: &LLMId) -> bool {
        self.models_by_feature.agent_mode.info_for_id(id).is_some()
    }

    /// Creates a pane-level override for the Agent Mode LLM.
    pub fn update_preferred_agent_mode_llm(
        &mut self,
        preferred_llm_id: &LLMId,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        let profile =
            AIExecutionProfilesModel::as_ref(ctx).active_profile(Some(terminal_view_id), ctx);

        let profile_default_model_id = profile
            .data()
            .base_model
            .as_ref()
            .and_then(|id| self.models_by_feature.agent_mode.info_for_id(id))
            .unwrap_or_else(|| self.models_by_feature.agent_mode.default_llm_info())
            .id
            .clone();

        // Only remove override if we're setting to the profile's default.
        // Otherwise, always set the override explicitly.
        let changed = if preferred_llm_id == &profile_default_model_id {
            self.base_llm_for_terminal_view
                .remove(&terminal_view_id)
                .is_some()
        } else {
            self.base_llm_for_terminal_view
                .insert(terminal_view_id, preferred_llm_id.clone());
            true
        };

        if changed {
            self.trigger_snapshot_save(ctx);
            ctx.emit(LLMPreferencesEvent::UpdatedActiveAgentModeLLM);
        }
    }

    /// Triggers a snapshot save to persist LLM override changes.
    fn trigger_snapshot_save(&self, ctx: &mut ModelContext<Self>) {
        ctx.dispatch_global_action("workspace:save_app", ());
    }

    pub fn update_preferred_coding_llm(
        &self,
        preferred_llm_id: &LLMId,
        terminal_view_id: Option<EntityId>,
        ctx: &mut ModelContext<Self>,
    ) {
        let new_value = if preferred_llm_id == &self.models_by_feature.coding.default_id {
            None
        } else {
            Some(preferred_llm_id.clone())
        };

        let mut changed = false;
        AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
            let profile = profiles.active_profile(terminal_view_id, ctx);

            if profile.data().coding_model != new_value {
                profiles.set_coding_model(*profile.id(), new_value, ctx);
                changed = true;
            }
        });

        if changed {
            ctx.emit(LLMPreferencesEvent::UpdatedActiveCodingLLM);
        }
    }

    pub fn new_choices_since_last_update(&self) -> Option<Vec<LLMInfo>> {
        self.last_update.as_ref().map(|update| {
            // We don't want to display new choices if they are warp branded.
            let filter_choices: Vec<LLMInfo> = update
                .new_choices
                .clone()
                .into_iter()
                .filter(|choice| !choice.display_name.starts_with("lite"))
                .collect();

            filter_choices
        })
    }

    pub fn should_show_new_choices_popup(&self, view_id: EntityId) -> bool {
        self.last_update.as_ref().is_some_and(|update| {
            let popup_state = &*update.popup_visibility_state.lock();
            matches!(popup_state, UpdatePopupVisibilityState::WaitingToBeShown)
                || matches!(
                popup_state,
                UpdatePopupVisibilityState::Visible(id) if *id == view_id)
        })
    }

    pub fn mark_new_choices_popup_as_shown(&self, view_id: EntityId) {
        if let Some(update) = self.last_update.as_ref() {
            if matches!(
                &*update.popup_visibility_state.lock(),
                UpdatePopupVisibilityState::WaitingToBeShown
            ) {
                *update.popup_visibility_state.lock() =
                    UpdatePopupVisibilityState::Visible(view_id);
            }
        }
    }

    pub fn hide_llm_popup(&self, view_id: EntityId) {
        if !self.should_show_new_choices_popup(view_id) {
            return;
        }
        let Some(last_update) = self.last_update.as_ref() else {
            return;
        };
        *last_update.popup_visibility_state.lock() = UpdatePopupVisibilityState::Hidden;
    }

    /// Fetches the latest set of models from the server for the currently logged in user, and updates the model.
    ///
    /// NOTE: Disabled — Galaxy uses only locally configured providers (Bedrock/LiteLLM).
    /// No models are fetched from Warp's cloud API.
    pub fn refresh_authed_models(&self, _ctx: &mut ModelContext<Self>) {
        log::debug!("[llm] Server model fetch disabled — using local providers only");
    }

    /// No auth required (i.e. to populate the pre-login onboarding picker).
    ///
    /// NOTE: Disabled — Galaxy uses only locally configured providers (Bedrock/LiteLLM).
    /// No models are fetched from Warp's cloud API.
    fn refresh_public_models(&self, _ctx: &mut ModelContext<Self>) {
        log::debug!("[llm] Server model fetch disabled — using local providers only");
    }

    /// NOTE: Disabled — Galaxy uses only locally configured providers (Bedrock/LiteLLM).
    pub fn refresh_available_models(&self, _ctx: &mut ModelContext<Self>) {
        log::debug!("[llm] Server model fetch disabled — using local providers only");
    }

    /// Disabled — Galaxy does not accept model updates from Warp's server.
    pub fn update_feature_model_choices(
        &mut self,
        _choices_result: Result<ModelsByFeature, anyhow::Error>,
        _ctx: &mut ModelContext<Self>,
    ) {
        log::debug!("[llm] Server model update ignored — using local providers only");
    }

    #[cfg(test)]
    pub(crate) fn set_models_by_feature_for_test(
        &mut self,
        models_by_feature: ModelsByFeature,
        ctx: &mut ModelContext<Self>,
    ) {
        self.models_by_feature = models_by_feature;
        ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
    }

    /// Disabled — Galaxy does not accept model updates from Warp's server.
    fn on_server_update(&mut self, _update: ModelsByFeature, _ctx: &mut ModelContext<Self>) {
        log::debug!("[llm] Server model update ignored — using local providers only");
    }

    /// Clear any model selections where the model is no longer supported
    /// or effectively disabled, and clear orphaned context window limits
    /// for non-configurable or unusable models.
    ///
    /// Called both when the model list is refreshed from the server and when
    /// BYOK API keys change (since `RequiresUpgrade` usability is BYOK-aware).
    fn reconcile_disabled_model_preferences(&self, ctx: &mut ModelContext<Self>) {
        let profiles_model = AIExecutionProfilesModel::handle(ctx);
        profiles_model.update(ctx, |profiles, ctx| {
            for profile_id in profiles.get_all_profile_ids() {
                if let Some(profile) = profiles.get_profile_by_id(profile_id, ctx) {
                    let profile_data = profile.data();
                    let preferred_base_model = profile_data.base_model.clone();
                    let effective_base_model_id = preferred_base_model
                        .as_ref()
                        .unwrap_or(&self.models_by_feature.agent_mode.default_id);
                    let effective_base_model_usable = self
                        .models_by_feature
                        .agent_mode
                        .usable_info_for_id(effective_base_model_id, ctx)
                        .or_else(|| {
                            self.custom_llm_info_for_id_if_enabled(effective_base_model_id, ctx)
                        });
                    let effective_base_model_unusable = effective_base_model_usable.is_none();
                    let effective_base_model_is_configurable = effective_base_model_usable
                        .is_some_and(|info| info.context_window.is_configurable);
                    let has_context_window_limit = profile_data.context_window_limit.is_some();

                    if preferred_base_model.is_some() && effective_base_model_unusable {
                        profiles.set_base_model(profile_id, None, ctx);
                    }
                    if has_context_window_limit
                        && (effective_base_model_unusable || !effective_base_model_is_configurable)
                    {
                        profiles.set_context_window_limit(profile_id, None, ctx);
                    }
                    if let Some(preferred_llm_id) = &profile.data().coding_model {
                        if self
                            .models_by_feature
                            .coding
                            .usable_info_for_id(preferred_llm_id, ctx)
                            .or_else(|| {
                                self.custom_llm_info_for_id_if_enabled(preferred_llm_id, ctx)
                            })
                            .is_none()
                        {
                            profiles.set_coding_model(profile_id, None, ctx);
                        }
                    }
                    if let Some(preferred_llm_id) = &profile.data().cli_agent_model {
                        if self
                            .get_cli_agent_available()
                            .usable_info_for_id(preferred_llm_id, ctx)
                            .or_else(|| {
                                self.custom_llm_info_for_id_if_enabled(preferred_llm_id, ctx)
                            })
                            .is_none()
                        {
                            profiles.set_cli_agent_model(profile_id, None, ctx);
                        }
                    }
                    if let Some(preferred_llm_id) = &profile.data().computer_use_model {
                        if self
                            .computer_use_llm_info_for_id(preferred_llm_id, ctx)
                            .filter(|info| {
                                let has_byok_key =
                                    is_using_api_key_for_provider(&info.provider, ctx);
                                info.disable_reason.as_ref().is_none_or(|reason| {
                                    !reason.should_clear_preference(has_byok_key)
                                })
                            })
                            .is_none()
                        {
                            profiles.set_computer_use_model(profile_id, None, ctx);
                        }
                    }
                }
            }
        });
    }

    pub fn vision_supported(&self, app: &AppContext, terminal_view_id: Option<EntityId>) -> bool {
        self.get_active_base_model(app, terminal_view_id)
            .vision_supported
    }

    pub fn get_base_llm_override(&self, terminal_view_id: EntityId) -> Option<String> {
        if let Some(override_str) = self
            .base_llm_for_terminal_view
            .get(&terminal_view_id)
            .and_then(|llm_id| serde_json::to_string(llm_id).ok())
        {
            return Some(override_str);
        }

        log::debug!("LLM override not found in memory for terminal view: {terminal_view_id:?}");
        None
    }

    /// Removes the LLM override for a terminal view.
    /// This ensures that the new profile's default model is used.
    pub fn remove_llm_override(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        let old = self.base_llm_for_terminal_view.remove(&terminal_view_id);
        if old.is_some() {
            self.trigger_snapshot_save(ctx);
            ctx.emit(LLMPreferencesEvent::UpdatedActiveAgentModeLLM);
        }
    }
}

#[derive(Clone, Debug)]
pub enum LLMPreferencesEvent {
    UpdatedAvailableLLMs,
    UpdatedActiveAgentModeLLM,
    UpdatedActiveCodingLLM,
}

impl Entity for LLMPreferences {
    type Event = LLMPreferencesEvent;
}

impl SingletonEntity for LLMPreferences {}

#[cfg(not(target_family = "wasm"))]
fn acp_agent_display_name(agent_id: &str) -> String {
    match agent_id.to_ascii_lowercase().as_str() {
        "codex" => "Codex".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        _ => agent_id.to_owned(),
    }
}

#[cfg(not(target_family = "wasm"))]
fn acp_model_is_enabled(value: &serde_json::Value, bedrock_enabled: bool) -> bool {
    bedrock_enabled
        || !value
            .as_str()
            .is_some_and(|model_id| model_id.starts_with("amazon-bedrock/"))
}

fn get_new_agent_mode_choices(
    old_config: &AvailableLLMs,
    new_config: &AvailableLLMs,
) -> Vec<LLMInfo> {
    let old_ids: HashSet<_> = old_config.choices.iter().map(|info| &info.id).collect();
    new_config
        .choices
        .iter()
        .filter(|info| !old_ids.contains(&info.id))
        .cloned()
        .collect()
}

pub(crate) fn openai_model_context_size(
    model: &OpenAIModelConfig,
    provider_kind: OpenAIProviderKind,
) -> u32 {
    let input_limit = model
        .max_input_tokens
        .unwrap_or(model.context_size)
        .min(model.context_size);
    if matches!(
        provider_kind,
        OpenAIProviderKind::ChatGPTSubscription | OpenAIProviderKind::LiteLLM
    ) {
        // Use the efficient session window for both subscriptions and proxies.
        // An advertised maximum does not opt a connection into long context.
        // Older saved catalogs kept the default in max_input_tokens instead.
        return model
            .default_context_size
            .unwrap_or(input_limit)
            .min(input_limit);
    }
    input_limit
}

/// Merges endpoint metadata into a provider's configured models without
/// discarding local routing choices or manually configured models.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn merge_discovered_provider_models(
    existing_models: &[OpenAIModelConfig],
    discovered_models: Vec<OpenAIModelConfig>,
) -> Vec<OpenAIModelConfig> {
    let mut merged = Vec::with_capacity(discovered_models.len() + existing_models.len());
    let mut discovered_ids = HashSet::new();

    for mut discovered in discovered_models {
        if !discovered_ids.insert(discovered.model_id.clone()) {
            continue;
        }

        if let Some(existing) = existing_models
            .iter()
            .find(|model| model.model_id == discovered.model_id)
        {
            discovered.display_name = existing.display_name.clone();
            discovered.enabled = existing.enabled;
            if existing.supports_system_messages.is_some() {
                discovered.supports_system_messages = existing.supports_system_messages;
            }
            if discovered.provider.is_none() {
                discovered.provider = existing.provider.clone();
            }
        }
        if discovered.model_id.starts_with("codex-gpt-") {
            discovered.supports_system_messages = Some(false);
        }

        merged.push(discovered);
    }

    merged.extend(
        existing_models
            .iter()
            .filter(|model| !discovered_ids.contains(&model.model_id))
            .cloned(),
    );
    merged
}

/// A successful LiteLLM /models response owns catalog membership.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn replace_litellm_catalog(
    existing: &[OpenAIModelConfig],
    discovered: Vec<OpenAIModelConfig>,
) -> Vec<OpenAIModelConfig> {
    let retained: Vec<_> = existing
        .iter()
        .filter(|old| discovered.iter().any(|new| new.model_id == old.model_id))
        .cloned()
        .collect();
    let mut models = merge_discovered_provider_models(&retained, discovered);
    for model in &mut models {
        if let Some(old) = retained.iter().find(|old| old.model_id == model.model_id) {
            model
                .capability_overrides
                .extend(old.capability_overrides.clone());
        }
    }
    models
}

/// Merges ChatGPT subscription model metadata as a backend-owned catalog.
///
/// Unlike generic OpenAI-compatible providers, ChatGPT subscription models come
/// from Codex's first-party model catalog. Models omitted from a successful
/// refresh should stop appearing in Galaxy unless they are rediscovered later.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn merge_discovered_chatgpt_subscription_models(
    existing_models: &[OpenAIModelConfig],
    discovered_models: Vec<OpenAIModelConfig>,
) -> Vec<OpenAIModelConfig> {
    let mut merged = Vec::with_capacity(discovered_models.len());
    let mut discovered_ids = HashSet::new();

    for mut discovered in discovered_models {
        if !discovered_ids.insert(discovered.model_id.clone()) {
            continue;
        }

        if let Some(existing) = existing_models
            .iter()
            .find(|model| model.model_id == discovered.model_id)
        {
            discovered.enabled = existing.enabled;
            if existing.supports_system_messages.is_some() {
                discovered.supports_system_messages = existing.supports_system_messages;
            }
            for (key, value) in &existing.capability_overrides {
                discovered.capability_overrides.insert(key.clone(), *value);
            }
        }

        merged.push(discovered);
    }

    merged
}

#[cfg(not(target_family = "wasm"))]
fn openai_model_variant_id(model_id: &str, reasoning_effort: &str) -> String {
    format!("{model_id}::reasoning::{reasoning_effort}")
}

#[cfg(not(target_family = "wasm"))]
fn openai_model_context_window(
    model: &OpenAIModelConfig,
    provider_kind: OpenAIProviderKind,
) -> LLMContextWindow {
    let max_context_size = openai_model_context_size(model, provider_kind);
    let default_context_size = model
        .default_context_size
        .or_else(|| {
            // Compatibility with saved catalogs from before the fields were separated.
            (provider_kind == OpenAIProviderKind::ChatGPTSubscription)
                .then_some(model.max_input_tokens)
                .flatten()
        })
        .unwrap_or(max_context_size)
        .min(max_context_size);
    LLMContextWindow {
        is_configurable: max_context_size > default_context_size,
        min: default_context_size,
        max: max_context_size,
        // Subscription and LiteLLM limits are capped to the catalog default above.
        default_max: max_context_size,
    }
}

#[cfg(not(target_family = "wasm"))]
fn u32_from_json_any(value: &serde_json::Value, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| value[*key].as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

/// Read enumerated effort support, never infer choices from model names or a
/// generic supports_reasoning flag. Explicit lists take precedence over flags.
#[cfg(not(target_family = "wasm"))]
fn advertised_reasoning_efforts(value: &serde_json::Value) -> Vec<String> {
    for key in [
        "reasoning_effort_levels",
        "supported_reasoning_efforts",
        "reasoning_efforts",
        "supported_reasoning_levels",
    ] {
        if let Some(levels) = value[key].as_array() {
            let mut result = Vec::new();
            for level in levels {
                if let Some(effort) = level.as_str().or_else(|| level["effort"].as_str()) {
                    let effort = effort.trim();
                    if !effort.is_empty() && !result.iter().any(|entry| entry == effort) {
                        result.push(effort.to_string());
                    }
                }
            }
            return result;
        }
    }
    ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"]
        .into_iter()
        .filter(|level| value[format!("supports_{level}_reasoning_effort")].as_bool() == Some(true))
        .map(str::to_string)
        .collect()
}

/// Keep an endpoint's recommended window separate from its optional expanded
/// maximum. A bare model ID carries no token metadata, so it must not replace
/// richer limits when the two catalog endpoints are combined.
#[cfg(not(target_family = "wasm"))]
fn openai_context_limits(metadata: &serde_json::Value) -> (u32, Option<u32>, Option<u32>) {
    let positive_limit = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            metadata[*key]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
        })
    };
    let max_input_tokens =
        positive_limit(&["max_input_tokens", "input_token_limit", "max_prompt_tokens"]);
    let default_context_size = positive_limit(&[
        "default_context_size",
        "default_context_window",
        "context_window",
    ]);
    let maximum_context_size = positive_limit(&[
        "max_context_window",
        "max_model_len",
        "context_size",
        "token_size",
    ]);
    let effective_context_percent = metadata["effective_context_window_percent"]
        .as_u64()
        .filter(|value| (1..=100).contains(value))
        .unwrap_or(100);
    // A proxy may already reduce max_input_tokens by this percentage. Apply it
    // only to the original session window, never to that input limit again.
    let default_context_size = default_context_size
        .map(|value| {
            u32::try_from(u64::from(value) * effective_context_percent / 100)
                .unwrap_or(value)
                .max(1)
        })
        .or(max_input_tokens)
        .or(maximum_context_size);
    let context_size = maximum_context_size
        .or(default_context_size)
        .or(max_input_tokens)
        .unwrap_or(DEFAULT_DISCOVERED_MODEL_CONTEXT_SIZE);
    (context_size, max_input_tokens, default_context_size)
}

#[cfg(not(target_family = "wasm"))]
fn normalize_codex_release_version(version: &str) -> Option<String> {
    let version = version.trim();
    let version = version
        .strip_prefix("rust-v")
        .or_else(|| version.strip_prefix('v'))
        .unwrap_or(version);
    if version.is_empty()
        || !version
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
        || !version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+')
        })
    {
        return None;
    }
    Some(version.to_string())
}

#[cfg(not(target_family = "wasm"))]
fn codex_client_version_from_release_json(body: &serde_json::Value) -> Option<String> {
    body["tag_name"]
        .as_str()
        .and_then(normalize_codex_release_version)
        .or_else(|| {
            body["name"]
                .as_str()
                .and_then(normalize_codex_release_version)
        })
}

#[cfg(not(target_family = "wasm"))]
async fn fetch_latest_codex_client_version(client: &reqwest::Client) -> Result<String, String> {
    let response = client
        .get(CODEX_LATEST_RELEASE_URL)
        .header(reqwest::header::USER_AGENT, "Galaxy")
        .send()
        .await
        .map_err(|error| format!("Could not fetch the latest Codex release: {error}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Could not fetch the latest Codex release: HTTP {}",
            response.status()
        ));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Could not parse the latest Codex release: {error}"))?;
    codex_client_version_from_release_json(&body)
        .ok_or_else(|| "The latest Codex release did not include a usable version.".to_string())
}

#[cfg(not(target_family = "wasm"))]
async fn fetch_from_chatgpt_codex_models(
    client_version: &str,
    credentials: crate::ai::chatgpt_auth::ChatGPTAuthCredentials,
    client: &reqwest::Client,
) -> Result<Vec<OpenAIModelConfig>, String> {
    let mut request = client
        .get(CHATGPT_CODEX_MODELS_URL)
        .query(&[("client_version", client_version)])
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", credentials.access_token),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "Galaxy");
    if let Some(account_id) = credentials.account_id {
        request = request.header("ChatGPT-Account-ID", account_id);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("Could not fetch ChatGPT subscription models: {error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "ChatGPT model discovery failed: HTTP {status} {}",
            body.chars().take(500).collect::<String>()
        ));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Could not parse ChatGPT subscription models: {error}"))?;
    let models = chatgpt_models_from_codex_response(&body);
    if models.is_empty() {
        return Err("ChatGPT model discovery returned no visible models.".to_string());
    }
    log::info!(
        "[chatgpt/models] Fetched {} model(s) from Codex models endpoint using client_version={client_version}",
        models.len()
    );
    Ok(models)
}

#[cfg(not(target_family = "wasm"))]
fn chatgpt_models_from_codex_response(body: &serde_json::Value) -> Vec<OpenAIModelConfig> {
    let Some(models) = body["models"].as_array() else {
        return Vec::new();
    };

    models
        .iter()
        .filter_map(|model| {
            if model["visibility"].as_str() != Some("list") {
                return None;
            }

            let model_id = model["slug"].as_str()?.trim();
            if model_id.is_empty() {
                return None;
            }

            let default_context_size = u32_from_json_any(model, &["context_window"])
                .or_else(|| u32_from_json_any(model, &["max_context_window"]))
                .unwrap_or(DEFAULT_DISCOVERED_MODEL_CONTEXT_SIZE);
            let effective_context_percent = model["effective_context_window_percent"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| (1..=100).contains(value))
                .unwrap_or(100);
            let effective_context_size = |context_size: u32| {
                u32::try_from(u64::from(context_size) * u64::from(effective_context_percent) / 100)
                    .unwrap_or(context_size)
            };
            // Store the supported session window, not the optional long-context
            // maximum. Catalog consumers must not mistake that maximum for an
            // enabled subscription budget.
            let context_size = effective_context_size(default_context_size);
            let default_context_size = Some(context_size);
            let max_input_tokens =
                u32_from_json_any(model, &["max_input_tokens"]).map(effective_context_size);

            let vision_supported = model["input_modalities"]
                .as_array()
                .map(|modalities| {
                    modalities
                        .iter()
                        .any(|modality| modality.as_str() == Some("image"))
                })
                .unwrap_or(true);
            let reasoning_efforts = advertised_reasoning_efforts(model);

            Some(OpenAIModelConfig {
                model_id: model_id.to_string(),
                display_name: model["display_name"]
                    .as_str()
                    .filter(|display_name| !display_name.trim().is_empty())
                    .unwrap_or(model_id)
                    .to_string(),
                vision_supported,
                context_size,
                max_input_tokens,
                default_context_size,
                max_output_tokens: None,
                provider: Some("openai".to_string()),
                supports_system_messages: Some(true),
                capability_overrides: std::collections::HashMap::new(),
                reasoning_efforts,
                enabled: true,
            })
        })
        .collect()
}

/// Fetches model metadata from LiteLLM's `/model/info` endpoint which returns rich
/// metadata including accurate context window sizes, output token limits, and
/// capability flags (vision, function calling).
///
/// Returns `None` if the endpoint is unavailable or doesn't return valid data,
/// allowing the caller to fall back to the standard `/models` endpoint.
#[cfg(not(target_family = "wasm"))]
async fn fetch_from_litellm_model_info(
    base_url: &str,
    api_key: Option<&str>,
    client: &reqwest::Client,
) -> Option<Vec<OpenAIModelConfig>> {
    let url = format!("{base_url}/model/info");
    let mut request = client.get(&url);
    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            log::info!("[openai/litellm] /model/info not available ({e}), falling back to /models");
            return None;
        }
    };

    if !response.status().is_success() {
        log::info!(
            "[openai/litellm] /model/info returned HTTP {}, falling back to /models",
            response.status()
        );
        return None;
    }

    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[openai/litellm] Failed to parse /model/info response: {e}");
            return None;
        }
    };

    let data = body["data"].as_array()?;
    if data.is_empty() {
        return None;
    }

    let models: Vec<OpenAIModelConfig> = data
        .iter()
        .filter_map(|entry| {
            let model_name = entry["model_name"].as_str()?;
            let model_info = &entry["model_info"];

            let (context_size, max_input_tokens, default_context_size) =
                openai_context_limits(model_info);
            let max_output_tokens = model_info["max_output_tokens"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok());
            let vision_supported = model_info["supports_vision"].as_bool().unwrap_or(true);

            let display_name = model_name.replace(['-', '_'], " ");
            let display_name = display_name
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            // Detect provider from the underlying model path if available
            let litellm_model = entry["litellm_params"]["model"]
                .as_str()
                .unwrap_or(model_name);
            let provider = if litellm_model.contains("claude")
                || litellm_model.contains("anthropic")
                || litellm_model.contains("bedrock")
            {
                Some("anthropic".to_string())
            } else if litellm_model.contains("gpt")
                || litellm_model.contains("o1")
                || litellm_model.contains("o3")
            {
                Some("openai".to_string())
            } else if litellm_model.contains("gemini") {
                Some("google".to_string())
            } else {
                None
            };

            log::info!(
                "[openai/litellm] Discovered model '{}': context={}, max_output={}, vision={}",
                model_name,
                context_size,
                max_output_tokens.unwrap_or(0),
                vision_supported,
            );

            Some(OpenAIModelConfig {
                model_id: model_name.to_string(),
                display_name,
                vision_supported,
                context_size,
                max_input_tokens,
                default_context_size,
                max_output_tokens,
                provider,
                supports_system_messages: if model_name.starts_with("codex-gpt-") {
                    Some(false)
                } else {
                    model_info["supports_system_messages"].as_bool()
                },
                capability_overrides: std::collections::HashMap::new(),
                reasoning_efforts: advertised_reasoning_efforts(model_info),
                enabled: true,
            })
        })
        .collect();

    if models.is_empty() {
        return None;
    }

    log::info!(
        "[openai/litellm] Fetched {} model(s) from /model/info endpoint",
        models.len()
    );
    Some(models)
}

/// Fetches models from the standard OpenAI-compatible `/models` endpoint.
/// Used as a fallback when `/model/info` is unavailable.
#[cfg(not(target_family = "wasm"))]
async fn fetch_litellm_catalog(
    base_url: &str,
    api_key: Option<&str>,
    client: &reqwest::Client,
) -> Result<Vec<OpenAIModelConfig>, String> {
    let models = fetch_from_openai_models(base_url, api_key, client).await?;
    if models.is_empty() {
        return Ok(models);
    }
    let metadata = fetch_from_litellm_model_info(base_url, api_key, client)
        .await
        .unwrap_or_default();
    Ok(models
        .into_iter()
        .map(|model| {
            metadata
                .iter()
                .find(|info| info.model_id == model.model_id)
                .map(|info| {
                    let mut enriched = info.clone();
                    // /model/info supplies capabilities, but must not erase a
                    // smaller session budget supplied by /models (or vice versa).
                    enriched.default_context_size = model
                        .default_context_size
                        .into_iter()
                        .chain(info.default_context_size)
                        .min();
                    enriched.max_input_tokens = model
                        .max_input_tokens
                        .into_iter()
                        .chain(info.max_input_tokens)
                        .min();
                    enriched.context_size = match (
                        model.default_context_size,
                        info.default_context_size,
                    ) {
                        (Some(_), Some(_)) => model.context_size.max(info.context_size),
                        (Some(_), None) => model.context_size,
                        _ => info.context_size,
                    };
                    if enriched.reasoning_efforts.is_empty() {
                        enriched.reasoning_efforts = model.reasoning_efforts.clone();
                    }
                    enriched
                })
                .unwrap_or(model)
        })
        .collect())
}

#[cfg(not(target_family = "wasm"))]
async fn fetch_from_openai_models(
    base_url: &str,
    api_key: Option<&str>,
    client: &reqwest::Client,
) -> Result<Vec<OpenAIModelConfig>, String> {
    let url = format!("{base_url}/models");
    let mut request = client.get(&url);
    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("Could not refresh /models: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Model refresh failed: /models returned HTTP {}",
            response.status()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Invalid /models response: {error}"))?;
    let entries = body["data"]
        .as_array()
        .ok_or_else(|| "Invalid /models response: missing data array".to_string())?;
    if entries
        .iter()
        .any(|entry| entry["id"].as_str().is_none_or(|id| id.trim().is_empty()))
    {
        return Err("Invalid /models response: model ID is missing or empty".to_string());
    }

    fn u32_from_any(value: &serde_json::Value, keys: &[&str]) -> Option<u32> {
        keys.iter()
            .find_map(|key| value[*key].as_u64())
            .and_then(|value| u32::try_from(value).ok())
    }

    let models: Vec<OpenAIModelConfig> = entries
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let (context_size, max_input_tokens, default_context_size) = openai_context_limits(m);
            let max_output_tokens = u32_from_any(
                m,
                &[
                    "max_output_tokens",
                    "output_token_limit",
                    "max_completion_tokens",
                    "max_tokens",
                ],
            );

            let display_name = id
                .split('/')
                .next_back()
                .unwrap_or(id)
                .replace(['-', '_'], " ");
            let display_name = display_name
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            let provider = if id.contains("claude") || id.contains("anthropic") {
                Some("anthropic".to_string())
            } else if id.contains("gpt") || id.contains("o1") || id.contains("o3") {
                Some("openai".to_string())
            } else if id.contains("gemini") {
                Some("google".to_string())
            } else {
                None
            };

            Some(OpenAIModelConfig {
                model_id: id.to_string(),
                display_name,
                vision_supported: m["supports_vision"]
                    .as_bool()
                    .or_else(|| m["vision_support"].as_bool())
                    .unwrap_or(true),
                context_size,
                max_input_tokens,
                default_context_size,
                max_output_tokens,
                provider,
                supports_system_messages: if id.starts_with("codex-gpt-") {
                    Some(false)
                } else {
                    m["supports_system_messages"].as_bool()
                },
                capability_overrides: std::collections::HashMap::new(),
                reasoning_efforts: advertised_reasoning_efforts(m),
                enabled: true,
            })
        })
        .collect();

    log::info!(
        "[openai/litellm] Fetched {} model(s) from /models endpoint",
        models.len()
    );
    Ok(models)
}

/// Gets the last cached LLM metadata.
/// Disabled — Galaxy uses only locally configured providers. No server-fetched models
/// are cached or restored. The model list is built exclusively from Bedrock/LiteLLM
/// settings at startup.
fn get_cached_models(_app: &mut AppContext) -> Option<ModelsByFeature> {
    None
}

#[cfg(test)]
#[path = "llms_tests.rs"]
mod tests;

#[cfg(not(target_family = "wasm"))]
fn bedrock_discovery_config(
    settings: &AISettings,
) -> crate::ai::provider::client::BedrockClientConfig {
    crate::ai::provider::client::BedrockClientConfig::from_settings(settings)
}
