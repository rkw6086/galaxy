use galaxy_agent_core::{AgentErrorKind, StopReason};
use rig_core::completion::CompletionError;

use super::{
    completion_error_stop_reason, domain_tool_call, is_keepalive_event, map_completion_error,
};

#[test]
fn domain_tool_call_prefers_responses_call_id() {
    let tool_call = rig_core::message::ToolCall::new(
        "fc_item_123".to_string(),
        rig_core::message::ToolFunction {
            name: "read_files".to_string(),
            arguments: serde_json::json!({"files": ["Cargo.toml"]}),
        },
    )
    .with_call_id("call_123".to_string());

    let call = domain_tool_call(tool_call);

    assert_eq!(call.id, "call_123");
    assert_eq!(call.name, "read_files");
}

#[test]
fn domain_tool_call_falls_back_to_wire_id_without_responses_call_id() {
    let tool_call = rig_core::message::ToolCall::new(
        "fc_item_123".to_string(),
        rig_core::message::ToolFunction {
            name: "read_files".to_string(),
            arguments: serde_json::json!({"files": ["Cargo.toml"]}),
        },
    );

    let call = domain_tool_call(tool_call);

    assert_eq!(call.id, "fc_item_123");
    assert_eq!(call.name, "read_files");
}

#[test]
fn provider_context_window_error_maps_to_semantic_stop_reason_and_kind() {
    let status = rig_core::http_client::Response::builder()
        .status(400)
        .body(())
        .unwrap()
        .status();
    let error = CompletionError::from_http_response(
        status,
        r#"{"error":{"message":"Your input exceeds the context window of this model. Please adjust your input and try again.","code":"400"}}"#,
    );

    assert_eq!(
        completion_error_stop_reason(&error),
        Some(StopReason::ContextWindowExceeded)
    );
    let mapped = map_completion_error(error);
    assert_eq!(mapped.kind, AgentErrorKind::ContextWindowExceeded);
    assert!(!mapped.recoverable);
}

#[test]
fn provider_context_length_string_maps_to_semantic_stop_reason() {
    let error = CompletionError::ProviderError(
        "context_length_exceeded: maximum context length is 128000 tokens".to_string(),
    );

    assert_eq!(
        completion_error_stop_reason(&error),
        Some(StopReason::ContextWindowExceeded)
    );
}

#[test]
fn provider_keepalive_event_is_a_transport_heartbeat() {
    assert!(is_keepalive_event(&serde_json::json!({
        "type": "keepalive",
        "sequence_number": 3,
    })));
    assert!(!is_keepalive_event(&serde_json::json!({
        "type": "unsupported",
    })));
}

#[test]
fn provider_server_error_is_recoverable() {
    let status = rig_core::http_client::Response::builder()
        .status(503)
        .body(())
        .unwrap()
        .status();
    let error = CompletionError::from_http_response(
        status,
        r#"{"error":{"message":"Service temporarily unavailable"}}"#,
    );

    let mapped = map_completion_error(error);
    assert_eq!(mapped.kind, AgentErrorKind::Provider);
    assert!(mapped.recoverable);
}

#[test]
fn flattened_bedrock_credential_errors_are_authentication_failures() {
    for message in [
        r#"{"__type":"ExpiredTokenException","message":"The security token included in the request is expired"}"#,
        "UnrecognizedClientException: The security token included in the request is invalid",
        "AccessDeniedException: not authorized to perform bedrock:ConverseStream",
    ] {
        let mapped = map_completion_error(CompletionError::ProviderError(message.to_string()));
        assert_eq!(mapped.kind, AgentErrorKind::Authentication);
        assert!(!mapped.recoverable);
        assert!(mapped.user_message.is_some());
    }
}
