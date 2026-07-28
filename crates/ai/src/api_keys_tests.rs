use std::time::{Duration, SystemTime};

use super::*;

fn make_manager() -> ApiKeyManager {
    ApiKeyManager {
        aws_credentials_state: AwsCredentialsState::Missing,
        aws_credentials_refresh_strategy: AwsCredentialsRefreshStrategy::default(),
        geap_credentials_state: GeapCredentialsState::Missing,
    }
}

fn make_manager_with_geap(geap_credentials_state: GeapCredentialsState) -> ApiKeyManager {
    let mut manager = make_manager();
    manager.geap_credentials_state = geap_credentials_state;
    manager
}

fn geap_credentials(access_token: &str, expires_in: Option<u64>) -> GeapCredentials {
    GeapCredentials::new(
        access_token.into(),
        expires_in.map(|secs| SystemTime::now() + Duration::from_secs(secs)),
    )
}

fn geap_binding() -> GeapMintBinding {
    GeapMintBinding {
        user_uid: "user-1".into(),
        audience:
            "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/p/providers/q"
                .into(),
        federation: GeapFederation::ServiceAccount {
            email: "sa@proj.iam.gserviceaccount.com".into(),
        },
    }
}

fn geap_gate() -> GeapMintBinding {
    geap_binding()
}

fn geap_loaded(access_token: &str, expires_in: Option<u64>) -> GeapCredentialsState {
    GeapCredentialsState::Loaded {
        credentials: geap_credentials(access_token, expires_in),
        loaded_at: SystemTime::now(),
        minted_for: geap_binding(),
    }
}

// ── geap credentials ────────────────────────────────────────────

#[test]
fn geap_access_token_present_without_expiry() {
    let credentials = GeapCredentials::new("tok".into(), None);
    assert_eq!(credentials.access_token_for_request(), Some("tok"));
}

#[test]
fn geap_access_token_blank_is_none() {
    let credentials = GeapCredentials::new("   ".into(), None);
    assert_eq!(credentials.access_token_for_request(), None);
}

#[test]
fn geap_access_token_near_expiry_still_sent() {
    // Expired tokens are still sent; Google is the authority on validity.
    let credentials = geap_credentials("tok", Some(0));
    assert_eq!(credentials.access_token_for_request(), Some("tok"));
}

#[test]
fn geap_needs_refresh_lead_time_boundaries() {
    // Within the 5-minute lead window.
    assert!(geap_credentials("tok", Some(30)).needs_refresh());
    // Comfortably fresh.
    assert!(!geap_credentials("tok", Some(3600)).needs_refresh());
    // Already expired -> still needs a refresh.
    assert!(geap_credentials("tok", Some(0)).needs_refresh());
    // Unknown expiry never reports as needing a refresh.
    assert!(!geap_credentials("tok", None).needs_refresh());
}

#[test]
fn api_keys_for_request_includes_geap_token_when_gate_and_binding_match() {
    let mgr = make_manager_with_geap(geap_loaded("geap-abc", Some(3600)));
    let result = mgr.api_keys_for_request(false, Some(geap_gate())).unwrap();
    let credentials = result.google_cloud_credentials.unwrap();
    assert_eq!(credentials.access_token, "geap-abc");
    assert!(result.aws_credentials.is_none());
}

#[test]
fn api_keys_for_request_includes_expired_geap_token() {
    // Expired tokens are still attached — never silently dropped. Google
    // rejects truly invalid ones, which surfaces a recoverable error instead
    // of a silent fallback to another route.
    let mgr = make_manager_with_geap(geap_loaded("geap-abc", Some(0)));
    let result = mgr.api_keys_for_request(false, Some(geap_gate())).unwrap();
    assert_eq!(
        result.google_cloud_credentials.unwrap().access_token,
        "geap-abc"
    );
}

#[test]
fn api_keys_for_request_omits_geap_token_without_gate() {
    // No gate (policy off at the call site) ⇒ no GEAP credentials, even when
    // a token is loaded.
    let mgr = make_manager_with_geap(geap_loaded("geap-abc", Some(3600)));
    assert!(mgr.api_keys_for_request(false, None).is_none());
}

