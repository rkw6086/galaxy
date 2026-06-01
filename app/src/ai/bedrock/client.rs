use std::sync::{Arc, Mutex};

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::config::Region;
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;

use crate::settings::ai::BedrockAuthMethod;

use super::external_config::ExternalBedrockConfig;
use super::convert::{build_converse_request, CachingConfig, ConversationMessage, ToolDefinition};
use super::diagnostic::BedrockDiagnosticLogger;
use super::models::apply_cross_region_prefix;
use super::response_translator::bedrock_stream_to_response_events;
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
}

impl BedrockClientConfig {
    /// Applies external config (from Claude Code / OpenCode) as fallback values
    /// when Galaxy's own settings are at their defaults.
    pub fn with_external_fallbacks(mut self) -> Self {
        let external = ExternalBedrockConfig::load();
        if external.is_empty() {
            return self;
        }

        if self.profile == "default" {
            if let Some(profile) = external.profile {
                log::info!("[bedrock] Using profile from external config: {profile}");
                self.profile = profile;
            }
        }

        if self.region.is_empty() {
            if let Some(region) = external.region {
                log::info!("[bedrock] Using region from external config: {region}");
                self.region = region;
            }
        }

        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BedrockError {
    #[error("Bedrock credentials not configured")]
    CredentialsNotConfigured,
    #[error("Bedrock region not configured and could not be auto-detected")]
    RegionNotConfigured,
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
        needs_create_task: bool,
        messages: Vec<ConversationMessage>,
        system_prompt: Option<String>,
        compact_summary: Option<String>,
        tools: Vec<ToolDefinition>,
        max_tokens: i32,
        temperature: Option<f32>,
        cross_region_inference: bool,
        user_query: Option<String>,
        diagnostic_logger: Option<Arc<BedrockDiagnosticLogger>>,
        messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
        is_summarization: bool,
    ) -> Result<ResponseStream, BedrockError> {
        let effective_model_id = if cross_region_inference {
            apply_cross_region_prefix(model_id, &self.region)
        } else {
            model_id.to_string()
        };

        let external_config = ExternalBedrockConfig::load();
        let caching_config = CachingConfig::from_external_config(&external_config);

        if !caching_config.enabled {
            log::info!("[bedrock] Prompt caching disabled (DISABLE_PROMPT_CACHING=1)");
        }

        log::info!(
            "[bedrock] converse_stream: model={effective_model_id}, region={}, messages={}, tools={}",
            self.region,
            messages.len(),
            tools.len()
        );

        let converted = build_converse_request(
            messages.clone(),
            system_prompt.clone(),
            compact_summary,
            tools.clone(),
            max_tokens,
            temperature,
            None,
            None,
            caching_config,
        );

        if let Some(ref logger) = diagnostic_logger {
            logger.log_bedrock_input(
                &messages,
                &system_prompt,
                &tools,
                max_tokens,
                temperature,
                cross_region_inference,
            );
        }

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
            let display_msg = format!("{e}");
            log::error!("[bedrock] API error (display): {display_msg}");
            log::error!("[bedrock] API error (debug): {debug_msg}");
            let msg = if debug_msg.len() > display_msg.len() {
                debug_msg.clone()
            } else {
                display_msg.clone()
            };
            if let Some(ref logger) = diagnostic_logger {
                logger.log_result_fail(&msg);
                if let Some(path) = logger.dump_error_snapshot(&display_msg, &debug_msg) {
                    log::error!(
                        "[bedrock] Wrote Bedrock failure snapshot to {}",
                        path.display()
                    );
                }
            }
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
        Ok(Box::pin(bedrock_stream_to_response_events(
            output,
            task_id.to_string(),
            needs_create_task,
            user_query,
            diagnostic_logger,
            messages_sent,
            effective_model_id,
            is_summarization,
        )))
    }

}
