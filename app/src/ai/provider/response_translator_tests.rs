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

    let bounded_read_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-2b",
        "read_shell_command_output",
        r#"{"command_id":"block-123","wait_seconds":120}"#,
    ));
    let api::message::tool_call::Tool::ReadShellCommandOutput(bounded_read) = bounded_read_tool
    else {
        panic!("expected bounded read_shell_command_output");
    };
    assert!(matches!(
        bounded_read.delay,
        Some(
            api::message::tool_call::read_shell_command_output::Delay::Duration(
                prost_types::Duration {
                    seconds: 10,
                    nanos: 0
                }
            )
        )
    ));

    let interrupt_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-3",
        "interrupt_shell_command",
        r#"{"command_id":"block-123"}"#,
    ));
    let api::message::tool_call::Tool::WriteToLongRunningShellCommand(interrupt) = interrupt_tool
    else {
        panic!("expected interrupt_shell_command to use the write-to-command transport");
    };
    assert_eq!(interrupt.command_id, "block-123");
    assert_eq!(
        interrupt.input,
        vec![galaxy_terminal::model::escape_sequences::C0::ETX]
    );
    assert!(matches!(
        interrupt.mode.and_then(|mode| mode.mode),
        Some(api::message::tool_call::write_to_long_running_shell_command::mode::Mode::Raw(()))
    ));
}

#[test]
fn read_skill_tool_call_preserves_bundled_reference_type() {
    let tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-1",
        "read_skill",
        r#"{"skill":"galaxyctrl","reference_type":"bundled"}"#,
    ));
    let api::message::tool_call::Tool::ReadSkill(read_skill) = tool else {
        panic!("expected read_skill");
    };
    assert!(matches!(
        read_skill.skill_reference,
        Some(
            api::message::tool_call::read_skill::SkillReference::BundledSkillId(ref id)
        ) if id == "galaxyctrl"
    ));
}

#[test]
fn read_skill_tool_call_preserves_path_and_legacy_inputs() {
    for input in [
        r#"{"skill":"/repo/.agents/skills/deploy/SKILL.md","reference_type":"path"}"#,
        r#"{"skill":"/repo/.agents/skills/deploy/SKILL.md"}"#,
    ] {
        let tool = tool_from_event(build_tool_call_message(
            "task-1",
            "tool-1",
            "read_skill",
            input,
        ));
        let api::message::tool_call::Tool::ReadSkill(read_skill) = tool else {
            panic!("expected read_skill");
        };
        assert!(matches!(
            read_skill.skill_reference,
            Some(
                api::message::tool_call::read_skill::SkillReference::SkillPath(ref path)
            ) if path == "/repo/.agents/skills/deploy/SKILL.md"
        ));
    }
}

#[test]
fn development_tool_calls_preserve_focused_reads_file_lifecycle_and_search_filters() {
    let read_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-read",
        "read_files",
        r#"{"files":["/repo/Cargo.toml",{"path":"/repo/src/lib.rs","line_ranges":[{"start":10,"end":25},{"start":0,"end":2}]}]}"#,
    ));
    let api::message::tool_call::Tool::ReadFiles(read_files) = read_tool else {
        panic!("expected read_files");
    };
    assert_eq!(read_files.files.len(), 2);
    assert_eq!(read_files.files[0].name, "/repo/Cargo.toml");
    assert!(read_files.files[0].line_ranges.is_empty());
    assert_eq!(read_files.files[1].name, "/repo/src/lib.rs");
    assert_eq!(
        read_files.files[1].line_ranges,
        vec![api::FileContentLineRange { start: 10, end: 25 }]
    );

    let edit_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-edit",
        "apply_file_diffs",
        r#"{
            "summary":"Update implementation",
            "diffs":[{"file_path":"/repo/src/lib.rs","search":"old","replace":"new"}],
            "new_files":[{"file_path":"/repo/src/new.rs","content":"pub fn new() {}"}],
            "deleted_files":["/repo/src/obsolete.rs"]
        }"#,
    ));
    let api::message::tool_call::Tool::ApplyFileDiffs(edits) = edit_tool else {
        panic!("expected apply_file_diffs");
    };
    assert_eq!(edits.diffs.len(), 1);
    assert_eq!(edits.new_files[0].file_path, "/repo/src/new.rs");
    assert_eq!(edits.new_files[0].content, "pub fn new() {}");
    assert_eq!(edits.deleted_files[0].file_path, "/repo/src/obsolete.rs");

    let search_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-search",
        "search_codebase",
        r#"{"query":"provider routing","path":"/repo","path_filters":["app/src/ai","crates/ai"]}"#,
    ));
    let api::message::tool_call::Tool::SearchCodebase(search) = search_tool else {
        panic!("expected search_codebase");
    };
    assert_eq!(search.codebase_path, "/repo");
    assert_eq!(search.path_filters, vec!["app/src/ai", "crates/ai"]);
}

