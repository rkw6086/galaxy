use std::sync::Arc;

use chrono::{DateTime, Utc};
use galaxy_core::command::ExitCode;
use warp_multi_agent_api as api;

use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentActionResult, AIAgentActionResultType, AIAgentContext, AIAgentInput, ImageContext,
    RunningCommand, TransferShellCommandControlToUserResult, UserQueryMode,
};
use crate::terminal::model::block::BlockId;

#[test]
fn image_context_decodes_base64_into_proto_bytes() {
    let api_context = super::convert_context(&[AIAgentContext::Image(ImageContext {
        data: "AQIDBA==".to_string(),
        mime_type: "image/png".to_string(),
        file_name: "test.png".to_string(),
        is_figma: false,
    })]);

    assert_eq!(api_context.images.len(), 1);
    assert_eq!(api_context.images[0].data, vec![1, 2, 3, 4]);
    assert_eq!(api_context.images[0].mime_type, "image/png");
}

#[test]
fn image_context_skips_invalid_base64() {
    let api_context = super::convert_context(&[AIAgentContext::Image(ImageContext {
        data: "not base64".to_string(),
        mime_type: "image/png".to_string(),
        file_name: "test.png".to_string(),
        is_figma: false,
    })]);

    assert_eq!(api_context.images, vec![]);
}

#[test]
fn git_context_converts_repository_and_pull_request_metadata() {
    let context = vec![
        AIAgentContext::Git {
            head: "abc123".to_string(),
            branch: Some("feature/repo-pr".to_string()),
        },
        AIAgentContext::Repository {
            name: "warp-internal".to_string(),
            owner: Some("warpdotdev".to_string()),
        },
        AIAgentContext::PullRequest {
            number: 42,
            state: "OPEN".to_string(),
            draft: true,
            base_branch: "main".to_string(),
        },
    ];

    let api_context = super::convert_context(&context);
    let git = api_context.git.expect("expected git context");
    assert_eq!(git.head, "abc123");
    assert_eq!(git.branch, "feature/repo-pr");

    let repository = git.repository.expect("expected repository context");
    assert_eq!(repository.name, "warp-internal");
    assert_eq!(repository.owner, "warpdotdev");

    let pull_request = git.pull_request.expect("expected pull request context");
    assert_eq!(pull_request.number, 42);
    assert_eq!(
        pull_request.state,
        api::input_context::git::pull_request::State::OpenDraft as i32
    );
    assert_eq!(pull_request.base_branch, "main");
}

#[test]
fn git_context_skips_pull_request_metadata_with_invalid_number() {
    for number in [0, -1] {
        let context = vec![
            AIAgentContext::Git {
                head: "abc123".to_string(),
                branch: Some("feature/repo-pr".to_string()),
            },
            AIAgentContext::PullRequest {
                number,
                state: "OPEN".to_string(),
                draft: false,
                base_branch: "main".to_string(),
            },
        ];

        let api_context = super::convert_context(&context);
        let git = api_context.git.expect("expected git context");
        assert_eq!(git.head, "abc123");
        assert_eq!(git.branch, "feature/repo-pr");
        assert_eq!(git.pull_request, None);
    }
}

#[test]
fn git_context_skips_pull_request_metadata_with_unknown_state() {
    let context = vec![
        AIAgentContext::Git {
            head: "abc123".to_string(),
            branch: Some("feature/repo-pr".to_string()),
        },
        AIAgentContext::PullRequest {
            number: 42,
            state: "SOMETHING_ELSE".to_string(),
            draft: false,
            base_branch: "main".to_string(),
        },
    ];

    let api_context = super::convert_context(&context);
    let git = api_context.git.expect("expected git context");
    assert_eq!(git.pull_request, None);
}

#[test]
fn git_context_deserializes_legacy_string_pull_request_number() {
    let pull_request = serde_json::from_str::<AIAgentContext>(
        r#"{"PullRequest":{"number":"42","state":"OPEN","draft":false,"base_branch":"main"}}"#,
    )
    .expect("expected legacy serialized pull request context");

    let api_context = super::convert_context(&[pull_request]);
    let pull_request = api_context
        .git
        .expect("expected git context")
        .pull_request
        .expect("expected pull request context");
    assert_eq!(pull_request.number, 42);
}

