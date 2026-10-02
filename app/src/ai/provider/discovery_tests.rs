use super::model_availability_is_usable;

#[test]
fn cached_access_is_not_authorization_and_refresh_preserves_disabled_models() {
    let model = crate::settings::BedrockModelConfig {
        model_id: "test.model".to_owned(),
        display_name: "Test".to_owned(),
        vision_supported: false,
        enabled: false,
        access_verified: true,
    };
    let saved = serde_json::to_value(&model).unwrap();
    assert!(saved.get("access_verified").is_none());
    let restored: crate::settings::BedrockModelConfig = serde_json::from_value(saved).unwrap();
    assert!(!restored.access_verified);
    assert!(!restored.enabled);
    let mut discovered = model.clone();
    discovered.enabled = true;
    let refreshed = super::preserve_model_toggles(&[model], vec![discovered]);
    assert!(!refreshed[0].enabled);
    assert!(refreshed[0].access_verified);
}

#[test]
fn requires_every_availability_status() {
    assert!(model_availability_is_usable(
        Some("AVAILABLE"),
        "AUTHORIZED",
        "AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        None,
        "AUTHORIZED",
        "AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        Some("AVAILABLE"),
        "NOT_AUTHORIZED",
        "AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        Some("AVAILABLE"),
        "AUTHORIZED",
        "NOT_AVAILABLE",
        "AVAILABLE",
    ));
    assert!(!model_availability_is_usable(
        Some("AVAILABLE"),
        "AUTHORIZED",
        "AVAILABLE",
        "NOT_AVAILABLE",
    ));
}
