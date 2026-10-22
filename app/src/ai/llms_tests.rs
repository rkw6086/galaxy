use serde_json::json;
use warpui::App;

use super::*;
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::auth::auth_manager::AuthManager;
use crate::auth::AuthStateProvider;
use crate::cloud_object::model::persistence::CloudModel;
use crate::network::NetworkStatus;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::server_api::ServerApiProvider;
use crate::server::sync_queue::SyncQueue;
use crate::settings::{
    AcpAgentSettings, AcpConfigOptionSettings, AcpConfigValueSettings, AcpProviderConfig,
    BedrockModelConfig, OpenAIModelConfig, OpenAIProviderConfig, OpenAIProviderKind,
};
use crate::test_util::settings::initialize_settings_for_tests;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::LaunchMode;

// -- DisableReason::should_clear_preference tests --

#[test]
fn should_clear_preference_admin_disabled() {
    // AdminDisabled always clears, regardless of BYOK status.
    assert!(DisableReason::AdminDisabled.should_clear_preference(false));
    assert!(DisableReason::AdminDisabled.should_clear_preference(true));
}

#[test]
fn should_clear_preference_unavailable() {
    assert!(DisableReason::Unavailable.should_clear_preference(false));
    assert!(DisableReason::Unavailable.should_clear_preference(true));
}

#[test]
fn should_not_clear_preference_out_of_requests() {
    // Transient — never clears.
    assert!(!DisableReason::OutOfRequests.should_clear_preference(false));
    assert!(!DisableReason::OutOfRequests.should_clear_preference(true));
}

#[test]
fn should_not_clear_preference_provider_outage() {
    // Transient — never clears.
    assert!(!DisableReason::ProviderOutage.should_clear_preference(false));
    assert!(!DisableReason::ProviderOutage.should_clear_preference(true));
}

#[test]
fn should_clear_preference_requires_upgrade_without_byok() {
    // No BYOK key → server will reject → clear.
    assert!(DisableReason::RequiresUpgrade.should_clear_preference(false));
}

#[test]
fn should_not_clear_preference_requires_upgrade_with_byok() {
    // BYOK key present → server allows → keep.
    assert!(!DisableReason::RequiresUpgrade.should_clear_preference(true));
}

#[test]
fn llm_info_deserializes_without_base_model_name() {
    let raw = r#"{
            "display_name": "gpt-4o",
            "id": "gpt-4o",
            "usage_metadata": {
                "request_multiplier": 1,
                "credit_multiplier": null
            },
            "description": null,
            "disable_reason": null,
            "vision_supported": false,
            "spec": null,
            "provider": "Unknown"
        }"#;

    let info: LLMInfo = serde_json::from_str(raw).expect("should deserialize");
    assert_eq!(info.display_name, "gpt-4o");
    assert_eq!(info.base_model_name, "gpt-4o");
}

#[test]
fn llm_info_deserializes_host_configs_as_vec() {
    // Wire format from server: host_configs is a Vec
    let raw = r#"{
            "display_name": "gpt-4o",
            "id": "gpt-4o",
            "usage_metadata": { "request_multiplier": 1, "credit_multiplier": null },
            "provider": "OpenAI",
            "host_configs": [
                { "enabled": true, "model_routing_host": "DirectApi" },
                { "enabled": false, "model_routing_host": "AwsBedrock" }
            ]
        }"#;

    let info: LLMInfo = serde_json::from_str(raw).expect("should deserialize vec format");
    assert_eq!(info.display_name, "gpt-4o");
    assert_eq!(info.host_configs.len(), 2);
    assert!(
        info.host_configs
            .get(&LLMModelHost::DirectApi)
            .unwrap()
            .enabled
    );
    assert!(
        !info
            .host_configs
            .get(&LLMModelHost::AwsBedrock)
            .unwrap()
            .enabled
    );
}

#[test]
fn llm_info_round_trip_serializes_and_deserializes() {
    // Start with wire format (Vec)
    let wire_json = r#"{
            "display_name": "claude-3",
            "base_model_name": "claude-3",
            "id": "claude-3",
            "usage_metadata": { "request_multiplier": 2, "credit_multiplier": 1.5 },
            "description": "A powerful model",
            "vision_supported": true,
            "provider": "Anthropic",
            "host_configs": [
                { "enabled": true, "model_routing_host": "DirectApi" }
            ]
        }"#;

    // Deserialize from wire format
    let info: LLMInfo = serde_json::from_str(wire_json).expect("should deserialize");

    // Serialize (produces HashMap format)
    let serialized = serde_json::to_string(&info).expect("should serialize");

    // Deserialize again (from HashMap format)
    let round_tripped: LLMInfo =
        serde_json::from_str(&serialized).expect("should deserialize after round trip");

    assert_eq!(info, round_tripped);
}

fn openai_model(model_id: &str) -> OpenAIModelConfig {
    OpenAIModelConfig {
        model_id: model_id.to_string(),
        display_name: model_id.to_string(),
        vision_supported: false,
        context_size: 200_000,
        max_input_tokens: None,
        default_context_size: None,
        max_output_tokens: None,
        provider: None,
        supports_system_messages: None,
        capability_overrides: std::collections::HashMap::new(),
        reasoning_efforts: Vec::new(),
        enabled: true,
    }
}

