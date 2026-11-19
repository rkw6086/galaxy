use galaxy_agent_core::ToolCall;
use prost::Message;
use serde_json::json;
use warp_multi_agent_api as api;

use super::{result_message, tool_message};
use crate::ai::agent::api::convert_conversation::ConvertToExchanges;
use crate::ai::agent::{
    AIAgentActionResult, AIAgentActionResultType, AIAgentInput, GrepResult,
    RequestCommandOutputResult, RunAgentsAgentOutcome, RunAgentsAgentOutcomeKind,
    RunAgentsLaunchedExecutionMode, RunAgentsResult,
};
use crate::ai::runtime::ProviderActionContext;

fn query(id: &str) -> api::Message {
    api::Message {
        id: id.to_owned(),
        task_id: "task".into(),
        message: Some(api::message::Message::UserQuery(api::message::UserQuery {
            query: id.to_owned(),
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[test]
fn computer_tool_display_and_native_restoration_preserve_the_same_actions() {
    let context = ProviderActionContext::new_for_test("task");
    let call = ToolCall {
        id: "desktop-call".into(),
        name: "use_computer".into(),
        arguments: json!({
            "action_summary": "Type and press Return", "actions": [
                { "type": "type_text", "text": "hello" },
                { "type": "key_down", "keycode": 36 },
                { "type": "key_up", "keycode": 36 },
            ],
        }),
    };
    let action = context.action_from_tool_call(&call).unwrap();
    let message = tool_message(&context, &call, &action).unwrap();
    let Some(api::message::Message::ToolCall(display)) = message.message else {
        panic!("expected tool display");
    };
    let Some(api::message::tool_call::Tool::UseComputer(display)) = display.tool else {
        panic!("expected computer display");
    };
    let display_action = crate::ai::agent::AIAgentActionType::try_from(display).unwrap();
    assert_eq!(display_action, action.action);
    let restored = super::restore_tool(&message.server_message_data)
        .unwrap()
        .unwrap();
    assert_eq!(restored.action, action.action);
    assert_eq!(restored.id, action.id);
}

#[test]
fn saved_tools_and_results_restore_as_individual_panes_across_turns() {
    let context = ProviderActionContext::new_for_test("task");
    let cases = [
        (
            ToolCall {
                id: "edit-call".into(),
                name: "apply_file_diffs".into(),
                arguments: json!({"summary": "Update file", "diffs": [{"file_path": "/project/file", "search": "before", "replace": "after"}]}),
            },
            AIAgentActionResultType::RequestFileEdits(
                crate::ai::agent::RequestFileEditsResult::Success {
                    diff: "-before\n+after".into(),
                    updated_files: Vec::new(),
                    deleted_files: Vec::new(),
                    lines_added: 1,
                    lines_removed: 1,
                },
            ),
        ),
        (
            ToolCall {
                id: "cancelled-grep".into(),
                name: "grep".into(),
                arguments: json!({"queries": ["needle"], "path": "/project"}),
            },
            AIAgentActionResultType::Grep(GrepResult::Cancelled),
        ),
        (
            ToolCall {
                id: "shell-call".into(),
                name: "run_shell_command".into(),
                arguments: json!({"command": "cat large.txt"}),
            },
            AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
                block_id: "shell-block".to_owned().into(),
                command: "cat large.txt".into(),
                output: "complete output line\n".repeat(10_000),
                exit_code: galaxy_core::command::ExitCode::from(0),
                start_ts: None,
                completed_ts: None,
            }),
        ),
        (
            ToolCall {
                id: "grep-call".into(),
                name: "grep".into(),
                arguments: json!({"queries": ["needle"], "path": "/project"}),
            },
            AIAgentActionResultType::Grep(GrepResult::Error("path unavailable".into())),
        ),
        (
            ToolCall {
                id: "agents-call".into(),
                name: "run_agents".into(),
                arguments: json!({
                    "summary": "Implement and review", "execution_mode": {"type": "local"},
                    "agent_run_configs": [{"name": "reviewer", "prompt": "Review changes", "model_id": "review-model"}]
                }),
            },
            AIAgentActionResultType::RunAgents(RunAgentsResult::Launched {
                model_id: "review-model".into(),
                harness_type: String::new(),
                execution_mode: RunAgentsLaunchedExecutionMode::Local,
                agents: vec![RunAgentsAgentOutcome {
                    name: "reviewer".into(),
                    kind: RunAgentsAgentOutcomeKind::Launched {
                        agent_id: "child-conversation".into(),
                    },
                }],
            }),
        ),
    ];
    let mut task = api::Task {
        id: "task".into(),
        ..Default::default()
    };
    for (index, (call, result)) in cases.iter().enumerate() {
        let request_id = format!("turn-{index}");
        let action_result = AIAgentActionResult {
            id: call.id.clone().into(),
            task_id: crate::ai::agent::task::TaskId::new("task".to_owned()),
            result: result.clone(),
        };
        for mut message in [
            query(&request_id),
            tool_message(
                &context,
                call,
                &context.action_from_tool_call(call).unwrap(),
            )
            .unwrap(),
            result_message(&action_result).unwrap(),
        ] {
            message.request_id = request_id.clone();
            task.messages.push(message);
        }
    }
    // Exercise the same protobuf bytes stored in agent_tasks, followed by UI hydration.
    let restored = api::Task::decode(task.encode_to_vec().as_slice()).unwrap();
    let exchanges = (&restored).into_exchanges();
    assert_eq!(exchanges.len(), cases.len());
    for (exchange, (call, expected_result)) in exchanges.iter().zip(cases.iter()) {
        let output = exchange.output_status.output().unwrap();
        let output = output.get();
        let actions = output.actions().collect::<Vec<_>>();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], &context.action_from_tool_call(call).unwrap());
        let result = exchange
            .input
            .iter()
            .find_map(|input| match input {
                AIAgentInput::ActionResult { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        assert_eq!(&result.result, expected_result);
    }
}

#[test]
fn legacy_messages_without_request_ids_split_at_each_user_query() {
    let task = api::Task {
        id: "task".into(),
        messages: vec![
            query("first"),
            api::Message {
                id: "reply".into(),
                task_id: "task".into(),
                message: Some(api::message::Message::AgentOutput(
                    api::message::AgentOutput {
                        text: "First reply".into(),
                    },
                )),
                ..Default::default()
            },
            query("second"),
        ],
        ..Default::default()
    };
    assert_eq!((&task).into_exchanges().len(), 2);
}

#[test]
fn restored_question_keeps_the_id_used_by_its_answer() {
    let context = ProviderActionContext::new_for_test("task");
    let call = ToolCall {
        id: "question-call".into(),
        name: "ask_user_question".into(),
        arguments: json!({"question": "Which option?", "options": ["A", "B"]}),
    };
    let action = context.action_from_tool_call(&call).unwrap();
    let message = tool_message(&context, &call, &action).unwrap();
    assert_eq!(
        super::restore_tool(&message.server_message_data)
            .unwrap()
            .unwrap(),
        action
    );
}
