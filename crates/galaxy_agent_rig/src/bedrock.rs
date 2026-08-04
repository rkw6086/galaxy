use async_trait::async_trait;
use aws_sdk_bedrockruntime::Client as AwsBedrockClient;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEventStream, AgentRuntime, RuntimeCapabilities,
    RuntimeDescriptor, RuntimeKind, TurnControl, TurnRequest,
};
use rig_bedrock::client::Client as RigBedrockClient;
use rig_bedrock::completion::CompletionModel;
use rig_core::client::CompletionClient;
use rig_core::completion::CompletionRequest;

use crate::request::build_completion_request;
use crate::stream::start_model_turn;

const INFERENCE_PROFILE_PREFIXES: &[&str] = &["us.", "eu.", "apac.", "jp.", "au.", "global."];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BedrockRigConfig {
    pub model: String,
    pub region: String,
    pub cross_region_inference: bool,
    pub prompt_caching: bool,
    pub max_output_tokens: Option<u64>,
}

/// A Rig Bedrock client built from Galaxy's already-resolved AWS SDK client.
///
/// Credential/profile/SSO resolution remains in Galaxy's explicit Bedrock
/// configuration boundary. Rig receives the resulting SDK client and owns the
/// Converse request/stream conversion from that point onward.
#[derive(Clone)]
pub struct BedrockRuntime {
    client: RigBedrockClient,
    config: BedrockRigConfig,
    resolved_model: String,
    descriptor: RuntimeDescriptor,
}

impl BedrockRuntime {
    pub fn from_aws_client(
        client: AwsBedrockClient,
        config: BedrockRigConfig,
    ) -> Result<Self, AgentError> {
        let resolved_model =
            resolve_bedrock_model_id(&config.model, &config.region, config.cross_region_inference)?;
        let descriptor = RuntimeDescriptor {
            id: format!("rig-bedrock:{resolved_model}"),
            display_name: format!("Rig / Bedrock / {resolved_model}"),
            kind: RuntimeKind::Provider,
            capabilities: RuntimeCapabilities {
                model_selection: true,
                session_resume: false,
                steering: false,
                tool_permissions: false,
            },
        };

        Ok(Self {
            client: RigBedrockClient::from(client),
            config,
            resolved_model,
            descriptor,
        })
    }

    pub fn resolved_model(&self) -> &str {
        &self.resolved_model
    }

    pub fn completion_model(&self) -> CompletionModel {
        let model = self.client.completion_model(&self.resolved_model);
        if self.config.prompt_caching {
            model.with_prompt_caching()
        } else {
            model
        }
    }
}

#[async_trait]
impl AgentRuntime for BedrockRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        let max_output_tokens = request.max_output_tokens.or(self.config.max_output_tokens);
        let mut completion_request =
            build_bedrock_completion_request(request, self.config.max_output_tokens)?;
        // Context markers and inference-profile expansion are Galaxy model
        // configuration, not identifiers that Rig should send unchanged.
        completion_request.model = Some(self.resolved_model.clone());
        start_model_turn(
            self.completion_model(),
            completion_request,
            control,
            max_output_tokens,
        )
        .await
    }
}

pub fn build_bedrock_completion_request(
    request: TurnRequest,
    configured_max_output_tokens: Option<u64>,
) -> Result<CompletionRequest, AgentError> {
    build_completion_request(request, configured_max_output_tokens, true, true, None)
}

pub fn resolve_bedrock_model_id(
    configured_model: &str,
    region: &str,
    cross_region_inference: bool,
) -> Result<String, AgentError> {
    let model = strip_context_marker(configured_model.trim());
    if model.is_empty() {
        return Err(AgentError::new(
            AgentErrorKind::Configuration,
            "Bedrock model ID is empty",
        ));
    }

    if !cross_region_inference
        || model.starts_with("arn:")
        || INFERENCE_PROFILE_PREFIXES
            .iter()
            .any(|prefix| model.starts_with(prefix))
    {
        return Ok(model.to_string());
    }

    let prefix = inference_profile_prefix(region);
    Ok(prefix
        .map(|prefix| format!("{prefix}.{model}"))
        .unwrap_or_else(|| model.to_string()))
}

fn strip_context_marker(model: &str) -> &str {
    model
        .get(..model.len().saturating_sub(4))
        .filter(|_| model.ends_with("[1m]") || model.ends_with("[1M]"))
        .unwrap_or(model)
}

fn inference_profile_prefix(region: &str) -> Option<&'static str> {
    match region {
        region if region.starts_with("us-") || region.starts_with("ca-") => Some("us"),
        region if region.starts_with("eu-") || region == "il-central-1" => Some("eu"),
        "ap-northeast-1" | "ap-northeast-3" => Some("jp"),
        "ap-southeast-2" | "ap-southeast-4" | "ap-southeast-6" => Some("au"),
        region if region.starts_with("ap-") => Some("apac"),
        _ => None,
    }
}

#[cfg(test)]
#[path = "bedrock_tests.rs"]
mod tests;
