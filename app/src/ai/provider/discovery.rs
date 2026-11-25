//! AWS Bedrock control-plane discovery.
//!
//! AWS supplies the model inventory and invocation routes. Bundled metadata
//! enriches known capabilities but never determines inventory membership.

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::future::Future;

use aws_config::BehaviorVersion;
use aws_sdk_bedrock::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_bedrock::operation::list_inference_profiles::ListInferenceProfilesOutput;
use aws_sdk_bedrock::types::{
    FoundationModelSummary, InferenceProfileStatus, InferenceProfileSummary, InferenceProfileType,
};
use aws_sdk_bedrock::Client;
use aws_sdk_bedrockruntime::config::Region;
use aws_sdk_sts::operation::assume_role::AssumeRoleError;
use futures::{stream, StreamExt};
use galaxy_bedrock_model_catalog::model_metadata;
use serde_json::json;

use super::client::{BedrockClientConfig, BedrockError};
use super::models::foundation_model_id;
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
    let inventory = client
        .list_foundation_models()
        .send()
        .await
        .map_err(|error| discovery_request_error("Unable to list Bedrock models", &error))?;
    let mut foundation_models = inventory.model_summaries.unwrap_or_default();
    let profiles = if config.cross_region_inference {
        collect_profile_pages(|token| {
            let client = client.clone();
            async move {
                client
                    .list_inference_profiles()
                    .set_next_token(token)
                    .send()
                    .await
                    .map_err(|error| {
                        discovery_request_error("Unable to list Bedrock inference profiles", &error)
                    })
            }
        })
        .await?
    } else {
        Vec::new()
    };
    let mut known_ids: HashSet<String> = foundation_models
        .iter()
        .map(|model| model.model_id().to_owned())
        .collect();
    // A profile can introduce a model before it appears in the regional listing.
    for profile in profiles
        .iter()
        .filter(|profile| profile.status() == &InferenceProfileStatus::Active)
    {
        for model in profile.models() {
            let Some(arn) = model.model_arn() else {
                continue;
            };
            if !arn.contains(":foundation-model/") {
                continue;
            }
            let id = foundation_model_id(arn);
            if !known_ids.insert(id.to_owned()) {
                continue;
            }
            let details = match client
                .get_foundation_model()
                .model_identifier(id)
                .send()
                .await
            {
                Ok(output) => match output.model_details {
                    Some(details) => details,
                    None => {
                        log::warn!("[bedrock] AWS returned no metadata for profile model {id}");
                        continue;
                    }
                },
                Err(error) => {
                    let error = sanitized_error(&error);
                    log::warn!("[bedrock] Unable to describe profile model {id}: {error}");
                    continue;
                }
            };
            let summary = FoundationModelSummary::builder()
                .model_id(details.model_id)
                .model_arn(details.model_arn)
                .set_model_name(details.model_name)
                .set_provider_name(details.provider_name)
                .set_input_modalities(details.input_modalities)
                .set_output_modalities(details.output_modalities)
                .set_response_streaming_supported(details.response_streaming_supported)
                .set_inference_types_supported(details.inference_types_supported)
                .set_model_lifecycle(details.model_lifecycle)
                .build()
                .map_err(|error| format!("Invalid Bedrock model metadata: {error}"))?;
            foundation_models.push(summary);
        }
    }
    let inventory_count = foundation_models.len();
    let candidates =
        models_from_inventory(foundation_models, &profiles, config.cross_region_inference);
    let candidate_count = candidates.len();
    let sweep_id = uuid::Uuid::new_v4().to_string();
    let region = aws_config
        .region()
        .map(|region| region.as_ref())
        .unwrap_or("UNKNOWN")
        .to_owned();
    log::info!(
        "[bedrock] Starting availability sweep {sweep_id}: {} live candidates, region={region}",
        candidates.len()
    );
    let checks = candidates.into_iter().map(|mut model| {
        let client = client.clone();
        let sweep_id = sweep_id.clone();
        let region = region.clone();
        async move {
            let model_id = model.model_id.clone();
            let availability = match client
                .get_foundation_model_availability()
                .model_id(&model_id)
                .send()
                .await
            {
                Ok(availability) => availability,
                Err(error) => {
                    let error = sanitized_error(&error);
                    log::warn!("[bedrock] Availability sweep {sweep_id}: model={model_id}, region={region}, decision=excluded, error={error}");
                    return (None, RemoteLogRecord {
                        level: RemoteLogLevel::Warn,
                        message: "Bedrock model availability check failed".to_owned(),
                        context: json!({"event": "bedrock_model_availability", "sweep_id": sweep_id, "model_id": model_id, "aws_region": region, "decision": "excluded", "error": error}),
                    }, false);
                }
            };
            let agreement = availability.agreement_availability().map(|value| value.status().as_str());
            let authorization = availability.authorization_status().as_str();
            let entitlement = availability.entitlement_availability().as_str();
            let region_status = availability.region_availability().as_str();
            let access_allowed = model_availability_is_usable(agreement, authorization, entitlement, region_status);
            let runtime_supported = model.runtime_supported != Some(false);
            let allowed = access_allowed && runtime_supported;
            let decision = if allowed { "allowed" } else { "excluded" };
            log::info!("[bedrock] Availability sweep {sweep_id}: model={model_id}, agreement={}, authorization={authorization}, entitlement={entitlement}, region_status={region_status}, runtime_supported={runtime_supported}, decision={decision}", agreement.unwrap_or("MISSING"));
            let record = RemoteLogRecord {
                level: RemoteLogLevel::Info,
                message: "Bedrock model availability checked".to_owned(),
                context: json!({"event": "bedrock_model_availability", "sweep_id": sweep_id, "model_id": model_id, "aws_region": region, "agreement": agreement, "authorization": authorization, "entitlement": entitlement, "region_availability": region_status, "agreement_error": availability.agreement_availability().and_then(|value| value.error_message()).map(crate::ai::remote_logging::sanitize_error), "access_allowed": access_allowed, "runtime_supported": runtime_supported, "decision": decision}),
            };
            model.access_verified = allowed;
            (allowed.then_some(model), record, true)
        }
    });
    let results = stream::iter(checks)
        .buffer_unordered(AVAILABILITY_CHECK_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    if candidate_count > 0 && !results.iter().any(|(_, _, checked)| *checked) {
        return Err(
            "All Bedrock availability checks failed; saved model preferences are unchanged"
                .to_owned(),
        );
    }

    let (models, mut records): (Vec<_>, Vec<_>) = results
        .into_iter()
        .map(|(model, record, _)| (model, record))
        .unzip();
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
        context: json!({"event": "bedrock_availability_sweep_completed", "sweep_id": sweep_id, "aws_region": region, "inventory_count": inventory_count, "candidate_count": candidate_count, "checked_count": records.len(), "allowed_count": models.len()}),
    });
    Ok(BedrockDiscovery { models, records })
}

