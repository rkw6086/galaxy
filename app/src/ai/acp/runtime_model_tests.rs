use galaxy_acp::{SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption};

use super::{AcpDiscoveryState, AcpRuntimeModel};
use crate::settings::{AcpAgentSettings, AcpConfigOptionSettings};

#[test]
fn normalize_config_options_preserves_values_and_order() {
    let options = vec![
        SessionConfigOption::select(
            "model",
            "Model",
            "fast",
            vec![
                SessionConfigSelectOption::new("fast", "Fast"),
                SessionConfigSelectOption::new("accurate", "Accurate"),
            ],
        )
        .category(SessionConfigOptionCategory::Model),
        SessionConfigOption::boolean("thinking", "Thinking", true),
    ];

    let normalized = AcpRuntimeModel::normalize_config_options(options);

    assert_eq!(normalized.len(), 2);
    assert_eq!(normalized[0].kind, "select");
    assert_eq!(normalized[0].current_value, serde_json::json!("fast"));
    assert_eq!(normalized[0].options[0].value, serde_json::json!("fast"));
    assert_eq!(
        normalized[0].options[1].value,
        serde_json::json!("accurate")
    );
    assert_eq!(normalized[1].kind, "boolean");
    assert_eq!(normalized[1].current_value, serde_json::json!(true));
}

#[test]
fn normalize_config_options_preserves_unknown_categories() {
    let option = SessionConfigOption::select(
        "custom",
        "Custom",
        "one",
        Vec::<SessionConfigSelectOption>::new(),
    )
    .category(SessionConfigOptionCategory::Other("_custom".to_owned()));

    let normalized = AcpRuntimeModel::normalize_config_options(vec![option]);

    assert_eq!(normalized[0].category.as_deref(), Some("_custom"));
}

#[test]
fn selection_replaces_model_and_preserves_other_current_values() {
    let options = vec![
        AcpConfigOptionSettings {
            id: "model".to_owned(),
            name: "Model".to_owned(),
            description: None,
            category: Some("model".to_owned()),
            kind: "select".to_owned(),
            current_value: serde_json::json!("fast"),
            options: Vec::new(),
        },
        AcpConfigOptionSettings {
            id: "thinking".to_owned(),
            name: "Thinking".to_owned(),
            description: None,
            category: None,
            kind: "boolean".to_owned(),
            current_value: serde_json::json!(true),
            options: Vec::new(),
        },
    ];

    assert_eq!(
        AcpRuntimeModel::selection_values_for_model(
            &options,
            "model",
            &serde_json::json!("accurate")
        ),
        std::collections::BTreeMap::from([
            ("model".to_owned(), serde_json::json!("accurate")),
            ("thinking".to_owned(), serde_json::json!(true)),
        ])
    );
}

#[test]
fn discovery_state_has_explicit_running_success_and_failure_values() {
    assert_eq!(AcpDiscoveryState::default(), AcpDiscoveryState::Idle);
    assert_eq!(
        AcpDiscoveryState::Succeeded { option_count: 2 },
        AcpDiscoveryState::Succeeded { option_count: 2 }
    );
    assert_eq!(
        AcpDiscoveryState::Failed {
            message: "timeout".to_owned()
        },
        AcpDiscoveryState::Failed {
            message: "timeout".to_owned()
        }
    );
}

#[test]
fn discovery_metadata_fields_are_optional_for_legacy_settings() {
    let json = serde_json::json!({
        "id": "codex",
        "name": "Codex",
        "config_options": []
    });
    let agent: AcpAgentSettings = serde_json::from_value(json).unwrap();

    assert!(agent.discovery_timestamp.is_none());
    assert!(agent.discovery_source.is_none());
    assert!(agent.discovery_error.is_none());
}