#[test]
fn live_bedrock_models_are_not_hidden_by_missing_catalog_metadata() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let id = "future.new-live-model";
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings.bedrock_enabled.set_value(true, ctx).unwrap();
            settings.bedrock_models.set_value(
                vec![bedrock_model("future.other-model"), bedrock_model(id)], ctx,
            ).unwrap();
        });
        let mut preferences = empty_preferences();
        preferences.models_by_feature.agent_mode.default_id = id.into();
        preferences.models_by_feature.coding.default_id = id.into();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids.insert(id.to_owned());
            preferences.bedrock_verified_model_ids.insert("future.other-model".to_owned());
            preferences.inject_bedrock_models(ctx);
        });
        assert!(preferences.models_by_feature.agent_mode.choices.iter().any(|model| model.id.as_str() == id));
        assert_eq!(preferences.models_by_feature.agent_mode.default_id.as_str(), id);
        assert_eq!(preferences.models_by_feature.coding.default_id.as_str(), id);
    });
}

#[test]
fn newer_bedrock_discovery_invalidates_older_results() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let preferences = app.add_model(|_| empty_preferences());
        let models = [bedrock_model("future.new-live-model")];
        preferences.update(&mut app, |preferences, ctx| {
            let first = preferences.begin_bedrock_verification();
            let latest = preferences.begin_bedrock_verification();
            let config = super::bedrock_discovery_config(AISettings::as_ref(ctx));
            assert!(!preferences.record_bedrock_verification(first, config.clone(), &models, ctx));
            assert!(preferences.bedrock_verified_model_ids.is_empty());
            assert!(preferences.record_bedrock_verification(latest, config, &models, ctx));
            assert!(preferences.bedrock_verified_model_ids.contains("future.new-live-model"));
        });
    });
}

fn bedrock_model(model_id: &str) -> BedrockModelConfig {
    BedrockModelConfig {
        enabled: true,
        access_verified: true,
        model_id: model_id.to_string(),
        display_name: model_id.to_string(),
        vision_supported: false,
        ..Default::default()
    }
}

fn acp_select_option(
    id: &str,
    category: &str,
    current_value: &str,
    values: &[(&str, &str)],
) -> AcpConfigOptionSettings {
    AcpConfigOptionSettings {
        id: id.to_owned(),
        name: id.to_owned(),
        description: None,
        category: Some(category.to_owned()),
        kind: "select".to_owned(),
        current_value: serde_json::json!(current_value),
        options: values
            .iter()
            .map(|(value, name)| AcpConfigValueSettings {
                value: serde_json::json!(value),
                name: (*name).to_owned(),
                description: None,
            })
            .collect(),
    }
}

fn acp_agent(
    id: &str,
    name: &str,
    config_options: Vec<AcpConfigOptionSettings>,
) -> AcpAgentSettings {
    AcpAgentSettings {
        id: id.to_owned(),
        name: name.to_owned(),
        version: None,
        description: None,
        icon_url: None,
        capabilities: Vec::new(),
        config_options,
        discovery_timestamp: None,
        discovery_source: None,
        discovery_error: None,
    }
}

fn empty_preferences() -> LLMPreferences {
    // Production defaults include a UI placeholder; inventory tests start empty.
    let mut models_by_feature = ModelsByFeature::default();
    models_by_feature.agent_mode.choices.clear();
    models_by_feature.coding.choices.clear();
    if let Some(cli) = &mut models_by_feature.cli_agent {
        cli.choices.clear();
    }
    if let Some(computer_use) = &mut models_by_feature.computer_use {
        computer_use.choices.clear();
    }
    LLMPreferences {
        models_by_feature,
        last_update: None,
        base_llm_for_terminal_view: HashMap::new(),
        custom_llms: Vec::new(),
        custom_model_routers: Vec::new(),
        openai_provider_routing: HashMap::new(),
        fetched_openai_models: Vec::new(),
        catalog_connections: Vec::new(),
        catalog_refresh_generations: HashMap::new(),
        chatgpt_subscription_models_refresh_in_flight: false,
        bedrock_models_refresh_in_flight: false,
        bedrock_verification_generation: 0,
        bedrock_verified_config: None,
        bedrock_verified_model_ids: HashSet::new(),
        acp_selections: HashMap::new(),
    }
}

#[test]
fn computer_use_picker_includes_vision_models_without_duplicate_entries() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let mut preferences = empty_preferences();
        let auto = default_computer_use_llms().choices[0].clone();
        let mut visual = auto.clone();
        visual.id = "visual-model".into();
        visual.display_name = "Visual model".to_owned();
        visual.vision_supported = true;
        let mut text_only = auto.clone();
        text_only.id = "text-only-model".into();
        text_only.display_name = "Text-only model".to_owned();
        preferences.models_by_feature.agent_mode.choices = vec![visual.clone(), text_only];
        preferences.models_by_feature.computer_use = Some(AvailableLLMs {
            default_id: "computer-use-agent-auto".into(),
            choices: vec![auto, visual],
            preferred_codex_model_id: None,
        });

        app.read(|ctx| {
            let ids = preferences
                .get_computer_use_llm_choices(ctx)
                .map(|info| info.id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(ids, ["computer-use-agent-auto", "visual-model"]);
        });
    });
}

