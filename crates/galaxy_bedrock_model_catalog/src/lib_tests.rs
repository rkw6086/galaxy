use serde_json::Value;

use super::*;

#[test]
fn bundled_catalog_documents_are_valid_json() {
    for document in [
        API_MANIFEST_JSON,
        FOUNDATION_MODELS_JSON,
        INFERENCE_PROFILES_JSON,
        BETA_MODELS_JSON,
        MANTLE_MODELS_JSON,
        MODEL_CARDS_JSON,
    ] {
        serde_json::from_str::<Value>(document).expect("catalog document should be valid JSON");
    }
}

#[test]
fn bundled_catalog_has_models_and_provenance() {
    let foundation_models: Value =
        serde_json::from_str(FOUNDATION_MODELS_JSON).expect("foundation models should parse");
    let mantle_models: Value =
        serde_json::from_str(MANTLE_MODELS_JSON).expect("Mantle models should parse");

    assert!(
        foundation_models
            .as_array()
            .is_some_and(|models| !models.is_empty())
    );
    assert!(
        mantle_models
            .as_object()
            .is_some_and(|models| !models.is_empty())
    );
    assert_eq!(upstream_commit().len(), 40);
    assert!(
        upstream_commit()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
}
