use std::cell::Cell;

use futures::FutureExt;

use super::*;

fn record(access: &str, refresh: &str) -> AuthRecord {
    AuthRecord {
        access_token: Some(access.to_owned()),
        refresh_token: Some(refresh.to_owned()),
        id_token: None,
        expires_at: Some(10_000),
        account_id: Some("test-account".to_owned()),
    }
}

#[test]
fn concurrent_requests_share_one_refresh_and_wait_for_persistence() {
    let mut coordinator = RefreshCoordinator::default();
    let (first_tx, mut first_rx) = oneshot::channel();
    let (second_tx, mut second_rx) = oneshot::channel();
    let generation = coordinator.begin(first_tx).unwrap();
    assert_eq!(coordinator.begin(second_tx), None);
    let saved = Cell::new(false);
    let result = coordinator.complete(generation, Ok(record("rotated-access", "rotated-refresh")), |record| {
        assert_eq!(first_rx.try_recv().unwrap(), None);
        assert_eq!(second_rx.try_recv().unwrap(), None);
        assert_eq!(record.refresh_token.as_deref(), Some("rotated-refresh"));
        saved.set(true);
        Ok(())
    });
    assert!(result.unwrap().is_ok());
    assert!(saved.get());
    let first = first_rx.now_or_never().unwrap().unwrap().unwrap();
    let second = second_rx.now_or_never().unwrap().unwrap().unwrap();
    assert_eq!(first, second);
    assert_eq!(first.access_token, "rotated-access");
}

#[test]
fn failed_persistence_never_publishes_rotated_credentials() {
    let mut coordinator = RefreshCoordinator::default();
    let (tx, rx) = oneshot::channel();
    let generation = coordinator.begin(tx).unwrap();
    let result = coordinator.complete(generation, Ok(record("rotated", "new-refresh")), |_| {
        Err("storage unavailable".to_owned())
    }).unwrap().unwrap_err();
    assert!(result.reauthenticate);
    assert!(rx.now_or_never().unwrap().unwrap().is_err());
}

#[test]
fn replaced_sign_in_rejects_late_refresh_without_overwriting_new_session() {
    let mut coordinator = RefreshCoordinator::default();
    let (old_tx, old_rx) = oneshot::channel();
    let old = coordinator.begin(old_tx).unwrap();
    coordinator.invalidate();
    assert!(old_rx.now_or_never().unwrap().unwrap().is_err());
    let (new_tx, new_rx) = oneshot::channel();
    let current = coordinator.begin(new_tx).unwrap();
    assert_ne!(old, current);
    assert!(coordinator.complete(old, Ok(record("stale", "stale-refresh")), |_| {
        panic!("A stale refresh must not write credentials")
    }).is_none());
    assert!(coordinator.complete(current, Ok(record("current", "current-refresh")), |_| Ok(()))
        .unwrap().is_ok());
    assert_eq!(new_rx.now_or_never().unwrap().unwrap().unwrap().access_token, "current");
}

#[test]
fn reconnect_starts_a_new_generation_even_with_a_cached_refresh_token() {
    let mut model = ChatGPTAuthModel::new_for_test();
    model.record = Some(record("old-access", "already-used-refresh"));
    model.state = ChatGPTAuthState::Connected;
    let first = model.begin_sign_in().unwrap();
    model.state = ChatGPTAuthState::AwaitingBrowser;
    assert_eq!(model.begin_sign_in(), None);
    model.state = ChatGPTAuthState::Failed("refresh_token_reused".to_owned());
    let replacement = model.begin_sign_in().unwrap();
    assert!(replacement > first);
}

