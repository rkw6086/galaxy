use galaxyui_core::{Entity, ModelContext, SingletonEntity};
use warp_multi_agent_api as api;

pub use crate::aws_credentials::{AwsCredentials, AwsCredentialsState};
pub use crate::geap_credentials::{
    GeapCredentials, GeapCredentialsState, GeapFederation, GeapMintBinding,
    LoadGeapCredentialsError, GEAP_REFRESH_LEAD_TIME,
};

/// Emitted when the manager's stored credentials (AWS Bedrock or Gemini
/// Enterprise) are updated in-memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyManagerEvent {
    KeysUpdated,
}

/// Controls how AWS credentials are refreshed by [`ApiKeyManager`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AwsCredentialsRefreshStrategy {
    /// Load credentials from the local AWS credential chain (~/.aws). This is the default.
    #[default]
    LocalChain,
    /// Credentials are managed externally via OIDC/STS.
    /// The task ID is used to scope the STS AssumeRoleWithWebIdentity session.
    /// The role ARN + region are the info used to assume the IAM role via STS.
    OidcManaged {
        task_id: Option<String>,
        role_arn: String,
        region: String,
    },
}

/// A structure that manages locally-held credentials used to authenticate AI
/// provider requests: AWS Bedrock credentials and Gemini Enterprise (GEAP)
/// credentials.
pub struct ApiKeyManager {
    pub(crate) aws_credentials_state: AwsCredentialsState,
    aws_credentials_refresh_strategy: AwsCredentialsRefreshStrategy,
    /// In-memory Gemini Enterprise (GEAP) credential state.
    pub(crate) geap_credentials_state: GeapCredentialsState,
}

impl ApiKeyManager {
    pub fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            aws_credentials_state: AwsCredentialsState::Missing,
            aws_credentials_refresh_strategy: AwsCredentialsRefreshStrategy::default(),
            geap_credentials_state: GeapCredentialsState::Missing,
        }
    }

    pub fn set_aws_credentials_state(
        &mut self,
        state: AwsCredentialsState,
        ctx: &mut ModelContext<Self>,
    ) {
        self.aws_credentials_state = state;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
    }

    pub fn aws_credentials_state(&self) -> &AwsCredentialsState {
        &self.aws_credentials_state
    }

    pub fn set_geap_credentials_state(
        &mut self,
        state: GeapCredentialsState,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.geap_credentials_state == state {
            return;
        }
        self.geap_credentials_state = state;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
    }

    pub fn geap_credentials_state(&self) -> &GeapCredentialsState {
        &self.geap_credentials_state
    }

    pub fn aws_credentials_refresh_strategy(&self) -> AwsCredentialsRefreshStrategy {
        self.aws_credentials_refresh_strategy.clone()
    }

    pub fn set_aws_credentials_refresh_strategy(
        &mut self,
        strategy: AwsCredentialsRefreshStrategy,
    ) {
        self.aws_credentials_refresh_strategy = strategy;
    }

    /// Builds the `ApiKeys` request payload carrying AWS Bedrock and/or Gemini
    /// Enterprise (GEAP) credentials, when applicable. Returns `None` when
    /// neither credential type applies to this request.
    pub fn api_keys_for_request(
        &self,
        include_aws_bedrock_credentials: bool,
        geap_binding: Option<GeapMintBinding>,
    ) -> Option<api::request::settings::ApiKeys> {
        // Also include credentials when running with OIDC-managed Bedrock inference, regardless
        // of the per-user setting flag (which only applies to the local credential chain path).
        let include_aws = include_aws_bedrock_credentials
            || matches!(
                self.aws_credentials_refresh_strategy,
                AwsCredentialsRefreshStrategy::OidcManaged { .. }
            );
        let aws_credentials = include_aws
            .then(|| match self.aws_credentials_state {
                AwsCredentialsState::Loaded {
                    ref credentials, ..
                } => Some(credentials.clone().into()),
                _ => None,
            })
            .flatten();

        // Gemini Enterprise (GEAP) credentials attach only when the caller's
        // gate is on AND the stored token was minted for that same
        // (user, audience, SA) binding.
        let google_cloud_credentials: Option<
            api::request::settings::api_keys::GoogleCloudCredentials,
        > = geap_binding
            .as_ref()
            .and_then(|binding| match self.geap_credentials_state {
                GeapCredentialsState::Loaded {
                    ref credentials,
                    ref minted_for,
                    ..
                } if minted_for == binding => credentials
                    .access_token_for_request()
                    .map(|_| credentials.clone().into()),
                GeapCredentialsState::Refreshing {
                    previous: Some((ref credentials, ref minted_for)),
                } if minted_for == binding => credentials
                    .access_token_for_request()
                    .map(|_| credentials.clone().into()),
                _ => None,
            });

        if aws_credentials.is_none() && google_cloud_credentials.is_none() {
            None
        } else {
            Some(api::request::settings::ApiKeys {
                aws_credentials,
                google_cloud_credentials,
                ..Default::default()
            })
        }
    }
}

impl Entity for ApiKeyManager {
    type Event = ApiKeyManagerEvent;
}

impl SingletonEntity for ApiKeyManager {}

#[cfg(test)]
#[path = "api_keys_tests.rs"]
mod tests;
