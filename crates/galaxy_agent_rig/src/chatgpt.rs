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

/// The information a user needs to complete ChatGPT's device authorization flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGPTDeviceCode {
    pub verification_uri: String,
    pub user_code: String,
}

/// Small application-facing wrapper around Rig's native ChatGPT OAuth client.
///
/// Keeping the Rig auth type behind this wrapper lets Galaxy present device-code
/// instructions without depending on Rig's private auth module.
pub struct ChatGPTSubscriptionClient {
    client: chatgpt::Client,
}

impl ChatGPTSubscriptionClient {
    pub fn with_device_code_handler<F>(handler: F) -> Result<Self, String>
    where
        F: Fn(ChatGPTDeviceCode) + Send + Sync + 'static,
    {
        let client = chatgpt::Client::builder()
            .oauth()
            .on_device_code(move |prompt| {
                handler(ChatGPTDeviceCode {
                    verification_uri: prompt.verification_uri,
                    user_code: prompt.user_code,
                });
            })
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { client })
    }

    pub async fn authorize(&self) -> Result<(), String> {
        self.client
            .authorize()
            .await
            .map_err(|error| error.to_string())
    }
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
        let mut builder = chatgpt::Client::builder().oauth();
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
mod tests {
    use rig_core::client::CompletionClient;
    use rig_core::completion::{AssistantContent, CompletionModel, Message};
    use rig_core::message::{ToolResultContent, UserContent};
    use rig_core::providers::chatgpt::ChatGPTAuth;
    use rig_core::test_utils::RecordingHttpClient;

    use super::*;

    const COMPLETED_RESPONSE: &str = r#"data: {"type":"response.output_text.delta","delta":"ok"}
data: {"type":"response.completed","response":{"id":"resp_test","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-5.3-codex","usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2},"output":[{"type":"message","id":"msg_test","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"ok"}]}],"tools":[]}}
data: [DONE]"#;

    #[test]
    fn runtime_descriptor_identifies_chatgpt_subscription() {
        let runtime = ChatGPTSubscriptionRuntime::new(ChatGPTSubscriptionRuntimeConfig {
            model: "gpt-5.3-codex".to_string(),
            reasoning_effort: Some("high".to_string()),
            max_output_tokens: None,
            auth_file: None,
        });

        assert_eq!(
            runtime.descriptor().id,
            "rig-chatgpt-subscription:gpt-5.3-codex"
        );
    }

    #[test]
    fn reasoning_effort_is_encoded_for_the_responses_request() {
        let request = TurnRequest::new(
            "gpt-5.4".to_string(),
            vec![ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text("hello".to_string()),
            }],
        );
        let request = build_completion_request(
            request,
            None,
            true,
            false,
            reasoning_additional_params(Some("xhigh")),
        )
        .expect("request should convert");

        assert_eq!(
            request.additional_params,
            reasoning_additional_params(Some("xhigh"))
        );

        assert_eq!(
            reasoning_additional_params(Some("ultra")),
            Some(serde_json::json!({
                "reasoning": { "effort": "max" }
            }))
        );
    }

    #[tokio::test]
    async fn chatgpt_follow_up_request_preserves_responses_call_ids() {
        let http_client = RecordingHttpClient::new(COMPLETED_RESPONSE);
        let client = chatgpt::Client::builder()
            .api_key(ChatGPTAuth::AccessToken {
                access_token: "test-token".to_string(),
                account_id: None,
            })
            .http_client(http_client.clone())
            .build()
            .expect("client should build");
        let model = client.completion_model("gpt-5.3-codex");
        let assistant_tool_call = AssistantContent::tool_call_with_call_id(
            "fc_native_1",
            "call_native_1".to_string(),
            "read_files",
            serde_json::json!({"files": ["Cargo.toml"]}),
        );
        let tool_result = UserContent::tool_result_with_call_id(
            "fc_native_1",
            "call_native_1".to_string(),
            rig_core::OneOrMany::one(ToolResultContent::text("contents")),
        );
        let chat_history = rig_core::OneOrMany::many(vec![
            Message::Assistant {
                id: None,
                content: rig_core::OneOrMany::one(assistant_tool_call),
            },
            Message::User {
                content: rig_core::OneOrMany::one(tool_result),
            },
            Message::user("Continue."),
        ])
        .expect("history should contain messages");

        model
            .completion(rig_core::completion::CompletionRequest {
                model: Some("gpt-5.3-codex".to_string()),
                preamble: None,
                chat_history,
                documents: Vec::new(),
                tools: Vec::new(),
                temperature: None,
                max_tokens: None,
                tool_choice: None,
                additional_params: None,
                output_schema: None,
                record_telemetry_content: false,
            })
            .await
            .expect("request should reach the mocked provider");

        let requests = http_client.requests();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let input = body["input"].as_array().expect("input should be an array");
        assert_eq!(input[0]["call_id"], "call_native_1");
        assert_eq!(input[1]["call_id"], "call_native_1");
    }
}
