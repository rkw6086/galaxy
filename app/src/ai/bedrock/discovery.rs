//! AWS Bedrock control-plane discovery.
//!
//! The foundation-model catalog is only a candidate list. Every candidate is
//! checked with `GetFoundationModelAvailability` before it is offered to the
//! user or persisted in Galaxy settings.

use aws_config::BehaviorVersion;
use aws_sdk_bedrock::Client;
use aws_sdk_bedrockruntime::config::Region;
use futures::{stream, StreamExt};
use galaxy_bedrock_model_catalog::model_metadata;

use super::client::{BedrockClientConfig, BedrockError};
use crate::settings::ai::BedrockModelConfig;

const AVAILABILITY_CHECK_CONCURRENCY: usize = 8;

pub async fn discover_available_models(
    config: BedrockClientConfig,
) -> Result<Vec<BedrockModelConfig>, String> {
    let aws_config = load_aws_config(&config)
        .await
        .map_err(|error| error.to_string())?;
    let client = Client::new(&aws_config);
    let catalog = client
        .list_foundation_models()
        .send()
        .await
        .map_err(|error| format!("Could not list AWS Bedrock foundation models: {error}"))?;

    // AWS exposes agreement/authorization state only through an individual
    // GetFoundationModelAvailability call. Keep using that control-plane API,
    // but bound the independent checks so a large catalog does not serialize
    // startup discovery one model at a time.
    let checks = catalog.model_summaries().iter().cloned().map(|summary| {
        let client = client.clone();
        async move {
            let model_id = summary.model_id();
            let availability = match client
                .get_foundation_model_availability()
                .model_id(model_id)
                .send()
                .await
            {
                Ok(availability) => availability,
                Err(error) => {
                    log::debug!(
                        "[bedrock] Availability check failed for {model_id}; excluding model: {error}"
                    );
                    return None;
                }
            };

            if !model_availability_is_usable(
                availability
                    .agreement_availability()
                    .map(|agreement| agreement.status().as_str()),
                availability.authorization_status().as_str(),
                availability.entitlement_availability().as_str(),
                availability.region_availability().as_str(),
            ) {
                log::debug!(
                    "[bedrock] Excluding {model_id}: agreement={}, authorization={}, entitlement={}, region={}",
                    availability
                        .agreement_availability()
                        .map(|agreement| agreement.status().as_str())
                        .unwrap_or("MISSING"),
                    availability.authorization_status().as_str(),
                    availability.entitlement_availability().as_str(),
                    availability.region_availability().as_str(),
                );
                return None;
            }

            let metadata = model_metadata(model_id);
            let display_name = summary.model_name().map(str::to_owned).unwrap_or_else(|| {
                metadata
                    .map(|metadata| metadata.model_name.clone())
                    .unwrap_or_else(|| prettify_model_id(model_id))
            });
            let vision_supported = summary
                .input_modalities()
                .iter()
                .any(|modality| modality.as_str() == "IMAGE")
                || metadata.is_some_and(|metadata| metadata.supports_vision_input());

            Some(BedrockModelConfig {
                model_id: model_id.to_owned(),
                display_name,
                vision_supported,
                use_rig: true,
            })
        }
    });
    let mut models = stream::iter(checks)
        .buffer_unordered(AVAILABILITY_CHECK_CONCURRENCY)
        .filter_map(futures::future::ready)
        .collect::<Vec<_>>()
        .await;

    models.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    if models.is_empty() {
        return Err(
            "AWS returned no Bedrock models that are authorized and available in this region."
                .to_string(),
        );
    }
    Ok(models)
}

fn model_availability_is_usable(
    agreement_status: Option<&str>,
    authorization_status: &str,
    entitlement_status: &str,
    region_status: &str,
) -> bool {
    agreement_status == Some("AVAILABLE")
        && authorization_status == "AUTHORIZED"
        && entitlement_status == "AVAILABLE"
        && region_status == "AVAILABLE"
}

async fn load_aws_config(
    config: &BedrockClientConfig,
) -> Result<aws_config::SdkConfig, BedrockError> {
    let sdk_config = match config.auth_method {
        crate::settings::ai::BedrockAuthMethod::Profile
        | crate::settings::ai::BedrockAuthMethod::Sso => {
            let mut loader = aws_config::defaults(BehaviorVersion::latest());
            if !config.profile.is_empty() && config.profile != "default" {
                loader = loader.profile_name(&config.profile);
            }
            if !config.region.is_empty() {
                loader = loader.region(Region::new(config.region.clone()));
            }
            loader.load().await
        }
        crate::settings::ai::BedrockAuthMethod::StaticKeys => {
            if config.access_key_id.is_empty() || config.secret_access_key.is_empty() {
                return Err(BedrockError::CredentialsNotConfigured);
            }
            let credentials = aws_credential_types::Credentials::new(
                &config.access_key_id,
                &config.secret_access_key,
                config.session_token.clone(),
                None,
                "galaxy-bedrock-discovery",
            );
            let mut loader =
                aws_config::defaults(BehaviorVersion::latest()).credentials_provider(credentials);
            loader = loader.region(Region::new(if config.region.is_empty() {
                "us-east-1".to_string()
            } else {
                config.region.clone()
            }));
            loader.load().await
        }
    };

    if sdk_config.region().is_none() {
        return Err(BedrockError::RegionNotConfigured);
    }
    Ok(sdk_config)
}

fn prettify_model_id(model_id: &str) -> String {
    model_id
        .rsplit('.')
        .next()
        .unwrap_or(model_id)
        .replace(['-', ':'], " ")
}

#[cfg(test)]
mod tests {
    use super::model_availability_is_usable;

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
}