#[test]
fn bedrock_and_openai_provider_models_coexist_in_picker() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .openai_enabled
                .set_value(true, ctx)
                .expect("OpenAI setting should update");
            settings
                .openai_providers
                .set_value(
                    vec![OpenAIProviderConfig {
                        kind: OpenAIProviderKind::OpenAI,
                        enabled: true,
                        name: "OpenAI Test".to_owned(),
                        base_url: "https://api.openai.test/v1".to_owned(),
                        api_key: Some("test-key".to_owned()),
                        project_id: None,
                        location: None,
                        models: vec![openai_model("gpt-test")],
                    }],
                    ctx,
                )
                .expect("OpenAI providers should update");
            settings
                .bedrock_enabled
                .set_value(true, ctx)
                .expect("Bedrock setting should update");
            settings
                .bedrock_models
                .set_value(vec![bedrock_model("anthropic.claude-test")], ctx)
                .expect("Bedrock models should update");
        });

        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.inject_openai_models(ctx);
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids = AISettings::as_ref(ctx)
                .bedrock_models
                .value()
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            preferences.inject_bedrock_models(ctx);
        });

        let agent_models = &preferences.models_by_feature.agent_mode.choices;
        assert!(
            agent_models
                .iter()
                .any(|model| model.id.as_str() == "gpt-test"
                    && model.provider == LLMProvider::LiteLLM)
        );
        assert!(agent_models.iter().any(|model| {
            model.id.as_str() == "anthropic.claude-test" && model.provider == LLMProvider::Bedrock
        }));
        assert!(preferences
            .openai_client_config_for_model("gpt-test")
            .is_some());

        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids = AISettings::as_ref(ctx)
                .bedrock_models
                .value()
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            preferences.inject_bedrock_models(ctx);
            preferences.inject_openai_models(ctx);
        });

        let agent_models = &preferences.models_by_feature.agent_mode.choices;
        assert!(
            agent_models
                .iter()
                .any(|model| model.id.as_str() == "gpt-test"
                    && model.provider == LLMProvider::LiteLLM)
        );
        assert!(agent_models.iter().any(|model| {
            model.id.as_str() == "anthropic.claude-test" && model.provider == LLMProvider::Bedrock
        }));
        assert!(preferences
            .openai_client_config_for_model("gpt-test")
            .is_some());
    });
}

#[test]
fn provider_discovery_preserves_local_model_overrides() {
    let mut existing = openai_model("codex-gpt-5.6-sol-xhigh");
    existing.display_name = "My Codex".to_string();
    existing.context_size = 100_000;
    existing.provider = Some("openai".to_string());
    // Even stale or incorrect endpoint metadata must not opt ChatGPT-backed
    // Codex models back into the system role.
    existing.supports_system_messages = Some(true);

    let mut discovered = openai_model("codex-gpt-5.6-sol-xhigh");
    discovered.display_name = "Codex from endpoint".to_string();
    discovered.context_size = 400_000;
    discovered.max_output_tokens = Some(32_000);
    discovered.supports_system_messages = Some(true);

    let merged = merge_discovered_provider_models(&[existing], vec![discovered]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].display_name, "My Codex");
    assert_eq!(merged[0].context_size, 400_000);
    assert_eq!(merged[0].max_output_tokens, Some(32_000));
    assert_eq!(merged[0].supports_system_messages, Some(false));
    assert_eq!(merged[0].provider.as_deref(), Some("openai"));
}

#[test]
fn codex_models_reject_system_messages_even_with_stale_true_metadata() {
    let mut model = openai_model("codex-gpt-5.6-sol-xhigh");
    model.supports_system_messages = Some(true);

    assert!(!model.supports_system_messages());
}

#[test]
fn acp_models_are_injected_only_while_acp_is_enabled() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .acp_enabled
                .set_value(true, ctx)
                .expect("ACP setting should update");
            settings
                .acp_agents
                .set_value(
                    vec![AcpAgentSettings {
                        id: "codex".to_owned(),
                        name: "Codex".to_owned(),
                        version: None,
                        description: None,
                        icon_url: None,
                        capabilities: Vec::new(),
                        config_options: vec![AcpConfigOptionSettings {
                            id: "model".to_owned(),
                            name: "Model".to_owned(),
                            description: None,
                            category: Some("model".to_owned()),
                            kind: "select".to_owned(),
                            current_value: serde_json::json!("gpt-test"),
                            options: vec![AcpConfigValueSettings {
                                value: serde_json::json!("gpt-test"),
                                name: "GPT Test".to_owned(),
                                description: None,
                            }],
                        }],
                        discovery_timestamp: None,
                        discovery_source: None,
                        discovery_error: None,
                    }],
                    ctx,
                )
                .expect("ACP agents should update");
        });

        let mut preferences = LLMPreferences {
            models_by_feature: ModelsByFeature::default(),
            last_update: None,
            base_llm_for_terminal_view: HashMap::new(),
            custom_llms: Vec::new(),
            custom_model_routers: Vec::new(),
            openai_provider_routing: HashMap::new(),
            fetched_openai_models: Vec::new(),
            catalog_connections: Vec::new(),
            catalog_refresh_generations: HashMap::new(),
            chatgpt_subscription_models_refresh_in_flight: false,
            bedrock_models_refresh_in_flight: false,
            bedrock_verification_generation: 0,
            bedrock_verified_config: None,
            bedrock_verified_model_ids: HashSet::new(),
            acp_selections: HashMap::new(),
        };
        app.read(|ctx| preferences.inject_acp_models(ctx));
        app.read(|ctx| preferences.inject_acp_models(ctx));

        assert_eq!(preferences.acp_selections.len(), 1);
        assert_eq!(
            preferences
                .models_by_feature
                .agent_mode
                .choices
                .iter()
                .filter(|model| model.id.as_str().starts_with("acp:"))
                .count(),
            1
        );

        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .acp_enabled
                .set_value(false, ctx)
                .expect("ACP setting should update");
        });
        app.read(|ctx| preferences.inject_acp_models(ctx));

        assert!(preferences.acp_selections.is_empty());
        assert!(preferences
            .models_by_feature
            .agent_mode
            .choices
            .iter()
            .all(|model| !model.id.as_str().starts_with("acp:")));
    });
}

