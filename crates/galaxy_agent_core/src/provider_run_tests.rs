use serde_json::json;

use super::*;
use crate::{AgentErrorKind, PermissionKind};

fn initial_messages() -> Vec<ConversationMessage> {
    vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Finish the task".to_string()),
    }]
}

fn run_with_limits(limits: ProviderRunLimits) -> ProviderRun {
    ProviderRun::new("run-1", initial_messages(), "base", limits)
}

fn run() -> ProviderRun {
    run_with_limits(ProviderRunLimits::default())
}

fn next_model_call(run: &mut ProviderRun) -> ProviderModelCall {
    let Some(ProviderRunStep::CallModel(call)) = run.next_step().unwrap() else {
        panic!("expected model call");
    };
    call
}

fn text_turn(text: &str) -> CompletedModelTurn {
    CompletedModelTurn {
        assistant_content: vec![ContentPart::Text(text.to_string())],
        tool_calls: Vec::new(),
        usage: Usage {
            input_tokens: 10,
            output_tokens: 3,
            ..Usage::default()
        },
        stop_reason: StopReason::Completed,
        advertised_tools: BTreeSet::new(),
    }
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: json!({"id": id}),
    }
}

fn tool_turn(calls: Vec<ToolCall>, advertised_tools: &[&str]) -> CompletedModelTurn {
    CompletedModelTurn {
        assistant_content: vec![ContentPart::Text("I will use tools.".to_string())],
        tool_calls: calls,
        usage: Usage {
            input_tokens: 20,
            output_tokens: 5,
            cached_input_tokens: 4,
            ..Usage::default()
        },
        stop_reason: StopReason::Completed,
        advertised_tools: advertised_tools
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    }
}

fn accept_tool_turn(run: &mut ProviderRun, turn: CompletedModelTurn) -> PendingToolBatch {
    let model_call = next_model_call(run);
    run.accept_model_turn(&model_call.work_id, turn).unwrap();
    let Some(ProviderRunStep::DispatchTools(batch)) = run.next_step().unwrap() else {
        panic!("expected tool dispatch");
    };
    batch
}

fn successful_result(call_id: &str, content: &str) -> ToolResult {
    ToolResult {
        call_id: call_id.to_string(),
        content: content.to_string(),
        status: ToolResultStatus::Success,
    }
}

fn assert_serialization_round_trip(run: &ProviderRun) {
    let json = serde_json::to_string(run).unwrap();
    let restored: ProviderRun = serde_json::from_str(&json).unwrap();
    assert_eq!(&restored, run);
}

#[test]
fn next_step_reemits_identical_pending_model_work() {
    let mut run = run();

    let first = run.next_step().unwrap();
    let second = run.next_step().unwrap();

    assert_eq!(first, second);
    assert_eq!(run.epoch(), RunEpoch::new(0));
    assert_serialization_round_trip(&run);
}

#[test]
fn stale_model_completion_is_rejected_without_mutation() {
    let mut run = run();
    let call = next_model_call(&mut run);
    let stale = ExternalWorkId {
        run_id: call.work_id.run_id.clone(),
        epoch: RunEpoch::new(call.work_id.epoch.get() + 1),
    };
    let before = run.clone();

    let error = run
        .accept_model_turn(&stale, text_turn("done"))
        .unwrap_err();

    assert!(matches!(
        error,
        ProviderRunProtocolError::WorkMismatch { .. }
    ));
    assert_eq!(run, before);
}

#[test]
fn model_retries_reuse_work_identity_and_stop_at_the_budget() {
    let mut run = run_with_limits(ProviderRunLimits {
        max_model_turns: 5,
        max_model_retries_per_turn: 1,
    });
    let call = next_model_call(&mut run);
    let mut recoverable = AgentError::new(AgentErrorKind::Transport, "network failed");
    recoverable.recoverable = true;

    assert_eq!(
        run.register_model_failure(&call.work_id, recoverable.clone())
            .unwrap(),
        ModelFailureDisposition::RetryScheduled
    );
    let retry = next_model_call(&mut run);
    assert_eq!(retry.work_id, call.work_id);
    assert_eq!(retry.retry_attempt, 1);
    assert_eq!(retry.messages, call.messages);
    assert_eq!(run.model_retries(), 1);

    assert_eq!(
        run.register_model_failure(&retry.work_id, recoverable)
            .unwrap(),
        ModelFailureDisposition::RunFailed
    );
    let Some(ProviderRunStep::Done(ProviderRunOutcome::Failed(failure))) = run.next_step().unwrap()
    else {
        panic!("expected failed run");
    };
    assert_eq!(failure.kind, ProviderRunFailureKind::RetryLimitExceeded);
}

