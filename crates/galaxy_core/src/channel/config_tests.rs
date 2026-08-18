use super::WarpServerConfig;

#[test]
fn disabled_warp_services_are_loopback_only() {
    let config = WarpServerConfig::disabled();

    assert_eq!(config.server_root_url, "http://127.0.0.1:9");
    assert_eq!(config.rtc_server_url, "ws://127.0.0.1:9/graphql/v2");
    assert!(config.session_sharing_server_url.is_none());
    assert!(config.firebase_auth_api_key.is_empty());
    assert!(config.iap_config.is_none());
}