fn discovery_request_error<E, R>(operation: &str, error: &SdkError<E, R>) -> String
where
    E: Error + ProvideErrorMetadata + 'static,
    R: std::fmt::Debug,
{
    // With an assume-role profile, the Bedrock request fails during dispatch.
    // The AWS SDK exposes the original STS service error through its sources.
    let role_codes = std::iter::successors(error.source(), |&cause| cause.source())
        .filter_map(|cause| cause.downcast_ref::<AssumeRoleError>())
        .filter_map(ProvideErrorMetadata::code);
    let mut known_code = None;
    for code in error.code().into_iter().chain(role_codes) {
        match code {
            "ExpiredToken" => {
                return "ExpiredToken: The security token included in the request is expired"
                    .to_owned();
            }
            // Only emit recognized codes, not arbitrary metadata or service messages.
            "AccessDenied"
            | "AccessDeniedException"
            | "InternalServerException"
            | "MalformedPolicyDocumentException"
            | "PackedPolicyTooLargeException"
            | "RegionDisabledException"
            | "ThrottlingException"
            | "ValidationException" => {
                known_code.get_or_insert(code);
            }
            _ => {}
        }
    }

    if let Some(code) = known_code {
        format!("{operation}: {code}")
    } else {
        format!("{operation}: {error}")
    }
}