#[test]
fn text_only_turn_requires_an_explicit_driver_decision() {
    let mut run = run();
    let call = next_model_call(&mut run);
    run.accept_model_turn(&call.work_id, text_turn("finished"))
        .unwrap();

    assert_eq!(run.next_step().unwrap(), None);
    let driver_work = run.active_work_id().unwrap().clone();
    assert_eq!(driver_work.epoch, RunEpoch::new(1));
    assert!(!run.is_terminal());

    run.complete(&driver_work).unwrap();
    assert_eq!(
        run.next_step().unwrap(),
        Some(ProviderRunStep::Done(ProviderRunOutcome::Completed(
            ProviderRunCompletion {
                stop_reason: StopReason::Completed,
            }
        )))
    );
}

#[test]
fn driver_continuation_appends_observation_switches_profile_and_advances_epoch() {
    let mut run = run();
    let call = next_model_call(&mut run);
    run.accept_model_turn(&call.work_id, text_turn("command is still running"))
        .unwrap();
    assert_eq!(run.next_step().unwrap(), None);
    let driver_work = run.active_work_id().unwrap().clone();

    run.continue_with_observation(
        &driver_work,
        MessageContent::Text("command exited with code 1".to_string()),
        "cli-monitor",
    )
    .unwrap();

    assert_eq!(run.epoch(), RunEpoch::new(2));
    assert_eq!(run.profile().as_str(), "cli-monitor");
    let next = next_model_call(&mut run);
    assert_eq!(next.work_id.epoch, RunEpoch::new(2));
    assert_eq!(
        next.messages.last(),
        Some(&ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("command exited with code 1".to_string()),
        })
    );
}

#[test]
fn ready_continuation_appends_observation_switches_profile_and_rejects_reuse() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![tool_call("shell", "run_shell_command")],
            &["run_shell_command"],
        ),
    );
    run.complete_tool(
        &batch.work_id,
        successful_result("shell", "command is still running"),
    )
    .unwrap();
    run.commit_tool_batch(&batch.work_id).unwrap();
    let ready_work = run.ready_work_id().expect("ready work identity");

    run.continue_ready_with_observation(
        &ready_work,
        MessageContent::Text("Monitor command block-1.".to_string()),
        "cli-monitor",
    )
    .unwrap();

    assert_eq!(run.epoch(), RunEpoch::new(3));
    assert_eq!(run.profile().as_str(), "cli-monitor");
    assert!(matches!(
        run.transcript().last(),
        Some(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(text),
        }) if text == "Monitor command block-1."
    ));
    let continued = run.clone();
    assert!(matches!(
        run.continue_ready_with_observation(
            &ready_work,
            MessageContent::Text("duplicate".to_string()),
            "cli-monitor",
        )
        .unwrap_err(),
        ProviderRunProtocolError::WorkMismatch { .. }
    ));
    assert_eq!(run, continued);
}

#[test]
fn unknown_malformed_and_inline_tools_are_pre_resolved_in_the_same_batch() {
    let mut run = ProviderRun::new(
        "run-1",
        vec![
            ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::ToolUse {
                    tool_use_id: "old-read".to_string(),
                    name: "read_files".to_string(),
                    input: json!({"path": "old.txt"}),
                },
            },
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::ToolResult {
                    tool_use_id: "old-read".to_string(),
                    content: "old contents".to_string(),
                    is_error: false,
                },
            },
        ],
        "base",
        ProviderRunLimits::default(),
    );
    let mut malformed = tool_call("malformed", "read_files");
    malformed.arguments = json!("not an object");
    let recall = ToolCall {
        id: "recall".to_string(),
        name: crate::RECALL_TOOL_HISTORY_NAME.to_string(),
        arguments: json!({"tool_use_id": "old-read"}),
    };
    let turn = tool_turn(
        vec![
            tool_call("external", "read_files"),
            tool_call("unknown", "invented_tool"),
            malformed,
            recall,
        ],
        &["read_files", crate::RECALL_TOOL_HISTORY_NAME],
    );

    let batch = accept_tool_turn(&mut run, turn);

    assert!(matches!(
        batch.calls[0].state,
        PendingToolCallState::Proposed
    ));
    for index in [1, 2, 3] {
        assert!(matches!(
            batch.calls[index].state,
            PendingToolCallState::Resolved { .. }
        ));
    }
    let recall_result = batch.calls[3].state.result().unwrap();
    assert!(recall_result.content.contains("old contents"));
    assert_eq!(batch.unresolved_call_ids(), vec!["external"]);
}

