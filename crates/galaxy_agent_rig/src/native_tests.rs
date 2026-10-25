use super::native_descriptor;

#[test]
fn native_descriptors_are_provider_specific() {
    let descriptor = native_descriptor("anthropic", "claude-sonnet");

    assert_eq!(descriptor.id, "rig-anthropic:claude-sonnet");
    assert_eq!(descriptor.display_name, "Rig / anthropic / claude-sonnet");
}

#[test]
fn anthropic_discovery_prefers_live_limits_over_catalog_defaults() {
    let model = super::anthropic_model_info(&serde_json::json!({
        "id": "claude-opus-4-6", "display_name": "Opus", "max_input_tokens": 500000, "max_tokens": 32000
    })).unwrap();
    assert_eq!(model.context_size, Some(500000));
    assert_eq!(model.max_output_tokens, Some(32000));
}

#[test]
fn anthropic_discovery_uses_exact_catalog_limits_when_api_omits_them() {
    let model = super::anthropic_model_info(&serde_json::json!({
        "id": "claude-opus-4-6", "max_input_tokens": null, "max_tokens": null
    }))
    .unwrap();
    assert_eq!(model.context_size, Some(1000000));
    assert_eq!(model.max_output_tokens, Some(128000));
    let unknown =
        super::anthropic_model_info(&serde_json::json!({"id": "claude-unknown"})).unwrap();
    assert_eq!(unknown.context_size, None);
    assert_eq!(unknown.max_output_tokens, None);
}

#[test]
fn anthropic_discovery_reads_only_supported_effort_capabilities() {
    let model = super::anthropic_model_info(&serde_json::json!({
        "id": "claude-future", "capabilities": {"effort": {
            "supported": true, "low": {"supported": true},
            "medium": {"supported": false}, "high": {"supported": true},
            "xhigh": {"supported": true}, "max": {"supported": false}
        }}
    }))
    .unwrap();
    assert_eq!(model.reasoning_efforts, ["low", "high", "xhigh"]);
    assert!(super::anthropic_reasoning_efforts(&serde_json::json!({
        "capabilities": {"effort": {"supported": false, "low": {"supported": true}}}
    }))
    .is_empty());
    assert!(super::anthropic_reasoning_efforts(&serde_json::json!({
        "id": "claude-opus-4-6"
    }))
    .is_empty());
}

#[test]
fn native_reasoning_selections_use_provider_request_fields() {
    assert_eq!(
        super::anthropic_reasoning_params("high"),
        serde_json::json!({
            "output_config": {"effort": "high"}
        })
    );
    assert_eq!(
        super::gemini_reasoning_params("low"),
        serde_json::json!({
            "generationConfig": {"thinkingConfig": {"thinkingLevel": "low"}}
        })
    );
}

#[test]
fn gemini_reasoning_parameters_survive_rig_typed_conversion() {
    use rig_core::providers::gemini::completion::gemini_api_types::{
        AdditionalParameters, ThinkingLevel,
    };
    let params: AdditionalParameters =
        serde_json::from_value(super::gemini_reasoning_params("medium")).unwrap();
    assert_eq!(
        params
            .generation_config
            .unwrap()
            .thinking_config
            .unwrap()
            .thinking_level,
        Some(ThinkingLevel::Medium)
    );
}
