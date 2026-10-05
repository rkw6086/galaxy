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
    })).unwrap();
    assert_eq!(model.context_size, Some(1000000));
    assert_eq!(model.max_output_tokens, Some(128000));
    let unknown = super::anthropic_model_info(&serde_json::json!({"id": "claude-unknown"})).unwrap();
    assert_eq!(unknown.context_size, None);
    assert_eq!(unknown.max_output_tokens, None);
}
