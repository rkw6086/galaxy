use super::model_availability_is_usable;
use aws_sdk_bedrock::operation::list_inference_profiles::ListInferenceProfilesOutput;
use aws_sdk_bedrock::types::{
    FoundationModelSummary, InferenceProfileModel, InferenceProfileStatus, InferenceProfileSummary,
    InferenceProfileType, InferenceType, ModelModality,
};

fn foundation(id: &str, on_demand: bool) -> FoundationModelSummary {
    FoundationModelSummary::builder()
        .model_id(id)
        .model_arn(format!("arn:aws:bedrock:us-east-1::foundation-model/{id}"))
        .model_name("New live model")
        .provider_name("New provider")
        .input_modalities(ModelModality::Text)
        .output_modalities(ModelModality::Text)
        .response_streaming_supported(true)
        .set_inference_types_supported(on_demand.then(|| vec![InferenceType::OnDemand]))
        .build()
        .unwrap()
}

fn inference_profile(id: &str, model: &str) -> InferenceProfileSummary {
    InferenceProfileSummary::builder()
        .inference_profile_id(id)
        .inference_profile_name(id)
        .inference_profile_arn(format!(
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/{id}"
        ))
        .status(InferenceProfileStatus::Active)
        .r#type(InferenceProfileType::SystemDefined)
        .models(
            InferenceProfileModel::builder()
                .model_arn(format!(
                    "arn:aws:bedrock:us-east-1::foundation-model/{model}"
                ))
                .build(),
        )
        .build()
        .unwrap()
}

#[test]
fn new_live_model_does_not_need_a_bundled_catalog_entry() {
    let id = "future.new-model-v100";
    assert!(galaxy_bedrock_model_catalog::model_metadata(id).is_none());
    let profile = inference_profile("us.future.new-model-v100", id);
    let models =
        super::models_from_inventory(vec![foundation(id, false)], &[profile.clone()], true);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model_id, id);
    assert_eq!(
        models[0].invocation_model_id,
        profile.inference_profile_arn()
    );
    assert_eq!(models[0].response_streaming_supported, Some(true));
    assert_eq!(models[0].provider_name, "New provider");
}

#[test]
fn cross_region_disabled_requires_an_actual_on_demand_route() {
    let id = "future.profile-only";
    let profile = inference_profile("us.future.profile-only", id);
    assert!(
        super::models_from_inventory(vec![foundation(id, false)], &[profile.clone()], false,)
            .is_empty()
    );
    let models = super::models_from_inventory(vec![foundation(id, true)], &[profile], false);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].invocation_model_id, id);
}

#[test]
fn inventory_deduplicates_routes_and_rejects_known_incompatible_models() {
    let id = "future.new-model-v100";
    let profile = inference_profile("us.future.new-model-v100", id);
    let models = super::models_from_inventory(
        vec![foundation(id, false), foundation(id, false)],
        &[profile.clone(), profile],
        true,
    );
    assert_eq!(models.len(), 1);
    assert!(
        super::models_from_inventory(vec![foundation("openai.gpt-5.5", true)], &[], true,)
            .is_empty()
    );
    let mut not_streaming = foundation(id, true);
    not_streaming.response_streaming_supported = Some(false);
    assert!(super::models_from_inventory(vec![not_streaming], &[], true).is_empty());
    let mut image_model = foundation(id, true);
    image_model.output_modalities = Some(vec![ModelModality::Image]);
    assert!(super::models_from_inventory(vec![image_model], &[], true).is_empty());
}

#[tokio::test]
async fn inference_profile_discovery_follows_every_page() {
    let mut calls = Vec::new();
    let profiles = super::collect_profile_pages(|token| {
        calls.push(token.clone());
        let page = match token.as_deref() {
            None => ListInferenceProfilesOutput::builder()
                .inference_profile_summaries(inference_profile("us.future.first", "future.first"))
                .next_token("page-2")
                .build(),
            Some("page-2") => ListInferenceProfilesOutput::builder()
                .inference_profile_summaries(inference_profile("us.future.second", "future.second"))
                .build(),
            Some(token) => panic!("Unexpected token: {token}"),
        };
        std::future::ready(Ok(page))
    })
    .await
    .unwrap();
    assert_eq!(profiles.len(), 2);
    assert_eq!(calls, [None, Some("page-2".to_owned())]);
}

#[tokio::test]
async fn failed_or_repeating_profile_pages_do_not_return_partial_success() {
    let result =
        super::collect_profile_pages(|_| std::future::ready(Err("network unavailable".to_owned())))
            .await;
    assert_eq!(result.unwrap_err(), "network unavailable");
    let result = super::collect_profile_pages(|_| {
        std::future::ready(Ok(ListInferenceProfilesOutput::builder()
            .next_token("repeated")
            .build()))
    })
    .await;
    assert!(result.is_err());
}

#[test]
fn transiently_missing_models_keep_preferences_without_restoring_authorization() {
    let mut saved = crate::settings::BedrockModelConfig {
        enabled: false,
        access_verified: true,
        model_id: "future.existing".to_owned(),
        display_name: "My model name".to_owned(),
        ..Default::default()
    };
    let preserved = super::preserve_model_toggles(&[saved.clone()], Vec::new());
    assert_eq!(preserved.len(), 1);
    assert!(!preserved[0].enabled);
    assert!(!preserved[0].access_verified);
    saved.invocation_model_id = "old-route".to_owned();
    let mut discovered = saved.clone();
    discovered.enabled = true;
    discovered.display_name = "AWS name".to_owned();
    discovered.invocation_model_id = "new-route".to_owned();
    let preserved = super::preserve_model_toggles(&[saved], vec![discovered]);
    assert!(!preserved[0].enabled);
    assert_eq!(preserved[0].display_name, "My model name");
    assert_eq!(preserved[0].invocation_model_id, "new-route");
}

#[test]
fn cached_access_is_not_authorization_and_refresh_preserves_disabled_models() {
    let model = crate::settings::BedrockModelConfig {
        model_id: "test.model".to_owned(),
        display_name: "Test".to_owned(),
        vision_supported: false,
        enabled: false,
        access_verified: true,
        ..Default::default()
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
