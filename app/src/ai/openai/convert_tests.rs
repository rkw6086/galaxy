use serde_json::json;

use crate::ai::openai::convert::build_openai_request;
use crate::ai::provider::types::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
};

#[test]
fn test_simple_text_message_conversion() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Hello world".to_string()),
    }];

    let request = build_openai_request(messages, None, vec![], 1024, None, "test-model");

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "Hello world");
    assert_eq!(request["model"], "test-model");
    assert_eq!(request["max_tokens"], 1024);
    assert_eq!(request["stream"], true);
}

#[test]
fn test_system_prompt_placement() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Hi".to_string()),
    }];

    let request = build_openai_request(
        messages,
        Some("You are a helpful assistant.".to_string()),
        vec![],
        1024,
        None,
        "test-model",
    );

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "You are a helpful assistant.");
    assert_eq!(msgs[1]["role"], "user");
}

#[test]
fn test_assistant_tool_use_conversion() {
    let messages = vec![ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::ToolUse {
            tool_use_id: "call_123".to_string(),
            name: "run_shell_command".to_string(),
            input: json!({"command": "ls -la"}),
        },
    }];

    let request = build_openai_request(messages, None, vec![], 1024, None, "test-model");

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "assistant");
    assert!(msgs[0]["content"].is_null());

    let tool_calls = msgs[0]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], "call_123");
    assert_eq!(tool_calls[0]["type"], "function");
    assert_eq!(tool_calls[0]["function"]["name"], "run_shell_command");
    assert_eq!(
        tool_calls[0]["function"]["arguments"],
        json!({"command": "ls -la"}).to_string()
    );
}

#[test]
fn test_tool_result_conversion() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "call_123".to_string(),
            content: "file1.txt\nfile2.txt".to_string(),
            is_error: false,
        },
    }];

    let request = build_openai_request(messages, None, vec![], 1024, None, "test-model");

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "tool");
    assert_eq!(msgs[0]["tool_call_id"], "call_123");
    assert_eq!(msgs[0]["content"], "file1.txt\nfile2.txt");
}

#[test]
fn test_tool_result_error_conversion() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "call_456".to_string(),
            content: "command not found".to_string(),
            is_error: true,
        },
    }];

    let request = build_openai_request(messages, None, vec![], 1024, None, "test-model");

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "tool");
    assert_eq!(msgs[0]["content"], "[ERROR] command not found");
}

#[test]
fn test_multipart_assistant_message() {
    let messages = vec![ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::MultiPart(vec![
            ContentPart::Text("I'll run that command for you.".to_string()),
            ContentPart::ToolUse {
                tool_use_id: "call_abc".to_string(),
                name: "run_shell_command".to_string(),
                input: json!({"command": "pwd"}),
            },
        ]),
    }];

    let request = build_openai_request(messages, None, vec![], 1024, None, "test-model");

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["content"], "I'll run that command for you.");

    let tool_calls = msgs[0]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], "call_abc");
    assert_eq!(tool_calls[0]["function"]["name"], "run_shell_command");
}

#[test]
fn test_multipart_user_message_with_tool_results() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::MultiPart(vec![
            ContentPart::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "result 1".to_string(),
                is_error: false,
            },
            ContentPart::ToolResult {
                tool_use_id: "call_2".to_string(),
                content: "result 2".to_string(),
                is_error: false,
            },
        ]),
    }];

    let request = build_openai_request(messages, None, vec![], 1024, None, "test-model");

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "tool");
    assert_eq!(msgs[0]["tool_call_id"], "call_1");
    assert_eq!(msgs[1]["role"], "tool");
    assert_eq!(msgs[1]["tool_call_id"], "call_2");
}

#[test]
fn test_tool_definitions_conversion() {
    let tools = vec![
        ToolDefinition {
            name: "run_shell_command".to_string(),
            description: "Runs a shell command".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        },
        ToolDefinition {
            name: "read_files".to_string(),
            description: "Reads files from disk".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "files": {"type": "array", "items": {"type": "string"}}
                }
            }),
        },
    ];

    let request = build_openai_request(vec![], None, tools, 1024, None, "test-model");

    let tool_defs = request["tools"].as_array().unwrap();
    assert_eq!(tool_defs.len(), 2);
    assert_eq!(tool_defs[0]["type"], "function");
    assert_eq!(tool_defs[0]["function"]["name"], "run_shell_command");
    assert_eq!(
        tool_defs[0]["function"]["description"],
        "Runs a shell command"
    );
    assert_eq!(tool_defs[0]["function"]["parameters"]["type"], "object");
    assert_eq!(tool_defs[1]["function"]["name"], "read_files");
}

#[test]
fn test_temperature_handling() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("test".to_string()),
    }];

    // Temperature absent when None
    let request = build_openai_request(messages.clone(), None, vec![], 1024, None, "test-model");
    assert!(request.get("temperature").is_none());

    // Temperature present when Some
    let request = build_openai_request(messages, None, vec![], 1024, Some(0.7), "test-model");
    assert!(request.get("temperature").is_some());
}

#[test]
fn test_full_conversation_roundtrip() {
    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("List files".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "call_1".to_string(),
                name: "run_shell_command".to_string(),
                input: json!({"command": "ls"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "file1.rs\nfile2.rs".to_string(),
                is_error: false,
            },
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("Here are the files: file1.rs and file2.rs".to_string()),
        },
    ];

    let request = build_openai_request(
        messages,
        Some("You are Galaxy AI.".to_string()),
        vec![],
        4096,
        None,
        "claude-sonnet",
    );

    let msgs = request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 5); // system + 4 conversation messages
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[4]["role"], "assistant");
}
