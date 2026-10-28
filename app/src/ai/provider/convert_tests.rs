#[test]
fn cache_preferences_do_not_enable_unsupported_or_unknown_model_caching() {
    let external = super::external_config::ExternalBedrockConfig {
        enable_prompt_caching_1h: true,
        ..Default::default()
    };
    for id in ["future.new-model-v100", "openai.gpt-5.6-sol"] {
        let config = super::convert::CachingConfig::from_external_config(&external).for_model(id);
        assert!(!config.enabled);
        assert!(!config.extended_ttl_requested);
    }
}
use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole, SystemContentBlock, Tool};
use serde_json::json;

use super::convert::*;

#[test]
fn test_text_message_converts_to_single_block() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Hello".to_string()),
    }];

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
    assert_eq!(result.messages[0].content().len(), 1);
    assert!(matches!(&result.messages[0].content()[0], ContentBlock::Text(t) if t == "Hello"));
}

#[test]
fn test_multimodal_user_message_converts_image_to_bedrock_block() {
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::MultiPart(vec![
            ContentPart::Text("Describe this image".to_string()),
            ContentPart::Image {
                data: vec![1, 2, 3, 4],
                mime_type: "image/png".to_string(),
            },
        ]),
    }];

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].content().len(), 2);
    assert!(matches!(
        &result.messages[0].content()[0],
        ContentBlock::Text(text) if text == "Describe this image"
    ));
    let ContentBlock::Image(image) = &result.messages[0].content()[1] else {
        panic!("expected Bedrock image block");
    };
    assert_eq!(
        image.format(),
        &aws_sdk_bedrockruntime::types::ImageFormat::Png
    );
    let source = image.source().expect("expected image source");
    assert_eq!(
        source
            .as_bytes()
            .expect("expected inline image bytes")
            .as_ref(),
        &[1, 2, 3, 4]
    );
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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

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
        "anthropic.claude-sonnet-4-6",
        messages,
        Some("You are a helpful assistant.".to_string()),
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig {
            enabled: false,
            extended_ttl_requested: false,
        },
    );

    assert_eq!(result.system.len(), 1);
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
}

#[test]
fn test_empty_system_prompt_produces_empty_vec() {
    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        vec![],
        Some("".to_string()),
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );
    assert!(result.system.is_empty());

    let result2 = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        vec![],
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );
    assert!(result2.system.is_empty());
}

#[test]
fn test_empty_tools_produce_none_config() {
    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        vec![],
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );
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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        vec![],
        None,
        None,
        tools,
        4096,
        None,
        None,
        None,
        CachingConfig {
            enabled: false,
            extended_ttl_requested: false,
        },
    );

    assert!(result.tool_config.is_some());
    let config = result.tool_config.unwrap();
    assert_eq!(config.tools().len(), 1);
}

#[test]
fn test_inference_config_max_tokens_only() {
    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        vec![],
        None,
        None,
        vec![],
        8192,
        None,
        None,
        None,
        CachingConfig::default(),
    );
    assert_eq!(result.inference_config.max_tokens(), Some(8192));
    assert_eq!(result.inference_config.temperature(), None);
    assert_eq!(result.inference_config.top_p(), None);
    assert!(result.inference_config.stop_sequences().is_empty());
}

#[test]
fn test_inference_config_all_params() {
    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        vec![],
        None,
        None,
        vec![],
        4096,
        Some(0.7),
        Some(0.9),
        Some(vec!["STOP".to_string()]),
        CachingConfig::default(),
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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

    assert_eq!(result.messages[0].content().len(), 2);
    assert!(matches!(
        &result.messages[0].content()[0],
        ContentBlock::Text(_)
    ));
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

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig::default(),
    );

    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role(), &ConversationRole::User);
    assert_eq!(result.messages[1].role(), &ConversationRole::Assistant);
    assert_eq!(result.messages[2].role(), &ConversationRole::User);
    assert!(matches!(
        &result.messages[2].content()[0],
        ContentBlock::ToolResult(_)
    ));
}

