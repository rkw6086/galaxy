use std::collections::HashMap;
use std::path::PathBuf;

use ai::diff_validation::ParsedDiff;
use ai::skills::{SkillPathOrigin, SkillReference};
use galaxy_agent_core::ToolCall;

use super::{action_from_tool_call, MCPToolTarget};
use crate::ai::agent::{AIAgentActionType, FileEdit};

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "call-1".to_string(),
        name: name.to_string(),
        arguments,
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
