use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ai::skills::{SkillProvider, SkillReference, SkillScope};
use galaxy_agent_core::{ContentPart, MessageContent, MessageRole, ToolResult, ToolResultStatus};
use galaxy_util::local_or_remote_path::LocalOrRemotePath;
use warp_multi_agent_api::ToolType;

use super::{input_messages, prepare_bedrock_rig_turn, prepare_rig_turn, tool_definitions};
use crate::ai::agent::api::RequestParams;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentActionId, AIAgentActionResult, AIAgentActionResultType, AIAgentContext, AIAgentInput,
    AnyFileContent, FileContext, MCPContext, MCPServer, RequestCommandOutputResult, UserQueryMode,
};
use crate::ai::llms::LLMId;
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::skills::SkillDescriptor;

fn config() -> OpenAIClientConfig {
    OpenAIClientConfig {
        kind: crate::settings::OpenAIProviderKind::OpenAICompatible,
        base_url: "http://localhost:4000/v1".to_string(),
        api_key: None,
        model: Some("provider-model".to_string()),
        reasoning_effort: None,
        max_input_tokens: Some(128_000),
        max_output_tokens: Some(8_192),
        use_rig: true,
        supports_system_messages: true,
    }
}

fn user_query(query: &str) -> AIAgentInput {
    user_query_with_context(query, Vec::new())
}

fn user_query_with_context(query: &str, context: Vec<AIAgentContext>) -> AIAgentInput {
    AIAgentInput::UserQuery {
        query: query.to_string(),
        context: Arc::from(context),
        static_query_type: None,
        referenced_attachments: HashMap::new(),
        user_query_mode: UserQueryMode::Normal,
        running_command: None,
        intended_agent: None,
    }
}

#[test]
fn native_context_reaches_rig_without_a_proto_context_conversion() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query_with_context(
        "Explain the selected implementation",
        vec![
            AIAgentContext::SelectedText("prepare_rig_turn(params)".to_string()),
            AIAgentContext::File(FileContext::new(
                "/repo/src/runtime.rs".to_string(),
                AnyFileContent::StringContent("fn prepare_rig_turn() {}".to_string()),
                None,
                None,
            )),
            AIAgentContext::Codebase {
                path: "/repo".to_string(),
                name: "galaxy".to_string(),
            },
        ],
    )];

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("prepare_rig_turn(params)"));
    assert!(prompt.contains("fn prepare_rig_turn() {}"));
    assert!(prompt.contains("Indexed codebase: galaxy (/repo)"));
}

#[test]
fn builds_a_rig_turn_directly_from_galaxy_request_state() {
    let mut params = RequestParams::new_for_test();
    params.model = LLMId::from("selected-model");
    params.root_task_id = Some("task-1".to_string());
    params.input = vec![user_query("Inspect this repository")];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![ToolType::ReadFiles, ToolType::RunShellCommand],
        Vec::new(),
    );

    assert_eq!(prepared.task_id, "task-1");
    assert_eq!(
        prepared.user_query.as_deref(),
        Some("Inspect this repository")
    );
    assert_eq!(prepared.request.model.as_str(), "provider-model");
    assert_eq!(prepared.request.max_output_tokens, Some(8_192));
    assert_eq!(prepared.request.messages, prepared.persistent_messages);
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "read_files"));
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "run_shell_command"));
    assert!(prepared
        .request
        .system_prompt
        .as_deref()
        .is_some_and(|prompt| prompt.contains("Galaxy owns tool permissions and execution")));
    assert!(matches!(
        &prepared.request.messages[0],
        galaxy_agent_core::ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(text),
        } if text == "Inspect this repository"
    ));
}

#[test]
fn rig_prompt_requires_follow_through_without_manual_continue_prompts() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query("Analyze and fix the issue")];

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("Continue until the user's requested outcome is complete"));
    assert!(prompt.contains("do not ask the user to say \"continue\""));
    assert!(prompt.contains("After each tool result, choose and perform the next necessary step"));
}

