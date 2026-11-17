//! The crosscheck reviewer model.
//!
//! This model manages the lifecycle of a crosscheck review for a given
//! conversation. It tracks the current iteration, invokes the reviewer
//! LLM, and emits events to signal outcomes.

use std::collections::HashMap;

use galaxyui::{Entity, ModelContext, SingletonEntity};

use super::prompt;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::ResponseStream;
use crate::ai::openai::client::{OpenAIClient, OpenAIClientConfig};
use crate::ai::provider::ProviderConfig;
use crate::settings::OpenAIProviderKind;

/// Maximum default iterations if the setting is somehow zero.
const FALLBACK_MAX_ITERATIONS: u32 = 3;

/// Max output tokens for the reviewer response.
const REVIEWER_MAX_OUTPUT_TOKENS: u32 = 4096;

/// The outcome of a single crosscheck review cycle.
#[derive(Debug, Clone)]
pub enum ReviewOutcome {
    /// The reviewer approved the work.
    Approved,
    /// The reviewer has feedback that should be sent to the main agent.
    Feedback { message: String },
    /// The maximum number of iterations was reached; auto-accepting.
    MaxIterationsReached { last_feedback: String },
    /// The reviewer invocation failed (e.g., network error).
    Error { error: String },
}

/// Events emitted by the `CrosscheckReviewer`.
#[derive(Debug, Clone)]
pub enum CrosscheckReviewerEvent {
    /// A review cycle completed with the given outcome.
    ReviewCompleted {
        conversation_id: AIConversationId,
        outcome: ReviewOutcome,
    },
}

/// Per-conversation crosscheck state.
#[derive(Debug, Clone)]
struct ConversationReviewState {
    iteration: u32,
    max_iterations: u32,
}

/// Singleton model that manages crosscheck reviews across conversations.
pub struct CrosscheckReviewer {
    /// Active review states keyed by conversation ID.
    active_reviews: HashMap<AIConversationId, ConversationReviewState>,
}

impl Entity for CrosscheckReviewer {
    type Event = CrosscheckReviewerEvent;
}

