use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEventStream, AgentRuntime, RuntimeCapabilities,
    RuntimeDescriptor, RuntimeKind, TurnControl, TurnRequest,
};
use rig_core::client::{CompletionClient, ModelListingClient};
use rig_core::providers::gemini;
use rig_core::http_client::{self, HttpClientExt};

use crate::anthropic_auth::{anthropic_client, prepare_subscription_request, restore_tool_names};
use crate::request::build_completion_request;
use crate::stream::{start_model_completion, start_model_turn};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RigModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_size: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

pub async fn discover_anthropic_models(api_key: &str) -> Result<Vec<RigModelInfo>, String> {
    let client = anthropic_client(api_key)?;
    let mut models = Vec::new();
    let mut cursors = HashSet::new();
    let mut path = "/v1/models?limit=1000".to_string();
    loop {
        let request = client.get(&path).and_then(|request| Ok(request.body(http_client::NoBody)?))
            .map_err(|_| "Could not build the Anthropic models request.".to_string())?;
        let response = client.send::<_, Vec<u8>>(request).await
            .map_err(|_| "Could not retrieve Anthropic models.".to_string())?;
        if !response.status().is_success() {
            return Err(format!("Anthropic model discovery returned HTTP {}.", response.status()));
        }
        let body = response.into_body().await
            .map_err(|_| "Could not read the Anthropic models response.".to_string())?;
        let page: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| "Invalid Anthropic models response.".to_string())?;
        let entries = page["data"].as_array()
            .ok_or_else(|| "Anthropic models response is missing its model list.".to_string())?;
        for entry in entries {
            models.push(anthropic_model_info(entry)?);
        }
        match page["has_more"].as_bool() {
            Some(false) => return Ok(models),
            Some(true) => {},
            None => return Err("Anthropic models response is missing pagination state.".to_string()),
        }
        let cursor = page["last_id"].as_str().filter(|cursor| {
            !cursor.is_empty() && cursor.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        }).ok_or_else(|| "Invalid Anthropic model pagination cursor.".to_string())?;
        if !cursors.insert(cursor.to_string()) {
            return Err("Anthropic model pagination did not advance.".to_string());
        }
        path = format!("/v1/models?limit=1000&after_id={cursor}");
    }
}

fn positive_token_limit(value: &serde_json::Value) -> Option<u32> {
    value.as_u64().and_then(|tokens| u32::try_from(tokens).ok()).filter(|tokens| *tokens > 0)
}

fn anthropic_model_info(entry: &serde_json::Value) -> Result<RigModelInfo, String> {
    let id = entry["id"].as_str().filter(|id| !id.is_empty())
        .ok_or_else(|| "Anthropic returned a model without an ID.".to_string())?;
    // Read live limits first. Rig's generic model lister drops these fields.
    // The models.dev snapshot is metadata only, never an availability allowlist.
    static LIMITS: OnceLock<serde_json::Value> = OnceLock::new();
    let limits = LIMITS.get_or_init(|| serde_json::from_str(include_str!("../data/anthropic_limits.json"))
        .expect("bundled Anthropic limits must be valid JSON"));
    let fallback = &limits["models"][id];
    Ok(RigModelInfo {
        id: id.to_string(),
        display_name: entry["display_name"].as_str().unwrap_or(id).to_string(),
        context_size: positive_token_limit(&entry["max_input_tokens"])
            .or_else(|| positive_token_limit(&fallback["context"])),
        max_output_tokens: positive_token_limit(&entry["max_tokens"])
            .or_else(|| positive_token_limit(&fallback["output"])),
    })
}

pub async fn discover_gemini_models(api_key: &str) -> Result<Vec<RigModelInfo>, String> {
    let client = gemini::Client::new(api_key).map_err(|error| error.to_string())?;
    let models = client
        .list_models()
        .await
        .map_err(|error| error.to_string())?;
    Ok(models
        .into_iter()
        .filter(|model| !model.id.contains("embedding"))
        .map(|model| RigModelInfo {
            display_name: model.display_name().to_string(),
            id: model.id,
            context_size: model.context_length,
            max_output_tokens: None,
        })
        .collect())
}

pub fn vertex_ai_model_catalog() -> Vec<RigModelInfo> {
    [
        (rig_vertexai::completion::GEMINI_2_5_PRO, "Gemini 2.5 Pro"),
        (
            rig_vertexai::completion::GEMINI_2_5_FLASH,
            "Gemini 2.5 Flash",
        ),
        (
            rig_vertexai::completion::GEMINI_2_5_FLASH_LITE,
            "Gemini 2.5 Flash Lite",
        ),
        (rig_vertexai::completion::GEMINI_1_5_PRO, "Gemini 1.5 Pro"),
        (
            rig_vertexai::completion::GEMINI_1_5_FLASH,
            "Gemini 1.5 Flash",
        ),
    ]
    .into_iter()
    .map(|(id, display_name)| RigModelInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        context_size: None,
        max_output_tokens: None,
    })
    .collect()
}

