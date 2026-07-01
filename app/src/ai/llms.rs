use galaxy_core::ui::icons::Icon;
use galaxy_core::user_preferences::GetUserPreferences;
use galaxyui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity};
use parking_lot::FairMutex;
use serde::{de, Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use crate::{
    auth::{
        auth_manager::{AuthManager, AuthManagerEvent},
        AuthStateProvider,
    },
    network::{NetworkStatus, NetworkStatusEvent, NetworkStatusKind},
    report_error,
    server::server_api::ServerApiProvider,
    settings::ai::{
        AISettings, AISettingsChangedEvent, BedrockModelConfig, OpenAIModelConfig,
        OpenAIProviderConfig,
    },
    workspaces::user_workspaces::{UserWorkspaces, UserWorkspacesEvent},
};

use settings::Setting;

use super::execution_profiles::profiles::AIExecutionProfilesModel;

#[cfg(not(target_family = "wasm"))]
use super::bedrock::models::get_effective_models;

pub use ai::LLMId;

/// Checks if a user's' API key is being used for the given provider.
/// Returns `true` if BYO API key is enabled and a key exists for the provider.
pub fn is_using_api_key_for_provider(provider: &LLMProvider, app: &AppContext) -> bool {
    use ai::api_keys::ApiKeyManager;

    let api_keys = UserWorkspaces::as_ref(app)
        .is_byo_api_key_enabled()
        .then(|| ApiKeyManager::as_ref(app).keys().clone());

    match provider {
        LLMProvider::OpenAI => api_keys.is_some_and(|keys| keys.openai.is_some()),
        LLMProvider::Anthropic => api_keys.is_some_and(|keys| keys.anthropic.is_some()),
        LLMProvider::Google => api_keys.is_some_and(|keys| keys.google.is_some()),
        LLMProvider::Bedrock => true,
        LLMProvider::LiteLLM => true,
        _ => false,
    }
}

/// Key for cached LLM metadata in user preferences.
///
/// Note: this key used to store a single [`AvailableLLMs`]
/// but was migrated to store a full [`ModelsByFeature`].
pub const MODELS_BY_FEATURE_CACHE_KEY: &str = "AvailableLLMs";

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
}

/// The host where an LLM can be routed to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LLMModelHost {
    DirectApi,
    AwsBedrock,
    #[serde(other)]
    Unknown,
}

/// Configuration for routing an LLM to a specific host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutingHostConfig {
    pub enabled: bool,
    pub model_routing_host: LLMModelHost,
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

    fn default_llm_info(&self) -> &LLMInfo {
        self.info_for_id(&self.default_id)
            .expect("Default LLM ID must be present in choices")
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
        }],
        preferred_codex_model_id: None,
    }
}

