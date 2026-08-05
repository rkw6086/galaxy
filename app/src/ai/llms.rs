#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use ai::api_keys::ApiKeyManager;
pub use ai::LLMId;
use galaxy_core::features::FeatureFlag;
use galaxy_core::ui::icons::Icon;
use galaxy_core::user_preferences::GetUserPreferences;
use galaxyui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity};
use parking_lot::FairMutex;
use serde::{de, Deserialize, Serialize};
use settings::Setting;
use warp_multi_agent_api as api;

use super::custom_model_routers::{self, CustomModelRouter, ModelConfigError};
use super::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::acp::acp_selection_identity;
use crate::ai::bedrock::models::get_effective_models;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::AuthStateProvider;
use crate::network::{NetworkStatus, NetworkStatusEvent, NetworkStatusKind};
use crate::server::server_api::ServerApiProvider;
use crate::settings::{AcpAgentSettings, BedrockModelConfig, OpenAIModelConfig};
use crate::user_config::{WarpConfig, WarpConfigUpdateEvent};
use crate::workspaces::user_workspaces::{UserWorkspaces, UserWorkspacesEvent};
use crate::{report_error, AISettings};

/// Checks if a user's' API key is being used for the given provider.
/// AWS Bedrock is the only supported provider in Galaxy; there is no
/// user-pasted BYO key path for other providers.
pub fn is_using_api_key_for_provider(_provider: &LLMProvider, _app: &AppContext) -> bool {
    false
}

pub fn should_show_bedrock_icon_for_model(llm: &LLMInfo, app: &AppContext) -> bool {
    UserWorkspaces::as_ref(app).is_bedrock_enabled(app)
        && llm
            .host_configs
            .get(&LLMModelHost::AwsBedrock)
            .is_some_and(|config| config.enabled)
}

