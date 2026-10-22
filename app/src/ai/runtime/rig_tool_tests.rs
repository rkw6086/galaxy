use std::collections::HashMap;
use std::path::PathBuf;

use ai::diff_validation::ParsedDiff;
use ai::skills::{SkillPathOrigin, SkillReference};
use galaxy_agent_core::ToolCall;

use super::{action_from_tool_call, MCPToolTarget};
use crate::ai::agent::{AIAgentActionType, FileEdit, RunAgentsExecutionMode};

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "call-1".to_string(),
        name: name.to_string(),
        arguments,
    }
}

#[test]
fn computer_use_request_always_captures_a_bounded_initial_screenshot() {
    let action = action_from_tool_call("task", &call("request_computer_use", serde_json::json!({
        "task_summary": "Inspect the browser",
    })), &SkillPathOrigin::Local, &HashMap::new()).unwrap();
    let AIAgentActionType::RequestComputerUse(request) = action.action else { panic!("expected computer-use request"); };
    let screenshot = request.screenshot_params.unwrap();
    assert_eq!(screenshot.max_long_edge_px, Some(1568));
    assert_eq!(screenshot.max_total_px, Some(1_150_000));
    assert_eq!(screenshot.target, computer_use::Target::Screen);
}

#[test]
fn computer_actions_preserve_coordinates_keys_and_screenshot_choices() {
    let action = action_from_tool_call("task", &call("use_computer", serde_json::json!({
        "action_summary": "Select the field and type",
        "actions": [
            { "type": "mouse_down", "button": "left", "x": 420, "y": 310 },
            { "type": "mouse_up", "button": "left" },
            { "type": "type_text", "text": "hello" },
            { "type": "key_down", "keycode": 36 },
            { "type": "key_up", "keycode": 36 },
        ],
        "take_screenshot": false,
    })), &SkillPathOrigin::Local, &HashMap::new()).unwrap();
    let AIAgentActionType::UseComputer(request) = action.action else { panic!("expected computer actions"); };
    assert_eq!(request.actions.len(), 5);
    assert_eq!(request.actions[0].action, computer_use::Action::MouseDown {
        button: computer_use::MouseButton::Left, at: computer_use::Vector2I::new(420, 310),
    });
    assert_eq!(request.actions[3].action, computer_use::Action::KeyDown { key: computer_use::Key::Keycode(36) });
    assert_eq!(request.screenshot_params, None);
}

#[test]
fn computer_actions_reject_invalid_targets_keys_and_unbounded_waiting() {
    for actions in [
        serde_json::json!([{ "type": "key_down", "key": "Return" }]),
        serde_json::json!([{ "type": "key_up", "key": "a", "keycode": 0 }]),
        serde_json::json!([{ "type": "mouse_move", "x": 1, "y": 2, "target": { "type": "window", "window_id": 0, "pid": 12 } }]),
        serde_json::json!([{ "type": "wait", "seconds": 6 }, { "type": "wait", "seconds": 6 }]),
        serde_json::json!(vec![serde_json::json!({ "type": "wait", "seconds": 0 }); 21]),
    ] {
        assert!(action_from_tool_call("task", &call("use_computer", serde_json::json!({
            "action_summary": "Invalid request", "actions": actions,
        })), &SkillPathOrigin::Local, &HashMap::new()).is_err());
    }
}

