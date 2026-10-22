use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ai::skills::{SkillProvider, SkillReference, SkillScope};
use galaxy_agent_core::{ContentPart, MessageContent, MessageRole, ToolResult, ToolResultStatus};
use galaxy_util::local_or_remote_path::LocalOrRemotePath;
use warp_multi_agent_api::ToolType;

use super::{
    add_orchestration_model_options, input_messages, prepare_bedrock_rig_turn, prepare_rig_turn,
    prepare_rig_turn_for_mode, tool_definitions, OrchestrationModelOption, RigRequestMode,
};
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
        kind: crate::settings::OpenAIProviderKind::LiteLLM,
        base_url: "http://localhost:4000/v1".to_string(),
        api_key: None,
        project_id: None,
        location: None,
        model: Some("provider-model".to_string()),
        reasoning_effort: None,
        max_input_tokens: Some(128_000),
        max_output_tokens: Some(8_192),
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
fn volatile_request_time_follows_the_cacheable_system_prompt_prefix() {
    let mut params = RequestParams::new_for_test();
    params.global_rules = vec![(
        "Stable rule".to_string(),
        "Preserve this cacheable instruction.".to_string(),
    )];
    params.input = vec![user_query_with_context(
        "Inspect the cache layout",
        vec![AIAgentContext::CurrentTime {
            current_time: chrono::Local::now(),
        }],
    )];

    let prepared = prepare_rig_turn(&config(), params, vec![ToolType::ReadFiles], Vec::new());
    let prompt = prepared.request.system_prompt.expect("system prompt");
    let rule_position = prompt
        .find("Preserve this cacheable instruction.")
        .expect("global rule");
    let tools_position = prompt.find("## Available Tools").expect("tool contract");
    let time_position = prompt.find("## Request Time").expect("request time");

    assert!(rule_position < time_position);
    assert!(tools_position < time_position);
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
fn system_prompt_prioritizes_semantic_code_search_over_regex_search() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query(
        "Find the implementation that handles ProviderRun",
    )];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![
            ToolType::RunShellCommand,
            ToolType::SearchCodebase,
            ToolType::FileGlob,
            ToolType::Grep,
            ToolType::ReadFiles,
        ],
        Vec::new(),
    );
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("read it directly; do not search again to rediscover it"));
    assert!(prompt.contains("Use scoped `grep`/`rg` for exact symbols"));
    assert!(
        prompt.contains("Prefer `file_glob` for filenames and `read_files` for focused content")
    );
    assert!(prompt.contains("do not use shell `find`, `grep`, `rg`, `cat`, `head`, or `tail`"));
}

#[test]
fn normal_turn_advertises_plan_creation_and_corrects_false_unavailability_claims() {
    let mut params = RequestParams::new_for_test();
    params.planning_enabled = true;
    params.input = vec![user_query("Please create a plan, and let's review.")];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![ToolType::CreateDocuments],
        Vec::new(),
    );
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "create_plan"));
    assert!(prompt.contains("Plan document creation is available through `create_plan`"));
    assert!(prompt.contains("do not merely return the plan as prose"));
    assert!(prompt.contains("no plan-creation tool is available"));
    assert!(prompt.contains("do not implement it until they approve"));
}

#[test]
fn plan_turn_requires_a_plan_tool_result() {
    let mut params = RequestParams::new_for_test();
    let mut input = user_query("Design the implementation");
    let AIAgentInput::UserQuery {
        user_query_mode, ..
    } = &mut input
    else {
        unreachable!("user_query returns a user query input");
    };
    *user_query_mode = UserQueryMode::Plan;
    params.input = vec![input];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![ToolType::CreateDocuments, ToolType::EditDocuments],
        Vec::new(),
    );
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "create_plan"));
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "edit_plan"));
    assert!(prompt.contains("finish by calling `create_plan`"));
    assert!(prompt.contains("call `edit_plan` instead"));
    assert!(prompt.contains("Do not return the plan only as prose"));
}

