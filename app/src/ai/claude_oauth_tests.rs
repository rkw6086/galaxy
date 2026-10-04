use super::desktop::{authorize_url, callback_code};

#[test]
fn browser_login_matches_pi_pkce_parameters() {
    let url = authorize_url("test-verifier", "test-challenge").unwrap();
    assert_eq!(url.origin().ascii_serialization(), "https://claude.ai");
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["redirect_uri"], "http://localhost:53692/callback");
    assert_eq!(params["client_id"], "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    assert_eq!(params["state"], "test-verifier");
    assert_eq!(params["code_challenge"], "test-challenge");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["code"], "true");
    assert!(params["scope"].contains("user:inference"));
}

#[test]
fn callback_requires_matching_state_and_expected_path() {
    assert_eq!(
        callback_code("GET /callback?code=abc&state=expected HTTP/1.1", "expected"),
        Some("abc".to_string())
    );
    for request in [
        "GET /callback?code=abc&state=wrong HTTP/1.1",
        "GET /callback?code=abc HTTP/1.1",
        "GET /callback?state=expected HTTP/1.1",
        "GET /favicon.ico?code=abc&state=expected HTTP/1.1",
        "POST /callback?code=abc&state=expected HTTP/1.1",
        "GET //evil.example/callback?code=abc&state=expected HTTP/1.1",
    ] {
        assert_eq!(callback_code(request, "expected"), None);
    }
}

#[test]
fn persisted_credentials_do_not_count_as_api_keys() {
    assert!(crate::ai::claude_subscription::is_stored_credential(
        "galaxy-claude-oauth:test"
    ));
    assert!(crate::ai::claude_subscription::is_subscription_credential(
        "galaxy-claude-oauth:test"
    ));
    assert!(!crate::ai::claude_subscription::is_stored_credential(
        "sk-ant-api-test"
    ));
}
