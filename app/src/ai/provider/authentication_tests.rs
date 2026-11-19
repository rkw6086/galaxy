use std::time::{Duration, SystemTime};

use ai::api_keys::{AwsCredentials, AwsCredentialsRefreshStrategy, AwsCredentialsState};

use super::*;

fn config(auth_method: BedrockAuthMethod) -> BedrockClientConfig {
    BedrockClientConfig {
        auth_method,
        profile: "default".to_owned(),
        region: String::new(),
        access_key_id: "configured-key".to_owned(),
        secret_access_key: "configured-secret".to_owned(),
        session_token: None,
        credentials_expiry: None,
        invocation_model_id: None,
        cross_region_inference: true,
    }
}

#[test]
fn provider_profile_and_region_are_authoritative_including_defaults() {
    galaxyui::App::test((), |mut app| async move {
        crate::test_util::settings::initialize_settings_for_tests(&mut app);
        for (profile, region) in [("default", ""), ("selected-profile", "eu-west-1")] {
            AISettings::handle(&app).update(&mut app, |settings, ctx| {
                settings
                    .bedrock_profile
                    .set_value(profile.to_owned(), ctx)
                    .unwrap();
                settings
                    .bedrock_region
                    .set_value(region.to_owned(), ctx)
                    .unwrap();
            });
            app.read(|ctx| {
                let resolved = BedrockClientConfig::from_settings(AISettings::as_ref(ctx));
                assert_eq!(resolved.profile, profile);
                assert_eq!(resolved.region, region);
            });
        }
    });
}

#[test]
fn custom_login_commands_are_not_rewritten() {
    for command in [
        "",
        "company-login --interactive",
        "aws sso login --profile custom",
    ] {
        assert_eq!(login_command(command, "selected"), command);
    }
}

#[test]
fn default_login_targets_and_quotes_the_effective_profile() {
    assert_eq!(
        login_command("aws sso login", "default"),
        "aws sso login --profile default"
    );
    let command = login_command("aws sso login", "team profile");
    assert_eq!(
        login_command_parts(&command).unwrap(),
        ["aws", "sso", "login", "--profile", "team profile"]
    );
    assert!(login_command_parts("").is_err());
    assert!(login_command_parts("aws 'unterminated").is_err());
}

#[test]
fn local_credential_snapshots_never_override_configured_authentication() {
    let now = SystemTime::now();
    for auth in [
        BedrockAuthMethod::Profile,
        BedrockAuthMethod::Sso,
        BedrockAuthMethod::StaticKeys,
    ] {
        for expiry in [
            None,
            Some(now - Duration::from_secs(1)),
            Some(now + Duration::from_secs(60)),
        ] {
            let configured = config(auth);
            let loaded = AwsCredentialsState::Loaded {
                credentials: AwsCredentials::new(
                    "snapshot-key".to_owned(),
                    "snapshot-secret".to_owned(),
                    Some("snapshot-token".to_owned()),
                    expiry,
                ),
                loaded_at: now,
            };
            assert_eq!(
                configured
                    .clone()
                    .with_managed_credentials(&AwsCredentialsRefreshStrategy::LocalChain, &loaded,),
                configured
            );
        }
    }
}

#[test]
fn managed_oidc_credentials_preserve_expiry_and_do_not_fall_back_to_local_keys() {
    let now = SystemTime::now();
    let strategy = AwsCredentialsRefreshStrategy::OidcManaged {
        task_id: Some("task".to_owned()),
        role_arn: "test-role".to_owned(),
        region: "us-east-1".to_owned(),
    };
    let loaded = AwsCredentialsState::Loaded {
        credentials: AwsCredentials::new(
            "managed-key".to_owned(),
            "managed-secret".to_owned(),
            Some("managed-token".to_owned()),
            Some(now + Duration::from_secs(60)),
        ),
        loaded_at: now,
    };
    let resolved = config(BedrockAuthMethod::Profile).with_managed_credentials(&strategy, &loaded);
    assert_eq!(resolved.auth_method, BedrockAuthMethod::StaticKeys);
    assert_eq!(resolved.region, "us-east-1");
    assert_eq!(resolved.access_key_id, "managed-key");
    assert_eq!(
        resolved.credentials_expiry,
        Some(now + Duration::from_secs(60))
    );
    for state in [
        AwsCredentialsState::Missing,
        AwsCredentialsState::Disabled,
        AwsCredentialsState::Refreshing,
        AwsCredentialsState::Failed {
            message: "failed".to_owned(),
        },
    ] {
        let resolved =
            config(BedrockAuthMethod::Profile).with_managed_credentials(&strategy, &state);
        assert!(resolved.access_key_id.is_empty());
        assert!(resolved.secret_access_key.is_empty());
    }
}

#[test]
fn automatic_recovery_is_bounded_per_user_exchange_but_manual_retry_is_available() {
    let mut attempts = RecoveryAttempts::default();
    assert!(attempts.begin("conversation", "query-1", "connection", false));
    assert!(!attempts.begin("conversation", "query-1", "connection", false));
    assert!(attempts.begin("conversation", "query-1", "connection", true));
    assert!(attempts.begin("conversation", "query-2", "connection", false));
    assert!(attempts.begin("conversation", "query-2", "new-connection", false));
    assert!(attempts.begin("another-conversation", "query-2", "connection", false));
}

