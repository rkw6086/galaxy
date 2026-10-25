use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionResultType, AIAgentActionType, AnyFileContent, FileContext,
    GrepResult, ReadFilesResult,
};
use crate::test_util::terminal::{add_window_with_terminal, initialize_app_for_terminal_view};

fn make_action_result(id: &str) -> Arc<AIAgentActionResult> {
    Arc::new(AIAgentActionResult {
        id: AIAgentActionId::from(id.to_owned()),
        task_id: TaskId::new("task".to_owned()),
        result: AIAgentActionResultType::InitProject,
    })
}

fn action_result(id: &str, result: AIAgentActionResultType) -> AIAgentActionResult {
    AIAgentActionResult {
        id: AIAgentActionId::from(id.to_owned()),
        task_id: TaskId::new("task".to_owned()),
        result,
    }
}

fn action(id: &str) -> AIAgentAction {
    AIAgentAction {
        id: AIAgentActionId::from(id.to_string()),
        action: AIAgentActionType::InitProject,
        task_id: TaskId::new("task".to_string()),
        requires_result: true,
        tool_name: Some("init_project".to_string()),
    }
}

fn pending_tool_batch(call_ids: &[&str]) -> PendingToolBatch {
    PendingToolBatch {
        work_id: galaxy_agent_core::ExternalWorkId {
            run_id: galaxy_agent_core::ProviderRunId::new("run"),
            epoch: galaxy_agent_core::RunEpoch::new(7),
        },
        calls: call_ids
            .iter()
            .map(|call_id| galaxy_agent_core::PendingToolCall {
                call: galaxy_agent_core::ToolCall {
                    id: (*call_id).to_string(),
                    name: "init_project".to_string(),
                    arguments: serde_json::json!({}),
                },
                state: galaxy_agent_core::PendingToolCallState::Proposed,
            })
            .collect(),
    }
}

#[test]
fn readiness_retry_cannot_preprocess_across_a_serial_barrier_or_skip_a_pending_tool() {
    warpui::App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            let terminal_view_id = ctx.view_id();
            let relevant_files = ctx.add_model(GetRelevantFilesController::new);
            let action_model = ctx.add_model(|ctx| {
                BlocklistAIActionModel::new(
                    view.model.clone(),
                    view.active_session().clone(),
                    view.model_event_dispatcher(),
                    relevant_files,
                    terminal_view_id,
                    ctx,
                )
            });
            action_model.update(ctx, |model, ctx| {
                let conversation_id = AIConversationId::new();
                let batch = pending_tool_batch(&["first", "second"]);
                let actions = vec![action("first"), action("second")];
                model.provider_tool_executions.extend(
                    provider_action_correlations(&actions, conversation_id, &batch).unwrap(),
                );
                for action in &actions {
                    model
                        .deferred_provider_preprocessing
                        .insert((conversation_id, action.id.clone()), false);
                }
                model
                    .pending_actions
                    .insert(conversation_id, actions.clone().into());
                model.add_running_action(
                    conversation_id,
                    AIAgentActionId::from("running".to_string()),
                    RunningActionPhase::Serial,
                );
                assert!(model
                    .start_pending_action_by_id(
                        &actions[0].id,
                        conversation_id,
                        ActionExecutionInitiator::Automatic,
                        ctx
                    )
                    .is_none());
                assert!(
                    !model.deferred_provider_preprocessing
                        [&(conversation_id, actions[0].id.clone())]
                );
                model.running_actions.remove(&conversation_id);
                assert!(model
                    .start_pending_action_by_id(
                        &actions[1].id,
                        conversation_id,
                        ActionExecutionInitiator::User,
                        ctx
                    )
                    .is_none());
                assert!(
                    !model.deferred_provider_preprocessing
                        [&(conversation_id, actions[1].id.clone())]
                );
                assert_eq!(model.pending_actions[&conversation_id].len(), 2);
            });
        });
    });
}

fn count_startable_actions_for_pass(phases: &[(RunningActionPhase, bool)]) -> usize {
    let mut current_phase = None;
    let mut count = 0;

    for (phase, can_autoexecute) in phases {
        if let Some(current_phase) = current_phase {
            if !can_start_action_with_current_phase(current_phase, *phase, *can_autoexecute) {
                break;
            }
        }

        count += 1;
        current_phase = Some(*phase);

        if matches!(*phase, RunningActionPhase::Serial) {
            break;
        }
    }

    count
}

