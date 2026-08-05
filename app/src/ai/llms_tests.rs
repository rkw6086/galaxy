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
