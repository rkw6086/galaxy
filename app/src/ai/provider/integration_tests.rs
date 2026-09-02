use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::json;

use super::client::{BedrockClient, BedrockClientConfig};
use super::convert::{ConversationMessage, MessageContent, MessageRole, ToolDefinition};
use crate::settings::ai::BedrockAuthMethod;

fn get_test_config() -> Option<BedrockClientConfig> {
    if std::env::var("BEDROCK_INTEGRATION_TEST").is_err() {
        return None;
    }

    let profile =
        std::env::var("BEDROCK_TEST_PROFILE").unwrap_or_else(|_| "coding-assistant".into());
    let region = std::env::var("BEDROCK_TEST_REGION").unwrap_or_else(|_| "us-east-1".into());

    Some(BedrockClientConfig {
        auth_method: BedrockAuthMethod::Profile,
        profile,
        region,
        access_key_id: String::new(),
        secret_access_key: String::new(),
        session_token: None,
        cross_region_inference: false,
    })
}

fn get_test_model() -> String {
    std::env::var("BEDROCK_TEST_MODEL").unwrap_or_else(|_| {
        "arn:aws:bedrock:us-east-1:156729649053:application-inference-profile/fma5bsw4oxhy".into()
    })
}

struct StreamOutput {
    text: String,
    tool_calls: Vec<ToolCallInfo>,
    finished_reason: Option<String>,
    total_tokens: u32,
}

#[derive(Debug)]
struct ToolCallInfo {
    name: String,
    input_json: String,
}

async fn collect_stream_output(
    client: &BedrockClient,
    model: &str,
    messages: Vec<ConversationMessage>,
    system_prompt: Option<String>,
    tools: Vec<ToolDefinition>,
) -> StreamOutput {
    let stream = client
        .converse_stream(
            model,
            "test-task-id",
            true,
            messages,
            system_prompt,
            None,
            tools,
            8192,
            None,
            false,
            None,
            None,
            Arc::new(Mutex::new(Vec::new())),
            Vec::new(),
        )
        .await
        .expect("converse_stream should succeed");

    let mut text = String::new();
    let tool_calls = Vec::new();
    let mut finished_reason = None;
    let mut total_tokens = 0u32;

    let mut stream = stream;
    while let Some(event) = stream.next().await {
        let event = event.expect("stream event should be Ok");
        if let Some(event_type) = event.r#type {
            use warp_multi_agent_api::message::Message;
            use warp_multi_agent_api::response_event::Type;
            match event_type {
                Type::ClientActions(actions) => {
                    for action in actions.actions {
                        if let Some(action_type) = action.action {
                            use warp_multi_agent_api::client_action::Action;
                            match action_type {
                                Action::AddMessagesToTask(add) => {
                                    for msg in add.messages {
                                        if let Some(Message::AgentOutput(output)) = msg.message {
                                            text.push_str(&output.text);
                                        }
                                    }
                                }
                                Action::AppendToMessageContent(append) => {
                                    if let Some(Message::AgentOutput(output)) =
                                        append.message.and_then(|m| m.message)
                                    {
                                        text.push_str(&output.text);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Type::Finished(finished) => {
                    finished_reason = Some(format!("{:?}", finished.reason));
                    if let Some(meta) = finished.conversation_usage_metadata {
                        if let Some(usage) = meta.byok_token_usage.get("bedrock") {
                            total_tokens = usage.total_tokens;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    StreamOutput {
        text,
        tool_calls,
        finished_reason,
        total_tokens,
    }
}

#[tokio::test]
async fn test_simple_text_response() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Say exactly: Hello there, how are you?".into()),
    }];

    let output = collect_stream_output(&client, &model, messages, None, vec![]).await;
    println!("[test] Text output: {:?}", output.text);
    println!("[test] Finished reason: {:?}", output.finished_reason);
    println!("[test] Total tokens: {}", output.total_tokens);

    assert!(!output.text.is_empty(), "Expected non-empty text response");
    assert!(
        output.finished_reason.is_some(),
        "Expected stream to finish"
    );
}

#[tokio::test]
async fn test_simple_with_system_prompt() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("What is your name?".into()),
    }];

    let output = collect_stream_output(
        &client,
        &model,
        messages,
        Some("You are a helpful assistant named Warp.".into()),
        vec![],
    )
    .await;
    println!("[test] Text output: {:?}", output.text);

    assert!(!output.text.is_empty());
    assert!(
        output.text.to_lowercase().contains("warp"),
        "Expected response to mention 'Warp', got: {}",
        &output.text[..output.text.len().min(200)]
    );
}

#[tokio::test]
async fn test_tool_call_round_trip() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let tools = vec![ToolDefinition {
        name: "list_files".into(),
        description: "List files in a directory".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path" }
            },
            "required": ["path"]
        }),
    }];

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("List the files in the /project directory.".into()),
    }];

    let output = collect_stream_output(&client, &model, messages, None, tools).await;
    println!(
        "[test] Text: {:?}",
        &output.text[..output.text.len().min(200)]
    );
    println!("[test] Tool calls: {:?}", output.tool_calls);
    println!("[test] Finished: {:?}", output.finished_reason);

    assert!(
        !output.text.is_empty() || !output.tool_calls.is_empty(),
        "Expected either text or a tool call"
    );
}