#[test]
fn acp_is_selectable_before_protocol_model_discovery_completes() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .acp_enabled
                .set_value(true, ctx)
                .expect("ACP setting should update");
            settings
                .acp_agent_id
                .set_value("opencode".to_owned(), ctx)
                .expect("ACP agent should update");
            settings
                .bedrock_enabled
                .set_value(false, ctx)
                .expect("Bedrock setting should update");
            settings
                .acp_agents
                .set_value(Vec::new(), ctx)
                .expect("ACP discovery cache should update");
        });

        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids = AISettings::as_ref(ctx)
                .bedrock_models
                .value()
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            preferences.inject_bedrock_models(ctx);
            preferences.inject_acp_models(ctx);
        });

        let models = &preferences.models_by_feature.agent_mode.choices;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].display_name, "OpenCode");
        assert_eq!(models[0].provider, LLMProvider::Acp);
        assert_eq!(
            preferences
                .acp_selections
                .get(&models[0].id)
                .map(|selection| selection.agent_id.as_str()),
            Some("opencode")
        );
        assert!(preferences.has_any_provider_models());
    });
}

#[test]
fn acp_models_expand_reasoning_levels_for_only_the_configured_agent() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .acp_enabled
                .set_value(true, ctx)
                .expect("ACP setting should update");
            settings
                .acp_agent_id
                .set_value("opencode".to_owned(), ctx)
                .expect("ACP agent should update");
            settings
                .bedrock_enabled
                .set_value(false, ctx)
                .expect("Bedrock setting should update");
            settings
                .acp_agents
                .set_value(
                    vec![
                        acp_agent(
                            "codex",
                            "Codex",
                            vec![acp_select_option(
                                "model",
                                "model",
                                "stale-model",
                                &[("stale-model", "Stale Model")],
                            )],
                        ),
                        acp_agent(
                            "opencode",
                            "OpenCode",
                            vec![
                                acp_select_option(
                                    "model",
                                    "model",
                                    "gpt-test",
                                    &[("gpt-test", "GPT Test")],
                                ),
                                acp_select_option(
                                    "mode",
                                    "mode",
                                    "read-only",
                                    &[("read-only", "Read-only")],
                                ),
                                acp_select_option(
                                    "thought_level",
                                    "thought_level",
                                    "xhigh",
                                    &[("high", "High"), ("xhigh", "Xhigh")],
                                ),
                            ],
                        ),
                    ],
                    ctx,
                )
                .expect("ACP agents should update");
        });

        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids = AISettings::as_ref(ctx)
                .bedrock_models
                .value()
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            preferences.inject_bedrock_models(ctx);
            preferences.inject_acp_models(ctx);
        });

        let models = &preferences.models_by_feature.agent_mode.choices;
        assert_eq!(models.len(), 2);
        assert_eq!(
            models
                .iter()
                .map(|model| model.display_name.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["GPT Test (High) · OpenCode", "GPT Test (Xhigh) · OpenCode"])
        );
        assert!(models.iter().all(|model| {
            model.provider == LLMProvider::Acp && !model.display_name.contains("Read-only")
        }));
        assert!(preferences.acp_selections.values().all(|selection| {
            selection.agent_id == "opencode"
                && selection.config_values.get("mode") == Some(&serde_json::json!("read-only"))
        }));
    });
}

#[test]
fn acp_models_are_scoped_to_each_configured_provider() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let config_options = vec![acp_select_option(
            "model",
            "model",
            "gpt-test",
            &[("gpt-test", "GPT Test")],
        )];
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .acp_enabled
                .set_value(true, ctx)
                .expect("ACP setting should update");
            settings
                .bedrock_enabled
                .set_value(false, ctx)
                .expect("Bedrock setting should update");
            settings
                .acp_providers
                .set_value(
                    vec![
                        AcpProviderConfig {
                            id: "work".to_owned(),
                            enabled: true,
                            name: "Codex Work".to_owned(),
                            agent_id: "codex".to_owned(),
                            command: String::new(),
                            args: Vec::new(),
                            config_options: config_options.clone(),
                        },
                        AcpProviderConfig {
                            id: "personal".to_owned(),
                            enabled: true,
                            name: "Codex Personal".to_owned(),
                            agent_id: "codex".to_owned(),
                            command: String::new(),
                            args: Vec::new(),
                            config_options,
                        },
                    ],
                    ctx,
                )
                .expect("ACP providers should update");
        });

        let mut preferences = empty_preferences();
        app.read(|ctx| preferences.inject_acp_models(ctx));

        let models = preferences
            .models_by_feature
            .agent_mode
            .choices
            .iter()
            .filter(|model| model.provider == LLMProvider::Acp)
            .collect::<Vec<_>>();
        assert_eq!(models.len(), 2);
        assert_eq!(
            models
                .iter()
                .map(|model| model.display_name.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["GPT Test · Codex Work", "GPT Test · Codex Personal"])
        );
        assert_eq!(
            preferences
                .acp_selections
                .values()
                .map(|selection| selection.provider_id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["work", "personal"])
        );
    });
}