#[test]
fn no_tools_turn_flattens_historical_tool_protocol_messages() {
    let mut params = RequestParams::new_for_test();
    params.message_history = vec![
        galaxy_agent_core::ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "call-1".to_string(),
                name: "run_shell_command".to_string(),
                input: serde_json::json!({
                    "command": "find . -name package.json",
                    "wait_until_complete": true,
                }),
            },
        },
        galaxy_agent_core::ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "command exited with code 1".to_string(),
                is_error: true,
            },
        },
    ];
    params.input = vec![user_query("Summarize what happened")];

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());

    assert_eq!(prepared.request.tools.len(), 2);
    assert!(prepared.request.tools.iter().any(|tool| tool.name == "recall_tool_history"));
    assert!(prepared.request.tools.iter().any(|tool| tool.name == "update_task_list"));
    assert!(prepared
        .request
        .messages
        .iter()
        .all(|message| !message.content.contains_tool_protocol_blocks()));
    assert!(prepared.request.messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::Text(text)
            if text.contains("Previous tool call `run_shell_command`")
                && text.contains("call-1")
    )));
    assert!(prepared.request.messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::Text(text)
            if text.contains("Previous tool result for id `call-1` (error)")
    )));
    assert!(prepared.persistent_messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::ToolUse { tool_use_id, .. } if tool_use_id == "call-1"
    )));
}

#[test]
fn tool_enabled_turn_preserves_structured_tool_history() {
    let mut params = RequestParams::new_for_test();
    params.message_history = vec![
        galaxy_agent_core::ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "call-1".to_string(),
                name: "read_files".to_string(),
                input: serde_json::json!({"files": ["Cargo.toml"]}),
            },
        },
        galaxy_agent_core::ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "[package]\nname = \"galaxy\"".to_string(),
                is_error: false,
            },
        },
    ];
    params.input = vec![user_query("Keep inspecting")];

    let prepared = prepare_rig_turn(&config(), params, vec![ToolType::ReadFiles], Vec::new());

    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "read_files"));
    assert!(prepared.request.messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::ToolUse { tool_use_id, .. } if tool_use_id == "call-1"
    )));
    assert!(prepared.request.messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::ToolResult { tool_use_id, .. } if tool_use_id == "call-1"
    )));
}

trait MessageContentTestExt {
    fn contains_tool_protocol_blocks(&self) -> bool;
}

impl MessageContentTestExt for MessageContent {
    fn contains_tool_protocol_blocks(&self) -> bool {
        match self {
            MessageContent::ToolUse { .. } | MessageContent::ToolResult { .. } => true,
            MessageContent::MultiPart(parts) => parts.iter().any(|part| {
                matches!(
                    part,
                    ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. }
                )
            }),
            MessageContent::Text(_) => false,
        }
    }
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
#[allow(deprecated)]
fn completed_command_assessment_uses_root_history_without_tools_or_monitor_instructions() {
    let block_id: galaxy_terminal::model::BlockId = "completed-lrc-test".to_string().into();
    let mcp_tool = serde_json::from_value(serde_json::json!({
        "name": "echo",
        "description": "Echo input",
        "inputSchema": {"type": "object"}
    }))
    .unwrap();
    let mut params = RequestParams::new_for_test();
    params.root_task_id = Some("root-task".to_string());
    params.message_history = vec![galaxy_agent_core::ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Prior root conversation".to_string()),
    }];
    params.mcp_context = Some(MCPContext {
        resources: Vec::new(),
        tools: Vec::new(),
        servers: vec![MCPServer {
            id: "11111111-1111-4111-8111-111111111111".to_string(),
            name: "Echo".to_string(),
            description: String::new(),
            resources: Vec::new(),
            tools: vec![mcp_tool],
        }],
    });
    params.input = vec![AIAgentInput::CommandCompletionAssessment {
        prompt: "Report the final result to the user.".to_string(),
        context: Arc::from([]),
        completed_command: crate::ai::agent::RunningCommand {
            command: "bash loop.sh".to_string(),
            block_id,
            grid_contents: "All 42 checks passed.".to_string(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        },
    }];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![ToolType::RunShellCommand, ToolType::CallMcpTool],
        vec![ToolType::ReadShellCommandOutput],
    );
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert_eq!(prepared.task_id, "root-task");
    assert_eq!(prepared.user_query, None);
    assert!(prepared.request.tools.is_empty());
    assert!(prepared.mcp_tool_aliases.is_empty());
    assert!(prompt.contains("## Completed Command Assessment"));
    assert!(prompt.contains("No tools are available"));
    assert!(!prompt.contains("## Running Command Monitor"));
    assert!(!prompt.contains("next assistant output MUST be a tool call"));
    assert!(prepared.persistent_messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::Text(text) if text == "Prior root conversation"
    )));
    assert!(prepared.persistent_messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::Text(text)
            if text.contains("[Completed command: bash loop.sh]")
                && text.contains("[Command ID: completed-lrc-test]")
                && text.contains("[Final terminal output:\nAll 42 checks passed.")
                && text.contains("Report the final result to the user.")
    )));
    assert_eq!(prepared.request.messages, prepared.persistent_messages);
}