#[test]
fn command_completion_assessment_converts_to_primary_cli_query() {
    let block_id = BlockId::from("completed-block".to_string());
    let converted = super::convert_input(vec![AIAgentInput::CommandCompletionAssessment {
        prompt: "Summarize whether the command succeeded.".to_string(),
        context: Arc::from([AIAgentContext::SelectedText("root context".to_string())]),
        completed_command: RunningCommand {
            command: "cargo test -p galaxy".to_string(),
            block_id: block_id.clone(),
            grid_contents: "test result: ok".to_string(),
            cursor: "cursor".to_string(),
            requested_command_id: Some("run-call".to_string().into()),
            is_alt_screen_active: true,
        },
    }])
    .unwrap();

    let Some(api::request::input::Type::UserInputs(inputs)) = converted.r#type else {
        panic!("expected user inputs");
    };
    let Some(api::request::input::user_inputs::user_input::Input::CliAgentUserQuery(query)) =
        inputs.inputs[0].input.as_ref()
    else {
        panic!("expected CLI agent query");
    };
    let user_query = query
        .user_query
        .as_ref()
        .expect("expected assessment prompt");
    assert_eq!(user_query.query, "Summarize whether the command succeeded.");
    assert_eq!(user_query.intended_agent(), api::AgentType::Primary);
    let command = query
        .running_command
        .as_ref()
        .expect("expected completed command");
    assert_eq!(command.command, "cargo test -p galaxy");
    let snapshot = command.snapshot.as_ref().expect("expected final snapshot");
    assert_eq!(snapshot.command_id, block_id.as_str());
    assert_eq!(snapshot.output, "test result: ok");
    assert_eq!(snapshot.cursor, "cursor");
    assert!(snapshot.is_alt_screen_active);
    assert_eq!(query.run_shell_command_tool_call_id, "run-call");

    let active = super::convert_input(vec![AIAgentInput::UserQuery {
        query: "Keep monitoring.".to_string(),
        context: Arc::from([]),
        static_query_type: None,
        referenced_attachments: Default::default(),
        user_query_mode: UserQueryMode::Normal,
        running_command: Some(RunningCommand {
            command: "cargo test -p galaxy".to_string(),
            block_id,
            grid_contents: "still running".to_string(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        }),
        intended_agent: None,
    }])
    .unwrap();
    let Some(api::request::input::Type::UserInputs(inputs)) = active.r#type else {
        panic!("expected active user inputs");
    };
    let Some(api::request::input::user_inputs::user_input::Input::CliAgentUserQuery(query)) =
        inputs.inputs[0].input.as_ref()
    else {
        panic!("expected active CLI agent query");
    };
    assert_eq!(
        query
            .user_query
            .as_ref()
            .expect("expected active monitor prompt")
            .intended_agent(),
        api::AgentType::Cli
    );
}

#[test]
fn transfer_control_snapshot_result_converts_to_tool_call_result_input() {
    let block_id = BlockId::default();
    let input =
        api::request::input::user_inputs::user_input::Input::try_from(AIAgentActionResult {
            id: "tool_call".to_string().into(),
            task_id: TaskId::new("task".to_string()),
            result: AIAgentActionResultType::TransferShellCommandControlToUser(
                TransferShellCommandControlToUserResult::Snapshot {
                    block_id: block_id.clone(),
                    grid_contents: "snapshot".to_string(),
                    cursor: "<|cursor|>".to_string(),
                    is_alt_screen_active: false,
                    is_preempted: false,
                },
            ),
        })
        .unwrap();

    match input {
        api::request::input::user_inputs::user_input::Input::ToolCallResult(result) => {
            assert_eq!(result.tool_call_id, "tool_call");
            match result.result {
                Some(api::request::input::tool_call_result::Result::TransferShellCommandControlToUser(
                    api_result,
                )) => match api_result.result {
                    Some(
                        api::transfer_shell_command_control_to_user_result::Result::LongRunningCommandSnapshot(snapshot),
                    ) => {
                        assert_eq!(snapshot.command_id, block_id.to_string());
                        assert_eq!(snapshot.output, "snapshot");
                        assert_eq!(snapshot.cursor, "<|cursor|>");
                    }
                    other => panic!("Expected snapshot result, got {other:?}"),
                },
                other => panic!("Expected transfer-control tool call result, got {other:?}"),
            }
        }
        other => panic!("Expected tool-call-result input, got {other:?}"),
    }
}

#[test]
fn transfer_control_finished_result_converts_to_tool_call_result_input() {
    let block_id = BlockId::default();
    let start_ts = DateTime::from(Utc::now());
    let completed_ts = DateTime::from(Utc::now());
    let input =
        api::request::input::user_inputs::user_input::Input::try_from(AIAgentActionResult {
            id: "tool_call".to_string().into(),
            task_id: TaskId::new("task".to_string()),
            result: AIAgentActionResultType::TransferShellCommandControlToUser(
                TransferShellCommandControlToUserResult::CommandFinished {
                    block_id: block_id.clone(),
                    output: "done".to_string(),
                    exit_code: ExitCode::from(17),
                    start_ts: Some(start_ts),
                    completed_ts: Some(completed_ts),
                },
            ),
        })
        .unwrap();

    match input {
        api::request::input::user_inputs::user_input::Input::ToolCallResult(result) => {
            assert_eq!(result.tool_call_id, "tool_call");
            match result.result {
                Some(api::request::input::tool_call_result::Result::TransferShellCommandControlToUser(
                    api_result,
                )) => match api_result.result {
                    Some(
                        api::transfer_shell_command_control_to_user_result::Result::CommandFinished(finished),
                    ) => {
                        assert_eq!(finished.command_id, block_id.to_string());
                        assert_eq!(finished.output, "done");
                        assert_eq!(finished.exit_code, 17);
                        assert_eq!(finished.start_ts, Some(super::local_datetime_to_timestamp(start_ts)));
                        assert_eq!(finished.finish_ts, Some(super::local_datetime_to_timestamp(completed_ts)));
                    }
                    other => panic!("Expected command-finished result, got {other:?}"),
                },
                other => panic!("Expected transfer-control tool call result, got {other:?}"),
            }
        }
        other => panic!("Expected tool-call-result input, got {other:?}"),
    }
}
