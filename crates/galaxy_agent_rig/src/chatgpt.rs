use std::path::PathBuf;

use async_trait::async_trait;
use futures::StreamExt;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, AgentRuntime, ConversationMessage,
    MessageContent, MessageRole, RuntimeCapabilities, RuntimeDescriptor, RuntimeKind, TurnControl,
    TurnRequest, turn_control,
};
use rig_core::client::CompletionClient;
use rig_core::providers::chatgpt;

use crate::request::build_completion_request;
use crate::stream::start_model_turn;

/// Refreshes Galaxy's cached ChatGPT subscription credentials without allowing
/// Rig to fall back to its interactive device-code flow.
pub async fn refresh_chatgpt_subscription_credentials() -> Result<(), String> {
    let client = chatgpt::Client::builder()
        .oauth()
        .allow_device_flow(false)
        .originator("galaxy")
        .build()
        .map_err(|error| error.to_string())?;
    client.authorize().await.map_err(|error| error.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGPTSubscriptionRuntimeConfig {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub max_output_tokens: Option<u64>,
    pub auth_file: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ChatGPTSubscriptionRuntime {
    config: ChatGPTSubscriptionRuntimeConfig,
    descriptor: RuntimeDescriptor,
}

fn reasoning_additional_params(reasoning_effort: Option<&str>) -> Option<serde_json::Value> {
    reasoning_effort.map(|effort| {
        // Codex exposes `ultra` as a client-side delegation mode, while the
        // Responses wire format carries its maximum reasoning as `max`.
        let wire_effort = if effort == "ultra" { "max" } else { effort };
        serde_json::json!({"reasoning": {"effort": wire_effort}})
    })
}

impl ChatGPTSubscriptionRuntime {
    pub fn new(config: ChatGPTSubscriptionRuntimeConfig) -> Self {
        let descriptor = RuntimeDescriptor {
            id: format!("rig-chatgpt-subscription:{}", config.model),
            display_name: format!("ChatGPT Subscription / {}", config.model),
            kind: RuntimeKind::Provider,
            capabilities: RuntimeCapabilities::provider(),
        };
        Self { config, descriptor }
    }

    /// Completes a text-only request for callers such as crosscheck reviewers.
    pub async fn complete_text(
        &self,
        system_prompt: String,
        user_prompt: String,
    ) -> Result<String, String> {
        let mut request = TurnRequest::new(
            self.config.model.clone(),
            vec![ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(user_prompt),
            }],
        );
        request.system_prompt = Some(system_prompt);
        request.max_output_tokens = self.config.max_output_tokens;

        let (_sender, control) = turn_control();
        let mut stream = self
            .start_turn(request, control)
            .await
            .map_err(|error| error.to_string())?;
        let mut output = String::new();
        while let Some(event) = stream.next().await {
            match event.map_err(|error| error.to_string())? {
                AgentEvent::TextDelta { text } => output.push_str(&text),
                AgentEvent::ReasoningDelta { .. }
                | AgentEvent::ReasoningCompleted { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::KeepAlive
                | AgentEvent::ToolCallProgress { .. }
                | AgentEvent::UsageUpdated { .. }
                | AgentEvent::RuntimeActivityUpdated { .. }
                | AgentEvent::ContextUsageUpdated { .. }
                | AgentEvent::UserInputAccepted { .. }
                | AgentEvent::RuntimeNotice { .. }
                | AgentEvent::TurnStopped { .. }
                | AgentEvent::Tool { .. } => {}
            }
        }

        if output.is_empty() {
            return Err("ChatGPT returned an empty response.".to_string());
        }
        Ok(output)
    }
}

#[async_trait]
impl AgentRuntime for ChatGPTSubscriptionRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let mut builder = chatgpt::Client::builder()
            .oauth()
            .allow_device_flow(false)
            .originator("galaxy");
        if let Some(auth_file) = &self.config.auth_file {
            builder = builder.auth_file(auth_file);
        }
        let client = builder
            .build()
            .map_err(|error| AgentError::new(AgentErrorKind::Configuration, error.to_string()))?;
        let model = client.completion_model(&self.config.model);
        let max_output_tokens = request.max_output_tokens.or(self.config.max_output_tokens);
        let additional_params =
            reasoning_additional_params(self.config.reasoning_effort.as_deref());
        let completion_request = build_completion_request(
            request,
            self.config.max_output_tokens,
            true,
            true,
            additional_params,
        )?;

        start_model_turn(model, completion_request, control, max_output_tokens).await
    }
}

#[cfg(test)]
#[path = "chatgpt_tests.rs"]
mod tests;
