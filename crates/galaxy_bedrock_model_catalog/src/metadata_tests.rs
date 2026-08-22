use super::*;

#[test]
fn reads_context_and_capabilities_for_an_agent_model() {
    let metadata = model_metadata("anthropic.claude-opus-4-8").unwrap();

    assert_eq!(metadata.model_name, "Claude Opus 4.8");
    assert_eq!(metadata.provider_name, "Anthropic");
    assert_eq!(metadata.context_window_tokens, Some(1_000_000));
    assert_eq!(metadata.max_output_tokens, Some(128_000));
    assert!(metadata.supports_vision_input());
    assert_eq!(metadata.supports_agent_runtime(), Some(true));
}

#[test]
fn normalizes_inference_profile_ids_and_galaxy_suffixes() {
    let metadata = model_metadata(
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-opus-4-8[1M]",
    )
    .unwrap();

    assert_eq!(metadata.model_id, "anthropic.claude-opus-4-8");
}

#[test]
fn preserves_unknown_and_invalid_metadata_as_unknown() {
    assert!(model_metadata("example.future-model-v1").is_none());
    assert_eq!(
        model_metadata("cohere.embed-english-v3")
            .unwrap()
            .context_window_tokens,
        None
    );
}

#[test]
fn rejects_known_non_agent_models() {
    assert_eq!(
        model_metadata("cohere.embed-english-v3")
            .unwrap()
            .supports_agent_runtime(),
        Some(false)
    );
    assert_eq!(
        model_metadata("amazon.nova-2-sonic-v1:0")
            .unwrap()
            .supports_agent_runtime(),
        Some(false)
    );
}