#[tokio::test]
async fn test_multi_turn_with_tool_result() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let tools = vec![ToolDefinition {
        name: "list_files".into(),
        description: "List files in a directory".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path" }
            },
            "required": ["path"]
        }),
    }];

    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(
                "List files in /project and tell me what you see.".into(),
            ),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "tool_1".into(),
                name: "list_files".into(),
                input: json!({"path": "/project"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "tool_1".into(),
                content: "README.md\nsrc/\nCargo.toml\n.gitignore".into(),
                is_error: false,
            },
        },
    ];

    let output = collect_stream_output(&client, &model, messages, None, tools).await;
    println!(
        "[test] Text after tool result: {:?}",
        &output.text[..output.text.len().min(300)]
    );

    assert!(
        !output.text.is_empty(),
        "Expected text response after tool result"
    );
}

#[tokio::test]
async fn test_multi_turn_conversation_with_tools() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let tools = vec![
        ToolDefinition {
            name: "list_files".into(),
            description: "List files in a directory".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path" }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "read_file".into(),
            description: "Read contents of a file".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "run_command".into(),
            description: "Run a shell command".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command" }
                },
                "required": ["command"]
            }),
        },
    ];

    let system = Some("You are a helpful coding assistant.".into());

    // Turn 1: Ask model to inspect project
    let turn1_messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text(
            "Inspect this project and tell me what it does. Start by listing files in /project."
                .into(),
        ),
    }];

    let turn1 = collect_stream_output(
        &client,
        &model,
        turn1_messages,
        system.clone(),
        tools.clone(),
    )
    .await;
    println!(
        "[test] Turn 1 text: {:?}",
        &turn1.text[..turn1.text.len().min(200)]
    );
    println!("[test] Turn 1 tool_calls: {:?}", turn1.tool_calls);

    // Turn 2: Provide tool result, continue
    let turn2_messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(
                "Inspect this project and tell me what it does. Start by listing files in /project."
                    .into(),
            ),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "tool_turn1".into(),
                name: "list_files".into(),
                input: json!({"path": "/project"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "tool_turn1".into(),
                content: "README.md\nsrc/main.rs\nsrc/lib.rs\nCargo.toml\ntests/\n.gitignore".into(),
                is_error: false,
            },
        },
    ];

    let turn2 = collect_stream_output(
        &client,
        &model,
        turn2_messages,
        system.clone(),
        tools.clone(),
    )
    .await;
    println!(
        "[test] Turn 2 text: {:?}",
        &turn2.text[..turn2.text.len().min(200)]
    );
    println!("[test] Turn 2 tool_calls: {:?}", turn2.tool_calls);

    // Turn 3: Provide README content and ask for summary
    let turn3_messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(
                "Inspect this project and tell me what it does. Start by listing files in /project."
                    .into(),
            ),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "tool_turn1".into(),
                name: "list_files".into(),
                input: json!({"path": "/project"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "tool_turn1".into(),
                content: "README.md\nsrc/main.rs\nsrc/lib.rs\nCargo.toml\ntests/\n.gitignore".into(),
                is_error: false,
            },
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "tool_turn2".into(),
                name: "read_file".into(),
                input: json!({"path": "/project/README.md"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "tool_turn2".into(),
                content: "# My CLI Tool\n\nA Rust command-line tool for managing developer workflows.\n\n## Features\n- Task tracking\n- Git integration\n- Custom scripts\n\n## Usage\n```\ncargo run -- <command>\n```".into(),
                is_error: false,
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(
                "Based on what you've seen, give me a brief summary of this project. Do not use any tools."
                    .into(),
            ),
        },
    ];

    let turn3 = collect_stream_output(
        &client,
        &model,
        turn3_messages,
        system.clone(),
        tools.clone(),
    )
    .await;
    println!(
        "[test] Turn 3 text: {:?}",
        &turn3.text[..turn3.text.len().min(500)]
    );

    assert!(
        !turn3.text.is_empty() || !turn3.tool_calls.is_empty(),
        "Expected final summary or tool use after multi-turn conversation"
    );
}

#[tokio::test]
async fn test_tool_error_recovery() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let tools = vec![ToolDefinition {
        name: "read_file".into(),
        description: "Read contents of a file".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" }
            },
            "required": ["path"]
        }),
    }];

    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Read the file /project/config.yaml".into()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "tool_err".into(),
                name: "read_file".into(),
                input: json!({"path": "/project/config.yaml"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "tool_err".into(),
                content: "Error: File not found: /project/config.yaml".into(),
                is_error: true,
            },
        },
    ];

    let output = collect_stream_output(&client, &model, messages, None, tools).await;
    println!(
        "[test] Error recovery text: {:?}",
        &output.text[..output.text.len().min(300)]
    );

    assert!(
        !output.text.is_empty(),
        "Expected model to respond to tool error"
    );
}

