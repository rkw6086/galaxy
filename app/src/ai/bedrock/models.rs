#![allow(dead_code)]

use crate::settings::ai::BedrockModelConfig;

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