#[test]
fn lrc_snapshot_follow_up_uses_the_cli_monitor_prompt_and_tools() {
    let block_id: galaxy_terminal::model::BlockId = "precmd-lrc-test".to_string().into();
    let result = AIAgentActionResult {
        id: AIAgentActionId::from("run-call".to_owned()),
        task_id: TaskId::new("task".to_owned()),
        result: AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::LongRunningCommandSnapshot {
                block_id: block_id.clone(),
                command: "bash loop.sh".to_string(),
                grid_contents: "Running for 2 seconds...".to_string(),
                cursor: String::new(),
                is_alt_screen_active: false,
            },
        ),
    };
    let snapshot_tool_result = ToolResult {
        call_id: "run-call".to_string(),
        content: result.result.model_content(),
        status: ToolResultStatus::Success,
    };
    let mut params = RequestParams::new_for_test();
    params.message_history = vec![galaxy_agent_core::ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::ToolUse {
            tool_use_id: "run-call".to_string(),
            name: "run_shell_command".to_string(),
            input: serde_json::json!({
                "command": "bash loop.sh",
                "wait_until_complete": false,
            }),
        },
    }];
    params.input = vec![AIAgentInput::ActionResult {
        result,
        context: Arc::from([]),
    }];
    params.tool_results = vec![snapshot_tool_result];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![ToolType::RunShellCommand],
        vec![ToolType::ReadShellCommandOutput],
    );
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("## Running Command Monitor"));
    assert!(prompt.contains("poll with `read_shell_command_output`"));
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "read_shell_command_output"));
    assert!(!prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "recall_tool_history"));
    assert!(prepared
        .request
        .messages
        .iter()
        .any(|message| match &message.content {
            MessageContent::ToolResult { content, .. } => {
                content.contains("Command ID: precmd-lrc-test")
                    && content.contains("Continue monitoring with `read_shell_command_output`")
            }
            MessageContent::MultiPart(parts) => parts.iter().any(|part| {
                matches!(
                    part,
                    ContentPart::ToolResult { content, .. }
                        if content.contains("Command ID: precmd-lrc-test")
                )
            }),
            _ => false,
        }));
}

#[test]
fn rig_prompt_requires_matching_project_skills_to_be_read_before_action() {
    let skill_path = LocalOrRemotePath::Local(PathBuf::from(
        "/repo/.agents/skills/galaxy-skill-probe/SKILL.md",
    ));
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query_with_context(
        "Run the Galaxy skill probe",
        vec![AIAgentContext::Skills {
            skills: vec![SkillDescriptor {
                reference: SkillReference::Path(skill_path),
                name: "galaxy-skill-probe".to_string(),
                description: "Reports a deterministic project-skill probe token".to_string(),
                scope: SkillScope::Project,
                provider: SkillProvider::Agents,
                icon_override: None,
            }],
        }],
    )];

    let prepared = prepare_rig_turn(&config(), params, vec![ToolType::ReadSkill], Vec::new());
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("name=\"galaxy-skill-probe\""));
    assert!(prompt.contains("skill=\"/repo/.agents/skills/galaxy-skill-probe/SKILL.md\""));
    assert!(prompt.contains(
        "call `read_skill` once with the exact `skill` and `reference_type` values shown before acting on it"
    ));
}

#[test]
fn bedrock_rig_turn_uses_bedrock_history_invariants_without_a_proto_round_trip() {
    let mut params = RequestParams::new_for_test();
    params.message_history = vec![galaxy_agent_core::ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::Text("Prior assistant message".to_string()),
    }];
    params.input = vec![user_query("Continue safely")];

    let prepared = prepare_bedrock_rig_turn(
        "anthropic.claude-test".to_string(),
        Some(64_000),
        params,
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(prepared.request.model.as_str(), "anthropic.claude-test");
    assert_eq!(prepared.request.max_output_tokens, Some(64_000));
    assert_eq!(
        prepared
            .request
            .messages
            .first()
            .map(|message| message.role),
        Some(MessageRole::User)
    );
    assert_eq!(
        prepared.request.messages.last().map(|message| message.role),
        Some(MessageRole::User)
    );
    assert_eq!(prepared.request.messages, prepared.persistent_messages);
}