#[test]
fn completed_command_assessment_bounds_only_the_provider_payload() {
    let full_output = format!("first\n{}\nlast", "x".repeat(100_000));
    let block_id: galaxy_terminal::model::BlockId = "large-completed-command".to_string().into();
    let mut params = RequestParams::new_for_test();
    params.context_window_limit = Some(8_000);
    params.input = vec![AIAgentInput::CommandCompletionAssessment {
        prompt: "Report the final result to the user.".to_string(),
        context: Arc::from([]),
        completed_command: crate::ai::agent::RunningCommand {
            command: "verbose-command".to_string(),
            block_id,
            grid_contents: full_output.clone(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        },
    }];

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());
    let MessageContent::Text(provider_text) = &prepared.request.messages[0].content else {
        panic!("expected text command assessment");
    };

    assert!(provider_text.contains("terminal output truncated for model context"));
    assert!(provider_text.contains("full output remains available in the terminal"));
    assert!(provider_text.contains("first"));
    assert!(provider_text.contains("last"));
    assert!(!provider_text.contains(&full_output));
    assert_eq!(full_output.chars().count(), 100_011);
}

#[test]
fn forced_cli_profile_uses_monitor_prompt_and_tools_for_an_initial_query() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query("Run the checks")];

    let prepared = prepare_rig_turn_for_mode(
        &config(),
        params,
        vec![ToolType::RunShellCommand],
        vec![ToolType::ReadShellCommandOutput],
        RigRequestMode::Cli,
    );
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("## Running Command Monitor"));
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "read_shell_command_output"));
    assert!(!prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "run_shell_command"));
    assert!(!prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "recall_tool_history"));
    assert!(!prepared.request.tools.iter().any(|tool| tool.name == "update_task_list"));
}