#[test]
fn parallel_tool_results_commit_atomically_in_original_call_order() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![
                tool_call("first", "read_files"),
                tool_call("second", "grep"),
            ],
            &["read_files", "grep"],
        ),
    );

    run.start_tool(&batch.work_id, "first").unwrap();
    run.start_tool(&batch.work_id, "second").unwrap();
    run.complete_tool(&batch.work_id, successful_result("second", "second result"))
        .unwrap();
    run.complete_tool(&batch.work_id, successful_result("first", "first result"))
        .unwrap();

    assert_eq!(run.transcript().len(), 2);
    let Some(ProviderRunStep::DispatchTools(completed)) = run.next_step().unwrap() else {
        panic!("completed batch must remain recoverable until commit");
    };
    assert!(completed.is_complete());
    assert_eq!(run.epoch(), RunEpoch::new(1));
    run.commit_tool_batch(&batch.work_id).unwrap();
    assert_eq!(run.epoch(), RunEpoch::new(2));

    let MessageContent::MultiPart(parts) = &run.transcript().last().unwrap().content else {
        panic!("expected atomic multi-part result message");
    };
    let ids = parts
        .iter()
        .map(|part| match part {
            ContentPart::ToolResult { tool_use_id, .. } => tool_use_id.as_str(),
            ContentPart::Text(_)
            | ContentPart::Reasoning { .. }
            | ContentPart::Image { .. }
            | ContentPart::ToolUse { .. } => panic!("expected only tool results"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["first", "second"]);
}

#[test]
fn exact_batch_submission_rejects_missing_duplicate_and_unknown_results_without_mutation() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![
                tool_call("first", "read_files"),
                tool_call("second", "grep"),
            ],
            &["read_files", "grep"],
        ),
    );
    let before = run.clone();

    let missing = run
        .complete_tool_batch(&batch.work_id, vec![successful_result("first", "one")])
        .unwrap_err();
    assert!(matches!(
        missing,
        ProviderRunProtocolError::ToolResultSetMismatch { .. }
    ));
    assert_eq!(run, before);

    let duplicate = run
        .complete_tool_batch(
            &batch.work_id,
            vec![
                successful_result("first", "one"),
                successful_result("first", "again"),
            ],
        )
        .unwrap_err();
    assert_eq!(
        duplicate,
        ProviderRunProtocolError::DuplicateToolResult {
            call_id: "first".to_string(),
        }
    );
    assert_eq!(run, before);

    let unknown = run
        .complete_tool_batch(
            &batch.work_id,
            vec![
                successful_result("first", "one"),
                successful_result("unknown", "bad"),
            ],
        )
        .unwrap_err();
    assert!(matches!(
        unknown,
        ProviderRunProtocolError::ToolResultSetMismatch { .. }
    ));
    assert_eq!(run, before);
}

#[test]
fn permission_denial_becomes_one_correlated_result() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![tool_call("shell", "run_shell_command")],
            &["run_shell_command"],
        ),
    );
    let request = PermissionRequest {
        id: "permission-shell".to_string(),
        call_id: "shell".to_string(),
        kind: PermissionKind::Execute,
        reason: Some("run a command".to_string()),
    };

    run.request_tool_permission(&batch.work_id, request)
        .unwrap();
    let wrong_request = run
        .resolve_tool_permission(
            &batch.work_id,
            "shell",
            "wrong",
            PermissionDecision::AllowOnce,
        )
        .unwrap_err();
    assert!(matches!(
        wrong_request,
        ProviderRunProtocolError::PermissionRequestMismatch { .. }
    ));
    run.resolve_tool_permission(
        &batch.work_id,
        "shell",
        "permission-shell",
        PermissionDecision::Denied {
            reason: Some("not allowed".to_string()),
        },
    )
    .unwrap();

    let Some(ProviderRunStep::DispatchTools(completed)) = run.next_step().unwrap() else {
        panic!("expected completed tool batch");
    };
    let result = completed.calls[0].state.result().unwrap();
    assert_eq!(result.status, ToolResultStatus::Denied);
    assert_eq!(result.content, "not allowed");
    assert!(completed.is_complete());
}