#[test]
fn provider_action_correlations_require_the_exact_unresolved_batch_order() {
    let conversation_id = AIConversationId::new();
    let batch = pending_tool_batch(&["first", "second"]);
    let actions = vec![action("first"), action("second")];

    let correlations = provider_action_correlations(&actions, conversation_id, &batch).unwrap();
    assert_eq!(correlations.len(), 2);
    assert_eq!(correlations[0].0, (conversation_id, actions[0].id.clone()));
    assert_eq!(correlations[0].1.run_id, batch.work_id.run_id);
    assert_eq!(correlations[0].1.epoch, batch.work_id.epoch);
    assert_eq!(correlations[0].1.call_id, "first");

    let error = provider_action_correlations(
        &[action("second"), action("first")],
        conversation_id,
        &batch,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ProviderActionQueueError::ActionSetMismatch {
            expected: vec!["first".to_string(), "second".to_string()],
            received: vec!["second".to_string(), "first".to_string()],
        }
    );
}

#[test]
fn exact_provider_action_correlation_is_idempotent() {
    let conversation_id = AIConversationId::new();
    let batch = pending_tool_batch(&["first"]);
    let actions = vec![action("first")];
    let execution_ref = ProviderToolExecutionRef::new(conversation_id, &batch.work_id, "first");
    let existing = HashMap::from([((conversation_id, actions[0].id.clone()), execution_ref)]);

    assert!(BlocklistAIActionModel::provider_action_ids_to_enqueue_from(
        &existing,
        &actions,
        conversation_id,
        &batch,
    )
    .unwrap()
    .is_empty());
}

#[test]
fn parallel_phase_only_admits_matching_autoexecutable_actions() {
    let phase =
        RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext);

    assert!(can_start_action_with_current_phase(phase, phase, true));
    assert!(!can_start_action_with_current_phase(phase, phase, false));
    assert!(!can_start_action_with_current_phase(
        phase,
        RunningActionPhase::Serial,
        true
    ));
    assert!(!can_start_action_with_current_phase(
        RunningActionPhase::Serial,
        phase,
        true
    ));
}

#[test]
fn parallel_user_started_action_allows_compatible_automatic_followups() {
    let parallel = StartedAction::Async {
        phase: RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext),
    };
    let serial = StartedAction::Async {
        phase: RunningActionPhase::Serial,
    };

    assert!(StartedAction::Sync.allows_compatible_following_actions());
    assert!(parallel.allows_compatible_following_actions());
    assert!(!serial.allows_compatible_following_actions());
}

#[test]
fn phased_scheduling_stops_at_serial_barrier_and_resumes_afterward() {
    let read_only_phase =
        RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext);
    let actions = vec![
        (read_only_phase, true),
        (read_only_phase, true),
        (RunningActionPhase::Serial, true),
        (read_only_phase, true),
        (read_only_phase, true),
    ];

    assert_eq!(count_startable_actions_for_pass(&actions), 2);
    assert_eq!(count_startable_actions_for_pass(&actions[2..]), 1);
    assert_eq!(count_startable_actions_for_pass(&actions[3..]), 2);
}

#[test]
fn automatic_retries_only_target_actions_deferred_as_not_ready() {
    let conversation_id = AIConversationId::new();
    let action_id = AIAgentActionId::from("file-edit".to_string());
    let mut tracker = NotReadyActionTracker::default();

    tracker.update_after_attempt(
        conversation_id,
        action_id.clone(),
        NotExecutedReason::NotReady,
        ActionExecutionInitiator::Automatic,
    );
    assert!(tracker.should_retry(conversation_id, &action_id));
    assert!(!ActionExecutionInitiator::Automatic.is_user_initiated());

    tracker.update_after_attempt(
        conversation_id,
        action_id.clone(),
        NotExecutedReason::NotReady,
        ActionExecutionInitiator::User,
    );
    assert!(!tracker.should_retry(conversation_id, &action_id));

    tracker.update_after_attempt(
        conversation_id,
        action_id.clone(),
        NotExecutedReason::NeedsConfirmation,
        ActionExecutionInitiator::Automatic,
    );
    assert!(!tracker.should_retry(conversation_id, &action_id));

    tracker.update_after_attempt(
        conversation_id,
        action_id.clone(),
        NotExecutedReason::WaitingOnSharer,
        ActionExecutionInitiator::Automatic,
    );
    assert!(!tracker.should_retry(conversation_id, &action_id));
    assert!(ActionExecutionInitiator::User.is_user_initiated());
}

