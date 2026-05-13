use std::result::Result as StdResult;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use firebase::FirebaseError;
use galaxy_core::errors::{AnyhowErrorExt, ErrorExt};
use galaxy_graphql::mutations::expire_api_key::ExpireApiKeyResult;
use galaxy_graphql::queries::get_conversation_usage::ConversationUsage;
use instant::Duration;
#[cfg(test)]
use mockall::{automock, predicate::*};
use thiserror::Error;

use galaxy_graphql::mutations::create_anonymous_user::{
    AnonymousUserType, CreateAnonymousUserResult,
};
use galaxy_graphql::mutations::generate_api_key::GenerateApiKeyResult;
use galaxy_graphql::mutations::mint_custom_token::MintCustomTokenResult;
use galaxy_graphql::queries::api_keys::ApiKeyProperties;
use galaxy_graphql::queries::get_user::UserOutput as GqlUserOutput;

use crate::auth::credentials::{AuthToken, Credentials, FirebaseToken, LoginToken};
use crate::auth::user::User;
use crate::server::experiments::ServerExperiment;
use crate::server::ids::ApiKeyUid;
use crate::server::server_api::register_error;
use crate::settings::PrivacySettingsSnapshot;

use super::ServerApi;

/// Header key for the ambient workload token attached to multi-agent requests.
pub const AMBIENT_WORKLOAD_TOKEN_HEADER: &str = "X-Warp-Ambient-Workload-Token";

/// Header key for the cloud agent task ID attached to requests from ambient agents.
pub const CLOUD_AGENT_ID_HEADER: &str = "X-Warp-Cloud-Agent-ID";

/// User settings that are currently 'synced' (e.g. stored server-side) on a per-user basis.
#[derive(Copy, Clone, Debug, Default)]
pub struct SyncedUserSettings {
    pub is_cloud_conversation_storage_enabled: bool,
    pub is_crash_reporting_enabled: bool,
    pub is_telemetry_enabled: bool,
}

/// Results of an attempt to fetch the current user.
pub struct FetchUserResult {
    pub user: User,
    /// The credentials used to authenticate this user.
    pub credentials: Credentials,
    pub server_experiments: Vec<ServerExperiment>,
    /// Whether this attempt to fetch the user was for refreshing an existing logged-in user.
    pub from_refresh: bool,
    /// LLM model choices for this user.
    pub llms: crate::ai::llms::ModelsByFeature,
}

