use async_trait::async_trait;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEventStream, AgentRuntime, RuntimeCapabilities,
    RuntimeDescriptor, RuntimeKind, TurnControl, TurnRequest,
};
use rig_core::client::CompletionClient;
use rig_core::completion::{CompletionModel, CompletionRequest};
use rig_core::providers::openai;

use crate::request::build_completion_request as build_provider_completion_request;
use crate::stream::start_model_turn as start_provider_model_turn;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAICompatibleRuntimeConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub max_output_tokens: Option<u64>,
    pub supports_system_messages: bool,
}

#[derive(Clone, Debug)]
pub struct OpenAICompatibleRuntime {
    config: OpenAICompatibleRuntimeConfig,
    descriptor: RuntimeDescriptor,
}

impl OpenAICompatibleRuntime {
    pub fn new(config: OpenAICompatibleRuntimeConfig) -> Self {
        let descriptor = RuntimeDescriptor {
            id: format!("rig-openai-compatible:{}", config.model),
            display_name: format!("Rig / {}", config.model),
            kind: RuntimeKind::Provider,
            capabilities: RuntimeCapabilities::provider(),
        };
        Self { config, descriptor }
    }
}

#[async_trait]
impl AgentRuntime for OpenAICompatibleRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let client = openai::CompletionsClient::builder()
            // Rig 0.40 requires an API-key builder value. An empty key preserves
            // compatibility with unauthenticated local OpenAI-compatible servers.
            .api_key(self.config.api_key.as_deref().unwrap_or_default())
            .base_url(&self.config.base_url)
            .build()
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let model = client.completion_model(&self.config.model);
        start_model_turn(
            model,
            request,
            control,
            self.config.max_output_tokens,
            self.config.supports_system_messages,
        )
        .await
    }
}

async fn start_model_turn<M>(
    model: M,
    request: TurnRequest,
    control: TurnControl,
    configured_max_output_tokens: Option<u64>,
    supports_system_messages: bool,
) -> Result<AgentEventStream, AgentError>
where
    M: CompletionModel + Send + Sync + 'static,
{
    let max_output_tokens = request.max_output_tokens.or(configured_max_output_tokens);
    let completion_request = build_completion_request(
        request,
        configured_max_output_tokens,
        supports_system_messages,
    )?;
    start_provider_model_turn(model, completion_request, control, max_output_tokens).await
}

fn build_completion_request(
    request: TurnRequest,
    configured_max_output_tokens: Option<u64>,
    supports_system_messages: bool,
) -> Result<CompletionRequest, AgentError> {
    build_provider_completion_request(
        request,
        configured_max_output_tokens,
        supports_system_messages,
        false,
        Some(serde_json::json!({
            "stream_options": { "include_usage": true }
        })),
    )
}

#[cfg(test)]
#[path = "openai_compatible_tests.rs"]
mod tests;