#[test]
fn acp_bedrock_models_are_hidden_while_bedrock_is_disabled() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .acp_enabled
                .set_value(true, ctx)
                .expect("ACP setting should update");
            settings
                .acp_agent_id
                .set_value("opencode".to_owned(), ctx)
                .expect("ACP agent should update");
            settings
                .bedrock_enabled
                .set_value(false, ctx)
                .expect("Bedrock setting should update");
            settings
                .acp_agents
                .set_value(
                    vec![acp_agent(
                        "opencode",
                        "OpenCode",
                        vec![acp_select_option(
                            "model",
                            "model",
                            "openai/gpt-test",
                            &[
                                ("amazon-bedrock/claude-test", "Bedrock Claude"),
                                ("openai/gpt-test", "GPT Test"),
                            ],
                        )],
                    )],
                    ctx,
                )
                .expect("ACP agents should update");
        });

        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids = AISettings::as_ref(ctx)
                .bedrock_models
                .value()
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            preferences.inject_bedrock_models(ctx);
            preferences.inject_acp_models(ctx);
        });
        assert_eq!(preferences.models_by_feature.agent_mode.choices.len(), 1);
        assert_eq!(
            preferences.models_by_feature.agent_mode.choices[0].display_name,
            "GPT Test · OpenCode"
        );

        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .bedrock_enabled
                .set_value(true, ctx)
                .expect("Bedrock setting should update");
        });
        app.read(|ctx| preferences.inject_acp_models(ctx));
        assert_eq!(preferences.models_by_feature.agent_mode.choices.len(), 2);
    });
}

#[test]
fn disabled_providers_do_not_leave_models_in_the_runtime_inventory() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .bedrock_enabled
                .set_value(false, ctx)
                .expect("Bedrock setting should update");
            settings
                .openai_enabled
                .set_value(false, ctx)
                .expect("OpenAI setting should update");
            settings
                .acp_enabled
                .set_value(false, ctx)
                .expect("ACP setting should update");
        });

        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences.bedrock_verified_model_ids = AISettings::as_ref(ctx)
                .bedrock_models
                .value()
                .iter()
                .map(|model| model.model_id.clone())
                .collect();
            preferences.inject_bedrock_models(ctx);
            preferences.inject_openai_models(ctx);
        });

        assert!(preferences.models_by_feature.agent_mode.choices.is_empty());
        assert!(preferences.models_by_feature.coding.choices.is_empty());
        assert!(preferences
            .models_by_feature
            .cli_agent
            .as_ref()
            .is_none_or(|models| models.choices.is_empty()));
    });
}

#[test]
fn provider_discovery_keeps_new_and_manual_models() {
    let manual = openai_model("manual-model");
    let discovered = openai_model("codex-gpt-new");

    let merged = merge_discovered_provider_models(&[manual], vec![discovered]);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].model_id, "codex-gpt-new");
    assert_eq!(merged[0].supports_system_messages, Some(false));
    assert_eq!(merged[1].model_id, "manual-model");
}

#[test]
fn codex_release_json_yields_client_version() {
    let release = serde_json::json!({
        "tag_name": "rust-v0.147.0",
        "name": "0.147.0"
    });

    assert_eq!(
        codex_client_version_from_release_json(&release).as_deref(),
        Some("0.147.0")
    );
}

#[test]
fn chatgpt_codex_models_parse_visible_catalog_entries() {
    let body = serde_json::json!({
        "models": [
            {
                "slug": "gpt-5.6-sol",
                "display_name": "GPT-5.6-Sol",
                "visibility": "list",
                "context_window": 272000,
                "max_context_window": 872000,
                "effective_context_window_percent": 95,
                "input_modalities": ["text", "image"],
                "supported_reasoning_levels": [
                    {"effort": "low", "description": "Fast"},
                    {"effort": "xhigh", "description": "Deep"},
                    {"effort": "ultra", "description": "Delegated"}
                ]
            },
            {
                "slug": "hidden",
                "display_name": "Hidden",
                "visibility": "hide",
                "context_window": 128000,
                "input_modalities": ["text"],
                "supported_reasoning_levels": []
            },
            {
                "slug": "gpt-text-only",
                "display_name": "GPT Text Only",
                "visibility": "list",
                "max_context_window": 128000,
                "effective_context_window_percent": 90,
                "input_modalities": ["text"],
                "supported_reasoning_levels": [{"effort": "medium"}]
            }
        ]
    });

    let models = chatgpt_models_from_codex_response(&body);

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_id, "gpt-5.6-sol");
    assert_eq!(models[0].display_name, "GPT-5.6-Sol");
    assert!(models[0].vision_supported);
    assert_eq!(models[0].context_size, 258_400);
    assert_eq!(models[0].max_input_tokens, None);
    assert_eq!(models[0].default_context_size, Some(258_400));
    assert_eq!(models[0].reasoning_efforts, ["low", "xhigh", "ultra"]);
    assert_eq!(models[0].provider.as_deref(), Some("openai"));
    assert_eq!(models[0].supports_system_messages, Some(true));

    assert_eq!(models[1].model_id, "gpt-text-only");
    assert!(!models[1].vision_supported);
    assert_eq!(models[1].context_size, 115_200);
    assert_eq!(models[1].max_input_tokens, None);
    assert_eq!(models[1].default_context_size, Some(115_200));

    let configurable = openai_model_context_window(
        &models[0],
        crate::settings::OpenAIProviderKind::ChatGPTSubscription,
    );
    assert!(!configurable.is_configurable);
    assert_eq!(configurable.min, 258_400);
    assert_eq!(configurable.default_max, 258_400);
    assert_eq!(configurable.max, 258_400);

    let fixed =
        openai_model_context_window(&models[0], crate::settings::OpenAIProviderKind::LiteLLM);
    assert!(!fixed.is_configurable);
    assert_eq!(fixed.min, 258_400);
    assert_eq!(fixed.default_max, 258_400);
    assert_eq!(fixed.max, 258_400);
}