pub fn validate_vertex_ai_credentials(project_id: &str, location: &str) -> Result<(), String> {
    rig_vertexai::Client::builder()
        .with_project(project_id)
        .with_location(location)
        .build()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[derive(Clone)]
pub struct AnthropicRuntimeConfig {
    pub api_key: String,
    pub model: String,
    pub max_output_tokens: Option<u64>,
}

impl std::fmt::Debug for AnthropicRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicRuntimeConfig")
            .field("credential_configured", &!self.api_key.is_empty())
            .field(
                "subscription",
                &crate::is_claude_subscription_token(&self.api_key),
            )
            .field("model", &self.model)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
}

pub type AnthropicCredentialResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

#[derive(Clone)]
pub struct AnthropicRuntime {
    config: AnthropicRuntimeConfig,
    descriptor: RuntimeDescriptor,
    credential_resolver: Option<AnthropicCredentialResolver>,
}

impl std::fmt::Debug for AnthropicRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicRuntime")
            .field("config", &self.config)
            .field("descriptor", &self.descriptor)
            .finish()
    }
}

impl AnthropicRuntime {
    pub fn new(config: AnthropicRuntimeConfig) -> Self {
        let descriptor = native_descriptor("anthropic", &config.model);
        Self {
            config,
            descriptor,
            credential_resolver: None,
        }
    }

    /// Resolves renewable credentials before each turn without owning model retries.
    pub fn with_credential_resolver(mut self, resolver: AnthropicCredentialResolver) -> Self {
        self.credential_resolver = Some(resolver);
        self
    }
}

#[async_trait]
impl AgentRuntime for AnthropicRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let credential = match &self.credential_resolver {
            Some(resolve) => resolve()
                .await
                .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error))?,
            None => self.config.api_key.clone(),
        };
        if credential.trim().is_empty() {
            return Err(AgentError::new(
                AgentErrorKind::Configuration,
                "An Anthropic API key or Claude subscription token is required",
            ));
        }
        let client = anthropic_client(&credential)
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let mut model = client.completion_model(&self.config.model);
        let max_output_tokens = request.max_output_tokens.or(self.config.max_output_tokens);
        let mut completion_request =
            build_completion_request(request, max_output_tokens, true, true, None)?;
        let aliases = if crate::is_claude_subscription_token(&credential) {
            model = model.with_prompt_caching();
            Some(prepare_subscription_request(&mut completion_request)?)
        } else {
            None
        };
        let stream =
            start_model_turn(model, completion_request, control, max_output_tokens).await?;
        if let Some(aliases) = aliases {
            Ok(Box::pin(stream.map(move |result| {
                result.map(|mut event| {
                    restore_tool_names(&mut event, &aliases);
                    event
                })
            })))
        } else {
            Ok(stream)
        }
    }
}

#[derive(Clone, Debug)]
pub struct GeminiRuntimeConfig {
    pub api_key: String,
    pub model: String,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct GeminiRuntime {
    config: GeminiRuntimeConfig,
    descriptor: RuntimeDescriptor,
}

impl GeminiRuntime {
    pub fn new(config: GeminiRuntimeConfig) -> Self {
        let descriptor = native_descriptor("gemini", &config.model);
        Self { config, descriptor }
    }
}

#[async_trait]
impl AgentRuntime for GeminiRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        if self.config.api_key.trim().is_empty() {
            return Err(AgentError::new(
                AgentErrorKind::Configuration,
                "A Gemini API key is required",
            ));
        }
        let client = gemini::Client::new(&self.config.api_key)
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let model = client.completion_model(&self.config.model);
        let max_output_tokens = request.max_output_tokens.or(self.config.max_output_tokens);
        let completion_request =
            build_completion_request(request, max_output_tokens, true, true, None)?;
        start_model_turn(model, completion_request, control, max_output_tokens).await
    }
}

#[derive(Clone, Debug)]
pub struct VertexAiRuntimeConfig {
    pub project_id: String,
    pub location: String,
    pub model: String,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct VertexAiRuntime {
    config: VertexAiRuntimeConfig,
    descriptor: RuntimeDescriptor,
}

impl VertexAiRuntime {
    pub fn new(config: VertexAiRuntimeConfig) -> Self {
        let descriptor = native_descriptor("vertex-ai", &config.model);
        Self { config, descriptor }
    }
}

#[async_trait]
impl AgentRuntime for VertexAiRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        if self.config.project_id.trim().is_empty() {
            return Err(AgentError::new(
                AgentErrorKind::Configuration,
                "A Google Cloud project ID is required for Vertex AI",
            ));
        }
        let client = rig_vertexai::Client::builder()
            .with_project(&self.config.project_id)
            .with_location(if self.config.location.trim().is_empty() {
                "global"
            } else {
                &self.config.location
            })
            .build()
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let model = client.completion_model(&self.config.model);
        let max_output_tokens = request.max_output_tokens.or(self.config.max_output_tokens);
        let completion_request =
            build_completion_request(request, max_output_tokens, true, true, None)?;
        start_model_completion(model, completion_request, control, max_output_tokens).await
    }
}

fn native_descriptor(provider: &str, model: &str) -> RuntimeDescriptor {
    RuntimeDescriptor {
        id: format!("rig-{provider}:{model}"),
        display_name: format!("Rig / {provider} / {model}"),
        kind: RuntimeKind::Provider,
        capabilities: RuntimeCapabilities::provider(),
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
