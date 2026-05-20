use serde_json::json;

use crate::ai::bedrock::convert::{ContentPart, ConversationMessage, MessageContent, MessageRole};
use super::sanitize_messages_for_bedrock;

#[test]
fn test_sanitize_messages_prepends_synthetic_tool_result_before_existing_user_text() {
    let tool_use_id = "tooluse_Pzmn1QfoWgJsA8sb4RHTM3".to_string();
    let existing_user_text = "What happened?".to_string();

    let mut messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Run a command.".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: tool_use_id.clone(),
                name: "run_shell_command".to_string(),
                input: json!({ "command": "ls" }),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(existing_user_text.clone()),
        },
    ];

    sanitize_messages_for_bedrock(&mut messages);

    assert_eq!(messages.len(), 3);

    let parts = match &messages[2].content {
        MessageContent::MultiPart(parts) => parts,
        other => panic!("Expected MultiPart content, got: {:?}", other),
    };

    assert_eq!(parts.len(), 2);
    assert!(
        matches!(&parts[0], ContentPart::ToolResult { tool_use_id: id, .. } if id == &tool_use_id)
    );
    assert!(matches!(
        &parts[1],
        ContentPart::Text(text) if text == &existing_user_text
    ));
}
