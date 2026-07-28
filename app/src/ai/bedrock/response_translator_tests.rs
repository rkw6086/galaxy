use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api};

use super::response_translator::*;

fn tool_from_event(event: api::ResponseEvent) -> api::message::tool_call::Tool {
    let Some(api::response_event::Type::ClientActions(actions)) = event.r#type else {
        panic!("expected client actions");
    };
    let Some(api::client_action::Action::AddMessagesToTask(add_messages)) =
        &actions.actions[0].action
    else {
        panic!("expected AddMessagesToTask");
    };
    let Some(api::message::Message::ToolCall(tool_call)) = &add_messages.messages[0].message else {
        panic!("expected tool call message");
    };
    tool_call.tool.clone().expect("expected concrete tool")
}

#[test]
fn test_build_stream_init_has_valid_ids() {
    let event = build_stream_init("req-123", "conv-456");

    match event.r#type {
        Some(api::response_event::Type::Init(init)) => {
            assert_eq!(init.request_id, "req-123");
            assert_eq!(init.conversation_id, "conv-456");
            assert_eq!(init.run_id, "");
        }
        other => panic!("Expected Init event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_done_reason() {
    let reason = stream_finished::Reason::Done(stream_finished::Done {});
    let event = build_stream_finished(
        reason,
        100,
        50,
        20,
        10,
        "anthropic.claude-sonnet-4-6",
        false,
    );

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::Done(_))
            ));
            assert!(!finished.should_refresh_model_config);
            let metadata = finished.conversation_usage_metadata.unwrap();
            assert_eq!(
                metadata
                    .byok_token_usage
                    .get("bedrock")
                    .unwrap()
                    .total_tokens,
                180
            );
            // Verify token_usage includes cache breakdown
            assert_eq!(finished.token_usage.len(), 1);
            let usage = &finished.token_usage[0];
            assert_eq!(usage.total_input, 100);
            assert_eq!(usage.output, 50);
            assert_eq!(usage.input_cache_read, 20);
            assert_eq!(usage.input_cache_write, 10);
            assert!(usage.cost_in_cents > 0.0);
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_max_token_limit() {
    let reason = stream_finished::Reason::MaxTokenLimit(stream_finished::ReachedMaxTokenLimit {});
    let event = build_stream_finished(reason, 200, 100, 0, 0, "anthropic.claude-sonnet-4-6", false);

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::MaxTokenLimit(_))
            ));
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_other_reason() {
    let reason = stream_finished::Reason::Other(stream_finished::Other {});
    let event = build_stream_finished(reason, 0, 0, 0, 0, "anthropic.claude-sonnet-4-6", false);

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::Other(_))
            ));
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}

#[test]
fn test_build_create_task_event() {
    let event = super::response_translator::build_create_task("task-abc-123");

    match event.r#type {
        Some(api::response_event::Type::ClientActions(actions)) => {
            assert_eq!(actions.actions.len(), 1);
            match &actions.actions[0].action {
                Some(api::client_action::Action::CreateTask(create)) => {
                    let task = create.task.as_ref().unwrap();
                    assert_eq!(task.id, "task-abc-123");
                    assert!(task.messages.is_empty());
                    assert!(task.description.is_empty());
                    assert!(task.dependencies.is_none());
                }
                other => panic!("Expected CreateTask action, got {:?}", other),
            }
        }
        other => panic!("Expected ClientActions event, got {:?}", other),
    }
}

#[test]
fn test_build_create_task_has_no_parent() {
    let event = super::response_translator::build_create_task("root-task-id");

    if let Some(api::response_event::Type::ClientActions(actions)) = event.r#type {
        if let Some(api::client_action::Action::CreateTask(create)) = &actions.actions[0].action {
            let task = create.task.as_ref().unwrap();
            assert!(
                task.dependencies.is_none(),
                "Root task CreateTask must have no dependencies (no parent_id)"
            );
        }
    }
}

#[test]
fn shell_tool_call_defaults_to_async_and_preserves_controls() {
    let tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-1",
        "run_shell_command",
        r#"{
            "command": "cargo test",
            "is_read_only": true,
            "is_risky": true,
            "uses_pager": false
        }"#,
    ));

    let api::message::tool_call::Tool::RunShellCommand(command) = tool else {
        panic!("expected run_shell_command");
    };
    assert!(command.is_read_only);
    assert!(command.is_risky);
    assert!(!command.uses_pager);
    assert!(matches!(
        command.wait_until_complete_value,
        Some(
            api::message::tool_call::run_shell_command::WaitUntilCompleteValue::WaitUntilComplete(
                false
            )
        )
    ));
}