/// Key for cached LLM metadata in user preferences.
///
/// Note: this key used to store a single [`AvailableLLMs`]
/// but was migrated to store a full [`ModelsByFeature`].
pub const MODELS_BY_FEATURE_CACHE_KEY: &str = "AvailableLLMs";
const CUSTOM_ENDPOINT_USAGE_FALLBACK_LABEL: &str = "Custom endpoint";

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
        // Base label includes optional description in parentheses
        match &self.description {
            // This is a temporary implementation that won't scale well for longer
            // descriptions. We should implement a better approach for displaying
            // model descriptions, maybe through subtext.
            Some(desc) => format!("{} ({})", self.display_name, desc),
            None => self.display_name.clone(),
        }
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
            preferred_codex_model_id,
        })
    }

    fn info_for_id(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.choices.iter().find(|info| info.id == *id)
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
        default_id: "computer-use-agent-auto".to_owned().into(),
        choices: vec![LLMInfo {
            display_name: "auto".to_owned(),
            base_model_name: "auto".to_owned(),
            id: "computer-use-agent-auto".to_owned().into(),
            reasoning_level: None,
            usage_metadata: LLMUsageMetadata {
                request_multiplier: 1,
                credit_multiplier: None,
            },
            description: None,
            disable_reason: None,
            vision_supported: true,
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
            default_id: "placeholder".to_owned().into(),
            choices: vec![LLMInfo {
                display_name: "No models configured".to_owned(),
                base_model_name: "No models configured".to_owned(),
                id: "placeholder".to_owned().into(),
                reasoning_level: None,
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
    acp_selections: HashMap<LLMId, BTreeMap<String, serde_json::Value>>,
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
            }
        });

        // TODO: Instead of querying this ad-hoc upon a successful log in, we should add the
        // available LLMs query to the general workspace metadata query which is polled
        // and hooked up to workspace changes. For that to work, each user would need to
        // have a personal workspace. This is a stop-gap.
        ctx.subscribe_to_model(&AuthManager::handle(ctx), |me, _, event, ctx| {
            if let AuthManagerEvent::AuthComplete = event {
                me.refresh_authed_models(ctx);
            }
        });

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

        // Re-inject provider models when Bedrock or OpenAI enabled state changes.
        #[cfg(not(target_family = "wasm"))]
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            use crate::settings::AISettingsChangedEvent;
            if matches!(
                event,
                AISettingsChangedEvent::BedrockEnabled { .. }
                    | AISettingsChangedEvent::OpenAIEnabled { .. }
                    | AISettingsChangedEvent::AcpEnabled { .. }
                    | AISettingsChangedEvent::OpenAIBaseUrl { .. }
                    | AISettingsChangedEvent::OpenAIApiKey { .. }
                    | AISettingsChangedEvent::OpenAIModels { .. }
                    | AISettingsChangedEvent::OpenAIProviders { .. }
                    | AISettingsChangedEvent::AcpAgents { .. }
                    | AISettingsChangedEvent::AcpAgentId { .. }
            ) {
                me.inject_bedrock_models(ctx);
                me.inject_openai_models(ctx);
                if matches!(
                    event,
                    AISettingsChangedEvent::OpenAIEnabled { .. }
                        | AISettingsChangedEvent::OpenAIBaseUrl { .. }
                        | AISettingsChangedEvent::OpenAIApiKey { .. }
                ) {
                    me.fetch_openai_models_from_endpoint(ctx);
                }
                // Safety: ensure the default model is still present in choices.
                // If all provider models were removed, the default_id would dangle.
                me.ensure_default_model_present();
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
            Self::ensure_default_models_in_settings(ctx);
            me.inject_bedrock_models(ctx);
            me.inject_openai_models(ctx);
            me.fetch_openai_models_from_endpoint(ctx);
        }

        me
    }

    #[cfg(not(target_family = "wasm"))]
    fn ensure_default_models_in_settings(ctx: &mut ModelContext<Self>) {
        use crate::ai::bedrock::models::DEFAULT_BEDROCK_MODELS;

        let settings = AISettings::as_ref(ctx);
        let mut current_models: Vec<BedrockModelConfig> = settings.bedrock_models.value().clone();

        let existing_ids: std::collections::HashSet<String> =
            current_models.iter().map(|m| m.model_id.clone()).collect();

        let mut added = false;
        for default in DEFAULT_BEDROCK_MODELS {
            if !existing_ids.contains(default.model_id as &str) {
                current_models.push(BedrockModelConfig {
                    model_id: default.model_id.to_string(),
                    display_name: default.display_name.to_string(),
                    vision_supported: default.vision_supported,
                    use_rig: false,
                });
                added = true;
            }
        }

        if added {
            log::info!(
                "[bedrock] Added missing default models to settings — now {} total",
                current_models.len()
            );
            AISettings::handle(ctx).update(ctx, |settings, ctx| {
                let _ = settings.bedrock_models.set_value(current_models, ctx);
            });
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn inject_bedrock_models(&mut self, ctx: &AppContext) {
        // Strip both existing Bedrock models and placeholder Unknown models.
        self.models_by_feature
            .agent_mode
            .choices
            .retain(|m| m.provider != LLMProvider::Bedrock && m.provider != LLMProvider::Unknown);
        self.models_by_feature
            .coding
            .choices
            .retain(|m| m.provider != LLMProvider::Bedrock && m.provider != LLMProvider::Unknown);
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            cli.choices.retain(|m| {
                m.provider != LLMProvider::Bedrock && m.provider != LLMProvider::Unknown
            });
        }

        let settings = AISettings::as_ref(ctx);
        if !*settings.bedrock_enabled.value() {
            return;
        }

        let user_models: Vec<BedrockModelConfig> = settings.bedrock_models.value().clone();
        let region = settings.bedrock_region.value().clone();
        let cross_region = *settings.bedrock_cross_region_inference.value();

        // Check if user wants only 1-hour cache models
        use crate::ai::bedrock::external_config::ExternalBedrockConfig;
        let external_config = ExternalBedrockConfig::load();
        let require_1h_cache = external_config.enable_prompt_caching_1h;

        let mut effective = get_effective_models(&user_models);

        // Filter out models that don't support 1-hour caching if required
        if require_1h_cache {
            effective.retain(|model| {
                // 1-hour caching is supported by Claude 4.5+ models
                // Opus 4.5+, Sonnet 4.5+, Haiku 4.5+
                let supports_1h = model.model_id.contains("4-5")
                    || model.model_id.contains("4.5")
                    || model.model_id.contains("-4-6") // Opus/Sonnet 4.6+ also support 1h
                    || model.model_id.contains("4.6")
                    || model.model_id.contains("-4-7")
                    || model.model_id.contains("4.7")
                    || model.model_id.contains("-4-8")
                    || model.model_id.contains("4.8");

                if !supports_1h {
                    log::info!(
                        "[bedrock] Filtering out model {} - does not support 1-hour cache (ENABLE_PROMPT_CACHING_1H=1)",
                        model.model_id
                    );
                }
                supports_1h
            });

            if effective.is_empty() {
                log::warn!("[bedrock] No models left after filtering for 1-hour cache support!");
            }
        }

        let effective = effective;
        for model in effective {
            let model_id = if cross_region && !region.is_empty() {
                super::bedrock::models::apply_cross_region_prefix(&model.model_id, &region)
            } else {
                model.model_id.clone()
            };

            let llm_info = LLMInfo {
                id: LLMId::from(model_id.as_str()),
                display_name: model.display_name.clone(),
                base_model_name: model.display_name.clone(),
                reasoning_level: None,
                usage_metadata: LLMUsageMetadata {
                    request_multiplier: 1,
                    credit_multiplier: None,
                },
                description: Some("AWS Bedrock".to_string()),
                disable_reason: None,
                vision_supported: model.vision_supported,
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
                context_window: LLMContextWindow::default(),
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
        // Note: external_config already loaded above for filtering
        let external_config_for_default = ExternalBedrockConfig::load();

        if let Some(id) = external_config_for_default
            .anthropic_model
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
            self.models_by_feature.agent_mode.default_id = id;
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
            self.models_by_feature.coding.default_id = id;
        }

        // Default CLI agent to the first available model.
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            if let Some(id) = cli.choices.first().map(|m| m.id.clone()) {
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

        let settings = AISettings::as_ref(ctx);
        self.inject_acp_models(ctx);
        if !*settings.openai_enabled.value() {
            return;
        }

        let mut provider_entries: Vec<(String, String, Option<String>, Vec<OpenAIModelConfig>)> =
            Vec::new();

        let configured_models = settings.openai_models.value().clone();
        let single_provider_models = if configured_models.is_empty() {
            self.fetched_openai_models.clone()
        } else {
            configured_models
        };

        if !single_provider_models.is_empty() {
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
            provider_entries.push((name, base_url, api_key, single_provider_models));
        }

        provider_entries.extend(
            settings
                .openai_providers
                .value()
                .iter()
                .filter_map(|provider| {
                    if provider.base_url.trim().is_empty() || provider.models.is_empty() {
                        return None;
                    }
                    Some((
                        provider.name.clone(),
                        provider.base_url.clone(),
                        provider.api_key.clone(),
                        provider.models.clone(),
                    ))
                }),
        );

        if provider_entries.is_empty() {
            return;
        }

        let mut total_injected = 0;
        let mut seen_model_ids: HashSet<String> = HashSet::new();
        for (provider_name, base_url, api_key, models) in provider_entries {
            for model in &models {
                if !seen_model_ids.insert(model.model_id.clone()) {
                    continue;
                }

                // Register the routing entry
                let client_config = OpenAIClientConfig {
                    base_url: base_url.clone(),
                    api_key: api_key.clone(),
                    model: None, // filled per-request from model_id
                    max_input_tokens: Some(openai_model_context_size(model)),
                    max_output_tokens: model.max_output_tokens,
                    use_rig: model.use_rig,
                    supports_system_messages: model.supports_system_messages(),
                };
                self.openai_provider_routing
                    .insert(model.model_id.clone(), client_config);

                let llm_info = LLMInfo {
                    id: LLMId::from(model.model_id.as_str()),
                    display_name: model.display_name.clone(),
                    base_model_name: model.display_name.clone(),
                    reasoning_level: None,
                    usage_metadata: LLMUsageMetadata {
                        request_multiplier: 1,
                        credit_multiplier: None,
                    },
                    description: Some(provider_name.clone()),
                    disable_reason: None,
                    vision_supported: model.vision_supported,
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
                    context_window: openai_model_context_window(model),
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
        if !*settings.acp_enabled.value() {
            return;
        }
        for agent in settings.acp_agents.value() {
            let model_option = agent
                .config_options
                .iter()
                .find(|option| option.category.as_deref() == Some("model"));
            let Some(model_option) = model_option else {
                continue;
            };
            let secondary = agent.config_options.iter().filter(|option| {
                matches!(
                    option.category.as_deref(),
                    Some("mode") | Some("thought_level")
                )
            });
            for value in &model_option.options {
                let suffix = secondary
                    .clone()
                    .filter_map(|option| {
                        option
                            .options
                            .iter()
                            .find(|value| value.value == option.current_value)
                            .or_else(|| option.options.first())
                            .map(|value| value.name.clone())
                    })
                    .collect::<Vec<_>>();
                let display_name = if suffix.is_empty() {
                    value.name.clone()
                } else {
                    format!("{} ({})", value.name, suffix.join(", "))
                };
                let mut selection =
                    crate::ai::acp::AcpRuntimeModel::current_config_values(&agent.config_options);
                selection.insert(model_option.id.clone(), value.value.clone());
                let id = acp_selection_identity(&agent.id, &selection);
                let llm_id = LLMId::from(id.as_str());
                self.acp_selections.insert(llm_id.clone(), selection);
                let info = LLMInfo {
                    id: llm_id,
                    display_name,
                    base_model_name: value.name.clone(),
                    reasoning_level: None,
                    usage_metadata: LLMUsageMetadata {
                        request_multiplier: 1,
                        credit_multiplier: None,
                    },
                    description: Some(agent.name.clone()),
                    disable_reason: None,
                    vision_supported: false,
                    spec: None,
                    provider: LLMProvider::Unknown,
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
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn acp_selection_for_model(
        &self,
        model_id: &LLMId,
    ) -> Option<&BTreeMap<String, serde_json::Value>> {
        self.acp_selections.get(model_id)
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn selected_acp_config_for_agent(
        &self,
        agent_name: &str,
        ctx: &AppContext,
    ) -> Option<BTreeMap<String, serde_json::Value>> {
        let profile = AIExecutionProfilesModel::as_ref(ctx).active_profile(None, ctx);
        let model_id = profile.data().base_model.as_ref()?;
        let model = self.models_by_feature.agent_mode.info_for_id(model_id)?;
        model
            .description
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(agent_name))
            .then(|| self.acp_selections.get(model_id).cloned())
            .flatten()
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
        if !*settings.openai_enabled.value() {
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

        let _ = ctx.spawn(
            async move {
                let base = base_url.trim_end_matches('/');
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default();

                // Try LiteLLM /model/info first for rich metadata
                if let Some(models) =
                    fetch_from_litellm_model_info(base, api_key.as_deref(), &client).await
                {
                    return models;
                }

                // Fallback to standard OpenAI /models endpoint
                fetch_from_openai_models(base, api_key.as_deref(), &client).await
            },
            |me, models, ctx| {
                if !models.is_empty() {
                    me.fetched_openai_models = models.clone();
                    AISettings::handle(ctx).update(ctx, |settings, ctx| {
                        if let Err(err) = settings.openai_models.set_value(models, ctx) {
                            report_error!(err.context("Failed to persist fetched OpenAI models"));
                        }
                    });
                    me.inject_openai_models(ctx);
                    ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
                }
            },
        );
    }

    /// Explicitly refreshes the models for one entry in the OpenAI-compatible
    /// provider registry. Unlike the legacy endpoint refresh, this is only
    /// called from a user action so configured remote endpoints are never
    /// contacted merely because Galaxy started.
    #[cfg(not(target_family = "wasm"))]
    pub fn fetch_openai_provider_models(
        &mut self,
        provider_index: usize,
        ctx: &mut ModelContext<Self>,
    ) {
        let settings = AISettings::as_ref(ctx);
        if !*settings.openai_enabled.value() {
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
        if provider.base_url.trim().is_empty() {
            return;
        }

        let requested_base_url = provider.base_url;
        let api_key = provider.api_key.filter(|key| !key.is_empty());
        let request_base_url = requested_base_url.clone();

        let _ = ctx.spawn(
            async move {
                let base = request_base_url.trim_end_matches('/');
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default();

                if let Some(models) =
                    fetch_from_litellm_model_info(base, api_key.as_deref(), &client).await
                {
                    return models;
                }

                fetch_from_openai_models(base, api_key.as_deref(), &client).await
            },
            move |_, discovered_models, ctx| {
                if discovered_models.is_empty() {
                    return;
                }

                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut providers = settings.openai_providers.value().clone();
                    let Some(provider) = providers.get_mut(provider_index) else {
                        return;
                    };

                    // Do not apply a response to an entry that was edited or
                    // reordered while its discovery request was in flight.
                    if provider.base_url != requested_base_url {
                        return;
                    }

                    provider.models =
                        merge_discovered_provider_models(&provider.models, discovered_models);
                    if let Err(err) = settings.openai_providers.set_value(providers, ctx) {
                        report_error!(err.context("Failed to persist discovered provider models"));
                    }
                });
            },
        );
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

    /// Returns the set of LLMs available for computer use agent.
    pub fn get_computer_use_llm_choices(&self) -> impl Iterator<Item = &LLMInfo> {
        self.get_computer_use_available().choices.iter()
    }

    /// Returns the `LLMInfo` for the computer use agent model.
    pub fn get_active_computer_use_model<'a>(
        &'a self,
        app: &'a AppContext,
        terminal_view_id: Option<EntityId>,
    ) -> &'a LLMInfo {
        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);

        let available = self.get_computer_use_available();
        profile
            .data()
            .computer_use_model
            .clone()
            .and_then(|id| available.info_for_id(&id))
            .unwrap_or_else(|| available.default_llm_info())
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
                            .get_computer_use_available()
                            .usable_info_for_id(preferred_llm_id, ctx)
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

#[cfg(not(target_family = "wasm"))]
fn openai_model_context_size(model: &OpenAIModelConfig) -> u32 {
    model.max_input_tokens.unwrap_or(model.context_size)
}

/// Merges endpoint metadata into a provider's configured models without
/// discarding local routing choices or manually configured models.
#[cfg(not(target_family = "wasm"))]
fn merge_discovered_provider_models(
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
            discovered.use_rig = existing.use_rig;
            if existing.supports_system_messages.is_some() {
                discovered.supports_system_messages = existing.supports_system_messages;
            }
            if discovered.provider.is_none() {
                discovered.provider = existing.provider.clone();
            }
        } else {
            discovered.use_rig = true;
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

#[cfg(not(target_family = "wasm"))]
fn openai_model_context_window(model: &OpenAIModelConfig) -> LLMContextWindow {
    let context_size = openai_model_context_size(model);
    LLMContextWindow {
        is_configurable: false,
        min: context_size,
        max: context_size,
        default_max: context_size,
    }
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

            let max_input_tokens = model_info["max_input_tokens"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok());
            let max_output_tokens = model_info["max_output_tokens"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok());
            let context_size = max_input_tokens.unwrap_or(200_000);

            let vision_supported = model_info["supports_vision"].as_bool().unwrap_or(false);

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
                max_output_tokens,
                provider,
                use_rig: false,
                supports_system_messages: if model_name.starts_with("codex-gpt-") {
                    Some(false)
                } else {
                    model_info["supports_system_messages"].as_bool()
                },
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
async fn fetch_from_openai_models(
    base_url: &str,
    api_key: Option<&str>,
    client: &reqwest::Client,
) -> Vec<OpenAIModelConfig> {
    let url = format!("{base_url}/models");
    let mut request = client.get(&url);
    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[openai/litellm] Failed to fetch models from /models endpoint: {e}");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        log::warn!(
            "[openai/litellm] /models returned HTTP {}",
            response.status()
        );
        return Vec::new();
    }

    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[openai/litellm] Failed to parse /models response: {e}");
            return Vec::new();
        }
    };

    fn u32_from_any(value: &serde_json::Value, keys: &[&str]) -> Option<u32> {
        keys.iter()
            .find_map(|key| value[*key].as_u64())
            .and_then(|value| u32::try_from(value).ok())
    }

    let models: Vec<OpenAIModelConfig> = body["data"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let max_input_tokens = u32_from_any(
                m,
                &["max_input_tokens", "input_token_limit", "max_prompt_tokens"],
            );
            let context_size = u32_from_any(m, &["max_model_len", "context_window", "token_size"])
                .or(max_input_tokens)
                .unwrap_or(200_000);
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
                    .unwrap_or(false),
                context_size,
                max_input_tokens,
                max_output_tokens,
                provider,
                use_rig: false,
                supports_system_messages: if id.starts_with("codex-gpt-") {
                    Some(false)
                } else {
                    m["supports_system_messages"].as_bool()
                },
            })
        })
        .collect();

    log::info!(
        "[openai/litellm] Fetched {} model(s) from /models endpoint",
        models.len()
    );
    models
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