#[test]
fn api_keys_for_request_omits_geap_token_on_binding_mismatch() {
    let mgr = make_manager_with_geap(geap_loaded("geap-abc", Some(3600)));

    // A different user (sign-out/account switch).
    let mut gate = geap_gate();
    gate.user_uid = "someone-else".into();
    assert!(mgr.api_keys_for_request(false, Some(gate)).is_none());

    // A different audience (admin changed the pool/provider).
    let mut gate = geap_gate();
    gate.audience = "//iam.googleapis.com/projects/2/locations/global/workloadIdentityPools/other/providers/other".into();
    assert!(mgr.api_keys_for_request(false, Some(gate)).is_none());

    // A different service account (admin changed impersonation target).
    let mut gate = geap_gate();
    gate.federation = GeapFederation::ServiceAccount {
        email: "other@proj.iam.gserviceaccount.com".into(),
    };
    assert!(mgr.api_keys_for_request(false, Some(gate)).is_none());
}

#[test]
fn api_keys_for_request_serves_previous_geap_token_while_refreshing() {
    // A re-mint in flight keeps serving the previous token — tokens stay
    // until replaced.
    let mgr = make_manager_with_geap(GeapCredentialsState::Refreshing {
        previous: Some((geap_credentials("geap-old", Some(10)), geap_binding())),
    });
    let result = mgr.api_keys_for_request(false, Some(geap_gate())).unwrap();
    assert_eq!(
        result.google_cloud_credentials.unwrap().access_token,
        "geap-old"
    );
}

#[test]
fn api_keys_for_request_omits_geap_token_during_first_mint() {
    // The very first mint has nothing to serve yet.
    let mgr = make_manager_with_geap(GeapCredentialsState::Refreshing { previous: None });
    assert!(mgr.api_keys_for_request(false, Some(geap_gate())).is_none());
}

#[test]
fn api_keys_for_request_omits_geap_token_for_non_loaded_states() {
    for state in [
        GeapCredentialsState::Missing,
        GeapCredentialsState::Disabled,
        GeapCredentialsState::Failed {
            error: LoadGeapCredentialsError::ExchangeToken {
                status: None,
                detail: "boom".into(),
            },
        },
    ] {
        let mgr = make_manager_with_geap(state);
        assert!(mgr.api_keys_for_request(false, Some(geap_gate())).is_none());
    }
}

#[test]
fn api_keys_for_request_omits_geap_token_when_previous_binding_mismatches() {
    let mgr = make_manager_with_geap(GeapCredentialsState::Refreshing {
        previous: Some((geap_credentials("geap-old", Some(10)), geap_binding())),
    });
    let mut gate = geap_gate();
    gate.user_uid = "someone-else".into();
    assert!(mgr.api_keys_for_request(false, Some(gate)).is_none());
}

// ── aws credentials ─────────────────────────────────────────────

#[test]
fn api_keys_for_request_none_when_nothing_configured() {
    let mgr = make_manager();
    assert!(mgr.api_keys_for_request(false, None).is_none());
}

#[test]
fn api_keys_for_request_includes_aws_credentials_when_requested() {
    let mut mgr = make_manager();
    mgr.aws_credentials_state = AwsCredentialsState::Loaded {
        credentials: AwsCredentials::new("ak".into(), "sk".into(), None, None),
        loaded_at: SystemTime::now(),
    };
    let result = mgr.api_keys_for_request(true, None).unwrap();
    assert_eq!(result.aws_credentials.unwrap().access_key, "ak");
}

#[test]
fn api_keys_for_request_omits_aws_credentials_when_not_requested() {
    let mut mgr = make_manager();
    mgr.aws_credentials_state = AwsCredentialsState::Loaded {
        credentials: AwsCredentials::new("ak".into(), "sk".into(), None, None),
        loaded_at: SystemTime::now(),
    };
    assert!(mgr.api_keys_for_request(false, None).is_none());
}

#[test]
fn api_keys_for_request_includes_aws_credentials_when_oidc_managed_regardless_of_flag() {
    let mut mgr = make_manager();
    mgr.aws_credentials_state = AwsCredentialsState::Loaded {
        credentials: AwsCredentials::new("ak".into(), "sk".into(), None, None),
        loaded_at: SystemTime::now(),
    };
    mgr.aws_credentials_refresh_strategy = AwsCredentialsRefreshStrategy::OidcManaged {
        task_id: Some("task-1".into()),
        role_arn: "arn:aws:iam::123:role/test".into(),
        region: "us-east-1".into(),
    };
    let result = mgr.api_keys_for_request(false, None).unwrap();
    assert_eq!(result.aws_credentials.unwrap().access_key, "ak");
}
