use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::config::Region;
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;

use crate::settings::ai::BedrockAuthMethod;

use super::convert::{build_converse_request, ConversationMessage, ToolDefinition};
use super::models::apply_cross_region_prefix;
use super::stream::bedrock_stream_to_response_events;
use crate::ai::agent::api::ResponseStream;

pub struct BedrockClient {
    runtime_client: BedrockRuntimeClient,
    region: String,
}

#[derive(Debug, Clone)]
pub struct BedrockClientConfig {
    pub auth_method: BedrockAuthMethod,
    pub profile: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub cross_region_inference: bool,
    pub fallback_to_warp: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BedrockError {
    #[error("Bedrock credentials not configured")]
    CredentialsNotConfigured,
    #[error("Bedrock region not configured and could not be auto-detected")]
    RegionNotConfigured,
    #[error("AWS credential error: {0}")]
    CredentialError(String),
    #[error("Bedrock API error: {0}")]
    ApiError(String),
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Access denied: {0}")]
    AccessDenied(String),
    #[error("Throttling: {0}")]
    Throttling(String),
    #[error("Validation error: {0}")]
    ValidationError(String),
}

impl BedrockClient {
    pub async fn from_config(config: BedrockClientConfig) -> Result<Self, BedrockError> {
        let aws_config = match config.auth_method {
            BedrockAuthMethod::Profile | BedrockAuthMethod::Sso => {
                let mut loader =
                    aws_config::defaults(BehaviorVersion::latest()).profile_name(&config.profile);

                if !config.region.is_empty() {
                    loader = loader.region(Region::new(config.region.clone()));
                }

                loader.load().await
            }
            BedrockAuthMethod::StaticKeys => {
                if config.access_key_id.is_empty() || config.secret_access_key.is_empty() {
                    return Err(BedrockError::CredentialsNotConfigured);
                }

                let creds = aws_credential_types::Credentials::new(
                    &config.access_key_id,
                    &config.secret_access_key,
                    None,
                    None,
                    "warp-bedrock-static",
                );

                let mut loader =
                    aws_config::defaults(BehaviorVersion::latest()).credentials_provider(creds);

                if !config.region.is_empty() {
                    loader = loader.region(Region::new(config.region.clone()));
                } else {
                    loader = loader.region(Region::new("us-east-1".to_string()));
                }

                loader.load().await
            }
        };

        let region = aws_config
            .region()
            .map(|r| r.to_string())
            .ok_or(BedrockError::RegionNotConfigured)?;

        let runtime_client = BedrockRuntimeClient::new(&aws_config);

        Ok(Self {
            runtime_client,
            region,
        })
    }

    pub async fn converse_stream(
        &self,
        model_id: &str,
        task_id: &str,
        messages: Vec<ConversationMessage>,
        system_prompt: Option<String>,
        tools: Vec<ToolDefinition>,
        max_tokens: i32,
        temperature: Option<f32>,
        cross_region_inference: bool,
    ) -> Result<ResponseStream, BedrockError> {
        let effective_model_id = if cross_region_inference {
            apply_cross_region_prefix(model_id, &self.region)
        } else {
            model_id.to_string()
        };

        log::info!(
            "[bedrock] converse_stream: model={effective_model_id}, region={}, messages={}, tools={}",
            self.region,
            messages.len(),
            tools.len()
        );

        let converted =
            build_converse_request(messages, system_prompt, tools, max_tokens, temperature, None, None);

        let mut request = self
            .runtime_client
            .converse_stream()
            .model_id(&effective_model_id)
            .set_system(Some(converted.system))
            .set_messages(Some(converted.messages))
            .inference_config(converted.inference_config);

        if let Some(tool_config) = converted.tool_config {
            request = request.tool_config(tool_config);
        }

        let output = request.send().await.map_err(|e| {
            let debug_msg = format!("{:?}", e);
            let display_msg = format!("{}", e);
            log::error!("[bedrock] API error (display): {display_msg}");
            log::error!("[bedrock] API error (debug): {debug_msg}");
            let msg = if debug_msg.len() > display_msg.len() {
                debug_msg
            } else {
                display_msg
            };
            if msg.contains("AccessDenied") || msg.contains("access denied") {
                BedrockError::AccessDenied(msg)
            } else if msg.contains("ThrottlingException") || msg.contains("throttl") {
                BedrockError::Throttling(msg)
            } else if msg.contains("ValidationException") || msg.contains("validation") {
                BedrockError::ValidationError(msg)
            } else if msg.contains("ResourceNotFoundException") {
                BedrockError::ModelNotFound(effective_model_id.clone())
            } else {
                BedrockError::ApiError(msg)
            }
        })?;

        log::info!("[bedrock] Stream connected successfully");
        Ok(Box::pin(bedrock_stream_to_response_events(output, task_id.to_string())))
    }

    pub fn runtime_client(&self) -> &BedrockRuntimeClient {
        &self.runtime_client
    }

    pub fn region(&self) -> &str {
        &self.region
    }
}
