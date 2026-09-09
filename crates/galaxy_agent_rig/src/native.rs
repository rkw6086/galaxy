use async_trait::async_trait;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEventStream, AgentRuntime, RuntimeCapabilities,
    RuntimeDescriptor, RuntimeKind, TurnControl, TurnRequest,
};
use rig_core::client::{CompletionClient, ModelListingClient};
use rig_core::providers::{anthropic, gemini};

use crate::request::build_completion_request;
use crate::stream::{start_model_completion, start_model_turn};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RigModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_size: Option<u32>,
}

pub async fn discover_anthropic_models(api_key: &str) -> Result<Vec<RigModelInfo>, String> {
    let client = anthropic::Client::new(api_key).map_err(|error| error.to_string())?;
    let models = client
        .list_models()
        .await
        .map_err(|error| error.to_string())?;
    Ok(models
        .into_iter()
        .map(|model| RigModelInfo {
            display_name: model.display_name().to_string(),
            id: model.id,
            context_size: model.context_length,
        })
        .collect())
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

#[derive(Clone, Debug)]
pub struct AnthropicRuntimeConfig {
    pub api_key: String,
    pub model: String,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct AnthropicRuntime {
    config: AnthropicRuntimeConfig,
    descriptor: RuntimeDescriptor,
}

impl AnthropicRuntime {
    pub fn new(config: AnthropicRuntimeConfig) -> Self {
        let descriptor = native_descriptor("anthropic", &config.model);
        Self { config, descriptor }
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
        if self.config.api_key.trim().is_empty() {
            return Err(AgentError::new(
                AgentErrorKind::Configuration,
                "An Anthropic API key is required",
            ));
        }
        let client = anthropic::Client::new(&self.config.api_key)
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let model = client.completion_model(&self.config.model);
        let max_output_tokens = request.max_output_tokens.or(self.config.max_output_tokens);
        let completion_request =
            build_completion_request(request, max_output_tokens, true, true, None)?;
        start_model_turn(model, completion_request, control, max_output_tokens).await
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
