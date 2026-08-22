use super::*;

#[test]
fn truncates_large_tool_results_for_provider_request() {
    let prefix = "start:";
    let suffix = ":end";
    let middle = "x".repeat(MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST + 1_000);
    let mut message = ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "toolu_1".to_string(),
            content: format!("{prefix}{middle}{suffix}"),
            is_error: false,
        },
    };

    message.truncate_tool_results_for_provider_request();

    let MessageContent::ToolResult { content, .. } = message.content else {
        panic!("expected tool result");
    };
    assert!(content.len() <= MAX_TOOL_RESULT_CHARS_FOR_PROVIDER_REQUEST + 128);
    assert!(content.starts_with(prefix));
    assert!(content.ends_with(suffix));
    assert!(content.contains("tool result truncated"));
}

#[test]
fn usage_total_combines_disjoint_cache_and_miss_buckets() {
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 25,
        cached_input_tokens: 80,
        cache_creation_input_tokens: 10,
    };

    assert_eq!(usage.total_tokens(), 125);
}

#[test]
fn tool_events_keep_one_call_id_across_permission_and_execution() {
    let events = [
        ToolEvent::Proposed {
            call: ToolCall {
                id: "call-1".to_string(),
                name: "run_shell_command".to_string(),
                arguments: serde_json::json!({"command": "cargo test"}),
            },
        },
        ToolEvent::PermissionRequested {
            request: PermissionRequest {
                id: "permission:call-1".to_string(),
                call_id: "call-1".to_string(),
                kind: PermissionKind::Execute,
                reason: Some("Run a command".to_string()),
            },
        },
        ToolEvent::PermissionResolved {
            request_id: "permission:call-1".to_string(),
            call_id: "call-1".to_string(),
            decision: PermissionDecision::AllowOnce,
        },
        ToolEvent::Started {
            call_id: "call-1".to_string(),
        },
        ToolEvent::Completed {
            result: ToolResult {
                call_id: "call-1".to_string(),
                content: "ok".to_string(),
                status: ToolResultStatus::Success,
            },
        },
    ];

    assert!(events.iter().all(|event| event.call_id() == "call-1"));
}

#[test]
fn denied_results_are_errors_but_cancelled_results_are_distinct() {
    let denied = ToolResult {
        call_id: "denied".to_string(),
        content: "permission denied".to_string(),
        status: ToolResultStatus::Denied,
    };
    let cancelled = ToolResult {
        call_id: "cancelled".to_string(),
        content: "cancelled".to_string(),
        status: ToolResultStatus::Cancelled,
    };

    assert!(denied.is_error());
    assert!(!cancelled.is_error());
}

#[test]
fn session_runtime_prompt_is_distinct_from_provider_history() {
    let request = TurnRequest::new("acp:codex", Vec::new())
        .with_prompt(MessageContent::Text("inspect the workspace".to_owned()));

    assert_eq!(
        request.prompt,
        Some(MessageContent::Text("inspect the workspace".to_owned()))
    );
    assert!(request.messages.is_empty());
}