#[test]
fn finished_results_stay_in_original_action_order() {
    let action_order = HashMap::from([
        (AIAgentActionId::from("first".to_owned()), 0),
        (AIAgentActionId::from("second".to_owned()), 1),
        (AIAgentActionId::from("third".to_owned()), 2),
    ]);
    let mut finished_results = [
        make_action_result("third"),
        make_action_result("first"),
        make_action_result("second"),
    ];

    sort_action_results_by_order(&mut finished_results, &action_order);

    assert_eq!(
        finished_results[0].id,
        AIAgentActionId::from("first".to_owned())
    );
    assert_eq!(
        finished_results[1].id,
        AIAgentActionId::from("second".to_owned())
    );
    assert_eq!(
        finished_results[2].id,
        AIAgentActionId::from("third".to_owned())
    );
}

#[test]
fn domain_tool_results_preserve_success_failure_cancellation_and_denial() {
    let success = domain_tool_result(
        &action_result("success", AIAgentActionResultType::InitProject),
        false,
    );
    let failure = domain_tool_result(
        &action_result(
            "failure",
            AIAgentActionResultType::Grep(GrepResult::Error("boom".to_string())),
        ),
        false,
    );
    let cancelled_result = action_result(
        "cancelled",
        AIAgentActionResultType::Grep(GrepResult::Cancelled),
    );
    let cancelled = domain_tool_result(&cancelled_result, false);
    let denied = domain_tool_result(&cancelled_result, true);

    assert_eq!(success.status, ToolResultStatus::Success);
    assert_eq!(failure.status, ToolResultStatus::Error);
    assert_eq!(cancelled.status, ToolResultStatus::Cancelled);
    assert_eq!(denied.status, ToolResultStatus::Denied);
    assert_eq!(success.call_id, "success");
    assert_eq!(failure.call_id, "failure");
    assert_eq!(cancelled.call_id, "cancelled");
    assert_eq!(denied.call_id, "cancelled");
    assert!(!cancelled.content.contains("Permission denied"));
    assert!(denied.content.contains("Permission denied by the user"));
}

#[test]
fn domain_read_result_contains_the_file_contents_for_the_next_model_turn() {
    let result = action_result(
        "read-call",
        AIAgentActionResultType::ReadFiles(ReadFilesResult::Success {
            files: vec![FileContext::new(
                "/workspace/src/lib.rs".to_string(),
                AnyFileContent::StringContent("pub fn answer() -> u8 { 42 }".to_string()),
                None,
                None,
            )],
        }),
    );

    let result = domain_tool_result(&result, false);

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.call_id, "read-call");
    assert!(result.content.contains("/workspace/src/lib.rs"));
    assert!(result.content.contains("pub fn answer() -> u8 { 42 }"));
}

#[test]
fn computer_results_keep_real_image_bytes_and_coordinate_metadata() {
    let screenshot = computer_use::Screenshot {
        width: 500, height: 300, original_width: 1000, original_height: 600,
        data: vec![1, 2, 3, 4], mime_type: "image/png".into(),
    };
    let action = action_result("desktop", AIAgentActionResultType::RequestComputerUse(RequestComputerUseResult::Approved {
        screenshot, platform: computer_use::Platform::Mac, windows: Vec::new(),
    }));
    let result = domain_tool_result(&action, false);
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].data, vec![1, 2, 3, 4]);
    assert_eq!(result.images[0].mime_type, "image/png");
    let metadata: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(metadata["platform"], "Mac");
    assert_eq!(metadata["screenshot"]["width"], 500);
    assert_eq!(metadata["screenshot"]["original_width"], 1000);
    assert!(domain_tool_result(&action, true).images.is_empty());
}

#[test]
fn action_permission_kinds_match_the_safety_boundary() {
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::Grep {
            queries: vec!["needle".to_string()],
            path: ".".to_string(),
        }),
        PermissionKind::Read
    );
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::InitProject),
        PermissionKind::Write
    );
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::RequestCommandOutput {
            command: "cargo test".to_string(),
            is_read_only: Some(true),
            is_risky: Some(false),
            wait_until_completion: true,
            uses_pager: Some(false),
            rationale: None,
            citations: Vec::new(),
        }),
        PermissionKind::Execute
    );
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::CallMCPTool {
            server_id: None,
            name: "tool".to_string(),
            input: serde_json::json!({}),
        }),
        PermissionKind::ExternalTool
    );
}

#[test]
fn denied_permission_event_resolves_the_pending_call() {
    let action = action("call-1");

    let ToolEvent::PermissionResolved {
        request_id,
        call_id,
        decision,
    } = permission_denied_tool_event(&action)
    else {
        panic!("expected a permission resolution event");
    };

    assert_eq!(request_id, "permission:call-1");
    assert_eq!(call_id, "call-1");
    assert_eq!(
        decision,
        PermissionDecision::Denied {
            reason: Some("Permission denied by the user.".to_string()),
        }
    );
}

