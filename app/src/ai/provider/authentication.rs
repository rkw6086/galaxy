use std::collections::HashMap;
use std::time::SystemTime;

use ai::api_keys::{ApiKeyManager, AwsCredentials, AwsCredentialsRefreshStrategy, AwsCredentialsState};
use futures::channel::oneshot;
use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use galaxyui::{AppContext, Entity, ModelContext, SingletonEntity};
use settings::Setting;

use super::client::{BedrockClientConfig, BedrockError};
use crate::ai::agent::RenderableAIError;
use crate::settings::{AISettings, AISettingsChangedEvent, BedrockAuthMethod};

impl BedrockClientConfig {
    pub(crate) fn from_settings(settings: &AISettings) -> Self {
        Self {
            auth_method: *settings.bedrock_auth_method.value(),
            profile: settings.bedrock_profile.value().clone(),
            region: settings.bedrock_region.value().clone(),
            access_key_id: settings.bedrock_access_key_id.value().clone(),
            secret_access_key: settings.bedrock_secret_access_key.value().clone(),
            session_token: None,
            credentials_expiry: None,
            cross_region_inference: *settings.bedrock_cross_region_inference.value(),
        }
    }

    pub(crate) fn with_managed_credentials(
        mut self,
        strategy: &AwsCredentialsRefreshStrategy,
        state: &AwsCredentialsState,
    ) -> Self {
        match strategy {
            AwsCredentialsRefreshStrategy::LocalChain => self,
            AwsCredentialsRefreshStrategy::OidcManaged { region, .. } => {
                self.auth_method = BedrockAuthMethod::StaticKeys;
                self.region.clone_from(region);
                self.access_key_id.clear();
                self.secret_access_key.clear();
                self.session_token = None;
                self.credentials_expiry = None;
                if let AwsCredentialsState::Loaded { credentials, .. } = state {
                    self.access_key_id = credentials.access_key().to_owned();
                    self.secret_access_key = credentials.secret_key().to_owned();
                    self.session_token = credentials.session_token().map(str::to_owned);
                    self.credentials_expiry = Some(
                        credentials.expires_at().unwrap_or(SystemTime::UNIX_EPOCH),
                    );
                }
                self
            }
        }
    }
}

pub(crate) fn credentials_are_current(expiry: Option<SystemTime>, now: SystemTime) -> bool {
    expiry.is_none_or(|expiry| expiry > now)
}

pub(crate) fn provider_startup_error(error: &anyhow::Error, model: &str) -> RenderableAIError {
    match error.downcast_ref::<BedrockError>() {
        Some(BedrockError::CredentialsNotConfigured | BedrockError::CredentialsLoadFailed(_)) => {
            RenderableAIError::AwsBedrockCredentialsExpiredOrInvalid {
                model_name: model.to_owned(),
            }
        }
        Some(
            BedrockError::RegionNotConfigured
            | BedrockError::ApiError(_)
            | BedrockError::ModelNotFound(_)
            | BedrockError::AccessDenied(_)
            | BedrockError::Throttling(_)
            | BedrockError::ValidationError(_),
        ) | None => RenderableAIError::other(error.to_string(), false),
    }
}

pub(crate) fn login_command(
    configured: &str,
    profile: &str,
) -> String {
    if configured != "aws sso login" {
        return configured.to_owned();
    }
    if profile.is_empty() {
        configured.to_owned()
    } else {
        format!("aws sso login --profile {}", shell_words::quote(profile))
    }
}

pub(crate) fn login_command_parts(command: &str) -> Result<Vec<String>, String> {
    let parts = shell_words::split(command)
        .map_err(|_| "AWS login command contains invalid quoting".to_owned())?;
    if parts.is_empty() {
        return Err("AWS login command is not configured".to_owned());
    }
    Ok(parts)
}

#[derive(Clone, PartialEq)]
pub(crate) struct BedrockAuthConnection {
    pub(crate) config: BedrockClientConfig,
    pub(crate) enabled: bool,
    pub(crate) auto_login: bool,
    pub(crate) command: String,
}

impl BedrockAuthConnection {
    pub(crate) fn current(ctx: &AppContext) -> Self {
        let settings = AISettings::as_ref(ctx);
        let config = BedrockClientConfig::from_settings(settings);
        Self {
            command: match config.auth_method {
                BedrockAuthMethod::Sso => login_command(
                    settings.bedrock_auth_refresh_command.value(),
                    &config.profile,
                ),
                BedrockAuthMethod::Profile | BedrockAuthMethod::StaticKeys => String::new(),
            },
            config,
            enabled: *settings.bedrock_enabled.value(),
            auto_login: *settings.bedrock_auto_login.value(),
        }
    }

    fn identity(&self) -> String {
        format!(
            "{:?}:{}:{}:{}",
            self.config.auth_method, self.config.profile, self.config.region, self.command
        )
    }
}

#[derive(Default)]
struct RecoveryAttempts {
    exchanges: HashMap<String, (String, String)>,
}

impl RecoveryAttempts {
    fn begin(&mut self, conversation: &str, exchange: &str, connection: &str, manual: bool) -> bool {
        let attempt = (exchange.to_owned(), connection.to_owned());
        if !manual && self.exchanges.get(conversation) == Some(&attempt) {
            return false;
        }
        self.exchanges.insert(conversation.to_owned(), attempt);
        true
    }
}