#[test]
fn stale_unknown_and_duplicate_tool_updates_do_not_mutate_the_batch() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(vec![tool_call("read", "read_files")], &["read_files"]),
    );
    let stale = ExternalWorkId {
        run_id: batch.work_id.run_id.clone(),
        epoch: RunEpoch::new(batch.work_id.epoch.get() + 1),
    };
    let before = run.clone();
    assert!(matches!(
        run.start_tool(&stale, "read").unwrap_err(),
        ProviderRunProtocolError::WorkMismatch { .. }
    ));
    assert_eq!(run, before);
    assert_eq!(
        run.start_tool(&batch.work_id, "missing").unwrap_err(),
        ProviderRunProtocolError::UnknownToolCall {
            call_id: "missing".to_string(),
        }
    );
    assert_eq!(run, before);

    run.complete_tool(&batch.work_id, successful_result("read", "ok"))
        .unwrap();
    let completed = run.clone();
    assert_eq!(
        run.complete_tool(&batch.work_id, successful_result("read", "again"))
            .unwrap_err(),
        ProviderRunProtocolError::DuplicateToolUpdate {
            call_id: "read".to_string(),
        }
    );
    assert_eq!(run, completed);
}

#[test]
fn run_cancellation_preserves_completed_results_and_synthesizes_the_rest() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![tool_call("done", "read_files"), tool_call("active", "grep")],
            &["read_files", "grep"],
        ),
    );
    run.complete_tool(&batch.work_id, successful_result("done", "contents"))
        .unwrap();
    run.start_tool(&batch.work_id, "active").unwrap();

    run.cancel("user cancelled").unwrap();

    assert_eq!(run.epoch(), RunEpoch::new(2));
    let MessageContent::MultiPart(parts) = &run.transcript().last().unwrap().content else {
        panic!("expected tool results");
    };
    assert!(matches!(
        &parts[0],
        ContentPart::ToolResult {
            tool_use_id,
            content,
            is_error: false,
        } if tool_use_id == "done" && content == "contents"
    ));
    assert!(matches!(
        &parts[1],
        ContentPart::ToolResult {
            tool_use_id,
            content,
            is_error: true,
        } if tool_use_id == "active" && content == "user cancelled"
    ));
    assert_eq!(
        run.next_step().unwrap(),
        Some(ProviderRunStep::Done(ProviderRunOutcome::Cancelled {
            reason: "user cancelled".to_string(),
        }))
    );
}

#[test]
fn failure_while_tools_are_pending_records_correlated_errors_before_terminal_state() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(vec![tool_call("project", "read_files")], &["read_files"]),
    );
    assert_eq!(batch.work_id.epoch, RunEpoch::new(1));

    run.fail(
        ProviderRunFailureKind::Projection,
        "proposal could not be projected",
    )
    .unwrap();

    assert_eq!(run.epoch(), RunEpoch::new(2));
    let MessageContent::MultiPart(parts) = &run.transcript().last().unwrap().content else {
        panic!("expected synthesized tool result");
    };
    assert!(matches!(
        &parts[0],
        ContentPart::ToolResult {
            tool_use_id,
            is_error: true,
            ..
        } if tool_use_id == "project"
    ));
    let Some(ProviderRunStep::Done(ProviderRunOutcome::Failed(failure))) = run.next_step().unwrap()
    else {
        panic!("expected failed run");
    };
    assert_eq!(failure.kind, ProviderRunFailureKind::Projection);
}

#[test]
fn turn_limit_cannot_finish_successfully_after_tools_require_another_model_turn() {
    let mut run = run_with_limits(ProviderRunLimits {
        max_model_turns: 1,
        max_model_retries_per_turn: 0,
    });
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(vec![tool_call("read", "read_files")], &["read_files"]),
    );
    run.complete_tool(&batch.work_id, successful_result("read", "ok"))
        .unwrap();
    run.commit_tool_batch(&batch.work_id).unwrap();

    let Some(ProviderRunStep::Done(ProviderRunOutcome::Failed(failure))) = run.next_step().unwrap()
    else {
        panic!("expected turn-limit failure");
    };
    assert_eq!(failure.kind, ProviderRunFailureKind::TurnLimitExceeded);
}

