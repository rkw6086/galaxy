use async_trait::async_trait;
use aws_sdk_bedrockruntime::Client as AwsBedrockClient;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEventStream, AgentRuntime, ContentPart, ConversationMessage,
    MessageContent, MessageRole, RuntimeCapabilities, RuntimeDescriptor, RuntimeKind, TurnControl,
    TurnRequest,
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
    reasoning_model: String,
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
            capabilities: RuntimeCapabilities::provider(),
        };

        Ok(Self {
            client: RigBedrockClient::from(client),
            config,
            reasoning_model: resolved_model.clone(),
            resolved_model,
            descriptor,
        })
    }

    pub fn resolved_model(&self) -> &str {
        &self.resolved_model
    }

    /// Use the discovered foundation model's capabilities when invocation targets an
    /// opaque application inference profile rather than a foundation-model ID.
    pub fn with_reasoning_model(mut self, model: impl Into<String>) -> Self {
        self.reasoning_model = model.into();
        self
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
        let mut completion_request = build_bedrock_completion_request_for_model(
            request,
            self.config.max_output_tokens,
            &self.reasoning_model,
        )?;
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
    let model = request.model.as_str().to_owned();
    build_bedrock_completion_request_for_model(request, configured_max_output_tokens, &model)
}

fn build_bedrock_completion_request_for_model(
    mut request: TurnRequest,
    configured_max_output_tokens: Option<u64>,
    model: &str,
) -> Result<CompletionRequest, AgentError> {
    request.messages = prepare_bedrock_history(model, request.messages);
    build_completion_request(request, configured_max_output_tokens, true, true, None)
}

/// Project durable history into Bedrock's input format without changing the saved
/// transcript. Reasoning output is not universally accepted as reasoning input:
/// DeepSeek, Nova and other providers return unsigned, display-only reasoning.
/// Claude tool continuations need their complete signed blocks unchanged.
pub fn prepare_bedrock_history(
    model_id: &str,
    messages: Vec<ConversationMessage>,
) -> Vec<ConversationMessage> {
    let supports_signed_reasoning = accepts_signed_reasoning(model_id);
    messages
        .into_iter()
        .filter_map(|mut message| {
            if let MessageContent::MultiPart(parts) = &mut message.content {
                parts.retain(|part| match part {
                    ContentPart::Reasoning { signature, .. } => {
                        message.role == MessageRole::Assistant
                            && supports_signed_reasoning
                            && signature
                                .as_ref()
                                .is_some_and(|signature| !signature.trim().is_empty())
                    }
                    ContentPart::Text(_)
                    | ContentPart::Image { .. }
                    | ContentPart::ToolUse { .. }
                    | ContentPart::ToolResult { .. } => true,
                });
                if parts.is_empty() {
                    return None;
                }
            }
            Some(message)
        })
        .collect()
}

fn accepts_signed_reasoning(model_id: &str) -> bool {
    let model_id = model_id.trim();
    // An opaque application profile may point to Claude. Preserve its signed
    // blocks when no foundation-model hint is available; unsigned output is
    // still omitted. Production discovery supplies that hint to the runtime.
    if model_id.starts_with("arn:") && model_id.contains(":application-inference-profile/") {
        return true;
    }
    let model = strip_context_marker(model_id.rsplit('/').next().unwrap_or(model_id));
    let model = INFERENCE_PROFILE_PREFIXES
        .iter()
        .find_map(|prefix| model.strip_prefix(prefix))
        .unwrap_or(model);
    model.starts_with("anthropic.claude-")
        && !model.starts_with("anthropic.claude-instant-")
        && !model.starts_with("anthropic.claude-v1")
        && !model.starts_with("anthropic.claude-v2")
        && (!model.starts_with("anthropic.claude-3-") || model.starts_with("anthropic.claude-3-7-"))
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