#[test]
fn task_progress_is_available_with_explicit_status_guidance_and_ui_title() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query("Implement and verify the fix")];
    let prepared = prepare_rig_turn(&config(), params, vec![ToolType::ReadFiles], Vec::new());
    let tool = prepared.request.tools.iter().find(|tool| tool.name == "update_task_list").unwrap();
    assert!(tool.input_schema["required"].as_array().unwrap().iter().any(|value| value == "_galaxy_tool_title"));
    let prompt = prepared.request.system_prompt.unwrap();
    assert!(prompt.contains("## Task List Progress"));
    assert!(prompt.contains("reconcile the list before the final response"));
    assert!(prompt.contains("Planning or returning a draft does not complete"));
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
        images: Vec::new(),
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
    assert!(
        prompt.contains("Use the command ID from the tool result for every read/write operation")
    );
    assert!(prompt.contains("one concise, user-facing sentence"));
    assert!(prompt.contains("Do not send a text-only progress response"));
    assert!(prompt.contains("alternate screen containing `(END)` is `less`"));
    assert!(prompt.contains("`write_to_long_running_shell_command` with input `q` and mode `raw`"));
    assert!(prompt.contains("make the polling tool call in the same response"));
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
                    && content.contains("The next assistant output MUST be a tool call")
            }
            MessageContent::MultiPart(parts) => parts.iter().any(|part| {
                matches!(
                    part,
                    ContentPart::ToolResult { content, .. }
                        if content.contains("Command ID: precmd-lrc-test")
                            && content.contains("The next assistant output MUST be a tool call")
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
fn modern_and_legacy_orchestration_tools_follow_subagent_capabilities() {
    for capability in [ToolType::Subagent, ToolType::StartAgentV2] {
        let (tools, _) = tool_definitions(&[capability], None);
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"run_agents"));
        assert!(names.contains(&"start_agent"));
    }

    let (leaf_tools, _) = tool_definitions(&[ToolType::Grep], None);
    assert!(!leaf_tools
        .iter()
        .any(|tool| matches!(tool.name.as_str(), "run_agents" | "start_agent")));
}

#[test]
fn run_agents_tool_requires_a_best_fit_model_from_the_available_catalog() {
    let (mut tools, _) = tool_definitions(&[ToolType::Subagent], None);
    add_orchestration_model_options(
        &mut tools,
        &[
            OrchestrationModelOption {
                id: "strong-model".to_string(),
                display_name: "Strong Model".to_string(),
                provider: "Provider A".to_string(),
                quality: Some(0.95),
                cost: Some(0.8),
                credit_multiplier: Some(2.0),
            },
            OrchestrationModelOption {
                id: "efficient-model".to_string(),
                display_name: "Efficient Model".to_string(),
                provider: "Provider B".to_string(),
                quality: Some(0.9),
                cost: Some(0.3),
                credit_multiplier: Some(0.5),
            },
        ],
    );

    let run_agents = tools
        .iter()
        .find(|tool| tool.name == "run_agents")
        .expect("run_agents tool");
    let model = &run_agents.input_schema["properties"]["agent_run_configs"]["items"]["properties"]
        ["model_id"];
    assert_eq!(
        model["enum"],
        serde_json::json!(["strong-model", "efficient-model"])
    );
    assert!(model["description"]
        .as_str()
        .is_some_and(|description| description.contains("Prioritize the model best suited")));
    assert!(
        run_agents.input_schema["properties"]["agent_run_configs"]["items"]["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|field| field == "model_id"))
    );
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
            images: Vec::new(),
            call_id: (*call_id).to_string(),
            content: format!("normalized-{call_id}"),
            status: *status,
        })
        .collect();

    let messages = input_messages(Vec::new(), tool_results, Some(128_000));

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
    assert_eq!(prepared.request.messages.len(), 2);
    assert!(matches!(
        &prepared.request.messages[0].content,
        MessageContent::Text(text) if text.contains("Earlier work was validated.")
    ));
    assert!(matches!(
        &prepared.request.messages[1].content,
        MessageContent::Text(text) if text == "Continue"
    ));
}

#[test]
fn tool_title_schema_preserves_existing_parameters() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {"_galaxy_tool_title": {"type": "integer"}},
        "required": ["_galaxy_tool_title"],
        "additionalProperties": false
    });
    let parameter = super::add_tool_title_parameter(&mut schema);
    assert_eq!(parameter, "_galaxy_tool_title_");
    assert_eq!(
        schema["properties"]["_galaxy_tool_title"]["type"],
        "integer"
    );
    assert_eq!(schema["properties"][&parameter]["type"], "string");
    assert_eq!(
        schema["required"],
        serde_json::json!(["_galaxy_tool_title", "_galaxy_tool_title_"])
    );
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn model_workflow_guidance_handles_provider_aliases_and_unknown_models() {
    assert!(
        super::model_workflow_guidance("litellm/codex-gpt-5.6-sol-high")
            .contains("next concrete step")
    );
    assert!(super::model_workflow_guidance("GPT-6-astra").contains("next concrete step"));
    assert!(
        super::model_workflow_guidance("us.anthropic.claude-sonnet-v1")
            .contains("one coherent slice")
    );
    assert_eq!(super::model_workflow_guidance("unknown-model"), "");
}
