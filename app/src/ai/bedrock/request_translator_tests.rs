use serde_json::json;
use warp_multi_agent_api as api;

use super::{
    extract_new_input_messages, extract_system_prompt, extract_tools, sanitize_messages_for_bedrock,
};
use crate::ai::bedrock::convert::{ContentPart, ConversationMessage, MessageContent, MessageRole};

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

#[test]
fn advertised_tools_follow_client_capabilities_and_include_local_subagents() {
    let request = api::Request {
        settings: Some(api::request::Settings {
            supported_tools: vec![
                api::ToolType::RunShellCommand.into(),
                api::ToolType::ReadFiles.into(),
                api::ToolType::Subagent.into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    };

    let names = extract_tools(&request)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["run_shell_command", "read_files", "start_agent"]
    );
}

#[test]
fn plan_mode_prompt_prohibits_mutation() {
    let request = api::Request {
        input: Some(api::request::Input {
            r#type: Some(api::request::input::Type::UserInputs(
                api::request::input::UserInputs {
                    inputs: vec![api::request::input::user_inputs::UserInput {
                        input: Some(
                            api::request::input::user_inputs::user_input::Input::UserQuery(
                                api::request::input::UserQuery {
                                    query: "plan the change".to_string(),
                                    mode: Some(api::UserQueryMode {
                                        r#type: Some(api::user_query_mode::Type::Plan(())),
                                    }),
                                    ..Default::default()
                                },
                            ),
                        ),
                    }],
                },
            )),
            ..Default::default()
        }),
        ..Default::default()
    };

    let prompt = extract_system_prompt(&request, &[]).unwrap();
    assert!(prompt.contains("## Plan Mode"));
    assert!(prompt.contains("do not edit files"));
}

#[test]
fn running_command_turn_gets_monitor_prompt_and_cli_tools() {
    let request = api::Request {
        input: Some(api::request::Input {
            r#type: Some(api::request::input::Type::UserInputs(
                api::request::input::UserInputs {
                    inputs: vec![api::request::input::user_inputs::UserInput {
                        input: Some(
                            api::request::input::user_inputs::user_input::Input::CliAgentUserQuery(
                                api::request::input::CliAgentUserQuery {
                                    user_query: Some(api::request::input::UserQuery {
                                        query: "monitor this".to_string(),
                                        ..Default::default()
                                    }),
                                    running_command: Some(api::RunningShellCommand {
                                        command: "cargo test".to_string(),
                                        snapshot: Some(api::LongRunningShellCommandSnapshot {
                                            command_id: "block-123".to_string(),
                                            ..Default::default()
                                        }),
                                    }),
                                    ..Default::default()
                                },
                            ),
                        ),
                    }],
                },
            )),
            ..Default::default()
        }),
        settings: Some(api::request::Settings {
            supported_tools: vec![api::ToolType::RunShellCommand.into()],
            supported_cli_agent_tools: vec![
                api::ToolType::WriteToLongRunningShellCommand.into(),
                api::ToolType::ReadShellCommandOutput.into(),
                api::ToolType::TransferShellCommandControlToUser.into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    };

    let names = extract_tools(&request)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "write_to_long_running_shell_command",
            "read_shell_command_output",
            "transfer_shell_command_control_to_user",
        ]
    );

    let prompt = extract_system_prompt(&request, &[]).unwrap();
    assert!(prompt.contains("## Running Command Monitor"));
    assert!(prompt.contains("command ID"));
    assert!(prompt.contains("read_shell_command_output"));
    assert!(!prompt.contains("- Use `run_shell_command`"));
}

#[test]
fn long_running_tool_result_preserves_command_id() {
    let request = api::Request {
        input: Some(api::request::Input {
            r#type: Some(api::request::input::Type::UserInputs(
                api::request::input::UserInputs {
                    inputs: vec![api::request::input::user_inputs::UserInput {
                        input: Some(
                            api::request::input::user_inputs::user_input::Input::ToolCallResult(
                                api::request::input::ToolCallResult {
                                    tool_call_id: "tool-1".to_string(),
                                    result: Some(
                                        api::request::input::tool_call_result::Result::RunShellCommand(
                                            api::RunShellCommandResult {
                                                command: "cargo test".to_string(),
                                                result: Some(
                                                    api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                                                        api::LongRunningShellCommandSnapshot {
                                                            command_id: "block-456".to_string(),
                                                            output: "running 42 tests".to_string(),
                                                            ..Default::default()
                                                        },
                                                    ),
                                                ),
                                                ..Default::default()
                                            },
                                        ),
                                    ),
                                },
                            ),
                        ),
                    }],
                },
            )),
            ..Default::default()
        }),
        ..Default::default()
    };

    let messages = extract_new_input_messages(&request);
    let MessageContent::ToolResult { content, .. } = &messages[0].content else {
        panic!("expected tool result");
    };
    assert!(content.contains("Command ID: block-456"));
    assert!(content.contains("running 42 tests"));
    assert!(content.contains("read_shell_command_output"));
}