#[test]
fn chatgpt_catalog_merge_drops_stale_models_but_preserves_overrides() {
    let mut existing = openai_model("gpt-5.6-sol");
    existing.enabled = false;
    existing.context_size = 272_000;
    existing.max_input_tokens = Some(272_000);
    existing.capability_overrides.insert(
        "vision".to_string(),
        crate::settings::ModelCapabilityOverride::Unsupported,
    );
    let stale = openai_model("stale-model");

    let mut discovered = openai_model("gpt-5.6-sol");
    discovered.display_name = "GPT-5.6-Sol".to_string();
    discovered.vision_supported = true;
    discovered.context_size = 828_400;
    discovered.default_context_size = Some(258_400);

    let merged = merge_discovered_chatgpt_subscription_models(&[existing, stale], vec![discovered]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].model_id, "gpt-5.6-sol");
    assert_eq!(merged[0].context_size, 828_400);
    assert_eq!(merged[0].max_input_tokens, None);
    assert_eq!(merged[0].default_context_size, Some(258_400));
    assert!(!merged[0].enabled);
    assert_eq!(
        merged[0].capability_override("vision"),
        crate::settings::ModelCapabilityOverride::Unsupported
    );
}

#[test]
fn provider_context_budget_respects_hard_input_and_total_limits() {
    let mut model = openai_model("limited-model");
    model.context_size = 872_000;
    model.max_input_tokens = Some(272_000);
    let window = openai_model_context_window(&model, OpenAIProviderKind::LiteLLM);
    assert_eq!(window.max, 272_000);
    assert_eq!(window.default_max, 272_000);
    assert!(!window.is_configurable);

    model.max_input_tokens = Some(1_000_000);
    assert_eq!(
        openai_model_context_size(&model, OpenAIProviderKind::LiteLLM),
        872_000
    );

    model.default_context_size = Some(272_000);
    model.max_input_tokens = Some(400_000);
    let window = openai_model_context_window(&model, OpenAIProviderKind::ChatGPTSubscription);
    assert_eq!(window.min, 272_000);
    assert_eq!(window.max, 272_000);
    assert_eq!(
        openai_model_context_size(&model, OpenAIProviderKind::ChatGPTSubscription),
        window.max
    );
}

#[test]
fn legacy_chatgpt_default_limits_routing() {
    let mut model = openai_model("legacy-model");
    model.context_size = 872_000;
    model.max_input_tokens = Some(272_000);
    let window = openai_model_context_window(&model, OpenAIProviderKind::ChatGPTSubscription);
    assert_eq!(window.min, 272_000);
    assert_eq!(window.max, 272_000);
    assert_eq!(window.default_max, 272_000);
    assert!(!window.is_configurable);
    assert_eq!(
        openai_model_context_size(&model, OpenAIProviderKind::ChatGPTSubscription),
        window.max
    );
}

#[test]
fn subscription_and_litellm_connections_keep_overlapping_models_and_independent_toggles() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings.openai_enabled.set_value(true, ctx).unwrap();
            settings
                .openai_base_url
                .set_value("https://legacy.test/v1".to_owned(), ctx)
                .unwrap();
            settings
                .openai_models
                .set_value(vec![openai_model("legacy-only")], ctx)
                .unwrap();
            settings
                .openai_providers
                .set_value(
                    vec![
                        OpenAIProviderConfig {
                            kind: OpenAIProviderKind::ChatGPTSubscription,
                            enabled: true,
                            name: "Subscription".to_owned(),
                            base_url: String::new(),
                            api_key: None,
                            project_id: None,
                            location: None,
                            models: vec![openai_model("shared-model")],
                        },
                        OpenAIProviderConfig {
                            kind: OpenAIProviderKind::LiteLLM,
                            enabled: true,
                            name: "LiteLLM".to_owned(),
                            base_url: "https://proxy.test/v1".to_owned(),
                            api_key: None,
                            project_id: None,
                            location: None,
                            models: vec![openai_model("shared-model")],
                        },
                    ],
                    ctx,
                )
                .unwrap();
        });
        let mut preferences = empty_preferences();
        app.read(|ctx| preferences.inject_openai_models(ctx));
        let models = &preferences.models_by_feature.agent_mode.choices;
        assert_eq!(models.len(), 3);
        for (label, kind) in [
            ("Subscription", OpenAIProviderKind::ChatGPTSubscription),
            ("LiteLLM", OpenAIProviderKind::LiteLLM),
        ] {
            let model = models
                .iter()
                .find(|model| model.description.as_deref() == Some(label))
                .unwrap();
            let route = preferences
                .openai_client_config_for_model(model.id.as_str())
                .unwrap();
            assert_eq!(route.kind, kind);
            assert_eq!(route.model.as_deref(), Some("shared-model"));
        }
        assert!(models
            .iter()
            .any(|model| model.id.as_str() == "legacy-only"));
        let litellm_id = models
            .iter()
            .find(|model| model.description.as_deref() == Some("LiteLLM"))
            .unwrap()
            .id
            .clone();
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            let mut providers = settings.openai_providers.value().clone();
            providers[0].models[0].enabled = false;
            settings.openai_providers.set_value(providers, ctx).unwrap();
        });
        app.read(|ctx| preferences.inject_openai_models(ctx));
        let models = &preferences.models_by_feature.agent_mode.choices;
        assert_eq!(models.len(), 2);
        assert!(models
            .iter()
            .all(|model| model.description.as_deref() != Some("Subscription")));
        assert!(models
            .iter()
            .any(|model| model.description.as_deref() == Some("LiteLLM")));
        assert!(models.iter().any(|model| model.id == litellm_id));
    });
}