#[test]
fn test_caching_disabled_no_cache_points() {
    let config = CachingConfig {
        enabled: false,
        extended_ttl_requested: false,
    };

    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("first message".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("response".to_string()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("second message".to_string()),
        },
    ];

    let tools = vec![ToolDefinition {
        name: "test_tool".to_string(),
        description: "Test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        Some("System prompt".to_string()),
        None,
        tools,
        4096,
        None,
        None,
        None,
        config,
    );

    // Check that no cache points exist in messages
    for msg in &result.messages {
        for content in msg.content() {
            assert!(!matches!(content, ContentBlock::CachePoint(_)));
        }
    }

    // Check that no cache points exist in system
    for block in &result.system {
        use aws_sdk_bedrockruntime::types::SystemContentBlock;
        assert!(!matches!(block, SystemContentBlock::CachePoint(_)));
    }

    // Check that no cache points exist in tools
    if let Some(tool_config) = result.tool_config {
        for tool in tool_config.tools() {
            use aws_sdk_bedrockruntime::types::Tool;
            assert!(!matches!(tool, Tool::CachePoint(_)));
        }
    }
}

#[test]
fn test_caching_enabled_has_cache_points() {
    let config = CachingConfig::default(); // enabled by default

    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("first message".to_string()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("response".to_string()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("second message".to_string()),
        },
    ];

    let tools = vec![ToolDefinition {
        name: "test_tool".to_string(),
        description: "Test tool".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
    }];

    let result = build_converse_request(
        "anthropic.claude-sonnet-4-6",
        messages,
        Some("System prompt".to_string()),
        None,
        tools,
        4096,
        None,
        None,
        None,
        config,
    );

    // Check that cache point exists in second-to-last message (index 1 of 3)
    let second_to_last_msg = &result.messages[1];
    let has_cache_point = second_to_last_msg
        .content()
        .iter()
        .any(|c| matches!(c, ContentBlock::CachePoint(_)));
    assert!(
        has_cache_point,
        "Second-to-last message should have cache point"
    );

    // Check that cache point exists in system
    let has_system_cache = result
        .system
        .iter()
        .any(|b| matches!(b, SystemContentBlock::CachePoint(_)));
    assert!(has_system_cache, "System should have cache point");

    // Check that cache point exists in tools
    if let Some(tool_config) = result.tool_config {
        let has_tool_cache = tool_config
            .tools()
            .iter()
            .any(|t| matches!(t, Tool::CachePoint(_)));
        assert!(has_tool_cache, "Tools should have cache point");
    }
}

fn reasoning_test_request(model_id: &str, messages: Vec<ConversationMessage>) -> ConvertedRequest {
    build_converse_request(
        model_id,
        messages,
        None,
        None,
        vec![],
        4096,
        None,
        None,
        None,
        CachingConfig {
            enabled: false,
            extended_ttl_requested: false,
        },
    )
}

#[test]
fn unsupported_reasoning_history_preserves_visible_content_images_and_tool_pairs() {
    for model_id in [
        "us.deepseek.r1-v1:0",
        "amazon.nova-2-lite-v1:0",
        "openai.gpt-oss-120b-1:0",
        "qwen.qwen3-32b-v1:0",
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
    ] {
        let result = reasoning_test_request(
            model_id,
            vec![
                ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::MultiPart(vec![
                        ContentPart::Text("Inspect this image".to_owned()),
                        ContentPart::Image {
                            data: vec![1, 2, 3, 4],
                            mime_type: "image/png".to_owned(),
                        },
                    ]),
                },
                ConversationMessage {
                    role: MessageRole::Assistant,
                    content: MessageContent::MultiPart(vec![
                        ContentPart::Reasoning {
                            text: "Display-only reasoning".to_owned(),
                            signature: None,
                        },
                        ContentPart::Reasoning {
                            text: "Reasoning from an earlier Claude turn".to_owned(),
                            signature: Some("original-claude-signature".to_owned()),
                        },
                        ContentPart::Text("I will inspect the manifest.".to_owned()),
                        ContentPart::ToolUse {
                            tool_use_id: "manifest-call".to_owned(),
                            name: "read_files".to_owned(),
                            input: json!({"files": ["Cargo.toml"]}),
                        },
                    ]),
                },
                ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::ToolResult {
                        tool_use_id: "manifest-call".to_owned(),
                        content: "[package]".to_owned(),
                        is_error: false,
                    },
                },
                ConversationMessage {
                    role: MessageRole::Assistant,
                    content: MessageContent::MultiPart(vec![ContentPart::Reasoning {
                        text: "No visible assistant content".to_owned(),
                        signature: None,
                    }]),
                },
                ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::Text("Continue".to_owned()),
                },
            ],
        );
        assert_eq!(result.messages.len(), 3, "model: {model_id}");
        assert!(result.messages.iter().all(|message| {
            !message.content().is_empty()
                && message
                    .content()
                    .iter()
                    .all(|part| !matches!(part, ContentBlock::ReasoningContent(_)))
        }));
        let user_content = result.messages[0].content();
        assert!(
            matches!(&user_content[0], ContentBlock::Text(text) if text == "Inspect this image")
        );
        let ContentBlock::Image(image) = &user_content[1] else {
            panic!("the image must survive reasoning filtering");
        };
        assert_eq!(
            image.source().unwrap().as_bytes().unwrap().as_ref(),
            &[1, 2, 3, 4]
        );
        let assistant_content = result.messages[1].content();
        assert_eq!(assistant_content.len(), 2);
        assert!(
            matches!(&assistant_content[0], ContentBlock::Text(text) if text == "I will inspect the manifest.")
        );
        let ContentBlock::ToolUse(tool) = &assistant_content[1] else {
            panic!("the tool call must survive reasoning filtering");
        };
        assert_eq!(tool.tool_use_id(), "manifest-call");
        assert_eq!(tool.name(), "read_files");
        let final_user_content = result.messages[2].content();
        let ContentBlock::ToolResult(tool_result) = &final_user_content[0] else {
            panic!("the paired tool result must survive reasoning filtering");
        };
        assert_eq!(tool_result.tool_use_id(), "manifest-call");
        assert!(matches!(&final_user_content[1], ContentBlock::Text(text) if text == "Continue"));
    }
}

