//! A versioned, offline snapshot of the public Amazon Bedrock model catalog.
//!
//! The JSON is kept verbatim so consumers can adopt new upstream fields without
//! first changing this crate. Run `./script/update_bedrock_model_catalog` from
//! the workspace root to refresh the snapshot.

mod metadata;

pub use metadata::{ModelMetadata, model_metadata, models};

/// Repository from which the bundled snapshot was downloaded.
pub const UPSTREAM_REPOSITORY: &str =
    "https://github.com/amazonbedrockmodels/amazonbedrockmodels.github.io";

/// The catalog's public API manifest.
pub const API_MANIFEST_JSON: &str = include_str!("../data/api.json");

/// Foundation models discovered through the AWS-native Bedrock APIs.
pub const FOUNDATION_MODELS_JSON: &str = include_str!("../data/models.json");

/// System-defined inference profiles discovered across AWS regions.
pub const INFERENCE_PROFILES_JSON: &str = include_str!("../data/profiles.json");

/// Models discovered before they appear in the main AWS documentation.
pub const BETA_MODELS_JSON: &str = include_str!("../data/beta_models.json");

/// Models available through the Bedrock Mantle endpoint.
pub const MANTLE_MODELS_JSON: &str = include_str!("../data/mantle_models.json");

/// Enriched model-card metadata maintained by the upstream catalog.
pub const MODEL_CARDS_JSON: &str = include_str!("../data/model_cards.json");

/// Returns the exact upstream commit used for the bundled snapshot.
pub fn upstream_commit() -> &'static str {
    include_str!("../data/UPSTREAM_COMMIT").trim()
}

/// All catalog documents embedded in this crate.
#[derive(Debug, Clone, Copy)]
pub struct CatalogSnapshot {
    pub api_manifest: &'static str,
    pub foundation_models: &'static str,
    pub inference_profiles: &'static str,
    pub beta_models: &'static str,
    pub mantle_models: &'static str,
    pub model_cards: &'static str,
}

/// The bundled catalog snapshot.
pub const SNAPSHOT: CatalogSnapshot = CatalogSnapshot {
    api_manifest: API_MANIFEST_JSON,
    foundation_models: FOUNDATION_MODELS_JSON,
    inference_profiles: INFERENCE_PROFILES_JSON,
    beta_models: BETA_MODELS_JSON,
    mantle_models: MANTLE_MODELS_JSON,
    model_cards: MODEL_CARDS_JSON,
};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod metadata_tests;
