use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole};
use serde_json::json;

use super::convert::*;

#[test]
fn test_text_message_converts_to_single_block() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Hello".to_string()),
    }];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
    assert_eq!(result.messages[0].content().len(), 1);
    assert!(matches!(&result.messages[0].content()[0], ContentBlock::Text(t) if t == "Hello"));
}

#[test]
fn test_tool_use_produces_valid_json_input() {
    let messages = vec![ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::ToolUse {
            tool_use_id: "tool_123".to_string(),
            name: "read_file".to_string(),
            input: json!({"path": "/tmp/test.txt"}),
        },
    }];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role(), &ConversationRole::Assistant);
    match &result.messages[0].content()[0] {
        ContentBlock::ToolUse(block) => {
            assert_eq!(block.tool_use_id(), "tool_123");
            assert_eq!(block.name(), "read_file");
        }
        other => panic!("Expected ToolUse, got {:?}", other),
    }
}

#[test]
fn test_tool_result_with_matching_id() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "tool_123".to_string(),
            content: "file contents here".to_string(),
            is_error: false,
        },
    }];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages.len(), 1);
    match &result.messages[0].content()[0] {
        ContentBlock::ToolResult(block) => {
            assert_eq!(block.tool_use_id(), "tool_123");
        }
        other => panic!("Expected ToolResult, got {:?}", other),
    }
}

#[test]
fn test_tool_result_error_status() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "tool_456".to_string(),
            content: "permission denied".to_string(),
            is_error: true,
        },
    }];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    match &result.messages[0].content()[0] {
        ContentBlock::ToolResult(block) => {
            assert_eq!(
                block.status(),
                Some(&aws_sdk_bedrockruntime::types::ToolResultStatus::Error)
            );
        }
        other => panic!("Expected ToolResult, got {:?}", other),
    }
}

#[test]
fn test_consecutive_same_role_messages_coalesced() {
    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("first".to_string()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("second".to_string()),
        },
    ];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].content().len(), 2);
    assert!(matches!(&result.messages[0].content()[0], ContentBlock::Text(t) if t == "first"));
    assert!(matches!(&result.messages[0].content()[1], ContentBlock::Text(t) if t == "second"));
}

#[test]
fn test_alternating_roles_not_coalesced() {
    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("question".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("answer".to_string()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("followup".to_string()),
        },
    ];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
    assert_eq!(result.messages[1].role(), &ConversationRole::Assistant);
    assert_eq!(result.messages[2].role(), &ConversationRole::User);
}

#[test]
fn test_system_prompt_separated_from_messages() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("hi".to_string()),
    }];

    let result = build_converse_request(
        messages,
        Some("You are a helpful assistant.".to_string()),
        vec![],
        4096,
        None,
        None,
        None,
    );

    assert_eq!(result.system.len(), 1);
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
}

#[test]
fn test_empty_system_prompt_produces_empty_vec() {
    let result = build_converse_request(vec![], Some("".to_string()), vec![], 4096, None, None, None);
    assert!(result.system.is_empty());

    let result2 = build_converse_request(vec![], None, vec![], 4096, None, None, None);
    assert!(result2.system.is_empty());
}

#[test]
fn test_empty_tools_produce_none_config() {
    let result = build_converse_request(vec![], None, vec![], 4096, None, None, None);
    assert!(result.tool_config.is_none());
}

#[test]
fn test_tool_definitions_produce_tool_config() {
    let tools = vec![ToolDefinition {
        name: "read_file".to_string(),
        description: "Read a file from disk".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path"}
            },
            "required": ["path"]
        }),
    }];

    let result = build_converse_request(vec![], None, tools, 4096, None, None, None);

    assert!(result.tool_config.is_some());
    let config = result.tool_config.unwrap();
    assert_eq!(config.tools().len(), 1);
}

#[test]
fn test_inference_config_max_tokens_only() {
    let result = build_converse_request(vec![], None, vec![], 8192, None, None, None);
    assert_eq!(result.inference_config.max_tokens(), Some(8192));
    assert_eq!(result.inference_config.temperature(), None);
    assert_eq!(result.inference_config.top_p(), None);
    assert!(result.inference_config.stop_sequences().is_empty());
}

#[test]
fn test_inference_config_all_params() {
    let result = build_converse_request(
        vec![],
        None,
        vec![],
        4096,
        Some(0.7),
        Some(0.9),
        Some(vec!["STOP".to_string()]),
    );
    assert_eq!(result.inference_config.max_tokens(), Some(4096));
    assert_eq!(result.inference_config.temperature(), Some(0.7));
    assert_eq!(result.inference_config.top_p(), Some(0.9));
    assert_eq!(result.inference_config.stop_sequences(), &["STOP"]);
}

#[test]
fn test_multipart_content_produces_multiple_blocks() {
    let messages = vec![ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::MultiPart(vec![
            ContentPart::Text("Let me help.".to_string()),
            ContentPart::ToolUse {
                tool_use_id: "tu_1".to_string(),
                name: "run_command".to_string(),
                input: json!({"command": "ls"}),
            },
        ]),
    }];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages[0].content().len(), 2);
    assert!(matches!(&result.messages[0].content()[0], ContentBlock::Text(_)));
    assert!(matches!(
        &result.messages[0].content()[1],
        ContentBlock::ToolUse(_)
    ));
}

#[test]
fn test_tool_result_after_tool_use_coalesced_into_user_message() {
    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Do something".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "tu_1".to_string(),
                name: "cmd".to_string(),
                input: json!({}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "tu_1".to_string(),
                content: "done".to_string(),
                is_error: false,
            },
        },
    ];

    let result = build_converse_request(messages, None, vec![], 4096, None, None, None);

    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
    assert_eq!(result.messages[1].role(), &ConversationRole::Assistant);
    assert_eq!(result.messages[2].role(), &ConversationRole::User);
    assert!(matches!(
        &result.messages[2].content()[0],
        ContentBlock::ToolResult(_)
    ));
}
