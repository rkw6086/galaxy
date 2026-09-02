use super::prepare_provider_run;
use crate::ai::agent::api::RequestParams;
use crate::ai::llms::LLMId;
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::provider::ProviderConfig;
use crate::settings::OpenAIProviderKind;

fn openai_config(model: &str) -> OpenAIClientConfig {
    OpenAIClientConfig {
        kind: OpenAIProviderKind::LiteLLM,
        base_url: "http://localhost:4000/v1".to_string(),
        api_key: None,
        project_id: None,
        location: None,
        model: Some(model.to_string()),
        reasoning_effort: None,
        max_input_tokens: Some(128_000),
        max_output_tokens: Some(8_192),
        supports_system_messages: true,
    }
}

#[tokio::test]
async fn missing_cli_provider_route_falls_back_to_base_provider_profile() {
    let mut params = RequestParams::new_for_test();
    params.model = LLMId::from("base-selection");
    params.cli_agent_model = LLMId::from("unroutable-cli-selection");

    let prepared = prepare_provider_run(
        ProviderConfig::OpenAI(openai_config("base-provider-model")),
        ProviderConfig::None,
        params,
        Vec::new(),
    )
    .await
    .unwrap();
    let cli_profile = prepared
        .cli_monitor_profile
        .expect("base provider should supply the CLI monitor fallback");

    assert_eq!(cli_profile.request.model.as_str(), "base-provider-model");
    assert!(cli_profile
        .request
        .system_prompt
        .as_deref()
        .is_some_and(|prompt| prompt.contains("## Running Command Monitor")));
    assert!(cli_profile
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "write_to_long_running_shell_command"));
    assert!(cli_profile
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "read_shell_command_output"));
}
