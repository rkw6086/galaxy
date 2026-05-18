use crate::settings::ai::BedrockModelConfig;

use super::external_config::ExternalBedrockConfig;

pub struct DefaultModel {
    pub model_id: &'static str,
    pub display_name: &'static str,
    pub vision_supported: bool,
}

pub const DEFAULT_BEDROCK_MODELS: &[DefaultModel] = &[
    DefaultModel {
        model_id: "anthropic.claude-opus-4-6",
        display_name: "Claude Opus 4.6",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "anthropic.claude-opus-4-7",
        display_name: "Claude Opus 4.7",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "anthropic.claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "anthropic.claude-sonnet-4-20250514-v1:0",
        display_name: "Claude Sonnet 4",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "anthropic.claude-haiku-4-5-20251001-v1:0",
        display_name: "Claude Haiku 4.5",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "amazon.nova-pro-v1:0",
        display_name: "Amazon Nova Pro",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "amazon.nova-lite-v1:0",
        display_name: "Amazon Nova Lite",
        vision_supported: true,
    },
    DefaultModel {
        model_id: "amazon.nova-micro-v1:0",
        display_name: "Amazon Nova Micro",
        vision_supported: false,
    },
    DefaultModel {
        model_id: "deepseek.r1-v1:0",
        display_name: "DeepSeek R1",
        vision_supported: false,
    },
];

pub fn get_effective_models(user_models: &[BedrockModelConfig]) -> Vec<BedrockModelConfig> {
    if !user_models.is_empty() {
        return user_models.to_vec();
    }

    // Fall back to models from external configs (Claude Code / OpenCode)
    let external = ExternalBedrockConfig::load();
    if !external.models.is_empty() {
        log::info!(
            "[bedrock] Using {} model(s) from external config",
            external.models.len()
        );
        // Merge external models with defaults so the user still sees all defaults
        let mut models = external.models;
        let defaults: Vec<BedrockModelConfig> = DEFAULT_BEDROCK_MODELS
            .iter()
            .map(|m| BedrockModelConfig {
                model_id: m.model_id.to_string(),
                display_name: m.display_name.to_string(),
                vision_supported: m.vision_supported,
            })
            .collect();
        for default in defaults {
            if !models.iter().any(|m| m.model_id == default.model_id) {
                models.push(default);
            }
        }
        return models;
    }

    DEFAULT_BEDROCK_MODELS
        .iter()
        .map(|m| BedrockModelConfig {
            model_id: m.model_id.to_string(),
            display_name: m.display_name.to_string(),
            vision_supported: m.vision_supported,
        })
        .collect()
}

pub fn apply_cross_region_prefix(model_id: &str, region: &str) -> String {
    if model_id.starts_with("arn:") {
        return model_id.to_string();
    }

    if model_id.contains('.') && model_id.split('.').next().unwrap_or("").len() <= 6 {
        return model_id.to_string();
    }

    let prefix = match region {
        r if r.starts_with("us-") || r.starts_with("ca-") => "us",
        r if r.starts_with("eu-") || r == "il-central-1" => "eu",
        r if r == "ap-northeast-1" || r == "ap-northeast-3" => "jp",
        r if r == "ap-southeast-2" || r == "ap-southeast-4" || r == "ap-southeast-6" => "au",
        r if r.starts_with("ap-") => "apac",
        _ => return model_id.to_string(),
    };
    format!("{}.{}", prefix, model_id)
}

pub fn is_bedrock_model(model_id: &str, configured_models: &[BedrockModelConfig]) -> bool {
    if model_id.starts_with("arn:aws:bedrock:") {
        return true;
    }

    let effective = get_effective_models(configured_models);
    effective.iter().any(|m| m.model_id == model_id)
        || model_id.starts_with("anthropic.")
        || model_id.starts_with("amazon.")
        || model_id.starts_with("meta.")
        || model_id.starts_with("mistral.")
        || model_id.starts_with("cohere.")
        || model_id.starts_with("ai21.")
        || model_id.starts_with("deepseek.")
        || has_cross_region_prefix(model_id)
}

fn has_cross_region_prefix(model_id: &str) -> bool {
    let prefixes = ["us.", "eu.", "jp.", "apac.", "au.", "global."];
    prefixes.iter().any(|p| model_id.starts_with(p))
}
