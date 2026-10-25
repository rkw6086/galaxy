use galaxy_agent_core::{ToolCall, TurnRequest};

use super::{prepare_provider_run, ProviderActionContext};
use crate::ai::agent::api::RequestParams;
use crate::ai::llms::LLMId;
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::provider::ProviderConfig;
use crate::settings::OpenAIProviderKind;

fn openai_config(model: &str) -> OpenAIClientConfig {
    OpenAIClientConfig {
        kind: OpenAIProviderKind::LiteLLM,
        connection_identity: Some("LiteLLM test connection".to_string()),
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
    super::restore_provider_config(&cli_profile.request, |model| {
        assert_eq!(model, "base-selection");
        ProviderConfig::OpenAI(openai_config("base-provider-model"))
    })
    .unwrap();
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

#[tokio::test]
async fn selected_computer_use_model_gets_its_own_profile() {
    let mut params = RequestParams::new_for_test();
    params.model = LLMId::from("base-selection");
    params.computer_use_model = LLMId::from("computer-selection");
    params.computer_use_enabled = true;

    let prepared = prepare_provider_run(
        ProviderConfig::OpenAI(openai_config("base-provider-model")),
        ProviderConfig::None,
        ProviderConfig::OpenAI(openai_config("computer-provider-model")),
        params,
        Vec::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        prepared
            .computer_use_profile
            .expect("selected computer-use model should have a profile")
            .request
            .model
            .as_str(),
        "computer-provider-model"
    );
}

#[tokio::test]
async fn automatic_and_same_model_computer_use_keep_a_dedicated_profile() {
    for selected in ["computer-use-agent-auto", "base-selection", "unroutable-model"] {
        let mut params = RequestParams::new_for_test();
        params.model = LLMId::from("base-selection");
        params.computer_use_model = LLMId::from(selected);
        params.computer_use_enabled = true;
        let prepared = prepare_provider_run(
            ProviderConfig::OpenAI(openai_config("base-provider-model")),
            ProviderConfig::None,
            ProviderConfig::None,
            params,
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(prepared
            .base_profile
            .request
            .tools
            .iter()
            .any(|tool| tool.name == "request_computer_use"));
        assert!(!prepared
            .base_profile
            .request
            .tools
            .iter()
            .any(|tool| tool.name == "use_computer"));
        let computer = prepared.computer_use_profile.unwrap();
        assert_eq!(computer.request.model.as_str(), "base-provider-model");
        super::restore_provider_config(&computer.request, |model| {
            assert_eq!(model, "base-selection");
            ProviderConfig::OpenAI(openai_config("base-provider-model"))
        })
        .unwrap();
        assert!(computer
            .request
            .tools
            .iter()
            .any(|tool| tool.name == "use_computer"));
        assert!(!computer
            .request
            .tools
            .iter()
            .any(|tool| tool.name == "request_computer_use"));
        assert!(computer
            .request
            .system_prompt
            .unwrap()
            .contains("Inspect the latest returned screenshot before acting"));
    }
}

#[tokio::test]
async fn restored_profiles_keep_their_selection_and_effort_with_refreshed_credentials() {
    let mut params = RequestParams::new_for_test();
    params.model = LLMId::from("base-selection::reasoning::high");
    params.cli_agent_model = LLMId::from("cli-selection::reasoning::low");
    params.computer_use_model = LLMId::from("computer-selection::reasoning::medium");
    params.computer_use_enabled = true;
    let config = |model, effort: &str| {
        let mut config = openai_config(model);
        config.reasoning_effort = Some(effort.to_string());
        config.api_key = Some("old-secret".to_string());
        ProviderConfig::OpenAI(config)
    };
    let prepared = prepare_provider_run(
        config("base-provider-model", "high"),
        config("cli-provider-model", "low"),
        config("computer-provider-model", "medium"),
        params,
        Vec::new(),
    )
    .await
    .unwrap();

    for (profile, selection, model, effort) in [
        (
            prepared.base_profile,
            "base-selection::reasoning::high",
            "base-provider-model",
            "high",
        ),
        (
            prepared.cli_monitor_profile.unwrap(),
            "cli-selection::reasoning::low",
            "cli-provider-model",
            "low",
        ),
        (
            prepared.computer_use_profile.unwrap(),
            "computer-selection::reasoning::medium",
            "computer-provider-model",
            "medium",
        ),
    ] {
        let serialized = serde_json::to_string(&profile.request).unwrap();
        assert!(!serialized.contains("old-secret"));
        assert!(!serialized.contains("http://localhost:4000"));
        let request: TurnRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(request.model.as_str(), model);
        let restored = super::restore_provider_config(&request, |id| {
            assert_eq!(id, selection);
            let mut current = openai_config(model);
            current.api_key = Some("new-secret".to_string());
            current.reasoning_effort = Some("changed-after-checkpoint".to_string());
            ProviderConfig::OpenAI(current)
        })
        .unwrap();
        let ProviderConfig::OpenAI(restored) = restored else {
            panic!("expected the original provider");
        };
        assert_eq!(restored.reasoning_effort.as_deref(), Some(effort));
        assert_eq!(restored.api_key.as_deref(), Some("new-secret"));
    }
}

#[test]
fn restored_provider_default_clears_a_later_effort_setting() {
    let mut request = TurnRequest::new("wire-model", Vec::new());
    super::remember_provider_selection(
        &mut request,
        "model-selection".to_string(),
        &ProviderConfig::OpenAI(openai_config("wire-model")),
    )
    .unwrap();
    let ProviderConfig::OpenAI(restored) = super::restore_provider_config(&request, |_| {
        let mut current = openai_config("wire-model");
        current.reasoning_effort = Some("high".to_string());
        ProviderConfig::OpenAI(current)
    })
    .unwrap() else {
        panic!("expected an OpenAI-compatible provider");
    };
    assert_eq!(restored.reasoning_effort, None);
}

#[test]
fn restored_selection_rejects_unavailable_changed_and_malformed_routes() {
    let mut request = TurnRequest::new("wire-model", Vec::new());
    super::remember_provider_selection(
        &mut request,
        "model-selection".to_string(),
        &ProviderConfig::OpenAI(openai_config("wire-model")),
    )
    .unwrap();
    assert!(super::restore_provider_config(&request, |_| ProviderConfig::None).is_err());
    assert!(super::restore_provider_config(&request, |_| {
        let mut current = openai_config("wire-model");
        current.base_url = "https://another-provider.example/v1".to_string();
        ProviderConfig::OpenAI(current)
    })
    .is_err());
    assert!(super::restore_provider_config(&request, |_| {
        let mut current = openai_config("wire-model");
        current.connection_identity = Some("different-account-at-same-endpoint".to_string());
        ProviderConfig::OpenAI(current)
    })
    .is_err());
    assert!(super::restore_provider_config(&request, |_| {
        ProviderConfig::OpenAI(openai_config("different-wire-model"))
    })
    .is_err());

    request
        .metadata
        .get_mut(super::PROVIDER_SELECTION_METADATA)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("reasoning");
    assert!(super::restore_provider_config(&request, |_| {
        panic!("malformed selection must fail before looking up a provider")
    })
    .is_err());
}

#[test]
fn legacy_request_without_selection_metadata_keeps_raw_model_lookup() {
    let request = TurnRequest::new("legacy-model", Vec::new());
    let ProviderConfig::OpenAI(restored) = super::restore_provider_config(&request, |model| {
        assert_eq!(model, "legacy-model");
        ProviderConfig::OpenAI(openai_config("legacy-model"))
    })
    .unwrap() else {
        panic!("expected legacy provider lookup to succeed");
    };
    assert_eq!(restored.model.as_deref(), Some("legacy-model"));
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
