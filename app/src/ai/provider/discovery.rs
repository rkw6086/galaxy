//! AWS Bedrock control-plane discovery.
//!
//! The foundation-model catalog is only a candidate list. Every candidate is
//! checked with `GetFoundationModelAvailability` before it is offered to the
//! user or persisted in Galaxy settings.

use aws_config::BehaviorVersion;
use aws_sdk_bedrock::Client;
use aws_sdk_bedrockruntime::config::Region;
use futures::{stream, StreamExt};
use galaxy_bedrock_model_catalog::models as catalog_models;
use serde_json::json;

use super::client::{BedrockClientConfig, BedrockError};
use crate::ai::remote_logging::{RemoteLogLevel, RemoteLogRecord};
use crate::settings::ai::BedrockModelConfig;

pub(crate) struct BedrockDiscovery {
    pub models: Vec<BedrockModelConfig>,
    pub records: Vec<RemoteLogRecord>,
}

const AVAILABILITY_CHECK_CONCURRENCY: usize = 8;

pub(crate) async fn discover_available_models(
    config: BedrockClientConfig,
) -> Result<BedrockDiscovery, String> {
    let aws_config = load_aws_config(&config)
        .await
        .map_err(|error| error.to_string())?;
    let client = Client::new(&aws_config);
    // The bundled catalog defines candidates; AWS defines account access.
    // Never add arbitrary entries from ListFoundationModels to this catalog.
    let candidates = catalog_models().to_vec();
    let sweep_id = uuid::Uuid::new_v4().to_string();
    let region = aws_config
        .region()
        .map(|region| region.as_ref())
        .unwrap_or("UNKNOWN")
        .to_owned();
    log::info!(
        "[bedrock] Starting availability sweep {sweep_id}: {} catalog models, region={region}",
        candidates.len()
    );
    let checks = candidates.into_iter().map(|metadata| {
        let client = client.clone();
        let sweep_id = sweep_id.clone();
        let region = region.clone();
        async move {
            let model_id = metadata.model_id.as_str();
            let availability = match client
                .get_foundation_model_availability()
                .model_id(model_id)
                .send()
                .await
            {
                Ok(availability) => availability,
                Err(error) => {
                    let error = crate::ai::remote_logging::sanitize_error(format!("{error:?}"));
                    log::warn!("[bedrock] Availability sweep {sweep_id}: model={model_id}, region={region}, decision=excluded, error={error}");
                    return (None, RemoteLogRecord {
                        level: RemoteLogLevel::Warn,
                        message: "Bedrock model availability check failed".to_owned(),
                        context: json!({"event": "bedrock_model_availability", "sweep_id": sweep_id, "model_id": model_id, "aws_region": region, "decision": "excluded", "error": error}),
                    });
                }
            };
            let agreement = availability.agreement_availability().map(|value| value.status().as_str());
            let authorization = availability.authorization_status().as_str();
            let entitlement = availability.entitlement_availability().as_str();
            let region_status = availability.region_availability().as_str();
            let access_allowed = model_availability_is_usable(agreement, authorization, entitlement, region_status);
            let runtime_supported = metadata.supports_agent_runtime() != Some(false);
            let allowed = access_allowed && runtime_supported;
            let decision = if allowed { "allowed" } else { "excluded" };
            log::info!("[bedrock] Availability sweep {sweep_id}: model={model_id}, agreement={}, authorization={authorization}, entitlement={entitlement}, region_status={region_status}, runtime_supported={runtime_supported}, decision={decision}", agreement.unwrap_or("MISSING"));
            let record = RemoteLogRecord {
                level: RemoteLogLevel::Info,
                message: "Bedrock model availability checked".to_owned(),
                context: json!({"event": "bedrock_model_availability", "sweep_id": sweep_id, "model_id": model_id, "aws_region": region, "agreement": agreement, "authorization": authorization, "entitlement": entitlement, "region_availability": region_status, "agreement_error": availability.agreement_availability().and_then(|value| value.error_message()).map(crate::ai::remote_logging::sanitize_error), "access_allowed": access_allowed, "runtime_supported": runtime_supported, "decision": decision}),
            };
            if !allowed { return (None, record); }

            (Some(BedrockModelConfig {
                model_id: model_id.to_owned(),
                display_name: metadata.model_name.clone(),
                vision_supported: metadata.supports_vision_input(),
                enabled: true,
                access_verified: true,
            }), record)
        }
    });
    let results = stream::iter(checks)
        .buffer_unordered(AVAILABILITY_CHECK_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let (models, mut records): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    let mut models = models.into_iter().flatten().collect::<Vec<_>>();
    models.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    log::info!(
        "[bedrock] Availability sweep {sweep_id} complete: checked={}, allowed={}",
        records.len(),
        models.len()
    );
    records.push(RemoteLogRecord {
        level: RemoteLogLevel::Info,
        message: "Bedrock availability sweep completed".to_owned(),
        context: json!({"event": "bedrock_availability_sweep_completed", "sweep_id": sweep_id, "aws_region": region, "catalog_count": catalog_models().len(), "checked_count": records.len(), "allowed_count": models.len()}),
    });
    Ok(BedrockDiscovery { models, records })
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

pub(crate) fn preserve_model_toggles(
    existing: &[BedrockModelConfig],
    mut discovered: Vec<BedrockModelConfig>,
) -> Vec<BedrockModelConfig> {
    for model in &mut discovered {
        if let Some(previous) = existing
            .iter()
            .find(|previous| previous.model_id == model.model_id)
        {
            model.enabled = previous.enabled;
        }
    }
    discovered
}

async fn load_aws_config(
    config: &BedrockClientConfig,
) -> Result<aws_config::SdkConfig, BedrockError> {
    let sdk_config = match config.auth_method {
        crate::settings::ai::BedrockAuthMethod::Profile
        | crate::settings::ai::BedrockAuthMethod::Sso => {
            let mut loader = aws_config::defaults(BehaviorVersion::latest());
            if !config.profile.is_empty() {
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

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