#[test]
fn invalid_model_turn_does_not_commit_partial_content_usage_or_epoch() {
    let mut run = run();
    let call = next_model_call(&mut run);
    let duplicate = tool_call("duplicate", "read_files");
    let turn = tool_turn(vec![duplicate.clone(), duplicate], &["read_files"]);
    let before = run.clone();

    assert!(matches!(
        run.accept_model_turn(&call.work_id, turn).unwrap_err(),
        ProviderRunProtocolError::InvalidModelTurn { .. }
    ));
    assert_eq!(run, before);
}

#[test]
fn every_nonterminal_phase_round_trips_through_json() {
    let mut ready = run();
    assert_serialization_round_trip(&ready);

    let call = next_model_call(&mut ready);
    assert_serialization_round_trip(&ready);

    let mut resolving_tools = ready.clone();
    resolving_tools
        .accept_model_turn(
            &call.work_id,
            tool_turn(vec![tool_call("read", "read_files")], &["read_files"]),
        )
        .unwrap();
    assert_eq!(
        resolving_tools.state().phase(),
        ProviderRunPhase::ResolvingModel
    );
    assert_serialization_round_trip(&resolving_tools);

    let Some(ProviderRunStep::DispatchTools(_)) = resolving_tools.next_step().unwrap() else {
        panic!("expected tool phase");
    };
    assert_serialization_round_trip(&resolving_tools);

    let mut resolving_text = ready;
    resolving_text
        .accept_model_turn(&call.work_id, text_turn("done"))
        .unwrap();
    assert_serialization_round_trip(&resolving_text);
    assert_eq!(resolving_text.next_step().unwrap(), None);
    assert_eq!(
        resolving_text.state().phase(),
        ProviderRunPhase::AwaitingDriver
    );
    assert_serialization_round_trip(&resolving_text);
}

#[test]
fn restore_normalization_preserves_safe_nonterminal_states() {
    let ready = run();
    let mut awaiting_model = ready.clone();
    let call = next_model_call(&mut awaiting_model);

    let mut resolving = awaiting_model.clone();
    resolving
        .accept_model_turn(&call.work_id, text_turn("done"))
        .unwrap();
    let mut awaiting_driver = resolving.clone();
    assert_eq!(awaiting_driver.next_step().unwrap(), None);

    for mut candidate in [ready, awaiting_model, resolving, awaiting_driver] {
        let before = candidate.clone();
        assert_eq!(
            candidate.normalize_after_restore().unwrap(),
            ProviderRunRestoreNormalization::default()
        );
        assert_eq!(candidate, before);
    }
}

#[test]
fn restore_normalization_reproposes_permissions_and_interrupts_unsafe_tools() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![
                tool_call("proposed", "read_files"),
                tool_call("permission", "grep"),
                tool_call("approved", "run_shell_command"),
                tool_call("executing", "read_files"),
                tool_call("resolved", "grep"),
            ],
            &["read_files", "grep", "run_shell_command"],
        ),
    );
    let permission = PermissionRequest {
        id: "permission-1".to_string(),
        call_id: "permission".to_string(),
        kind: PermissionKind::Read,
        reason: None,
    };
    run.request_tool_permission(&batch.work_id, permission)
        .unwrap();
    let approved = PermissionRequest {
        id: "permission-2".to_string(),
        call_id: "approved".to_string(),
        kind: PermissionKind::Execute,
        reason: None,
    };
    run.request_tool_permission(&batch.work_id, approved)
        .unwrap();
    run.resolve_tool_permission(
        &batch.work_id,
        "approved",
        "permission-2",
        PermissionDecision::AllowOnce,
    )
    .unwrap();
    run.start_tool(&batch.work_id, "executing").unwrap();
    run.complete_tool(
        &batch.work_id,
        successful_result("resolved", "already finished"),
    )
    .unwrap();

    let normalization = run.normalize_after_restore().unwrap();

    assert_eq!(
        normalization.permission_call_ids_reset,
        vec!["permission".to_string()]
    );
    assert_eq!(
        normalization.interrupted_call_ids,
        vec!["approved".to_string(), "executing".to_string()]
    );
    assert!(!normalization.committed_tool_batch);
    let ProviderRunState::AwaitingTools { batch } = run.state() else {
        panic!("partially resolved batch must remain pending");
    };
    assert!(matches!(
        batch.calls[0].state,
        PendingToolCallState::Proposed
    ));
    assert!(matches!(
        batch.calls[1].state,
        PendingToolCallState::Proposed
    ));
    for index in [2, 3] {
        let result = batch.calls[index].state.result().unwrap();
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(result.content.contains("was not replayed"));
    }
    assert_eq!(
        batch.calls[4].state.result(),
        Some(&successful_result("resolved", "already finished"))
    );
}

