use std::collections::HashMap;
use std::path::PathBuf;

use crate::settings::ai::BedrockModelConfig;

#[derive(Debug, Clone, Default)]
pub struct ExternalBedrockConfig {
    pub profile: Option<String>,
    pub region: Option<String>,
    pub models: Vec<BedrockModelConfig>,
}

impl ExternalBedrockConfig {
    pub fn load() -> Self {
        let claude_config = load_claude_code_config();
        let opencode_config = load_opencode_config();

        // Claude Code takes priority over OpenCode since it has richer model mappings
        Self {
            profile: claude_config
                .profile
                .or(opencode_config.profile),
            region: claude_config
                .region
                .or(opencode_config.region),
            models: if claude_config.models.is_empty() {
                opencode_config.models
            } else {
                claude_config.models
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.profile.is_none() && self.region.is_none() && self.models.is_empty()
    }
}

fn load_claude_code_config() -> ExternalBedrockConfig {
    let path = dirs::home_dir()
        .map(|h| h.join(".claude").join("settings.json"))
        .unwrap_or_default();

    parse_claude_code_config(path)
}

fn parse_claude_code_config(path: PathBuf) -> ExternalBedrockConfig {
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return ExternalBedrockConfig::default(),
    };

    let json: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return ExternalBedrockConfig::default(),
    };

    let env = match json.get("env").and_then(|v| v.as_object()) {
        Some(e) => e,
        None => return ExternalBedrockConfig::default(),
    };

    let profile = env
        .get("AWS_PROFILE")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let region = env
        .get("AWS_REGION")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let models = parse_claude_code_model_map(env);

    ExternalBedrockConfig {
        profile,
        region,
        models,
    }
}

fn parse_claude_code_model_map(
    env: &serde_json::Map<String, serde_json::Value>,
) -> Vec<BedrockModelConfig> {
    let model_map_str = match env.get("DCP_MODEL_MAP").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Vec::new(),
    };

    let model_map: HashMap<String, String> = match serde_json::from_str(model_map_str) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    model_map
        .into_iter()
        .map(|(source_id, arn)| {
            let display_name = derive_display_name(&source_id);
            BedrockModelConfig {
                model_id: arn,
                display_name,
                vision_supported: true,
            }
        })
        .collect()
}

fn derive_display_name(model_id: &str) -> String {
    // Strip region prefix (e.g. "us." from "us.anthropic.claude-opus-4-6-v1")
    let id = model_id
        .strip_prefix("us.")
        .or_else(|| model_id.strip_prefix("eu."))
        .or_else(|| model_id.strip_prefix("ap."))
        .or_else(|| model_id.strip_prefix("global."))
        .unwrap_or(model_id);

    // Strip vendor prefix
    let id = id
        .strip_prefix("anthropic.")
        .or_else(|| id.strip_prefix("amazon."))
        .or_else(|| id.strip_prefix("meta."))
        .unwrap_or(id);

    // Convert model slug to display name
    // e.g. "claude-opus-4-6-v1" -> "Claude Opus 4.6"
    // e.g. "claude-sonnet-4-5-20250929-v1:0" -> "Claude Sonnet 4.5"
    prettify_model_slug(id)
}

fn prettify_model_slug(slug: &str) -> String {
    // Remove version suffixes like "-v1:0", "-v1", "-v2:0"
    let slug = slug
        .split("-v")
        .next()
        .unwrap_or(slug);

    // Remove date suffixes like "-20250514" or "-20251001"
    let parts: Vec<&str> = slug.split('-').collect();
    let mut cleaned: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        // Skip parts that look like dates (8 digits)
        if parts[i].len() == 8 && parts[i].chars().all(|c| c.is_ascii_digit()) {
            i += 1;
            continue;
        }
        cleaned.push(parts[i]);
        i += 1;
    }

    // Try to detect version numbers: consecutive single-digit parts become "X.Y"
    let mut result = String::new();
    let mut i = 0;
    while i < cleaned.len() {
        if i > 0 {
            result.push(' ');
        }

        // Check if this and the next part form a version number (e.g. "4" "6" -> "4.6")
        if cleaned[i].len() == 1
            && cleaned[i].chars().all(|c| c.is_ascii_digit())
            && i + 1 < cleaned.len()
            && cleaned[i + 1].len() == 1
            && cleaned[i + 1].chars().all(|c| c.is_ascii_digit())
        {
            result.push_str(cleaned[i]);
            result.push('.');
            result.push_str(cleaned[i + 1]);
            i += 2;
            continue;
        }

        // Capitalize first letter of each word
        let word = cleaned[i];
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars);
        }
        i += 1;
    }

    result
}

fn load_opencode_config() -> ExternalBedrockConfig {
    let path = dirs::home_dir()
        .map(|h| h.join(".config").join("opencode").join("opencode.json"))
        .unwrap_or_default();

    parse_opencode_config(path)
}

fn parse_opencode_config(path: PathBuf) -> ExternalBedrockConfig {
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return ExternalBedrockConfig::default(),
    };

    let json: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return ExternalBedrockConfig::default(),
    };

    let bedrock_opts = json
        .get("provider")
        .and_then(|p| p.get("amazon-bedrock"))
        .and_then(|b| b.get("options"));

    let profile = bedrock_opts
        .and_then(|o| o.get("profile"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let region = bedrock_opts
        .and_then(|o| o.get("region"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    ExternalBedrockConfig {
        profile,
        region,
        models: Vec::new(),
    }
}

#[cfg(test)]
#[path = "external_config_tests.rs"]
mod tests;
