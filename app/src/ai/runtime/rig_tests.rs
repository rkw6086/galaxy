use std::sync::{Arc, Mutex};

use ai::skills::SkillPathOrigin;
use galaxy_agent_core::{
    ContentPart, MessageContent, MessageRole, StopReason, ToolCall, ToolResult, ToolResultStatus,
};
use warp_multi_agent_api::response_event::stream_finished;

use super::{
    append_tool_result, build_add_reasoning, build_append_reasoning, build_tool_proposed,
    map_stop_reason, saturating_i32, sync_assistant_turn,
};

#[test]
fn stop_reasons_map_to_the_existing_ui_contract() {
    assert!(matches!(
        map_stop_reason(StopReason::Completed),
        stream_finished::Reason::Done(_)
    ));
    assert!(matches!(
        map_stop_reason(StopReason::MaxTokens),
        stream_finished::Reason::MaxTokenLimit(_)
    ));
    assert!(matches!(
        map_stop_reason(StopReason::Cancelled),
        stream_finished::Reason::Other(_)
    ));
}

#[test]
fn token_counts_saturate_at_the_proto_limit() {
    assert_eq!(saturating_i32(u64::MAX), i32::MAX);
}

#[test]
fn reasoning_events_match_the_existing_ui_message_contract() {
    let add = build_add_reasoning("task", "message", "think");
    let append = build_append_reasoning("task", "message", " more");

    let Some(warp_multi_agent_api::response_event::Type::ClientActions(add)) = add.r#type else {
        panic!("expected client actions");
    };
    let Some(warp_multi_agent_api::client_action::Action::AddMessagesToTask(add)) =
        &add.actions[0].action
    else {
        panic!("expected add-message action");
    };
    assert!(matches!(
        add.messages[0].message.as_ref(),
        Some(warp_multi_agent_api::message::Message::AgentReasoning(reasoning))
            if reasoning.reasoning == "think"
    ));

    let Some(warp_multi_agent_api::response_event::Type::ClientActions(append)) = append.r#type
    else {
        panic!("expected client actions");
    };
    let Some(warp_multi_agent_api::client_action::Action::AppendToMessageContent(append)) =
        &append.actions[0].action
    else {
        panic!("expected append-message action");
    };
    assert_eq!(
        append.mask.as_ref().unwrap().paths,
        ["agent_reasoning.reasoning"]
    );
}

#[test]
fn tool_proposal_matches_the_domain_permission_contract() {
    let action = build_tool_proposed(
        "task",
        &ToolCall {
            id: "call-1".to_string(),
            name: "run_shell_command".to_string(),
            arguments: serde_json::json!({
                "command": "cargo test",
                "is_read_only": true
            }),
        },
        &SkillPathOrigin::Local,
    )
    .unwrap();

    assert_eq!(action.id.to_string(), "call-1");
    assert!(matches!(
        action.action,
        crate::ai::agent::AIAgentActionType::RequestCommandOutput {
            command,
            is_read_only: Some(true),
            ..
        } if command == "cargo test"
    ));
}

#[test]
fn mcp_tool_proposal_routes_directly_to_the_mcp_executor_contract() {
    let action = build_tool_proposed(
        "task",
        &ToolCall {
            id: "call-mcp".to_string(),
            name: "mcp__11111111-1111-4111-8111-111111111111__read_file".to_string(),
            arguments: serde_json::json!({"path": "Cargo.toml"}),
        },
        &SkillPathOrigin::Local,
    )
    .unwrap();

    assert!(matches!(
        action.action,
        crate::ai::agent::AIAgentActionType::CallMCPTool {
            server_id: Some(server_id),
            name,
            ..
        } if server_id.to_string() == "11111111-1111-4111-8111-111111111111"
            && name == "read_file"
    ));
}

#[test]
fn assistant_history_is_updated_before_fast_tool_execution_can_continue() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut history_index = None;
    let first_call = ToolCall {
        id: "call-1".to_string(),
        name: "read_files".to_string(),
        arguments: serde_json::json!({"files": ["Cargo.toml"]}),
    };
    let second_call = ToolCall {
        id: "call-2".to_string(),
        name: "grep".to_string(),
        arguments: serde_json::json!({"queries": ["rig"]}),
    };

    sync_assistant_turn(
        &messages,
        "",
        None,
        "I'll inspect both.",
        std::slice::from_ref(&first_call),
        &mut history_index,
    );
    sync_assistant_turn(
        &messages,
        "",
        None,
        "I'll inspect both.",
        &[first_call, second_call],
        &mut history_index,
    );

    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected combined assistant content");
    };
    assert_eq!(parts.len(), 3);
    assert!(
        matches!(&parts[0], galaxy_agent_core::ContentPart::Text(text) if text == "I'll inspect both.")
    );
    assert!(
        matches!(&parts[1], galaxy_agent_core::ContentPart::ToolUse { tool_use_id, .. } if tool_use_id == "call-1")
    );
    assert!(
        matches!(&parts[2], galaxy_agent_core::ContentPart::ToolUse { tool_use_id, .. } if tool_use_id == "call-2")
    );
}

#[test]
fn signed_reasoning_is_persisted_before_the_tool_call() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut history_index = None;
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "read_files".to_string(),
        arguments: serde_json::json!({"files": ["Cargo.toml"]}),
    };

    sync_assistant_turn(
        &messages,
        "I should inspect the manifest.",
        Some("signed-reasoning"),
        "",
        std::slice::from_ref(&call),
        &mut history_index,
    );

    let messages = messages.lock().unwrap();
    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected reasoning and tool call parts");
    };
    assert!(matches!(
        parts.as_slice(),
        [
            ContentPart::Reasoning {
                text,
                signature: Some(signature),
            },
            ContentPart::ToolUse { tool_use_id, .. },
        ] if text == "I should inspect the manifest."
            && signature == "signed-reasoning"
            && tool_use_id == "call-1"
    ));
}

#[test]
fn rejected_tool_result_is_paired_with_the_assistant_call_in_history() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut history_index = None;
    let call = ToolCall {
        id: "call-unknown".to_string(),
        name: "invented_tool".to_string(),
        arguments: serde_json::json!({}),
    };
    sync_assistant_turn(
        &messages,
        "",
        None,
        "",
        std::slice::from_ref(&call),
        &mut history_index,
    );
    append_tool_result(
        &messages,
        ToolResult {
            call_id: call.id.clone(),
            content: "tool is unavailable".to_string(),
            status: ToolResultStatus::Error,
        },
    );

    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(matches!(
        &messages[0].content,
        MessageContent::ToolUse {
            tool_use_id,
            name,
            ..
        } if tool_use_id == "call-unknown" && name == "invented_tool"
    ));
    assert_eq!(messages[1].role, MessageRole::User);
    assert!(matches!(
        &messages[1].content,
        MessageContent::ToolResult {
            tool_use_id,
            content,
            is_error: true,
        } if tool_use_id == "call-unknown" && content == "tool is unavailable"
    ));
}
