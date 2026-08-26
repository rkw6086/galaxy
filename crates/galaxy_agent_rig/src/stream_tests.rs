use galaxy_agent_core::AgentErrorKind;
use rig_core::completion::CompletionError;

use super::map_completion_error;

#[test]
fn flattened_sse_http_client_error_is_recoverable_transport() {
    let error = CompletionError::ProviderError(
        "Http client error: error decoding response body".to_string(),
    );

    let mapped = map_completion_error(error);

    assert_eq!(mapped.kind, AgentErrorKind::Transport);
    assert!(mapped.recoverable);
}

#[test]
fn flattened_streamed_provider_overload_is_recoverable() {
    let error = CompletionError::ProviderError(
        r#"{"type":"error","error":{"type":"service_unavailable_error","code":"server_is_overloaded","message":"Our servers are currently overloaded. Please try again later."}}"#
            .to_string(),
    );

    let mapped = map_completion_error(error);

    assert_eq!(mapped.kind, AgentErrorKind::Provider);
    assert!(mapped.recoverable);
}