#[test]
fn provider_registry_overrides_legacy_endpoint_for_duplicate_models() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings.openai_enabled.set_value(true, ctx).unwrap();
            settings
                .openai_base_url
                .set_value("https://stale.test/v1".to_owned(), ctx)
                .unwrap();
            settings
                .openai_models
                .set_value(vec![openai_model("shared-model")], ctx)
                .unwrap();
            settings
                .openai_providers
                .set_value(
                    vec![OpenAIProviderConfig {
                        kind: OpenAIProviderKind::LiteLLM,
                        enabled: true,
                        name: "Current LiteLLM".to_owned(),
                        base_url: "https://current.test/v1".to_owned(),
                        api_key: Some("current-key".to_owned()),
                        project_id: None,
                        location: None,
                        models: vec![openai_model("shared-model")],
                    }],
                    ctx,
                )
                .unwrap();
        });
        let mut preferences = empty_preferences();
        app.read(|ctx| preferences.inject_openai_models(ctx));
        let config = preferences
            .openai_client_config_for_model("shared-model")
            .unwrap();
        assert_eq!(config.base_url, "https://current.test/v1");
        assert_eq!(config.api_key.as_deref(), Some("current-key"));
    });
}

#[test]
fn chatgpt_default_window_limits_routing_without_a_separate_input_limit() {
    let mut model = openai_model("subscription-model");
    model.context_size = 872_000;
    model.default_context_size = Some(272_000);
    model.max_input_tokens = None;
    let window = openai_model_context_window(&model, OpenAIProviderKind::ChatGPTSubscription);
    assert_eq!(window.default_max, 272_000);
    assert_eq!(window.max, 272_000);
    assert!(!window.is_configurable);
    assert_eq!(
        openai_model_context_size(&model, OpenAIProviderKind::ChatGPTSubscription),
        272_000
    );
}

#[test]
fn litellm_efficient_windows_limit_model_picker_and_provider_routing() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let models = [("terminal-model", 272_000), ("main-model", 500_000)]
            .into_iter()
            .map(|(id, efficient_window)| {
                let mut model = openai_model(id);
                model.context_size = 872_000;
                model.max_input_tokens = Some(872_000);
                model.default_context_size = Some(efficient_window);
                model
            })
            .collect();
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            settings.openai_enabled.set_value(true, ctx).unwrap();
            settings
                .openai_providers
                .set_value(
                    vec![OpenAIProviderConfig {
                        kind: OpenAIProviderKind::LiteLLM,
                        enabled: true,
                        name: "LiteLLM".to_owned(),
                        base_url: "https://proxy.test/v1".to_owned(),
                        api_key: None,
                        project_id: None,
                        location: None,
                        models,
                    }],
                    ctx,
                )
                .unwrap();
        });
        let mut preferences = empty_preferences();
        app.read(|ctx| preferences.inject_openai_models(ctx));
        for (id, efficient_window) in [("terminal-model", 272_000), ("main-model", 500_000)] {
            let route = preferences.openai_client_config_for_model(id).unwrap();
            assert_eq!(route.max_input_tokens, Some(efficient_window));
            let model = preferences
                .models_by_feature
                .agent_mode
                .choices
                .iter()
                .find(|model| model.id.as_str() == id)
                .unwrap();
            assert_eq!(model.context_window.max, efficient_window);
            assert_eq!(model.context_window.default_max, efficient_window);
            assert!(!model.context_window.is_configurable);
        }
    });
}

#[tokio::test]
async fn litellm_discovery_preserves_efficient_windows_across_catalog_endpoints() {
    let mut server = mockito::Server::new_async().await;
    for (model_metadata, rich_metadata, maximum, efficient_window, request_budget) in [
        (
            json!({"id": "model"}),
            Some(json!({
                "context_window": 500000,
                "max_context_window": 872000,
                "max_input_tokens": 872000
            })),
            872_000,
            500_000,
            500_000,
        ),
        (
            json!({"id": "model", "context_window": 272000, "max_context_window": 872000}),
            Some(json!({"max_context_window": 872000})),
            872_000,
            272_000,
            272_000,
        ),
        (
            json!({"id": "model", "context_window": 272000, "max_context_window": 872000}),
            None,
            872_000,
            272_000,
            272_000,
        ),
        (
            json!({"id": "model", "context_window": 500000, "max_context_window": 872000}),
            Some(json!({"supports_vision": true})),
            872_000,
            500_000,
            500_000,
        ),
        (
            json!({"id": "model"}),
            Some(json!({
                "context_window": 272000,
                "max_context_window": 872000,
                "max_input_tokens": 258400,
                "effective_context_window_percent": 95
            })),
            872_000,
            258_400,
            258_400,
        ),
        (
            json!({"id": "model"}),
            Some(json!({
                "context_window": 272000,
                "max_context_window": 872000,
                "max_input_tokens": 128000
            })),
            872_000,
            272_000,
            128_000,
        ),
    ] {
        let catalog = server
            .mock("GET", "/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({"data": [model_metadata]}).to_string())
            .create_async()
            .await;
        let metadata = server
            .mock("GET", "/model/info")
            .with_status(if rich_metadata.is_some() { 200 } else { 404 })
            .with_header("content-type", "application/json")
            .with_body(
                json!({"data": [{"model_name": "model", "model_info": rich_metadata}]}).to_string(),
            )
            .create_async()
            .await;
        let models = fetch_litellm_catalog(&server.url(), None, &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_size, maximum);
        assert_eq!(models[0].default_context_size, Some(efficient_window));
        assert_eq!(
            openai_model_context_size(&models[0], OpenAIProviderKind::LiteLLM),
            request_budget
        );
        let window = openai_model_context_window(&models[0], OpenAIProviderKind::LiteLLM);
        assert_eq!(window.default_max, request_budget);
        assert_eq!(window.max, request_budget);
        assert!(!window.is_configurable);
        catalog.assert_async().await;
        metadata.assert_async().await;
        catalog.remove_async().await;
        metadata.remove_async().await;
    }
}

#[test]
fn litellm_refresh_replaces_membership_and_preserves_preferences() {
    let mut retained = openai_model("retained");
    retained.enabled = false;
    retained.display_name = "My model".to_string();
    retained.capability_overrides.insert(
        "vision".to_string(),
        crate::settings::ModelCapabilityOverride::Unsupported,
    );
    let removed = openai_model("removed");
    let mut updated = openai_model("retained");
    updated.context_size = 300_000;
    let models = replace_litellm_catalog(&[retained, removed], vec![updated, openai_model("new")]);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_id, "retained");
    assert_eq!(models[0].context_size, 300_000);
    assert_eq!(models[0].display_name, "My model");
    assert!(!models[0].enabled);
    assert_eq!(
        models[0].capability_override("vision"),
        crate::settings::ModelCapabilityOverride::Unsupported
    );
    assert_eq!(models[1].model_id, "new");
    assert!(replace_litellm_catalog(&models, vec![]).is_empty());
}