#[test]
fn rotated_tokens_replace_previous_tokens_and_omitted_refresh_tokens_are_preserved() {
    let previous = record("old-access", "old-refresh");
    for refresh in [None, Some("new-refresh".to_owned())] {
        let result = record_from_tokens(TokenResponse {
            access_token: "new-access".to_owned(),
            refresh_token: refresh.clone(),
            id_token: None,
            expires_in: Some(3600),
        }, Some(&previous), 100).unwrap();
        assert_eq!(result.access_token.as_deref(), Some("new-access"));
        assert_eq!(result.refresh_token, refresh.or_else(|| Some("old-refresh".to_owned())));
        assert_eq!(result.expires_at, Some(3700));
        assert_eq!(result.account_id.as_deref(), Some("test-account"));
    }
}

#[test]
fn rejected_refresh_tokens_require_reauthentication_without_echoing_tokens() {
    for code in ["refresh_token_reused", "invalid_grant", "refresh_token_expired"] {
        let body = serde_json::to_vec(&serde_json::json!({
            "error": {"code": code, "message": "sensitive-refresh-token"}
        })).unwrap();
        let error = parse_token_response(401, &body).err().unwrap();
        assert!(error.reauthenticate);
        assert!(error.message.contains("sign in"));
        assert!(!error.message.contains("sensitive-refresh-token"));
    }
    let error = parse_token_response(400, br#"{"error":"invalid_grant"}"#).err().unwrap();
    assert!(error.reauthenticate);
}

#[test]
fn transient_refresh_failure_preserves_the_session_and_does_not_save_rejected_data() {
    let error = parse_token_response(503, br#"{"message":"sensitive-data"}"#).err().unwrap();
    assert!(!error.reauthenticate);
    assert!(!error.message.contains("sensitive-data"));
    let mut coordinator = RefreshCoordinator::default();
    let (tx, rx) = oneshot::channel();
    let generation = coordinator.begin(tx).unwrap();
    let result = coordinator.complete(generation, Err(error), |_| {
        panic!("Transient failures must not overwrite stored credentials")
    }).unwrap().unwrap_err();
    assert!(!result.reauthenticate);
    assert!(rx.now_or_never().unwrap().unwrap().is_err());
}

#[test]
fn credential_debug_output_is_redacted_and_expiry_has_a_refresh_margin() {
    let credentials = ChatGPTAuthCredentials {
        access_token: "sensitive-access-token".to_owned(),
        account_id: Some("account".to_owned()),
    };
    assert!(!format!("{credentials:?}").contains("sensitive-access-token"));
    let credentials = record("sensitive-access-token", "sensitive-refresh-token");
    assert!(!format!("{credentials:?}").contains("sensitive-access-token"));
    assert!(!format!("{credentials:?}").contains("sensitive-refresh-token"));
    assert!(credentials.credentials(100).is_ok());
    assert!(credentials.credentials(9940).is_err());
    assert!(AuthRecord::default().credentials(100).is_err());
}

#[test]
fn invalid_token_payloads_are_not_saved_or_exposed() {
    assert!(parse_token_response(200, b"invalid-sensitive-data").is_err());
    assert!(record_from_tokens(TokenResponse {
        access_token: String::new(),
        refresh_token: Some("refresh".to_owned()),
        id_token: None,
        expires_in: Some(3600),
    }, None, 100).is_err());
    assert!(record_from_tokens(TokenResponse {
        access_token: "opaque-token-without-expiry".to_owned(),
        refresh_token: None,
        id_token: None,
        expires_in: None,
    }, None, 100).is_err());
}

#[test]
fn browser_authorization_keeps_pkce_and_state_validation() {
    let url = build_authorize_url("http://localhost:1455/auth/callback", "challenge", "state").unwrap();
    let params = url.query_pairs().collect::<std::collections::HashMap<_, _>>();
    assert_eq!(params.get("code_challenge_method").map(|v| v.as_ref()), Some("S256"));
    assert_eq!(params.get("state").map(|v| v.as_ref()), Some("state"));
    let callback = parse_callback_request_line("GET /auth/callback?code=code&state=state HTTP/1.1").unwrap();
    assert_eq!(callback.state, "state");
    assert!(parse_callback_request_line("GET /auth/callback?code=code HTTP/1.1").is_err());
}
