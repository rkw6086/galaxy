use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::FOUNDATION_MODELS_JSON;

const CROSS_REGION_PREFIXES: &[&str] = &["global.", "apac.", "us.", "eu.", "jp.", "au."];

/// Provider-neutral metadata for one Bedrock foundation model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMetadata {
    pub model_id: String,
    pub model_name: String,
    pub provider_name: String,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub response_streaming_supported: Option<bool>,
    pub regions: Vec<String>,
    pub context_window_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub converse_supported: Option<bool>,
    pub invoke_supported: Option<bool>,
    pub bedrock_runtime_supported: Option<bool>,
    pub bedrock_mantle_supported: Option<bool>,
    pub mantle_regions: Vec<String>,
    pub model_card_url: Option<String>,
}

impl ModelMetadata {
    pub fn supports_vision_input(&self) -> bool {
        self.input_modalities
            .iter()
            .any(|modality| modality == "IMAGE")
    }

    /// Reports whether the catalog has enough evidence to classify this as an
    /// Agent Mode model. `None` deliberately leaves new or incomplete catalog
    /// entries usable until their metadata catches up.
    pub fn supports_agent_runtime(&self) -> Option<bool> {
        if !self.output_modalities.is_empty()
            && !self
                .output_modalities
                .iter()
                .any(|modality| modality == "TEXT")
        {
            return Some(false);
        }

        for support in [
            self.response_streaming_supported,
            self.converse_supported,
            self.bedrock_runtime_supported,
        ] {
            if support == Some(false) {
                return Some(false);
            }
        }

        if self.output_modalities.is_empty()
            || self.response_streaming_supported.is_none()
            || self.converse_supported.is_none()
            || self.bedrock_runtime_supported.is_none()
        {
            None
        } else {
            Some(true)
        }
    }
}

/// Returns all parsed foundation-model metadata in the bundled snapshot.
pub fn models() -> &'static [ModelMetadata] {
    &catalog_index().models
}

/// Finds metadata for a foundation model, accepting Bedrock ARNs,
/// cross-region inference prefixes, and Galaxy's optional `[1m]` suffix.
pub fn model_metadata(model_id: &str) -> Option<&'static ModelMetadata> {
    let index = catalog_index();
    let normalized = normalize_model_id(model_id);
    index
        .by_id
        .get(normalized)
        .and_then(|position| index.models.get(*position))
}

fn catalog_index() -> &'static CatalogIndex {
    static INDEX: OnceLock<CatalogIndex> = OnceLock::new();
    INDEX.get_or_init(CatalogIndex::load)
}

struct CatalogIndex {
    models: Vec<ModelMetadata>,
    by_id: HashMap<String, usize>,
}

impl CatalogIndex {
    fn load() -> Self {
        let raw_models =
            serde_json::from_str::<Vec<RawModel>>(FOUNDATION_MODELS_JSON).unwrap_or_default();
        let models = raw_models
            .into_iter()
            .map(ModelMetadata::from)
            .collect::<Vec<_>>();
        let by_id = models
            .iter()
            .enumerate()
            .map(|(position, model)| (model.model_id.clone(), position))
            .collect();
        Self { models, by_id }
    }
}

impl From<RawModel> for ModelMetadata {
    fn from(raw: RawModel) -> Self {
        let card = raw.model_card.unwrap_or_default();
        Self {
            model_id: raw.model_id,
            model_name: raw.model_name,
            provider_name: raw.provider_name,
            input_modalities: raw.input_modalities,
            output_modalities: raw.output_modalities,
            response_streaming_supported: raw.response_streaming_supported,
            regions: raw.regions,
            context_window_tokens: card.context_window.as_deref().and_then(parse_token_count),
            max_output_tokens: card
                .max_output_tokens
                .as_deref()
                .and_then(parse_token_count),
            converse_supported: card.apis_supported.as_ref().and_then(|apis| apis.converse),
            invoke_supported: card.apis_supported.as_ref().and_then(|apis| apis.invoke),
            bedrock_runtime_supported: card
                .endpoints_supported
                .as_ref()
                .and_then(|endpoints| endpoints.bedrock_runtime),
            bedrock_mantle_supported: card
                .endpoints_supported
                .as_ref()
                .and_then(|endpoints| endpoints.bedrock_mantle),
            mantle_regions: card.mantle_regions,
            model_card_url: card.model_card_url,
        }
    }
}

fn normalize_model_id(model_id: &str) -> &str {
    let model_id = model_id.rsplit('/').next().unwrap_or(model_id);
    let model_id = model_id
        .strip_suffix("[1m]")
        .or_else(|| model_id.strip_suffix("[1M]"))
        .unwrap_or(model_id);
    CROSS_REGION_PREFIXES
        .iter()
        .find_map(|prefix| model_id.strip_prefix(prefix))
        .unwrap_or(model_id)
}

fn parse_token_count(value: &str) -> Option<u32> {
    let value = value
        .trim()
        .strip_suffix("tokens")
        .or_else(|| value.trim().strip_suffix("token"))
        .unwrap_or(value)
        .trim();
    let suffix_start = value
        .find(|character: char| !character.is_ascii_digit() && character != ',' && character != '.')
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(suffix_start);
    let multiplier = match suffix.trim().to_ascii_uppercase().as_str() {
        "" => 1.0,
        "K" => 1_000.0,
        "M" => 1_000_000.0,
        "B" => 1_000_000_000.0,
        _ => return None,
    };
    let number = number.replace(',', "").parse::<f64>().ok()?;
    let tokens = number * multiplier;
    if !tokens.is_finite() || tokens < 0.0 || tokens > u32::MAX as f64 {
        return None;
    }
    Some(tokens.round() as u32)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawModel {
    model_id: String,
    #[serde(default)]
    model_name: String,
    #[serde(default)]
    provider_name: String,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
    #[serde(default)]
    response_streaming_supported: Option<bool>,
    #[serde(default)]
    regions: Vec<String>,
    #[serde(default)]
    model_card: Option<RawModelCard>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawModelCard {
    context_window: Option<String>,
    max_output_tokens: Option<String>,
    apis_supported: Option<RawApisSupported>,
    endpoints_supported: Option<RawEndpointsSupported>,
    #[serde(default)]
    mantle_regions: Vec<String>,
    model_card_url: Option<String>,
}

#[derive(Deserialize)]
struct RawApisSupported {
    converse: Option<bool>,
    invoke: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEndpointsSupported {
    bedrock_runtime: Option<bool>,
    bedrock_mantle: Option<bool>,
}