impl Default for ModelsByFeature {
    fn default() -> Self {
        Self {
            agent_mode: AvailableLLMs {
                default_id: "anthropic.claude-opus-4-6[1m]".to_owned().into(),
                choices: vec![LLMInfo {
                    display_name: "Claude Opus 4.6".to_owned(),
                    base_model_name: "Claude Opus 4.6".to_owned(),
                    id: "anthropic.claude-opus-4-6[1m]".to_owned().into(),
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
                }],
                preferred_codex_model_id: None,
            },
            coding: AvailableLLMs {
                default_id: "anthropic.claude-sonnet-4-6[1m]".to_owned().into(),
                choices: vec![LLMInfo {
                    display_name: "Claude Sonnet 4.6".to_owned(),
                    base_model_name: "Claude Sonnet 4.6".to_owned(),
                    id: "anthropic.claude-sonnet-4-6[1m]".to_owned().into(),
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
                }],
                preferred_codex_model_id: None,
            },
            cli_agent: Some(AvailableLLMs {
                default_id: "anthropic.claude-haiku-4-5-20251001-v1:0".to_owned().into(),
                choices: vec![LLMInfo {
                    display_name: "Claude Haiku 4.5".to_owned(),
                    base_model_name: "Claude Haiku 4.5".to_owned(),
                    id: "anthropic.claude-haiku-4-5-20251001-v1:0".to_owned().into(),
                    reasoning_level: None,
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
                }],
                preferred_codex_model_id: None,
            }),
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
    /// Maps model IDs from OpenAI-compatible providers to their client configs.
    /// Used by `resolve_provider_config` to route requests to the correct endpoint.
    #[cfg(not(target_family = "wasm"))]
    openai_provider_routing: HashMap<String, super::openai::client::OpenAIClientConfig>,
}

impl LLMPreferences {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let models_by_feature = get_cached_models(ctx).unwrap_or_default();

        ctx.subscribe_to_model(&NetworkStatus::handle(ctx), |me, event, ctx| {
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
        ctx.subscribe_to_model(&AuthManager::handle(ctx), |me, event, ctx| {
            if let AuthManagerEvent::AuthComplete = event {
                me.refresh_authed_models(ctx);
            }
        });

        ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), |me, event, ctx| {
            if let UserWorkspacesEvent::TeamsChanged = event {
                me.refresh_authed_models(ctx);
            }
        });

        #[cfg(not(target_family = "wasm"))]
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, event, ctx| {
            if matches!(
                event,
                AISettingsChangedEvent::BedrockEnabled { .. }
                    | AISettingsChangedEvent::BedrockModels { .. }
                    | AISettingsChangedEvent::BedrockCrossRegionInference { .. }
                    | AISettingsChangedEvent::BedrockRegion { .. }
            ) {
                me.inject_bedrock_models(ctx);
                ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
            }
            if matches!(
                event,
                AISettingsChangedEvent::OpenAIEnabled { .. }
                    | AISettingsChangedEvent::OpenAIModels { .. }
                    | AISettingsChangedEvent::OpenAIBaseUrl { .. }
                    | AISettingsChangedEvent::OpenAIProviders { .. }
            ) {
                me.inject_openai_models(ctx);
                ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
            }
        });

        let base_llm_for_terminal_view = HashMap::new();

        let mut me = Self {
            models_by_feature,
            last_update: None,
            base_llm_for_terminal_view,
            #[cfg(not(target_family = "wasm"))]
            openai_provider_routing: HashMap::new(),
        };

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
                    context_size: default.context_size,
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

        // Remove any placeholder/auto-routing entries now that real Bedrock models are available.
        self.models_by_feature
            .agent_mode
            .choices
            .retain(|m| m.provider != LLMProvider::Unknown);
        self.models_by_feature
            .coding
            .choices
            .retain(|m| m.provider != LLMProvider::Unknown);
        if let Some(ref mut cli) = self.models_by_feature.cli_agent {
            cli.choices.retain(|m| m.provider != LLMProvider::Unknown);
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
        if !*settings.openai_enabled.value() {
            return;
        }

        // Collect all (provider_name, base_url, api_key, models) tuples from both config paths.
        let mut provider_entries: Vec<(String, String, Option<String>, Vec<OpenAIModelConfig>)> =
            Vec::new();

        // Path 1: Multi-provider `ai.providers[]`
        let providers: Vec<OpenAIProviderConfig> = settings.openai_providers.value().clone();
        for provider in providers {
            if provider.models.is_empty() {
                continue;
            }
            provider_entries.push((
                provider.name,
                provider.base_url,
                provider.api_key,
                provider.models,
            ));
        }

        // Path 2: Legacy single-provider `ai.openai.{base_url, models}`
        let legacy_models: Vec<OpenAIModelConfig> = settings.openai_models.value().clone();
        if !legacy_models.is_empty() {
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
            provider_entries.push((name, base_url, api_key, legacy_models));
        }

        if provider_entries.is_empty() {
            return;
        }

        let mut total_injected = 0;
        for (provider_name, base_url, api_key, models) in provider_entries {
            let client_config = OpenAIClientConfig {
                base_url: base_url.clone(),
                api_key: api_key.clone(),
                model: None, // filled per-request from model_id
            };

            for model in &models {
                // Register the routing entry
                self.openai_provider_routing
                    .insert(model.model_id.clone(), client_config.clone());

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

    /// Returns the OpenAI client config for a given model ID, if it was injected
    /// from an OpenAI-compatible provider.
    #[cfg(not(target_family = "wasm"))]
    pub fn openai_client_config_for_model(
        &self,
        model_id: &str,
    ) -> Option<&super::openai::client::OpenAIClientConfig> {
        self.openai_provider_routing.get(model_id)
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
                if let Some(llm_info) = self.models_by_feature.agent_mode.info_for_id(llm_id) {
                    return llm_info;
                }
            }
        }

        let profile = AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);

        profile
            .data()
            .base_model
            .clone()
            .and_then(|id| self.models_by_feature.agent_mode.info_for_id(&id))
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
            .and_then(|id| self.models_by_feature.coding.info_for_id(&id))
            .unwrap_or_else(|| self.models_by_feature.coding.default_llm_info())
    }

    /// Returns the set of LLMs available for Agent Mode use.
    pub fn get_base_llm_choices_for_agent_mode(&self) -> impl Iterator<Item = &LLMInfo> {
        // Don't show admin-disabled models in the dropdown
        self.models_by_feature
            .agent_mode
            .choices
            .iter()
            .filter(|llm| !matches!(llm.disable_reason, Some(DisableReason::AdminDisabled)))
    }

    /// Returns the set of LLMs available for coding.
    pub fn get_coding_llm_choices(&self) -> impl Iterator<Item = &LLMInfo> {
        // Don't show admin-disabled models in the dropdown
        self.models_by_feature
            .coding
            .choices
            .iter()
            .filter(|llm| !matches!(llm.disable_reason, Some(DisableReason::AdminDisabled)))
    }

    /// Returns the set of LLMs available for CLI agent.
    pub fn get_cli_agent_llm_choices(&self) -> impl Iterator<Item = &LLMInfo> {
        self.get_cli_agent_available().choices.iter()
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
            .and_then(|id| available.info_for_id(&id))
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
    pub fn get_llm_info(&self, id: &LLMId) -> Option<&LLMInfo> {
        self.models_by_feature.info_for_id(id)
    }

    /// Returns the default base model as a fallback.
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
    pub fn refresh_authed_models(&self, ctx: &mut ModelContext<Self>) {
        // Don't try to fetch auth'd models if the user is not logged in yet.
        if !AuthStateProvider::as_ref(ctx).get().is_logged_in() {
            return;
        }

        let ai_api_client = ServerApiProvider::as_ref(ctx).get_ai_client();
        ctx.spawn(
            async move { ai_api_client.get_feature_model_choices().await },
            |me, result, ctx| match result {
                Ok(update) => {
                    if update != me.models_by_feature {
                        me.on_server_update(update, ctx);
                    }
                }
                Err(e) => {
                    report_error!(e.context("Failed to fetch LLMs from server"));
                }
            },
        );
    }

    /// No auth required (i.e. to populate the pre-login onboarding picker).
    fn refresh_public_models(&self, ctx: &mut ModelContext<Self>) {
        let ai_api_client = ServerApiProvider::as_ref(ctx).get_ai_client();
        ctx.spawn(
            async move { ai_api_client.get_free_available_models(None).await },
            |me, result, ctx| match result {
                Ok(update) => {
                    if update != me.models_by_feature {
                        me.on_server_update(update, ctx);
                    }
                }
                Err(e) => {
                    report_error!(e.context("Failed to fetch free-tier LLMs from server"));
                }
            },
        );
    }

    pub fn refresh_available_models(&self, ctx: &mut ModelContext<Self>) {
        if AuthStateProvider::as_ref(ctx).get().is_logged_in() {
            self.refresh_authed_models(ctx);
        } else {
            self.refresh_public_models(ctx);
        }
    }

    pub fn update_feature_model_choices(
        &mut self,
        choices_result: Result<ModelsByFeature, anyhow::Error>,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Ok(choices) = choices_result {
            self.on_server_update(choices, ctx);
        }
    }

    fn on_server_update(&mut self, update: ModelsByFeature, ctx: &mut ModelContext<Self>) {
        let has_existing_persisted_config = get_cached_models(ctx).is_some();

        let old = std::mem::replace(&mut self.models_by_feature, update);

        match serde_json::to_string(&self.models_by_feature) {
            Ok(serialized_update) => {
                if let Err(e) = ctx
                    .private_user_preferences()
                    .write_value(MODELS_BY_FEATURE_CACHE_KEY, serialized_update)
                {
                    log::error!("Failed to cache LLMs: {e}");
                }
            }
            Err(e) => {
                log::error!("Failed to serialize LLMs for cache: {e}");
            }
        }

        #[cfg(not(target_family = "wasm"))]
        self.inject_bedrock_models(ctx);

        // Clear any model selections where the model is no longer supported.
        let profiles_model = AIExecutionProfilesModel::handle(ctx);
        profiles_model.update(ctx, |profiles, ctx| {
            for profile_id in profiles.get_all_profile_ids() {
                if let Some(profile) = profiles.get_profile_by_id(profile_id, ctx) {
                    if let Some(preferred_llm_id) = &profile.data().base_model {
                        if self
                            .models_by_feature
                            .agent_mode
                            .info_for_id(preferred_llm_id)
                            .is_none()
                        {
                            profiles.set_base_model(profile_id, None, ctx);
                        }
                    }
                    if let Some(preferred_llm_id) = &profile.data().coding_model {
                        if self
                            .models_by_feature
                            .coding
                            .info_for_id(preferred_llm_id)
                            .is_none()
                        {
                            profiles.set_coding_model(profile_id, None, ctx);
                        }
                    }
                    if let Some(preferred_llm_id) = &profile.data().cli_agent_model {
                        if self
                            .get_cli_agent_available()
                            .info_for_id(preferred_llm_id)
                            .is_none()
                        {
                            profiles.set_cli_agent_model(profile_id, None, ctx);
                        }
                    }
                    if let Some(preferred_llm_id) = &profile.data().computer_use_model {
                        if self
                            .get_computer_use_available()
                            .info_for_id(preferred_llm_id)
                            .is_none()
                        {
                            profiles.set_computer_use_model(profile_id, None, ctx);
                        }
                    }
                }
            }
        });

        let new_choices =
            get_new_agent_mode_choices(&old.agent_mode, &self.models_by_feature.agent_mode);
        if !new_choices.is_empty() {
            self.last_update = Some(AvailableLLMsUpdate {
                new_choices,
                // We shouldn't show the update for the initial LLM config creation.
                popup_visibility_state: Arc::new(FairMutex::new(
                    if has_existing_persisted_config {
                        UpdatePopupVisibilityState::WaitingToBeShown
                    } else {
                        UpdatePopupVisibilityState::Hidden
                    },
                )),
            });
        }

        ctx.emit(LLMPreferencesEvent::UpdatedAvailableLLMs);
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

/// Gets the last cached LLM metadata.
fn get_cached_models(app: &mut AppContext) -> Option<ModelsByFeature> {
    let value = app
        .private_user_preferences()
        .read_value(MODELS_BY_FEATURE_CACHE_KEY)
        .ok()
        .flatten()?;

    // Try to deserialize to the [`ModelsByFeature`] type.
    match serde_json::from_str::<ModelsByFeature>(value.as_str()) {
        Ok(config) => Some(config),
        Err(e1) => {
            // If that fails, try to deserialize directly to [`AvailableLLMs`].
            // Before we had model choice by feature, all available LLMs were solely
            // for Agent Mode.
            match serde_json::from_str::<AvailableLLMs>(value.as_str()) {
                Ok(config) => Some(ModelsByFeature {
                    agent_mode: config,
                    ..Default::default()
                }),
                Err(e2) => {
                    log::warn!("Failed to deserialize cached LLMs: {e1}\n{e2}");
                    None
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "llms_tests.rs"]
mod tests;