#[test]
fn credential_expiry_is_checked_at_the_boundary() {
    let now = SystemTime::now();
    assert!(credentials_are_current(None, now));
    assert!(credentials_are_current(
        Some(now + Duration::from_secs(1)),
        now
    ));
    assert!(!credentials_are_current(Some(now), now));
    assert!(!credentials_are_current(
        Some(now - Duration::from_secs(1)),
        now
    ));
}

#[test]
fn model_routes_use_discovered_ids_and_match_legacy_cross_region_selections() {
    let model = crate::settings::BedrockModelConfig {
        model_id: "future.new-model".to_owned(),
        invocation_model_id:
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.future.new-model"
                .to_owned(),
        ..Default::default()
    };
    for selected in ["future.new-model", "us.future.new-model"] {
        let resolved =
            config(BedrockAuthMethod::Profile).with_model_route(selected, &[model.clone()]);
        assert_eq!(
            resolved.invocation_model_id.as_deref(),
            Some(model.invocation_model_id.as_str())
        );
    }
    let resolved = config(BedrockAuthMethod::Profile).with_model_route("unrelated.model", &[model]);
    assert_eq!(resolved.invocation_model_id, None);
}

fn test_credentials() -> AwsCredentials {
    AwsCredentials::new("test-key".to_owned(), "test-secret".to_owned(), None, None)
}

#[tokio::test]
async fn external_refresh_recovers_without_running_login() {
    let credentials = recover_credentials(
        BedrockAuthMethod::Sso,
        false,
        || futures::future::ready(Ok(test_credentials())).boxed(),
        || futures::future::ready(Err("Login must not run".to_owned())).boxed(),
    )
    .await
    .unwrap();
    assert_eq!(credentials, test_credentials());
}

#[tokio::test]
async fn expired_session_runs_login_then_awaits_a_fresh_reload() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let load_calls = calls.clone();
    let login_calls = calls.clone();
    let result = recover_credentials(
        BedrockAuthMethod::Sso,
        false,
        move || {
            let index = load_calls.fetch_add(1, Ordering::SeqCst);
            let result = match index {
                0 => Err("expired".to_owned()),
                2 => Ok(test_credentials()),
                index => panic!("Unexpected reload order: {index}"),
            };
            futures::future::ready(result).boxed()
        },
        move || {
            assert_eq!(login_calls.fetch_add(1, Ordering::SeqCst), 1);
            futures::future::ready(Ok(())).boxed()
        },
    )
    .await;
    assert_eq!(result, Ok(test_credentials()));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn manual_login_failure_does_not_reload_or_resume() {
    let result = recover_credentials(
        BedrockAuthMethod::Sso,
        true,
        || panic!("Credentials must not reload after failed login"),
        || futures::future::ready(Err("login failed".to_owned())).boxed(),
    )
    .await;
    assert_eq!(result, Err("login failed".to_owned()));
}

#[tokio::test]
async fn login_success_with_failed_reload_remains_an_error() {
    let result = recover_credentials(
        BedrockAuthMethod::Sso,
        true,
        || futures::future::ready(Err("reload failed".to_owned())).boxed(),
        || futures::future::ready(Ok(())).boxed(),
    )
    .await;
    assert_eq!(result, Err("reload failed".to_owned()));
}

#[tokio::test]
async fn shared_recovery_executes_login_once_for_overlapping_waiters() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let login_calls = calls.clone();
    let recovery = recover_credentials(
        BedrockAuthMethod::Sso,
        true,
        || futures::future::ready(Ok(test_credentials())).boxed(),
        move || {
            login_calls.fetch_add(1, Ordering::SeqCst);
            futures::future::ready(Ok(())).boxed()
        },
    )
    .boxed()
    .shared();
    let (first, second) = futures::join!(recovery.clone(), recovery);
    assert_eq!(first, Ok(test_credentials()));
    assert_eq!(second, Ok(test_credentials()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn credential_preparation_failures_surface_authentication_recovery() {
    use super::super::client::BedrockError;

    for error in [
        BedrockError::CredentialsNotConfigured,
        BedrockError::CredentialsLoadFailed("SSO session invalid".to_owned()),
    ] {
        let error = provider_startup_error(&anyhow::Error::new(error), "selected-model");
        assert!(error.is_aws_bedrock_credentials_error());
    }
    for error in [
        BedrockError::RegionNotConfigured,
        BedrockError::ApiError("transport".to_owned()),
        BedrockError::ModelNotFound("model".to_owned()),
        BedrockError::AccessDenied("policy".to_owned()),
        BedrockError::Throttling("busy".to_owned()),
        BedrockError::ValidationError("request".to_owned()),
    ] {
        let error = provider_startup_error(&anyhow::Error::new(error), "selected-model");
        assert!(!error.is_aws_bedrock_credentials_error());
    }
    assert!(
        !provider_startup_error(&anyhow::anyhow!("unrelated"), "selected-model")
            .is_aws_bedrock_credentials_error()
    );
}

#[tokio::test]
async fn profile_authentication_never_starts_a_login_flow() {
    for manual in [false, true] {
        let result = recover_credentials(
            BedrockAuthMethod::Profile,
            manual,
            || futures::future::ready(Ok(test_credentials())).boxed(),
            || panic!("Profile authentication must not execute login commands"),
        )
        .await;
        assert_eq!(result, Ok(test_credentials()));
        let result = recover_credentials(
            BedrockAuthMethod::Profile,
            manual,
            || futures::future::ready(Err("profile needs fresh credentials".to_owned())).boxed(),
            || panic!("A failed profile reload must not execute login commands"),
        )
        .await;
        assert_eq!(result, Err("profile needs fresh credentials".to_owned()));
    }
}