#[test]
fn provider_owned_denial_suppresses_duplicate_completion() {
    assert!(!should_emit_tool_completion(true, true));
    assert!(should_emit_tool_completion(true, false));
    assert!(should_emit_tool_completion(false, true));
}

#[test]
fn only_rejecting_a_blocked_action_is_a_permission_denial() {
    assert!(is_permission_denial(
        CancellationReason::ManuallyCancelled,
        Some(&AIActionStatus::Blocked),
    ));
    assert!(!is_permission_denial(
        CancellationReason::ManuallyCancelled,
        Some(&AIActionStatus::Queued),
    ));
    assert!(!is_permission_denial(
        CancellationReason::FollowUpSubmitted {
            is_for_same_conversation: true,
        },
        Some(&AIActionStatus::Blocked),
    ));
}

#[test]
fn duplicate_action_ids_resolve_only_within_the_requested_conversation() {
    let first_conversation = AIConversationId::new();
    let second_conversation = AIConversationId::new();
    let duplicate_id = AIAgentActionId::from("duplicate".to_string());
    let first_result = make_action_result("duplicate");
    let mut second_result = action_result("duplicate", AIAgentActionResultType::InitProject);
    second_result.task_id = TaskId::new("second-task".to_string());
    let second_result = Arc::new(second_result);
    let finished_results = HashMap::from([(first_conversation, vec![first_result.clone()])]);
    let provider_results = HashMap::new();
    let archive = HashMap::from([
        (
            (first_conversation, duplicate_id.clone()),
            first_result.clone(),
        ),
        (
            (second_conversation, duplicate_id.clone()),
            second_result.clone(),
        ),
    ]);

    assert!(Arc::ptr_eq(
        action_result_for_conversation(
            &finished_results,
            &provider_results,
            &archive,
            first_conversation,
            &duplicate_id,
        )
        .unwrap(),
        &first_result,
    ));
    assert!(Arc::ptr_eq(
        action_result_for_conversation(
            &finished_results,
            &provider_results,
            &archive,
            second_conversation,
            &duplicate_id,
        )
        .unwrap(),
        &second_result,
    ));
    assert!(action_result_for_conversation(
        &finished_results,
        &provider_results,
        &archive,
        AIConversationId::new(),
        &duplicate_id,
    )
    .is_none());
}

#[test]
fn cancellation_permission_inference_uses_the_matching_conversation_status() {
    let blocked_conversation = AIConversationId::new();
    let queued_conversation = AIConversationId::new();
    let duplicate_id = AIAgentActionId::from("duplicate".to_string());
    let pending_actions = HashMap::from([
        (blocked_conversation, VecDeque::from([action("duplicate")])),
        (
            queued_conversation,
            VecDeque::from([action("first"), action("duplicate")]),
        ),
    ]);
    let running_actions = HashMap::new();

    let blocked_status = pending_action_status(
        &pending_actions,
        &running_actions,
        blocked_conversation,
        &duplicate_id,
        false,
    );
    let queued_status = pending_action_status(
        &pending_actions,
        &running_actions,
        queued_conversation,
        &duplicate_id,
        false,
    );

    assert!(is_permission_denial(
        CancellationReason::ManuallyCancelled,
        blocked_status.as_ref(),
    ));
    assert!(!is_permission_denial(
        CancellationReason::ManuallyCancelled,
        queued_status.as_ref(),
    ));
}

#[test]
fn action_lifecycle_events_disambiguate_duplicate_ids_by_conversation() {
    let first_conversation = AIConversationId::new();
    let second_conversation = AIConversationId::new();
    let duplicate_id = AIAgentActionId::from("duplicate".to_owned());
    let events = [
        BlocklistAIActionEvent::ActionBlockedOnUserConfirmation {
            action_id: duplicate_id.clone(),
            conversation_id: first_conversation,
            execution_ref: None,
        },
        BlocklistAIActionEvent::ExecutingAction {
            action_id: duplicate_id.clone(),
            conversation_id: second_conversation,
            execution_ref: None,
        },
        BlocklistAIActionEvent::FinishedAction {
            action_id: duplicate_id.clone(),
            conversation_id: first_conversation,
            cancellation_reason: None,
            execution_ref: None,
        },
    ];

    assert_eq!(events[0].conversation_id(), Some(first_conversation));
    assert_eq!(events[1].conversation_id(), Some(second_conversation));
    assert_eq!(events[2].conversation_id(), Some(first_conversation));
    assert!(events
        .iter()
        .all(|event| event.action_id() == &duplicate_id));
}
