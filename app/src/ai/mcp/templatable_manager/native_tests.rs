use futures_util::stream::AbortHandle;

use super::*;

#[test]
fn oauth_callback_preserves_decoded_issuer() {
    let mut manager = TemplatableMCPServerManager::default();
    let installation_uuid = Uuid::new_v4();
    let (abort_handle, _) = AbortHandle::new_pair();
    let (oauth_result_tx, oauth_result_rx) = async_channel::unbounded();
    manager.spawned_servers.insert(
        installation_uuid,
        SpawnedServerInfo {
            abort_handle,
            oauth_result_tx,
        },
    );
    manager
        .pending_oauth_csrf
        .insert("test-state".to_string(), installation_uuid);
    let callback_url = Url::parse(
        "galaxydev://mcp/oauth2callback?code=test-code&state=test-state&iss=https%3A%2F%2Fmcp.linear.app",
    ).unwrap();
    manager.handle_oauth_callback(&callback_url).unwrap();
    match oauth_result_rx.try_recv().unwrap() {
        CallbackResult::Success {
            code,
            csrf_token,
            issuer,
        } => {
            assert_eq!(code, "test-code");
            assert_eq!(csrf_token, "test-state");
            assert_eq!(issuer.as_deref(), Some("https://mcp.linear.app"));
        }
        CallbackResult::Error { error } => panic!("unexpected callback error: {error:?}"),
    }
}