#[test]
#[allow(deprecated)]
fn grouped_mcp_tool_names_use_the_installation_id_not_the_display_name() {
    let tool = serde_json::from_value(serde_json::json!({
        "name": "echo",
        "description": "Echo input",
        "inputSchema": {
            "type": "object",
            "properties": {"message": {"type": "string"}}
        }
    }))
    .unwrap();
    let context = MCPContext {
        resources: Vec::new(),
        tools: Vec::new(),
        servers: vec![MCPServer {
            id: "11111111-1111-4111-8111-111111111111".to_string(),
            name: "Friendly Server".to_string(),
            description: String::new(),
            resources: Vec::new(),
            tools: vec![tool],
        }],
    };

    let (tools, aliases) = tool_definitions(&[ToolType::CallMcpTool], Some(&context));

    assert!(tools
        .iter()
        .any(|tool| { tool.name == "mcp__11111111-1111-4111-8111-111111111111__echo" }));
    assert!(!tools
        .iter()
        .any(|tool| tool.name == "mcp__Friendly Server__echo"));
    assert_eq!(
        aliases
            .get("mcp__11111111-1111-4111-8111-111111111111__echo")
            .map(|target| target.name.as_str()),
        Some("echo")
    );
}

#[test]
#[allow(deprecated)]
fn long_mcp_tool_names_are_provider_safe_and_reversible() {
    let original_names = [
        "performance_analyze_insight",
        "performance_start_trace",
        "performance_stop_trace",
    ];
    let context = MCPContext {
        resources: Vec::new(),
        tools: Vec::new(),
        servers: vec![MCPServer {
            id: "10804e3a-859e-4474-bf89-80e98d1dd086".to_string(),
            name: "Performance".to_string(),
            description: String::new(),
            resources: Vec::new(),
            tools: original_names
                .iter()
                .map(|name| {
                    serde_json::from_value(serde_json::json!({
                        "name": name,
                        "description": "Performance tool",
                        "inputSchema": {"type": "object"}
                    }))
                    .unwrap()
                })
                .collect(),
        }],
    };

    let (tools, aliases) = tool_definitions(&[ToolType::CallMcpTool], Some(&context));

    for original_name in original_names {
        let (alias, target) = aliases
            .iter()
            .find(|(_, target)| target.name == original_name)
            .expect("long MCP tool should have an execution alias");
        assert!(
            alias.len() <= 64,
            "alias was {} bytes: {alias}",
            alias.len()
        );
        assert!(
            alias
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "alias contains provider-unsafe characters: {alias}"
        );
        assert_ne!(
            alias,
            &format!("mcp__10804e3a-859e-4474-bf89-80e98d1dd086__{original_name}")
        );
        assert_eq!(
            target.server_id.map(|id| id.to_string()).as_deref(),
            Some("10804e3a-859e-4474-bf89-80e98d1dd086")
        );
        assert!(tools.iter().any(|tool| tool.name == *alias));
    }
}

#[test]
fn normalized_tool_outcomes_are_the_only_action_results_sent_to_rig() {
    let statuses = [
        ("read", ToolResultStatus::Success, false),
        ("shell", ToolResultStatus::Error, true),
        ("denied", ToolResultStatus::Denied, true),
        ("cancelled", ToolResultStatus::Cancelled, false),
    ];
    let tool_results = statuses
        .iter()
        .map(|(call_id, status, _)| ToolResult {
            call_id: (*call_id).to_string(),
            content: format!("normalized-{call_id}"),
            status: *status,
        })
        .collect();

    let messages = input_messages(Vec::new(), tool_results);

    assert_eq!(messages.len(), 1);
    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected normalized tool results to remain in one user turn");
    };
    for ((call_id, _, expected_error), part) in statuses.iter().zip(parts) {
        assert!(matches!(
            part,
            ContentPart::ToolResult {
                tool_use_id,
                content,
                is_error,
            } if tool_use_id == call_id
                && content == &format!("normalized-{call_id}")
                && is_error == expected_error
        ));
    }
}

#[test]
fn progressive_summary_is_provider_context_not_persistent_history() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query("Continue")];
    params.progressive_summary = Some("Earlier work was validated.".to_string());

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());

    assert_eq!(prepared.persistent_messages.len(), 1);
    assert_eq!(prepared.request.messages.len(), 3);
    assert!(matches!(
        &prepared.request.messages[0].content,
        MessageContent::Text(text) if text.contains("Earlier work was validated.")
    ));
    assert!(matches!(
        &prepared.request.messages[2].content,
        MessageContent::Text(text) if text == "Continue"
    ));
}
