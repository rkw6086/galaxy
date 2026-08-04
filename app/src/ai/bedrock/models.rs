#![allow(dead_code)]

use super::external_config::ExternalBedrockConfig;
use crate::settings::ai::BedrockModelConfig;

pub struct DefaultModel {
    pub model_id: &'static str,
    pub display_name: &'static str,
    pub vision_supported: bool,
    pub context_size: u32,
}

pub const DEFAULT_BEDROCK_MODELS: &[DefaultModel] = &[
    DefaultModel {
        model_id: "us.anthropic.claude-opus-4-6-v1[1m]",
        display_name: "Claude Opus 4.6 (1M)",
        vision_supported: true,
        context_size: 1_000_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-sonnet-4-6[1m]",
        display_name: "Claude Sonnet 4.6 (1M)",
        vision_supported: true,
        context_size: 1_000_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        display_name: "Claude Sonnet 4.5",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-sonnet-4-20250514-v1:0",
        display_name: "Claude Sonnet 4",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-3-sonnet-20240229-v1:0",
        display_name: "Claude 3 Sonnet",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "global.anthropic.claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6 (Global)",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        display_name: "Claude Sonnet 4.5 (Global)",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "global.anthropic.claude-sonnet-4-20250514-v1:0",
        display_name: "Claude Sonnet 4 (Global)",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-opus-4-5-20251101-v1:0",
        display_name: "Claude Opus 4.5",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-opus-4-1-20250805-v1:0",
        display_name: "Claude Opus 4.1",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "global.anthropic.claude-opus-4-6-v1",
        display_name: "Claude Opus 4.6 (Global)",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "global.anthropic.claude-opus-4-5-20251101-v1:0",
        display_name: "Claude Opus 4.5 (Global)",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        display_name: "Claude Haiku 4.5",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-3-haiku-20240307-v1:0",
        display_name: "Claude 3 Haiku",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "us.anthropic.claude-3-5-haiku-20241022-v1:0",
        display_name: "Claude 3.5 Haiku",
        vision_supported: true,
        context_size: 200_000,
    },
    DefaultModel {
        model_id: "global.anthropic.claude-haiku-4-5-20251001-v1:0",
        display_name: "Claude Haiku 4.5 (Global)",
        vision_supported: true,
        context_size: 200_000,
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
                use_rig: false,
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
            use_rig: false,
        })
        .collect()
}

pub fn configured_model_uses_rig(
    selected_model_id: &str,
    configured_models: &[BedrockModelConfig],
    region: &str,
    cross_region_inference: bool,
) -> bool {
    let selected_model_id = strip_context_marker(selected_model_id);
    configured_models.iter().any(|model| {
        if !model.use_rig {
            return false;
        }
        let configured_model_id = strip_context_marker(&model.model_id);
        if configured_model_id == selected_model_id {
            return true;
        }
        galaxy_agent_rig::resolve_bedrock_model_id(&model.model_id, region, cross_region_inference)
            .is_ok_and(|resolved| strip_context_marker(&resolved) == selected_model_id)
    })
}

fn strip_context_marker(model_id: &str) -> &str {
    model_id
        .strip_suffix("[1m]")
        .or_else(|| model_id.strip_suffix("[1M]"))
        .unwrap_or(model_id)
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
