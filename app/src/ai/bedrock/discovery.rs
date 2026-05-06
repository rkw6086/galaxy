use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_bedrock::types::InferenceProfileType;
use aws_sdk_bedrock::Client as BedrockControlClient;
use aws_sdk_bedrockruntime::config::Region;

use crate::settings::ai::{BedrockAuthMethod, BedrockModelConfig};

use super::client::BedrockClientConfig;

pub fn list_aws_profiles() -> Vec<String> {
    let mut profiles = BTreeSet::new();

    if let Some(home) = dirs::home_dir() {
        parse_config_file(home.join(".aws").join("config"), &mut profiles);
        parse_credentials_file(home.join(".aws").join("credentials"), &mut profiles);
    }

    profiles.into_iter().collect()
}

fn parse_config_file(path: PathBuf, profiles: &mut BTreeSet<String>) {
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = &trimmed[1..trimmed.len() - 1];
            if section == "default" {
                profiles.insert("default".to_string());
            } else if let Some(name) = section.strip_prefix("profile ") {
                profiles.insert(name.trim().to_string());
            }
        }
    }
}

fn parse_credentials_file(path: PathBuf, profiles: &mut BTreeSet<String>) {
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            profiles.insert(name.trim().to_string());
        }
    }
}

pub async fn discover_inference_profiles(
    config: &BedrockClientConfig,
) -> Result<Vec<BedrockModelConfig>> {
    let aws_config = build_aws_config(config).await;
    let client = BedrockControlClient::new(&aws_config);

    let mut models = Vec::new();

    fetch_profiles_by_type(&client, InferenceProfileType::SystemDefined, &mut models).await?;
    fetch_profiles_by_type(&client, InferenceProfileType::Application, &mut models).await?;

    models.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    models.dedup_by(|a, b| a.model_id == b.model_id);

    Ok(models)
}

async fn fetch_profiles_by_type(
    client: &BedrockControlClient,
    profile_type: InferenceProfileType,
    models: &mut Vec<BedrockModelConfig>,
) -> Result<()> {
    let mut next_token: Option<String> = None;

    loop {
        let mut req = client
            .list_inference_profiles()
            .type_equals(profile_type.clone())
            .max_results(100);
        if let Some(token) = next_token.take() {
            req = req.next_token(token);
        }

        let resp = req.send().await?;

        for summary in resp.inference_profile_summaries() {
            let profile_id = summary.inference_profile_id();
            let profile_name = summary.inference_profile_name();
            let profile_arn = summary.inference_profile_arn();

            let model_id = if profile_arn.contains(":application-inference-profile/") {
                profile_arn.to_string()
            } else {
                profile_id.to_string()
            };

            if should_skip_model(profile_name) {
                continue;
            }

            models.push(BedrockModelConfig {
                model_id,
                display_name: profile_name.to_string(),
                vision_supported: true,
            });
        }

        next_token = resp.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }

    Ok(())
}

fn should_skip_model(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("embed")
        || lower.contains("stable image")
        || lower.contains("stable-image")
        || lower.contains("upscale")
        || lower.contains("outpaint")
        || lower.contains("inpaint")
        || lower.contains("recolor")
        || lower.contains("erase")
        || lower.contains("style transfer")
        || lower.contains("style guide")
        || lower.contains("remove background")
        || lower.contains("search and replace")
        || lower.contains("control sketch")
        || lower.contains("control structure")
}

async fn build_aws_config(config: &BedrockClientConfig) -> aws_config::SdkConfig {
    match config.auth_method {
        BedrockAuthMethod::Profile | BedrockAuthMethod::Sso => {
            let mut loader =
                aws_config::defaults(BehaviorVersion::latest()).profile_name(&config.profile);

            if !config.region.is_empty() {
                loader = loader.region(Region::new(config.region.clone()));
            }

            loader.load().await
        }
        BedrockAuthMethod::StaticKeys => {
            let creds = aws_credential_types::Credentials::new(
                &config.access_key_id,
                &config.secret_access_key,
                None,
                None,
                "warp-bedrock-discovery",
            );

            let mut loader =
                aws_config::defaults(BehaviorVersion::latest()).credentials_provider(creds);

            if !config.region.is_empty() {
                loader = loader.region(Region::new(config.region.clone()));
            } else {
                loader = loader.region(Region::new("us-east-1".to_string()));
            }

            loader.load().await
        }
    }
}