#[tokio::test]
async fn litellm_models_endpoint_owns_membership_even_with_extra_metadata_models() {
    let mut server = mockito::Server::new_async().await;
    let catalog = server
        .mock("GET", "/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"current"}]}"#)
        .create_async()
        .await;
    let metadata = server.mock("GET", "/model/info")
        .with_status(200).with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"model_name":"removed","model_info":{"max_input_tokens":100000}},{"model_name":"current","model_info":{"max_input_tokens":128000}}]}"#)
        .create_async().await;
    let models = fetch_litellm_catalog(&server.url(), None, &reqwest::Client::new())
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model_id, "current");
    catalog.assert_async().await;
    metadata.assert_async().await;
}

#[tokio::test]
async fn model_refresh_distinguishes_empty_catalog_from_failure() {
    let mut server = mockito::Server::new_async().await;
    for (status, body, succeeds) in [
        (200, r#"{"data":[]}"#, true),
        (200, r#"{"error":"unavailable"}"#, false),
        (200, r#"{"data":[{}]}"#, false),
        (401, r#"{"error":"unauthorized"}"#, false),
    ] {
        let response = server
            .mock("GET", "/models")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let result = fetch_litellm_catalog(&server.url(), None, &reqwest::Client::new()).await;
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(models) = result {
            assert!(models.is_empty());
        }
        response.assert_async().await;
        response.remove_async().await;
    }
}

#[test]
fn bedrock_verification_survives_settings_reload_but_not_connection_changes() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        let model = bedrock_model("anthropic.claude-test");
        let reloaded: BedrockModelConfig =
            serde_json::from_value(serde_json::to_value(&model).unwrap()).unwrap();
        assert!(!reloaded.access_verified);
        let mut preferences = empty_preferences();
        app.read(|ctx| {
            preferences.bedrock_verified_config =
                Some(super::bedrock_discovery_config(AISettings::as_ref(ctx)));
            preferences
                .bedrock_verified_model_ids
                .insert(model.model_id.clone());
        });
        app.update(|ctx| {
            AISettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .bedrock_models
                    .set_value(vec![reloaded], ctx)
                    .unwrap();
            })
        });
        app.read(|ctx| assert!(preferences.bedrock_model_is_verified(&model.model_id, ctx)));
        // Another process/startup cannot inherit an in-memory approval.
        app.read(|ctx| {
            assert!(!empty_preferences().bedrock_model_is_verified(&model.model_id, ctx))
        });
        app.update(|ctx| {
            AISettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .bedrock_region
                    .set_value("different-region".to_owned(), ctx)
                    .unwrap();
            })
        });
        app.read(|ctx| assert!(!preferences.bedrock_model_is_verified(&model.model_id, ctx)));
    });
}

#[test]
fn model_selector_labels_and_reasoning_groups_include_configured_provider() {
    let first: LLMInfo = serde_json::from_value(serde_json::json!({
        "id": "model-a", "display_name": "Claude (high)", "base_model_name": "Claude",
        "description": "My Claude Subscription", "provider": "LiteLLM",
        "usage_metadata": { "request_multiplier": 1 }
    })).unwrap();
    assert_eq!(first.menu_display_name(), "Claude (high) (My Claude Subscription)");
    assert_eq!(first.label_with_provider(first.base_model_name()), "Claude (My Claude Subscription)");
    let mut other = first.clone();
    other.description = Some("My LiteLLM".to_string());
    assert_ne!(first.reasoning_group_key(), other.reasoning_group_key());
    other.description = first.description.clone();
    other.display_name = "Claude (low)".to_string();
    other.id = LLMId::from("model-b");
    assert_eq!(first.reasoning_group_key(), other.reasoning_group_key());
    other.id = LLMId::from("model-b::connection::second-connection");
    assert_ne!(first.reasoning_group_key(), other.reasoning_group_key());
}