impl CrosscheckReviewer {
    pub fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            active_reviews: HashMap::new(),
        }
    }

    /// Starts or continues a crosscheck review for the given conversation.
    ///
    /// `agent_output` is the text content of the main agent's last response
    /// that should be reviewed.
    ///
    /// Returns `true` if a review was initiated, `false` if the conversation
    /// is not eligible (e.g., already at max iterations).
    pub fn start_review(
        &mut self,
        conversation_id: AIConversationId,
        max_iterations: u32,
        agent_output: String,
        model_id: String,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let max_iterations = if max_iterations == 0 {
            FALLBACK_MAX_ITERATIONS
        } else {
            max_iterations
        };

        let state = self
            .active_reviews
            .entry(conversation_id)
            .or_insert_with(|| ConversationReviewState {
                iteration: 0,
                max_iterations,
            });

        if state.iteration >= state.max_iterations {
            // Already exhausted iterations
            return false;
        }

        state.iteration += 1;
        let current_iteration = state.iteration;
        let max = state.max_iterations;

        log::info!(
            "[crosscheck] Starting review iteration {current_iteration}/{max} for conversation {conversation_id:?} using model {model_id}"
        );

        // Resolve the provider config for the reviewer model
        let provider_config = Self::resolve_reviewer_provider_config(&model_id, ctx);

        // Spawn the reviewer request asynchronously
        let conversation_id_for_task = conversation_id;
        ctx.spawn(
            Self::invoke_reviewer(
                agent_output,
                model_id,
                provider_config,
                #[cfg(not(target_family = "wasm"))]
                serde_json::json!({
                    "conversation_id": conversation_id,
                    "iteration": current_iteration,
                    "max_iterations": max,
                }),
            ),
            move |me, reviewer_response, ctx| {
                me.handle_reviewer_response(
                    conversation_id_for_task,
                    reviewer_response,
                    current_iteration,
                    max,
                    ctx,
                );
            },
        );

        true
    }

    /// Clears crosscheck state for a conversation (e.g., on new user query or cancel).
    pub fn reset_review(&mut self, conversation_id: AIConversationId) {
        self.active_reviews.remove(&conversation_id);
    }

    /// Returns the current iteration count for a conversation, or 0 if not active.
    pub fn current_iteration(&self, conversation_id: AIConversationId) -> u32 {
        self.active_reviews
            .get(&conversation_id)
            .map_or(0, |s| s.iteration)
    }

    /// Returns `true` if a crosscheck review is in-flight for the conversation.
    pub fn is_reviewing(&self, conversation_id: AIConversationId) -> bool {
        self.active_reviews.contains_key(&conversation_id)
    }

    /// Resolves the provider configuration for the reviewer model.
    ///
    /// Uses the same enabled-model routing rules as foreground requests.
    fn resolve_reviewer_provider_config(
        model_id: &str,
        ctx: &ModelContext<Self>,
    ) -> ProviderConfig {
        let mut provider_config = ResponseStream::resolve_provider_config(model_id, ctx);
        if let ProviderConfig::OpenAI(config) = &mut provider_config {
            config.max_output_tokens = Some(REVIEWER_MAX_OUTPUT_TOKENS);
        }
        provider_config
    }

    /// Invokes the reviewer LLM with the given agent output.
    ///
    /// Sends a request to the configured provider with:
    /// - System prompt: the crosscheck reviewer prompt
    /// - User message: the agent's output to review
    /// - No tools (reviewer is text-only)
    /// - Non-streaming (collects the full response)
    async fn invoke_reviewer(
        agent_output: String,
        model_id: String,
        provider_config: ProviderConfig,
        #[cfg(not(target_family = "wasm"))] mut activity_context: serde_json::Value,
    ) -> Result<String, String> {
        #[cfg(not(target_family = "wasm"))]
        let started_at = instant::Instant::now();
        #[cfg(not(target_family = "wasm"))]
        if crate::ai::activity::is_recording() {
            let effective_model = match &provider_config {
                ProviderConfig::OpenAI(config) => config.model.as_deref().unwrap_or(&model_id),
                ProviderConfig::Bedrock(_) | ProviderConfig::None => &model_id,
            };
            activity_context["model_id"] = serde_json::json!(effective_model);
            activity_context["request_id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
            activity_context["snapshot_kind"] = serde_json::json!("reviewer_input");
            #[derive(serde::Serialize)]
            struct ReviewerInput<'a> {
                system_prompt: &'a str,
                user_prompt_prefix: &'a str,
                agent_output: &'a str,
                max_output_tokens: u32,
            }
            crate::ai::activity::record_serializable(
                "crosscheck_model_request",
                "Crosscheck reviewer request",
                activity_context.clone(),
                &ReviewerInput {
                    system_prompt: prompt::CROSSCHECK_REVIEWER_SYSTEM_PROMPT,
                    user_prompt_prefix: "Please review the following agent output:\n\n---\n\n",
                    agent_output: &agent_output,
                    max_output_tokens: REVIEWER_MAX_OUTPUT_TOKENS,
                },
            );
        }
        let response = Self::invoke_reviewer_inner(agent_output, model_id, provider_config).await;
        #[cfg(not(target_family = "wasm"))]
        if crate::ai::activity::is_recording() {
            activity_context["elapsed_ms"] =
                serde_json::json!(started_at.elapsed().as_millis() as u64);
            // These compatibility helpers return text only, with no usage report.
            activity_context["usage_reported"] = serde_json::json!(false);
            match &response {
                Ok(text) => crate::ai::activity::record_serializable(
                    "crosscheck_model_response",
                    "Crosscheck reviewer response",
                    activity_context,
                    text,
                ),
                Err(error) => {
                    activity_context["error"] =
                        serde_json::json!(crate::ai::remote_logging::sanitize_error(error));
                    crate::ai::activity::record(
                        "crosscheck_model_error",
                        "Crosscheck reviewer failed",
                        activity_context,
                    );
                }
            }
        }
        response
    }

    async fn invoke_reviewer_inner(
        agent_output: String,
        model_id: String,
        provider_config: ProviderConfig,
    ) -> Result<String, String> {
        match provider_config {
            ProviderConfig::OpenAI(config)
                if config.kind == OpenAIProviderKind::ChatGPTSubscription =>
            {
                let runtime = galaxy_agent_rig::ChatGPTSubscriptionRuntime::new(
                    galaxy_agent_rig::ChatGPTSubscriptionRuntimeConfig {
                        model: config.model.unwrap_or(model_id),
                        reasoning_effort: config.reasoning_effort,
                        max_output_tokens: Some(u64::from(REVIEWER_MAX_OUTPUT_TOKENS)),
                    },
                )
                .with_credential_resolver(crate::ai::chatgpt_auth::credential_resolver());
                runtime
                    .complete_text(
                        prompt::CROSSCHECK_REVIEWER_SYSTEM_PROMPT.to_string(),
                        format!(
                            "Please review the following agent output:\n\n---\n\n{agent_output}"
                        ),
                    )
                    .await
            }
            ProviderConfig::OpenAI(config) => {
                Self::invoke_via_openai(agent_output, model_id, config).await
            }
            ProviderConfig::Bedrock(_config) => {
                // For Bedrock, we route through the OpenAI-compatible interface
                // if available, otherwise use a simplified Bedrock invocation.
                // Since most setups use LiteLLM or have OpenAI-compatible proxies,
                // we log a warning and attempt Bedrock converse API.
                Self::invoke_via_bedrock(agent_output, model_id, _config).await
            }
            ProviderConfig::None => Err(
                "No AI provider configured. Enable either OpenAI/LiteLLM or Bedrock in settings."
                    .to_string(),
            ),
        }
    }

    /// Invokes the reviewer via an OpenAI-compatible endpoint (non-streaming).
    async fn invoke_via_openai(
        agent_output: String,
        model_id: String,
        config: OpenAIClientConfig,
    ) -> Result<String, String> {
        let client = OpenAIClient::from_config(config.clone());

        let effective_model = config.model.unwrap_or(model_id);

        let mut request_body = serde_json::json!({
            "model": effective_model,
            "messages": [
                {
                    "role": "system",
                    "content": prompt::CROSSCHECK_REVIEWER_SYSTEM_PROMPT
                },
                {
                    "role": "user",
                    "content": format!(
                        "Please review the following agent output:\n\n---\n\n{agent_output}"
                    )
                }
            ],
            "max_tokens": REVIEWER_MAX_OUTPUT_TOKENS,
            "stream": false
        });

        if let Some(effort) = config.reasoning_effort {
            request_body["reasoning_effort"] = effort.into();
        }

        // Use a non-streaming request by calling the same endpoint but with stream: false
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));

        let http = reqwest::Client::new();
        let mut request_builder = http.post(&url).json(&request_body);

        if let Some(ref key) = config.api_key {
            request_builder = request_builder.header("Authorization", format!("Bearer {key}"));
        }

        let response = request_builder
            .send()
            .await
            .map_err(|e| format!("Crosscheck reviewer request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Crosscheck reviewer returned HTTP {status}: {body}"
            ));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse reviewer response: {e}"))?;

        // Extract the assistant's message content from the OpenAI response format
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        if content.is_empty() {
            return Err("Crosscheck reviewer returned an empty response.".to_string());
        }

        log::info!(
            "[crosscheck] Reviewer response (len={}): {}",
            content.len(),
            &content[..content.len().min(200)]
        );

        Ok(content)
    }

    /// Invokes the reviewer via AWS Bedrock Converse API.
    async fn invoke_via_bedrock(
        agent_output: String,
        model_id: String,
        config: crate::ai::provider::client::BedrockClientConfig,
    ) -> Result<String, String> {
        use crate::ai::provider::client::BedrockClient;
        use crate::ai::provider::types::{ConversationMessage, MessageContent, MessageRole};

        let cross_region_inference = config.cross_region_inference;
        let client = BedrockClient::from_config(config)
            .await
            .map_err(|e| format!("Failed to create Bedrock client: {e}"))?;

        let messages = vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(format!(
                "Please review the following agent output:\n\n---\n\n{agent_output}"
            )),
        }];

        let system_prompt = Some(prompt::CROSSCHECK_REVIEWER_SYSTEM_PROMPT.to_string());

        let (response_text, _input_tokens, _output_tokens) = client
            .converse_collect(
                &model_id,
                messages,
                system_prompt,
                REVIEWER_MAX_OUTPUT_TOKENS as i32,
                cross_region_inference,
            )
            .await
            .map_err(|e| format!("Bedrock reviewer invocation failed: {e}"))?;

        if response_text.is_empty() {
            return Err("Crosscheck reviewer returned an empty response.".to_string());
        }

        log::info!(
            "[crosscheck] Bedrock reviewer response (len={}): {}",
            response_text.len(),
            &response_text[..response_text.len().min(200)]
        );

        Ok(response_text)
    }

    fn handle_reviewer_response(
        &mut self,
        conversation_id: AIConversationId,
        response: Result<String, String>,
        current_iteration: u32,
        max_iterations: u32,
        ctx: &mut ModelContext<Self>,
    ) {
        let outcome = match response {
            Err(error) => {
                log::error!(
                    "[crosscheck] Reviewer invocation failed for {conversation_id:?}: {error}"
                );
                self.active_reviews.remove(&conversation_id);
                ReviewOutcome::Error { error }
            }
            Ok(text) => {
                if prompt::is_approved(&text) {
                    log::info!(
                        "[crosscheck] Reviewer approved work for conversation {conversation_id:?} at iteration {current_iteration}"
                    );
                    self.active_reviews.remove(&conversation_id);
                    ReviewOutcome::Approved
                } else if current_iteration >= max_iterations {
                    log::warn!(
                        "[crosscheck] Max iterations ({max_iterations}) reached for conversation {conversation_id:?}; auto-accepting"
                    );
                    self.active_reviews.remove(&conversation_id);
                    ReviewOutcome::MaxIterationsReached {
                        last_feedback: text,
                    }
                } else {
                    log::info!(
                        "[crosscheck] Reviewer has feedback for conversation {conversation_id:?} (iteration {current_iteration}/{max_iterations})"
                    );
                    ReviewOutcome::Feedback { message: text }
                }
            }
        };

        ctx.emit(CrosscheckReviewerEvent::ReviewCompleted {
            conversation_id,
            outcome,
        });
    }
}
