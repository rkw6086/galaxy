use galaxy_agent_core::ToolCall;

use super::{prepare_provider_run, ProviderActionContext};
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

#[test]
fn subscription_context_budget_uses_catalog_limit_and_clamps_stale_overrides() {
    let mut config = openai_config("subscription-model");
    config.kind = OpenAIProviderKind::ChatGPTSubscription;
    config.max_input_tokens = Some(272_000);
    let provider = ProviderConfig::OpenAI(config);
    for (selected, expected) in [
        (None, 272_000),
        (Some(872_000), 272_000),
        (Some(128_000), 128_000),
    ] {
        assert_eq!(
            super::provider_context_window_tokens(&provider, "subscription-model", selected),
            Some(expected)
        );
    }
}

#[test]
fn tool_titles_are_optional_and_normalized_for_display() {
    let context = ProviderActionContext::new_for_test("task");
    let mut call = ToolCall {
        id: "call".to_string(),
        name: "read_files".to_string(),
        arguments: serde_json::json!({"_galaxy_tool_title": "  Read\n cache   configuration "}),
    };
    assert_eq!(
        context.tool_call_title(&call).as_deref(),
        Some("Read cache configuration")
    );
    call.arguments = serde_json::json!({"_galaxy_tool_title": "界".repeat(200)});
    assert_eq!(context.tool_call_title(&call).unwrap().chars().count(), 120);
    call.arguments = serde_json::json!({"_galaxy_tool_title": "  "});
    assert_eq!(context.tool_call_title(&call), None);
    call.arguments = serde_json::json!({});
    assert_eq!(context.tool_call_title(&call), None);
}
