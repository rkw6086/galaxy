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
    AcpAgentSettings, AcpConfigOptionSettings, AcpConfigValueSettings, OpenAIModelConfig,
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
        max_output_tokens: None,
        provider: None,
        use_rig: false,
        supports_system_messages: None,
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
    LLMPreferences {
        models_by_feature: ModelsByFeature::default(),
        last_update: None,
        base_llm_for_terminal_view: HashMap::new(),
        custom_llms: Vec::new(),
        custom_model_routers: Vec::new(),
        openai_provider_routing: HashMap::new(),
        fetched_openai_models: Vec::new(),
        acp_selections: HashMap::new(),
    }
}

#[test]
fn provider_discovery_preserves_local_model_overrides() {
    let mut existing = openai_model("codex-gpt-5.6-sol-xhigh");
    existing.display_name = "My Codex".to_string();
    existing.context_size = 100_000;
    existing.provider = Some("openai".to_string());
    existing.use_rig = true;
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
    assert!(merged[0].use_rig);
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
            HashSet::from(["GPT Test (High)", "GPT Test (Xhigh)"])
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
            preferences.inject_bedrock_models(ctx);
            preferences.inject_acp_models(ctx);
        });
        assert_eq!(preferences.models_by_feature.agent_mode.choices.len(), 1);
        assert_eq!(
            preferences.models_by_feature.agent_mode.choices[0].display_name,
            "GPT Test"
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
fn provider_discovery_enables_rig_for_new_models_and_keeps_manual_models() {
    let manual = openai_model("manual-model");
    let discovered = openai_model("codex-gpt-new");

    let merged = merge_discovered_provider_models(&[manual], vec![discovered]);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].model_id, "codex-gpt-new");
    assert!(merged[0].use_rig);
    assert_eq!(merged[0].supports_system_messages, Some(false));
    assert_eq!(merged[1].model_id, "manual-model");
}