#[allow(dead_code)]
#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait AuthClient: 'static + Send + Sync {
    /// Creates an anonymous user, who is allowed to use Warp but may lack the ability
    /// to interact with particular features.
    async fn create_anonymous_user(
        &self,
        referral_code: Option<String>,
        anonymous_user_type: AnonymousUserType,
    ) -> Result<CreateAnonymousUserResult>;

    /// Returns the cached access token, if it is still valid. If it has expired, fetches a new
    /// access token using the user's refresh token, caches it, and the returns it.
    /// Returns an auth mode that may not require an Authorization header (e.g. session cookies or
    /// test credentials).
    async fn get_or_refresh_access_token(&self) -> Result<AuthToken>;

    /// Fetches data required to construct the [`User`] object. This includes the user's metadata
    /// and authentication tokens.
    async fn fetch_user(
        &self,
        token: LoginToken,
        for_refresh: bool,
    ) -> StdResult<FetchUserResult, UserAuthenticationError>;

    /// Creates and fetches an new custom token for the current user from Firebase.
    /// This only works for anonymous users, and will surface an error if the user is not anonymous.
    async fn fetch_new_custom_token(&self) -> Result<MintCustomTokenResult>;

    /// Handles the response from [`Self::fetch_new_custom_token`], returning the newly-minted custom token.
    fn on_custom_token_fetched(
        &self,
        response: Result<MintCustomTokenResult>,
    ) -> Result<String, MintCustomTokenError>;

    /// Queries warp-server for a set of the currently logged-in user's fields.
    async fn fetch_user_properties<'a>(&self, auth_token: Option<&'a str>)
        -> Result<GqlUserOutput>;

    /// Upon success, returns an `Option` containing the user's settings retrieved from the server,
    /// if any. The user may not have server-side settings if they onboarded prior to the launch
    /// of telemetry opt-out, have not logged in since the launch, and have never changed defaults
    /// for any of the settings in [`SyncedUserSettings`]. If the fetched settings object exists
    /// but is missing required fields, or if the request itself failed, returns an error.
    async fn get_user_settings(&self) -> Result<Option<SyncedUserSettings>>;

    /// Returns conversation usage history for the current user over the past n days.
    /// If last_updated_end_timestamp is provided, only conversations with
    /// lastUpdated earlier than this timestamp are returned.
    async fn get_conversation_usage_history(
        &self,
        days: Option<i32>,
        limit: Option<i32>,
        last_updated_end_timestamp: Option<galaxy_graphql::scalars::Time>,
    ) -> Result<Vec<ConversationUsage>>;

    async fn set_is_telemetry_enabled(&self, value: bool) -> Result<()>;

    async fn set_is_crash_reporting_enabled(&self, value: bool) -> Result<()>;

    async fn set_is_cloud_conversation_storage_enabled(&self, value: bool) -> Result<()>;

    /// Sends a request to update the user's settings on the server with values contained in the
    /// given `settings_snapshot`.
    async fn update_user_settings(&self, settings_snapshot: PrivacySettingsSnapshot) -> Result<()>;

    async fn set_user_is_onboarded(&self) -> Result<bool>;

    /// Requests a device authorization code from the server. This is only used for headless CLI/SDK authentication.
    async fn request_device_code(
        &self,
    ) -> StdResult<oauth2::StandardDeviceAuthorizationResponse, UserAuthenticationError>;

    /// Wait for the request to be approved or rejected and exchange it for a short-lived custom access token.
    async fn exchange_device_access_token(
        &self,
        details: &oauth2::StandardDeviceAuthorizationResponse,
        timeout: Duration,
    ) -> StdResult<FirebaseToken, UserAuthenticationError>;
    // API Keys
    async fn list_api_keys(&self) -> Result<Vec<ApiKeyProperties>>;

    async fn create_api_key(
        &self,
        name: String,
        team_id: Option<cynic::Id>,
        expires_at: Option<galaxy_graphql::scalars::Time>,
    ) -> Result<GenerateApiKeyResult>;

    async fn expire_api_key(&self, key_uid: &ApiKeyUid) -> Result<ExpireApiKeyResult>;

    /// Returns a cached ambient workload token, or issues a new one if not present or expired.
    ///
    /// Returns `Ok(None)` if not running in an isolation platform (e.g., Namespace) or on WASM.
    async fn get_or_create_ambient_workload_token(&self) -> Result<Option<String>>;
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl AuthClient for ServerApi {
    async fn create_anonymous_user(
        &self,
        _referral_code: Option<String>,
        _anonymous_user_type: AnonymousUserType,
    ) -> Result<CreateAnonymousUserResult> {
        bail!("Server auth disabled")
    }

    async fn get_or_refresh_access_token(&self) -> Result<AuthToken> {
        Ok(AuthToken::NoAuth)
    }

    async fn fetch_user(
        &self,
        _token: LoginToken,
        _for_refresh: bool,
    ) -> StdResult<FetchUserResult, UserAuthenticationError> {
        Err(UserAuthenticationError::Unexpected(anyhow!(
            "Server auth disabled"
        )))
    }

    async fn fetch_new_custom_token(&self) -> Result<MintCustomTokenResult> {
        bail!("Server auth disabled")
    }

    fn on_custom_token_fetched(
        &self,
        _response: Result<MintCustomTokenResult>,
    ) -> Result<String, MintCustomTokenError> {
        Err(MintCustomTokenError::Unknown)
    }

    async fn fetch_user_properties<'a>(
        &self,
        _auth_token: Option<&'a str>,
    ) -> Result<GqlUserOutput> {
        bail!("Server auth disabled")
    }

    async fn get_user_settings(&self) -> Result<Option<SyncedUserSettings>> {
        Ok(None)
    }

    async fn get_conversation_usage_history(
        &self,
        _days: Option<i32>,
        _limit: Option<i32>,
        _last_updated_end_timestamp: Option<galaxy_graphql::scalars::Time>,
    ) -> Result<Vec<ConversationUsage>> {
        Ok(vec![])
    }

    async fn set_is_telemetry_enabled(&self, _value: bool) -> Result<()> {
        Ok(())
    }

    async fn set_is_crash_reporting_enabled(&self, _value: bool) -> Result<()> {
        Ok(())
    }

    async fn set_is_cloud_conversation_storage_enabled(&self, _value: bool) -> Result<()> {
        Ok(())
    }

    async fn update_user_settings(
        &self,
        _settings_snapshot: PrivacySettingsSnapshot,
    ) -> Result<()> {
        Ok(())
    }

    async fn set_user_is_onboarded(&self) -> Result<bool> {
        Ok(true)
    }

    async fn request_device_code(
        &self,
    ) -> StdResult<oauth2::StandardDeviceAuthorizationResponse, UserAuthenticationError> {
        Err(UserAuthenticationError::Unexpected(anyhow!(
            "Server auth disabled"
        )))
    }

    async fn exchange_device_access_token(
        &self,
        _details: &oauth2::StandardDeviceAuthorizationResponse,
        _timeout: Duration,
    ) -> StdResult<FirebaseToken, UserAuthenticationError> {
        Err(UserAuthenticationError::Unexpected(anyhow!(
            "Server auth disabled"
        )))
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKeyProperties>> {
        Ok(vec![])
    }

    async fn create_api_key(
        &self,
        _name: String,
        _team_id: Option<cynic::Id>,
        _expires_at: Option<galaxy_graphql::scalars::Time>,
    ) -> Result<GenerateApiKeyResult> {
        bail!("Server auth disabled")
    }

    async fn expire_api_key(&self, _key_uid: &ApiKeyUid) -> Result<ExpireApiKeyResult> {
        bail!("Server auth disabled")
    }

    async fn get_or_create_ambient_workload_token(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

/// The [`oauth2::Client`] type, specialized to the endpoints that we require.
pub type OAuth2Client = oauth2::basic::BasicClient<
    oauth2::EndpointNotSet, // HasAuthUrl
    oauth2::EndpointSet,    // HasDeviceAuthUrl
    oauth2::EndpointNotSet, // HasIntrospectionUrl
    oauth2::EndpointNotSet, // HasRevocationUrl
    oauth2::EndpointSet,    // HasTokenUrl
>;

#[derive(Error, Debug)]
#[allow(dead_code)]
/// Error type when retrieving a user and validating it against Firebase.
pub enum UserAuthenticationError {
    /// The user's refresh token is invalid. This could occur if the user authed through
    /// e.g. Google/GitHub and changed their password.
    #[error("Firebase returned a token error when fetching an ID token")]
    DeniedAccessToken(FirebaseError),
    /// The user's account is invalid. This could occur if the user requested their account
    /// be deleted per their GDPR/CCPA rights.
    #[error("Firebase returned a user error when fetching an ID token")]
    UserAccountDisabled(FirebaseError),
    #[error("Invalid state parameter in auth redirect")]
    InvalidStateParameter,
    #[error("Missing state parameter in auth redirect")]
    MissingStateParameter,
    #[error("unexpected error occurred when fetching an ID token: {0:#}")]
    Unexpected(#[from] anyhow::Error),
}

impl ErrorExt for UserAuthenticationError {
    fn is_actionable(&self) -> bool {
        match self {
            UserAuthenticationError::DeniedAccessToken(err) => {
                // If a request to our server failed because the user's refresh token
                // has expired, they should re-auth, but there's no value in reporting
                // this back to us.
                log::info!("ignoring denied access token error: {err:#}");
                false
            }
            UserAuthenticationError::UserAccountDisabled(err) => {
                // Similarly, if their account is disabled, they can't make requests.
                log::info!("ignoring user account disabled error: {err:#}");
                false
            }
            UserAuthenticationError::Unexpected(err) => err.is_actionable(),
            UserAuthenticationError::InvalidStateParameter
            | UserAuthenticationError::MissingStateParameter => {
                // For now, we're marking these as actionable, since a surplus of these errors
                // could mean that something is wrong in our login flow (e.g. we're not properly
                // passing the `state` variable back to the desktop client).
                // But in general, someone attempting to trick another into logging into their
                // account with a spoofed `state` variable is not actionable.
                true
            }
        }
    }
}
register_error!(UserAuthenticationError);

impl From<FirebaseError> for UserAuthenticationError {
    fn from(error: FirebaseError) -> Self {
        UserAuthenticationError::Unexpected(
            anyhow::Error::from(error).context("Firebase error (server auth disabled)"),
        )
    }
}

#[derive(Error, Debug)]
#[allow(dead_code)]
/// Error type when creating anonymous users
pub enum AnonymousUserCreationError {
    #[error("The network request to create the anonymous user failed")]
    CreationFailed,

    #[error("Received a user facing error: {0}")]
    UserFacingError(String),

    /// Failure that occurs after the user is created, but the ID token could not be fetched.
    #[error("The user was created, but the ID token could not be fetched")]
    UserAuthenticationFailed(#[from] UserAuthenticationError),

    #[error("Failed to create anonymous user with unknown error")]
    Unknown,
}

#[derive(Error, Debug)]
#[allow(dead_code)]
/// Error type when minting a new custom token for an anonymous user
pub enum MintCustomTokenError {
    #[error("Received a user facing error: {0}")]
    UserFacingError(String),
    #[error("Failed to create new custom token with unknown error")]
    Unknown,
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod tests;
