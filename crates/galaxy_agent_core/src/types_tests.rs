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
fn usage_total_excludes_cached_breakdown_to_avoid_double_counting() {
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 25,
        cached_input_tokens: 80,
        cache_creation_input_tokens: 10,
    };

    assert_eq!(usage.total_tokens(), 125);
}