#[test]
fn signed_claude_reasoning_keeps_exact_text_signatures_and_tool_order() {
    use aws_sdk_bedrockruntime::types::ReasoningContentBlock;

    let result = reasoning_test_request(
        "global.anthropic.claude-sonnet-4-6[1m]",
        vec![ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::MultiPart(vec![
                ContentPart::Reasoning {
                    text: "Exact signed thinking\nincluding whitespace.\n".to_owned(),
                    signature: Some("first-signature".to_owned()),
                },
                ContentPart::Reasoning {
                    text: String::new(),
                    signature: Some("signature-only-block".to_owned()),
                },
                ContentPart::ToolUse {
                    tool_use_id: "signed-call".to_owned(),
                    name: "read_files".to_owned(),
                    input: json!({"files": ["Cargo.toml"]}),
                },
            ]),
        }],
    );
    assert_eq!(result.messages.len(), 1);
    let parts = result.messages[0].content();
    assert_eq!(parts.len(), 3);
    for (index, text, signature) in [
        (
            0,
            "Exact signed thinking\nincluding whitespace.\n",
            "first-signature",
        ),
        (1, "", "signature-only-block"),
    ] {
        let ContentBlock::ReasoningContent(ReasoningContentBlock::ReasoningText(reasoning)) =
            &parts[index]
        else {
            panic!("signed Claude reasoning must retain its native block");
        };
        assert_eq!(reasoning.text(), text);
        assert_eq!(reasoning.signature(), Some(signature));
    }
    assert!(
        matches!(&parts[2], ContentBlock::ToolUse(tool) if tool.tool_use_id() == "signed-call")
    );
}

#[test]
fn unsigned_reasoning_is_not_replayed_even_to_claude() {
    for signature in [None, Some(String::new()), Some("   ".to_owned())] {
        let result = reasoning_test_request(
            "anthropic.claude-sonnet-4-6",
            vec![ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::MultiPart(vec![
                    ContentPart::Reasoning {
                        text: "Unsigned reasoning cannot be replayed as Claude thinking".to_owned(),
                        signature,
                    },
                    ContentPart::Text("Visible answer".to_owned()),
                ]),
            }],
        );
        assert_eq!(result.messages.len(), 1);
        assert!(
            matches!(result.messages[0].content(), [ContentBlock::Text(text)] if text == "Visible answer")
        );
    }
}
