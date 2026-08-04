use super::*;
use crate::MessageRole;

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: String::new(),
        input_schema: serde_json::json!({"type": "object"}),
    }
}

fn tool_exchange(id: &str, name: &str, input: JsonValue, result: &str) -> Vec<ConversationMessage> {
    vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: id.to_string(),
                name: name.to_string(),
                input,
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: id.to_string(),
                content: result.to_string(),
                is_error: false,
            },
        },
    ]
}

#[test]
fn executes_only_tools_advertised_for_this_turn() {
    let policy = ToolPolicy::new(&[definition("read_files")]);
    let read = ToolCall {
        id: "read-1".to_string(),
        name: "read_files".to_string(),
        arguments: serde_json::json!({"files": ["Cargo.toml"]}),
    };
    let shell = ToolCall {
        id: "shell-1".to_string(),
        name: "run_shell_command".to_string(),
        arguments: serde_json::json!({"command": "pwd"}),
    };

    assert_eq!(policy.decide(&read, &[], &[]), ToolCallDecision::Execute);
    let ToolCallDecision::Reject(result) = policy.decide(&shell, &[], &[]) else {
        panic!("unadvertised tool should be rejected");
    };
    assert_eq!(result.call_id, "shell-1");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result.content.contains("read_files"));
    assert!(
        !result
            .content
            .contains("available tools are: run_shell_command")
    );
}

#[test]
fn recall_searches_archived_and_live_results_with_live_results_most_recent() {
    let policy = ToolPolicy::new(&[definition(RECALL_TOOL_HISTORY_NAME)]);
    let archive = tool_exchange(
        "archived-read",
        "read_files",
        serde_json::json!({"files": ["old.txt"]}),
        "old contents",
    );
    let messages = tool_exchange(
        "live-read",
        "read_files",
        serde_json::json!({"files": ["new.txt"]}),
        "new contents",
    );
    let latest = ToolCall {
        id: "recall-latest".to_string(),
        name: RECALL_TOOL_HISTORY_NAME.to_string(),
        arguments: serde_json::json!({"tool_name": "read_files"}),
    };
    let previous = ToolCall {
        id: "recall-previous".to_string(),
        name: RECALL_TOOL_HISTORY_NAME.to_string(),
        arguments: serde_json::json!({
            "tool_name": "read_files",
            "offset_from_end": 1,
        }),
    };

    let ToolCallDecision::Inline(latest_result) = policy.decide(&latest, &messages, &archive)
    else {
        panic!("recall should execute inline");
    };
    assert!(latest_result.content.contains("Tool Use ID: live-read"));
    assert!(latest_result.content.contains("new contents"));

    let ToolCallDecision::Inline(previous_result) = policy.decide(&previous, &messages, &archive)
    else {
        panic!("recall should execute inline");
    };
    assert!(
        previous_result
            .content
            .contains("Tool Use ID: archived-read")
    );
    assert!(previous_result.content.contains("old contents"));
}

#[test]
fn recall_supports_exact_call_id_and_case_insensitive_text_search() {
    let messages = tool_exchange(
        "shell-7",
        "run_shell_command",
        serde_json::json!({"command": "cargo test"}),
        "ALL TESTS PASSED",
    );

    let exact = recall_tool_history(
        &messages,
        &[],
        ToolHistoryQuery {
            tool_use_id: "shell-7",
            search_query: "all tests",
            ..Default::default()
        },
    );
    let missing = recall_tool_history(
        &messages,
        &[],
        ToolHistoryQuery {
            tool_use_id: "missing",
            ..Default::default()
        },
    );

    assert!(exact.contains("cargo test"));
    assert!(exact.contains("ALL TESTS PASSED"));
    assert_eq!(
        missing,
        "No matching tool calls found in conversation history."
    );
}

#[test]
fn loop_guard_detects_repeated_failures_and_resets_after_detection() {
    let mut guard = ToolLoopGuard::new(5, 3);
    guard.record_failure(7, "cargo test failed");
    guard.record_failure(11, "another command failed");
    guard.record_failure(7, "cargo test failed again");
    assert_eq!(guard.detect_and_reset(), None);

    guard.record_failure(7, "cargo test failed a third time");
    assert_eq!(
        guard.detect_and_reset(),
        Some(ToolLoopDetected {
            description: "cargo test failed a third time".to_string(),
            threshold: 3,
        })
    );
    assert_eq!(guard.detect_and_reset(), None);
}

#[test]
fn loop_guard_clears_failures_when_a_tool_makes_progress() {
    let mut guard = ToolLoopGuard::new(5, 2);
    guard.record_failure(7, "first failure");
    guard.record_success();
    guard.record_failure(7, "failure after success");

    assert_eq!(guard.detect_and_reset(), None);
}