#[test]
fn restore_normalization_preserves_selected_executing_tools_for_recovery() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![
                tool_call("run-agents", "run_agents"),
                tool_call("read", "read_files"),
            ],
            &["run_agents", "read_files"],
        ),
    );
    run.start_tool(&batch.work_id, "run-agents").unwrap();
    run.start_tool(&batch.work_id, "read").unwrap();

    let normalization = run
        .normalize_after_restore_with_recoverable_calls(&HashSet::from(["run-agents".to_string()]))
        .unwrap();

    assert_eq!(normalization.recovery_call_ids, vec!["run-agents"]);
    assert_eq!(normalization.interrupted_call_ids, vec!["read"]);
    let ProviderRunState::AwaitingTools { batch } = run.state() else {
        panic!("recovered tool batch should remain pending");
    };
    assert!(matches!(
        batch.calls[0].state,
        PendingToolCallState::RecoveryPending
    ));
    assert_eq!(
        batch.calls[1].state.result().unwrap().status,
        ToolResultStatus::Error
    );
}

#[test]
fn recovery_pending_tool_survives_another_restore_and_completes_once() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(vec![tool_call("run-agents", "run_agents")], &["run_agents"]),
    );
    run.start_tool(&batch.work_id, "run-agents").unwrap();
    run.normalize_after_restore_with_recoverable_calls(&HashSet::from(["run-agents".to_string()]))
        .unwrap();
    let serialized = serde_json::to_string(&run).unwrap();
    let mut restored: ProviderRun = serde_json::from_str(&serialized).unwrap();

    let normalization = restored.normalize_after_restore().unwrap();
    assert_eq!(normalization.recovery_call_ids, vec!["run-agents"]);
    restored
        .complete_tool(
            &batch.work_id,
            successful_result("run-agents", "children completed"),
        )
        .unwrap();
    assert!(matches!(
        restored.complete_tool(
            &batch.work_id,
            successful_result("run-agents", "duplicate completion"),
        ),
        Err(ProviderRunProtocolError::DuplicateToolUpdate { .. })
    ));
    restored.commit_tool_batch(&batch.work_id).unwrap();
    assert_eq!(restored.state().phase(), ProviderRunPhase::ReadyToCallModel);
}

#[test]
fn restore_normalization_commits_a_fully_resolved_batch() {
    let mut run = run();
    let batch = accept_tool_turn(
        &mut run,
        tool_turn(
            vec![
                tool_call("executing", "read_files"),
                tool_call("resolved", "grep"),
            ],
            &["read_files", "grep"],
        ),
    );
    run.start_tool(&batch.work_id, "executing").unwrap();
    run.complete_tool(
        &batch.work_id,
        successful_result("resolved", "already finished"),
    )
    .unwrap();

    let normalization = run.normalize_after_restore().unwrap();

    assert!(normalization.committed_tool_batch);
    assert_eq!(
        normalization.interrupted_call_ids,
        vec!["executing".to_string()]
    );
    assert_eq!(run.state().phase(), ProviderRunPhase::ReadyToCallModel);
    assert_eq!(run.epoch(), RunEpoch::new(2));
    let next = next_model_call(&mut run);
    assert_eq!(next.work_id.epoch, RunEpoch::new(2));
    let MessageContent::MultiPart(parts) = &next.messages.last().unwrap().content else {
        panic!("expected committed tool results");
    };
    assert!(matches!(
        &parts[0],
        ContentPart::ToolResult {
            tool_use_id,
            is_error: true,
            ..
        } if tool_use_id == "executing"
    ));
    assert!(matches!(
        &parts[1],
        ContentPart::ToolResult {
            tool_use_id,
            is_error: false,
            ..
        } if tool_use_id == "resolved"
    ));
}