type CredentialResult = Result<AwsCredentials, String>;
type CredentialFuture = Shared<BoxFuture<'static, CredentialResult>>;

#[derive(Default)]
pub(crate) struct BedrockAuthentication {
    attempts: RecoveryAttempts,
    active: Vec<(u64, BedrockAuthConnection, CredentialFuture)>,
    generation: u64,
    revision: u64,
}

impl BedrockAuthentication {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, _| {
            if matches!(
                event,
                AISettingsChangedEvent::BedrockEnabled { .. }
                    | AISettingsChangedEvent::BedrockAuthMethod { .. }
                    | AISettingsChangedEvent::BedrockProfile { .. }
                    | AISettingsChangedEvent::BedrockRegion { .. }
                    | AISettingsChangedEvent::BedrockAuthRefreshCommand { .. }
                    | AISettingsChangedEvent::BedrockAccessKeyId { .. }
                    | AISettingsChangedEvent::BedrockSecretAccessKey { .. }
            ) {
                me.revision += 1;
                me.active.clear();
            }
        });
        Self::default()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn recover(
        &mut self,
        conversation: &str,
        exchange: &str,
        manual: bool,
        ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, Result<(), String>> {
        let connection = BedrockAuthConnection::current(ctx);
        if !connection.enabled || (!manual && !connection.auto_login) {
            return Box::pin(async { Err("Automatic AWS login is disabled".to_owned()) });
        }
        if !self.attempts.begin(conversation, exchange, &connection.identity(), manual) {
            return Box::pin(async {
                Err("AWS authentication still failed after recovery. Use Refresh AWS Credentials to retry.".to_owned())
            });
        }
        let strategy = ApiKeyManager::as_ref(ctx).aws_credentials_refresh_strategy();
        if matches!(strategy, AwsCredentialsRefreshStrategy::OidcManaged { .. }) {
            return ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
                crate::ai::aws_credentials::refresh_aws_credentials(manager, ctx)
            });
        }
        if connection.config.auth_method == BedrockAuthMethod::StaticKeys {
            return Box::pin(async {
                Err("Update the configured AWS static keys to authenticate.".to_owned())
            });
        }

        let (generation, future) = match self.active.iter().find(|(_, active, _)| active == &connection) {
            Some((generation, _, future)) => (*generation, future.clone()),
            None => {
                self.generation += 1;
                let config = connection.config.clone();
                let command = connection.command.clone();
                let future = recover_credentials(
                    connection.config.auth_method,
                    manual,
                    move || {
                        let config = config.clone();
                        async move {
                            crate::ai::aws_credentials::load_aws_credentials_from_sdk(&config)
                                .await
                                .map_err(|error| error.to_string())
                        }.boxed()
                    },
                    move || {
                        let command = command.clone();
                        async move {
                            let parts = login_command_parts(&command)?;
                            let status = tokio::process::Command::new(&parts[0])
                                .args(&parts[1..])
                                .status()
                                .await
                                .map_err(|error| format!("Failed to start AWS login: {error}"))?;
                            if !status.success() {
                                return Err(format!("AWS login command exited with status: {status}"));
                            }
                            Ok(())
                        }.boxed()
                    },
                ).boxed().shared();
                self.active.push((self.generation, connection.clone(), future.clone()));
                (self.generation, future)
            }
        };
        let revision = self.revision;
        let (tx, rx) = oneshot::channel();
        let _ = ctx.spawn(future, move |me, result, ctx| {
            me.active.retain(|(active, _, _)| *active != generation);
            if me.revision != revision
                || BedrockAuthConnection::current(ctx) != connection
                || ApiKeyManager::as_ref(ctx).aws_credentials_refresh_strategy() != strategy
            {
                let _ = tx.send(Err("AWS connection changed during authentication".to_owned()));
                return;
            }
            let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
            ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
                let state = match result {
                    Ok(credentials) => AwsCredentialsState::Loaded {
                        credentials,
                        loaded_at: SystemTime::now(),
                    },
                    Err(message) => AwsCredentialsState::Failed { message },
                };
                manager.set_aws_credentials_state(state, ctx);
            });
            let _ = tx.send(outcome);
        });
        async move {
            rx.await.unwrap_or_else(|_| Err("AWS authentication was interrupted".to_owned()))
        }.boxed()
    }
}

async fn recover_credentials(
    auth_method: BedrockAuthMethod,
    login_first: bool,
    load: impl Fn() -> BoxFuture<'static, CredentialResult>,
    login: impl Fn() -> BoxFuture<'static, Result<(), String>>,
) -> CredentialResult {
    match auth_method {
        BedrockAuthMethod::Profile => return load().await,
        BedrockAuthMethod::StaticKeys => {
            return Err("Update the configured AWS static keys to authenticate.".to_owned());
        }
        BedrockAuthMethod::Sso => {}
    }
    if !login_first {
        if let Ok(credentials) = load().await {
            return Ok(credentials);
        }
    }
    login().await?;
    load().await
}

impl Entity for BedrockAuthentication {
    type Event = ();
}

impl SingletonEntity for BedrockAuthentication {}

pub(crate) fn connection_revision(ctx: &AppContext) -> u64 {
    ctx.try_get_singleton_model_as_ref::<BedrockAuthentication>()
        .map_or(0, |authentication| authentication.revision())
}

#[cfg(test)]
#[path = "authentication_tests.rs"]
mod tests;