#[test]
fn shell_calls_become_domain_actions_without_a_proto_round_trip() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "run_shell_command",
            serde_json::json!({
                "command": "cargo test",
                "is_read_only": true,
                "is_risky": false
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(action.id.to_string(), "call-1");
    assert_eq!(action.task_id.to_string(), "task-1");
    assert!(matches!(
        action.action,
        AIAgentActionType::RequestCommandOutput {
            command,
            is_read_only: Some(true),
            is_risky: Some(false),
            ..
        } if command == "cargo test"
    ));
}

#[test]
fn shell_permission_rationale_is_separate_from_executable_command() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "run_shell_command",
            serde_json::json!({
                "command": "git push origin master",
                "rationale": "The user requested a normal push to origin/master."
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();
    assert!(matches!(action.action,
        AIAgentActionType::RequestCommandOutput { command, rationale: Some(rationale), .. }
        if command == "git push origin master"
            && rationale == "The user requested a normal push to origin/master."
    ));
}

#[test]
fn transfer_control_calls_become_domain_actions() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "transfer_shell_command_control_to_user",
            serde_json::json!({"reason": "The command needs interactive input"}),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    assert!(matches!(
        action.action,
        AIAgentActionType::TransferShellCommandControlToUser { reason }
            if reason == "The command needs interactive input"
    ));
}

#[test]
fn create_plan_calls_become_document_actions() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "create_plan",
            serde_json::json!({
                "documents": [{
                    "title": "Duplicate content items",
                    "content": "# Implementation plan"
                }]
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    let AIAgentActionType::CreateDocuments(request) = action.action else {
        panic!("expected create-documents action");
    };
    assert_eq!(request.documents.len(), 1);
    assert_eq!(request.documents[0].title, "Duplicate content items");
    assert_eq!(request.documents[0].content, "# Implementation plan");
}

#[test]
fn read_files_converts_advertised_inclusive_ranges_to_half_open_ranges() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "read_files",
            serde_json::json!({
                "files": [{
                    "path": "/tmp/example.rs",
                    "line_ranges": [
                        {"start": 1, "end": 1},
                        {"start": 10, "end": 25}
                    ]
                }]
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    let AIAgentActionType::ReadFiles(request) = action.action else {
        panic!("expected read-files action");
    };
    assert_eq!(request.locations[0].lines, vec![1..2, 10..26]);
}

#[test]
fn known_tools_reject_malformed_required_inputs() {
    let cases = [
        ("read_files", serde_json::json!({}), "files"),
        (
            "read_files",
            serde_json::json!({"files": "not-an-array"}),
            "expected an array",
        ),
        (
            "read_files",
            serde_json::json!({"files": [{"path": "/tmp/a", "line_ranges": [{"start": 0, "end": 1}]}]}),
            "positive integer",
        ),
        (
            "read_files",
            serde_json::json!({"files": [{"path": "/tmp/a", "line_ranges": [{"start": 3, "end": 2}]}]}),
            "greater than or equal",
        ),
        (
            "read_files",
            serde_json::json!({"files": [{"path": "/tmp/a", "line_ranges": [{"start": 1, "end": u64::MAX}]}]}),
            "inclusive end is too large",
        ),
        (
            "grep",
            serde_json::json!({"queries": ["ok", 7]}),
            "queries[1]",
        ),
        (
            "file_glob",
            serde_json::json!({"patterns": false}),
            "expected an array",
        ),
        (
            "search_codebase",
            serde_json::json!({"query": 42}),
            "expected a string",
        ),
        (
            "apply_file_diffs",
            serde_json::json!({"summary": "edit", "diffs": [{"file_path": "/tmp/a", "search": "x"}]}),
            "replace",
        ),
        (
            "apply_file_diffs",
            serde_json::json!({"summary": "Nothing to do"}),
            "at least one diff",
        ),
        (
            "run_shell_command",
            serde_json::json!({"command": 42}),
            "expected a string",
        ),
        (
            "run_shell_command",
            serde_json::json!({"command": "  "}),
            "non-empty string",
        ),
        (
            "run_shell_command",
            serde_json::json!({"command": "pwd", "is_read_only": "yes"}),
            "expected a boolean",
        ),
        (
            "write_to_long_running_shell_command",
            serde_json::json!({"command_id": "command-1", "input": "yes", "mode": "words"}),
            "mode",
        ),
        (
            "interrupt_shell_command",
            serde_json::json!({}),
            "command_id",
        ),
        (
            "read_shell_command_output",
            serde_json::json!({"command_id": 12}),
            "expected a string",
        ),
        (
            "read_shell_command_output",
            serde_json::json!({"command_id": "command-1", "wait_seconds": 11}),
            "no greater than",
        ),
        (
            "run_agents",
            serde_json::json!({"summary": "Investigate", "agent_run_configs": []}),
            "at least one item",
        ),
        (
            "run_agents",
            serde_json::json!({"summary": "Investigate", "agent_run_configs": [{"name": "one"}]}),
            "prompt",
        ),
        (
            "run_agents",
            serde_json::json!({"summary": "Investigate", "agent_run_configs": [{"name": "one", "prompt": "Inspect"}], "execution_mode": {"type": "other"}}),
            "execution_mode.type",
        ),
        (
            "run_agents",
            serde_json::json!({"summary": "Investigate", "agent_run_configs": [{"name": "one", "prompt": "Inspect"}], "skills": [{"skill": "test", "reference_type": "other"}]}),
            "skills[0].reference_type",
        ),
        (
            "start_agent",
            serde_json::json!({"name": "worker"}),
            "prompt",
        ),
        (
            "transfer_shell_command_control_to_user",
            serde_json::json!({"reason": false}),
            "expected a string",
        ),
        (
            "wait_for_events",
            serde_json::json!({"idle_timeout_seconds": -1}),
            "non-negative",
        ),
        (
            "create_plan",
            serde_json::json!({"documents": [{"title": "Plan"}]}),
            "content",
        ),
        (
            "read_skill",
            serde_json::json!({"skill": "/tmp/SKILL.md", "reference_type": "other"}),
            "reference_type",
        ),
        (
            "fetch_conversation",
            serde_json::json!({"conversation_id": null}),
            "expected a string",
        ),
    ];

    for (name, arguments, expected_error) in cases {
        let error = action_from_tool_call(
            "task-1",
            &call(name, arguments),
            &SkillPathOrigin::Local,
            &HashMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains(expected_error),
            "{name} error {error:?} did not contain {expected_error:?}"
        );
    }
}

#[test]
fn known_tools_preserve_legitimate_optional_defaults() {
    let cases = [
        ("grep", serde_json::json!({"queries": ["needle"]})),
        ("file_glob", serde_json::json!({"patterns": ["**/*.rs"]})),
        (
            "ask_user_question",
            serde_json::json!({"question": "Continue?"}),
        ),
        (
            "apply_file_diffs",
            serde_json::json!({"summary": "Create file", "new_files": [{"file_path": "/tmp/new", "content": ""}]}),
        ),
        ("run_shell_command", serde_json::json!({"command": "pwd"})),
        (
            "write_to_long_running_shell_command",
            serde_json::json!({"command_id": "command-1", "input": ""}),
        ),
        (
            "read_shell_command_output",
            serde_json::json!({"command_id": "command-1"}),
        ),
        (
            "run_agents",
            serde_json::json!({
                "summary": "Investigate",
                "agent_run_configs": [{"name": "worker", "prompt": "Inspect"}]
            }),
        ),
        ("wait_for_events", serde_json::json!({})),
    ];

    for (name, arguments) in cases {
        action_from_tool_call(
            "task-1",
            &call(name, arguments),
            &SkillPathOrigin::Local,
            &HashMap::new(),
        )
        .unwrap_or_else(|error| panic!("{name} rejected optional defaults: {error}"));
    }
}

#[test]
fn edit_calls_preserve_file_edits_in_the_domain_model() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "apply_file_diffs",
            serde_json::json!({
                "summary": "Update greeting",
                "diffs": [{
                    "file_path": "/tmp/greeting.txt",
                    "search": "hello",
                    "replace": "hello galaxy"
                }]
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    let AIAgentActionType::RequestFileEdits { file_edits, title } = action.action else {
        panic!("expected file-edit action");
    };
    assert_eq!(title.as_deref(), Some("Update greeting"));
    assert!(matches!(
        &file_edits[0],
        FileEdit::Edit(ParsedDiff::StrReplaceEdit {
            file: Some(file),
            search: Some(search),
            replace: Some(replace),
        }) if file == "/tmp/greeting.txt" && search == "hello" && replace == "hello galaxy"
    ));
}

#[test]
fn grouped_mcp_calls_keep_the_installation_uuid_and_json_input() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "mcp__11111111-1111-4111-8111-111111111111__echo",
            serde_json::json!({"message": "hello"}),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    assert!(matches!(
        action.action,
        AIAgentActionType::CallMCPTool {
            server_id: Some(server_id),
            name,
            input,
        } if server_id.to_string() == "11111111-1111-4111-8111-111111111111"
            && name == "echo"
            && input == serde_json::json!({"message": "hello"})
    ));
}

#[test]
fn provider_safe_mcp_aliases_resolve_to_the_original_tool() {
    let server_id = uuid::Uuid::parse_str("10804e3a-859e-4474-bf89-80e98d1dd086").unwrap();
    let alias = "mcp__performance_analyze_insight__0123456789abcdef";
    let aliases = HashMap::from([(
        alias.to_string(),
        MCPToolTarget {
            server_id: Some(server_id),
            name: "performance_analyze_insight".to_string(),
            title_parameter: None,
        },
    )]);

    let action = action_from_tool_call(
        "task-1",
        &call(alias, serde_json::json!({"trace_id": "trace-1"})),
        &SkillPathOrigin::Local,
        &aliases,
    )
    .unwrap();

    assert!(matches!(
        action.action,
        AIAgentActionType::CallMCPTool {
            server_id: Some(actual_server_id),
            name,
            input,
        } if actual_server_id == server_id
            && name == "performance_analyze_insight"
            && input == serde_json::json!({"trace_id": "trace-1"})
    ));
}

#[test]
fn local_skill_paths_preserve_the_session_origin() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "read_skill",
            serde_json::json!({
                "skill": "/tmp/example/SKILL.md",
                "reference_type": "path"
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    assert!(matches!(
        action.action,
        AIAgentActionType::ReadSkill(request)
            if request.skill == SkillReference::Path(
                galaxy_util::local_or_remote_path::LocalOrRemotePath::Local(PathBuf::from(
                    "/tmp/example/SKILL.md"
                ))
            )
    ));
}

#[test]
fn run_agents_calls_decode_to_local_domain_requests_with_safe_defaults() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "run_agents",
            serde_json::json!({
                "summary": "Parallel investigation",
                "base_prompt": "Inspect before changing files.",
                "agent_run_configs": [
                    {
                        "name": "runtime",
                        "prompt": "Inspect runtime behavior",
                        "title": "Runtime investigator",
                        "model_id": "strong-model"
                    },
                    {
                        "name": "tests",
                        "prompt": "Design focused tests"
                    }
                ]
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    let AIAgentActionType::RunAgents(request) = action.action else {
        panic!("expected run-agents action");
    };
    assert_eq!(request.summary, "Parallel investigation");
    assert_eq!(request.base_prompt, "Inspect before changing files.");
    assert!(request.skills.is_empty());
    assert!(request.model_id.is_empty());
    assert!(request.harness_type.is_empty());
    assert_eq!(request.execution_mode, RunAgentsExecutionMode::Local);
    assert!(request.plan_id.is_empty());
    assert!(request.harness_auth_secret_name.is_none());
    assert_eq!(request.agent_run_configs.len(), 2);
    assert_eq!(request.agent_run_configs[0].name, "runtime");
    assert_eq!(
        request.agent_run_configs[0].prompt,
        "Inspect runtime behavior"
    );
    assert_eq!(request.agent_run_configs[0].title, "Runtime investigator");
    assert_eq!(request.agent_run_configs[0].model_id, "strong-model");
    assert_eq!(request.agent_run_configs[1].name, "tests");
    assert_eq!(request.agent_run_configs[1].prompt, "Design focused tests");
    assert!(request.agent_run_configs[1].title.is_empty());
    assert!(request.agent_run_configs[1].model_id.is_empty());
}

#[test]
fn run_agents_calls_preserve_remote_config_and_skills() {
    let action = action_from_tool_call(
        "task-1",
        &call(
            "run_agents",
            serde_json::json!({
                "summary": "Remote investigation",
                "model_id": "remote-model",
                "harness_type": "codex",
                "execution_mode": {
                    "type": "remote",
                    "environment_id": "env-1",
                    "worker_host": "worker.example",
                    "computer_use_enabled": true
                },
                "skills": [
                    {"skill": "galaxyctrl", "reference_type": "bundled"},
                    {"skill": "/repo/SKILL.md", "reference_type": "path"}
                ],
                "agent_run_configs": [{"name": "remote", "prompt": "Inspect"}],
                "plan_id": "plan-1"
            }),
        ),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    let AIAgentActionType::RunAgents(request) = action.action else {
        panic!("expected run-agents action");
    };
    assert_eq!(request.model_id, "remote-model");
    assert_eq!(request.harness_type, "codex");
    assert_eq!(request.plan_id, "plan-1");
    assert_eq!(
        request.skills,
        vec![
            SkillReference::BundledSkillId("galaxyctrl".to_string()),
            SkillReference::Path(galaxy_util::local_or_remote_path::LocalOrRemotePath::Local(
                PathBuf::from("/repo/SKILL.md")
            )),
        ]
    );
    assert_eq!(
        request.execution_mode,
        RunAgentsExecutionMode::Remote {
            environment_id: "env-1".to_string(),
            worker_host: "worker.example".to_string(),
            computer_use_enabled: true,
        }
    );
}

#[test]
fn unknown_tools_are_rejected_before_the_permission_boundary() {
    let error = action_from_tool_call(
        "task-1",
        &call("invented_tool", serde_json::json!({})),
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap_err();

    assert!(error.contains("unsupported Rig tool proposal"));
}

#[test]
fn mcp_title_metadata_does_not_change_tool_arguments() {
    let alias = "mcp__echo";
    let aliases = HashMap::from([(
        alias.to_string(),
        MCPToolTarget {
            server_id: None,
            name: "echo".to_string(),
            title_parameter: Some("_galaxy_tool_title_".to_string()),
        },
    )]);
    let arguments = serde_json::json!({
        "message": "hello",
        "_galaxy_tool_title": "a real server argument",
        "_galaxy_tool_title_": "Check the echo service"
    });
    let action = action_from_tool_call(
        "task-1",
        &call(alias, arguments),
        &SkillPathOrigin::Local,
        &aliases,
    )
    .unwrap();
    assert!(
        matches!(action.action, AIAgentActionType::CallMCPTool { input, .. }
        if input == serde_json::json!({"message": "hello", "_galaxy_tool_title": "a real server argument"}))
    );
}