fn sanitized_error(error: &impl std::fmt::Debug) -> String {
    crate::ai::remote_logging::sanitize_error(format!("{error:?}"))
}

async fn collect_profile_pages<F, Fut>(mut fetch: F) -> Result<Vec<InferenceProfileSummary>, String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<ListInferenceProfilesOutput, String>>,
{
    let mut profiles = Vec::new();
    let mut token = None;
    let mut seen = HashSet::new();
    loop {
        let page = fetch(token).await?;
        profiles.extend(page.inference_profile_summaries.unwrap_or_default());
        token = page.next_token.filter(|token| !token.is_empty());
        let Some(next) = &token else {
            return Ok(profiles);
        };
        if !seen.insert(next.clone()) {
            return Err("AWS repeated an inference-profile pagination token".to_owned());
        }
    }
}

fn models_from_inventory(
    foundation_models: Vec<FoundationModelSummary>,
    profiles: &[InferenceProfileSummary],
    cross_region: bool,
) -> Vec<BedrockModelConfig> {
    let mut routes = profiles
        .iter()
        .filter(|profile| profile.status() == &InferenceProfileStatus::Active)
        .collect::<Vec<_>>();
    routes.sort_by_key(|profile| {
        (
            profile.r#type() != &InferenceProfileType::SystemDefined,
            profile.inference_profile_id().starts_with("global."),
            profile.inference_profile_id().to_owned(),
        )
    });
    let mut models = BTreeMap::new();
    for summary in foundation_models {
        let id = summary.model_id();
        let metadata = model_metadata(id);
        let text_output = summary
            .output_modalities()
            .iter()
            .any(|modality| modality.as_str() == "TEXT");
        let incompatible = (!summary.output_modalities().is_empty() && !text_output)
            || summary.response_streaming_supported() == Some(false)
            || metadata.is_some_and(|metadata| metadata.supports_agent_runtime() == Some(false));
        if incompatible {
            continue;
        }
        let profile = cross_region
            .then(|| {
                routes.iter().find(|profile| {
                    !profile.models().is_empty()
                        && profile.models().iter().all(|model| {
                            model
                                .model_arn()
                                .is_some_and(|arn| foundation_model_id(arn) == id)
                        })
                })
            })
            .flatten();
        let invocation = if let Some(profile) = profile {
            profile.inference_profile_arn().to_owned()
        } else if summary
            .inference_types_supported()
            .iter()
            .any(|kind| kind.as_str() == "ON_DEMAND")
        {
            id.to_owned()
        } else {
            continue;
        };
        models
            .entry(id.to_owned())
            .or_insert_with(|| BedrockModelConfig {
                model_id: id.to_owned(),
                invocation_model_id: invocation,
                display_name: summary.model_name().unwrap_or(id).to_owned(),
                provider_name: summary.provider_name().unwrap_or_default().to_owned(),
                vision_supported: summary
                    .input_modalities()
                    .iter()
                    .any(|modality| modality.as_str() == "IMAGE"),
                response_streaming_supported: summary.response_streaming_supported(),
                runtime_supported: (text_output
                    && summary.response_streaming_supported() == Some(true))
                .then_some(true),
                ..Default::default()
            });
    }
    models.into_values().collect()
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
            model.display_name.clone_from(&previous.display_name);
        }
    }
    for previous in existing {
        if !discovered
            .iter()
            .any(|model| model.model_id == previous.model_id)
        {
            let mut preserved = previous.clone();
            preserved.access_verified = false;
            discovered.push(preserved);
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
                config.credentials_expiry,
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