#[test]
fn long_running_tool_calls_preserve_command_id_and_delay() {
    let write_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-1",
        "write_to_long_running_shell_command",
        r#"{"command_id":"block-123","input":"yes","mode":"line"}"#,
    ));
    let api::message::tool_call::Tool::WriteToLongRunningShellCommand(write) = write_tool else {
        panic!("expected write_to_long_running_shell_command");
    };
    assert_eq!(write.command_id, "block-123");
    assert!(matches!(
        write.mode.and_then(|mode| mode.mode),
        Some(api::message::tool_call::write_to_long_running_shell_command::mode::Mode::Line(()))
    ));

    let read_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-2",
        "read_shell_command_output",
        r#"{"command_id":"block-123","wait_seconds":7}"#,
    ));
    let api::message::tool_call::Tool::ReadShellCommandOutput(read) = read_tool else {
        panic!("expected read_shell_command_output");
    };
    assert_eq!(read.command_id, "block-123");
    assert!(matches!(
        read.delay,
        Some(
            api::message::tool_call::read_shell_command_output::Delay::Duration(
                prost_types::Duration {
                    seconds: 7,
                    nanos: 0
                }
            )
        )
    ));
}

#[test]
fn test_context_window_for_model_1m_marker() {
    assert_eq!(
        context_window_for_model("anthropic.claude-opus-4-6[1m]"),
        1_000_000
    );
    assert_eq!(
        context_window_for_model("us.anthropic.claude-opus-4-6[1M]"),
        1_000_000
    );
    assert_eq!(
        context_window_for_model("anthropic.claude-sonnet-4-6[1m]"),
        1_000_000
    );
}

#[test]
fn test_context_window_for_model_standard_claude() {
    assert_eq!(
        context_window_for_model("anthropic.claude-opus-4-6"),
        200_000
    );
    assert_eq!(
        context_window_for_model("us.anthropic.claude-sonnet-4-6"),
        200_000
    );
    assert_eq!(
        context_window_for_model("anthropic.claude-haiku-4-5-20251001-v1:0"),
        200_000
    );
}

#[test]
fn test_context_window_for_model_nova() {
    assert_eq!(context_window_for_model("amazon.nova-pro-v1:0"), 300_000);
    assert_eq!(context_window_for_model("amazon.nova-lite-v1:0"), 300_000);
    assert_eq!(context_window_for_model("amazon.nova-micro-v1:0"), 300_000);
}

#[test]
fn test_context_window_for_model_deepseek() {
    assert_eq!(context_window_for_model("deepseek.r1-v1:0"), 128_000);
}

#[test]
fn test_cost_varies_by_model() {
    let reason = stream_finished::Reason::Done(stream_finished::Done {});
    let opus_event = build_stream_finished(
        reason.clone(),
        1000,
        1000,
        0,
        0,
        "anthropic.claude-opus-4-6",
        false,
    );
    let reason = stream_finished::Reason::Done(stream_finished::Done {});
    let sonnet_event = build_stream_finished(
        reason.clone(),
        1000,
        1000,
        0,
        0,
        "anthropic.claude-sonnet-4-6",
        false,
    );
    let reason = stream_finished::Reason::Done(stream_finished::Done {});
    let haiku_event = build_stream_finished(
        reason,
        1000,
        1000,
        0,
        0,
        "anthropic.claude-haiku-4-5-20251001-v1:0",
        false,
    );

    let opus_cost = match opus_event.r#type {
        Some(api::response_event::Type::Finished(f)) => f.token_usage[0].cost_in_cents,
        _ => panic!("Expected Finished"),
    };
    let sonnet_cost = match sonnet_event.r#type {
        Some(api::response_event::Type::Finished(f)) => f.token_usage[0].cost_in_cents,
        _ => panic!("Expected Finished"),
    };
    let haiku_cost = match haiku_event.r#type {
        Some(api::response_event::Type::Finished(f)) => f.token_usage[0].cost_in_cents,
        _ => panic!("Expected Finished"),
    };

    assert!(opus_cost > sonnet_cost, "Opus should cost more than Sonnet");
    assert!(
        sonnet_cost > haiku_cost,
        "Sonnet should cost more than Haiku"
    );
    assert!(
        haiku_cost > 0.0,
        "All costs should be positive for non-zero tokens"
    );
}

#[test]
fn test_cost_zero_for_zero_tokens() {
    let reason = stream_finished::Reason::Done(stream_finished::Done {});
    let event = build_stream_finished(reason, 0, 0, 0, 0, "anthropic.claude-sonnet-4-6", false);

    let cost = match event.r#type {
        Some(api::response_event::Type::Finished(f)) => f.token_usage[0].cost_in_cents,
        _ => panic!("Expected Finished"),
    };
    assert_eq!(cost, 0.0);
}
