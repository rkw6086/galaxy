use serde_json::json;

use crate::ai::openai::request_translator::sanitize_messages_for_openai;
use crate::ai::provider::types::{ContentPart, ConversationMessage, MessageContent, MessageRole};

#[test]
fn test_removes_orphaned_tool_results() {
    let mut messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Hello".to_string()),
        },
        // This tool result references a tool_use that doesn't exist
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "nonexistent_id".to_string(),
                content: "some result".to_string(),
                is_error: false,
            },
        },
    ];

    sanitize_messages_for_openai(&mut messages);

    assert_eq!(messages.len(), 1);
    matches!(&messages[0].content, MessageContent::Text(_));
}

#[test]
fn test_keeps_valid_tool_results() {
    let mut messages = vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "valid_id".to_string(),
                name: "run_shell_command".to_string(),
                input: json!({"command": "ls"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "valid_id".to_string(),
                content: "file1.txt".to_string(),
                is_error: false,
            },
        },
    ];

    sanitize_messages_for_openai(&mut messages);

    assert_eq!(messages.len(), 2);
}

#[test]
fn test_synthesizes_missing_tool_results() {
    let mut messages = vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "unanswered_id".to_string(),
                name: "read_files".to_string(),
                input: json!({"files": ["test.rs"]}),
            },
        },
        // No corresponding tool result!
    ];

    sanitize_messages_for_openai(&mut messages);

    // Should have synthesized a tool result
    assert_eq!(messages.len(), 2);
    match &messages[1].content {
        MessageContent::ToolResult {
            tool_use_id,
            is_error,
            ..
        } => {
            assert_eq!(tool_use_id, "unanswered_id");
            assert!(*is_error);
        }
        _ => panic!("Expected ToolResult"),
    }
}

#[test]
fn test_does_not_require_user_assistant_alternation() {
    let mut messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("First message".to_string()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Second message".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("Response".to_string()),
        },
    ];

    sanitize_messages_for_openai(&mut messages);

    // Both user messages should remain — OpenAI allows consecutive same-role.
    // A trailing user message is appended since it ends with assistant.
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::User);
    assert_eq!(messages[2].role, MessageRole::Assistant);
    assert_eq!(messages[3].role, MessageRole::User);
}

#[test]
fn test_does_not_require_starting_with_user() {
    let mut messages = vec![ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::Text("I start the conversation".to_string()),
    }];

    sanitize_messages_for_openai(&mut messages);

    // Should NOT prepend a user message (unlike Bedrock), but WILL append
    // a user continuation because Bedrock (via LiteLLM) requires the
    // conversation to end with a user message.
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert_eq!(messages[1].role, MessageRole::User);
}

#[test]
fn test_multipart_tool_uses_all_get_results() {
    let mut messages = vec![ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::MultiPart(vec![
            ContentPart::ToolUse {
                tool_use_id: "id_1".to_string(),
                name: "grep".to_string(),
                input: json!({"queries": ["test"]}),
            },
            ContentPart::ToolUse {
                tool_use_id: "id_2".to_string(),
                name: "file_glob".to_string(),
                input: json!({"patterns": ["*.rs"]}),
            },
        ]),
    }];

    sanitize_messages_for_openai(&mut messages);

    // Should synthesize results for both unanswered tool calls in a single
    // user message with MultiPart content
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert_eq!(messages[1].role, MessageRole::User);
    match &messages[1].content {
        MessageContent::MultiPart(parts) => {
            assert_eq!(parts.len(), 2);
            for part in parts {
                match part {
                    ContentPart::ToolResult { is_error, .. } => assert!(is_error),
                    _ => panic!("Expected ToolResult part"),
                }
            }
        }
        _ => panic!("Expected MultiPart content"),
    }
}

#[test]
fn test_ensure_ends_with_user_message_no_op_when_already_user() {
    let mut messages = vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("Hello".to_string()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Hi back".to_string()),
        },
    ];

    sanitize_messages_for_openai(&mut messages);

    // Already ends with user — no extra message appended
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, MessageRole::User);
}

#[test]
fn test_ensure_ends_with_user_message_empty_messages() {
    let mut messages: Vec<ConversationMessage> = vec![];

    sanitize_messages_for_openai(&mut messages);

    // Empty messages should stay empty
    assert!(messages.is_empty());
}

#[test]
fn sanitizer_preserves_image_parts() {
    let image_bytes = b"\x89PNG\r\n\x1a\nsanitizer".to_vec();
    let mut messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::MultiPart(vec![
            ContentPart::Text("Describe this".to_string()),
            ContentPart::Image {
                data: image_bytes.clone(),
                mime_type: "image/png".to_string(),
            },
        ]),
    }];

    sanitize_messages_for_openai(&mut messages);

    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected multimodal message");
    };
    assert!(matches!(
        &parts[1],
        ContentPart::Image { data, mime_type }
            if data == &image_bytes && mime_type == "image/png"
    ));
}