#[test]
fn orchestration_tool_calls_build_run_agents_and_wait_for_events() {
    let run_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-run-agents",
        "run_agents",
        r#"{
            "summary": "Investigate in parallel",
            "base_prompt": "Shared instructions",
            "model_id": "coding-assistant-max",
            "harness_type": "codex",
            "execution_mode": {
                "type": "local"
            },
            "agent_run_configs": [
                {
                    "name": "code",
                    "prompt": "Inspect code",
                    "title": "Code inspection"
                }
            ],
            "plan_id": "plan-1"
        }"#,
    ));
    let api::message::tool_call::Tool::RunAgents(run_agents) = run_tool else {
        panic!("expected run_agents");
    };
    assert_eq!(run_agents.summary, "Investigate in parallel");
    assert_eq!(run_agents.base_prompt, "Shared instructions");
    assert_eq!(run_agents.model_id, "coding-assistant-max");
    assert!(matches!(
        run_agents.execution_mode,
        Some(api::run_agents::ExecutionMode::Local(_))
    ));
    assert_eq!(run_agents.agent_run_configs.len(), 1);
    assert_eq!(run_agents.agent_run_configs[0].name, "code");
    assert_eq!(run_agents.agent_run_configs[0].prompt, "Inspect code");

    let wait_tool = tool_from_event(build_tool_call_message(
        "task-1",
        "tool-wait",
        "wait_for_events",
        r#"{"idle_timeout_seconds": 120}"#,
    ));
    let api::message::tool_call::Tool::WaitForEvents(wait) = wait_tool else {
        panic!("expected wait_for_events");
    };
    assert_eq!(wait.idle_timeout_seconds, 120);
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

#[test]
fn direct_provider_known_tools_exclude_hosted_only_tools() {
    assert!(!is_known_tool("send_message_to_agent"));
    assert!(!is_known_tool("suggest_next_prompt"));
    assert!(is_known_tool("run_agents"));
    assert!(is_known_tool("wait_for_events"));
    assert!(is_known_tool("recall_tool_history"));
    assert!(is_known_tool("interrupt_shell_command"));
}

#[test]
fn compatibility_grep_literal_mode_uses_the_same_escaped_patterns() {
    let tool = tool_from_event(build_tool_call_message(
        "task-1",
        "grep-1",
        "grep",
        r#"{"queries":["variantResolutions: []","indexAsset(context"],"fixed_strings":true,"path":"/project/src"}"#,
    ));
    let api::message::tool_call::Tool::Grep(grep) = tool else {
        panic!("expected grep tool");
    };
    assert_eq!(
        grep.queries,
        vec![r"variantResolutions: \[\]", r"indexAsset\(context"]
    );
    assert_eq!(grep.path, "/project/src");
}

#[test]
fn compatibility_grep_keeps_default_regex_semantics() {
    let tool = tool_from_event(build_tool_call_message(
        "task-1",
        "grep-1",
        "grep",
        r#"{"queries":["foo|bar","[unclosed"]}"#,
    ));
    let api::message::tool_call::Tool::Grep(grep) = tool else {
        panic!("expected grep tool");
    };
    assert_eq!(grep.queries, vec!["foo|bar", "[unclosed"]);
}