#[tokio::test]
async fn test_arn_based_model() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let Some(arn) = std::env::var("BEDROCK_TEST_ARN").ok() else {
        eprintln!("Skipping: BEDROCK_TEST_ARN not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Reply with the single word: confirmed".into()),
    }];

    let output = collect_stream_output(&client, &arn, messages, None, vec![]).await;
    println!("[test] ARN model text: {:?}", output.text);

    assert!(
        !output.text.is_empty(),
        "Expected response from ARN-based model"
    );
}

#[tokio::test]
async fn test_reasoning_model_output() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("What is 15 * 37? Show your reasoning step by step.".into()),
    }];

    let output = collect_stream_output(&client, &model, messages, None, vec![]).await;
    println!(
        "[test] Reasoning model text ({} chars): {:?}",
        output.text.len(),
        &output.text[..output.text.len().min(500)]
    );
    println!("[test] Finished: {:?}", output.finished_reason);

    assert!(!output.text.is_empty(), "Expected reasoning output");
    assert!(
        output.text.contains("555"),
        "Expected correct answer (555) in output, got: {}",
        &output.text[..output.text.len().min(300)]
    );
}

#[tokio::test]
async fn test_all_tools_visible_to_model() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let tools = super::request_translator::default_tool_definitions();
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();

    println!(
        "[test] Sending {} tools to Bedrock: {:?}",
        tools.len(),
        tool_names
    );

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text(
            "List every tool you have access to. Output ONLY the tool names, one per line, no descriptions, no formatting, no markdown."
                .into(),
        ),
    }];

    let output = collect_stream_output(
        &client,
        &model,
        messages,
        Some("You are a helpful assistant. When asked about your tools, list them exactly as they appear in your tool configuration.".into()),
        tools.clone(),
    )
    .await;

    println!("[test] Model's tool list response:\n{}", output.text);
    println!("[test] Total tokens: {}", output.total_tokens);

    let response_lower = output.text.to_lowercase();

    let mut missing_tools = Vec::new();
    for tool in &tools {
        if !response_lower.contains(&tool.name.to_lowercase()) {
            missing_tools.push(&tool.name);
        }
    }

    if !missing_tools.is_empty() {
        println!(
            "[test] WARNING: Model did not mention these tools: {:?}",
            missing_tools
        );
    }

    let expected_core_tools = [
        "run_shell_command",
        "read_files",
        "apply_file_diffs",
        "grep",
        "file_glob",
        "search_codebase",
        "start_agent",
        "ask_user_question",
    ];

    let mut missing_core = Vec::new();
    for name in &expected_core_tools {
        if !response_lower.contains(name) {
            missing_core.push(*name);
        }
    }

    assert!(
        missing_core.is_empty(),
        "Model failed to list these core tools: {:?}\n\nFull response:\n{}",
        missing_core,
        output.text
    );

    assert!(
        tools.len() >= 17,
        "Expected at least 17 tool definitions, got {}",
        tools.len()
    );
}

#[tokio::test]
async fn test_bedrock_integration_live_stream() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    println!("[live-test] Initializing Bedrock client...");
    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();
    println!("[live-test] Using model: {model}");

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text(
            "Write a 2-line poem about antigravity. Output only the poem.".into(),
        ),
    }];

    println!("[live-test] Requesting stream response from Bedrock...");
    let stream = client
        .converse_stream(
            &model,
            "live-test-task-id",
            true,
            messages,
            None,
            None,
            vec![],
            1024,
            None,
            false,
            None,
            None,
            Arc::new(Mutex::new(Vec::new())),
            Vec::new(),
        )
        .await
        .expect("converse_stream should succeed");

    let mut full_text = String::new();
    let mut stream = stream;

    while let Some(event) = stream.next().await {
        let event = event.expect("stream event should be Ok");
        if let Some(event_type) = event.r#type {
            use warp_multi_agent_api::message::Message;
            use warp_multi_agent_api::response_event::Type;
            match event_type {
                Type::ClientActions(actions) => {
                    for action in actions.actions {
                        if let Some(action_type) = action.action {
                            use warp_multi_agent_api::client_action::Action;
                            match action_type {
                                Action::AddMessagesToTask(add) => {
                                    for msg in add.messages {
                                        if let Some(Message::AgentOutput(output)) = msg.message {
                                            print!("{}", output.text);
                                            full_text.push_str(&output.text);
                                        }
                                    }
                                }
                                Action::AppendToMessageContent(append) => {
                                    if let Some(Message::AgentOutput(output)) =
                                        append.message.and_then(|m| m.message)
                                    {
                                        print!("{}", output.text);
                                        full_text.push_str(&output.text);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Type::Finished(finished) => {
                    println!(
                        "\n[live-test] Stream finished. Reason: {:?}",
                        finished.reason
                    );
                    if let Some(meta) = finished.conversation_usage_metadata {
                        if let Some(usage) = meta.byok_token_usage.get("bedrock") {
                            println!("[live-test] Token Usage - Total: {}", usage.total_tokens);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    println!("[live-test] Received full response:\n{}", full_text.trim());
    assert!(!full_text.is_empty(), "Model returned an empty response");
}
