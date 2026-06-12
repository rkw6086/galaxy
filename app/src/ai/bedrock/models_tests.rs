use super::models::*;
use crate::settings::ai::BedrockModelConfig;

#[test]
fn test_cross_region_prefix_us_east() {
    assert_eq!(
        apply_cross_region_prefix("anthropic.claude-3-5-sonnet-20241022-v1:0", "us-east-1"),
        "us.anthropic.claude-3-5-sonnet-20241022-v1:0"
    );
}

#[test]
fn test_cross_region_prefix_eu_west() {
    assert_eq!(
        apply_cross_region_prefix("anthropic.claude-3-5-sonnet-20241022-v1:0", "eu-west-1"),
        "eu.anthropic.claude-3-5-sonnet-20241022-v1:0"
    );
}

#[test]
fn test_cross_region_prefix_ap_northeast_1() {
    assert_eq!(
        apply_cross_region_prefix(
            "anthropic.claude-3-5-sonnet-20241022-v1:0",
            "ap-northeast-1"
        ),
        "jp.anthropic.claude-3-5-sonnet-20241022-v1:0"
    );
}

#[test]
fn test_cross_region_prefix_ap_southeast_2() {
    assert_eq!(
        apply_cross_region_prefix(
            "anthropic.claude-3-5-sonnet-20241022-v1:0",
            "ap-southeast-2"
        ),
        "au.anthropic.claude-3-5-sonnet-20241022-v1:0"
    );
}

#[test]
fn test_cross_region_prefix_ap_southeast_1() {
    assert_eq!(
        apply_cross_region_prefix(
            "anthropic.claude-3-5-sonnet-20241022-v1:0",
            "ap-southeast-1"
        ),
        "apac.anthropic.claude-3-5-sonnet-20241022-v1:0"
    );
}

#[test]
fn test_cross_region_prefix_already_prefixed() {
    assert_eq!(
        apply_cross_region_prefix("us.anthropic.claude-sonnet-4-6", "us-east-1"),
        "us.anthropic.claude-sonnet-4-6"
    );
    assert_eq!(
        apply_cross_region_prefix("global.anthropic.claude-sonnet-4-6", "us-east-1"),
        "global.anthropic.claude-sonnet-4-6"
    );
}

#[test]
fn test_cross_region_prefix_canada_maps_to_us() {
    assert_eq!(
        apply_cross_region_prefix("anthropic.claude-sonnet-4-6", "ca-central-1"),
        "us.anthropic.claude-sonnet-4-6"
    );
}

#[test]
fn test_cross_region_prefix_unknown_region() {
    assert_eq!(
        apply_cross_region_prefix("anthropic.claude-3-5-sonnet-20241022-v1:0", "me-south-1"),
        "anthropic.claude-3-5-sonnet-20241022-v1:0"
    );
}

#[test]
fn test_get_effective_models_empty_returns_defaults() {
    let models = get_effective_models(&[]);
    assert_eq!(models.len(), DEFAULT_BEDROCK_MODELS.len());
    assert_eq!(models[0].model_id, "anthropic.claude-opus-4-6[1m]");
    assert_eq!(models[0].display_name, "Claude Opus 4.6");
}

#[test]
fn test_get_effective_models_custom_overrides() {
    let custom = vec![BedrockModelConfig {
        model_id: "custom.model-v1:0".to_string(),
        display_name: "Custom Model".to_string(),
        vision_supported: false,
    }];
    let models = get_effective_models(&custom);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model_id, "custom.model-v1:0");
}

#[test]
fn test_cross_region_prefix_skips_arn() {
    let arn = "arn:aws:bedrock:us-east-1:156729649053:application-inference-profile/fma5bsw4oxhy";
    assert_eq!(apply_cross_region_prefix(arn, "us-east-1"), arn);
}
